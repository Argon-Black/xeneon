// SPDX-License-Identifier: GPL-3.0-or-later
//! Live-retranslation glue on top of `xeneon_core::i18n`'s pure lookup:
//! one global active language + a list of no-arg callbacks fired whenever
//! it changes, mirroring `i18n.py`'s module-level `_listeners`/`on_change`.
//! A `thread_local!` rather than a cross-thread-safe global is enough (and
//! simpler) since GTK's main loop - and everything touching these widgets
//! - runs on a single thread.

use std::cell::RefCell;
use std::path::PathBuf;
use xeneon_core::i18n::{self, Catalog};

const FALLBACK_LANGUAGE: &str = "fr";

struct State {
    locales_dir: PathBuf,
    fallback: Catalog,
    active: Catalog,
    current_code: String,
    listeners: Vec<Box<dyn Fn()>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        locales_dir: PathBuf::new(),
        fallback: Catalog::default(),
        active: Catalog::default(),
        current_code: String::new(),
        listeners: Vec::new(),
    });
}

/// `locales_dir` is resolved once at startup, relative to the crate's own
/// source directory (`CARGO_MANIFEST_DIR`) rather than the process's
/// current working directory - reliable for `cargo run`/`cargo build`
/// regardless of where they're invoked from. Revisit once real packaging
/// needs locales installed under an XDG data dir instead.
///
/// `language` is the saved `Config.language` (`"fr"` by default on a
/// first run, via `Config::default()`) - callers read it from
/// `config_store::get()` before calling this, matching `i18n.init()`
/// taking the configured language in the Python original.
pub fn init(language: &str) {
    let locales_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("locales");
    let fallback = i18n::load_catalog(&locales_dir, FALLBACK_LANGUAGE);
    let active = i18n::load_catalog(&locales_dir, language);
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.locales_dir = locales_dir;
        state.fallback = fallback;
        state.active = active;
        state.current_code = language.to_string();
    });
}

pub fn current_language() -> String {
    STATE.with(|state| state.borrow().current_code.clone())
}

pub fn available_languages() -> Vec<(String, String)> {
    STATE.with(|state| i18n::discover_languages(&state.borrow().locales_dir))
}

/// Switches the active language and fires every registered listener so
/// live widgets can re-fetch their translated text - mirrors
/// `i18n.set_language()`.
pub fn set_language(code: &str) {
    let listeners_to_call = STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.current_code == code {
            return None;
        }
        state.active = i18n::load_catalog(&state.locales_dir, code);
        state.current_code = code.to_string();
        Some(state.listeners.len())
    });
    // Called outside the borrow: a listener may itself call t()/on_change()
    // while retranslating, which would otherwise re-enter STATE.borrow_mut().
    if listeners_to_call.is_some() {
        STATE.with(|state| {
            let state = state.borrow();
            for listener in &state.listeners {
                listener();
            }
        });
    }
}

/// Registers a no-arg callback fired whenever the active language changes
/// - mirrors `i18n.on_change()`. Call once per widget instance, typically
/// at the end of its constructor.
pub fn on_change(listener: impl Fn() + 'static) {
    STATE.with(|state| state.borrow_mut().listeners.push(Box::new(listener)));
}

/// Translates `key`, falling back through the fallback language then the
/// raw key itself - mirrors `i18n._()`.
pub fn t(key: &str) -> String {
    STATE.with(|state| {
        let state = state.borrow();
        i18n::translate(&state.active, &state.fallback, key, &[])
    })
}

/// Same as [`t`], with `{name}`-style placeholders substituted.
pub fn t_args(key: &str, args: &[(&str, &str)]) -> String {
    STATE.with(|state| {
        let state = state.borrow();
        i18n::translate(&state.active, &state.fallback, key, args)
    })
}
