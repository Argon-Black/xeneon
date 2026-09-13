//! Bare-bones content for previewing a grid size preset: just the size
//! code, large and centered. Ported from `DummyContent` in
//! widgets/dummy.py. No settings, no persisted content of its own -
//! relies entirely on the generic appearance popover every widget gets
//! for free (in the Python original; not ported yet on the Rust side).

use gtk::prelude::*;
use std::sync::Once;

use crate::i18n_runtime as i18n;
use crate::widgets::registry::{self, WidgetInstance};

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

fn label_key(size_code: &str) -> String {
    format!("widgets.dummy.label_{}", size_code.to_lowercase())
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

    // Fills the whole card (no halign/valign here - Center would size the
    // box down to the label's own natural size and center *that*, leaving
    // the background color only behind the text instead of across the
    // full widget). The label below is what gets centered inside it.
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.add_css_class(&format!("dummy-{}", size_code.to_lowercase()));

    let label = gtk::Label::new(None);
    label.set_halign(gtk::Align::Center);
    label.set_valign(gtk::Align::Center);
    label.set_hexpand(true);
    label.set_vexpand(true);
    let key = label_key(size_code);
    let set_text = {
        let label = label.clone();
        let key = key.clone();
        move || {
            label.set_markup(&format!(
                "<span size=\"300%\" foreground=\"#ffffff\">{}</span>",
                gtk::glib::markup_escape_text(&i18n::t(&key))
            ));
        }
    };
    set_text();
    content.append(&label);
    i18n::on_change(set_text);

    content
}

fn instance_for(size_code: &'static str) -> WidgetInstance {
    registry::instance_without_settings(build(size_code))
}

// One pair of tiny wrappers per size code so the registry's static
// CATALOG table (which needs plain `fn` pointers, not closures) can name
// each size - see registry.rs's doc comment for why this table is worth
// keeping to one entry per kind rather than a second hand-written dispatch
// like the Python original's `build_from_state` if/elif chain.
pub fn spawn_s() -> WidgetInstance {
    instance_for("S")
}
pub fn restore_s(_data: &serde_json::Value) -> WidgetInstance {
    instance_for("S")
}
pub fn spawn_m() -> WidgetInstance {
    instance_for("M")
}
pub fn restore_m(_data: &serde_json::Value) -> WidgetInstance {
    instance_for("M")
}
pub fn spawn_l() -> WidgetInstance {
    instance_for("L")
}
pub fn restore_l(_data: &serde_json::Value) -> WidgetInstance {
    instance_for("L")
}
pub fn spawn_sq() -> WidgetInstance {
    instance_for("SQ")
}
pub fn restore_sq(_data: &serde_json::Value) -> WidgetInstance {
    instance_for("SQ")
}
pub fn spawn_sx() -> WidgetInstance {
    instance_for("SX")
}
pub fn restore_sx(_data: &serde_json::Value) -> WidgetInstance {
    instance_for("SX")
}
pub fn spawn_ssx() -> WidgetInstance {
    instance_for("SSX")
}
pub fn restore_ssx(_data: &serde_json::Value) -> WidgetInstance {
    instance_for("SSX")
}
