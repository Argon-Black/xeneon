//! Per-widget visual customization: opacity, background color/image,
//! border, rounded corners. Ported from `WidgetAppearance` in
//! `widget_appearance.py`. Only the data model and the touched-state
//! bookkeeping live here (pure logic); actually turning this into CSS
//! belongs to `xeneon-app` since it needs a `gtk4::CssProvider`.

use crate::config::DefaultWidgetAppearance;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Which category of visual customization has been explicitly touched.
/// Until a category is touched, no CSS for it is generated at all - a
/// freshly spawned widget keeps the theme's plain, unstyled card look.
/// This is the mechanism that stops the global default-appearance setting
/// from clobbering a widget that already customized itself (see
/// [`WidgetAppearance::has_customizations`]). A real enum here instead of
/// the loose strings the Python `_touched: set[str]` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TouchedField {
    Bg,
    Border,
    Corner,
}

/// Mirrors `WidgetAppearance` in widget_appearance.py field for field, so
/// the same JSON (`to_dict()`/`apply_dict()` shape) can be read and
/// written. `#[serde(default)]` means any field missing from an
/// older/partial saved file falls back to this struct's `Default`, applied
/// per field rather than replacing the whole object - the same additive,
/// forward-compatible behaviour used throughout this crate's persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WidgetAppearance {
    pub touched: HashSet<TouchedField>,
    pub opacity: f64,
    pub bg_color: String,
    pub bg_image_path: Option<String>,
    pub border_enabled: bool,
    pub border_width: u32,
    pub border_color: String,
    pub rounded: bool,
}

impl Default for WidgetAppearance {
    fn default() -> Self {
        Self {
            touched: HashSet::new(),
            opacity: 1.0,
            bg_color: "#242424".to_string(),
            bg_image_path: None,
            border_enabled: false,
            border_width: 2,
            border_color: "#ffffff".to_string(),
            rounded: true,
        }
    }
}

impl WidgetAppearance {
    /// Whether any category has been explicitly touched. If false, this
    /// widget is still a plain, unstyled card and the global
    /// default-appearance setting (see [`Self::from_config_default`]) is
    /// free to apply to it. If true - the user changed something, or the
    /// widget's own spawner pre-set a look (e.g. the dummy widgets' per-size
    /// background color) - that must never be silently overwritten.
    pub fn has_customizations(&self) -> bool {
        !self.touched.is_empty()
    }

    /// Back to the untouched, plain-card state: not just the field values,
    /// `touched` is cleared entirely too, exactly like `reset()` in
    /// widget_appearance.py.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Builds the appearance state that applying the config's global
    /// default look actually renders, from the 5-field
    /// `DefaultWidgetAppearance` preset. Mirrors `defaults_touched_dict()`:
    /// `touched` is hardcoded to `{Bg, Border}` since, per
    /// `has_customizations` above, storing these values without marking
    /// them touched would mean they're saved but never actually shown.
    pub fn from_config_default(defaults: &DefaultWidgetAppearance) -> Self {
        Self {
            touched: HashSet::from([TouchedField::Bg, TouchedField::Border]),
            opacity: defaults.opacity,
            bg_color: defaults.bg_color.clone(),
            bg_image_path: None,
            border_enabled: defaults.border_enabled,
            border_width: defaults.border_width,
            border_color: defaults.border_color.clone(),
            rounded: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_by_default_and_after_reset() {
        let mut appearance = WidgetAppearance::default();
        assert!(!appearance.has_customizations());
        appearance.touched.insert(TouchedField::Bg);
        appearance.bg_color = "#123456".to_string();
        assert!(appearance.has_customizations());
        appearance.reset();
        assert!(!appearance.has_customizations());
        assert_eq!(appearance.bg_color, "#242424");
    }

    #[test]
    fn config_default_is_touched_so_it_actually_renders() {
        let defaults = DefaultWidgetAppearance::default();
        let appearance = WidgetAppearance::from_config_default(&defaults);
        assert!(appearance.has_customizations());
        assert!(appearance.touched.contains(&TouchedField::Bg));
        assert!(appearance.touched.contains(&TouchedField::Border));
    }

    #[test]
    fn touched_set_round_trips_through_json_as_lowercase_strings() {
        let mut appearance = WidgetAppearance::default();
        appearance.touched.insert(TouchedField::Corner);
        let json = serde_json::to_string(&appearance).unwrap();
        assert!(json.contains("\"corner\""));
        let back: WidgetAppearance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.touched, appearance.touched);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults_not_a_hard_error() {
        // Simulates an older/partial saved file - only `touched` present.
        let partial = r#"{"touched": ["bg"]}"#;
        let appearance: WidgetAppearance = serde_json::from_str(partial).unwrap();
        assert!(appearance.touched.contains(&TouchedField::Bg));
        assert_eq!(appearance.bg_color, "#242424"); // fell back to default
    }
}
