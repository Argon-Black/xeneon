import uuid

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, GLib, Gtk

from xeneon_dashboard import i18n
from xeneon_dashboard.page_background import PageBackground
from xeneon_dashboard.widget_appearance import AppearancePopover, WidgetAppearance

# How long the configure button stays revealed after a tap, on touch (no
# hover events to hide it on "leave" like the mouse gets).
TOUCH_REVEAL_SECONDS = 3

# Delete/configure buttons render an oversized icon glyph (not just a bigger
# button - see xeneon-widget-configure/-delete in page_indicator.py's CSS)
# so they're comfortably tappable with a finger, not just a mouse pointer.
CORNER_BUTTON_ICON_PIXEL_SIZE = 32

# S/SX/SSX all share the same short height (see SIZE_S/SX/SSX below) - too
# short for three corner buttons at the normal icon size without crowding
# each other, so DashboardWidget.__init__ switches to this smaller size
# whenever a widget's own height is that short. M/L/SQ (taller) keep
# CORNER_BUTTON_ICON_PIXEL_SIZE untouched.
SMALL_CORNER_BUTTON_ICON_PIXEL_SIZE = 18

# Space between two widgets, and between a widget and the page edge - kept
# identical on purpose so the layout reads as one consistent rhythm. See
# window.py's PAGE_MARGIN, which reuses this same value for the page edge.
GAP = 16

# Fixed widget footprints a plugin picks from - there is no free resize.
# Tuned to exactly tile the Xeneon Edge panel (2560x720, display scale set
# to 100% in GNOME Settings so logical pixels == physical pixels - see
# window.py) in 3 columns, with GAP between columns and PAGE_MARGIN (==
# GAP) at each screen edge:
#   3 * 832 + 2 * GAP + 2 * GAP = 2560   (3 columns wide)
# Heights follow an iCUE-style S/M/L progression, picked so a stack always
# lines up flush with the bottom of a single L widget - no leftover gap:
#   L = 688                        (= 720 - 2*GAP, fills the column height)
#   2*M + GAP = L  ->  M = 336
#   6*S + 5*GAP = L ->  S = 101    (2px short of a perfect fit - negligible)
SIZE_S = (832, 101)
SIZE_M = (832, 336)
SIZE_L = (832, 688)

# "carré" - two side by side, with a GAP between them like everywhere else
# in the grid, tile one M's footprint exactly: 2 * 408 + GAP = 832 =
# SIZE_M's width; same height as M. Six of them (2 per column) tile the
# full screen width, two rows of them its height.
SIZE_SQ = (408, 336)

# "SX" - S cut in half the same way SQ cuts M: two side by side, with a
# GAP between them, tile one S's footprint exactly: 2 * 408 + GAP = 832 =
# SIZE_S's width; same height as S.
SIZE_SX = (408, 101)

# "SSX" - SX cut in half again, same principle: two side by side, with a
# GAP between them, tile one SX's footprint exactly: 2 * 196 + GAP = 408 =
# SIZE_SX's width; same height as SX (and S).
SIZE_SSX = (196, 101)

# A page's interior size (inside PAGE_MARGIN, see window.py) - used as a
# fallback by find_free_position() when it runs on a page GTK hasn't
# allocated yet (e.g. one just created to receive an overflowing widget),
# where get_width()/get_height() would otherwise read back 0.
PAGE_W = 3 * SIZE_S[0] + 2 * GAP
PAGE_H = SIZE_L[1]


def _overlaps(a: tuple[int, int, int, int], b: tuple[int, int, int, int]) -> bool:
    ax, ay, aw, ah = a
    bx, by, bw, bh = b
    return ax < bx + bw and bx < ax + aw and ay < by + bh and by < ay + ah


def _column_positions(page_width: int, width: int) -> list[int]:
    """Every x a `width`-wide widget could start at while tiling flush from
    the left edge, GAP between each - the 3 full-width (S/M/L) columns, or
    the 6 half-width (SQ) columns, derived from the page's own width rather
    than hardcoded, so this doesn't silently go stale if PAGE_W or a SIZE_*
    ever changes. Used to snap a widget being dragged onto one of these
    instead of an arbitrary pixel offset - see WidgetGrid._snap_to_layout."""
    step = width + GAP
    positions = []
    x = 0
    while x + width <= page_width:
        positions.append(x)
        x += step
    return positions or [0]


