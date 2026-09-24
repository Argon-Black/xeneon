// SPDX-License-Identifier: GPL-3.0-or-later
//! Export/import of the whole config directory as a plain folder - see
//! `xeneon_core::backup` for the underlying recursive copy. Export just
//! copies `config_dir()` out; import replaces it, keeping the previous one
//! aside rather than deleting it outright (see `import`'s own doc
//! comment), and requires the caller to relaunch the app afterward (see
//! `settings_page::relaunch`) since so much state (`Config`, every
//! `WidgetGrid`, ...) is only ever loaded once at startup.

use std::io;
use std::path::{Path, PathBuf};
use xeneon_core::backup::copy_dir_all;
use xeneon_core::config::config_dir;

pub use xeneon_core::backup::looks_like_config_dir;

/// Copies the whole config directory into a fresh, timestamped subfolder of
/// `destination` - never directly into `destination` itself, so picking an
/// already-populated folder (Documents, say) can't dump loose files into it
/// or collide with something already there. Returns the folder actually
/// written to.
pub fn export(destination: &Path) -> io::Result<PathBuf> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let target = destination.join(format!("xeneon-dashboard-config-{stamp}"));
    copy_dir_all(&config_dir(), &target)?;
    Ok(target)
}

/// Replaces the current config directory with `source`'s contents. The
/// previous config directory is renamed aside (its own name plus
/// `.before-import-<timestamp>`) rather than deleted outright, in case
/// `source` turns out to be the wrong folder - the settings page already
/// asks for confirmation before calling this, but a mistaken import
/// shouldn't be unrecoverable on top of that. The caller must relaunch the
/// app afterward (see this module's own doc comment) - nothing here
/// reloads any in-memory state.
pub fn import(source: &Path) -> io::Result<()> {
    let current = config_dir();
    if current.exists() {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let backup_name = format!("{}.before-import-{stamp}", current.file_name().and_then(|n| n.to_str()).unwrap_or("xeneon-dashboard-rs"));
        std::fs::rename(&current, current.with_file_name(backup_name))?;
    }
    copy_dir_all(source, &current)
}
