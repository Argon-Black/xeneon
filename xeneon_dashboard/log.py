"""Logging setup for the app - init() is called once from XeneonApp.__init__
(before anything else runs, so even config-loading issues get captured),
same pattern as config.load()/i18n.init(). Everywhere else just does the
stdlib-standard `logging.getLogger(__name__)`, which gives free-form
categorisation along the existing module layout (xeneon_dashboard.window,
xeneon_dashboard.widgets.weather, ...) without a taxonomy to maintain."""

import logging
import logging.handlers
import os
from pathlib import Path

STATE_DIR = Path(os.environ.get("XDG_STATE_HOME", str(Path.home() / ".local" / "state"))) / "xeneon-dashboard"
LOG_FILE = STATE_DIR / "xeneon-dashboard.log"

_initialized = False


def init(level: int = logging.INFO) -> None:
    global _initialized
    if _initialized:
        return
    _initialized = True

    formatter = logging.Formatter("%(asctime)s %(levelname)-8s %(name)s: %(message)s", datefmt="%Y-%m-%d %H:%M:%S")

    console_handler = logging.StreamHandler()
    console_handler.setFormatter(formatter)

    handlers = [console_handler]
    try:
        STATE_DIR.mkdir(parents=True, exist_ok=True)
        # 1 MB x 3 backups - plenty for a diagnostic trail across many runs
        # without growing unbounded.
        file_handler = logging.handlers.RotatingFileHandler(LOG_FILE, maxBytes=1_000_000, backupCount=3, encoding="utf-8")
        file_handler.setFormatter(formatter)
        handlers.append(file_handler)
    except OSError:
        # No writable state dir - still log to the console rather than
        # crashing the app over logging itself.
        pass

    root = logging.getLogger("xeneon_dashboard")
    root.setLevel(level)
    for handler in handlers:
        root.addHandler(handler)
