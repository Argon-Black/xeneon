import json
import logging
import os
from pathlib import Path

logger = logging.getLogger(__name__)

CONFIG_DIR = Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config"))) / "xeneon-dashboard"
CONFIG_FILE = CONFIG_DIR / "config.json"

DEFAULTS = {
    # Hint only, offered to the portal's own binding dialog as a suggestion.
    "fullscreen_shortcut_hint": "<Super>f",
    # Set once the portal grants a session; lets later launches restore it
    # silently (no binding dialog shown again).
    "fullscreen_shortcut_restore_token": None,
    # Human-readable description of whatever the user actually bound,
    # reported by the portal - for display only, not used to trigger it.
    "fullscreen_shortcut_bound_trigger": None,
    "indicator_hide_delay_seconds": 2,
    # Inactive-button baseline opacity, percent - the active (current) page
    # button always stays fully opaque regardless, so it's still clear which
    # page you're on even at a low setting here.
    "indicator_opacity": 55,
    # Hex string, or None to just follow the theme's own foreground color.
    "indicator_button_color": None,
    "language": "fr",
    # Off by default: with this app packaged as a Flatpak, the Raccourcis
    # widget's custom shell commands and app launches otherwise only reach
    # the sandbox, not the host (see widgets/shortcuts.py's _in_flatpak_
    # sandbox()/_host_access_enabled()). Turning this on routes them through
    # flatpak-spawn --host instead - full host access, so the settings page
    # confirms with the user once before flipping it (see settings_page.py's
    # _on_shortcuts_host_access_toggled). Meaningless (and unused) outside a
    # Flatpak sandbox, where commands/apps already run on the host directly.
    "shortcuts_host_access": False,
    # App-wide accent color (see theme.py) - the picker's own selection/
    # hover chrome, independent of any widget's own appearance. Defaults to
    # theme.DEFAULT_ACCENT_HEX, duplicated here rather than imported so
    # config.py stays free of the gi/Gtk dependency (same reasoning as
    # default_widget_appearance below).
    "accent_color": "#7e57c2",
    # When true, accent_color above is ignored and the accent instead
    # follows the desktop's own accent color (GNOME Réglages > Couleurs) -
    # see theme.system_accent_hex() / app.py's set_accent_follow_system().
    "accent_follow_system": False,
    # Applied to every newly-added widget that doesn't already set its own
    # look (e.g. a demo/dummy widget's per-size color) - see
    # widget_appearance.defaults_touched_dict() and window.py's add_widget().
    # Values mirror widget_appearance.DEFAULT_BG_HEX/DEFAULT_BORDER_HEX and
    # WidgetAppearance's own constructor defaults, duplicated here rather
    # than imported so config.py stays free of the gi/Gtk dependency.
    "default_widget_appearance": {
        "opacity": 1.0,
        "bg_color": "#242424",
        "border_enabled": False,
        "border_width": 2,
        "border_color": "#ffffff",
    },
}


def load() -> dict:
    if CONFIG_FILE.exists():
        try:
            data = json.loads(CONFIG_FILE.read_text())
            return {**DEFAULTS, **data}
        except (json.JSONDecodeError, OSError):
            logger.warning("Config illisible (%s), retour aux valeurs par défaut", CONFIG_FILE, exc_info=True)
    return dict(DEFAULTS)


def save(config: dict) -> None:
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    CONFIG_FILE.write_text(json.dumps(config, indent=2))
