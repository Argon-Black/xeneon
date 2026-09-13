//! Settings page: an Interface group (page-indicator hide delay/opacity/
//! color), a language switcher and Theme group (accent color) in column 1;
//! a "Pages" group to rename each widget page in column 2; keyboard
//! shortcuts plus (dev mode only) a restart button in column 3 - laid out
//! in 3 columns side by side, same structure as `SettingsPage` in
//! settings_page.py (a plain `Gtk.Box`, not `Adw.PreferencesPage` - the
//! page indicator handles this page's own scroll/carousel framing, so
//! this is just a title label plus a horizontally-scrolled row of column
//! boxes, each holding a few `Adw.PreferencesGroup`s).
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
use std::cell::RefCell;
use std::rc::Rc;

use crate::appearance_css;
use crate::appearance_popover::{hex_to_rgba, rgba_to_hex};
use crate::config_store;
use crate::grid_widget::WidgetGrid;
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
}

impl PagesHandle {
    pub fn add_page(&self, grid: Rc<WidgetGrid>) {
        let row = build_page_row(grid.clone(), self.page_indicator.clone());
        self.group.add(&row);
        self.rows.borrow_mut().push(row);
        self.pages.borrow_mut().push(grid);
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
    root.set_margin_bottom(24);
    root.set_margin_start(24);
    root.set_margin_end(24);

    let title = gtk::Label::new(Some(&i18n::t("settings.title")));
    title.add_css_class("title-1");
    title.set_halign(gtk::Align::Start);
    title.set_margin_bottom(12);
    root.append(&title);

    let columns_box = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    columns_box.set_homogeneous(true);
    columns_box.set_hexpand(true);

    let column1 = gtk::Box::new(gtk::Orientation::Vertical, 24);
    column1.set_valign(gtk::Align::Start);
    let column2 = gtk::Box::new(gtk::Orientation::Vertical, 24);
    column2.set_valign(gtk::Align::Start);
    let column3 = gtk::Box::new(gtk::Orientation::Vertical, 24);
    column3.set_valign(gtk::Align::Start);

    // --- Column 1: Interface (page indicator look) ---
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

    column1.append(&interface_group);

    // --- Column 1 (cont'd): Language + Theme (accent) ---
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
    column1.append(&language_group);

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

    column1.append(&theme_group);

    // --- Column 2: Pages ---
    let pages_group = adw::PreferencesGroup::new();
    pages_group.set_title(&i18n::t("settings.pages_group.title"));
    column2.append(&pages_group);
    let page_rows: Rc<RefCell<Vec<adw::ExpanderRow>>> = Rc::new(RefCell::new(Vec::new()));
    // Shared (not just a snapshot) so a page added at runtime via
    // `PagesHandle::add_page` is still there when the retranslate closure
    // below rebuilds the group - see `PagesHandle`'s own doc comment.
    let pages_shared: Rc<RefCell<Vec<Rc<WidgetGrid>>>> = Rc::new(RefCell::new(pages.to_vec()));
    refresh_pages_group(&pages_group, &page_rows, &pages_shared.borrow(), &page_indicator);

    // --- Column 2 (cont'd): Default widget appearance ---
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

    column2.append(&appearance_group);

    // --- Column 3: Keyboard shortcuts + dev tools ---
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

    column3.append(&shortcuts_group);

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
    column3.append(&dev_toggle_group);

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
        column3.append(group);
    }

    columns_box.append(&column1);
    columns_box.append(&column2);
    columns_box.append(&column3);

    // Scrolls instead of just growing: this page's content only gets
    // taller as settings groups are added, and the Xeneon Edge panel is a
    // fixed 720px tall with no room to spare - if this page's natural
    // height ever exceeded what's actually available, an un-scrolled
    // Gtk.Box would instead push the whole carousel (and the fullscreened
    // window itself) taller than the physical monitor.
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_vexpand(true);
    scroller.set_child(Some(&columns_box));
    root.append(&scroller);

    i18n::on_change({
        let title = title.clone();
        let interface_group = interface_group.clone();
        let hide_delay_row = hide_delay_row.clone();
        let indicator_opacity_row = indicator_opacity_row.clone();
        let indicator_color_row = indicator_color_row.clone();
        let indicator_color_reset_button = indicator_color_reset_button.clone();
        let language_group = language_group.clone();
        let language_row = language_row.clone();
        let theme_group = theme_group.clone();
        let accent_row = accent_row.clone();
        let accent_buttons = accent_buttons.clone();
        let follow_system_row = follow_system_row.clone();
        let dev_group = dev_group.clone();
        let pages_group = pages_group.clone();
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
        move || {
            title.set_label(&i18n::t("settings.title"));
            interface_group.set_title(&i18n::t("settings.interface_group"));
            hide_delay_row.set_title(&i18n::t("settings.hide_delay_row.title"));
            hide_delay_row.set_subtitle(&i18n::t("settings.hide_delay_row.subtitle"));
            indicator_opacity_row.set_title(&i18n::t("settings.indicator_opacity_row.title"));
            indicator_opacity_row.set_subtitle(&i18n::t("settings.indicator_opacity_row.subtitle"));
            indicator_color_row.set_title(&i18n::t("settings.indicator_color_row.title"));
            indicator_color_reset_button.set_tooltip_text(Some(&i18n::t("settings.indicator_color_row.reset_tooltip")));
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

    PagesHandle { group: pages_group, rows: page_rows, pages: pages_shared, page_indicator }
}

/// Spawns a fresh instance with dev mode set to `dev_mode` and quits this
/// one - used by both the always-visible dev-mode toggle and the dev-only
/// restart button. Explicit rather than relying on env inheritance from
/// this process to the spawned one - the single-instance GApplication
/// D-Bus registration means the new process can race this one's shutdown
/// in ways that make "obviously inherited" state look like it vanished,
/// so don't leave it to chance.
fn relaunch(dev_mode: bool) {
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