def _nearest(value: int, candidates: list[int]) -> int:
    return min(candidates, key=lambda c: abs(c - value))


def _button_icon(icon_name: str, pixel_size: int = CORNER_BUTTON_ICON_PIXEL_SIZE) -> Gtk.Image:
    image = Gtk.Image.new_from_icon_name(icon_name)
    image.set_pixel_size(pixel_size)
    return image


# Move-preview outline shown on the page while a widget's move button is
# held (see WidgetGrid.begin/update/end_move_preview) - green-ish while the
# hovered spot is free, red once it would overlap another widget. One
# shared provider/pair of classes for every page, since only one preview
# can ever be visible at a time.
_MOVE_PREVIEW_VALID_CLASS = "xeneon-move-preview-valid"
_MOVE_PREVIEW_INVALID_CLASS = "xeneon-move-preview-invalid"
_move_preview_provider = Gtk.CssProvider()
_move_preview_css_installed = False


def _ensure_move_preview_css():
    global _move_preview_css_installed
    if _move_preview_css_installed:
        return
    _move_preview_provider.load_from_string(
        f".{_MOVE_PREVIEW_VALID_CLASS} {{"
        " border: 2px solid rgba(255, 255, 255, 0.85); border-radius: 8px;"
        " background-color: rgba(255, 255, 255, 0.08);"
        " }"
        f".{_MOVE_PREVIEW_INVALID_CLASS} {{"
        " border: 2px solid rgba(231, 76, 60, 0.9); border-radius: 8px;"
        " background-color: rgba(231, 76, 60, 0.15);"
        " }"
    )
    Gtk.StyleContext.add_provider_for_display(
        Gdk.Display.get_default(), _move_preview_provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )
    _move_preview_css_installed = True


