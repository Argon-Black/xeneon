// SPDX-License-Identifier: GPL-3.0-or-later
//! Renders a `WidgetAppearance` as CSS scoped to a widget's own unique
//! class, exactly like `widget_appearance.py`'s `_apply()`: a shared,
//! module-level `{css_class: rule}` map, the whole thing reloaded into one
//! shared `CssProvider` on every single change. That doesn't parallelize
//! well in the abstract, but it's what the Python original does and this
//! app has a handful of widgets on screen at once, not hundreds - not
//! worth a more elaborate per-widget-provider scheme for that scale.

use gtk::gio::prelude::FileExt;
use std::cell::RefCell;
use std::collections::HashMap;
use xeneon_core::appearance::{TouchedField, WidgetAppearance};

const ROUNDED_RADIUS_PX: i32 = 12;

thread_local! {
    static RULES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

fn ensure_provider() -> gtk::CssProvider {
    PROVIDER.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            let provider = gtk::CssProvider::new();
            if let Some(display) = gtk::gdk::Display::default() {
                // One priority above the app's baseline (whatever
                // installs at APPLICATION elsewhere) so a widget's own
                // customization always wins - matches
                // widget_appearance.py's `_ensure_installed()`.
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
                );
            }
            *cell = Some(provider);
        }
        cell.as_ref().unwrap().clone()
    })
}

fn rgba_css(rgba: &gtk::gdk::RGBA, alpha: f64) -> String {
    format!(
        "rgba({}, {}, {}, {:.2})",
        (rgba.red() * 255.0).round() as u8,
        (rgba.green() * 255.0).round() as u8,
        (rgba.blue() * 255.0).round() as u8,
        alpha
    )
}

/// (Re)generates `css_class`'s rule from `appearance` and reloads the
/// shared provider - call after every change, mirroring `_apply()`.
pub fn apply(css_class: &str, appearance: &WidgetAppearance) {
    ensure_provider();
    let mut rules_text = Vec::new();

    if appearance.touched.contains(&TouchedField::Bg) {
        let bg = gtk::gdk::RGBA::parse(&appearance.bg_color).unwrap_or(gtk::gdk::RGBA::BLACK);
        rules_text.push(format!("background-color: {};", rgba_css(&bg, appearance.opacity)));
        if let Some(path) = &appearance.bg_image_path {
            let uri = gtk::gio::File::for_path(path).uri();
            rules_text.push(format!("background-image: url('{uri}');"));
            rules_text.push("background-size: cover;".to_string());
            rules_text.push("background-position: center;".to_string());
        }
    }
    if appearance.touched.contains(&TouchedField::Border) {
        if appearance.border_enabled {
            let border = gtk::gdk::RGBA::parse(&appearance.border_color).unwrap_or(gtk::gdk::RGBA::WHITE);
            rules_text.push(format!("border: {}px solid {};", appearance.border_width, rgba_css(&border, 1.0)));
        } else {
            rules_text.push("border: none;".to_string());
        }
    }
    if appearance.touched.contains(&TouchedField::Corner) {
        let radius = if appearance.rounded { ROUNDED_RADIUS_PX } else { 0 };
        rules_text.push(format!("border-radius: {radius}px;"));
    }

    let body = rules_text.join(" ");
    RULES.with(|rules| {
        let mut rules = rules.borrow_mut();
        if body.is_empty() {
            rules.remove(css_class);
        } else {
            rules.insert(css_class.to_string(), format!(".{css_class} {{ {body} }}"));
        }
        let css: String = rules.values().cloned().collect::<Vec<_>>().join("\n");
        ensure_provider().load_from_string(&css);
    });
}

/// Installs (or, with `rule: None`, removes) an already-formatted CSS rule
/// under `css_class` in the same shared provider `apply()` above uses -
/// lets an unrelated feature (the app-wide/per-page background image, see
/// grid_widget.rs's `set_background_image`) piggy-back on one
/// CssProvider/reload cycle instead of installing a second provider.
pub fn set_raw_rule(css_class: &str, rule: Option<String>) {
    ensure_provider();
    RULES.with(|rules| {
        let mut rules = rules.borrow_mut();
        match rule {
            Some(rule) => {
                rules.insert(css_class.to_string(), rule);
            }
            None => {
                rules.remove(css_class);
            }
        }
        let css: String = rules.values().cloned().collect::<Vec<_>>().join("\n");
        ensure_provider().load_from_string(&css);
    });
}

/// The reset button's own hardcoded punchy red/white - the theme's
/// "destructive" style renders too muted (dark red on dark red) to read
/// clearly, same rationale as `_rules["_reset_button"]` in
/// widget_appearance.py. Install once, alongside the first widget's own
/// rule (piggy-backing on the same shared provider/reload).
pub fn ensure_reset_button_css_installed() {
    RULES.with(|rules| {
        let mut rules = rules.borrow_mut();
        if rules.contains_key("_reset_button") {
            return;
        }
        rules.insert(
            "_reset_button".to_string(),
            ".xeneon-reset-button { background-color: #d5303f; color: #ffffff; font-weight: bold; }\n\
             .xeneon-reset-button:hover { background-color: #c02836; }"
                .to_string(),
        );
        let css: String = rules.values().cloned().collect::<Vec<_>>().join("\n");
        ensure_provider().load_from_string(&css);
    });
}
