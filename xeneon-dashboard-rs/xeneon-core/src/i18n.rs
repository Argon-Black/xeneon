// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure data/lookup side of the translation system: loading a locale's
//! flat `{key: text}` JSON file, discovering which languages are
//! available, and the fallback/substitution lookup itself. Ported from
//! `i18n.py`. The live "retranslate everything when the language changes"
//! notification bus is GTK/Relm4-shaped glue, not pure logic, so it lives
//! in `xeneon-app` instead (see `i18n_runtime.rs` there).

use log::warn;
use std::collections::HashMap;
use std::path::Path;

/// One language's strings: a flat `{"a.b.c": "text"}` map, same shape as a
/// `locales/<code>.json` file - including the special `_language_name` key
/// naming the language in its own tongue.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    strings: HashMap<String, String>,
}

impl Catalog {
    pub fn from_json_str(json: &str) -> Self {
        // A parse failure used to silently yield an empty catalog - every
        // key then falling back to the raw key everywhere it's used
        // (see `translate`), with nothing to say the locale file itself
        // was the problem rather than a genuinely missing translation.
        let strings = match serde_json::from_str(json) {
            Ok(strings) => strings,
            Err(err) => {
                warn!("failed to parse locale JSON: {err}");
                HashMap::new()
            }
        };
        Self { strings }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.strings.get(key).map(String::as_str)
    }

    pub fn language_name(&self) -> Option<&str> {
        self.get("_language_name")
    }
}

/// Looks up `key` in `active`, falling back to `fallback`, then the raw
/// key itself if missing everywhere (so a missed translation is visually
/// obvious rather than silently blank) - mirrors `i18n._()`. `args` are
/// `{name}`-style placeholders substituted after lookup: Python leans on
/// `str.format(**kwargs)` for this, done here with a plain string replace
/// per pair instead of pulling in a format-string parser for the handful
/// of templated keys that need it (e.g. `"Page {n}"`).
pub fn translate(active: &Catalog, fallback: &Catalog, key: &str, args: &[(&str, &str)]) -> String {
    let text = active.get(key).or_else(|| fallback.get(key)).unwrap_or(key);
    let mut result = text.to_string();
    for (name, value) in args {
        result = result.replace(&format!("{{{name}}}"), value);
    }
    result
}

/// One `(code, display name)` pair per `locales/*.json` file found, sorted
/// by code - mirrors `i18n.available_languages()`. A file that fails to
/// parse is skipped rather than aborting discovery of the others (same
/// resilience principle used for widget/page state loading).
pub fn discover_languages(locales_dir: &Path) -> Vec<(String, String)> {
    // Missing entirely means *zero* languages show up in the settings
    // picker - previously indistinguishable from a locales dir that's
    // just empty, which shouldn't happen but would look identical.
    let Ok(entries) = std::fs::read_dir(locales_dir) else {
        warn!("failed to read locales directory {}", locales_dir.display());
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    paths.sort();

    paths
        .into_iter()
        .filter_map(|path| {
            let code = path.file_stem()?.to_str()?.to_string();
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(err) => {
                    warn!("failed to read locale file {}: {err}, skipping", path.display());
                    return None;
                }
            };
            let catalog = Catalog::from_json_str(&text);
            let name = catalog.language_name().unwrap_or(&code).to_string();
            Some((code, name))
        })
        .collect()
}

pub fn load_catalog(locales_dir: &Path, code: &str) -> Catalog {
    let path = locales_dir.join(format!("{code}.json"));
    match std::fs::read_to_string(&path) {
        Ok(text) => Catalog::from_json_str(&text),
        Err(err) => {
            // The UI keeps running (every key just falls back to itself,
            // see `translate`) but this is *why* - worth more than
            // silence, especially for the fallback language, whose
            // catalog missing entirely would surface as literally every
            // untranslated string in the app.
            warn!("failed to read locale file {}: {err} - its strings will show as raw keys", path.display());
            Catalog::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_prefers_active_then_fallback_then_raw_key() {
        let active = Catalog::from_json_str(r#"{"a": "Active A"}"#);
        let fallback = Catalog::from_json_str(r#"{"a": "Fallback A", "b": "Fallback B"}"#);

        assert_eq!(translate(&active, &fallback, "a", &[]), "Active A");
        assert_eq!(translate(&active, &fallback, "b", &[]), "Fallback B");
        assert_eq!(translate(&active, &fallback, "missing", &[]), "missing");
    }

    #[test]
    fn translate_substitutes_named_placeholders() {
        let active = Catalog::from_json_str(r#"{"greeting": "Page {n}"}"#);
        let fallback = Catalog::default();
        assert_eq!(translate(&active, &fallback, "greeting", &[("n", "3")]), "Page 3");
    }

    #[test]
    fn discover_languages_reads_language_name_from_each_file() {
        let dir = std::env::temp_dir().join(format!("xeneon-test-locales-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fr.json"), r#"{"_language_name": "Français"}"#).unwrap();
        std::fs::write(dir.join("en.json"), r#"{"_language_name": "English"}"#).unwrap();

        let mut langs = discover_languages(&dir);
        langs.sort();
        assert_eq!(langs, vec![("en".to_string(), "English".to_string()), ("fr".to_string(), "Français".to_string())]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
