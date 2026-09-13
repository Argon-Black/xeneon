//! Bare-bones content for previewing a grid size preset: just the size
//! code, large and centered. Ported from `DummyContent` in
//! widgets/dummy.py. No i18n yet (that's a later step) - the label is the
//! raw size code for now.

use gtk::prelude::*;
use std::sync::Once;

/// One background color per preset so sizes are told apart at a glance,
/// matching `_COLOR_HEX` in dummy.py.
fn color_hex(size_code: &str) -> &'static str {
    match size_code {
        "S" => "#993C1D",
        "M" => "#0F6E56",
        "L" => "#3C3489",
        "SQ" => "#72243E",
        "SX" => "#854F0B",
        "SSX" => "#3B6D11",
        _ => "#333333",
    }
}

const SIZE_CODES: [&str; 6] = ["S", "M", "L", "SQ", "SX", "SSX"];
static INSTALL_CSS: Once = Once::new();

/// Installs the `.dummy-<code>` CSS classes once, display-wide - simpler
/// and less deprecated than giving every instance its own CssProvider, and
/// good enough for this throwaway preview content (the real per-widget
/// styling system is WidgetAppearance, ported in a later phase).
fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        let rules: String = SIZE_CODES
            .iter()
            .map(|code| format!(".dummy-{} {{ background-color: {}; }}\n", code.to_lowercase(), color_hex(code)))
            .collect();
        css.load_from_string(&rules);
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

pub fn build(size_code: &str) -> gtk::Box {
    ensure_css_installed();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_halign(gtk::Align::Center);
    content.set_valign(gtk::Align::Center);
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.add_css_class(&format!("dummy-{}", size_code.to_lowercase()));

    let label = gtk::Label::new(None);
    label.set_markup(&format!(
        "<span size=\"300%\" foreground=\"#ffffff\">{}</span>",
        gtk::glib::markup_escape_text(size_code)
    ));
    content.append(&label);

    content
}
