//! Full-screen widget picker: a `gtk::Revealer` overlay child of the app's
//! root `gtk::Overlay` (see main.rs) that slides down from the top to cover
//! the whole window, ported from `WidgetPicker` in widget_picker.py.
//! Replaces the earlier `adw::Dialog` stand-in (see git history) that only
//! proved the add-a-widget flow end to end.
//!
//! Ported in two steps, tracked in the memory system so a future session
//! can resume cleanly:
//! - **Step 1 (this one)**: the overlay mechanics only - reveal/hide
//!   animation, input handling, Escape-to-close - with the same plain list
//!   of catalog entries the old dialog showed. Landed first, deliberately,
//!   to validate the trickiest part (a full-screen Gtk.Revealer/gtk::Overlay
//!   combo) on the real Xeneon hardware before adding the heavier size-
//!   grouped live-preview content from the Python original.
//! - **Step 2 (not yet done)**: replace the plain list with
//!   `Gtk::FlowBox` sections grouped by size family (compact/medium/large,
//!   see `_size_family`/`_grouped_catalog` in widget_picker.py), each tile
//!   showing the real, live widget content via `(descriptor.spawn)().content`
//!   instead of just a title row.
//!
//! **The `can_target` gotcha** (the actual bug that cost the most time
//! porting this from Python tonight, see feedback_rust_gtk_dev_loop_gotchas
//! in the memory system): a `Gtk.Revealer` overlay child with
//! `halign`/`valign` set to `Fill` is allocated the *whole* `gtk::Overlay`
//! area by GTK regardless of `reveal_child` - hidden or not. Left
//! targetable, an invisible-but-full-size Revealer like this one sits over
//! the entire window and silently swallows every click and swipe. Fixed the
//! same way `PageIndicator` (page_indicator.rs) already handles it: toggle
//! `can_target` together with `reveal_child`, never leave it targetable
//! while closed.

use adw::prelude::*;
use gtk::glib;

use crate::i18n_runtime as i18n;
use crate::widgets::registry::CATALOG;

pub struct WidgetPicker {
    revealer: gtk::Revealer,
    title_label: gtk::Label,
    /// One (row, title_key) pair per CATALOG entry, same order - kept
    /// around so `retranslate` can re-apply `i18n::t` on a language change
    /// without rebuilding the whole list.
    rows: Vec<(adw::ActionRow, &'static str)>,
}

impl WidgetPicker {
    pub fn widget(&self) -> &gtk::Revealer {
        &self.revealer
    }

    pub fn new(on_pick: impl Fn(&'static str) + 'static) -> std::rc::Rc<Self> {
        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        revealer.set_transition_duration(550);
        revealer.set_halign(gtk::Align::Fill);
        revealer.set_valign(gtk::Align::Fill);
        revealer.set_hexpand(true);
        revealer.set_vexpand(true);
        revealer.set_reveal_child(false);
        // See the can_target gotcha in the module doc comment above.
        revealer.set_can_target(false);
        revealer.add_css_class("xeneon-widget-picker");

        let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        surface.add_css_class("xeneon-widget-picker-surface");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.add_css_class("xeneon-widget-picker-header");
        let title_label = gtk::Label::new(Some(&i18n::t("widgets.add_menu.title")));
        title_label.add_css_class("title-2");
        title_label.set_halign(gtk::Align::Start);
        title_label.set_hexpand(true);
        header.append(&title_label);
        let close_button = gtk::Button::new();
        close_button.add_css_class("flat");
        close_button.add_css_class("circular");
        close_button.set_icon_name("window-close-symbolic");
        close_button.set_tooltip_text(Some(&i18n::t("widgets.appearance.close_tooltip")));
        header.append(&close_button);
        surface.append(&header);

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.add_css_class("xeneon-widget-picker-body");
        scroller.set_child(Some(&body));
        surface.append(&scroller);

        revealer.set_child(Some(&surface));

        // Same list widget/styling as the old adw::Dialog version - only
        // the container around it changed in this step, see the module
        // doc comment. Step 2 replaces this ListBox with grouped FlowBox
        // tiles.
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        let mut rows = Vec::with_capacity(CATALOG.len());
        for descriptor in CATALOG {
            let row = adw::ActionRow::new();
            row.set_title(&i18n::t(descriptor.title_key));
            row.set_activatable(true);
            list.append(&row);
            rows.push((row, descriptor.title_key));
        }
        body.append(&list);

        let close = {
            let revealer = revealer.clone();
            move || {
                revealer.set_reveal_child(false);
                revealer.set_can_target(false);
            }
        };

        close_button.connect_clicked({
            let close = close.clone();
            move |_| close()
        });

        list.connect_row_activated({
            let close = close.clone();
            move |_, row| {
                let index = row.index();
                if index >= 0 {
                    if let Some(descriptor) = CATALOG.get(index as usize) {
                        on_pick(descriptor.kind);
                    }
                }
                close();
            }
        });

        let key_controller = gtk::EventControllerKey::new();
        key_controller.connect_key_pressed({
            let revealer = revealer.clone();
            let close = close.clone();
            move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape && revealer.reveals_child() {
                    close();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        revealer.add_controller(key_controller);

        let picker = std::rc::Rc::new(Self { revealer, title_label, rows });

        i18n::on_change({
            let picker = picker.clone();
            move || picker.retranslate()
        });

        picker
    }

    pub fn open(&self) {
        self.revealer.set_can_target(true);
        self.revealer.set_reveal_child(true);
        self.revealer.grab_focus();
    }

    fn retranslate(&self) {
        self.title_label.set_label(&i18n::t("widgets.add_menu.title"));
        for (row, title_key) in &self.rows {
            row.set_title(&i18n::t(title_key));
        }
    }
}
