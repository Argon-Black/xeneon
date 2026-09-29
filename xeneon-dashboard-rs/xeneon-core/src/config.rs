// SPDX-License-Identifier: GPL-3.0-or-later
//! App-level settings (`config.json`): language, fullscreen shortcut,
//! indicator look, accent color, the global default widget appearance.
//! Ported from `config.py`. Deliberately has no GTK/gi dependency of its
//! own in the Python original, for the same reason preserved here: a
//! layering choice that keeps settings data testable and reusable on its
//! own.

use crate::persistence;
use log::warn;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::PathBuf;

thread_local! {
    // Set only by xeneon-app's dev-mode sandbox (see its `main()`) - `None`
    // the rest of the time, so `config_dir()` behaves exactly as before for
    // normal (non-dev) launches.
    static SANDBOX_DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Redirects `config_dir()` - and therefore `widgets_dir()`, `pages_dir()`,
/// `background_dir()`, `config_file()`, every path derived from it - to
/// `path` for the rest of this process. Used only by dev mode's sandbox:
/// dev mode is meant to always come up in the genuine "nothing saved yet"
/// state (see xeneon-app's `main()`, which wipes `path` before calling
/// this), so nothing it does ever touches the real saved layout.
/// `real_config_dir()` below is deliberately unaffected by this - the
/// dev-mode marker file has to survive the redirection it itself decides
/// whether to apply.
pub fn set_sandbox_dir(path: Option<PathBuf>) {
    SANDBOX_DIR.with(|s| *s.borrow_mut() = path);
}

/// Base directory for all of this app's persisted state - `config.json`
/// directly inside it, `widgets/` and `pages/` subdirectories alongside.
/// Resolves to the sandbox directory instead, for the lifetime of a
/// dev-mode process, once `set_sandbox_dir` has been called - see its own
/// doc comment.
///
/// Deliberately named `xeneon-dashboard-rs`, distinct from the Python app's
/// `xeneon-dashboard` config directory, so the two can run side by side on
/// the same machine during development without one clobbering the other's
/// saved layout. Rename this to match the Python app's directory only at
/// the actual cutover, once the Rust version is what actually runs day to
/// day - not before.
pub fn config_dir() -> PathBuf {
    if let Some(path) = SANDBOX_DIR.with(|s| s.borrow().clone()) {
        return path;
    }
    real_config_dir()
}

/// `config_dir()`'s own resolution logic, but never redirected by
/// `set_sandbox_dir` - only for state that has to survive dev mode's
/// sandbox regardless (currently just the dev-mode marker file, see
/// `dev_mode_flag_enabled`/`set_dev_mode_flag` below).
pub fn real_config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("xeneon-dashboard-rs")
}

/// Bare marker file (content unused, only its presence matters) recording
/// whether dev mode is on - deliberately not a field on `Config`, since
/// `Config` lives under `config_dir()` and gets swept into the sandbox
/// redirection while dev mode is active. This flag has to survive that
/// (it's what decides whether to apply the redirection at all), so it's
/// always read/written against `real_config_dir()` instead.
fn dev_mode_marker_file() -> PathBuf {
    real_config_dir().join("dev_mode.on")
}

pub fn dev_mode_flag_enabled() -> bool {
    dev_mode_marker_file().exists()
}

pub fn set_dev_mode_flag(enabled: bool) -> std::io::Result<()> {
    let path = dev_mode_marker_file();
    if enabled {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, b"")
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }
}

pub fn widgets_dir() -> PathBuf {
    config_dir().join("widgets")
}

pub fn pages_dir() -> PathBuf {
    config_dir().join("pages")
}

/// Where the app-wide background image (`Config::app_background_image_path`)
/// is copied to via `assets::store_asset` - separate from `pages_dir()`
/// because this image applies across every page rather than belonging to
/// one. A future per-page background override would instead store into
/// `pages_dir()` itself, alongside that page's own `<id>.json`, since it
/// belongs to that one page.
pub fn background_dir() -> PathBuf {
    config_dir().join("background")
}

/// Public so callers can check `.exists()` before `Config::load()` runs -
/// `load()` itself can't tell "the file was missing" apart from "the file
/// existed and happened to match `Config::default()`", which matters to
/// xeneon-app's `config_store::init()` (see its own doc comment on why).
pub fn config_file() -> PathBuf {
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
    // Whether the dedicated Home Assistant page exists in the carousel at
    // all - a plain on/off switch rather than an addable/removable page
    // like the widget pages, so "at most one" falls out for free (a bool
    // can't be true twice). Off by default: a freshly-installed app has no
    // Home Assistant instance to point at yet.
    pub ha_page_enabled: bool,
    // The Home Assistant dashboard URL the page's WebView loads - None
    // until the user fills it in (the enable switch alone doesn't imply a
    // URL is already known), same optional-until-configured shape as
    // `indicator_button_color` above.
    pub ha_page_url: Option<String>,
    // Philips Hue bridge pairing, shared by every Hue widget instance
    // rather than each card pairing on its own - matches how a real Hue
    // account works (one bridge, one link-button press) and lets several
    // cards on the dashboard (e.g. one per room, or a "favorites" card)
    // reuse the same connection. `None` until the user runs discovery +
    // pairing once from Settings (see xeneon-app's settings_page.rs).
    // `hue_bridge_ip` alone (no username yet) can happen if pairing was
    // started but never completed (link button not pressed in time) -
    // callers should treat that the same as "not configured".
    pub hue_bridge_ip: Option<String>,
    // The Hue bridge's per-application username - the credential proving
    // this app already pressed the link button once. Required on every
    // `/clip/v2` call as the `hue-application-key` header.
    pub hue_username: Option<String>,
    // Returned alongside `hue_username` on a successful pairing
    // (`generateclientkey: true`) - not used yet (only needed for the
    // Entertainment/streaming API, which this app doesn't do), kept only
    // so a full re-pair isn't needed if a future feature wants it.
    pub hue_clientkey: Option<String>,

    // Which carousel page to actually open on at launch, independent of
    // carousel *order* (the Home Assistant page, when enabled, is always
    // the leftmost/first page regardless - see xeneon-app's ha_page.rs).
    // `None` means "Page 1" (whichever real widget page is currently
    // first) - the implicit default so a fresh install needs no explicit
    // choice. `Some("ha")` (a plain sentinel string, not a real page id -
    // see xeneon-app's settings_page.rs for the one other place that
    // string is compared against) means the Home Assistant page; any
    // other `Some(id)` is a real widget page's own id. A value naming a
    // page that no longer exists (deleted, or Home Assistant since
    // disabled) is read the same as `None` by whoever resolves it -
    // deliberately not validated/corrected here, since `Config` itself
    // has no way to know which pages currently exist.
    pub default_page: Option<String>,
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
            hue_bridge_ip: None,
            hue_username: None,
            hue_clientkey: None,
            ha_page_enabled: false,
            ha_page_url: None,
            default_page: None,
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
                warn!("{} is corrupt or unreadable, falling back to defaults", path.display());
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
