// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings page: a title, then a small vertical carousel (`gtk::Stack`,
//! not a scrollbar) of "screens", each a fixed 3-column row of "blocks" -
//! invisible layout slots (no border, no fill, see `new_column`) holding
//! whichever `Adw.PreferencesGroup`s fit. A group too tall to fit on
//! screen 1 moves whole onto the matching block on screen 2 rather than
//! being truncated or scrolled past - see `populate`'s own comment for
//! exactly which group lives where. Numbered dots next to the stack
//! (`.xeneon-settings-page-dot`, styled off the same opacity/color knobs
//! as the app's own bottom page indicator) switch between screens, sliding
//! vertically so the gesture never collides with the app's own horizontal
//! page carousel. This whole page is a plain `Gtk.Box`, not
//! `Adw.PreferencesPage` - the page indicator handles the carousel framing,
//! so this is just a title label plus the pager described above.
//!
//! The keyboard-shortcuts group shows F11 (fullscreen) and Ctrl+, (go to
//! settings) as static `Gtk.ShortcutLabel`s, not the Python original's
//! live "bound trigger" row with a reconfigure button - that needs the
//! global-shortcuts portal (a separate later phase) so F11 here only
//! works while the window has focus, unlike the portal-bound version.
//!
//! Not ported yet, deliberately: anything tied to the Shortcuts widget
//! (host-access toggle, the Deja Window extension row) - waits until that
//! widget itself is ported, same as the default-widget-appearance group
//! waits on `WidgetGrid` retaining per-widget appearance handles (a
//! structural change orthogonal to any particular widget, tracked
//! separately).

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use log::{debug, warn};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use crate::appearance_css;
use crate::appearance_popover::{hex_to_rgba, rgba_to_hex};
use crate::config_store;
use crate::grid_widget::WidgetGrid;
use crate::ha_page;
use crate::i18n_runtime as i18n;
use crate::page_indicator::PageIndicator;
use crate::theme;
use xeneon_core::appearance::WidgetAppearance;
use xeneon_core::config::DefaultWidgetAppearance;

/// Handle kept by the caller (`AppModel`) to append a page to the "Pages"
/// rename list *after* `populate` has already built it - needed once
/// dynamic page creation (adding a widget that overflows onto a fresh
/// page, see `main.rs`'s `AppMsg::AddWidget`) can grow the page count at
/// runtime, not just at startup, and to remove one again when it's
/// deleted (`AppMsg::PageEmptied`). `pages` is the same shared list the
/// i18n retranslate closure in `populate` reads from, so a page added or
/// removed here stays correct after a language switch rebuilds the group.
#[derive(Clone)]
pub struct PagesHandle {
    group: adw::PreferencesGroup,
    rows: Rc<RefCell<Vec<adw::ExpanderRow>>>,
    pages: Rc<RefCell<Vec<Rc<WidgetGrid>>>>,
    page_indicator: PageIndicator,
    default_row: adw::ComboRow,
    default_values: Rc<RefCell<Vec<String>>>,
}

impl PagesHandle {
    pub fn add_page(&self, grid: Rc<WidgetGrid>) {
        let row = build_page_row(grid.clone(), self.page_indicator.clone());
        self.group.add(&row);
        self.rows.borrow_mut().push(row);
        self.pages.borrow_mut().push(grid);
        refresh_default_row(&self.default_row, &self.default_values, &self.pages.borrow());
    }

    /// Drops a page's rename row - `rows` and `pages` are built in
    /// lockstep everywhere else (`add_page`, `refresh_pages_group`), so
    /// `grid`'s position in `pages` is also its row's position in `rows`.
    pub fn remove_page(&self, grid: &Rc<WidgetGrid>) {
        let mut pages = self.pages.borrow_mut();
        let Some(idx) = pages.iter().position(|g| Rc::ptr_eq(g, grid)) else { return };
        pages.remove(idx);
        let row = self.rows.borrow_mut().remove(idx);
        self.group.remove(&row);
        // A deleted page named as the saved default is left as-is here
        // (not cleared back to `None`) - whoever resolves `default_page`
        // next (main.rs, at the next startup) already treats a
        // no-longer-existing choice the same as unset, see `Config`'s own
        // doc comment on that field, so this doesn't need to duplicate
        // that fallback.
        refresh_default_row(&self.default_row, &self.default_values, &pages);
    }
}

