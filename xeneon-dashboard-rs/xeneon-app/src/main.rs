//! Xeneon Dashboard - Rust/Relm4 port. Phase 1: a window sized to the
//! Xeneon Edge panel, F11 fullscreen, and a carousel of pages backed by
//! real `WidgetGrid`s (drag/snap wired to xeneon-core's grid math), with a
//! page indicator matching the Python app's clickable/touch-sized/
//! auto-hiding one. Widgets and pages are persisted to
//! `$XDG_CONFIG_HOME/xeneon-dashboard-rs/{widgets,pages}/<id>.json` (one
//! file per widget/page, exactly like the Python app's `widget_store.py`/
//! `page_store.py`) and reloaded on startup. The settings page (language
//! switcher, page rename, plus a dev-only restart button) is always the
//! carousel's last page. Ctrl+Plus/Ctrl+= (also the numpad +) opens a
//! simple list of every registered widget kind - a reduced stand-in for
//! `WidgetPicker` in widget_picker.py - and adds the chosen one to the
//! current page (or the next real page with room, same overflow-forward
//! scan as `XeneonWindow.add_widget`). Unlike the Python original -
//! which has no page cap - running out of room on every existing page
//! (the current page is full, or the widget is too big to ever fit an
//! empty one) creates a fresh page and lands the widget there, up to
//! `MAX_PAGES`; past that cap, adding is refused with a toast instead
//! (see `AppMsg::AddWidget`).
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
mod i18n_runtime;
mod page_indicator;
mod settings_page;
mod theme;
mod widget_picker;
mod widgets;

use adw::prelude::*;
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
    widget_picker_dialog: adw::Dialog,
    window: adw::ApplicationWindow,
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
}

#[derive(Debug)]
enum AppMsg {
    ToggleFullscreen,
    Retranslate,
    ShowWidgetPicker,
    AddWidget(&'static str),
    GotoSettings,
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

            // Global F11 (fullscreen), Ctrl+Plus/Ctrl+=/numpad + (open the
            // widget picker) and Ctrl+, (go to settings) toggles. The
            // Python app instead registers Gio.SimpleActions with
            // accelerators at the application level (plus a
            // global-shortcuts portal binding for F11 so it works even
            // unfocused) - deferred to a later phase, see
            // settings_page.rs's own note on the shortcuts group.
            add_controller = gtk::EventControllerKey {
                connect_key_pressed[sender] => move |_, key, _, modifiers| {
                    if key == gtk::gdk::Key::F11 {
                        sender.input(AppMsg::ToggleFullscreen);
                        gtk::glib::Propagation::Stop
                    } else if modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                        && matches!(key, gtk::gdk::Key::plus | gtk::gdk::Key::equal | gtk::gdk::Key::KP_Add)
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
        let page_count = max_page_index.map_or(1, |m| m + 1);

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
            real_grids.push(Rc::new(grid));
        }
        for state in &widget_states {
            let Some(grid) = real_grids.get(state.page_index) else {
                eprintln!("xeneon-dashboard: widget {} ignored, page_index {} out of range", state.id, state.page_index);
                continue;
            };
            match widgets::registry::find(&state.kind) {
                Some(descriptor) => grid.restore_widget(state, descriptor.title_key, (descriptor.restore)(&state.content)),
                None => eprintln!("xeneon-dashboard: widget {} has unknown kind {:?}, skipped", state.id, state.kind),
            }
        }

        // Dev-mode-only page: one of every registered kind, to validate
        // move/snap/delete/swipe (and, for Clock, live settings) on
        // demand. Its widgets are never persisted (WidgetGrid::ephemeral)
        // - regenerated fresh in code every launch instead.
        let dev_grid = dev_mode_enabled().then(|| {
            let grid = WidgetGrid::ephemeral(PAGE_W, PAGE_H, real_grids.len());
            // Not persisted (WidgetGrid::ephemeral guards set_custom_name
            // the same as everything else), just sets the in-memory label
            // the page indicator shows for this page.
            grid.set_custom_name(Some(i18n_runtime::t("widgets.dev_page.title")));
            for descriptor in widgets::registry::CATALOG {
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
        let page_indicator = PageIndicator::new(&carousel, &settings_root);
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

        let widget_picker_dialog = widget_picker::build({
            let sender = sender.clone();
            move |kind| sender.input(AppMsg::AddWidget(kind))
        });

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
        let xeneon = gtk::gdk::Display::default().and_then(|d| xeneon_monitor(&d));
        let fullscreened = xeneon.is_some();
        header_bar.set_visible(!fullscreened);

        let model = AppModel {
            fullscreened,
            title: i18n_runtime::t("window.title"),
            header_bar,
            window_title,
            carousel: carousel.clone(),
            settings_root: settings_root.clone(),
            toast_overlay: toast_overlay.clone(),
            widget_picker_dialog,
            window: root.clone(),
            widgets_dir,
            pages_dir,
            real_grids,
            _dev_grid: dev_grid,
            _page_indicator: page_indicator,
            pages_handle,
        };

        i18n_runtime::on_change({
            let sender = sender.clone();
            move || sender.input(AppMsg::Retranslate)
        });

        let widgets = view_output!();

        widgets.overlay.set_child(Some(&carousel));
        widgets.overlay.add_overlay(model._page_indicator.widget());
        widgets.overlay.add_overlay(&model.header_bar);

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

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
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
                self.widget_picker_dialog.present(Some(&self.window));
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
                // already does across existing pages - capped at
                // MAX_PAGES so this can't grow the carousel without bound.
                let target = target.or_else(|| {
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
                    let grid = Rc::new(WidgetGrid::new(PAGE_W, PAGE_H, new_index, self.widgets_dir.clone(), self.pages_dir.clone()));
                    self._page_indicator.register_page(grid.widget(), grid.clone());
                    // Insert right after the last real page - ahead of the
                    // dev-mode test page and/or the settings page, which
                    // always come after real_grids in the carousel (see
                    // the append order at the end of init()).
                    self.carousel.insert(grid.widget(), new_index as i32);
                    self.pages_handle.add_page(grid.clone());
                    self.real_grids.push(grid);
                    Some(new_index)
                });

                if let Some(index) = target {
                    let grid = &self.real_grids[index];
                    grid.add_widget(descriptor.title_key, descriptor.kind, descriptor.size, (descriptor.spawn)());
                    self.carousel.scroll_to(grid.widget(), true);
                }
            }
        }
    }
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

fn main() {
    let app = RelmApp::new("com.n3tlab.XeneonDashboardRust");
    app.run::<AppModel>(());
}
