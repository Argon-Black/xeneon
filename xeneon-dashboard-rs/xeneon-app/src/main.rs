// SPDX-License-Identifier: GPL-3.0-or-later
//! Xeneon Dashboard - Rust/Relm4 port. Phase 1: a window sized to the
//! Xeneon Edge panel, F11 fullscreen, and a carousel of pages backed by
//! real `WidgetGrid`s (drag/snap wired to xeneon-core's grid math), with a
//! page indicator matching the Python app's clickable/touch-sized/
//! auto-hiding one. Widgets and pages are persisted to
//! `$XDG_CONFIG_HOME/xeneon-dashboard-rs/{widgets,pages}/<id>.json` (one
//! file per widget/page, exactly like the Python app's `widget_store.py`/
//! `page_store.py`) and reloaded on startup. The settings page (language
//! switcher, page rename, plus a dev-only restart button) is always the
//! carousel's last page. Ctrl+Shift+A opens the widget picker
//! (widget_picker.rs, a full-screen slide-down overlay ported from
//! `WidgetPicker` in widget_picker.py, currently still showing a plain
//! list rather than the Python original's size-grouped live previews - see
//! that module's own doc comment for the two-step porting plan) and adds
//! the chosen kind to the current page (or the next real page with room,
//! same overflow-forward
//! scan as `XeneonWindow.add_widget`). Unlike the Python original -
//! which has no page cap - running out of room on every existing page
//! (the current page is full, or the widget is too big to ever fit an
//! empty one) creates a fresh page and lands the widget there, up to
//! `MAX_PAGES`; past that cap, adding is refused with a toast instead
//! (see `AppMsg::AddWidget`). The page indicator also carries its own
//! "+" button (`AppMsg::AddPage`) to create an empty page directly,
//! independent of adding a widget - not in the Python original, added
//! directly here. Both paths share the actual page-creation logic (see
//! `AppModel::create_page`/`scroll_to_once_sized`).
//!
//! Startup layout: whatever was saved (or one empty page, on a first run
//! with nothing saved yet), then - only in dev mode (see
//! `dev_mode_enabled`) - a page with one of every dummy widget size, to
//! validate the base interaction stack (move, snap, delete, page swipe)
//! on demand. That page's own widgets are never persisted (see
//! `WidgetGrid::ephemeral`), so dev-only test content can never leak into
//! the real saved layout.

mod appearance_css;
mod appearance_popover;
mod config_store;
mod dashboard_widget;
mod grid_widget;
mod help_overlay;
mod i18n_runtime;
mod logging;
mod page_indicator;
mod settings_page;
mod theme;
mod tray;
mod widget_picker;
mod widgets;

use adw::prelude::*;
use log::warn;
use page_indicator::PageIndicator;
use relm4::prelude::*;
use std::path::PathBuf;
use std::rc::Rc;
use xeneon_core::config;
use xeneon_core::grid::{PAGE_H, PAGE_W};
use xeneon_core::page_state;
use xeneon_core::widget_state;

use grid_widget::WidgetGrid;

/// Real panel resolution of the Xeneon Edge bar screen at 100% display
/// scale (logical px == physical px - see CLAUDE.md in the Python app).
const WINDOW_WIDTH: i32 = 2560;
const WINDOW_HEIGHT: i32 = 720;

/// GApplication id, also reused as the icon name below - both need to
/// agree so a future .desktop file's `Icon=` key (conventionally set to
/// the app id) resolves to the same icon this window already uses.
const APP_ID: &str = "com.n3tlab.XeneonDashboardRust";

/// Hard cap on real (non-dev, non-settings) pages, matching the limit the
/// user asked for so a runaway sequence of "add a widget" calls can't
/// grow the carousel without bound. Past this, `AddWidget` shows a toast
/// instead of creating another page.
const MAX_PAGES: usize = 10;

/// Set `XENEON_DEV_MODE=1` (any value works) to show the dummy-widgets
/// test page at startup and the settings page's restart button. Runtime
/// env var rather than a Cargo feature flag so it can be toggled
/// per-launch without recompiling.
pub(crate) fn dev_mode_enabled() -> bool {
    std::env::var("XENEON_DEV_MODE").is_ok()
}

