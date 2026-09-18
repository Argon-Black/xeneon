//! Full-screen "Aide" overlay - the tray menu's "Aide" entry (see
//! tray.rs), a static list of the app's keyboard shortcuts. Deliberately
//! mirrors `widget_picker.rs`'s overlay mechanics rather than inventing a
//! second way to do the same thing: same `gtk::Revealer` slide-down,
//! same header/close-button construction (down to reusing its exact CSS
//! classes via `widget_picker::ensure_css_installed`), same
//! Escape-to-close wiring at the *window* level in main.rs. See that
//! module's own doc comment for the two GTK gotchas this sidesteps by
//! copying its working pattern instead of a plausible-looking new one:
//! - a `Fill`/`Fill` revealer left targetable while hidden swallows every
//!   click/swipe underneath it regardless of `reveal_child` (`can_target`
//!   must toggle together with it).
//! - `Escape` wired on the revealer's own subtree never fires - nothing
//!   inside it is focusable, so `grab_focus()` has nowhere to land.
//!
//! Unlike the widget picker, this overlay's content is plain static text -
//! no live widget previews holding a GLib timer or D-Bus watch to avoid
//! keeping alive while hidden - so it's built once in `new()` and just has
//! its labels retranslated in place on a language change, no per-open
//! rebuild/teardown needed.

use adw::prelude::*;

use crate::i18n_runtime as i18n;
use crate::widget_picker;

/// `(key-combo i18n key, description i18n key)` - both localized (e.g.
/// French "Maj" vs English "Shift" for the same physical key), so neither
/// half of a row is hardcoded.
const SHORTCUTS: [(&str, &str); 4] = [
    ("help.shortcuts.fullscreen_key", "help.shortcuts.fullscreen_desc"),
    ("help.shortcuts.add_widget_key", "help.shortcuts.add_widget_desc"),
    ("help.shortcuts.settings_key", "help.shortcuts.settings_desc"),
    ("help.shortcuts.close_key", "help.shortcuts.close_desc"),
];

pub struct HelpOverlay {
    revealer: gtk::Revealer,
    title_label: gtk::Label,
    // One row per SHORTCUTS entry, in the same order - the row's own
    // title is the description, `key_label` is its trailing key-combo
    // chip. retranslate() walks both lists in lockstep.
    rows: Vec<(adw::ActionRow, gtk::Label)>,
}

impl HelpOverlay {
    pub fn widget(&self) -> &gtk::Revealer {
        &self.revealer
    }

    pub fn new() -> std::rc::Rc<Self> {
        // Same stylesheet the widget picker installs (Once-guarded, so
        // calling this again here is a no-op if that module got there
        // first) - reuses its header/close-button rules verbatim instead
        // of a near-duplicate block for a second overlay that looks the
        // same.
        widget_picker::ensure_css_installed();

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        revealer.set_transition_duration(550);
        revealer.set_halign(gtk::Align::Fill);
        revealer.set_valign(gtk::Align::Fill);
        revealer.set_hexpand(true);
        revealer.set_vexpand(true);
        revealer.set_reveal_child(false);
        revealer.set_can_target(false);
        revealer.add_css_class("xeneon-help-overlay");

        let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        surface.add_css_class("xeneon-help-overlay-surface");
        // Same opaque-background mechanism as every other full-screen
        // page (see widget_picker.rs's own comment on this exact class).
        surface.add_css_class("view");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.add_css_class("xeneon-widget-picker-header");
        let title_label = gtk::Label::new(Some(&i18n::t("tray.help")));
        title_label.add_css_class("title-2");
        title_label.set_halign(gtk::Align::Start);
        title_label.set_hexpand(true);
        header.append(&title_label);
        // Same constructor as every other close button in the app (see
        // widget_picker.rs's own comment on why not `Button::new()` +
        // `set_icon_name()`).
        let close_button = gtk::Button::from_icon_name("window-close-symbolic");
        close_button.add_css_class("flat");
        close_button.add_css_class("circular");
        close_button.add_css_class("xeneon-widget-picker-close");
        close_button.set_tooltip_text(Some(&i18n::t("widgets.appearance.close_tooltip")));
        header.append(&close_button);
        surface.append(&header);

        let scroller = gtk::ScrolledWindow::new();
        // Unlike widget_picker.rs's row of same-width tiles, nothing here
        // ever lays out horizontally - each row is a single full-width
        // Adw.ActionRow - so "Never" can't hit the same "forces the whole
        // window wider than the physical panel" bug documented there:
        // there's no unbounded-width child for it to fail to clip.
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 20);
        body.set_margin_top(12);
        body.set_margin_bottom(20);
        body.set_margin_start(24);
        body.set_margin_end(24);

        let group = adw::PreferencesGroup::new();
        let mut rows = Vec::with_capacity(SHORTCUTS.len());
        for (key_key, desc_key) in SHORTCUTS {
            let row = adw::ActionRow::new();
            row.set_title(&i18n::t(desc_key));
            // GTK's own "keycap" style class (used internally by
            // GtkShortcutsWindow) - a small bordered pill for a single
            // key/combo, with no custom CSS needed here.
            let key_label = gtk::Label::new(Some(&i18n::t(key_key)));
            key_label.add_css_class("keycap");
            key_label.set_valign(gtk::Align::Center);
            row.add_suffix(&key_label);
            group.add(&row);
            rows.push((row, key_label));
        }
        body.append(&group);
        scroller.set_child(Some(&body));
        surface.append(&scroller);

        revealer.set_child(Some(&surface));

        let close = {
            let revealer = revealer.clone();
            move || {
                revealer.set_reveal_child(false);
                revealer.set_can_target(false);
            }
        };
        close_button.connect_clicked(move |_| close());

        // Escape-to-close is wired at the *window* level in main.rs, same
        // as the widget picker and for the same reason - see this
        // module's own doc comment.

        let overlay = std::rc::Rc::new(Self { revealer, title_label, rows });

        i18n::on_change({
            let overlay = overlay.clone();
            move || overlay.retranslate()
        });

        overlay
    }

    pub fn open(&self) {
        self.revealer.set_can_target(true);
        self.revealer.set_reveal_child(true);
        self.revealer.grab_focus();
    }

    pub fn close(&self) {
        self.revealer.set_reveal_child(false);
        self.revealer.set_can_target(false);
    }

    fn retranslate(&self) {
        self.title_label.set_label(&i18n::t("tray.help"));
        for ((row, key_label), (key_key, desc_key)) in self.rows.iter().zip(SHORTCUTS) {
            row.set_title(&i18n::t(desc_key));
            key_label.set_label(&i18n::t(key_key));
        }
    }
}
