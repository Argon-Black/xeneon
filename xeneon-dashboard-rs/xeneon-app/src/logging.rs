// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal custom `log` backend - stderr only, two verbosity levels tied
//! to `dev_mode_enabled()` (see `main::dev_mode_enabled` and the
//! `settings.dev_toggle` switch in settings_page.rs). Deliberately not
//! `env_logger`/`tracing`: this app only ever needs these two fixed
//! levels, not `RUST_LOG`-style runtime filtering or structured spans -
//! pulling either crate in would add several transitive dependencies for
//! a need this project doesn't have (see the deps-minimal discussion in
//! the memory system).

use log::{Level, LevelFilter, Log, Metadata, Record};

/// User mode: info/warn/error only - still enough for a bug report,
/// without dev-only noise (per-navigation events, etc.).
const USER_LEVEL: LevelFilter = LevelFilter::Info;
/// Dev mode adds debug, but only for `xeneon_app`/`xeneon_core`'s own
/// targets (see `level_for` below) - `trace` is never used anywhere in
/// this app, no code path here needs per-frame/per-call tracing, just
/// per-event.
const DEV_LEVEL: LevelFilter = LevelFilter::Debug;

/// Module path prefixes for this workspace's own crates, as `log` sees
/// them (`record.target()` defaults to the calling module path). Used to
/// keep dev mode's extra verbosity scoped to our own code - several
/// dependencies (`ureq`, `rustls`, ...) use the `log` facade internally
/// too, and turning their `debug!`/`trace!` on unfiltered was, in
/// testing, orders of magnitude noisier than anything this app logs
/// itself (full HTTP request/response dumps, TLS handshake internals).
const OWN_CRATE_PREFIXES: [&str; 2] = ["xeneon_app", "xeneon_core"];

struct StderrLogger {
    /// Latched once at `init()` rather than re-read from the env var on
    /// every log call - `dev_mode_enabled()` is already fixed for the
    /// whole process lifetime (set before launch, see `relaunch()` in
    /// settings_page.rs), so re-checking it per line would just be a
    /// wasted syscall.
    dev_mode: bool,
}

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level_for(metadata.target())
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        if self.dev_mode {
            // Dev mode: timestamp + level + module path (which widget/
            // file the line came from) + message - enough to pinpoint
            // the exact code path from a dev session's terminal output.
            let time = chrono::Local::now().format("%H:%M:%S%.3f");
            eprintln!("{time} {:<5} {}: {}", record.level(), record.target(), record.args());
        } else {
            // User mode: level + message only, no module path/timestamp -
            // short enough to paste straight into a bug report.
            eprintln!("{:<5} {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

impl StderrLogger {
    /// The max level to let through for a given log `target` - debug in
    /// dev mode, but only for this workspace's own crates (see
    /// `OWN_CRATE_PREFIXES`); everything else (dependencies, dev mode or
    /// not) is capped at the same level user mode gets.
    fn level_for(&self, target: &str) -> Level {
        let is_own_crate = OWN_CRATE_PREFIXES.iter().any(|prefix| target.starts_with(prefix));
        if self.dev_mode && is_own_crate { Level::Debug } else { Level::Info }
    }
}

/// Installs the global logger. Must run once, at the very start of
/// `main()`, before anything else calls `log::info!`/`warn!`/`debug!`/
/// etc. (a call before this point is silently dropped, per the `log`
/// crate's own default no-op logger - not a panic, so this is easy to
/// get away with forgetting, hence this note).
pub fn init(dev_mode: bool) {
    log::set_max_level(if dev_mode { DEV_LEVEL } else { USER_LEVEL });
    log::set_boxed_logger(Box::new(StderrLogger { dev_mode }))
        .expect("logger must only be installed once, at startup");
}