struct AppModel {
    fullscreened: bool,
    title: String,
    header_bar: adw::HeaderBar,
    window_title: adw::WindowTitle,
    carousel: adw::Carousel,
    // Scroll target for the "go to settings" shortcut (Ctrl+,) - see
    // AppMsg::GotoSettings. Always the carousel's last page (see its
    // append order at the end of init()).
    settings_root: gtk::Box,
    // Shown on top of everything else (carousel, header bar, page
    // indicator) so a toast - currently only the "max pages reached"
    // warning - floats above them rather than being clipped by the
    // gtk::Overlay underneath. See the manual reparenting after
    // view_output!() below for why this isn't built inside the view!
    // macro like the rest of the window content.
    toast_overlay: adw::ToastOverlay,
    widget_picker: std::rc::Rc<widget_picker::WidgetPicker>,
    help_overlay: std::rc::Rc<help_overlay::HelpOverlay>,
    // Needed again whenever AddWidget creates a fresh page at runtime -
    // WidgetGrid::new takes them, same as every real page built at
    // startup below.
    widgets_dir: PathBuf,
    pages_dir: PathBuf,
    // Kept alive for the app's lifetime - each owns the Rc<RefCell<_>>
    // state its drag/delete closures capture. real_grids also doubles as
    // the "which pages can I add a widget to" list for AddWidget below.
    real_grids: Vec<Rc<WidgetGrid>>,
    _dev_grid: Option<Rc<WidgetGrid>>,
    _page_indicator: PageIndicator,
    // Lets AddWidget append a page to the Settings "Pages" rename list the
    // moment it creates one, instead of the list only catching up on the
    // next language switch - see `settings_page::PagesHandle`.
    pages_handle: settings_page::PagesHandle,
    // Kept alive for the app's lifetime - dropping it tears the tray icon
    // down. `None` when no StatusNotifierWatcher is running (see
    // `tray::spawn`); the app stays fully usable either way.
    _tray: Option<ksni::blocking::Handle<tray::XeneonTray>>,
}

#[derive(Debug)]
enum AppMsg {
    ToggleFullscreen,
    Retranslate,
    ShowWidgetPicker,
    AddWidget(&'static str),
    GotoSettings,
    /// A widget delete left the named page (by `page_id`, not
    /// `page_index` - the latter can shift under a page deleted earlier
    /// in the same batch) with zero widgets on it. Looked up again here
    /// rather than passing an index directly, since some time may pass
    /// between the delete closure firing (deep inside `grid_widget.rs`,
    /// with no view of `AppModel`) and this message being handled.
    PageEmptied(String),
    /// The page indicator's "+" button (see page_indicator.rs's module
    /// doc comment) - creates a brand new empty page and navigates to it.
    /// Not in the Python original; added directly in this Rust port.
    AddPage,
    /// Tray menu "Relancer l'application" - see `settings_page::relaunch`.
    Relaunch,
    /// Tray menu "Quitter".
    Quit,
    /// Tray menu "Aide" - see help_overlay.rs.
    ShowHelp,
}

#[relm4::component]
impl SimpleComponent for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        adw::ApplicationWindow {
            set_default_width: WINDOW_WIDTH,
            set_default_height: WINDOW_HEIGHT,
            #[watch]
            set_fullscreened: model.fullscreened,
            #[watch]
            set_title: Some(&model.title),
            // GTK gives every window an implicit default CSD titlebar
            // unless told otherwise - separate from our own HeaderBar
            // widget below, and unaffected by merely hiding that widget
            // (which is what the band surviving two different attempts at
            // hiding the HeaderBar turned out to mean). Explicitly
            // undecorating the window in fullscreen removes it outright.
            #[watch]
            set_decorated: !model.fullscreened,

