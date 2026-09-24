// SPDX-License-Identifier: GPL-3.0-or-later
//! Recursively copying a directory tree - the mechanism export/import (see
//! `xeneon-app`'s own `backup` module) builds on. A plain folder copy
//! rather than a zip/tar archive: no new dependency to read or write one,
//! and the result stays just as inspectable/editable by hand as the config
//! directory itself.

use std::fs;
use std::io;
use std::path::Path;

/// Recursively copies every file and subdirectory from `src` into `dst`,
/// creating `dst` (and any subdirectory under it) as needed. Symlinks
/// aren't followed specially - `entry.file_type()` reports a symlink as
/// neither `is_dir()` nor `is_file()`, so one is silently skipped rather
/// than copied or resolved; the config directory this is meant for never
/// contains any.
pub fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &dst_path)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

/// Whether `dir` looks like an exported (or live) config directory - just
/// checks for `config.json` directly inside it, the one file every real
/// config directory always has (see `config::config_file`). A cheap sanity
/// check against importing an unrelated folder by mistake, not a full
/// validation of its contents.
pub fn looks_like_config_dir(dir: &Path) -> bool {
    dir.join("config.json").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_nested_files_and_directories() {
        let tmp = std::env::temp_dir().join(format!("xeneon-test-backup-{}", uuid::Uuid::new_v4()));
        let src = tmp.join("src");
        fs::create_dir_all(src.join("pages")).unwrap();
        fs::write(src.join("config.json"), b"{}").unwrap();
        fs::write(src.join("pages").join("p1.json"), b"{}").unwrap();

        let dst = tmp.join("dst");
        copy_dir_all(&src, &dst).unwrap();

        assert!(dst.join("config.json").is_file());
        assert!(dst.join("pages").join("p1.json").is_file());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn looks_like_config_dir_checks_for_config_json() {
        let tmp = std::env::temp_dir().join(format!("xeneon-test-backup-check-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        assert!(!looks_like_config_dir(&tmp));

        fs::write(tmp.join("config.json"), b"{}").unwrap();
        assert!(looks_like_config_dir(&tmp));

        let _ = fs::remove_dir_all(&tmp);
    }
}
