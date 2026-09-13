"""Raccourcis widget: a launcher grid (up to 5x5 icons) for installed apps
and free-form command/URL shortcuts, iCUE-style. Icons are added via a
right-click on an empty cell, launched by a plain click, reordered via a
dedicated move button (bottom-left corner overlay), deleted via a
dedicated delete button (top-right corner overlay), and - custom shortcuts
only - edited via a right-click on the icon. Both corner buttons are
revealed on hover, mirroring DashboardWidget's own corner buttons for a
whole widget (grid.py) - see ShortcutsContent below for the gesture wiring.

A widget page sits inside an Adw.Carousel (see window.py), which recognizes
a horizontal drag anywhere on the page as "swipe to the next/previous
page" - including a drag that starts on top of an icon here. Reordering
therefore can't just be a plain drag on the tile itself, or every attempt
to swipe pages while the pointer happens to be over this widget would
instead (or also) reorder an icon - mirroring exactly why DashboardWidget
(grid.py) moved off its own full-width header strip onto a dedicated move
button. A dedicated button has no such ambiguity to resolve: any press on
it unambiguously means "move this icon", so its gesture claims the pointer
sequence immediately (Gtk.Gesture.set_state(CLAIMED) - GTK denies that
same sequence to every other gesture in the hierarchy once one claims it,
including the carousel's own swipe recognizer), and the tile's own click
handling goes back to being a plain click, free to ignore anything that
moves the pointer away before release (which is exactly what a swipe
starting on an icon looks like).

While a move is in progress, the icon doesn't visually follow the pointer
- only the currently-hovered cell gets a highlight (see
_show_drop_highlight), and the icon jumps straight there on release. There
is deliberately no trajectory to track: the real pointer can't be confined
to this widget (that GDK4 API doesn't exist portably), so any approach
that has to track a continuous path is at the mercy of a pointer that
wanders past the grid's edges - even onto another monitor - with no way to
tell it to stop.

The backdrop panel behind the icons is NOT the generic per-widget appearance
(card background/border/opacity - see widget_appearance.py, still available
through the gear icon for the outer card). It is drawn by this content
itself and sized to the bounding box of the icons actually placed, so it
grows/shrinks with how full the grid is, like the reference layout."""

import os

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("Gdk", "4.0")
gi.require_version("Gio", "2.0")
from gi.repository import Adw, Gdk, Gio, GLib, Gtk

from xeneon_dashboard import i18n
from xeneon_dashboard.grid import GAP
from xeneon_dashboard.widget_appearance import IMAGE_MIME_TYPES

GRID_COLS = 5
GRID_ROWS = 5

ICON_PIXEL_SIZE = 64
# Smaller than CORNER_BUTTON_ICON_PIXEL_SIZE (grid.py) - a whole widget's
# move button sits in a card with room to spare, but a shortcut tile is
# tiny by comparison, so its own move button has to stay proportionate.
ICON_MOVE_BUTTON_PIXEL_SIZE = 16
# Bigger than a tile's own corner buttons, since it's the main way to add
# anything to an otherwise-empty grid - but it's a small hover overlay near
# the top-left corner (see ShortcutsContent.__init__), not sized to fill
# reserved layout space the way a tile's own corner buttons are.
ADD_BUTTON_PIXEL_SIZE = 20
DEFAULT_ICON_NAME_COMMAND = "application-x-executable-symbolic"
DEFAULT_ICON_NAME_URL = "web-browser-symbolic"

DEFAULT_BACKDROP_HEX = "#7e57c2"
DEFAULT_BACKDROP_ALPHA = 0.55
BACKDROP_RADIUS_PX = 12

# One shared provider for both the static tile-hover rule and every
# instance's own backdrop color (keyed by its unique css class, like
# WidgetAppearance in widget_appearance.py) - a dict so restyling one
# instance's backdrop doesn't clobber another's or the static rule.
_provider = Gtk.CssProvider()
_installed = False
_rules: dict[str, str] = {
    "_tile": (
        ".xeneon-shortcut-tile { border-radius: 10px; padding: 4px; }"
        ".xeneon-shortcut-tile:hover { background-color: rgba(255, 255, 255, 0.12); }"
        # Feedback while the move button's drag is in progress for this tile.
        ".xeneon-shortcut-tile-moving { background-color: rgba(255, 255, 255, 0.22); }"
        # The move/delete corner buttons: small and unobtrusive, matching
        # the tiny tile they sit in rather than grid.py's full-size corner
        # buttons.
        "button.xeneon-shortcut-corner {"
        " min-width: 24px; min-height: 24px; padding: 2px; margin: 2px;"
        " }"
        "button.xeneon-shortcuts-add {"
        " min-width: 36px; min-height: 36px; padding: 4px; margin: 2px;"
        " }"
        # The currently-hovered target cell while a move is in progress (see
        # ShortcutsContent._show_drop_highlight) - green-ish border for a
        # free cell (or the icon's own), red for one that's already taken.
        ".xeneon-shortcuts-drop-valid {"
        " border: 2px solid rgba(255, 255, 255, 0.85); border-radius: 10px;"
        " background-color: rgba(255, 255, 255, 0.08);"
        " }"
        ".xeneon-shortcuts-drop-invalid {"
        " border: 2px solid rgba(231, 76, 60, 0.9); border-radius: 10px;"
        " background-color: rgba(231, 76, 60, 0.15);"
        " }"
    ),
}


def _ensure_css_installed():
    global _installed
    if _installed:
        return
    Gtk.StyleContext.add_provider_for_display(
        Gdk.Display.get_default(), _provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )
    _installed = True