            // Global F11 (fullscreen), Ctrl+Shift+A (open the widget
            // picker) and Ctrl+, (go to settings) toggles. The
            // Python app instead registers Gio.SimpleActions with
            // accelerators at the application level (plus a
            // global-shortcuts portal binding for F11 so it works even
            // unfocused) - deferred to a later phase, see
            // settings_page.rs's own note on the shortcuts group.
            //
            // The widget picker's shortcut used to be Ctrl+Plus/Ctrl+=/
            // numpad + (matching the Python original), but GtkRange
            // (the Audio widget's progress Gtk.Scale) binds bare
            // "+"/"-" to its own value increment/decrement by default -
            // with that scale focused (e.g. while a track is playing),
            // Ctrl+Plus got eaten by the scale before ever reaching this
            // window-level controller. Ctrl+Shift+A sidesteps that
            // specific conflict (no GTK widget binds Shift+letter combos
            // by default), but the underlying class of bug is broader:
            // an EventController's default propagation phase is Bubble,
            // so it only sees a key event *after* it's already been
            // offered to whichever widget currently has focus (and that
            // widget's own ancestors) - any focused widget/row that
            // consumes the event first (a Switch, a ListBoxRow, ...)
            // means it never reaches this controller at all, which read
            // as the shortcut "sometimes" not firing depending on what
            // had focus. Capture phase fixes that class of bug outright:
            // this controller sees every key press *before* GTK
            // dispatches it to the focused widget, so nothing downstream
            // can eat it first - matches how the Python original's
            // Gio.SimpleAction + set_accels_for_action shortcuts behave
            // (an app-level accelerator, not a per-widget one).
            add_controller = gtk::EventControllerKey {
                set_propagation_phase: gtk::PropagationPhase::Capture,
                connect_key_pressed[sender, widget_picker, help_overlay] => move |_, key, _, modifiers| {
                    if key == gtk::gdk::Key::F11 {
                        sender.input(AppMsg::ToggleFullscreen);
                        gtk::glib::Propagation::Stop
                    // Only intercepts Escape (and only stops it here)
                    // while one of these overlays is actually open -
                    // otherwise falls through to Propagation::Proceed
                    // below so anything else that wants Escape for itself
                    // (a Gtk.Popover's own dismiss handling, say) still
                    // gets it normally. See widget_picker.rs's own
                    // comment on why this can't just be a key controller
                    // on either overlay itself - help_overlay.rs mirrors
                    // the same reasoning.
                    } else if key == gtk::gdk::Key::Escape && widget_picker.widget().reveals_child() {
                        widget_picker.close();
                        gtk::glib::Propagation::Stop
                    } else if key == gtk::gdk::Key::Escape && help_overlay.widget().reveals_child() {
                        help_overlay.close();
                        gtk::glib::Propagation::Stop
                    } else if modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                        && modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK)
                        && matches!(key, gtk::gdk::Key::a | gtk::gdk::Key::A)
                    {
                        sender.input(AppMsg::ShowWidgetPicker);
                        gtk::glib::Propagation::Stop
                    } else if modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK) && key == gtk::gdk::Key::comma {
                        sender.input(AppMsg::GotoSettings);
                        gtk::glib::Propagation::Stop
                    } else {
                        gtk::glib::Propagation::Proceed
                    }
                }
            },

            // Empty here - the carousel, page indicator and header bar
            // are all built and attached imperatively in init() instead,
            // since none of relm4's declarative child-attaching sugar
            // (local_ref, add_overlay=, #[watch]) is confirmed to compose
            // together for "an externally pre-built widget attached via a
            // named method" - safer to just wire it by hand here, the same
            // way the carousel's own pages already are below.
            #[wrap(Some)]
            #[name = "overlay"]
            set_content = &gtk::Overlay {
                // Adw.ToolbarView normally tags its content "view" for a
                // flat background; bypassing it (as we do here, going
                // straight to an Overlay) means the window's own
                // titlebar-blend gradient shows through as a banded
                // background otherwise - see window.py in the Python app,
                // same fix, same root cause.
                add_css_class: "view",
            },
        }
    }

    fn init(_init: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        config_store::init();
        let app_config = config_store::get();

        i18n_runtime::init(&app_config.language);

        // "Follow system" only takes if this desktop actually reports a
        // system accent (system_accent_hex() returns None otherwise, e.g.
        // some non-GNOME desktops) - falls back to the saved accent_color
        // either way, matching app.py's `_apply_configured_accent()`.
        let accent = if app_config.accent_follow_system { theme::system_accent_hex() } else { None };
        theme::apply_accent(accent.as_deref().unwrap_or(&app_config.accent_color));

        let widgets_dir = config::widgets_dir();
        let pages_dir = config::pages_dir();

        let page_states = page_state::load_all(&pages_dir);
        let widget_states = widget_state::load_all(&widgets_dir);

        // A page can have saved state (a custom name) with no widgets on
        // it at all, so the page count has to account for both sources -
        // mirrors window.py's _build_pages_from_state. No saved state
        // anywhere (first run) falls back to a single empty page rather
        // than the Python original's hardcoded demo layout (registry
        // exists now, but a demo layout is a deliberate choice to make
        // later, not a side effect of this refactor).
        let max_page_index =
            widget_states.iter().map(|s| s.page_index).chain(page_states.iter().map(|s| s.page_index)).max();
        // Audit finding 2026-09-18: previously `m + 1` with no cap - a
        // corrupted/hand-edited widgets/<id>.json or pages/<id>.json with
        // a large page_index would make startup try to build that many
        // WidgetGrid/gtk::Fixed pages (a hang/OOM), and page_index ==
        // usize::MAX would overflow this addition (panic in debug,
        // silent wraparound to 0 pages in release). Saturate the
        // addition and clamp to the same MAX_PAGES the interactive
        // "add page" path already enforces - anything saved past that
        // cap still surfaces via the existing out-of-range warning below
        // when a widget/page is skipped, rather than growing the
        // carousel without bound.
        let page_count = max_page_index.map_or(1, |m| m.saturating_add(1)).min(MAX_PAGES);
        if max_page_index.is_some_and(|m| m.saturating_add(1) > MAX_PAGES) {
            warn!("saved state references page_index up to {}, clamping startup page count to MAX_PAGES ({MAX_PAGES})", max_page_index.unwrap());
        }

        let mut real_grids: Vec<Rc<WidgetGrid>> = Vec::with_capacity(page_count);
        for index in 0..page_count {
            let grid = match page_states.iter().find(|s| s.page_index == index) {
                Some(state) => WidgetGrid::restore(
                    PAGE_W,
                    PAGE_H,
                    index,
                    state.id.clone(),
                    state.name.clone(),
                    widgets_dir.clone(),
                    pages_dir.clone(),
                ),
                None => WidgetGrid::new(PAGE_W, PAGE_H, index, widgets_dir.clone(), pages_dir.clone()),
            };
            grid.set_background_image(app_config.app_background_image_path.as_deref());
            let grid = Rc::new(grid);
            register_page_emptied(&grid, &sender);
            real_grids.push(grid);
        }
        for state in &widget_states {
            let Some(grid) = real_grids.get(state.page_index) else {
                warn!("widget {} ignored, page_index {} out of range", state.id, state.page_index);
                continue;
            };
            match widgets::registry::find(&state.kind) {
                Some(descriptor) => grid.restore_widget(state, descriptor.card_title_key, (descriptor.restore)(&state.content)),
                None => warn!("widget {} has unknown kind {:?}, skipped", state.id, state.kind),
            }
        }

        // Dev-mode-only page: one of every dummy size preset, to validate
        // move/snap/delete/swipe on demand. Its widgets are never
        // persisted (WidgetGrid::ephemeral) - regenerated fresh in code
        // every launch instead, so it always comes back exactly like
        // this regardless of anything done to it in a previous session.
        let dev_grid = dev_mode_enabled().then(|| {
            let grid = WidgetGrid::ephemeral(PAGE_W, PAGE_H, real_grids.len());
            // Not persisted (WidgetGrid::ephemeral guards set_custom_name
            // the same as everything else), just sets the in-memory label
            // the page indicator shows for this page.
            grid.set_custom_name(Some(i18n_runtime::t("widgets.dev_page.title")));
            for descriptor in widgets::registry::CATALOG {
                // This page exists to validate every grid size preset
                // (the dummy_* kinds), not to preview every real plugin -
                // those are already exercised through the normal widget
                // picker on a real page. Originally this skipped "clock"
                // by name (it's medium-sized and, added ahead of the
                // dummies, ate the column space dummy_l - a full-height
                // column - needs, so dummy_l would silently fail to place
                // and be missing from the page); now that more real
                // plugins exist (audio...), skip by pattern instead of
                // growing a per-kind exclusion list by hand each time.
                if !descriptor.kind.starts_with("dummy_") {
                    continue;
                }
                grid.add_widget(descriptor.title_key, descriptor.kind, descriptor.size, (descriptor.spawn)());
            }
            Rc::new(grid)
        });

        let carousel = adw::Carousel::new();
        carousel.set_vexpand(true); // parity with window.py's self.carousel.set_vexpand(True)

        // The settings page's real identity is established here, empty,
        // *before* the page indicator - the indicator needs the actual
        // widget it'll later see in the carousel (for its "stay revealed
        // here" and gear-icon behaviour), and settings_page::populate
        // below needs a live indicator to refresh on rename. Building the
        // shell first and filling it in after breaks that cycle.
        let settings_root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let page_indicator = PageIndicator::new(&carousel, &settings_root, {
            let sender = sender.clone();
            move || sender.input(AppMsg::AddPage)
        });
        page_indicator.set_hide_delay_seconds(app_config.indicator_hide_delay_seconds);
        page_indicator.set_style(app_config.indicator_opacity, app_config.indicator_button_color.as_deref());
        for grid in &real_grids {
            page_indicator.register_page(grid.widget(), grid.clone());
        }
        if let Some(dev_grid) = &dev_grid {
            page_indicator.register_page(dev_grid.widget(), dev_grid.clone());
        }

        let pages_handle = settings_page::populate(&settings_root, &real_grids, page_indicator.clone());

        let window_title = adw::WindowTitle::new(&i18n_runtime::t("window.title"), "");
        let header_bar = adw::HeaderBar::new();
        header_bar.set_valign(gtk::Align::Start);
        header_bar.set_title_widget(Some(&window_title));

        let toast_overlay = adw::ToastOverlay::new();

        let widget_picker = widget_picker::WidgetPicker::new({
            let sender = sender.clone();
            move |kind| sender.input(AppMsg::AddWidget(kind))
        });
        let help_overlay = help_overlay::HelpOverlay::new();

        // Quick, temporary visual-QA aid: fullscreen straight onto the
        // real Xeneon Edge panel (identified by its distinctive
        // resolution) when one is connected, bypassing window chrome
        // entirely so what's on screen is exactly what the final kiosk
        // display will show. The real version of this (display.py in the
        // Python app) is more involved - portal-backed global shortcut to
        // toggle it, `Adw.ToolbarView` swapped out of the content tree in
        // fullscreen - and is deferred to a later phase; this is only
        // here so dragging/snapping can be judged on the actual hardware
        // while building it. Detected *before* building the model so
        // `fullscreened` starts true (hiding the header bar below)
        // instead of only becoming true after a subsequent F11 toggle.
        let display = gtk::gdk::Display::default();
        if let Some(display) = &display {
            register_app_icon(display);
        }

        let xeneon = display.and_then(|d| xeneon_monitor(&d));
        let fullscreened = xeneon.is_some();
        header_bar.set_visible(!fullscreened);

        let tray = tray::spawn(sender.input_sender().clone());
        if let Some(handle) = tray.clone() {
            i18n_runtime::on_change(move || {
                handle.update(|tray| tray.retranslate());
            });
        }

        let model = AppModel {
            fullscreened,
            title: i18n_runtime::t("window.title"),
            header_bar,
            window_title,
            carousel: carousel.clone(),
            settings_root: settings_root.clone(),
            toast_overlay: toast_overlay.clone(),
            widget_picker: widget_picker.clone(),
            help_overlay: help_overlay.clone(),
            widgets_dir,
            pages_dir,
            real_grids,
            _dev_grid: dev_grid,
            _page_indicator: page_indicator,
            pages_handle,
            _tray: tray,
        };

        i18n_runtime::on_change({
            let sender = sender.clone();
            move || sender.input(AppMsg::Retranslate)
        });

        let widgets = view_output!();

        widgets.overlay.set_child(Some(&carousel));
        widgets.overlay.add_overlay(model._page_indicator.widget());
        widgets.overlay.add_overlay(&model.header_bar);
        // Added last so it stacks on top of the header bar too, covering
        // the whole window while windowed (fullscreen/kiosk already hides
        // the header bar - see ToggleFullscreen).
        widgets.overlay.add_overlay(model.widget_picker.widget());
        widgets.overlay.add_overlay(model.help_overlay.widget());

        // Splice the toast overlay in between the window and its existing
        // content rather than building it inside the view! macro above -
        // consistent with this file's own stated preference (see the
        // set_content doc comment) for wiring externally-built widgets by
        // hand. set_content() on the window unparents the gtk::Overlay
        // (its current content) as part of replacing it, which is what
        // makes the immediately following set_child() below valid - a
        // widget can't be given a second parent while it still has one.
        root.set_content(Some(&toast_overlay));
        toast_overlay.set_child(Some(&widgets.overlay));

        for grid in &model.real_grids {
            carousel.append(grid.widget());
        }
        if let Some(dev_grid) = &model._dev_grid {
            carousel.append(dev_grid.widget());
        }
        // Always last, same as window.py's
        // `self.carousel.append(self._settings_page)`.
        carousel.append(&settings_root);

        if let Some(monitor) = xeneon {
            root.fullscreen_on_monitor(&monitor);
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ToggleFullscreen => {
                self.fullscreened = !self.fullscreened;
                self.header_bar.set_visible(!self.fullscreened);
            }
            AppMsg::Retranslate => {
                self.title = i18n_runtime::t("window.title");
                self.window_title.set_title(&self.title);
            }
            AppMsg::ShowWidgetPicker => {
                let existing_kinds: Vec<String> = self.real_grids.iter().flat_map(|grid| grid.kinds()).collect();
                self.widget_picker.open(&existing_kinds);
            }
            AppMsg::GotoSettings => {
                self.carousel.scroll_to(&self.settings_root, true);
            }
            AppMsg::AddWidget(kind) => {
                let Some(descriptor) = widgets::registry::find(kind) else { return };
                let start = current_widget_page_index(&self.carousel, self.real_grids.len());
                let target = (start..self.real_grids.len()).find(|&i| self.real_grids[i].has_room_for(descriptor.size));

                // No existing page has room - either every one is full, or
                // the widget is too big to ever fit an empty one (both
                // read the same way through has_room_for/find_free_position:
                // simply "no free spot"). Overflow forward onto a brand
                // new page instead of giving up, same as the scan above
                // already does across existing pages - create_page() itself
                // enforces MAX_PAGES so this can't grow the carousel
                // without bound.
                let target = target.or_else(|| self.create_page(&sender));

                if let Some(index) = target {
                    let grid = &self.real_grids[index];
                    grid.add_widget(descriptor.card_title_key, descriptor.kind, descriptor.size, (descriptor.spawn)());
                    self.scroll_to_once_sized(index);
                }
            }
            AppMsg::AddPage => {
                if let Some(index) = self.create_page(&sender) {
                    self.scroll_to_once_sized(index);
                }
            }
            AppMsg::Relaunch => settings_page::relaunch(dev_mode_enabled()),
            AppMsg::Quit => relm4::main_application().quit(),
            AppMsg::ShowHelp => self.help_overlay.open(),
            AppMsg::PageEmptied(page_id) => {
                let Some(pos) = self.real_grids.iter().position(|g| g.page_id() == page_id) else { return };
                // The first page always stays, even empty - there must
                // always be somewhere to add the next widget to.
                if pos == 0 {
                    return;
                }
                let grid = self.real_grids.remove(pos);
                self.carousel.remove(grid.widget());
                self._page_indicator.unregister_page(grid.widget());
                self.pages_handle.remove_page(&grid);
                if let Err(err) = page_state::delete(&self.pages_dir, grid.page_id()) {
                    warn!("failed to delete saved page {}: {err}", grid.page_id());
                }
                // Every page after the deleted one shifts down by one
                // index - and resaves its own widgets under that
                // corrected index - so persistence stays contiguous
                // (0..page_count) for the next restart. See
                // WidgetGrid::set_page_index.
                for later in &self.real_grids[pos..] {
                    later.set_page_index(later.page_index() - 1);
                }
            }
        }
    }
}

