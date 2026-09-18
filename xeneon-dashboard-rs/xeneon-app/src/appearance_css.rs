// SPDX-License-Identifier: GPL-3.0-or-later
//! Renders a `WidgetAppearance` as CSS scoped to a widget's own unique
//! class, exactly like `widget_appearance.py`'s `_apply()`: a shared,
//! module-level `{css_class: rule}` map, the whole thing reloaded into one
//! shared `CssProvider` on every single change. That doesn't parallelize
//! well in the abstract, but it's what the Python original does and this
//! app has a handful of widgets on screen at once, not hundreds - not
//! worth a more elaborate per-widget-provider scheme for that scale.

use gtk::gio::prelude::FileExt;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use xeneon_core::appearance::{TouchedField, WidgetAppearance};

const ROUNDED_RADIUS_PX: i32 = 12;

/// A generic `{css_class: rule}` map reloaded into its own dedicated
/// `CssProvider` on every change - the "per-instance CSS rule registry"
/// pattern this codebase used to hand-roll separately at 5 call sites
/// (this module's own rules below, `audio.rs`'s scaled-badge rules,
/// `temp_gauge.rs`'s `GaugeCss`, and `agenda.rs`'s bar-color and
/// content-scale rules). Audit finding 2026-09-18. Each call site still
/// owns its own instance, wrapped in its own `thread_local!` (GTK's main
/// loop is single-threaded here, same reasoning as every other
/// `thread_local!` in this codebase) - this only factors out the shared
/// insert/remove/reload mechanics, not the rules themselves, since each
/// site's stylesheet is logically separate.
pub struct CssRuleRegistry {
    provider: gtk::CssProvider,
    installed: Cell<bool>,
    rules: RefCell<HashMap<String, String>>,
    priority: u32,
}

impl CssRuleRegistry {
    pub fn new(priority: u32) -> Self {
        Self { provider: gtk::CssProvider::new(), installed: Cell::new(false), rules: RefCell::new(HashMap::new()), priority }
    }

    fn ensure_installed(&self) {
        if self.installed.get() {
            return;
        }
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &self.provider, self.priority);
        }
        self.installed.set(true);
    }

    fn reload(&self) {
        let css: String = self.rules.borrow().values().cloned().collect::<Vec<_>>().join("\n");
        self.provider.load_from_string(&css);
    }

    pub fn contains(&self, css_class: &str) -> bool {
        self.rules.borrow().contains_key(css_class)
    }

    /// Installs or replaces `css_class`'s rule and reloads the whole
    /// stylesheet from every rule currently registered.
    pub fn set_rule(&self, css_class: &str, rule: String) {
        self.ensure_installed();
        self.rules.borrow_mut().insert(css_class.to_string(), rule);
        self.reload();
    }

    /// Removes `css_class`'s rule, if any, and reloads.
    pub fn remove_rule(&self, css_class: &str) {
        self.ensure_installed();
        self.rules.borrow_mut().remove(css_class);
        self.reload();
    }
}

thread_local! {
    // One priority above the app's baseline (whatever installs at
    // APPLICATION elsewhere) so a widget's own customization always wins
    // - matches widget_appearance.py's `_ensure_installed()`.
    static REGISTRY: CssRuleRegistry = CssRuleRegistry::new(gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
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
    REGISTRY.with(|registry| {
        if body.is_empty() {
            registry.remove_rule(css_class);
        } else {
            registry.set_rule(css_class, format!(".{css_class} {{ {body} }}"));
        }
    });
}

/// Installs (or, with `rule: None`, removes) an already-formatted CSS rule
/// under `css_class` in the same shared provider `apply()` above uses -
/// lets an unrelated feature (the app-wide/per-page background image, see
/// grid_widget.rs's `set_background_image`) piggy-back on one
/// CssProvider/reload cycle instead of installing a second provider.
pub fn set_raw_rule(css_class: &str, rule: Option<String>) {
    REGISTRY.with(|registry| match rule {
        Some(rule) => registry.set_rule(css_class, rule),
        None => registry.remove_rule(css_class),
    });
}

/// The reset button's own hardcoded punchy red/white - the theme's
/// "destructive" style renders too muted (dark red on dark red) to read
/// clearly, same rationale as `_rules["_reset_button"]` in
/// widget_appearance.py. Install once, alongside the first widget's own
/// rule (piggy-backing on the same shared provider/reload).
pub fn ensure_reset_button_css_installed() {
    REGISTRY.with(|registry| {
        if registry.contains("_reset_button") {
            return;
        }
        registry.set_rule(
            "_reset_button",
            ".xeneon-reset-button { background-color: #d5303f; color: #ffffff; font-weight: bold; }\n\
             .xeneon-reset-button:hover { background-color: #c02836; }"
                .to_string(),
        );
    });
}
