import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gtk


class Placeholder(Gtk.Box):
    """Stand-in content for a widget slot, until the real widget exists."""

    def __init__(self, text: str):
        super().__init__()
        self.set_halign(Gtk.Align.CENTER)
        self.set_valign(Gtk.Align.CENTER)
        self._label = Gtk.Label(label=text)
        self._label.add_css_class("dim-label")
        self._label.set_wrap(True)
        self._label.set_justify(Gtk.Justification.CENTER)
        self.append(self._label)

    def set_text(self, text: str):
        self._label.set_label(text)


def placeholder(text: str) -> Placeholder:
    return Placeholder(text)