impl AppModel {
    /// Creates a fresh, empty real page - background applied, delete/empty
    /// callback wired, inserted into the carousel right after the existing
    /// real pages (ahead of the dev-mode test page and/or the settings
    /// page, which always come after `real_grids` - see the append order
    /// at the end of `init()`), registered with the page indicator and the
    /// Settings "Pages" rename list, and pushed onto `real_grids`. Returns
    /// its index, or `None` (after showing a "max pages" toast) once
    /// `MAX_PAGES` is already reached.
    ///
    /// Shared by `AddWidget`'s page-overflow fallback and `AddPage` (the
    /// page indicator's "+" button) - both used to duplicate this same
    /// create/register/insert/push sequence inline.
    fn create_page(&mut self, sender: &ComponentSender<Self>) -> Option<usize> {
        if self.real_grids.len() >= MAX_PAGES {
            let toast = adw::Toast::new(&i18n_runtime::t_args(
                "widgets.add_menu.max_pages_toast",
                &[("max", &MAX_PAGES.to_string())],
            ));
            toast.set_timeout(4);
            self.toast_overlay.add_toast(toast);
            return None;
        }

        let new_index = self.real_grids.len();
        let grid = WidgetGrid::new(PAGE_W, PAGE_H, new_index, self.widgets_dir.clone(), self.pages_dir.clone());
        grid.set_background_image(config_store::get().app_background_image_path.as_deref());
        // Without this, a page with no name and no widgets on it yet has no
        // pages/<id>.json and no widgets/<id>.json referencing its index -
        // nothing on disk points back to it at all, so it silently vanishes
        // on the next restart (init()'s page_count is derived from the max
        // page_index seen across both files). Saving here the moment the
        // page is created - not just later on rename - is what makes an
        // intentionally-added empty page (the whole point of the "+"
        // button/AddPage) actually survive a restart.
        grid.save_state();
        let grid = Rc::new(grid);
        register_page_emptied(&grid, sender);
        self._page_indicator.register_page(grid.widget(), grid.clone());
        self.carousel.insert(grid.widget(), new_index as i32);
        self.pages_handle.add_page(grid.clone());
        self.real_grids.push(grid);
        Some(new_index)
    }

