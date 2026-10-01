// SPDX-License-Identifier: GPL-3.0-or-later
//! Resolves a path to a bundled resource (an icon, a locale catalog, the
//! default background...) relative to wherever this binary actually ended
//! up: under an installed prefix (`<prefix>/bin/xeneon-app` ->
//! `<prefix>/share/xeneon-dashboard-rust/<relative>`, matching where the
//! Flatpak manifest - or any other packaging - installs `assets/`/
//! `resources/` alongside the binary) when that directory exists, falling
//! back to this crate's own source tree (`CARGO_MANIFEST_DIR`) otherwise -
//! the case for `cargo run`/`cargo build`, where nothing was ever
//! installed anywhere.
//!
//! Every one of this module's callers used to bake `CARGO_MANIFEST_DIR` in
//! directly, which only ever worked run from a source checkout - this is a
//! real, packaging-independent bug (it would have broken identically
//! behind a plain `.deb`/`.rpm`/`cp`-the-binary-elsewhere install, not just
//! a Flatpak sandbox), not something specific to Flatpak. This is the one
//! place that "installed vs. dev tree" distinction lives now, instead of
//! being duplicated at each of the 8 call sites it used to live at.

use std::path::{Path, PathBuf};

fn installed_prefix() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bin_dir = exe.parent()?;
    let prefix = bin_dir.parent()?;
    Some(prefix.to_path_buf())
}

/// `relative` is a path like `"assets/audio-empty.svg"` or
/// `"resources/icons"`, always relative to this crate's own root
/// (`xeneon-app/`) - matching both where `resources/`/`assets/` sit in the
/// source tree and where the Flatpak manifest installs them.
pub(crate) fn resource_path(relative: &str) -> PathBuf {
    if let Some(installed) = installed_prefix().map(|prefix| prefix.join("share/xeneon-dashboard-rust").join(relative))
    {
        if installed.exists() {
            return installed;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}
