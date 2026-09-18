// SPDX-License-Identifier: GPL-3.0-or-later
//! Thin shared-state wrapper around `xeneon_core::config::Config`, the
//! same role `XeneonApp.config` (a plain dict) plays in the Python
//! original: load once at startup, mutate + save immediately on every
//! settings change. A `thread_local!` rather than threading a `Config`
//! handle through every component that needs to read or change a setting
//! - fine since GTK's main loop (and everything touching these settings)
//! runs on a single thread, same rationale as `i18n_runtime`.

use std::cell::RefCell;
use xeneon_core::config::Config;

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::default());
}

pub fn init() {
    CONFIG.with(|c| *c.borrow_mut() = Config::load());
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
            eprintln!("xeneon-dashboard: failed to save config: {err}");
        }
    });
}
