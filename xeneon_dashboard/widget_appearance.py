import logging

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, Gio, GLib, Gtk

from xeneon_dashboard import i18n

logger = logging.getLogger(__name__)

DEFAULT_BG_HEX = "#242424"
DEFAULT_BORDER_HEX = "#ffffff"
ROUNDED_RADIUS_PX = 12
IMAGE_MIME_TYPES = ("image/png", "image/jpeg", "image/webp", "image/bmp", "image/gif", "image/tiff", "image/svg+xml")

_provider = Gtk.CssProvider()
_installed = False
_rules: dict[str, str] = {
    # A punchy-but-not-neon red, bold white text - the theme's own
    # destructive-action style rendered too muted (dark red on dark red)
    # to read as a clear "this resets things" button, so this spells it
    # out directly instead of relying on the theme to get it right.
    "_reset_button": (
        ".xeneon-reset-button { background-color: #d5303f; color: #ffffff; font-weight: bold; }"
        ".xeneon-reset-button:hover { background-color: #c02836; }"
    ),
}


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


def _rgba_to_css(rgba: Gdk.RGBA, alpha: float | None = None) -> str:
    r, g, b = (round(c * 255) for c in (rgba.red, rgba.green, rgba.blue))
    a = rgba.alpha if alpha is None else alpha
    return f"rgba({r}, {g}, {b}, {a:.2f})"


def rgba_to_hex(rgba: Gdk.RGBA) -> str:
    r, g, b = (round(c * 255) for c in (rgba.red, rgba.green, rgba.blue))
    return f"#{r:02x}{g:02x}{b:02x}"


def hex_to_rgba(hex_str: str) -> Gdk.RGBA:
    rgba = Gdk.RGBA()
    rgba.parse(hex_str)
    return rgba


def defaults_touched_dict(defaults: dict) -> dict:
    """Turns a `default_widget_appearance` config dict (see config.py,
    settings_page.py's default-appearance group) into an apply_dict()-shaped
    payload with "bg"/"border" marked touched - without that, apply_dict
    would just store the values without rendering them, since an untouched
    widget deliberately stays the theme's plain .card look (see
    WidgetAppearance's own docstring)."""
    return {
        "touched": ["bg", "border"],
        "opacity": defaults["opacity"],
        "bg_color": defaults["bg_color"],
        "border_enabled": defaults["border_enabled"],
        "border_width": defaults["border_width"],
        "border_color": defaults["border_color"],
    }