    /// Scrolls the carousel to `real_grids[index]`'s page once it actually
    /// has a size, polled once per frame via a tick callback rather than
    /// called inline: a page just inserted via `carousel.insert()` hasn't
    /// been allocated a size yet in the same call, and `AdwCarousel`
    /// computes `scroll_to`'s target offset from each page's allocated
    /// width - so scrolling immediately can silently fall short of a
    /// brand-new last page (correctly added, just visually not switched
    /// to). Two signal-based attempts were tried and measured (via
    /// temporary debug logging) to not work here: an idle callback can run
    /// before the frame clock's layout pass, and - confirmed by that
    /// logging - the page is already mapped (width 0) *before* insert()
    /// even returns, so its own "map" signal never fires afterwards to
    /// hang a callback off of. Polling `width() > 0` every frame
    /// sidesteps needing to know exactly which signal/priority is
    /// guaranteed to land after allocation.
    fn scroll_to_once_sized(&self, index: usize) {
        let carousel = self.carousel.clone();
        let page = self.real_grids[index].widget().clone();
        page.add_tick_callback(move |page, _frame_clock| {
            if page.width() == 0 {
                return gtk::glib::ControlFlow::Continue;
            }
            carousel.scroll_to(page, true);
            gtk::glib::ControlFlow::Break
        });
    }
}

