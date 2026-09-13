"""App-wide accent color - the one theme knob exposed in Réglages > Thème,
independent of each widget's own appearance (background/border/opacity,
see widget_appearance.py) or per-widget colors like the agenda's weekend
color or the shortcuts backdrop. Used for chrome that isn't "content":
right now that's the widget picker's own selection/hover accents (see
widget_picker.py) - other modules can adopt the same `@accent_color` named
color as more chrome grows to use it.

Implemented as a single GTK named color (`@define-color accent_color ...`)
on one shared CssProvider, rather than the _rules-dict-per-module pattern
used by WidgetAppearance/AudioContent/etc: those rebuild *their own*
stylesheet text per instance because they hold instance-specific values
(one widget's chosen bg color). The accent is a single global value, and
GTK already resolves a named color display-wide across every CssProvider
attached to the same Gdk.Display, resolved lazily at style computation
time rather than at parse time - so any other stylesheet can reference
`@accent_color` once, in a provider installed independently, and it keeps
resolving correctly across every future apply_accent() call without that
other provider ever needing to reload."""

import logging

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("Gdk", "4.0")
from gi.repository import Adw, Gdk, Gtk

logger = logging.getLogger(__name__)

# (preset key, hex) offered in the settings picker - each reuses a color
# already present somewhere else in the app, so picking the default
# ("iris") doesn't introduce a shade nothing else on screen has. "iris"
# matches widgets/shortcuts.py's DEFAULT_BACKDROP_HEX.
ACCENT_PRESETS = [
    ("iris", "#7e57c2"),
    ("glacier", "#3584e4"),
    ("sarcelle", "#26a269"),
    ("ambre", "#ff9f43"),
]
DEFAULT_ACCENT_HEX = ACCENT_PRESETS[0][1]

SWATCH_CSS_CLASS = "xeneon-accent-swatch"
SWATCH_SELECTED_CSS_CLASS = "xeneon-accent-swatch-selected"

_provider = Gtk.CssProvider()
_installed = False
_accent_hex = DEFAULT_ACCENT_HEX
_listeners: list = []
_style_manager_signal_connected = False

_SWATCH_STATIC_CSS = (
    f".{SWATCH_CSS_CLASS} {{ min-width: 22px; min-height: 22px; padding: 0; border-radius: 999px;"
    " border: 2px solid transparent; }\n"
    + "\n".join(f".{SWATCH_CSS_CLASS}-{key} {{ background-color: {hex_value}; }}" for key, hex_value in ACCENT_PRESETS)
    + f"\n.{SWATCH_SELECTED_CSS_CLASS} {{ border-color: #ffffff; }}"
)


def _ensure_installed():
    global _installed
    if _installed:
        return
    Gtk.StyleContext.add_provider_for_display(
        Gdk.Display.get_default(), _provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )
    _installed = True


def install_css(accent_hex: str = DEFAULT_ACCENT_HEX):
    _ensure_installed()
    apply_accent(accent_hex)


def apply_accent(accent_hex: str) -> None:
    """Sets the live accent color and reloads the one shared provider -
    every other stylesheet referencing `@accent_color` (see the module
    docstring) picks it up immediately without needing to reload itself."""
    global _accent_hex
    _ensure_installed()
    _accent_hex = accent_hex
    _provider.load_from_string(f"{_SWATCH_STATIC_CSS}\n@define-color accent_color {accent_hex};")
    for callback in list(_listeners):
        callback()


def get_accent() -> str:
    return _accent_hex


def on_change(callback) -> None:
    _listeners.append(callback)


def system_accent_hex() -> str | None:
    """The desktop's own accent color (GNOME Réglages > Couleurs), or None
    if this desktop doesn't report one at all - some environments/older
    GNOME versions don't, so "suivre l'accent système" has nothing to
    follow there and the caller should just leave the current accent as
    is (see app.py's _apply_system_accent_if_available)."""
    style_manager = Adw.StyleManager.get_default()
    if not style_manager.get_system_supports_accent_colors():
        return None
    rgba = style_manager.get_accent_color_rgba()
    r, g, b = (round(component * 255) for component in (rgba.red, rgba.green, rgba.blue))
    return f"#{r:02x}{g:02x}{b:02x}"


def system_accent_supported() -> bool:
    return Adw.StyleManager.get_default().get_system_supports_accent_colors()


def connect_system_accent_changed(callback) -> None:
    """Lets app.py re-apply the system accent whenever GNOME's own pick
    changes while "follow system" is enabled. Connected once regardless of
    how many times this is called (app.py only calls it once at startup
    anyway, but this guards against ever doing it twice)."""
    global _style_manager_signal_connected
    if _style_manager_signal_connected:
        return
    Adw.StyleManager.get_default().connect("notify::accent-color", lambda *_a: callback())
    _style_manager_signal_connected = True
