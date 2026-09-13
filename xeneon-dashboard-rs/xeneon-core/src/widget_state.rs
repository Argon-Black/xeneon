//! Per-widget persistence: one JSON file per widget instance under
//! `widgets/<id>.json`. Ported from `widget_store.py` (the file format) and
//! `window.py::_save_widget` (who builds it). `content` is left as an
//! opaque `serde_json::Value` here - it's plugin-specific and this crate
//! doesn't know about individual widget kinds; `xeneon-app`'s plugin
//! registry parses it per `kind` when restoring.

use crate::appearance::WidgetAppearance;
use crate::persistence;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// One saved widget instance: identity (`id`) is separate from its
/// position/page (which move freely) - `id` is the JSON filename itself,
/// injected after loading rather than duplicated inside the file body, the
/// same identity/position split used throughout the original app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WidgetState {
    #[serde(skip)]
    pub id: String,
    pub kind: String,
    pub page_index: usize,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub appearance: WidgetAppearance,
    pub content: serde_json::Value,
}

impl Default for WidgetState {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: String::new(),
            page_index: 0,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            appearance: WidgetAppearance::default(),
            content: serde_json::Value::Null,
        }
    }
}

/// Loads every `widgets/<id>.json` file, sorted by `(page_index, y, x)` -
/// the same deterministic top-to-bottom, left-to-right rebuild order
/// `widget_store.load_all` uses. A missing directory yields an empty list
/// (nothing saved yet); a corrupt or unreadable individual file is skipped
/// with a warning rather than aborting the whole load.
pub fn load_all(widgets_dir: &Path) -> Vec<WidgetState> {
    let Ok(entries) = fs::read_dir(widgets_dir) else {
        return Vec::new();
    };
    let mut states = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        match fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str::<WidgetState>(&text).ok()) {
            Some(mut state) => {
                state.id = id;
                states.push(state);
            }
            None => eprintln!("xeneon-dashboard: skipping corrupt widget file {}", path.display()),
        }
    }
    states.sort_by_key(|s| (s.page_index, s.y, s.x));
    states
}

pub fn save(widgets_dir: &Path, state: &WidgetState) -> std::io::Result<()> {
    persistence::write_json_atomic(&widgets_dir.join(format!("{}.json", state.id)), state)
}

pub fn delete(widgets_dir: &Path, id: &str) -> std::io::Result<()> {
    let path = widgets_dir.join(format!("{id}.json"));
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Rewrites just the `page_index` field of an already-saved widget file in
/// place - used when a page's own index shifts (e.g. an earlier page was
/// deleted and every later page moves down by one) so this widget stays
/// correctly bucketed on the next restart. Reads the file back first
/// rather than reconstructing a fresh `WidgetState` from live state, since
/// the caller (a `WidgetGrid` reindexing itself) only has the widget's id
/// and rect on hand, not its kind/appearance/content.
pub fn update_page_index(widgets_dir: &Path, id: &str, new_page_index: usize) -> std::io::Result<()> {
    let path = widgets_dir.join(format!("{id}.json"));
    let text = fs::read_to_string(&path)?;
    let mut state: WidgetState =
        serde_json::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    state.id = id.to_string();
    state.page_index = new_page_index;
    persistence::write_json_atomic(&path, &state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::TouchedField;

    #[test]
    fn save_load_delete_round_trip() {
        let dir = std::env::temp_dir().join(format!("xeneon-test-widgets-{}", uuid::Uuid::new_v4()));

        let mut state = WidgetState {
            id: "abc123".to_string(),
            kind: "clock".to_string(),
            page_index: 1,
            x: 10,
            y: 20,
            w: 832,
            h: 336,
            ..Default::default()
        };
        state.appearance.touched.insert(TouchedField::Bg);
        state.content = serde_json::json!({"city_key": "widgets.clock.cities.paris"});

        save(&dir, &state).unwrap();
        let loaded = load_all(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "abc123"); // recovered from the filename, not the body
        assert_eq!(loaded[0].kind, "clock");
        assert_eq!(loaded[0].content["city_key"], "widgets.clock.cities.paris");

        delete(&dir, "abc123").unwrap();
        assert!(load_all(&dir).is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_page_index_rewrites_only_that_field() {
        let dir = std::env::temp_dir().join(format!("xeneon-test-reindex-{}", uuid::Uuid::new_v4()));

        let state = WidgetState {
            id: "abc123".to_string(),
            kind: "clock".to_string(),
            page_index: 2,
            x: 10,
            y: 20,
            content: serde_json::json!({"city_key": "widgets.clock.cities.paris"}),
            ..Default::default()
        };
        save(&dir, &state).unwrap();

        update_page_index(&dir, "abc123", 1).unwrap();

        let loaded = load_all(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].page_index, 1);
        assert_eq!(loaded[0].x, 10);
        assert_eq!(loaded[0].content["city_key"], "widgets.clock.cities.paris");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_directory_yields_empty_list_not_an_error() {
        let dir = std::env::temp_dir().join(format!("xeneon-test-missing-{}", uuid::Uuid::new_v4()));
        assert!(load_all(&dir).is_empty());
    }

    #[test]
    fn corrupt_file_is_skipped_not_fatal() {
        let dir = std::env::temp_dir().join(format!("xeneon-test-corrupt-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("broken.json"), "{not valid json").unwrap();
        fs::write(
            dir.join("fine.json"),
            serde_json::to_string(&WidgetState { kind: "dummy_s".to_string(), ..Default::default() }).unwrap(),
        )
        .unwrap();

        let loaded = load_all(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].kind, "dummy_s");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sorted_by_page_then_y_then_x() {
        let dir = std::env::temp_dir().join(format!("xeneon-test-sort-{}", uuid::Uuid::new_v4()));
        for (id, page, y, x) in [("a", 1, 0, 0), ("b", 0, 50, 0), ("c", 0, 0, 100), ("d", 0, 0, 0)] {
            save(
                &dir,
                &WidgetState { id: id.to_string(), page_index: page, y, x, ..Default::default() },
            )
            .unwrap();
        }
        let ids: Vec<String> = load_all(&dir).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["d", "c", "b", "a"]);

        let _ = fs::remove_dir_all(&dir);
    }
}