/// Wires a real page's `on_emptied` callback to `AppMsg::PageEmptied` -
/// called for every real page, including the first one, since whether a
/// page is allowed to auto-delete depends on its *current* `page_index`
/// at the moment it empties (which can shift after an earlier page is
/// deleted), not on whether it happened to be first at registration time.
/// The `update()` handler is what actually enforces "never the first
/// page".
fn register_page_emptied(grid: &Rc<WidgetGrid>, sender: &ComponentSender<AppModel>) {
    let sender = sender.clone();
    let page_id = grid.page_id().to_string();
    grid.set_on_emptied(move || sender.input(AppMsg::PageEmptied(page_id.clone())));
}

/// Which real (non-dev, non-settings) page a newly added widget should
/// land on - the currently visible one, clamped into range. Mirrors
/// `XeneonWindow._current_widget_page_index()`.
fn current_widget_page_index(carousel: &adw::Carousel, real_page_count: usize) -> usize {
    if real_page_count == 0 {
        return 0;
    }
    (carousel.position().round().max(0.0) as usize).min(real_page_count - 1)
}

/// The Xeneon Edge bar's real panel resolution (2560x720) is distinctive
/// enough among normal monitors to identify it by geometry alone, without
/// needing to match a connector/EDID name.
fn xeneon_monitor(display: &gtk::gdk::Display) -> Option<gtk::gdk::Monitor> {
    display.monitors().iter::<gtk::gdk::Monitor>().flatten().find(|m| {
        let geo = m.geometry();
        geo.width() == WINDOW_WIDTH && geo.height() == WINDOW_HEIGHT
    })
}

