import json
import logging

from xeneon_dashboard.config import CONFIG_DIR

logger = logging.getLogger(__name__)

WIDGETS_DIR = CONFIG_DIR / "widgets"


def load_all() -> list[dict]:
    """Every persisted widget state, one per file in WIDGETS_DIR (see save()),
    ordered by page then position so callers can rebuild pages deterministically.
    A corrupted file is skipped rather than aborting the whole load."""
    if not WIDGETS_DIR.is_dir():
        return []
    states = []
    for path in sorted(WIDGETS_DIR.glob("*.json")):
        try:
            data = json.loads(path.read_text())
        except (json.JSONDecodeError, OSError):
            logger.warning("Widget illisible ignoré: %s", path, exc_info=True)
            continue
        data["id"] = path.stem
        states.append(data)
    states.sort(key=lambda s: (s.get("page_index", 0), s.get("y", 0), s.get("x", 0)))
    return states


def save(widget_id: str, data: dict) -> None:
    WIDGETS_DIR.mkdir(parents=True, exist_ok=True)
    (WIDGETS_DIR / f"{widget_id}.json").write_text(json.dumps(data, indent=2))


def delete(widget_id: str) -> None:
    path = WIDGETS_DIR / f"{widget_id}.json"
    if path.exists():
        path.unlink()
