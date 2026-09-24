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

pub fn init() {
    // A brand-new install (no config.json on disk yet) starts with this
    // bundled image as the app-wide background - saved immediately so it
    // becomes an ordinary user-editable path from then on, same as one
    // chosen through the file picker. That matters because Config::load()
    // alone can't tell "the file was missing, so these are just defaults"
    // apart from "the file existed and happened to match defaults" - only
    // checking existence *before* loading can, and only there can this
    // decision be made once, correctly: doing it after every load would
    // silently undo "Retirer l'image" (see settings_page.rs) on every
    // subsequent launch instead of respecting that the user cleared it.
    let first_run = !xeneon_core::config::config_file().exists();
    let mut config = Config::load();
    if first_run {
        // Copied into `background_dir()` rather than pointed at
        // straight from the source tree - same storage every
        // user-picked background image goes through (see
        // `settings_page.rs`), so a fresh install's config directory is
        // self-contained from the very first launch, not dependent on
        // this crate's own source checkout still being around.
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_BACKGROUND_ASSET);
        match xeneon_core::assets::store_asset(&xeneon_core::config::background_dir(), &source) {
            Ok(path) => config.app_background_image_path = Some(path.display().to_string()),
            Err(err) => warn!("failed to copy default background image: {err}"),
        }
        if let Err(err) = config.save() {
            warn!("failed to save config: {err}");
        }
    }
    CONFIG.with(|c| *c.borrow_mut() = config);
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