/// Points the icon theme at `resources/icons/hicolor/...` (freedesktop
/// hicolor layout - `scalable/apps/<name>.svg`, `symbolic/apps/<name>-symbolic.svg`)
/// and sets the app icon name from that lookup, so the window resolves
/// "com.n3tlab.XeneonDashboardRust" the same way `resources/com.n3tlab.XeneonDashboardRust.desktop`'s
/// `Icon=` key does once that's actually installed (see that file's own
/// comment on the current no-install-step-yet state). The tray icon
/// (tray.rs) doesn't use this lookup at all - it rasterizes the SVG
/// directly, for the same reason.
///
/// Resolved from `CARGO_MANIFEST_DIR` at compile time rather than an
/// installed system path - there's no packaging/install step yet (see
/// CLAUDE.md's Flatpak notes), so this only works run from a source
/// checkout (`cargo run`/the built binary staying next to its source
/// tree). Revisit this once packaging exists: an installed icon would
/// live under `/usr/share/icons/hicolor/...` or a Flatpak's equivalent,
/// found by the icon theme automatically with no search path needed.
fn register_app_icon(display: &gtk::gdk::Display) {
    let icons_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/icons");
    gtk::IconTheme::for_display(display).add_search_path(icons_dir);
    gtk::Window::set_default_icon_name(APP_ID);
}

