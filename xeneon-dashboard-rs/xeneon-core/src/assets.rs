// SPDX-License-Identifier: GPL-3.0-or-later
//! Copying user-picked binary files (background images, icons, ...) into a
//! directory under `config_dir()`, so the app's saved state stops pointing
//! at wherever the file happened to live when it was chosen (the user's
//! `~/Pictures`, a USB stick, ...) and becomes self-contained - a
//! prerequisite for a future "export my config" that's just "copy this
//! directory", since a raw external path can't be exported at all.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

/// Copies `source` into `dir`, named after a hash of its own bytes (not a
/// cryptographic hash - collision resistance for dedup purposes only, no
/// untrusted-input threat model here). Two callers storing the same image
/// land on the same filename and the second copy is skipped, so picking the
/// same picture for several widgets/pages doesn't duplicate it on disk.
/// Returns the path the file was stored at.
///
/// This is the mechanism `config_store::init()` uses for the bundled
/// default background, and `settings_page.rs` for a user-picked one; a
/// future per-page background would reuse it the same way, storing into
/// `pages_dir()` instead of `config::background_dir()` so each page's image
/// lives alongside that page's own `<id>.json`.
pub fn store_asset(dir: &Path, source: &Path) -> io::Result<PathBuf> {
    let bytes = fs::read(source)?;
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    let hash = hasher.finish();

    let ext = source.extension().and_then(|e| e.to_str());
    let filename = match ext {
        Some(ext) => format!("{hash:016x}.{ext}"),
        None => format!("{hash:016x}"),
    };

    fs::create_dir_all(dir)?;
    let dest = dir.join(filename);
    if !dest.exists() {
        fs::write(&dest, &bytes)?;
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_file_under_hashed_name() {
        let tmp = std::env::temp_dir().join(format!("xeneon-test-assets-{}", uuid::Uuid::new_v4()));
        let source_dir = tmp.join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("photo.png");
        fs::write(&source, b"pretend image bytes").unwrap();

        let dest_dir = tmp.join("dest");
        let stored = store_asset(&dest_dir, &source).unwrap();

        assert_eq!(stored.parent(), Some(dest_dir.as_path()));
        assert_eq!(stored.extension().and_then(|e| e.to_str()), Some("png"));
        assert_eq!(fs::read(&stored).unwrap(), b"pretend image bytes");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn identical_content_dedups_to_the_same_file() {
        let tmp = std::env::temp_dir().join(format!("xeneon-test-assets-dedup-{}", uuid::Uuid::new_v4()));
        let source_dir = tmp.join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let a = source_dir.join("a.jpg");
        let b = source_dir.join("b.jpg");
        fs::write(&a, b"same bytes").unwrap();
        fs::write(&b, b"same bytes").unwrap();

        let dest_dir = tmp.join("dest");
        let stored_a = store_asset(&dest_dir, &a).unwrap();
        let stored_b = store_asset(&dest_dir, &b).unwrap();

        assert_eq!(stored_a, stored_b);
        assert_eq!(fs::read_dir(&dest_dir).unwrap().count(), 1);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn different_content_gets_different_files() {
        let tmp = std::env::temp_dir().join(format!("xeneon-test-assets-distinct-{}", uuid::Uuid::new_v4()));
        let source_dir = tmp.join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let a = source_dir.join("a.jpg");
        let b = source_dir.join("b.jpg");
        fs::write(&a, b"bytes one").unwrap();
        fs::write(&b, b"bytes two").unwrap();

        let dest_dir = tmp.join("dest");
        let stored_a = store_asset(&dest_dir, &a).unwrap();
        let stored_b = store_asset(&dest_dir, &b).unwrap();

        assert_ne!(stored_a, stored_b);

        let _ = fs::remove_dir_all(&tmp);
    }
}