class DashboardWidget(Gtk.Overlay):
    """A movable card hosting one dashboard widget's content, at a fixed
    S/M/L/SQ footprint (see SIZE_S/SIZE_M/SIZE_L/SIZE_SQ). Hovering it (or,
    on touch, tapping it) reveals two buttons: a delete button in the
    top-right corner (removes the widget from its page on click) and a
    configure button in the bottom-right corner, opening the appearance
    settings every widget gets for free: background transparency/color/
    image, border, corner shape. A plugin can pass its own `settings`
    widget, shown alongside those in the same popover."""

    def __init__(
        self,
        title_key: str,
        content: Gtk.Widget,
        x: int,
        y: int,
        size: tuple[int, int],
        settings: Gtk.Widget | None = None,
        *,
        kind: str = "",
        widget_id: str | None = None,
        appearance_state: dict | None = None,
        on_change=None,
        on_delete=None,
        on_reset=None,
    ):
        super().__init__()
        self.grid: "WidgetGrid | None" = None
        self.x, self.y = x, y
        self.w, self.h = size
        # `kind` + `widget_id` identify this instance for persistence (see
        # widget_store.py / widget_picker.build_from_state) - kind says which
        # factory can rebuild it, widget_id names its config file on disk so
        # the same file gets overwritten across saves instead of duplicated.
        self.kind = kind
        self.widget_id = widget_id or uuid.uuid4().hex
        self.content = content
        self._on_change = on_change
        self._on_delete = on_delete
        self._title_key = title_key
        self._hide_source_id: int | None = None

        card = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        card.add_css_class("card")

        content.set_hexpand(True)
        content.set_vexpand(True)
        card.append(content)

        self.set_child(card)
        self.set_size_request(self.w, self.h)

        # See SMALL_CORNER_BUTTON_ICON_PIXEL_SIZE above - only the short
        # S/SX/SSX presets drop to it, M/L/SQ stay at the normal size.
        self._corner_icon_size = SMALL_CORNER_BUTTON_ICON_PIXEL_SIZE if self.h <= SIZE_S[1] else CORNER_BUTTON_ICON_PIXEL_SIZE

        # The header floats as an overlay, like the delete/configure
        # buttons, rather than living in-flow above content: it's invisible
        # until hover anyway, and reserving its own row in the vertical box
        # would eat real layout space only at the top (nothing balances it
        # at the bottom), which throws off any content that centers itself
        # vertically against the full card - see ClockContent. It's purely
        # a label now (see _move_button below for how a widget actually
        # gets moved) - set_can_target(False) so it never intercepts a
        # click meant for content underneath, which a full-width strip
        # sitting right at the top used to do to any content (e.g. a grid
        # of icons) that also started near the widget's own top edge.
        self._header = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
        self._header.set_halign(Gtk.Align.FILL)
        self._header.set_valign(Gtk.Align.START)
        self._header.set_margin_start(8)
        self._header.set_margin_end(8)
        self._header.set_margin_top(6)
        self._header.set_margin_bottom(4)
        self._header.set_can_target(False)
        self._title_label = Gtk.Label(label=i18n._(title_key))
        self._title_label.add_css_class("heading")
        self._title_label.set_halign(Gtk.Align.START)
        self._title_label.set_hexpand(True)
        # Hidden via opacity (not set_visible) so its allocation stays
        # stable - it reveals/hides together with the configure/delete
        # buttons on hover instead of the card reflowing each time.
        self._title_label.set_opacity(0)
        self._header.append(self._title_label)
        self.add_overlay(self._header)

        self._delete_button = Gtk.Button()
        self._delete_button.add_css_class("flat")
        self._delete_button.add_css_class("circular")
        self._delete_button.add_css_class("xeneon-widget-delete")
        self._delete_button.set_child(_button_icon("user-trash-symbolic", self._corner_icon_size))
        self._delete_button.set_tooltip_text(i18n._("widgets.delete_tooltip"))
        self._delete_button.set_halign(Gtk.Align.END)
        self._delete_button.set_valign(Gtk.Align.START)
        self._delete_button.set_visible(False)
        self._delete_button.connect("clicked", lambda _b: self._on_delete_clicked())
        self.add_overlay(self._delete_button)

        self._configure_button = Gtk.Button()
        self._configure_button.add_css_class("flat")
        self._configure_button.add_css_class("circular")
        self._configure_button.add_css_class("xeneon-widget-configure")
        self._configure_button.set_child(_button_icon("preferences-system-symbolic", self._corner_icon_size))
        self._configure_button.set_tooltip_text(i18n._("widgets.configure_tooltip"))
        self._configure_button.set_halign(Gtk.Align.END)
        self._configure_button.set_valign(Gtk.Align.END)
        self._configure_button.set_visible(False)
        self.add_overlay(self._configure_button)

        # Moving the widget around the page: press-and-hold, then hover
        # (see WidgetGrid.begin/update/end_move_preview for the outline
        # preview) and release to drop - the same hold/hover/release model
        # as reordering a shortcuts icon, and for the same reason: a
        # continuous drag has no way to keep the real pointer confined to
        # the page (that GDK4 API doesn't exist portably), so a naive
        # drag_start+offset formula is at the mercy of a pointer that
        # wanders past an edge. A dedicated button (rather than the old
        # full-width header strip) also means there's no ambiguity to
        # resolve against a click or a page swipe - any press on it is
        # unambiguously "move this widget", so no arm delay is needed here.
        self._move_button = Gtk.Button()
        self._move_button.add_css_class("flat")
        self._move_button.add_css_class("circular")
        self._move_button.add_css_class("xeneon-widget-move")
        self._move_button.set_child(_button_icon("list-drag-handle-symbolic", self._corner_icon_size))
        self._move_button.set_tooltip_text(i18n._("widgets.move_tooltip"))
        self._move_button.set_halign(Gtk.Align.START)
        self._move_button.set_valign(Gtk.Align.END)
        self._move_button.set_cursor_from_name("move")
        self._move_button.set_visible(False)
        self.add_overlay(self._move_button)

        move_gesture = Gtk.GestureDrag()
        move_gesture.connect("drag-begin", self._on_move_begin)
        move_gesture.connect("drag-update", self._on_move_update)
        move_gesture.connect("drag-end", self._on_move_end)
        self._move_button.add_controller(move_gesture)

        self.appearance = WidgetAppearance(card)
        if appearance_state:
            self.appearance.apply_dict(appearance_state)
        self._appearance_popover = AppearancePopover(self.appearance, extra_settings=settings, on_reset=on_reset)
        self._appearance_popover.set_parent(self._configure_button)
        self._configure_button.connect("clicked", lambda _b: self._appearance_popover.popup())
        # The popover (appearance + any plugin-specific settings, e.g.
        # ClockSettings) is the single place both kinds of per-widget config
        # get edited - saving once on close covers both instead of wiring a
        # save call into every individual control.
        self._appearance_popover.connect("closed", lambda _p: self._notify_change())

        hover = Gtk.EventControllerMotion()
        hover.connect("enter", lambda *_a: self._reveal_configure())
        hover.connect("leave", lambda *_a: self._hide_configure_unless_popover_open())
        self.add_controller(hover)

        tap = Gtk.GestureClick()
        tap.connect("released", lambda *_a: self._reveal_configure(TOUCH_REVEAL_SECONDS))
        self.add_controller(tap)

        i18n.on_change(self._retranslate)

    def _retranslate(self):
        self._title_label.set_label(i18n._(self._title_key))
        self._delete_button.set_tooltip_text(i18n._("widgets.delete_tooltip"))
        self._configure_button.set_tooltip_text(i18n._("widgets.configure_tooltip"))
        self._move_button.set_tooltip_text(i18n._("widgets.move_tooltip"))

    def _reveal_configure(self, hide_after_seconds: int | None = None):
        self._delete_button.set_visible(True)
        self._configure_button.set_visible(True)
        self._move_button.set_visible(True)
        self._title_label.set_opacity(1)
        if self._hide_source_id is not None:
            GLib.source_remove(self._hide_source_id)
            self._hide_source_id = None
        if hide_after_seconds is not None:
            self._hide_source_id = GLib.timeout_add_seconds(hide_after_seconds, self._hide_configure)

    def _hide_configure_unless_popover_open(self):
        if self._appearance_popover.get_visible():
            return
        self._hide_configure()

    def _hide_configure(self):
        self._delete_button.set_visible(False)
        self._configure_button.set_visible(False)
        self._move_button.set_visible(False)
        self._title_label.set_opacity(0)
        self._hide_source_id = None
        return GLib.SOURCE_REMOVE

    def _notify_change(self):
        if self._on_change is not None:
            self._on_change(self)

    def _on_delete_clicked(self):
        if self._on_delete is not None:
            self._on_delete(self)
        if self.grid is not None:
            self.grid.remove_widget(self)

    def _on_move_begin(self, gesture: Gtk.GestureDrag, _x, _y):
        # Claims the pointer sequence right away - a dedicated button has
        # no ambiguity to resolve (unlike a shortcuts icon, which can also
        # be a plain click or, on the same surface, a page swipe), so
        # there's nothing to wait on. Without this, the page carousel -
        # an ancestor watching the same sequence for its own swipe
        # recognizer - could still claim a horizontal drag out from under
        # the move instead of GTK denying it to the carousel like this
        # forces. See widgets/shortcuts.py's module docstring for the same
        # reasoning applied to icon reordering.
        gesture.set_state(Gtk.EventSequenceState.CLAIMED)
        # self.x/self.y stay untouched for the whole gesture now - only the
        # preview outline moves while the button is held (see
        # WidgetGrid.begin_move_preview), so there's nothing to "start
        # from" other than the widget's own current, unchanged position.
        if self.grid is not None:
            self.grid.begin_move_preview(self)

    def _on_move_update(self, _gesture, offset_x: float, offset_y: float):
        if self.grid is not None:
            self.grid.update_move_preview(self, self.x + int(offset_x), self.y + int(offset_y))

    def _on_move_end(self, _gesture, offset_x: float, offset_y: float):
        if self.grid is None:
            return
        landing = self.grid.end_move_preview(self, self.x + int(offset_x), self.y + int(offset_y))
        if landing is not None:
            self.grid.move_widget(self, *landing)
            self._notify_change()


