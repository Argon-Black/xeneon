import json

from xeneon_dashboard.config import CONFIG_DIR

PAGES_DIR = CONFIG_DIR / "pages"


def load_all() -> list[dict]:
    """Every persisted page state, one per file in PAGES_DIR (see save()),
    ordered by page_index. A corrupted file is skipped rather than aborting
    the whole load - same convention as widget_store.load_all()."""
    if not PAGES_DIR.is_dir():
        return []
    states = []
    for path in sorted(PAGES_DIR.glob("*.json")):
        try:
            data = json.loads(path.read_text())
        except (json.JSONDecodeError, OSError):
            continue
        data["id"] = path.stem
        states.append(data)
    states.sort(key=lambda s: s.get("page_index", 0))
    return states


def save(page_id: str, data: dict) -> None:
    PAGES_DIR.mkdir(parents=True, exist_ok=True)
    (PAGES_DIR / f"{page_id}.json").write_text(json.dumps(data, indent=2))


def delete(page_id: str) -> None:
    path = PAGES_DIR / f"{page_id}.json"
    if path.exists():
        path.unlink()
