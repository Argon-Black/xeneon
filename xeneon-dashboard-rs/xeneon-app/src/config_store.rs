// SPDX-License-Identifier: GPL-3.0-or-later
//! Thin shared-state wrapper around `xeneon_core::config::Config`, the
//! same role `XeneonApp.config` (a plain dict) plays in the Python
//! original: load once at startup, mutate + save immediately on every
//! settings change. A `thread_local!` rather than threading a `Config`
//! handle through every component that needs to read or change a setting
//! - fine since GTK's main loop (and everything touching these settings)
//! runs on a single thread, same rationale as `i18n_runtime`.

use log::warn;
use std::cell::RefCell;
use xeneon_core::config::Config;

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::default());
}

// Resolved relative to this crate's own source directory, same mechanism
// (and same reason: reliable under `cargo run`/`cargo build` regardless of
// the process's working directory) as `widgets/audio.rs`'s
// `EMPTY_STATE_ICON_PATH`.
const DEFAULT_BACKGROUND_ASSET: &str = "assets/default-background.svg";

fn default_background_source() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_BACKGROUND_ASSET)
}

/// Copies the bundled default background image into `background_dir()` and
/// returns the path it was stored at - shared by `init()` (first run) and
/// `restore_default_background()` (what "Retirer l'image" actually does,
/// see below). Copied in rather than pointed at straight from the source
/// tree - same storage every user-picked background image goes through
/// (see `settings_page.rs`), so the config directory stays self-contained,
/// not dependent on this crate's own source checkout still being around.
fn store_default_background() -> std::io::Result<String> {
    let path = xeneon_core::assets::store_asset(&xeneon_core::config::background_dir(), &default_background_source())?;
    Ok(path.display().to_string())
}

/// Whether `path` is the bundled default background's own stored path, as
/// opposed to one the user picked through the file dialog - compares by
/// content hash (the same one `store_asset` names files with), not by
/// string equality with whatever `store_default_background` last returned,
/// so it stays correct even if the bundled asset's own filename changes.
/// Used by `settings_page.rs` to decide whether "Choisir une image" or
/// "Retirer l'image" is the one that makes sense to offer right now.
pub fn is_default_background(path: &str) -> bool {
    match xeneon_core::assets::content_filename(&default_background_source()) {
        Ok(default_name) => std::path::Path::new(path).file_name().and_then(|f| f.to_str()) == Some(default_name.as_str()),
        Err(_) => false,
    }
}

pub fn init() {
    // A brand-new install (no config.json on disk yet) starts with this
    // bundled image as the app-wide background - saved immediately so it
    // becomes an ordinary user-editable path from then on, same as one
    // chosen through the file picker. That matters because Config::load()
    // alone can't tell "the file was missing, so these are just defaults"
    // apart from "the file existed and happened to match defaults" - only
    // checking existence *before* loading can, and only there can this
    // decision be made once, correctly: doing it after every load would
    // silently overwrite a background the user picked on every subsequent
    // launch instead of respecting it.
    let first_run = !xeneon_core::config::config_file().exists();
    let mut config = Config::load();
    if first_run {
        match store_default_background() {
            Ok(path) => config.app_background_image_path = Some(path),
            Err(err) => warn!("failed to copy default background image: {err}"),
        }
        if let Err(err) = config.save() {
            warn!("failed to save config: {err}");
        }
    }
    CONFIG.with(|c| *c.borrow_mut() = config);
}

/// Re-copies the bundled default background image and sets it as the
/// current app-wide background. What "Retirer l'image" in the settings page
/// actually does when a custom image is set - there's no "no background at
/// all" state to fall into, only ever "the default" or "something the user
/// picked".
pub fn restore_default_background() -> std::io::Result<()> {
    let path = store_default_background()?;
    update(|c| c.app_background_image_path = Some(path));
    Ok(())
}

pub fn get() -> Config {
    CONFIG.with(|c| c.borrow().clone())
}

/// Applies `f` to the current config and saves immediately - mirrors every
/// `XeneonApp.set_*` method's `self.config[...] = value; config.save(...)`
/// pattern.
pub fn update(f: impl FnOnce(&mut Config)) {
    CONFIG.with(|c| {
        let mut cfg = c.borrow_mut();
        f(&mut cfg);
        if let Err(err) = cfg.save() {
            warn!("failed to save config: {err}");
        }
    });
}