def _reload_css():
    _provider.load_from_string("\n".join(rule for rule in _rules.values() if rule))


def _rgba_to_css(rgba: Gdk.RGBA, alpha: float | None = None) -> str:
    r, g, b = (round(c * 255) for c in (rgba.red, rgba.green, rgba.blue))
    a = rgba.alpha if alpha is None else alpha
    return f"rgba({r}, {g}, {b}, {a:.2f})"


def _rgba_to_hex(rgba: Gdk.RGBA) -> str:
    r, g, b = (round(c * 255) for c in (rgba.red, rgba.green, rgba.blue))
    return f"#{r:02x}{g:02x}{b:02x}"


def _hex_to_rgba(hex_str: str) -> Gdk.RGBA:
    rgba = Gdk.RGBA()
    rgba.parse(hex_str)
    return rgba


def _default_backdrop_rgba() -> Gdk.RGBA:
    return _hex_to_rgba(DEFAULT_BACKDROP_HEX)


def _is_url(text: str) -> bool:
    return "://" in text


def _launch_uri(uri: str):
    # Goes through the OpenURI portal automatically (Gio picks the portal
    # backend when sandboxed) - reaches the host browser/handler either way,
    # unlike _launch_command/_launch_app below, so this needs no host-access
    # opt-in.
    try:
        Gio.AppInfo.launch_default_for_uri(uri, None)
    except GLib.Error:
        pass


def _in_flatpak_sandbox() -> bool:
    return os.path.exists("/.flatpak-info")


def _host_access_enabled() -> bool:
    # Reads the same setting settings_page.py's host-access switch writes
    # (see config.py's "shortcuts_host_access" and app.py's
    # set_shortcuts_host_access) - looked up through the app singleton
    # rather than threaded through every ShortcutIcon/ShortcutsContent, since
    # launching is triggered straight from a tile's click gesture with no
    # other path back to app config.
    app = Gio.Application.get_default()
    return bool(app is not None and app.config.get("shortcuts_host_access"))


def _launch_command(command: str):
    # Run through a shell (not argv-split directly) so pipes/args/env the
    # user types work exactly like a normal shell command - same tradeoff
    # GNOME's own custom-keybinding commands make; this is the user's own
    # local input, not untrusted data.
    argv = ["/bin/sh", "-c", command]
    if _in_flatpak_sandbox() and _host_access_enabled():
        # Without this, /bin/sh only ever sees the sandbox's own tiny
        # filesystem/binaries - a command like a launcher script or a
        # notify-send meant for the real desktop would silently do nothing
        # useful. flatpak-spawn --host needs --talk-name=org.freedesktop.
        # Flatpak (see the manifest) and, since that's equivalent to
        # stepping outside the sandbox entirely, is only ever used once the
        # user has explicitly opted in via the host-access setting (see
        # settings_page.py's confirmation dialog).
        argv = ["flatpak-spawn", "--host", *argv]
    try:
        Gio.Subprocess.new(argv, Gio.SubprocessFlags.NONE)
    except GLib.Error:
        pass


def _desktop_app_info(app_id: str) -> Gio.DesktopAppInfo | None:
    # Gio.DesktopAppInfo.new() raises TypeError ("constructor returned
    # NULL"), not just None, for an id with no matching .desktop file (e.g.
    # an app uninstalled since the shortcut was saved) - PyGObject turning a
    # NULL-returning constructor into an exception rather than None.
    try:
        return Gio.DesktopAppInfo.new(app_id)
    except TypeError:
        return None


def _launch_app(app_id: str):
    app_info = _desktop_app_info(app_id)
    if app_info is None:
        return
    if _in_flatpak_sandbox() and _host_access_enabled():
        # app_info.launch() below would try to exec the app inside the
        # sandbox, where it isn't installed - route through flatpak-spawn
        # instead, same opt-in host-access path as _launch_command. Needs
        # the app's real .desktop file visible at this same path inside the
        # sandbox (see the manifest's read-only host applications-directory
        # permissions, also gated behind the host-access setting) so
        # flatpak-spawn's host-side `gio launch` can find it.
        desktop_path = app_info.get_filename()
        if desktop_path:
            try:
                Gio.Subprocess.new(
                    ["flatpak-spawn", "--host", "gio", "launch", desktop_path], Gio.SubprocessFlags.NONE
                )
            except GLib.Error:
                pass
            return
    try:
        app_info.launch([], None)
    except GLib.Error:
        pass


