import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, GLib, Gtk

from xeneon_dashboard import i18n

# One i18n label key per grid size preset - shown big and centered so the
# preset is identifiable at a glance while tuning the grid layout.
_LABEL_KEYS = {
    "S": "widgets.dummy.label_s",
    "M": "widgets.dummy.label_m",
    "L": "widgets.dummy.label_l",
    "SQ": "widgets.dummy.label_sq",
    "SX": "widgets.dummy.label_sx",
    "SSX": "widgets.dummy.label_ssx",
}

# One background color per preset, so sizes are told apart at a glance
# without reading the label - applied to the card via DashboardWidget's own
# appearance system (see color_for_size()), not hardcoded into the content.
_COLOR_HEX = {
    "S": "#993C1D",
    "M": "#0F6E56",
    "L": "#3C3489",
    "SQ": "#72243E",
    "SX": "#854F0B",
    "SSX": "#3B6D11",
}


def color_for_size(size_code: str) -> Gdk.RGBA:
    rgba = Gdk.RGBA()
    rgba.parse(_COLOR_HEX[size_code])
    return rgba


class DummyContent(Gtk.Box):
    """Bare-bones content for previewing a grid size preset: just the size
    code (S/M/L/SQ/SX/SSX), large and centered. No plugin-specific settings
    - relies entirely on the generic appearance popover every widget gets
    for free."""

    def __init__(self, size_code: str):
        super().__init__()
        self._size_code = size_code
        self.set_halign(Gtk.Align.CENTER)
        self.set_valign(Gtk.Align.CENTER)
        self._label = Gtk.Label()
        self.append(self._label)
        self._refresh()
        i18n.on_change(self._refresh)

    def _refresh(self):
        text = i18n._(_LABEL_KEYS[self._size_code])
        markup = f'<span size="300%" foreground="#ffffff">{GLib.markup_escape_text(text)}</span>'
        self._label.set_markup(markup)