fn main() {
    // Works around a WebKitGTK/Mesa bug that otherwise kills the youtube
    // widget's WebKitWebProcess repeatedly during normal use (confirmed via
    // `coredumpctl`: `util_queue_kill_threads`/`_gbm_device_destroy` racing
    // glibc's heap during that process's own exit cleanup - a corrupted-heap
    // SIGABRT/SIGSEGV in Mesa's GBM/DRI teardown, not this app's code - see
    // youtube.rs's `connect_web_process_terminated` doc comment for the
    // existing auto-reload mitigation). The DMA-BUF renderer is what
    // exercises that GBM/DRI path in the first place; disabling it is the
    // community-known workaround and must be set before WebKit's process
    // launcher reads the environment, i.e. before any WebView/NetworkSession
    // is ever created (youtube.rs's own `NETWORK_SESSION` included).
    // SAFETY: this is the very first statement in `main`, before GTK,
    // relm4, tokio or any other thread-spawning machinery exists - nothing
    // else can be reading the environment concurrently yet.
    unsafe {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    // Must be the very first thing - every `log::` call before this
    // point is silently dropped (see `logging::init`'s own doc comment).
    logging::init(dev_mode_enabled());

    let app = RelmApp::new(APP_ID);
    app.run::<AppModel>(());
}