class WidgetGrid(Gtk.Fixed):
    """A page of DashboardWidgets. Positions are real-panel pixel
    coordinates (up to 2560x720) - the window is hardcoded to that same
    size (see window.py), so the page always has exactly the room it
    needs, edge to edge. Not actually free-form: a drag always snaps onto
    the page's own column layout (see _snap_to_layout), so blocks stay
    flush against each other and the page edges regardless of size."""

    # Set by XeneonWindow.add_widget_page() to this page's position among
    # the carousel's widget pages - persisted alongside each widget's own
    # state (see widget_store.py) so a saved layout can be rebuilt page by
    # page on the next launch.
    page_index: int = 0

    def __init__(
        self,
        *,
        page_id: str | None = None,
        name: str | None = None,
        background_state: dict | None = None,
    ):
        super().__init__()
        self._move_preview: Gtk.Box | None = None
        # Identifies this page for its own persisted file (see page_store.py
        # / XeneonWindow._save_page) - separate from page_index (a position)
        # for the same reason DashboardWidget.widget_id is separate from
        # x/y: identity has to survive things that change position.
        self.page_id = page_id or uuid.uuid4().hex
        self.custom_name = name
        self.background = PageBackground(self)
        if background_state:
            self.background.apply_dict(background_state)

    def display_name(self) -> str:
        if self.custom_name:
            return self.custom_name
        return i18n._("settings.pages_group.default_name", n=self.page_index + 1)

    def add_widget(self, widget: DashboardWidget):
        widget.grid = self
        self.put(widget, widget.x, widget.y)

    def remove_widget(self, widget: DashboardWidget):
        widget.grid = None
        self.remove(widget)

    def move_widget(self, widget: DashboardWidget, x: int, y: int):
        widget.x, widget.y = x, y
        self.move(widget, x, y)

    def begin_move_preview(self, widget: DashboardWidget):
        """Shows the move-preview outline at `widget`'s current spot, sized
        to *its own* footprint - widgets come in several sizes (S/M/L/SQ),
        so this can't assume one fixed size the way the shortcuts grid's
        icon cells can. Built fresh every time (not reused + resized) so
        there's no way for a previous widget's size to still be in effect -
        a plain Gtk.Box has no state worth keeping across drags anyway."""
        _ensure_move_preview_css()
        if self._move_preview is not None:
            self.remove(self._move_preview)
        self._move_preview = Gtk.Box()
        self._move_preview.set_can_target(False)
        self._move_preview.set_size_request(widget.w, widget.h)
        # Added last - i.e. on top of every widget already on the page.
        self.put(self._move_preview, widget.x, widget.y)
        self._update_move_preview_validity(widget, widget.x, widget.y)

    def update_move_preview(self, widget: DashboardWidget, raw_x: int, raw_y: int) -> tuple[int, int]:
        """Snaps (raw_x, raw_y) onto the page's own column layout (see
        _snap_to_layout) rather than an arbitrary pixel offset, repositions
        the preview there, and colors it for whether that spot is actually
        free. Returns the snapped position, so callers don't redo this math
        themselves."""
        x, y = self._snap_to_layout(widget, raw_x, raw_y)
        self.move(self._move_preview, x, y)
        self._update_move_preview_validity(widget, x, y)
        return x, y

    def _snap_to_layout(self, widget: DashboardWidget, raw_x: int, raw_y: int) -> tuple[int, int]:
        """Where `widget` would actually land, snapped to the page's own
        invisible layout grid instead of a free pixel offset: x onto one of
        the fixed full- or half-width columns its own width tiles at (see
        _column_positions), y onto a "shelf" line - either the very top,
        just below another widget that shares that x range, or one of
        `widget`'s own natural stacking rows (as if the column were filled
        with copies of it) - that last part matters for a short widget like
        S: with only one (or none) of them already in a column, "just below
        another widget" alone gives at most one candidate below 0, so
        nothing between 0 and the page bottom is ever reachable and the
        widget can't be dragged down at all. This is the same shelf idea
        find_free_position() already uses to auto-place a new widget,
        applied here to a drag instead, so a block always ends up flush
        against whatever it's near - never almost-but-not-quite touching
        another one, or a few stray pixels off the page edge - regardless
        of which mix of S/M/L/SQ sizes share the page, since they're all
        built to nest into the same underlying grid (see SIZE_S/M/L/SQ
        above)."""
        page_w = self.get_width() or PAGE_W
        page_h = self.get_height() or PAGE_H
        max_x = max(0, page_w - widget.w)
        max_y = max(0, page_h - widget.h)

        columns = _column_positions(page_w, widget.w)
        x = _nearest(max(0, min(raw_x, max_x)), columns)

        shelves = set()
        step = widget.h + GAP
        shelf = 0
        while shelf <= max_y:
            shelves.add(shelf)
            shelf += step
        for other in self._widgets():
            if other is widget:
                continue
            if other.x < x + widget.w and x < other.x + other.w:
                shelves.add(other.y + other.h + GAP)
        candidates = [shelf for shelf in shelves if shelf <= max_y] or [0]
        y = _nearest(max(0, min(raw_y, max_y)), candidates)
        return x, y

    def _update_move_preview_validity(self, widget: DashboardWidget, x: int, y: int):
        if self.is_free(widget, x, y):
            self._move_preview.remove_css_class(_MOVE_PREVIEW_INVALID_CLASS)
            self._move_preview.add_css_class(_MOVE_PREVIEW_VALID_CLASS)
        else:
            self._move_preview.remove_css_class(_MOVE_PREVIEW_VALID_CLASS)
            self._move_preview.add_css_class(_MOVE_PREVIEW_INVALID_CLASS)

    def end_move_preview(self, widget: DashboardWidget, raw_x: int, raw_y: int) -> tuple[int, int] | None:
        """Removes the preview and returns the final (snapped, clamped)
        landing position if it's free, None if it would overlap another
        widget - the widget itself never actually moved while the preview
        was showing, so the caller only needs to act on a valid result."""
        x, y = self.update_move_preview(widget, raw_x, raw_y)
        valid = self.is_free(widget, x, y)
        self.remove(self._move_preview)
        self._move_preview = None
        return (x, y) if valid else None

    def widgets(self) -> list[DashboardWidget]:
        return list(self._widgets())

    def _widgets(self):
        # Skips non-DashboardWidget children - namely _move_preview, which
        # lives in this same Fixed once shown (see begin_move_preview) but
        # isn't a placed widget and has none of x/y/w/h.
        child = self.get_first_child()
        while child is not None:
            if isinstance(child, DashboardWidget):
                yield child
            child = child.get_next_sibling()

    def is_free(self, widget: DashboardWidget, x: int, y: int) -> bool:
        """Whether `widget` could sit at (x, y) - its own current size, that
        position - without overlapping any other widget already on this
        page. Used to let a drag land only on genuinely free ground (see
        DashboardWidget._on_move_update/_on_move_end), instead of just
        letting it settle on top of whatever's already there."""
        rect = (x, y, widget.w, widget.h)
        return not any(
            _overlaps(rect, (other.x, other.y, other.w, other.h))
            for other in self._widgets()
            if other is not widget
        )

    def find_free_position(self, size: tuple[int, int]) -> tuple[int, int] | None:
        """First free (x, y) on this page for a new widget of `size`,
        scanning left to right at the column pitch its width tiles at (a
        full SIZE_S/M/L column, or an SIZE_SQ half-column), then top to
        bottom in shelves below the widgets already placed. None if this
        page has no room left for it - the caller should try the next page,
        or create one (see XeneonWindow.add_widget)."""
        w, h = size
        page_w = self.get_width() or PAGE_W
        page_h = self.get_height() or PAGE_H
        occupied = [(child.x, child.y, child.w, child.h) for child in self._widgets()]

        step_x = SIZE_SQ[0] + GAP if w <= SIZE_SQ[0] else SIZE_S[0] + GAP
        candidate_ys = sorted({0} | {y + ch + GAP for _x, y, _w, ch in occupied})
        for y in candidate_ys:
            if y + h > page_h:
                continue
            x = 0
            while x + w <= page_w:
                if not any(_overlaps((x, y, w, h), rect) for rect in occupied):
                    return (x, y)
                x += step_x
        return None
