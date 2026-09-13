//! A simple list of every registered widget kind, opened with Ctrl+Plus
//! (matching the Python app's `app.add-widget` accelerators - Ctrl+=, the
//! numpad +, and bare Ctrl+= since shift is annoying to reach). Picking an
//! entry closes the dialog and reports the chosen `kind` back to the
//! caller. Much simpler than `WidgetPicker` in widget_picker.py (no
//! per-size grouping, no live preview tiles) - good enough to prove the
//! add-a-widget flow end to end; the fancier picker UI is a later polish
//! pass.

use adw::prelude::*;

use crate::i18n_runtime as i18n;
use crate::widgets::registry::CATALOG;

pub fn build(on_pick: impl Fn(&'static str) + 'static) -> adw::Dialog {
    let dialog = adw::Dialog::new();
    dialog.set_title(&i18n::t("widgets.add_menu.title"));
    dialog.set_content_width(320);
    dialog.set_content_height(420);

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_margin_top(12);
    list.set_margin_bottom(12);
    list.set_margin_start(12);
    list.set_margin_end(12);

    for descriptor in CATALOG {
        let row = adw::ActionRow::new();
        row.set_title(&i18n::t(descriptor.title_key));
        row.set_activatable(true);
        list.append(&row);
    }

    list.connect_row_activated({
        let dialog = dialog.clone();
        move |_, row| {
            let index = row.index();
            if index >= 0 {
                if let Some(descriptor) = CATALOG.get(index as usize) {
                    on_pick(descriptor.kind);
                }
            }
            dialog.close();
        }
    });

    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_child(Some(&list));
    dialog.set_child(Some(&scrolled));

    dialog
}