class ShortcutIcon:
    """One grid tile: either an installed app (`app_id`, a .desktop file id
    resolved through Gio.DesktopAppInfo) or a custom shortcut (`command`, a
    shell command or a URL - see _is_url). `icon_path`, when set, overrides
    whatever icon the app/command would otherwise resolve to."""

    def __init__(self, col, row, *, kind, app_id=None, command=None, label="", icon_name=None, icon_path=None):
        self.col = col
        self.row = row
        self.kind = kind  # "app" or "custom"
        self.app_id = app_id
        self.command = command
        self.label = label
        self.icon_name = icon_name
        self.icon_path = icon_path

    def display_name(self) -> str:
        if self.label:
            return self.label
        if self.kind == "app" and self.app_id:
            app_info = _desktop_app_info(self.app_id)
            if app_info is not None:
                return app_info.get_display_name() or app_info.get_name() or self.app_id
            return self.app_id
        return self.command or ""

    def gicon(self) -> Gio.Icon | None:
        if self.icon_path:
            return Gio.FileIcon.new(Gio.File.new_for_path(self.icon_path))
        if self.kind == "app" and self.app_id:
            app_info = _desktop_app_info(self.app_id)
            if app_info is not None and app_info.get_icon() is not None:
                return app_info.get_icon()
        if self.icon_name:
            return Gio.ThemedIcon.new(self.icon_name)
        default = DEFAULT_ICON_NAME_URL if self.command and _is_url(self.command) else DEFAULT_ICON_NAME_COMMAND
        return Gio.ThemedIcon.new(default)

    def launch(self):
        if self.kind == "app" and self.app_id:
            _launch_app(self.app_id)
        elif self.command:
            (_launch_uri if _is_url(self.command) else _launch_command)(self.command)

    def to_dict(self) -> dict:
        return {
            "col": self.col,
            "row": self.row,
            "kind": self.kind,
            "app_id": self.app_id,
            "command": self.command,
            "label": self.label,
            "icon_name": self.icon_name,
            "icon_path": self.icon_path,
        }

    @classmethod
    def from_dict(cls, data: dict) -> "ShortcutIcon | None":
        col, row, kind = data.get("col"), data.get("row"), data.get("kind")
        if col is None or row is None or kind not in ("app", "custom"):
            return None
        return cls(
            col,
            row,
            kind=kind,
            app_id=data.get("app_id"),
            command=data.get("command"),
            label=data.get("label", ""),
            icon_name=data.get("icon_name"),
            icon_path=data.get("icon_path"),
        )


class _IconTile(Gtk.Overlay):
    """A tile's visible content: the icon, centered in the cell (name as a
    tooltip, not on-tile text - see refresh()), plus a move button
    (bottom-left) and a delete button (top-right), both overlaid and
    revealed on hover - the same corner-button pattern DashboardWidget uses
    for a whole widget (grid.py), just scaled down to fit a tiny tile. A
    plain click anywhere else on the tile launches it; only the move
    button's own drag actually repositions the icon (see
    ShortcutsContent._wire_tile_gestures and the module docstring for why
    moving isn't just "drag the tile")."""

    def __init__(self, icon: ShortcutIcon):
        super().__init__()
        self.icon = icon
        # Absolute content-local point where the move button's press
        # started (its own position at press time, plus where inside it
        # was clicked) - added to the drag's offset on each update to get
        # the pointer's current absolute position, which is all _cell_at
        # needs to know which cell is currently hovered.
        self.press_x = 0.0
        self.press_y = 0.0
        self.hover_col = icon.col
        self.hover_row = icon.row
        self.add_css_class("xeneon-shortcut-tile")
        self.set_cursor_from_name("pointer")

        self._image = Gtk.Image()
        self._image.set_pixel_size(ICON_PIXEL_SIZE)
        self._image.set_halign(Gtk.Align.CENTER)
        self._image.set_valign(Gtk.Align.CENTER)
        self._image.set_hexpand(True)
        self._image.set_vexpand(True)
        self.set_child(self._image)

        self.move_button = Gtk.Button()
        self.move_button.add_css_class("flat")
        self.move_button.add_css_class("circular")
        self.move_button.add_css_class("xeneon-shortcut-corner")
        move_icon = Gtk.Image.new_from_icon_name("list-drag-handle-symbolic")
        move_icon.set_pixel_size(ICON_MOVE_BUTTON_PIXEL_SIZE)
        self.move_button.set_child(move_icon)
        self.move_button.set_halign(Gtk.Align.START)
        self.move_button.set_valign(Gtk.Align.END)
        self.move_button.set_cursor_from_name("move")
        self.move_button.set_visible(False)
        self.add_overlay(self.move_button)

        self.delete_button = Gtk.Button()
        self.delete_button.add_css_class("flat")
        self.delete_button.add_css_class("circular")
        self.delete_button.add_css_class("xeneon-shortcut-corner")
        delete_icon = Gtk.Image.new_from_icon_name("user-trash-symbolic")
        delete_icon.set_pixel_size(ICON_MOVE_BUTTON_PIXEL_SIZE)
        self.delete_button.set_child(delete_icon)
        self.delete_button.set_halign(Gtk.Align.END)
        self.delete_button.set_valign(Gtk.Align.START)
        self.delete_button.set_tooltip_text(i18n._("widgets.delete_tooltip"))
        self.delete_button.set_visible(False)
        self.add_overlay(self.delete_button)

        hover = Gtk.EventControllerMotion()
        hover.connect("enter", lambda *_a: self._set_corner_buttons_visible(True))
        hover.connect("leave", lambda *_a: self._set_corner_buttons_visible(False))
        self.add_controller(hover)

        i18n.on_change(self._retranslate)
        self.refresh()

    def _set_corner_buttons_visible(self, visible: bool):
        self.move_button.set_visible(visible)
        self.delete_button.set_visible(visible)

    def _retranslate(self):
        self.delete_button.set_tooltip_text(i18n._("widgets.delete_tooltip"))

    def refresh(self):
        gicon = self.icon.gicon()
        if gicon is not None:
            self._image.set_from_gicon(gicon)
        self.set_tooltip_text(self.icon.display_name())