/// `root` is a bare, already-constructed `gtk::Box` - created by the
/// caller *before* this is called and before the page indicator too, so
/// the indicator can be built against this page's real identity (needed
/// for its "stay revealed on the settings page" and gear-icon behaviour)
/// without a chicken-and-egg problem: this function needs a live
/// `PageIndicator` (to refresh it on rename), and the indicator needs the
/// real settings widget, not a stand-in.
///
/// `pages` is every *renameable* widget page (not the dev-mode test page,
/// not the settings page itself) - mirrors `window.widget_pages()` feeding
/// `SettingsPage.refresh_pages()`.
pub fn populate(root: &gtk::Box, pages: &[Rc<WidgetGrid>], page_indicator: PageIndicator) -> PagesHandle {
    root.set_orientation(gtk::Orientation::Vertical);
    root.set_hexpand(true);
    root.set_vexpand(true);
    root.set_margin_top(24);
    // The page indicator bar stays permanently visible here (unlike every
    // other page, where it auto-hides after a swipe - see
    // page_indicator.rs's own module doc comment), so this page alone
    // needs extra bottom room to keep its own content from running
    // underneath that bar. Only this page's margin grows - a regular
    // widget page is unaffected.
    root.set_margin_bottom(24 + crate::page_indicator::RESERVED_HEIGHT_PX);
    root.set_margin_start(24);
    root.set_margin_end(24);

    let title = gtk::Label::new(Some(&i18n::t("settings.title")));
    title.add_css_class("title-1");
    title.set_halign(gtk::Align::Start);
    title.set_margin_bottom(12);
    root.append(&title);

    // Two fixed-size "screens", each a 3-column row of blocks - not a
    // scrolled single column. A block is purely a layout slot (no border,
    // no fill: invisible on purpose, see `new_column`'s own doc comment)
    // that holds whichever groups fit; a group too tall to fit anywhere on
    // screen 1 moves whole onto the matching block on screen 2 instead of
    // truncating or scrolling - laid out by hand below, block by block.
    // Screen 1: block1 Interface+Language, block2 Theme, block3 Pages
    // (which also carries the Home Assistant row - see its own comment
    // below on why it lives there rather than in a group of its own).
    // Screen 2: block1 default widget appearance, block2 empty (nothing
    // needs it yet), block3 keyboard shortcuts + dev tools.
    let screen1 = new_screen();
    let (screen1_block1, screen1_block2, screen1_block3) = (new_column(), new_column(), new_column());
    screen1.append(&screen1_block1);
    screen1.append(&screen1_block2);
    screen1.append(&screen1_block3);

    let screen2 = new_screen();
    let (screen2_block1, screen2_block2, screen2_block3) = (new_column(), new_column(), new_column());
    screen2.append(&screen2_block1);
    screen2.append(&screen2_block2);
    screen2.append(&screen2_block3);

    // Every real widget page, kept live (not just a startup snapshot) so
    // both a page added at runtime (`PagesHandle::add_page`) and a setting
    // that must apply to every currently-visible page (the app-wide
    // background image below, the "apply appearance to all" button in
    // column 2) always see the full, current list. Declared once up front
    // rather than down in the Pages group's own section since the
    // Interface group needs it too.
    let pages_shared: Rc<RefCell<Vec<Rc<WidgetGrid>>>> = Rc::new(RefCell::new(pages.to_vec()));

    // --- Screen 1, block 1: Interface (page indicator look) ---
    let config = config_store::get();

    let interface_group = adw::PreferencesGroup::new();
    interface_group.set_title(&i18n::t("settings.interface_group"));

    let hide_delay_row = adw::SpinRow::with_range(1.0, 30.0, 1.0);
    hide_delay_row.set_title(&i18n::t("settings.hide_delay_row.title"));
    hide_delay_row.set_subtitle(&i18n::t("settings.hide_delay_row.subtitle"));
    hide_delay_row.set_value(config.indicator_hide_delay_seconds as f64);
    hide_delay_row.connect_value_notify({
        let page_indicator = page_indicator.clone();
        move |row| {
            let seconds = row.value() as u32;
            page_indicator.set_hide_delay_seconds(seconds);
            config_store::update(|c| c.indicator_hide_delay_seconds = seconds);
        }
    });
    interface_group.add(&hide_delay_row);

    let indicator_opacity_row = adw::SpinRow::with_range(10.0, 100.0, 5.0);
    indicator_opacity_row.set_title(&i18n::t("settings.indicator_opacity_row.title"));
    indicator_opacity_row.set_subtitle(&i18n::t("settings.indicator_opacity_row.subtitle"));
    indicator_opacity_row.set_value(config.indicator_opacity as f64);
    interface_group.add(&indicator_opacity_row);

    let indicator_color_row = adw::ActionRow::new();
    indicator_color_row.set_title(&i18n::t("settings.indicator_color_row.title"));
    let indicator_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    let initial_indicator_color = config.indicator_button_color.clone().unwrap_or_else(|| "#ffffff".to_string());
    indicator_color_button.set_rgba(&gtk::gdk::RGBA::parse(&initial_indicator_color).unwrap_or(gtk::gdk::RGBA::WHITE));
    indicator_color_button.set_valign(gtk::Align::Center);
    indicator_color_row.add_suffix(&indicator_color_button);
    let indicator_color_reset_button = gtk::Button::from_icon_name("edit-undo-symbolic");
    indicator_color_reset_button.add_css_class("flat");
    indicator_color_reset_button.set_valign(gtk::Align::Center);
    indicator_color_reset_button.set_tooltip_text(Some(&i18n::t("settings.indicator_color_row.reset_tooltip")));
    indicator_color_row.add_suffix(&indicator_color_reset_button);
    interface_group.add(&indicator_color_row);

    // Full-bleed background image behind every *real* widget page (not the
    // dev-mode test page, not this settings page - see
    // grid_widget.rs's `set_background_image`). A later phase adds a
    // per-page override on top of this app-wide default, hence the
    // subtitle spelling that out now rather than leaving it a surprise.
    let app_background_row = adw::ActionRow::new();
    app_background_row.set_title(&i18n::t("settings.app_background_row.title"));
    app_background_row.set_subtitle(&i18n::t("settings.app_background_row.subtitle"));
    let app_background_choose_button = gtk::Button::with_label(&i18n::t("settings.app_background_row.choose_image"));
    app_background_choose_button.set_valign(gtk::Align::Center);
    app_background_row.add_suffix(&app_background_choose_button);
    let app_background_clear_button = gtk::Button::with_label(&i18n::t("settings.app_background_row.clear_image"));
    app_background_clear_button.set_valign(gtk::Align::Center);
    app_background_row.add_suffix(&app_background_clear_button);
    interface_group.add(&app_background_row);

    // There's no "no background at all" state to offer a way back from -
    // only ever "the bundled default" or "something the user picked" (see
    // `config_store::is_default_background`/`restore_default_background`).
    // So the pair stays a plain toggle: on the default image, "choose" is
    // the only next step; on a custom one, "clear" (which really means
    // "restore the default") is. Mirrors the same show-one-hide-the-other
    // pattern the Python original uses for a page's own background image.
    let sync_app_background_buttons = {
        let app_background_choose_button = app_background_choose_button.clone();
        let app_background_clear_button = app_background_clear_button.clone();
        move || {
            let is_custom = config_store::get()
                .app_background_image_path
                .is_some_and(|path| !config_store::is_default_background(&path));
            app_background_choose_button.set_visible(!is_custom);
            app_background_clear_button.set_visible(is_custom);
        }
    };
    sync_app_background_buttons();

    app_background_choose_button.connect_clicked({
        let pages_shared = pages_shared.clone();
        let sync_app_background_buttons = sync_app_background_buttons.clone();
        move |button| {
            let dialog = gtk::FileDialog::new();
            let image_filter = gtk::FileFilter::new();
            image_filter.set_name(Some(&i18n::t("widgets.appearance.bg_image_filter")));
            for mime in crate::appearance_popover::IMAGE_MIME_TYPES {
                image_filter.add_mime_type(mime);
            }
            let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&image_filter);
            dialog.set_filters(Some(&filters));
            let root = button.root().and_downcast::<gtk::Window>();
            let pages_shared = pages_shared.clone();
            let sync_app_background_buttons = sync_app_background_buttons.clone();
            dialog.open(root.as_ref(), gtk::gio::Cancellable::NONE, move |result| {
                let Ok(file) = result else { return };
                let Some(source) = file.path() else { return };
                // Copied into the config directory rather than kept
                // pointing at wherever the user picked it from (their
                // Pictures folder, a USB stick, ...) - see
                // `xeneon_core::assets::store_asset` for why.
                let path = match xeneon_core::assets::store_asset(&xeneon_core::config::background_dir(), &source) {
                    Ok(path) => path.display().to_string(),
                    Err(err) => {
                        warn!("failed to store background image: {err}");
                        return;
                    }
                };
                config_store::update(|c| c.app_background_image_path = Some(path.clone()));
                for page in pages_shared.borrow().iter() {
                    page.set_background_image(Some(&path));
                }
                sync_app_background_buttons();
            });
        }
    });
    app_background_clear_button.connect_clicked({
        let pages_shared = pages_shared.clone();
        let sync_app_background_buttons = sync_app_background_buttons.clone();
        move |_| {
            // "Retirer" the current (custom) image really means "go back
            // to the default one" - see `config_store::is_default_background`.
            if let Err(err) = config_store::restore_default_background() {
                warn!("failed to restore default background image: {err}");
                return;
            }
            let path = config_store::get().app_background_image_path;
            for page in pages_shared.borrow().iter() {
                page.set_background_image(path.as_deref());
            }
            sync_app_background_buttons();
        }
    });

    // Both the opacity spin row and the color button/reset feed the same
    // "reapply the indicator's style" step, since set_style() takes both
    // together - a small shared closure avoids repeating that pairing.
    let apply_indicator_style = {
        let page_indicator = page_indicator.clone();
        let indicator_opacity_row = indicator_opacity_row.clone();
        let indicator_color_button = indicator_color_button.clone();
        move || {
            let opacity = indicator_opacity_row.value() as u32;
            let has_custom_color = config_store::get().indicator_button_color.is_some();
            let color_hex = has_custom_color.then(|| {
                let rgba = indicator_color_button.rgba();
                format!(
                    "#{:02x}{:02x}{:02x}",
                    (rgba.red() * 255.0).round() as u8,
                    (rgba.green() * 255.0).round() as u8,
                    (rgba.blue() * 255.0).round() as u8
                )
            });
            page_indicator.set_style(opacity, color_hex.as_deref());
        }
    };
    indicator_opacity_row.connect_value_notify({
        let apply_indicator_style = apply_indicator_style.clone();
        move |row| {
            let opacity = row.value() as u32;
            config_store::update(|c| c.indicator_opacity = opacity);
            apply_indicator_style();
        }
    });
    indicator_color_button.connect_rgba_notify({
        let apply_indicator_style = apply_indicator_style.clone();
        move |button| {
            let rgba = button.rgba();
            let hex = format!(
                "#{:02x}{:02x}{:02x}",
                (rgba.red() * 255.0).round() as u8,
                (rgba.green() * 255.0).round() as u8,
                (rgba.blue() * 255.0).round() as u8
            );
            config_store::update(|c| c.indicator_button_color = Some(hex));
            apply_indicator_style();
        }
    });
    indicator_color_reset_button.connect_clicked({
        let apply_indicator_style = apply_indicator_style.clone();
        let indicator_color_button = indicator_color_button.clone();
        move |_| {
            config_store::update(|c| c.indicator_button_color = None);
            indicator_color_button.set_rgba(&gtk::gdk::RGBA::WHITE);
            apply_indicator_style();
        }
    });
    apply_indicator_style();

    screen1_block1.append(&interface_group);

    // --- Screen 1, block 1 (cont'd): Language; block 2: Theme (accent) ---
    let language_group = adw::PreferencesGroup::new();
    language_group.set_title(&i18n::t("settings.language_group"));

    let language_row = adw::ComboRow::new();
    language_row.set_title(&i18n::t("settings.language_row.title"));

    let languages = i18n::available_languages();
    let current = i18n::current_language();

    let names = gtk::StringList::new(&languages.iter().map(|(_, name)| name.as_str()).collect::<Vec<_>>());
    language_row.set_model(Some(&names));
    if let Some(index) = languages.iter().position(|(code, _)| *code == current) {
        language_row.set_selected(index as u32);
    }

    language_row.connect_selected_notify({
        let languages = languages.clone();
        move |row| {
            let index = row.selected() as usize;
            if let Some((code, _)) = languages.get(index) {
                i18n::set_language(code);
                config_store::update(|c| c.language = code.clone());
            }
        }
    });

    language_group.add(&language_row);
    screen1_block1.append(&language_group);

    let theme_group = adw::PreferencesGroup::new();
    theme_group.set_title(&i18n::t("settings.theme_group.title"));

    let accent_row = adw::ActionRow::new();
    accent_row.set_title(&i18n::t("settings.theme_group.accent_row.title"));
    let accent_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    accent_box.set_valign(gtk::Align::Center);
    let accent_buttons: Rc<RefCell<Vec<(gtk::Button, &'static str, &'static str)>>> = Rc::new(RefCell::new(Vec::new()));
    let follow_system_switch = gtk::Switch::new();
    for (key, hex) in theme::ACCENT_PRESETS {
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class(theme::SWATCH_CSS_CLASS);
        button.add_css_class(&format!("{}-{key}", theme::SWATCH_CSS_CLASS));
        button.set_tooltip_text(Some(&i18n::t(&format!("settings.theme_group.accent.{key}"))));
        button.connect_clicked({
            let follow_system_switch = follow_system_switch.clone();
            move |_| {
                if follow_system_switch.is_active() {
                    // Picking a preset explicitly overrides "follow
                    // system" - matches how GNOME's own accent picker
                    // treats a manual pick.
                    follow_system_switch.set_active(false);
                }
                theme::apply_accent(hex);
                config_store::update(|c| c.accent_color = hex.to_string());
            }
        });
        accent_box.append(&button);
        accent_buttons.borrow_mut().push((button, key, hex));
    }
    accent_row.add_suffix(&accent_box);
    theme_group.add(&accent_row);

    let follow_system_row = adw::ActionRow::new();
    follow_system_row.set_title(&i18n::t("settings.theme_group.follow_system_row.title"));
    follow_system_row.set_subtitle(&i18n::t("settings.theme_group.follow_system_row.subtitle"));
    follow_system_switch.set_active(config.accent_follow_system);
    follow_system_switch.set_valign(gtk::Align::Center);
    follow_system_switch.set_sensitive(theme::system_accent_supported());
    follow_system_switch.connect_active_notify(|s| {
        let following = s.is_active();
        config_store::update(|c| c.accent_follow_system = following);
        if following {
            if let Some(hex) = theme::system_accent_hex() {
                theme::apply_accent(&hex);
            }
        }
    });
    follow_system_row.add_suffix(&follow_system_switch);
    theme_group.add(&follow_system_row);

    let sync_accent_controls = {
        let accent_buttons = accent_buttons.clone();
        move || {
            let current = theme::current_accent().to_lowercase();
            for (button, _key, hex) in accent_buttons.borrow().iter() {
                if hex.to_lowercase() == current {
                    button.add_css_class(theme::SWATCH_SELECTED_CSS_CLASS);
                } else {
                    button.remove_css_class(theme::SWATCH_SELECTED_CSS_CLASS);
                }
            }
        }
    };
    sync_accent_controls();
    theme::on_change(sync_accent_controls);
    // Re-applies the system accent if it changes while "follow system" is
    // on - connected once regardless of how many settings pages exist.
    theme::connect_system_accent_changed(|| {
        if config_store::get().accent_follow_system {
            if let Some(hex) = theme::system_accent_hex() {
                theme::apply_accent(&hex);
            }
        }
    });

    screen1_block2.append(&theme_group);

    // --- Screen 1, block 3: Pages ---
    let pages_group = adw::PreferencesGroup::new();
    pages_group.set_title(&i18n::t("settings.pages_group.title"));
    screen1_block3.append(&pages_group);

    // The Home Assistant page's row - added here, first, rather than in a
    // group of its own (an earlier version had one - see git history):
    // it's conceptually just another entry in this same "which pages
    // exist" list, and its own carousel page is likewise always first
    // (see ha_page.rs's append order) - one list, one mental model,
    // instead of a second one to keep in sync with it. Same `ExpanderRow`
    // shape as `build_page_row` below, minus the rename row (this isn't a
    // renameable widget page - `set_icon_name`-equivalent via
    // `add_prefix` is the whole identity it needs, no name to edit) and
    // using `show-enable-switch` (a built-in `AdwExpanderRow` feature)
    // for the on/off switch instead of a separate suffix `gtk::Switch` -
    // it already ties directly into whether the row can even expand,
    // which is exactly "no URL to look at until this is on" for free.
    let ha_row = adw::ExpanderRow::new();
    ha_row.set_title(&i18n::t("settings.ha_group.title"));
    let ha_row_icon = gtk::Image::from_icon_name("user-home-symbolic");
    ha_row.add_prefix(&ha_row_icon);
    ha_row.set_show_enable_switch(true);
    ha_row.set_enable_expansion(config.ha_page_enabled);
    ha_row.set_expanded(config.ha_page_enabled);

    let ha_url_row = adw::EntryRow::new();
    ha_url_row.set_title(&i18n::t("settings.ha_group.url_row.title"));
    ha_url_row.set_text(config.ha_page_url.as_deref().unwrap_or(""));
    ha_url_row.set_show_apply_button(true);
    // Flags a value already on disk that wouldn't pass `is_http_url` today -
    // a hand-edited or pre-this-change config.json - the same way a fresh
    // invalid entry gets flagged below, rather than silently showing as if
    // nothing were wrong.
    if config.ha_page_url.as_deref().is_some_and(|url| !is_http_url(url)) {
        ha_url_row.add_css_class("error");
        ha_url_row.set_tooltip_text(Some(&i18n::t("settings.ha_group.url_row.invalid")));
    }

    // Reachability badge: purely informational (never blocks saving the
    // URL, see the design discussion in the memory system - a kiosk can
    // legitimately be set up before Home Assistant itself is reachable on
    // the network), so it's a suffix on the row rather than anything tied
    // to `connect_apply`'s accept/reject flow above. `ha_ping_spinner`
    // spins while a check is in flight; `ha_ping_status_icon` shows the
    // outcome once it lands - never both visible at once.
    let ha_ping_spinner = gtk::Spinner::new();
    ha_ping_spinner.set_valign(gtk::Align::Center);
    ha_ping_spinner.set_visible(false);
    ha_url_row.add_suffix(&ha_ping_spinner);
    let ha_ping_status_icon = gtk::Image::new();
    ha_ping_status_icon.set_valign(gtk::Align::Center);
    ha_ping_status_icon.set_visible(false);
    ha_url_row.add_suffix(&ha_ping_status_icon);

    ha_row.add_row(&ha_url_row);
    pages_group.add(&ha_row);

    // Bumped on every new ping and compared after the blocking call
    // returns - a still-in-flight ping from a superseded URL becomes a
    // no-op when it lands, same technique (and same reason) as
    // `weather.rs`'s own `fetch_generation`/`search_generation`.
    let ha_ping_generation = Rc::new(Cell::new(0u64));

    let hide_ha_status = {
        let ha_ping_spinner = ha_ping_spinner.clone();
        let ha_ping_status_icon = ha_ping_status_icon.clone();
        move || {
            ha_ping_spinner.set_visible(false);
            ha_ping_spinner.set_spinning(false);
            ha_ping_status_icon.set_visible(false);
        }
    };

    let run_ha_ping = {
        let ha_ping_spinner = ha_ping_spinner.clone();
        let ha_ping_status_icon = ha_ping_status_icon.clone();
        let ha_ping_generation = ha_ping_generation.clone();
        move |url: String| {
            ha_ping_status_icon.set_visible(false);
            ha_ping_spinner.set_visible(true);
            ha_ping_spinner.set_spinning(true);
            let generation = ha_ping_generation.get() + 1;
            ha_ping_generation.set(generation);
            debug!("pinging Home Assistant URL {url:?}");

            let ha_ping_spinner = ha_ping_spinner.clone();
            let ha_ping_status_icon = ha_ping_status_icon.clone();
            let ha_ping_generation = ha_ping_generation.clone();
            glib::spawn_future_local(async move {
                let url_for_probe = url.clone();
                let reachable = gio::spawn_blocking(move || ping_ha_url(&url_for_probe)).await.unwrap_or(false);
                if generation != ha_ping_generation.get() {
                    debug!("ping for {url:?} superseded, discarding");
                    return;
                }
                ha_ping_spinner.set_visible(false);
                ha_ping_spinner.set_spinning(false);
                ha_ping_status_icon.set_visible(true);
                if reachable {
                    ha_ping_status_icon.set_icon_name(Some("emblem-ok-symbolic"));
                    ha_ping_status_icon.set_tooltip_text(Some(&i18n::t("settings.ha_group.url_row.reachable")));
                } else {
                    ha_ping_status_icon.set_icon_name(Some("dialog-warning-symbolic"));
                    ha_ping_status_icon.set_tooltip_text(Some(&i18n::t("settings.ha_group.url_row.unreachable")));
                }
            });
        }
    };

    // Checked once up front too, not just on the next `connect_apply` -
    // otherwise reopening settings on an already-configured page would
    // show no badge at all until the user re-applies the same URL.
    if config.ha_page_enabled {
        if let Some(url) = config.ha_page_url.as_deref() {
            if is_http_url(url) {
                run_ha_ping(url.to_string());
            }
        }
    }

    ha_row.connect_enable_expansion_notify({
        let root = root.clone();
        move |row| {
            let enabled = row.enables_expansion();
            config_store::update(|c| c.ha_page_enabled = enabled);
            if !enabled {
                // Tearing the carousel page back down needs the same full
                // relaunch building it did - see ha_page.rs's own doc
                // comment on why this isn't live carousel surgery.
                confirm_ha_restart(&root);
                return;
            }
            // Turning it on is only immediately actionable if a URL is
            // already on file (re-enabling after a previous disable,
            // say) - relaunch now so the page that URL points to
            // actually appears. Enabling with nothing configured yet
            // (the common first-time case) has nothing to relaunch
            // *for* - see `ha_url_row.connect_apply` below, which
            // relaunches instead once a URL is actually entered.
            let has_valid_url = config_store::get().ha_page_url.as_deref().is_some_and(is_http_url);
            if has_valid_url {
                confirm_ha_restart(&root);
            }
        }
    });
    ha_url_row.connect_apply({
        let root = root.clone();
        move |row| {
            let text = row.text().to_string();
            let trimmed = text.trim();
            // Empty text means "not configured yet", same as a freshly
            // installed app - stored as `None`, not an empty string, so
            // `Option::is_some()` stays a reliable "has a URL" check for
            // whatever reads this later (the page's own WebView load logic).
            if trimmed.is_empty() {
                row.remove_css_class("error");
                row.set_tooltip_text(None);
                hide_ha_status();
                config_store::update(|c| c.ha_page_url = None);
                return;
            }
            // Restricted to http(s) so a typo or a hand-edited config.json
            // can never hand WebKit's `load_uri` a `file://` (local
            // filesystem read) or `javascript:`/`data:` URI (arbitrary
            // script in the page's context) - this is the only gate that
            // check ever gets on the settings side, so it has to reject
            // here rather than merely warn (ha_page.rs's own `build`
            // re-checks independently too, see its own doc comment, since
            // config.json is a plain user-editable file).
            if !is_http_url(trimmed) {
                row.add_css_class("error");
                row.set_tooltip_text(Some(&i18n::t("settings.ha_group.url_row.invalid")));
                hide_ha_status();
                return;
            }
            row.remove_css_class("error");
            row.set_tooltip_text(None);
            config_store::update(|c| c.ha_page_url = Some(trimmed.to_string()));
            run_ha_ping(trimmed.to_string());
            // Two different situations look the same here (a valid URL
            // just applied) but need different handling: editing an
            // already-live page's address just needs a live navigation
            // (`ha_page::set_url`, no restart) - but the very first URL
            // entered right after flipping the enable switch on is what
            // actually has to build the real carousel page in the first
            // place, which only a relaunch can do (see
            // `ha_row.connect_enable_expansion_notify` above for why that
            // switch alone doesn't relaunch in this case).
            if config_store::get().ha_page_enabled && !ha_page::is_live() {
                confirm_ha_restart(&root);
            } else {
                ha_page::set_url(trimmed);
            }
        }
    });

    // "Page par défaut": which carousel page actually opens at launch -
    // independent of carousel *order* (the Home Assistant page, when
    // enabled, is always leftmost regardless of this choice - see
    // ha_page.rs's own append order) since "Home Assistant always
    // leftmost" and "Page 1 by default" turned out to be two different
    // needs (see the design discussion in the memory system). Options are
    // rebuilt (see `refresh_default_row`) rather than fixed at startup,
    // same rebuild-on-change spirit as `refresh_pages_group` just below -
    // a page added/removed/renamed, or Home Assistant enabled/disabled,
    // all change what belongs in this list.
    let default_row = adw::ComboRow::new();
    default_row.set_title(&i18n::t("settings.pages_group.default_row.title"));
    pages_group.add(&default_row);
    let default_values: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    refresh_default_row(&default_row, &default_values, &pages_shared.borrow());
    default_row.connect_selected_notify({
        let default_values = default_values.clone();
        move |row| {
            let value = default_values.borrow().get(row.selected() as usize).cloned();
            config_store::update(|c| c.default_page = value);
        }
    });

    let page_rows: Rc<RefCell<Vec<adw::ExpanderRow>>> = Rc::new(RefCell::new(Vec::new()));
    refresh_pages_group(&pages_group, &page_rows, &pages_shared.borrow(), &page_indicator);

    // --- Screen 2, block 1: Default widget appearance ---
    // Applied to every widget newly added from here on (see
    // grid_widget.rs's `add_widget`); an already-customized widget's own
    // look stays untouched unless the "apply to all" button below is
    // used - mirrors `has_customizations()` / `defaults_touched_dict()`
    // in the Python original's widget_appearance.py.
    appearance_css::ensure_reset_button_css_installed();

    let appearance_group = adw::PreferencesGroup::new();
    appearance_group.set_title(&i18n::t("settings.appearance_group.title"));
    appearance_group.set_description(Some(&i18n::t("settings.appearance_group.subtitle")));

    let defaults = config.default_widget_appearance.clone();

    let appearance_opacity_row = adw::ActionRow::new();
    appearance_opacity_row.set_title(&i18n::t("widgets.appearance.opacity"));
    let appearance_opacity_scale = gtk::Scale::new(gtk::Orientation::Horizontal, gtk::Adjustment::NONE);
    appearance_opacity_scale.set_range(0.0, 100.0);
    appearance_opacity_scale.set_value(defaults.opacity * 100.0);
    appearance_opacity_scale.set_draw_value(true);
    appearance_opacity_scale.set_value_pos(gtk::PositionType::Right);
    appearance_opacity_scale.set_size_request(140, -1);
    appearance_opacity_scale.set_hexpand(true);
    appearance_opacity_scale.set_valign(gtk::Align::Center);
    appearance_opacity_row.add_suffix(&appearance_opacity_scale);
    appearance_group.add(&appearance_opacity_row);

    let appearance_bg_color_row = adw::ActionRow::new();
    appearance_bg_color_row.set_title(&i18n::t("widgets.appearance.bg_color"));
    let appearance_bg_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    appearance_bg_color_button.set_rgba(&hex_to_rgba(&defaults.bg_color));
    appearance_bg_color_button.set_valign(gtk::Align::Center);
    appearance_bg_color_row.add_suffix(&appearance_bg_color_button);
    appearance_group.add(&appearance_bg_color_row);

    let appearance_border_row = adw::ActionRow::new();
    appearance_border_row.set_title(&i18n::t("widgets.appearance.border_enabled"));
    let appearance_border_switch = gtk::Switch::new();
    appearance_border_switch.set_active(defaults.border_enabled);
    appearance_border_switch.set_valign(gtk::Align::Center);
    appearance_border_row.add_suffix(&appearance_border_switch);
    appearance_group.add(&appearance_border_row);

    let appearance_border_width_row = adw::SpinRow::with_range(1.0, 12.0, 1.0);
    appearance_border_width_row.set_title(&i18n::t("widgets.appearance.border_width"));
    appearance_border_width_row.set_value(defaults.border_width as f64);
    appearance_group.add(&appearance_border_width_row);

    let appearance_border_color_row = adw::ActionRow::new();
    appearance_border_color_row.set_title(&i18n::t("widgets.appearance.border_color"));
    let appearance_border_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    appearance_border_color_button.set_rgba(&hex_to_rgba(&defaults.border_color));
    appearance_border_color_button.set_valign(gtk::Align::Center);
    appearance_border_color_row.add_suffix(&appearance_border_color_button);
    appearance_group.add(&appearance_border_color_row);

    // Pushes the 5 controls above into `config.default_widget_appearance`
    // as one unit on every change - saved as a whole so a partial write
    // never leaves the nested struct half-updated (config.rs's
    // `#[serde(default)]` merge only ever sees a complete value here).
    let save_appearance_defaults = {
        let appearance_opacity_scale = appearance_opacity_scale.clone();
        let appearance_bg_color_button = appearance_bg_color_button.clone();
        let appearance_border_switch = appearance_border_switch.clone();
        let appearance_border_width_row = appearance_border_width_row.clone();
        let appearance_border_color_button = appearance_border_color_button.clone();
        move || {
            let updated = DefaultWidgetAppearance {
                opacity: appearance_opacity_scale.value() / 100.0,
                bg_color: rgba_to_hex(&appearance_bg_color_button.rgba()),
                border_enabled: appearance_border_switch.is_active(),
                border_width: appearance_border_width_row.value() as u32,
                border_color: rgba_to_hex(&appearance_border_color_button.rgba()),
            };
            config_store::update(|c| c.default_widget_appearance = updated);
        }
    };
    appearance_opacity_scale.connect_value_changed({
        let save_appearance_defaults = save_appearance_defaults.clone();
        move |_| save_appearance_defaults()
    });
    appearance_bg_color_button.connect_rgba_notify({
        let save_appearance_defaults = save_appearance_defaults.clone();
        move |_| save_appearance_defaults()
    });
    appearance_border_switch.connect_active_notify({
        let save_appearance_defaults = save_appearance_defaults.clone();
        move |_| save_appearance_defaults()
    });
    appearance_border_width_row.connect_value_notify({
        let save_appearance_defaults = save_appearance_defaults.clone();
        move |_| save_appearance_defaults()
    });
    appearance_border_color_button.connect_rgba_notify({
        let save_appearance_defaults = save_appearance_defaults.clone();
        move |_| save_appearance_defaults()
    });

    let appearance_apply_row = adw::ActionRow::new();
    appearance_apply_row.set_title(&i18n::t("settings.appearance_apply_row.title"));
    appearance_apply_row.set_subtitle(&i18n::t("settings.appearance_apply_row.subtitle"));
    let appearance_apply_button = gtk::Button::with_label(&i18n::t("settings.appearance_apply_row.button"));
    appearance_apply_button.add_css_class("xeneon-reset-button");
    appearance_apply_button.set_valign(gtk::Align::Center);
    appearance_apply_row.add_suffix(&appearance_apply_button);
    appearance_group.add(&appearance_apply_row);

    // The click itself has no other visible effect (the button stays red
    // before and after), so swap the label to a confirmation and disable
    // it briefly - the only sign the overwrite actually happened. Reads
    // `pages_shared`, not the `pages` parameter, so a page created after
    // startup (overflow, see PagesHandle::add_page) is included too.
    appearance_apply_button.connect_clicked({
        let pages_shared = pages_shared.clone();
        let appearance_apply_button = appearance_apply_button.clone();
        move |_| {
            let appearance = WidgetAppearance::from_config_default(&config_store::get().default_widget_appearance);
            for page in pages_shared.borrow().iter() {
                page.apply_appearance_to_all(&appearance);
            }
            appearance_apply_button.set_label(&i18n::t("settings.appearance_apply_row.button_applied"));
            appearance_apply_button.set_sensitive(false);
            let appearance_apply_button = appearance_apply_button.clone();
            gtk::glib::timeout_add_seconds_local(2, move || {
                appearance_apply_button.set_label(&i18n::t("settings.appearance_apply_row.button"));
                appearance_apply_button.set_sensitive(true);
                gtk::glib::ControlFlow::Break
            });
        }
    });

    screen2_block1.append(&appearance_group);

    // --- Screen 2, block 3: Keyboard shortcuts + dev tools ---
    let shortcuts_group = adw::PreferencesGroup::new();
    shortcuts_group.set_title(&i18n::t("settings.shortcuts_group"));

    let fullscreen_row = adw::ActionRow::new();
    fullscreen_row.set_title(&i18n::t("settings.fullscreen_row.title"));
    fullscreen_row.set_subtitle(&i18n::t("settings.fullscreen_row.subtitle"));
    let fullscreen_shortcut_label = gtk::ShortcutLabel::new("F11");
    fullscreen_shortcut_label.set_valign(gtk::Align::Center);
    fullscreen_row.add_suffix(&fullscreen_shortcut_label);
    shortcuts_group.add(&fullscreen_row);

    let goto_row = adw::ActionRow::new();
    goto_row.set_title(&i18n::t("settings.goto_row.title"));
    let goto_shortcut_label = gtk::ShortcutLabel::new("<Primary>comma");
    goto_shortcut_label.set_valign(gtk::Align::Center);
    goto_row.add_suffix(&goto_shortcut_label);
    shortcuts_group.add(&goto_row);

    screen2_block3.append(&shortcuts_group);

    // Always visible (unlike dev_group below) - otherwise there'd be no
    // way to turn dev mode *on* from the UI at all, only off (the rest of
    // the dev tools only show once it's already on).
    let dev_toggle_group = adw::PreferencesGroup::new();
    let dev_toggle_row = adw::ActionRow::new();
    dev_toggle_row.set_title(&i18n::t("settings.dev_toggle.title"));
    dev_toggle_row.set_subtitle(&i18n::t("settings.dev_toggle.subtitle"));
    let dev_switch = gtk::Switch::new();
    dev_switch.set_active(crate::dev_mode_enabled());
    dev_switch.set_valign(gtk::Align::Center);
    dev_switch.connect_state_set(|_, active| {
        relaunch(active);
        gtk::glib::Propagation::Proceed
    });
    dev_toggle_row.add_suffix(&dev_switch);
    dev_toggle_group.add(&dev_toggle_row);
    screen2_block3.append(&dev_toggle_group);

    // Dev-only: restarting is otherwise just "close the window and run
    // the binary again by hand", which got old fast while iterating on
    // this very carousel/settings page - not something an end user
    // building their kiosk dashboard would ever want a button for.
    let dev_group = crate::dev_mode_enabled().then(|| {
        let group = adw::PreferencesGroup::new();
        group.set_title(&i18n::t("settings.dev_group.title"));

        let restart_row = adw::ActionRow::new();
        restart_row.set_title(&i18n::t("settings.dev_group.restart_button"));
        let restart_button = gtk::Button::from_icon_name("view-refresh-symbolic");
        restart_button.set_valign(gtk::Align::Center);
        restart_button.add_css_class("flat");
        restart_button.connect_clicked(|_| relaunch(crate::dev_mode_enabled()));
        restart_row.add_suffix(&restart_button);
        restart_row.set_activatable_widget(Some(&restart_button));

        group.add(&restart_row);
        (group, restart_row)
    });
    if let Some((group, _)) = &dev_group {
        screen2_block3.append(group);
    }

    // One vertical carousel for the whole settings screen - not a
    // scrollbar, and not one carousel per block. Sliding vertically
    // (rather than the app's own horizontal page carousel) keeps the two
    // gestures unambiguous. GtkStack's SlideUpDown transition picks the
    // slide direction itself from each child's position in the stack, so
    // screen1 -> screen2 slides up and back down, matching the numbered
    // dots below.
    let pager = gtk::Stack::new();
    pager.set_transition_type(gtk::StackTransitionType::SlideUpDown);
    pager.set_transition_duration(220);
    pager.set_hexpand(true);
    pager.set_vexpand(true);
    pager.add_named(&screen1, Some("screen1"));
    pager.add_named(&screen2, Some("screen2"));
    pager.set_visible_child_name("screen1");

    let dot1 = gtk::Button::with_label("1");
    dot1.add_css_class("xeneon-settings-page-dot");
    dot1.add_css_class("active");
    let dot2 = gtk::Button::with_label("2");
    dot2.add_css_class("xeneon-settings-page-dot");
    dot1.connect_clicked({
        let pager = pager.clone();
        let dot2 = dot2.clone();
        move |dot1| {
            pager.set_visible_child_name("screen1");
            dot1.add_css_class("active");
            dot2.remove_css_class("active");
        }
    });
    dot2.connect_clicked({
        let pager = pager.clone();
        let dot1 = dot1.clone();
        move |dot2| {
            pager.set_visible_child_name("screen2");
            dot2.add_css_class("active");
            dot1.remove_css_class("active");
        }
    });
    let page_dots = gtk::Box::new(gtk::Orientation::Vertical, 4);
    page_dots.set_valign(gtk::Align::Center);
    page_dots.append(&dot1);
    page_dots.append(&dot2);

    let pager_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    pager_row.set_hexpand(true);
    pager_row.set_vexpand(true);
    pager_row.append(&pager);
    pager_row.append(&page_dots);
    root.append(&pager_row);

    i18n::on_change({
        let title = title.clone();
        let interface_group = interface_group.clone();
        let hide_delay_row = hide_delay_row.clone();
        let indicator_opacity_row = indicator_opacity_row.clone();
        let indicator_color_row = indicator_color_row.clone();
        let indicator_color_reset_button = indicator_color_reset_button.clone();
        let app_background_row = app_background_row.clone();
        let app_background_choose_button = app_background_choose_button.clone();
        let app_background_clear_button = app_background_clear_button.clone();
        let language_group = language_group.clone();
        let language_row = language_row.clone();
        let theme_group = theme_group.clone();
        let accent_row = accent_row.clone();
        let accent_buttons = accent_buttons.clone();
        let follow_system_row = follow_system_row.clone();
        let dev_group = dev_group.clone();
        let pages_group = pages_group.clone();
        let default_row = default_row.clone();
        let default_values = default_values.clone();
        let page_rows = page_rows.clone();
        let pages_shared = pages_shared.clone();
        let page_indicator = page_indicator.clone();
        let shortcuts_group = shortcuts_group.clone();
        let fullscreen_row = fullscreen_row.clone();
        let goto_row = goto_row.clone();
        let appearance_group = appearance_group.clone();
        let appearance_opacity_row = appearance_opacity_row.clone();
        let appearance_bg_color_row = appearance_bg_color_row.clone();
        let appearance_border_row = appearance_border_row.clone();
        let appearance_border_width_row = appearance_border_width_row.clone();
        let appearance_border_color_row = appearance_border_color_row.clone();
        let appearance_apply_row = appearance_apply_row.clone();
        let appearance_apply_button = appearance_apply_button.clone();
        let ha_row = ha_row.clone();
        let ha_url_row = ha_url_row.clone();
        move || {
            title.set_label(&i18n::t("settings.title"));
            interface_group.set_title(&i18n::t("settings.interface_group"));
            hide_delay_row.set_title(&i18n::t("settings.hide_delay_row.title"));
            hide_delay_row.set_subtitle(&i18n::t("settings.hide_delay_row.subtitle"));
            indicator_opacity_row.set_title(&i18n::t("settings.indicator_opacity_row.title"));
            indicator_opacity_row.set_subtitle(&i18n::t("settings.indicator_opacity_row.subtitle"));
            indicator_color_row.set_title(&i18n::t("settings.indicator_color_row.title"));
            indicator_color_reset_button.set_tooltip_text(Some(&i18n::t("settings.indicator_color_row.reset_tooltip")));
            app_background_row.set_title(&i18n::t("settings.app_background_row.title"));
            app_background_row.set_subtitle(&i18n::t("settings.app_background_row.subtitle"));
            app_background_choose_button.set_label(&i18n::t("settings.app_background_row.choose_image"));
            app_background_clear_button.set_label(&i18n::t("settings.app_background_row.clear_image"));
            language_group.set_title(&i18n::t("settings.language_group"));
            language_row.set_title(&i18n::t("settings.language_row.title"));
            // The language names themselves don't change (each is spelled
            // in its own tongue, e.g. "Français" stays "Français" no
            // matter the active language), so the dropdown's model/
            // selection doesn't need rebuilding here - only these labels
            // do, unlike ClockSettings's city dropdown which retranslates
            // its *entries* because those are ordinary translated strings.
            theme_group.set_title(&i18n::t("settings.theme_group.title"));
            accent_row.set_title(&i18n::t("settings.theme_group.accent_row.title"));
            for (button, key, _hex) in accent_buttons.borrow().iter() {
                button.set_tooltip_text(Some(&i18n::t(&format!("settings.theme_group.accent.{key}"))));
            }
            follow_system_row.set_title(&i18n::t("settings.theme_group.follow_system_row.title"));
            follow_system_row.set_subtitle(&i18n::t("settings.theme_group.follow_system_row.subtitle"));
            pages_group.set_title(&i18n::t("settings.pages_group.title"));
            default_row.set_title(&i18n::t("settings.pages_group.default_row.title"));
            // Same reasoning as the page list just below: its labels
            // ("Home Assistant", "Page N"...) are translated strings too.
            refresh_default_row(&default_row, &default_values, &pages_shared.borrow());
            // Rebuilding is the simplest way to keep a dynamic-length,
            // per-row-translated list in sync - same call Python's
            // _retranslate() makes to refresh_pages() for the same reason.
            refresh_pages_group(&pages_group, &page_rows, &pages_shared.borrow(), &page_indicator);
            shortcuts_group.set_title(&i18n::t("settings.shortcuts_group"));
            fullscreen_row.set_title(&i18n::t("settings.fullscreen_row.title"));
            fullscreen_row.set_subtitle(&i18n::t("settings.fullscreen_row.subtitle"));
            goto_row.set_title(&i18n::t("settings.goto_row.title"));
            appearance_group.set_title(&i18n::t("settings.appearance_group.title"));
            appearance_group.set_description(Some(&i18n::t("settings.appearance_group.subtitle")));
            appearance_opacity_row.set_title(&i18n::t("widgets.appearance.opacity"));
            appearance_bg_color_row.set_title(&i18n::t("widgets.appearance.bg_color"));
            appearance_border_row.set_title(&i18n::t("widgets.appearance.border_enabled"));
            appearance_border_width_row.set_title(&i18n::t("widgets.appearance.border_width"));
            appearance_border_color_row.set_title(&i18n::t("widgets.appearance.border_color"));
            appearance_apply_row.set_title(&i18n::t("settings.appearance_apply_row.title"));
            appearance_apply_row.set_subtitle(&i18n::t("settings.appearance_apply_row.subtitle"));
            appearance_apply_button.set_label(&i18n::t("settings.appearance_apply_row.button"));
            ha_row.set_title(&i18n::t("settings.ha_group.title"));
            ha_url_row.set_title(&i18n::t("settings.ha_group.url_row.title"));
            if let Some((group, row)) = &dev_group {
                group.set_title(&i18n::t("settings.dev_group.title"));
                row.set_title(&i18n::t("settings.dev_group.restart_button"));
            }
        }
    });

    i18n::on_change({
        let dev_toggle_row = dev_toggle_row.clone();
        move || {
            dev_toggle_row.set_title(&i18n::t("settings.dev_toggle.title"));
            dev_toggle_row.set_subtitle(&i18n::t("settings.dev_toggle.subtitle"));
        }
    });

    PagesHandle { group: pages_group, rows: page_rows, pages: pages_shared, page_indicator, default_row, default_values }
}

