// SPDX-License-Identifier: GPL-3.0-or-later
//! Packing/unpacking the config directory as a single gzip-compressed tar
//! archive - the mechanism export/import (see `xeneon-app`'s own `backup`
//! module) builds on. A single file rather than a plain folder copy: easy
//! to move, attach, or store anywhere a folder isn't, at the cost of two
//! small, well-established dependencies (`tar`, `flate2`) instead of none -
//! both pure Rust (flate2's default `miniz_oxide` backend needs no system
//! zlib), so this stays as portable as the rest of the app, including
//! inside a future Flatpak sandbox.

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// Creates a gzip-compressed tar archive at `archive_path` containing
/// everything under `src_dir` (recursively), with paths inside the archive
/// relative to `src_dir` itself (e.g. `config.json`, not
/// `xeneon-dashboard-rs/config.json`) - so extracting it back always
/// recreates a directory with exactly `src_dir`'s own layout, regardless of
/// what `src_dir` was actually called on the machine that made the export.
pub fn create_archive(src_dir: &Path, archive_path: &Path) -> io::Result<()> {
    let file = File::create(archive_path)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.append_dir_all(".", src_dir)?;
    builder.into_inner()?.finish()?;
    Ok(())
}

/// Extracts a gzip-compressed tar archive (as `create_archive` produces)
/// into `dest_dir`, creating it if needed.
pub fn extract_archive(archive_path: &Path, dest_dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dest_dir)?;
    let file = File::open(archive_path)?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    archive.unpack(dest_dir)
}

/// Whether `archive_path` looks like something `create_archive` produced -
/// peeks at the archive's own entry list for a top-level `config.json`
/// without extracting anything. A cheap sanity check against importing an
/// unrelated file by mistake, not a full validation of the archive's
/// contents.
pub fn looks_like_config_archive(archive_path: &Path) -> bool {
    let Ok(file) = File::open(archive_path) else { return false };
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let Ok(entries) = archive.entries() else { return false };
    entries.flatten().any(|entry| {
        let Ok(path) = entry.path() else { return false };
        // `append_dir_all(".", ...)` prefixes every entry with "./" -
        // strip that before comparing, so this doesn't depend on exactly
        // how the archive's paths happen to be spelled.
        let normalized: PathBuf = path.components().filter(|c| !matches!(c, std::path::Component::CurDir)).collect();
        normalized == Path::new("config.json")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn round_trips_nested_files_and_directories() {
        let tmp = std::env::temp_dir().join(format!("xeneon-test-backup-{}", uuid::Uuid::new_v4()));
        let src = tmp.join("src");
        fs::create_dir_all(src.join("pages")).unwrap();
        fs::write(src.join("config.json"), b"{}").unwrap();
        fs::write(src.join("pages").join("p1.json"), b"{\"id\":\"p1\"}").unwrap();

        let archive = tmp.join("export.tar.gz");
        create_archive(&src, &archive).unwrap();

        let dest = tmp.join("dest");
        extract_archive(&archive, &dest).unwrap();

        assert_eq!(fs::read_to_string(dest.join("config.json")).unwrap(), "{}");
        assert_eq!(fs::read_to_string(dest.join("pages").join("p1.json")).unwrap(), "{\"id\":\"p1\"}");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn looks_like_config_archive_checks_for_a_top_level_config_json() {
        let tmp = std::env::temp_dir().join(format!("xeneon-test-backup-check-{}", uuid::Uuid::new_v4()));
        let with_config = tmp.join("with_config");
        fs::create_dir_all(&with_config).unwrap();
        fs::write(with_config.join("config.json"), b"{}").unwrap();
        let archive_with = tmp.join("with.tar.gz");
        create_archive(&with_config, &archive_with).unwrap();
        assert!(looks_like_config_archive(&archive_with));

        let without_config = tmp.join("without_config");
        fs::create_dir_all(without_config.join("nested")).unwrap();
        fs::write(without_config.join("nested").join("config.json"), b"{}").unwrap();
        let archive_without = tmp.join("without.tar.gz");
        create_archive(&without_config, &archive_without).unwrap();
        assert!(!looks_like_config_archive(&archive_without));

        let _ = fs::remove_dir_all(&tmp);
    }
}