class WidgetAppearance:
    """Live-editable visual settings for one DashboardWidget's card:
    background color/opacity/image and border. Renders as CSS scoped to a
    unique class added to that card. Nothing is overridden until a setting
    is actually touched, so an untouched widget keeps the theme's plain
    .card look."""

    _next_id = 0

    def __init__(self, card: Gtk.Widget):
        WidgetAppearance._next_id += 1
        self.css_class = f"xeneon-appearance-{WidgetAppearance._next_id}"
        card.add_css_class(self.css_class)

        self.opacity = 1.0
        self.bg_color = Gdk.RGBA()
        self.bg_color.parse(DEFAULT_BG_HEX)
        self.bg_image_path: str | None = None
        self.border_enabled = False
        self.border_width = 2
        self.border_color = Gdk.RGBA()
        self.border_color.parse(DEFAULT_BORDER_HEX)
        self.rounded = True
        self._touched: set[str] = set()

        _ensure_installed()
        self._apply()

    def set_opacity(self, opacity: float):
        self.opacity = opacity
        self._touched.add("bg")
        self._apply()

    def set_bg_color(self, rgba: Gdk.RGBA):
        self.bg_color = rgba
        self._touched.add("bg")
        self._apply()

    def set_bg_image(self, path: str | None):
        self.bg_image_path = path
        self._touched.add("bg")
        self._apply()

    def set_border_enabled(self, enabled: bool):
        self.border_enabled = enabled
        self._touched.add("border")
        self._apply()

    def set_border_width(self, width: int):
        self.border_width = width
        self._touched.add("border")
        self._apply()

    def set_border_color(self, rgba: Gdk.RGBA):
        self.border_color = rgba
        self._touched.add("border")
        self._apply()

    def set_rounded(self, rounded: bool):
        self.rounded = rounded
        self._touched.add("corner")
        self._apply()

    def reset(self):
        """Back to an untouched widget's plain .card look - see the class
        docstring on why that means clearing _touched rather than just
        setting fields back to their constructor defaults."""
        self.opacity = 1.0
        self.bg_color = hex_to_rgba(DEFAULT_BG_HEX)
        self.bg_image_path = None
        self.border_enabled = False
        self.border_width = 2
        self.border_color = hex_to_rgba(DEFAULT_BORDER_HEX)
        self.rounded = True
        self._touched.clear()
        self._apply()

    def _apply(self):
        rules = []
        if "bg" in self._touched:
            rules.append(f"background-color: {_rgba_to_css(self.bg_color, self.opacity)};")
            if self.bg_image_path:
                uri = Gio.File.new_for_path(self.bg_image_path).get_uri()
                rules.append(f"background-image: url('{uri}');")
                rules.append("background-size: cover;")
                rules.append("background-position: center;")
        if "border" in self._touched:
            if self.border_enabled:
                rules.append(f"border: {self.border_width}px solid {_rgba_to_css(self.border_color)};")
            else:
                rules.append("border: none;")
        if "corner" in self._touched:
            rules.append(f"border-radius: {ROUNDED_RADIUS_PX if self.rounded else 0}px;")
        body = " ".join(rules)
        _rules[self.css_class] = f".{self.css_class} {{ {body} }}" if body else ""
        _reload()

    def has_customizations(self) -> bool:
        """Whether anything (a spawner's own look, a restored per-widget
        save, or a forced default) has already touched this appearance -
        used to decide whether the global default-appearance setting should
        still apply, since it must never stomp something more specific."""
        return bool(self._touched)

    def to_dict(self) -> dict:
        return {
            "touched": sorted(self._touched),
            "opacity": self.opacity,
            "bg_color": rgba_to_hex(self.bg_color),
            "bg_image_path": self.bg_image_path,
            "border_enabled": self.border_enabled,
            "border_width": self.border_width,
            "border_color": rgba_to_hex(self.border_color),
            "rounded": self.rounded,
        }

    def apply_dict(self, data: dict) -> None:
        """Restores a state previously returned by to_dict(). Only touches
        fields actually present, so a partial/older dict still applies
        cleanly."""
        if not data:
            return
        if "opacity" in data:
            self.opacity = data["opacity"]
        if "bg_color" in data:
            self.bg_color = hex_to_rgba(data["bg_color"])
        if "bg_image_path" in data:
            self.bg_image_path = data["bg_image_path"]
        if "border_enabled" in data:
            self.border_enabled = data["border_enabled"]
        if "border_width" in data:
            self.border_width = data["border_width"]
        if "border_color" in data:
            self.border_color = hex_to_rgba(data["border_color"])
        if "rounded" in data:
            self.rounded = data["rounded"]
        self._touched.update(data.get("touched", []))
        self._apply()


