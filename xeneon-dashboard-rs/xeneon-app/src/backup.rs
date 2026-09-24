// SPDX-License-Identifier: GPL-3.0-or-later
//! Export/import of the whole config directory as a single gzip-compressed
//! tar archive - see `xeneon_core::backup` for the underlying archive
//! read/write. Export just archives `config_dir()`; import extracts into a
//! staging directory first and only swaps it into place once that fully
//! succeeds (see `import`'s own doc comment), and requires the caller to
//! relaunch the app afterward (see `settings_page::relaunch`) since so much
//! state (`Config`, every `WidgetGrid`, ...) is only ever loaded once at
//! startup.

use std::io;
use std::path::Path;
use xeneon_core::backup::{create_archive, extract_archive};
use xeneon_core::config::config_dir;

pub use xeneon_core::backup::looks_like_config_archive;

/// Archives the whole config directory into `archive_path` (a `.tar.gz`
/// file the user picked, in a save dialog that already suggests a
/// sensible default name - see `settings_page.rs`).
pub fn export(archive_path: &Path) -> io::Result<()> {
    create_archive(&config_dir(), archive_path)
}

/// Replaces the current config directory with `archive_path`'s contents.
/// Extracted into a fresh staging directory first, alongside the real one
/// rather than into it directly - only once that fully succeeds does this
/// rename the current config directory aside (its own name plus
/// `.before-import-<timestamp>`, not deleted outright) and rename the
/// staging directory into its place. That way a corrupt or truncated
/// archive fails loudly without having touched the live configuration at
/// all, rather than leaving it half-overwritten. The caller must relaunch
/// the app afterward (see this module's own doc comment) - nothing here
/// reloads any in-memory state.
pub fn import(archive_path: &Path) -> io::Result<()> {
    let current = config_dir();
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let base_name = current.file_name().and_then(|n| n.to_str()).unwrap_or("xeneon-dashboard-rs");

    let staging = current.with_file_name(format!("{base_name}.importing-{stamp}"));
    extract_archive(archive_path, &staging)?;

    if current.exists() {
        let backup = current.with_file_name(format!("{base_name}.before-import-{stamp}"));
        std::fs::rename(&current, backup)?;
    }
    std::fs::rename(&staging, &current)
}
