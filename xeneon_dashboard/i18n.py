"""Minimal JSON-based translation layer.

Each language is one file in locales/<code>.json — a flat {key: text}
mapping, plus a "_language_name" entry naming the language in its own
tongue (used to build the language picker). Dropping in a new file such as
locales/de.json is enough to add a language everywhere it's listed; no code
changes needed.

Translating text is a plain lookup: `_("settings.title")`. Widgets that
must update live when the user switches language register a callback via
`on_change()`.
"""

import json
from pathlib import Path

LOCALES_DIR = Path(__file__).parent / "locales"
DEFAULT_LANGUAGE = "fr"
FALLBACK_LANGUAGE = "fr"

_current_lang = DEFAULT_LANGUAGE
_strings: dict[str, str] = {}
_fallback_strings: dict[str, str] = {}
_listeners: list = []


def _load(lang: str) -> dict:
    path = LOCALES_DIR / f"{lang}.json"
    if not path.exists():
        return {}
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return {}


def available_languages() -> dict[str, str]:
    """code -> display name (in that language), one entry per locales/*.json file."""
    languages = {}
    if LOCALES_DIR.exists():
        for path in sorted(LOCALES_DIR.glob("*.json")):
            languages[path.stem] = _load(path.stem).get("_language_name", path.stem)
    return languages


def init(lang: str | None = None) -> None:
    global _fallback_strings
    _fallback_strings = _load(FALLBACK_LANGUAGE)
    set_language(lang or DEFAULT_LANGUAGE, notify=False)


def set_language(lang: str, notify: bool = True) -> None:
    global _current_lang, _strings
    _current_lang = lang
    _strings = _load(lang)
    if notify:
        for callback in list(_listeners):
            callback()


def get_language() -> str:
    return _current_lang


def on_change(callback) -> None:
    """Register a no-arg callback fired whenever the active language changes."""
    _listeners.append(callback)


def _(key: str, **kwargs) -> str:
    text = _strings.get(key) or _fallback_strings.get(key) or key
    return text.format(**kwargs) if kwargs else text
