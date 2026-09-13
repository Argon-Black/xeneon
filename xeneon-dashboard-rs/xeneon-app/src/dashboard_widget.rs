//! Generic chrome wrapped around every widget's own content: a title
//! label, a delete button, and a move handle that drags the widget around
//! its `WidgetGrid`. Ported (simplified for this phase) from
//! `DashboardWidget` in grid.py - the appearance popover, hover-only
//! reveal, and touch-hold reveal aren't built yet, so the buttons are
//! always visible for now rather than only on hover.

use gtk::prelude::*;

/// The constructed chrome plus the interactive bits `WidgetGrid` needs to
/// wire up drag/delete behaviour - it owns the gestures/signals, this
/// module only builds the widgets.
pub struct DashboardWidgetHandles {
    pub root: gtk::Overlay,
    pub delete_button: gtk::Button,
    pub move_button: gtk::Button,
}

pub fn build(title: &str, content: &impl IsA<gtk::Widget>, w: i32, h: i32) -> DashboardWidgetHandles {
    let root = gtk::Overlay::new();
    root.set_size_request(w, h);
    root.add_css_class("card");
    root.set_child(Some(content));

    // Never steals clicks meant for the content underneath it - same fix
    // as the Python original's `_header.set_can_target(False)`.
    let header = gtk::Label::new(Some(title));
    header.set_halign(gtk::Align::Start);
    header.set_valign(gtk::Align::Start);
    header.set_margin_start(8);
    header.set_margin_top(6);
    header.add_css_class("caption-heading");
    header.set_can_target(false);
    root.add_overlay(&header);

    let delete_button = gtk::Button::from_icon_name("window-close-symbolic");
    delete_button.set_halign(gtk::Align::End);
    delete_button.set_valign(gtk::Align::Start);
    delete_button.set_margin_end(4);
    delete_button.set_margin_top(4);
    delete_button.add_css_class("circular");
    delete_button.add_css_class("flat");
    root.add_overlay(&delete_button);

    // A plain glyph rather than an icon name - avoids depending on a
    // "drag handle" symbolic icon actually existing in whatever icon theme
    // is installed (window-close-symbolic above is safe, a drag-handle
    // icon is far less universally present).
    let move_button = gtk::Button::with_label("⠿");
    move_button.set_halign(gtk::Align::Start);
    move_button.set_valign(gtk::Align::End);
    move_button.set_margin_start(4);
    move_button.set_margin_bottom(4);
    move_button.add_css_class("circular");
    move_button.add_css_class("flat");
    move_button.set_cursor_from_name(Some("move"));
    root.add_overlay(&move_button);

    DashboardWidgetHandles { root, delete_button, move_button }
}
