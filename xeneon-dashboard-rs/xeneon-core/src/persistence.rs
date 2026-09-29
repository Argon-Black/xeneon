// SPDX-License-Identifier: GPL-3.0-or-later
//! Small shared helper for writing JSON state to disk. Kept separate from
//! `config`/`widget_state`/`page_state` since all three need exactly this
//! one operation and shouldn't each reimplement it slightly differently.

use serde::Serialize;
use std::fs;
use std::io;
use std::path::Path;

/// Writes `value` as pretty-printed JSON to `path`, atomically: to a
/// sibling `<path>.tmp` file first, then renamed into place. A crash or
/// power loss mid-write can then never leave a half-written, corrupt file
/// behind - the Python original (`config.py`/`widget_store.py`) writes
/// directly in one call and doesn't have this guarantee; this is a small,
/// low-risk improvement worth making in the port rather than reproducing
/// the gap verbatim.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(".tmp");
    let tmp_path = std::path::PathBuf::from(tmp_name);

    let body = serde_json::to_string_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(&tmp_path, body)?;
    // Owner-only, rather than relying solely on the inherited umask -
    // `config.json` (one of the files that goes through this helper) can
    // hold the Hue bridge's long-lived pairing token in plaintext. Audit
    // finding 2026-09-29, same gap already fixed for the YouTube cookie
    // jar's directory (xeneon-app/src/widgets/youtube.rs). Set on the tmp
    // file before the rename - `fs::rename` keeps the source file's own
    // mode, not the destination's, so this still applies to the final
    // path.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn written_file_is_owner_only() {
        let dir = std::env::temp_dir().join(format!("xeneon-test-perms-{}", uuid::Uuid::new_v4()));
        let path = dir.join("test.json");
        write_json_atomic(&path, &serde_json::json!({"a": 1})).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected owner-only permissions, got {mode:o}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