/// One settings-page "screen": a fixed 3-column row of blocks, exactly
/// like the other screen - homogeneous widths, same height (whatever the
/// `gtk::Stack` allocates it, see `populate`'s `pager`). Content that
/// doesn't fit here moves to the matching block on the other screen
/// instead of scrolling or stretching this one.
fn new_screen() -> gtk::Box {
    let screen = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    screen.set_homogeneous(true);
    screen.set_hexpand(true);
    screen
}

/// One block: a fixed-size layout slot for a handful of
/// `Adw.PreferencesGroup`s, not a widget in its own right - no border, no
/// fill, so it's invisible to the user exactly like the plain column
/// boxes this replaces. `set_overflow(Hidden)` is a clip-to-bounds safety
/// net, not the primary correctness mechanism - the primary one is
/// curating which groups go in which block/screen (see `populate`'s own
/// comment on the layout) so real content already fits without needing
/// to clip anything.
fn new_column() -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 24);
    column.set_valign(gtk::Align::Start);
    column.set_overflow(gtk::Overflow::Hidden);
    column
}

/// Whether `text` is safe to eventually hand to WebKit's `load_uri` for the
/// Home Assistant page - a plain scheme allowlist, not a full URL parser
/// (no new dependency needed for that: see the memory system's preference
/// for minimal deps). `http`/`https` is exactly what a Home Assistant
/// dashboard ever needs; a `file://` or `javascript:`/`data:` value getting
/// this far is always either a typo or a hand-edited `config.json`, never a
/// legitimate dashboard URL, so this rejects those outright rather than
/// trying to sanitize them.
fn is_http_url(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Blocking reachability probe for the Home Assistant URL - runs on GIO's
/// thread pool via `gio::spawn_blocking`, never on the GTK main thread,
/// same pattern as `weather.rs`'s own blocking HTTP calls. Purely
/// informational (see the design discussion in the memory system: a kiosk
/// can be set up before Home Assistant is reachable on the network, so
/// this never blocks saving the URL) - it only drives the small
/// reachable/unreachable badge next to the field.
///
/// Any actual HTTP response counts as reachable, even a 4xx/5xx one (Home
/// Assistant behind a reverse proxy, or an auth wall, can easily answer
/// with one of those) - only a transport-level failure (DNS, connection
/// refused, timeout) means the address itself isn't reachable.
fn ping_ha_url(url: &str) -> bool {
    const PING_TIMEOUT_SECONDS: u64 = 3;
    match ureq::get(url).config().timeout_global(Some(Duration::from_secs(PING_TIMEOUT_SECONDS))).build().call() {
        Ok(_) => true,
        Err(ureq::Error::StatusCode(_)) => true,
        Err(_) => false,
    }
}

/// Spawns a fresh instance with dev mode set to `dev_mode` and quits this
/// one - used by both the always-visible dev-mode toggle and the dev-only
/// restart button, and (via `AppMsg::Relaunch`) the tray menu's "relaunch"
/// entry. Explicit rather than relying on env inheritance from this
/// process to the spawned one - the single-instance GApplication D-Bus
/// registration means the new process can race this one's shutdown in
/// ways that make "obviously inherited" state look like it vanished, so
/// don't leave it to chance.
pub(crate) fn relaunch(dev_mode: bool) {
    if let Ok(exe) = std::env::current_exe() {
        let mut cmd = std::process::Command::new(exe);
        if dev_mode {
            cmd.env("XENEON_DEV_MODE", "1");
        } else {
            cmd.env_remove("XENEON_DEV_MODE");
        }
        let _ = cmd.spawn();
    }
    relm4::main_application().quit();
}

/// Used wherever a Home Assistant config change needs the same full
/// relaunch enabling/disabling the page always does (see ha_page.rs's own
/// doc comment on why that isn't live carousel surgery) - asks first
/// rather than just vanishing the window: `relaunch()` itself is
/// effectively instant (spawn the new process, quit this one, in the same
/// call), so without a confirmation step first there'd be nothing to see
/// or react to at all, not even a moment's warning. Only "Redémarrer"
/// actually relaunches; "Annuler" (also the dialog's close response, so
/// Escape/clicking outside behaves the same as clicking it) just closes
/// the dialog and leaves the setting saved but not yet applied - the same
/// prompt reappears the next time something tries to apply it.
/// `dev_mode_enabled()` carries the *current* dev-mode state through
/// unchanged - this relaunch is about the HA page, not about dev mode.
fn confirm_ha_restart(parent: &gtk::Box) {
    let dialog = adw::AlertDialog::new(
        Some(&i18n::t("settings.ha_group.restart_dialog.heading")),
        Some(&i18n::t("settings.ha_group.restart_dialog.body")),
    );
    dialog.add_response("cancel", &i18n::t("settings.ha_group.restart_dialog.cancel"));
    dialog.add_response("restart", &i18n::t("settings.ha_group.restart_dialog.restart"));
    dialog.set_response_appearance("restart", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("restart"));
    dialog.set_close_response("cancel");
    let dev_mode = crate::dev_mode_enabled();
    dialog.connect_response(None, move |_dialog, response| {
        if response == "restart" {
            relaunch(dev_mode);
        }
    });
    dialog.present(Some(parent));
}

/// Sentinel `Config.default_page` value meaning "the Home Assistant
/// page" - never a real page id (those come from `WidgetGrid::page_id`,
/// a UUID-shaped string this could never collide with). The one other
/// place this exact string is compared is main.rs's own default-page
/// resolution at startup - kept as a plain literal in both rather than a
/// shared constant, since these are the only two places it's ever used.
const HA_DEFAULT_PAGE_VALUE: &str = "ha";

/// Rebuilds the "Page par défaut" combo's options from the current pages
/// (+ "Home Assistant" first, if enabled) - same rebuild-on-change spirit
/// as `refresh_pages_group`, just for this row's model instead of the
/// page list itself. Selection is preserved by *value* (a page id, or
/// `HA_DEFAULT_PAGE_VALUE`), not by index, since the same value can land
/// on a different index across rebuilds (a page added/removed ahead of
/// it, Home Assistant's own slot appearing/disappearing) - falls back to
/// the first real page whenever the saved choice is unset or no longer
/// among the options, mirroring `Config.default_page`'s own doc comment
/// on why that's the right fallback (not an error) for both cases.
fn refresh_default_row(row: &adw::ComboRow, values: &Rc<RefCell<Vec<String>>>, pages: &[Rc<WidgetGrid>]) {
    let ha_enabled = config_store::get().ha_page_enabled;
    let mut new_values = Vec::with_capacity(pages.len() + 1);
    let mut labels = Vec::with_capacity(pages.len() + 1);
    if ha_enabled {
        new_values.push(HA_DEFAULT_PAGE_VALUE.to_string());
        labels.push(i18n::t("settings.ha_group.title"));
    }
    // Page 1 is always right here, immediately after Home Assistant's own
    // slot if present - the fallback index below relies on that.
    for grid in pages {
        new_values.push(grid.page_id().to_string());
        labels.push(grid.display_name());
    }

    let model = gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    row.set_model(Some(&model));

    let page_1_index = if ha_enabled { 1 } else { 0 };
    let saved = config_store::get().default_page;
    let index = saved.and_then(|value| new_values.iter().position(|v| *v == value)).unwrap_or(page_1_index);
    row.set_selected(index as u32);

    *values.borrow_mut() = new_values;
}

/// Tears down and rebuilds one `Adw.ExpanderRow` per page - mirrors
/// `SettingsPage.refresh_pages()`.
fn refresh_pages_group(
    pages_group: &adw::PreferencesGroup,
    page_rows: &Rc<RefCell<Vec<adw::ExpanderRow>>>,
    pages: &[Rc<WidgetGrid>],
    page_indicator: &PageIndicator,
) {
    for row in page_rows.borrow_mut().drain(..) {
        pages_group.remove(&row);
    }
    for grid in pages {
        let row = build_page_row(grid.clone(), page_indicator.clone());
        pages_group.add(&row);
        page_rows.borrow_mut().push(row);
    }
}

fn build_page_row(grid: Rc<WidgetGrid>, page_indicator: PageIndicator) -> adw::ExpanderRow {
    let expander = adw::ExpanderRow::new();
    expander.set_title(&grid.display_name());
    expander.set_subtitle(&i18n::t_args("settings.pages_group.subtitle", &[("n", &(grid.page_index() + 1).to_string())]));

    let name_row = adw::EntryRow::new();
    name_row.set_title(&i18n::t("settings.pages_group.name_row.title"));
    name_row.set_text(&grid.custom_name().unwrap_or_default());
    name_row.set_show_apply_button(true);

    let restore_button = gtk::Button::from_icon_name("edit-undo-symbolic");
    restore_button.add_css_class("flat");
    restore_button.set_valign(gtk::Align::Center);
    restore_button.set_tooltip_text(Some(&i18n::t("settings.pages_group.restore_name_tooltip")));
    name_row.add_suffix(&restore_button);

    name_row.connect_apply({
        let grid = grid.clone();
        let expander = expander.clone();
        let page_indicator = page_indicator.clone();
        move |row| {
            grid.set_custom_name(Some(row.text().to_string()));
            expander.set_title(&grid.display_name());
            page_indicator.refresh();
        }
    });
    restore_button.connect_clicked({
        let grid = grid.clone();
        let expander = expander.clone();
        let name_row = name_row.clone();
        move |_| {
            grid.set_custom_name(None);
            name_row.set_text("");
            expander.set_title(&grid.display_name());
            page_indicator.refresh();
        }
    });

    expander.add_row(&name_row);
    // Background (color/image) isn't ported yet - PageBackground doesn't
    // exist on the Rust side yet, see project memory.
    expander
}
