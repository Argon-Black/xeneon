//! Per-page persistence: one JSON file per carousel page under
//! `pages/<id>.json`, mirroring `page_store.py`/`window.py::_save_page`. A
//! page can exist (with a custom name or background) even with zero
//! widgets on it, which is why page count can't be derived from the widget
//! list alone - both must be loaded and reconciled by whoever rebuilds the
//! carousel (`xeneon-app`).
//!
//! `background` is left as an opaque `serde_json::Value` for now - the
//! Python `PageBackground` shape wasn't ported in this phase, so this is a
//! placeholder pass-through until that widget-background feature is built.

use crate::persistence;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PageState {
    #[serde(skip)]
    pub id: String,
    pub page_index: usize,
    pub name: Option<String>,
    pub background: serde_json::Value,
}

impl Default for PageState {
    fn default() -> Self {
        Self { id: String::new(), page_index: 0, name: None, background: serde_json::Value::Null }
    }
}

/// Loads every `pages/<id>.json` file, sorted by `page_index`. Same
/// resilience as `widget_state::load_all`: missing directory -> empty list,
/// corrupt individual file -> skipped with a warning, not fatal.
pub fn load_all(pages_dir: &Path) -> Vec<PageState> {
    let Ok(entries) = fs::read_dir(pages_dir) else {
        return Vec::new();
    };
    let mut states = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        match fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str::<PageState>(&text).ok()) {
            Some(mut state) => {
                state.id = id;
                states.push(state);
            }
            None => eprintln!("xeneon-dashboard: skipping corrupt page file {}", path.display()),
        }
    }
    states.sort_by_key(|s| s.page_index);
    states
}

pub fn save(pages_dir: &Path, state: &PageState) -> std::io::Result<()> {
    persistence::write_json_atomic(&pages_dir.join(format!("{}.json", state.id)), state)
}

pub fn delete(pages_dir: &Path, id: &str) -> std::io::Result<()> {
    let path = pages_dir.join(format!("{id}.json"));
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_round_trip_with_custom_name() {
        let dir = std::env::temp_dir().join(format!("xeneon-test-pages-{}", uuid::Uuid::new_v4()));
        let state = PageState { id: "p1".to_string(), page_index: 2, name: Some("Cuisine".to_string()), ..Default::default() };
        save(&dir, &state).unwrap();

        let loaded = load_all(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name.as_deref(), Some("Cuisine"));
        assert_eq!(loaded[0].id, "p1");

        let _ = fs::remove_dir_all(&dir);
    }
}