class ShortcutsContent(Gtk.Fixed):
    """The launcher grid itself - see the module docstring for the overall
    behaviour. `size` is the widget's fixed S/M/L/SQ footprint (see
    grid.py); since widgets here never resize at runtime, cell geometry is
    computed once from it rather than tracked through allocation signals."""

    _next_id = 0

    def __init__(self, size: tuple[int, int]):
        super().__init__()
        _ensure_css_installed()
        self._size = size
        self._cell_w, self._cell_h = self._compute_cell_size(size)
        self._icons: dict[tuple[int, int], ShortcutIcon] = {}
        self._tiles: dict[tuple[int, int], _IconTile] = {}
        self._on_change_cb = lambda: None

        ShortcutsContent._next_id += 1
        self._backdrop_css_class = f"xeneon-shortcuts-backdrop-{ShortcutsContent._next_id}"
        self.backdrop_color = _default_backdrop_rgba()
        self.backdrop_opacity = DEFAULT_BACKDROP_ALPHA

        self._backdrop_segments: list[Gtk.Box] = []
        self._apply_backdrop_css()

        hint_w, hint_h = 320, 40
        self._empty_hint = Gtk.Label()
        self._empty_hint.add_css_class("dim-label")
        self._empty_hint.set_wrap(True)
        self._empty_hint.set_justify(Gtk.Justification.CENTER)
        self._empty_hint.set_size_request(hint_w, hint_h)
        self.put(self._empty_hint, (size[0] - hint_w) // 2, (size[1] - hint_h) // 2)

        # The currently-hovered target cell while an icon is being moved
        # (see _show_drop_highlight) - one reusable widget, repositioned
        # and hidden/shown rather than recreated, since only one tile can
        # be moved at a time.
        self._drop_highlight = Gtk.Box()
        self._drop_highlight.set_can_target(False)
        self._drop_highlight.set_visible(False)
        self._drop_highlight.set_size_request(int(self._cell_w), int(self._cell_h))

        # Top-left corner, in the generic per-widget title's usual spot
        # (see widget_picker.py's empty title_key for this widget) - hover-
        # revealed like every other overlay button (tile corners, the
        # widget's own delete/configure/move buttons in grid.py); the
        # empty-state hint text (see _retranslate) is what carries the
        # "click + at the top-left" discoverability instead. Floats over
        # row 0's own corner rather than reserving layout space for it (the
        # way TOP_CLEARANCE_PX used to, before the header it was working
        # around became non-interactive - see grid.py) - it's only ever
        # visible transiently, on hover, so a little overlap there is fine.
        self._add_button = Gtk.Button()
        self._add_button.add_css_class("flat")
        self._add_button.add_css_class("circular")
        self._add_button.add_css_class("xeneon-shortcuts-add")
        add_icon = Gtk.Image.new_from_icon_name("list-add-symbolic")
        add_icon.set_pixel_size(ADD_BUTTON_PIXEL_SIZE)
        self._add_button.set_child(add_icon)
        self._add_button.set_tooltip_text(i18n._("widgets.shortcuts.context.add"))
        self._add_button.set_visible(False)
        self._add_button.connect("clicked", self._on_add_button_clicked)
        self.put(self._add_button, GAP // 2, GAP // 2)

        hover = Gtk.EventControllerMotion()
        hover.connect("enter", lambda *_a: self._reveal_add_button())
        hover.connect("leave", lambda *_a: self._add_button.set_visible(False))
        self.add_controller(hover)

        self._retranslate()
        i18n.on_change(self._retranslate)

    def set_change_notifier(self, callback):
        """Wired by the spawner (widget_picker.py) to the same on_change
        used to persist the owning DashboardWidget - icon add/remove/move
        here happen straight on the canvas, outside the popover-close /
        widget-move hooks that otherwise trigger a save (see CLAUDE.md)."""
        self._on_change_cb = callback or (lambda: None)

    def _notify(self):
        self._on_change_cb()

    def _apply_backdrop_css(self):
        # Just the color here - corner rounding is per-row-segment (see
        # _update_backdrop) since adjacent rows can be seamed together.
        _rules[self._backdrop_css_class] = (
            f".{self._backdrop_css_class} {{"
            f" background-color: {_rgba_to_css(self.backdrop_color, self.backdrop_opacity)};"
            " }"
        )
        _reload_css()

    def set_backdrop_color(self, rgba: Gdk.RGBA):
        # Not wired through set_change_notifier/_notify: this setting lives
        # in the popover (see ShortcutsSettings), saved on its "closed"
        # signal like every other widget's settings - see grid.py. The
        # color's own alpha is ignored - backdrop_opacity is the single
        # source of transparency, same split as WidgetAppearance's own
        # bg_color/opacity in widget_appearance.py.
        self.backdrop_color = rgba
        self._apply_backdrop_css()

    def set_backdrop_opacity(self, opacity: float):
        self.backdrop_opacity = opacity
        self._apply_backdrop_css()

    def reset_backdrop(self):
        self.backdrop_color = _default_backdrop_rgba()
        self.backdrop_opacity = DEFAULT_BACKDROP_ALPHA
        self._apply_backdrop_css()

    def _compute_cell_size(self, size: tuple[int, int]) -> tuple[float, float]:
        w, h = size
        avail_w = w - 2 * GAP - (GRID_COLS - 1) * GAP
        avail_h = h - 2 * GAP - (GRID_ROWS - 1) * GAP
        return avail_w / GRID_COLS, avail_h / GRID_ROWS

    def _cell_x(self, col: int) -> float:
        return GAP + col * (self._cell_w + GAP)

    def _cell_y(self, row: int) -> float:
        return GAP + row * (self._cell_h + GAP)

    def _tile_position(self, col: int, row: int) -> tuple[int, int]:
        return int(self._cell_x(col)), int(self._cell_y(row))

    def _cell_at(self, x: float, y: float) -> tuple[int, int]:
        col = int((x - GAP) // (self._cell_w + GAP))
        row = int((y - GAP) // (self._cell_h + GAP))
        return max(0, min(GRID_COLS - 1, col)), max(0, min(GRID_ROWS - 1, row))

    def _update_backdrop(self):
        """One backdrop rectangle per row that has an icon, each spanning
        only from that row's own leftmost icon to its own rightmost one -
        not the whole bounding box, and not always from column 0 either:
        dragging an icon away from the left edge must not leave a panel
        covering empty columns to its left.

        Two such rectangles sit flush against each other (no gap) when
        their rows are adjacent, so each one only rounds the corners on a
        side that isn't touching another row's segment with overlapping
        columns - otherwise the touching edge would show as a pinched
        notch instead of one smooth panel spanning both rows."""
        for segment in self._backdrop_segments:
            self.remove(segment)
        self._backdrop_segments = []

        if not self._icons:
            self._empty_hint.set_visible(True)
            return
        self._empty_hint.set_visible(False)

        cols_by_row: dict[int, list[int]] = {}
        for col, row in self._icons:
            cols_by_row.setdefault(row, []).append(col)
        ranges = {row: (min(cols), max(cols)) for row, cols in cols_by_row.items()}

        def _adjoins(row: int, neighbor_row: int) -> bool:
            if neighbor_row not in ranges:
                return False
            min_col, max_col = ranges[row]
            n_min, n_max = ranges[neighbor_row]
            return min_col <= n_max and n_min <= max_col

        pad = GAP // 2
        for row, (min_col, max_col) in ranges.items():
            round_top = not _adjoins(row, row - 1)
            round_bottom = not _adjoins(row, row + 1)
            corner_class = f"{self._backdrop_css_class}-r{row}"
            top_radius = BACKDROP_RADIUS_PX if round_top else 0
            bottom_radius = BACKDROP_RADIUS_PX if round_bottom else 0
            _rules[corner_class] = (
                f".{corner_class} {{"
                f" border-top-left-radius: {top_radius}px;"
                f" border-top-right-radius: {top_radius}px;"
                f" border-bottom-left-radius: {bottom_radius}px;"
                f" border-bottom-right-radius: {bottom_radius}px;"
                " }"
            )

            segment = Gtk.Box()
            segment.add_css_class(self._backdrop_css_class)
            segment.add_css_class(corner_class)
            x0 = self._cell_x(min_col) - pad
            y0 = self._cell_y(row) - pad
            x1 = self._cell_x(max_col) + self._cell_w + pad
            y1 = self._cell_y(row) + self._cell_h + pad
            segment.set_size_request(int(x1 - x0), int(y1 - y0))
            self.put(segment, int(x0), int(y0))
            # New children land on top by default (last = painted last) -
            # move each segment to the very back so it never covers a tile.
            segment.insert_after(self, None)
            self._backdrop_segments.append(segment)
        _reload_css()

    def _add_tile(self, icon: ShortcutIcon) -> _IconTile:
        tile = _IconTile(icon)
        tile.set_size_request(int(self._cell_w), int(self._cell_h))
        self._icons[(icon.col, icon.row)] = icon
        self._tiles[(icon.col, icon.row)] = tile
        x, y = self._tile_position(icon.col, icon.row)
        self.put(tile, x, y)
        self._wire_tile_gestures(tile)
        self._update_backdrop()
        self._update_add_button_state()
        return tile

    def _wire_tile_gestures(self, tile: _IconTile):
        # A plain click launches - simple GestureClick, no drag distance to
        # watch, since moving is now the dedicated move button's job (see
        # the module docstring). GTK only fires "released" for a click that
        # both starts and ends inside the tile, so a swipe that starts on
        # an icon and moves away before releasing never triggers this -
        # nothing needs to claim or deny anything for that to hold.
        launch = Gtk.GestureClick()
        launch.connect("released", lambda *_a: tile.icon.launch())
        tile.add_controller(launch)

        secondary = Gtk.GestureClick()
        secondary.set_button(Gdk.BUTTON_SECONDARY)
        secondary.connect("pressed", self._on_tile_secondary_click, tile)
        tile.add_controller(secondary)

        move = Gtk.GestureDrag()
        move.connect("drag-begin", self._on_tile_move_begin, tile)
        move.connect("drag-update", self._on_tile_move_update, tile)
        move.connect("drag-end", self._on_tile_move_end, tile)
        tile.move_button.add_controller(move)

        tile.delete_button.connect("clicked", lambda _b: self._remove_icon(tile))

    def _on_tile_move_begin(self, gesture: Gtk.GestureDrag, x: float, y: float, tile: _IconTile):
        # Claims immediately - a dedicated button has no ambiguity to
        # resolve (unlike the tile itself, which can also be a plain
        # click, or, on the same surface, a page swipe), so there's
        # nothing to wait on. See the module docstring and
        # DashboardWidget._on_move_begin (grid.py) for the same reasoning
        # applied to moving a whole widget.
        gesture.set_state(Gtk.EventSequenceState.CLAIMED)
        tile.add_css_class("xeneon-shortcut-tile-moving")
        button_x, button_y = tile.move_button.translate_coordinates(self, x, y)
        tile.press_x, tile.press_y = button_x, button_y
        tile.hover_col, tile.hover_row = tile.icon.col, tile.icon.row
        self._show_drop_highlight(tile)

    def _on_tile_move_update(self, _gesture, offset_x: float, offset_y: float, tile: _IconTile):
        # No trajectory to track and nothing to clamp-and-remember: the
        # tile doesn't follow the pointer at all while being moved (see
        # the class docstring on _IconTile) - each update just asks "which
        # cell is the pointer over right now" from its current absolute
        # position, exactly like the hover-highlight demo this behaviour
        # is modeled on. A real pointer wandering past the grid's edges -
        # even onto another monitor - simply keeps reporting the boundary
        # cell; there's no accumulated state to walk back, so reversing
        # direction responds immediately no matter how far it wandered.
        x, y = tile.press_x + offset_x, tile.press_y + offset_y
        tile.hover_col, tile.hover_row = self._cell_at(x, y)
        self._show_drop_highlight(tile)

    def _show_drop_highlight(self, tile: _IconTile):
        col, row = tile.hover_col, tile.hover_row
        valid = (col, row) == (tile.icon.col, tile.icon.row) or (col, row) not in self._icons
        self._drop_highlight.set_visible(True)
        if self._drop_highlight.get_parent() is None:
            self.put(self._drop_highlight, *self._tile_position(col, row))
        else:
            self.move(self._drop_highlight, *self._tile_position(col, row))
        # Always on top - a highlight sitting behind a tile would be
        # invisible over an occupied cell, which is exactly the case that
        # most needs to read clearly (in red) as "can't drop here".
        self._drop_highlight.insert_before(self, None)
        if valid:
            self._drop_highlight.remove_css_class("xeneon-shortcuts-drop-invalid")
            self._drop_highlight.add_css_class("xeneon-shortcuts-drop-valid")
        else:
            self._drop_highlight.remove_css_class("xeneon-shortcuts-drop-valid")
            self._drop_highlight.add_css_class("xeneon-shortcuts-drop-invalid")

    def _on_tile_move_end(self, _gesture, _offset_x: float, _offset_y: float, tile: _IconTile):
        tile.remove_css_class("xeneon-shortcut-tile-moving")
        self._drop_highlight.set_visible(False)
        # The tile never moved from its own cell while being dragged (only
        # the highlight did) - landing on the same cell or an occupied one
        # just means there's nothing to undo visually.
        col, row = tile.hover_col, tile.hover_row
        if (col, row) == (tile.icon.col, tile.icon.row) or (col, row) in self._icons:
            return
        self._move_icon(tile, col, row)

    def _move_icon(self, tile: _IconTile, col: int, row: int):
        del self._icons[(tile.icon.col, tile.icon.row)]
        del self._tiles[(tile.icon.col, tile.icon.row)]
        tile.icon.col, tile.icon.row = col, row
        self._icons[(col, row)] = tile.icon
        self._tiles[(col, row)] = tile
        x, y = self._tile_position(col, row)
        self.move(tile, x, y)
        self._update_backdrop()
        self._notify()

    def _remove_icon(self, tile: _IconTile):
        key = (tile.icon.col, tile.icon.row)
        del self._icons[key]
        del self._tiles[key]
        self.remove(tile)
        self._update_backdrop()
        self._update_add_button_state()
        self._notify()

    def _popup_menu(self, parent: Gtk.Widget, x: float, y: float, entries: list[tuple[str, "callable"]]):
        popover = Gtk.Popover()
        popover.set_parent(parent)
        popover.set_has_arrow(False)
        rect = Gdk.Rectangle()
        rect.x, rect.y, rect.width, rect.height = int(x), int(y), 1, 1
        popover.set_pointing_to(rect)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        for label, callback in entries:
            button = Gtk.Button(label=label)
            button.add_css_class("flat")

            def _on_clicked(_button, _cb=callback):
                popover.popdown()
                _cb()

            button.connect("clicked", _on_clicked)
            box.append(button)
        popover.set_child(box)
        popover.connect("closed", lambda _p: GLib.idle_add(popover.unparent))
        popover.popup()

    def _first_free_cell(self) -> tuple[int, int] | None:
        """Top-left, filling left to right then wrapping to the next row -
        new icons always land here regardless of where the "Ajouter" click
        happened, like a launcher/dock auto-arranging its icons rather than
        free placement."""
        for row in range(GRID_ROWS):
            for col in range(GRID_COLS):
                if (col, row) not in self._icons:
                    return col, row
        return None

    def _reveal_add_button(self):
        self._add_button.set_visible(True)
        # It floats over row 0's own corner (see __init__) rather than
        # reserving space, so it has to be raised above whatever tile is
        # already sitting there to stay clickable and visible.
        self._add_button.insert_before(self, None)

    def _on_add_button_clicked(self, _button):
        target = self._first_free_cell()
        if target is None:
            return
        self._open_add_dialog(*target)

    def _update_add_button_state(self):
        self._add_button.set_sensitive(self._first_free_cell() is not None)

    def _on_tile_secondary_click(self, _gesture, _n_press, x: float, y: float, tile: _IconTile):
        # Deleting is the corner button's job now (see _IconTile) - only
        # custom shortcuts still have anything for a right-click to offer.
        if tile.icon.kind != "custom":
            return
        self._popup_menu(tile, x, y, [(i18n._("widgets.shortcuts.context.edit"), lambda: self._open_edit_dialog(tile))])

    def _open_add_dialog(self, col: int, row: int):
        window = self.get_root()
        dialog = ShortcutPickerDialog(window, on_pick=lambda icon: self._apply_add(col, row, icon))
        dialog.present()

    def _apply_add(self, col: int, row: int, icon: ShortcutIcon):
        if (col, row) in self._icons:
            return
        icon.col, icon.row = col, row
        self._add_tile(icon)
        self._notify()

    def _open_edit_dialog(self, tile: _IconTile):
        window = self.get_root()
        dialog = ShortcutPickerDialog(window, on_pick=lambda icon: self._apply_edit(tile, icon), initial=tile.icon)
        dialog.present()

    def _apply_edit(self, tile: _IconTile, new_icon: ShortcutIcon):
        new_icon.col, new_icon.row = tile.icon.col, tile.icon.row
        tile.icon = new_icon
        self._icons[(new_icon.col, new_icon.row)] = new_icon
        tile.refresh()
        self._notify()

    def _retranslate(self):
        self._empty_hint.set_label(i18n._("widgets.shortcuts.empty_hint"))
        self._add_button.set_tooltip_text(i18n._("widgets.shortcuts.context.add"))

    def to_dict(self) -> dict:
        return {
            "icons": [icon.to_dict() for icon in self._icons.values()],
            "backdrop_color": _rgba_to_hex(self.backdrop_color),
            "backdrop_opacity": self.backdrop_opacity,
        }

    def apply_dict(self, data: dict) -> None:
        if not data:
            return
        for entry in data.get("icons", []):
            icon = ShortcutIcon.from_dict(entry)
            if icon is None:
                continue
            if not (0 <= icon.col < GRID_COLS and 0 <= icon.row < GRID_ROWS):
                continue
            if (icon.col, icon.row) in self._icons:
                continue
            self._add_tile(icon)
        if "backdrop_color" in data:
            self.set_backdrop_color(_hex_to_rgba(data["backdrop_color"]))
        if "backdrop_opacity" in data:
            self.set_backdrop_opacity(data["backdrop_opacity"])


class ShortcutPickerDialog(Adw.Window):
    """Modal opened from a right-click on the grid: pick an installed app,
    or fill in a custom shortcut (name, command/URL, optional icon image).
    With `initial` set (editing an existing custom shortcut, see
    ShortcutsContent._open_edit_dialog) the app picker is hidden and the
    form is pre-filled instead. Either way `on_pick(ShortcutIcon)` is called
    with col/row left at (0, 0) - the caller places it."""

    def __init__(self, parent: Gtk.Window | None, *, on_pick, initial: ShortcutIcon | None = None):
        super().__init__(transient_for=parent, modal=True)
        self._on_pick = on_pick
        self._initial = initial
        self._custom_icon_path: str | None = initial.icon_path if initial else None
        self.set_default_size(420, 540)

        toolbar_view = Adw.ToolbarView()
        toolbar_view.add_top_bar(Adw.HeaderBar())

        root = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        root.set_margin_start(12)
        root.set_margin_end(12)
        root.set_margin_top(6)
        root.set_margin_bottom(12)

        self._search_entry = Gtk.SearchEntry()
        self._search_entry.connect("search-changed", self._on_search_changed)
        root.append(self._search_entry)

        self._apps_group_label = Gtk.Label(halign=Gtk.Align.START)
        self._apps_group_label.add_css_class("heading")
        root.append(self._apps_group_label)

        scroller = Gtk.ScrolledWindow()
        scroller.set_vexpand(True)
        scroller.set_min_content_height(200)
        self._apps_list = Gtk.ListBox()
        self._apps_list.add_css_class("boxed-list")
        self._apps_list.set_selection_mode(Gtk.SelectionMode.NONE)
        self._apps_list.set_activate_on_single_click(True)
        self._apps_list.connect("row-activated", self._on_app_row_activated)
        scroller.set_child(self._apps_list)
        root.append(scroller)

        self._separator = Gtk.Separator()
        root.append(self._separator)

        self._custom_group_label = Gtk.Label(halign=Gtk.Align.START)
        self._custom_group_label.add_css_class("heading")
        root.append(self._custom_group_label)

        form_list = Gtk.ListBox()
        form_list.add_css_class("boxed-list")
        form_list.set_selection_mode(Gtk.SelectionMode.NONE)
        self._name_row = Adw.EntryRow()
        form_list.append(self._name_row)
        self._command_row = Adw.EntryRow()
        form_list.append(self._command_row)
        root.append(form_list)

        icon_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        self._icon_preview = Gtk.Image()
        self._icon_preview.set_pixel_size(32)
        icon_row.append(self._icon_preview)
        self._icon_choose_button = Gtk.Button()
        self._icon_choose_button.connect("clicked", self._on_choose_icon)
        icon_row.append(self._icon_choose_button)
        self._icon_clear_button = Gtk.Button()
        self._icon_clear_button.connect("clicked", self._on_clear_icon)
        icon_row.append(self._icon_clear_button)
        root.append(icon_row)

        self._add_custom_button = Gtk.Button()
        self._add_custom_button.add_css_class("suggested-action")
        self._add_custom_button.set_halign(Gtk.Align.END)
        self._add_custom_button.connect("clicked", self._on_add_custom_clicked)
        root.append(self._add_custom_button)

        toolbar_view.set_content(root)
        self.set_content(toolbar_view)

        if initial is not None:
            self._search_entry.set_visible(False)
            self._apps_group_label.set_visible(False)
            scroller.set_visible(False)
            self._separator.set_visible(False)
            self._name_row.set_text(initial.label)
            self._command_row.set_text(initial.command or "")
            if initial.icon_path:
                self._icon_preview.set_from_file(initial.icon_path)
        else:
            self._all_apps = [app for app in Gio.AppInfo.get_all() if app.should_show()]
            self._all_apps.sort(key=lambda a: (a.get_display_name() or "").lower())
            self._populate_apps("")

        self._retranslate()
        i18n.on_change(self._retranslate)

    def _populate_apps(self, query: str):
        child = self._apps_list.get_first_child()
        while child is not None:
            next_child = child.get_next_sibling()
            self._apps_list.remove(child)
            child = next_child
        query = query.strip().lower()
        for app in self._all_apps:
            name = app.get_display_name() or app.get_name() or ""
            if query and query not in name.lower():
                continue
            row = Adw.ActionRow(title=GLib.markup_escape_text(name), activatable=True)
            icon = app.get_icon()
            if icon is not None:
                image = Gtk.Image.new_from_gicon(icon)
                image.set_pixel_size(28)
                row.add_prefix(image)
            row.app_info = app
            self._apps_list.append(row)

    def _on_search_changed(self, entry: Gtk.SearchEntry):
        self._populate_apps(entry.get_text())

    def _on_app_row_activated(self, _listbox, row):
        app_info = getattr(row, "app_info", None)
        if app_info is None:
            return
        self._on_pick(ShortcutIcon(0, 0, kind="app", app_id=app_info.get_id()))
        self.close()

    def _on_choose_icon(self, _button):
        dialog = Gtk.FileDialog()
        image_filter = Gtk.FileFilter()
        image_filter.set_name(i18n._("widgets.appearance.bg_image_filter"))
        for mime_type in IMAGE_MIME_TYPES:
            image_filter.add_mime_type(mime_type)
        filters = Gio.ListStore.new(Gtk.FileFilter)
        filters.append(image_filter)
        dialog.set_filters(filters)
        dialog.open(self, None, self._on_icon_chosen)

    def _on_icon_chosen(self, dialog, result):
        try:
            file = dialog.open_finish(result)
        except GLib.Error:
            return
        if file is not None:
            self._custom_icon_path = file.get_path()
            self._icon_preview.set_from_file(self._custom_icon_path)

    def _on_clear_icon(self, _button):
        self._custom_icon_path = None
        self._icon_preview.clear()

    def _on_add_custom_clicked(self, _button):
        name = self._name_row.get_text().strip()
        command = self._command_row.get_text().strip()
        if not command:
            return
        icon = ShortcutIcon(0, 0, kind="custom", command=command, label=name, icon_path=self._custom_icon_path)
        self._on_pick(icon)
        self.close()

    def _retranslate(self):
        self.set_title(i18n._("widgets.shortcuts.edit.title" if self._initial else "widgets.shortcuts.picker.title"))
        self._search_entry.set_placeholder_text(i18n._("widgets.shortcuts.picker.search_placeholder"))
        self._apps_group_label.set_label(i18n._("widgets.shortcuts.picker.apps_group"))
        self._custom_group_label.set_label(i18n._("widgets.shortcuts.picker.custom_group"))
        self._name_row.set_title(i18n._("widgets.shortcuts.picker.name_label"))
        self._command_row.set_title(i18n._("widgets.shortcuts.picker.command_label"))
        self._icon_choose_button.set_label(i18n._("widgets.shortcuts.picker.icon_choose"))
        self._icon_clear_button.set_label(i18n._("widgets.shortcuts.picker.icon_clear"))
        self._add_custom_button.set_label(
            i18n._("widgets.shortcuts.picker.save_button" if self._initial else "widgets.shortcuts.picker.add_button")
        )


class ShortcutsSettings(Gtk.Box):
    """The shortcuts grid's own setting, shown next to the generic
    appearance controls in the configure popover (see DashboardWidget in
    grid.py): the backdrop color that grows with the icons (see
    ShortcutsContent), separate from the generic per-widget card
    background/border already covered by WidgetAppearance."""

    def __init__(self, content: ShortcutsContent):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self._content = content
        self.set_size_request(220, -1)

        self._color_label = Gtk.Label()
        self._color_label.set_halign(Gtk.Align.START)
        self.append(self._color_label)
        self._color_button = Gtk.ColorDialogButton.new(Gtk.ColorDialog.new())
        self._color_button.set_rgba(content.backdrop_color)
        self._color_button.connect("notify::rgba", self._on_color_changed)
        self.append(self._color_button)

        self._opacity_label = Gtk.Label()
        self._opacity_label.set_halign(Gtk.Align.START)
        self.append(self._opacity_label)
        self._opacity_scale = Gtk.Scale(orientation=Gtk.Orientation.HORIZONTAL)
        self._opacity_scale.set_range(0, 100)
        self._opacity_scale.set_value(content.backdrop_opacity * 100)
        self._opacity_scale.set_draw_value(True)
        self._opacity_scale.set_value_pos(Gtk.PositionType.RIGHT)
        self._opacity_scale.connect("value-changed", self._on_opacity_changed)
        self.append(self._opacity_scale)

        self._retranslate()
        i18n.on_change(self._retranslate)

    def sync_from_content(self):
        """Re-reads the controls from self._content - needed after
        content.reset_backdrop() changes it directly, mirroring
        ClockSettings.sync_from_content() in widgets/clock.py."""
        self._color_button.set_rgba(self._content.backdrop_color)
        self._opacity_scale.set_value(self._content.backdrop_opacity * 100)

    def _on_color_changed(self, button, _pspec):
        self._content.set_backdrop_color(button.get_rgba())

    def _on_opacity_changed(self, scale):
        self._content.set_backdrop_opacity(scale.get_value() / 100)

    def _retranslate(self):
        self._color_label.set_label(i18n._("widgets.shortcuts.settings.backdrop_color"))
        self._opacity_label.set_label(i18n._("widgets.shortcuts.settings.backdrop_opacity"))