class AppearancePopover(Gtk.Popover):
    """The configure button's popup: the appearance controls every widget
    gets for free (background transparency/color/image, border, corners) on
    the left, plus - when a plugin passes one - that plugin's own settings
    on the right, separated by a vertical bar. Side by side rather than
    stacked so adding plugin-specific settings widens the popover instead of
    making it taller, since a taller popover is more likely to need
    repositioning over (and visually colliding with) another widget lower
    on the page."""

    def __init__(self, appearance: WidgetAppearance, extra_settings: Gtk.Widget | None = None, on_reset=None):
        super().__init__()
        self._appearance = appearance
        self._on_reset = on_reset
        _ensure_installed()
        _reload()

        # The popover is parented to the small configure button in the
        # card's corner, not the whole card. Popping to the RIGHT keeps it
        # off the card's own body so live-editing stays visible instead of
        # being hidden under the popover. autohide is off because GTK's
        # autohide popover closes itself the instant a sub-dialog (the
        # color or font chooser) opens and takes the grab, before the user
        # can pick anything - the explicit close button below is how it's
        # meant to be dismissed instead.
        self.set_autohide(False)
        self.set_position(Gtk.PositionType.RIGHT)

        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
        outer.set_margin_top(6)
        outer.set_margin_bottom(12)
        outer.set_margin_start(12)
        outer.set_margin_end(12)

        self._close_button = Gtk.Button()
        self._close_button.add_css_class("flat")
        self._close_button.add_css_class("circular")
        self._close_button.set_icon_name("window-close-symbolic")
        self._close_button.set_halign(Gtk.Align.END)
        self._close_button.connect("clicked", lambda _b: self.popdown())
        outer.append(self._close_button)

        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        box.set_size_request(240, -1)

        self._opacity_label = Gtk.Label()
        self._opacity_label.set_halign(Gtk.Align.START)
        box.append(self._opacity_label)
        self._opacity_scale = Gtk.Scale(orientation=Gtk.Orientation.HORIZONTAL)
        self._opacity_scale.set_range(0, 100)
        self._opacity_scale.set_value(appearance.opacity * 100)
        self._opacity_scale.set_draw_value(True)
        self._opacity_scale.set_value_pos(Gtk.PositionType.RIGHT)
        self._opacity_scale.connect("value-changed", self._on_opacity_changed)
        box.append(self._opacity_scale)

        self._bg_color_label, self._bg_color_button = self._add_color_row(box, appearance.bg_color)
        self._bg_color_button.connect("notify::rgba", self._on_bg_color_changed)

        image_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        self._image_button = Gtk.Button()
        self._image_button.connect("clicked", self._on_choose_image)
        image_row.append(self._image_button)
        self._image_clear_button = Gtk.Button()
        self._image_clear_button.connect("clicked", self._on_clear_image)
        image_row.append(self._image_clear_button)
        box.append(image_row)

        box.append(Gtk.Separator())

        border_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        self._border_label = Gtk.Label()
        self._border_label.set_hexpand(True)
        self._border_label.set_halign(Gtk.Align.START)
        border_row.append(self._border_label)
        self._border_switch = Gtk.Switch()
        self._border_switch.set_active(appearance.border_enabled)
        self._border_switch.set_valign(Gtk.Align.CENTER)
        self._border_switch.connect("notify::active", self._on_border_toggled)
        border_row.append(self._border_switch)
        box.append(border_row)

        self._border_width_label = Gtk.Label()
        self._border_width_label.set_halign(Gtk.Align.START)
        box.append(self._border_width_label)
        self._border_width_spin = Gtk.SpinButton.new_with_range(1, 12, 1)
        self._border_width_spin.set_value(appearance.border_width)
        self._border_width_spin.connect("value-changed", self._on_border_width_changed)
        box.append(self._border_width_spin)

        self._border_color_label, self._border_color_button = self._add_color_row(box, appearance.border_color)
        self._border_color_button.connect("notify::rgba", self._on_border_color_changed)

        box.append(Gtk.Separator())

        self._corner_label = Gtk.Label()
        self._corner_label.set_halign(Gtk.Align.START)
        box.append(self._corner_label)
        corner_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        self._round_button = Gtk.ToggleButton()
        self._square_button = Gtk.ToggleButton()
        self._square_button.set_group(self._round_button)
        self._round_button.set_active(appearance.rounded)
        self._square_button.set_active(not appearance.rounded)
        self._round_button.connect("toggled", self._on_corner_toggled)
        corner_row.append(self._round_button)
        corner_row.append(self._square_button)
        box.append(corner_row)

        box.append(Gtk.Separator())

        self._reset_button = Gtk.Button()
        self._reset_button.add_css_class("xeneon-reset-button")
        self._reset_button.set_halign(Gtk.Align.CENTER)
        self._reset_button.set_margin_top(6)
        self._reset_button.connect("clicked", self._on_reset_clicked)
        box.append(self._reset_button)

        if extra_settings is None:
            outer.append(box)
        else:
            root = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=12)
            root.append(box)
            root.append(Gtk.Separator(orientation=Gtk.Orientation.VERTICAL))
            root.append(extra_settings)
            outer.append(root)

        self.set_child(outer)
        self._retranslate()
        i18n.on_change(self._retranslate)

    def _add_color_row(self, box: Gtk.Box, initial: Gdk.RGBA) -> tuple[Gtk.Label, "Gtk.ColorDialogButton"]:
        row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        label = Gtk.Label()
        label.set_hexpand(True)
        label.set_halign(Gtk.Align.START)
        row.append(label)
        button = Gtk.ColorDialogButton.new(Gtk.ColorDialog.new())
        button.set_rgba(initial)
        row.append(button)
        box.append(row)
        return label, button

    def _retranslate(self):
        self._reset_button.set_label(i18n._("widgets.appearance.reset"))
        self._close_button.set_tooltip_text(i18n._("widgets.appearance.close_tooltip"))
        self._opacity_label.set_label(i18n._("widgets.appearance.opacity"))
        self._bg_color_label.set_label(i18n._("widgets.appearance.bg_color"))
        self._image_button.set_label(i18n._("widgets.appearance.bg_image_choose"))
        self._image_clear_button.set_label(i18n._("widgets.appearance.bg_image_clear"))
        self._border_label.set_label(i18n._("widgets.appearance.border_enabled"))
        self._border_width_label.set_label(i18n._("widgets.appearance.border_width"))
        self._border_color_label.set_label(i18n._("widgets.appearance.border_color"))
        self._corner_label.set_label(i18n._("widgets.appearance.corner"))
        self._round_button.set_label(i18n._("widgets.appearance.corner_round"))
        self._square_button.set_label(i18n._("widgets.appearance.corner_square"))

    def _on_reset_clicked(self, _button):
        self._appearance.reset()
        self._sync_controls()
        if self._on_reset is not None:
            self._on_reset()

    def _sync_controls(self):
        """Re-reads every control's displayed value from self._appearance -
        needed after reset() changes the model directly, since the controls
        otherwise only push edits one-way and don't notice a programmatic
        change underneath them."""
        appearance = self._appearance
        self._opacity_scale.set_value(appearance.opacity * 100)
        self._bg_color_button.set_rgba(appearance.bg_color)
        self._border_switch.set_active(appearance.border_enabled)
        self._border_width_spin.set_value(appearance.border_width)
        self._border_color_button.set_rgba(appearance.border_color)
        self._round_button.set_active(appearance.rounded)
        self._square_button.set_active(not appearance.rounded)

    def _on_opacity_changed(self, scale):
        self._appearance.set_opacity(scale.get_value() / 100)

    def _on_bg_color_changed(self, button, _pspec):
        self._appearance.set_bg_color(button.get_rgba())

    def _on_choose_image(self, _button):
        dialog = Gtk.FileDialog()
        image_filter = Gtk.FileFilter()
        image_filter.set_name(i18n._("widgets.appearance.bg_image_filter"))
        for mime_type in IMAGE_MIME_TYPES:
            image_filter.add_mime_type(mime_type)
        filters = Gio.ListStore.new(Gtk.FileFilter)
        filters.append(image_filter)
        dialog.set_filters(filters)
        dialog.open(self.get_root(), None, self._on_image_chosen)

    def _on_image_chosen(self, dialog, result):
        try:
            file = dialog.open_finish(result)
        except GLib.Error as exc:
            # DISMISSED just means the user closed the picker without
            # choosing anything - not a failure worth logging.
            if not exc.matches(Gtk.DialogError.quark(), Gtk.DialogError.DISMISSED):
                logger.warning("Échec du sélecteur d'image de fond: %s", exc.message)
            return
        if file is not None:
            self._appearance.set_bg_image(file.get_path())

    def _on_clear_image(self, _button):
        self._appearance.set_bg_image(None)

    def _on_border_toggled(self, switch, _pspec):
        self._appearance.set_border_enabled(switch.get_active())

    def _on_border_width_changed(self, spin):
        self._appearance.set_border_width(int(spin.get_value()))

    def _on_border_color_changed(self, button, _pspec):
        self._appearance.set_border_color(button.get_rgba())

    def _on_corner_toggled(self, button):
        self._appearance.set_rounded(button.get_active())
