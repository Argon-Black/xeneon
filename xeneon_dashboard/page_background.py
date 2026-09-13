import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, Gio, Gtk

from xeneon_dashboard.widget_appearance import DEFAULT_BG_HEX, hex_to_rgba, _rgba_to_css, rgba_to_hex

_provider = Gtk.CssProvider()
_installed = False
_rules: dict[str, str] = {}


def _ensure_installed():
    global _installed
    if _installed:
        return
    Gtk.StyleContext.add_provider_for_display(
        Gdk.Display.get_default(), _provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION + 1
    )
    _installed = True


def _reload():
    _provider.load_from_string("\n".join(rule for rule in _rules.values() if rule))


class PageBackground:
    """A dashboard page's own background: a solid color and/or an image,
    rendered as CSS scoped to a unique class added to that page - mirrors
    WidgetAppearance in widget_appearance.py, but only the background half
    of it (pages don't have a border/corner setting). Untouched (the
    default) means no CSS override at all, so a page keeps the app's plain
    background until its color or image is actually set."""

    _next_id = 0

    def __init__(self, page: Gtk.Widget):
        PageBackground._next_id += 1
        self.css_class = f"xeneon-page-bg-{PageBackground._next_id}"
        page.add_css_class(self.css_class)

        self.color = hex_to_rgba(DEFAULT_BG_HEX)
        self.image_path: str | None = None
        self._touched = False

        _ensure_installed()
        self._apply()

    def set_color(self, rgba: Gdk.RGBA):
        self.color = rgba
        self._touched = True
        self._apply()

    def set_image(self, path: str | None):
        self.image_path = path
        self._touched = True
        self._apply()

    def reset(self):
        self.color = hex_to_rgba(DEFAULT_BG_HEX)
        self.image_path = None
        self._touched = False
        self._apply()

    def _apply(self):
        rules = []
        if self._touched:
            rules.append(f"background-color: {_rgba_to_css(self.color)};")
            if self.image_path:
                uri = Gio.File.new_for_path(self.image_path).get_uri()
                rules.append(f"background-image: url('{uri}');")
                rules.append("background-size: cover;")
                rules.append("background-position: center;")
        body = " ".join(rules)
        _rules[self.css_class] = f".{self.css_class} {{ {body} }}" if body else ""
        _reload()

    def to_dict(self) -> dict:
        return {
            "touched": self._touched,
            "color": rgba_to_hex(self.color),
            "image_path": self.image_path,
        }

    def apply_dict(self, data: dict) -> None:
        """Restores a state previously returned by to_dict(). Only touches
        fields actually present, so a partial/older dict still applies
        cleanly - see WidgetAppearance.apply_dict for the same convention."""
        if not data:
            return
        if "color" in data:
            self.color = hex_to_rgba(data["color"])
        if "image_path" in data:
            self.image_path = data["image_path"]
        self._touched = bool(data.get("touched", False))
        self._apply()
