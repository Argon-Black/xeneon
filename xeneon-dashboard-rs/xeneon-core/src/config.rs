// SPDX-License-Identifier: GPL-3.0-or-later
//! App-level settings (`config.json`): language, fullscreen shortcut,
//! indicator look, accent color, the global default widget appearance.
//! Ported from `config.py`. Deliberately has no GTK/gi dependency of its
//! own in the Python original, for the same reason preserved here: a
//! layering choice that keeps settings data testable and reusable on its
//! own.

use crate::persistence;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Base directory for all of this app's persisted state - `config.json`
/// directly inside it, `widgets/` and `pages/` subdirectories alongside.
///
/// Deliberately named `xeneon-dashboard-rs`, distinct from the Python app's
/// `xeneon-dashboard` config directory, so the two can run side by side on
/// the same machine during development without one clobbering the other's
/// saved layout. Rename this to match the Python app's directory only at
/// the actual cutover, once the Rust version is what actually runs day to
/// day - not before.
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("xeneon-dashboard-rs")
}

pub fn widgets_dir() -> PathBuf {
    config_dir().join("widgets")
}

pub fn pages_dir() -> PathBuf {
    config_dir().join("pages")
}

fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

/// The 5-field appearance preset stored in `Config`, applied to every newly
/// spawned widget that hasn't already customized its own look. Kept
/// separate from the full `WidgetAppearance` (no `rounded`/`bg_image_path`)
/// exactly as in the Python original's `default_widget_appearance` dict.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DefaultWidgetAppearance {
    pub opacity: f64,
    pub bg_color: String,
    pub border_enabled: bool,
    pub border_width: u32,
    pub border_color: String,
}

impl Default for DefaultWidgetAppearance {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            bg_color: "#242424".to_string(),
            border_enabled: false,
            border_width: 2,
            border_color: "#ffffff".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub fullscreen_shortcut_hint: String,
    pub fullscreen_shortcut_restore_token: Option<String>,
    pub fullscreen_shortcut_bound_trigger: Option<String>,
    pub indicator_hide_delay_seconds: u32,
    pub indicator_opacity: u32,
    pub indicator_button_color: Option<String>,
    pub language: String,
    pub shortcuts_host_access: bool,
    pub accent_color: String,
    pub accent_follow_system: bool,
    pub default_widget_appearance: DefaultWidgetAppearance,
    // Full-bleed background image shown behind every *real* widget page
    // (not the dev-mode test page, not the settings page) - None means no
    // image, just the plain theme background. A later phase adds a
    // per-page override on top of this app-wide default (see grid_widget.rs
    // in xeneon-app).
    pub app_background_image_path: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fullscreen_shortcut_hint: "<Super>f".to_string(),
            fullscreen_shortcut_restore_token: None,
            fullscreen_shortcut_bound_trigger: None,
            indicator_hide_delay_seconds: 2,
            indicator_opacity: 55,
            indicator_button_color: None,
            language: "fr".to_string(),
            shortcuts_host_access: false,
            accent_color: "#7e57c2".to_string(),
            accent_follow_system: false,
            default_widget_appearance: DefaultWidgetAppearance::default(),
            app_background_image_path: None,
        }
    }
}

impl Config {
    /// Loads `config.json`, filling in any field missing from the file (an
    /// older save, or a hand-edited partial file) with `Config::default()`
    /// - the `#[serde(default)]` on the struct gives this additive,
    /// forward-compatible merge for free, field by field. This also covers
    /// nested structs like `default_widget_appearance` field-by-field,
    /// unlike the Python original's shallow dict merge which would replace
    /// a nested object wholesale if present at all.
    ///
    /// A missing file (first launch) is silent; a present-but-corrupt file
    /// logs a warning and falls back to defaults entirely, matching
    /// `config.py::load()`.
    pub fn load() -> Self {
        let path = config_file();
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
        {
            Some(config) => config,
            None => {
                eprintln!(
                    "xeneon-dashboard: {} is corrupt or unreadable, falling back to defaults",
                    path.display()
                );
                Self::default()
            }
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        persistence::write_json_atomic(&config_file(), self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // An old config saved before `accent_follow_system` existed.
        let partial = r#"{"language": "en"}"#;
        let config: Config = serde_json::from_str(partial).unwrap();
        assert_eq!(config.language, "en");
        assert_eq!(config.accent_color, "#7e57c2"); // untouched field, defaulted
        assert!(!config.accent_follow_system);
    }

    #[test]
    fn nested_default_widget_appearance_merges_field_by_field() {
        let partial = r##"{"default_widget_appearance": {"bg_color": "#000000"}}"##;
        let config: Config = serde_json::from_str(partial).unwrap();
        assert_eq!(config.default_widget_appearance.bg_color, "#000000");
        assert_eq!(config.default_widget_appearance.border_width, 2); // defaulted
    }
}
