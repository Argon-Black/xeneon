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
    fs::rename(&tmp_path, path)?;
    Ok(())
}
