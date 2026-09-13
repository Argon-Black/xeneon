import logging

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, GLib, Gtk

from xeneon_dashboard import i18n

logger = logging.getLogger(__name__)
from xeneon_dashboard.grid import GAP, SIZE_L, SIZE_M, SIZE_S, SIZE_SQ, SIZE_SSX, SIZE_SX, DashboardWidget
from xeneon_dashboard.widgets.agenda import AgendaContent, AgendaSettings
from xeneon_dashboard.widgets.audio import AudioContent, AudioSettings
from xeneon_dashboard.widgets.clock import ClockContent, ClockSettings
from xeneon_dashboard.widgets.cpu_temp import CpuTempContent, CpuTempSettings, TempGaugeContent, TempGaugeSettings
from xeneon_dashboard.widgets.dummy import DummyContent, color_for_size
from xeneon_dashboard.widgets.shortcuts import ShortcutsContent, ShortcutsSettings
from xeneon_dashboard.widgets.weather import WeatherContent, WeatherSettings


_DUMMY_TITLE_KEYS = {
    "S": "widgets.dummy.title_s",
    "M": "widgets.dummy.title_m",
    "L": "widgets.dummy.title_l",
    "SQ": "widgets.dummy.title_sq",
    "SX": "widgets.dummy.title_sx",
    "SSX": "widgets.dummy.title_ssx",
}


def _spawn_clock(x: int, y: int, *, on_change=None, on_delete=None) -> DashboardWidget:
    content = ClockContent()
    settings = ClockSettings(content)
    return DashboardWidget(
        "widgets.clock.title",
        content,
        x=x,
        y=y,
        size=SIZE_M,
        settings=settings,
        kind="clock",
        on_change=on_change,
        on_delete=on_delete,
        on_reset=lambda: (content.reset(), settings.sync_from_content()),
    )


def _spawn_weather(x: int, y: int, *, on_change=None, on_delete=None) -> DashboardWidget:
    content = WeatherContent()
    settings = WeatherSettings(content)
    return DashboardWidget(
        "widgets.weather.title",
        content,
        x=x,
        y=y,
        size=SIZE_M,
        settings=settings,
        kind="weather",
        on_change=on_change,
        on_delete=on_delete,
    )


def _spawn_agenda(x: int, y: int, *, on_change=None, on_delete=None) -> DashboardWidget:
    content = AgendaContent()
    settings = AgendaSettings(content)
    return DashboardWidget(
        "widgets.agenda.title",
        content,
        x=x,
        y=y,
        size=SIZE_M,
        settings=settings,
        kind="agenda",
        on_change=on_change,
        on_delete=on_delete,
    )


def _spawn_cpu_temp(x: int, y: int, *, on_change=None, on_delete=None) -> DashboardWidget:
    content = CpuTempContent()
    settings = CpuTempSettings(content)
    return DashboardWidget(
        "widgets.cpu_temp.title",
        content,
        x=x,
        y=y,
        size=SIZE_SSX,
        settings=settings,
        kind="cpu_temp",
        on_change=on_change,
        on_delete=on_delete,
    )


def _spawn_temp_gauge(x: int, y: int, *, on_change=None, on_delete=None) -> DashboardWidget:
    content = TempGaugeContent()
    settings = TempGaugeSettings(content)
    return DashboardWidget(
        "widgets.temp_gauge.title",
        content,
        x=x,
        y=y,
        size=SIZE_SQ,
        settings=settings,
        kind="temp_gauge",
        on_change=on_change,
        on_delete=on_delete,
    )


def _audio_spawner(size: tuple[int, int]):
    # One factory for both sizes offered in CATALOG (L and SQ) - same
    # kind for both, since AudioContent adapts its own layout from `size`
    # (see its _apply_scale) rather than needing a separate widget per
    # footprint the way the dummy widgets do.
    def spawn(x: int, y: int, *, on_change=None, on_delete=None) -> DashboardWidget:
        content = AudioContent(size)
        settings = AudioSettings(content)
        # Empty title_key, like _spawn_shortcuts: the content draws its
        # own title/artist over the album art, so the generic header
        # label would just duplicate it.
        return DashboardWidget(
            "",
            content,
            x=x,
            y=y,
            size=size,
            settings=settings,
            kind="audio",
            on_change=on_change,
            on_delete=on_delete,
        )

    return spawn


def _spawn_shortcuts(x: int, y: int, *, on_change=None, on_delete=None) -> DashboardWidget:
    content = ShortcutsContent(SIZE_L)
    settings = ShortcutsSettings(content)
    # No on-card title (empty title_key) - its own always-visible "+"
    # button sits in that same top-left corner instead (see
    # ShortcutsContent), which needs the room and is more useful there
    # than a label. CATALOG below still names it "Raccourcis" in the
    # picker (see widgets.shortcuts.title), unaffected by this.
    widget = DashboardWidget(
        "",
        content,
        x=x,
        y=y,
        size=SIZE_L,
        settings=settings,
        kind="shortcuts",
        on_change=on_change,
        on_delete=on_delete,
        on_reset=lambda: (content.reset_backdrop(), settings.sync_from_content()),
    )
    content.set_change_notifier(lambda: on_change(widget) if on_change else None)
    return widget


def _dummy_spawner(size_code: str, title_key: str, size: tuple[int, int]):
    kind = f"dummy_{size_code.lower()}"

    def spawn(x: int, y: int, *, on_change=None, on_delete=None) -> DashboardWidget:
        widget = DashboardWidget(
            title_key, DummyContent(size_code), x=x, y=y, size=size, kind=kind, on_change=on_change, on_delete=on_delete
        )
        widget.appearance.set_bg_color(color_for_size(size_code))
        return widget

    return spawn


# Every kind of widget the picker offers, as (title key, size preset,
# spawn(x, y, *, on_change, on_delete) factory, preview() factory). One
# clock, and one dummy widget per grid size preset (see grid.py) so every
# footprint can be tried. `spawn`'s kind (set on the built DashboardWidget)
# is what build_from_state() below uses to find its way back to the same
# factory when restoring a saved layout. `preview` returns a fresh,
# throwaway content widget (no DashboardWidget wrapper, no settings) used
# only to show what this entry actually looks like in WidgetPicker - see
# _PreviewTile. Kept as a small zero-arg lambda per entry rather than
# derived from `spawn` so a preview never accidentally ends up wired to a
# grid/on_change/on_delete it doesn't have.
#
# Quand on ajoute un nouveau plugin ici: choisir la taille (S/M/L/SQ/SX/
# SSX) avec l'utilisateur plutôt que de deviner, et fournir les deux
# factories - `spawn` comme avant, `preview` comme `lambda: MonContent()`
# (ou l'équivalent avec les arguments qu'il attend, ex. AudioContent(size)).
CATALOG = [
    ("widgets.clock.title", SIZE_M, _spawn_clock, lambda: ClockContent()),
    ("widgets.weather.title", SIZE_M, _spawn_weather, lambda: WeatherContent()),
    ("widgets.agenda.title", SIZE_M, _spawn_agenda, lambda: AgendaContent()),
    ("widgets.cpu_temp.title", SIZE_SSX, _spawn_cpu_temp, lambda: CpuTempContent()),
    ("widgets.temp_gauge.title", SIZE_SQ, _spawn_temp_gauge, lambda: TempGaugeContent()),
    ("widgets.shortcuts.title", SIZE_L, _spawn_shortcuts, lambda: ShortcutsContent(SIZE_L)),
    ("widgets.audio.title", SIZE_L, _audio_spawner(SIZE_L), lambda: AudioContent(SIZE_L)),
    ("widgets.audio.title", SIZE_SQ, _audio_spawner(SIZE_SQ), lambda: AudioContent(SIZE_SQ)),
    ("widgets.dummy.title_s", SIZE_S, _dummy_spawner("S", "widgets.dummy.title_s", SIZE_S), lambda: DummyContent("S")),
    ("widgets.dummy.title_m", SIZE_M, _dummy_spawner("M", "widgets.dummy.title_m", SIZE_M), lambda: DummyContent("M")),
    ("widgets.dummy.title_l", SIZE_L, _dummy_spawner("L", "widgets.dummy.title_l", SIZE_L), lambda: DummyContent("L")),
    ("widgets.dummy.title_sq", SIZE_SQ, _dummy_spawner("SQ", "widgets.dummy.title_sq", SIZE_SQ), lambda: DummyContent("SQ")),
    ("widgets.dummy.title_sx", SIZE_SX, _dummy_spawner("SX", "widgets.dummy.title_sx", SIZE_SX), lambda: DummyContent("SX")),
    (
        "widgets.dummy.title_ssx",
        SIZE_SSX,
        _dummy_spawner("SSX", "widgets.dummy.title_ssx", SIZE_SSX),
        lambda: DummyContent("SSX"),
    ),
]


def build_from_state(state: dict, *, on_change=None, on_delete=None) -> DashboardWidget | None:
    """Rebuilds one DashboardWidget from a dict previously produced by
    XeneonWindow._save_widget() (see widget_store.py) - None if `kind` is
    unrecognized (e.g. a config file left over from a removed widget type),
    so the caller can just skip it instead of crashing the whole layout
    load. Content-specific state (font, colors, city...) is applied to the
    content *before* building its settings widget/the appearance popover, so
    both immediately reflect the restored state rather than defaults."""
    kind = state.get("kind")
    w, h = state.get("w"), state.get("h")
    if not kind or w is None or h is None:
        logger.warning("Widget %s ignoré: état incomplet (kind=%r, w=%r, h=%r)", state.get("id"), kind, w, h)
        return None
    x, y = state.get("x", 0), state.get("y", 0)
    widget_id = state.get("id")
    appearance_state = state.get("appearance")

    if kind == "clock":
        content = ClockContent()
        content.apply_dict(state.get("content", {}))
        settings = ClockSettings(content)
        return DashboardWidget(
            "widgets.clock.title",
            content,
            x=x,
            y=y,
            size=(w, h),
            settings=settings,
            kind=kind,
            widget_id=widget_id,
            appearance_state=appearance_state,
            on_change=on_change,
            on_delete=on_delete,
            on_reset=lambda: (content.reset(), settings.sync_from_content()),
        )

    if kind == "weather":
        content = WeatherContent()
        content.apply_dict(state.get("content", {}))
        settings = WeatherSettings(content)
        return DashboardWidget(
            "widgets.weather.title",
            content,
            x=x,
            y=y,
            size=(w, h),
            settings=settings,
            kind=kind,
            widget_id=widget_id,
            appearance_state=appearance_state,
            on_change=on_change,
            on_delete=on_delete,
        )

    if kind == "agenda":
        content = AgendaContent()
        content.apply_dict(state.get("content", {}))
        settings = AgendaSettings(content)
        return DashboardWidget(
            "widgets.agenda.title",
            content,
            x=x,
            y=y,
            size=(w, h),
            settings=settings,
            kind=kind,
            widget_id=widget_id,
            appearance_state=appearance_state,
            on_change=on_change,
            on_delete=on_delete,
        )

    if kind == "cpu_temp":
        content = CpuTempContent()
        content.apply_dict(state.get("content", {}))
        settings = CpuTempSettings(content)
        return DashboardWidget(
            "widgets.cpu_temp.title",
            content,
            x=x,
            y=y,
            size=(w, h),
            settings=settings,
            kind=kind,
            widget_id=widget_id,
            appearance_state=appearance_state,
            on_change=on_change,
            on_delete=on_delete,
        )

    if kind == "temp_gauge":
        content = TempGaugeContent()
        content.apply_dict(state.get("content", {}))
        settings = TempGaugeSettings(content)
        return DashboardWidget(
            "widgets.temp_gauge.title",
            content,
            x=x,
            y=y,
            size=(w, h),
            settings=settings,
            kind=kind,
            widget_id=widget_id,
            appearance_state=appearance_state,
            on_change=on_change,
            on_delete=on_delete,
        )

    if kind == "audio":
        content = AudioContent((w, h))
        content.apply_dict(state.get("content", {}))
        settings = AudioSettings(content)
        return DashboardWidget(
            "",
            content,
            x=x,
            y=y,
            size=(w, h),
            settings=settings,
            kind=kind,
            widget_id=widget_id,
            appearance_state=appearance_state,
            on_change=on_change,
            on_delete=on_delete,
        )

    if kind == "shortcuts":
        content = ShortcutsContent((w, h))
        content.apply_dict(state.get("content", {}))
        settings = ShortcutsSettings(content)
        widget = DashboardWidget(
            "",
            content,
            x=x,
            y=y,
            size=(w, h),
            settings=settings,
            kind=kind,
            widget_id=widget_id,
            appearance_state=appearance_state,
            on_change=on_change,
            on_delete=on_delete,
            on_reset=lambda: (content.reset_backdrop(), settings.sync_from_content()),
        )
        content.set_change_notifier(lambda: on_change(widget) if on_change else None)
        return widget

    if kind.startswith("dummy_"):
        size_code = kind[len("dummy_") :].upper()
        title_key = _DUMMY_TITLE_KEYS.get(size_code)
        if title_key is None:
            logger.warning("Widget %s ignoré: taille de dummy inconnue %r", widget_id, size_code)
            return None
        return DashboardWidget(
            title_key,
            DummyContent(size_code),
            x=x,
            y=y,
            size=(w, h),
            kind=kind,
            widget_id=widget_id,
            appearance_state=appearance_state,
            on_change=on_change,
            on_delete=on_delete,
        )

    logger.warning("Widget %s ignoré: kind inconnu %r", widget_id, kind)
    return None


# Every CATALOG entry is bucketed into one of these three families by its
# own height alone (see _size_family) - SQ shares M's height and SSX/SX/S
# all share S's, so this sorts them into exactly the three "shelves" the
# grid itself already groups into (see grid.py's SIZE_* comments), shown
# smallest first since that's also screen-space order: at the panel's real
# 720px height, SIZE_L already fills 95% of it on its own, so "large" is
# always the last (and typically the only) family visible without
# scrolling - see WidgetPicker's own docstring.
_FAMILY_ORDER = ["compact", "medium", "large"]
_FAMILY_LABEL_KEYS = {
    "compact": "widgets.add_menu.family_compact",
    "medium": "widgets.add_menu.family_medium",
    "large": "widgets.add_menu.family_large",
}


def _size_family(size: tuple[int, int]) -> str:
    height = size[1]
    if height >= SIZE_L[1]:
        return "large"
    if height >= SIZE_M[1]:
        return "medium"
    return "compact"


def _grouped_catalog() -> list[tuple[str, list[tuple]]]:
    buckets: dict[str, list[tuple]] = {family: [] for family in _FAMILY_ORDER}
    for entry in CATALOG:
        buckets[_size_family(entry[1])].append(entry)
    for entries in buckets.values():
        entries.sort(key=lambda entry: entry[1][0])
    return [(family, buckets[family]) for family in _FAMILY_ORDER if buckets[family]]


_picker_provider = Gtk.CssProvider()
_picker_css_installed = False

# `@accent_color` is defined by theme.py's own provider (see its module
# docstring for why this doesn't need reloading whenever the accent
# changes) - referencing it here just needs theme.install_css() to have
# run once before this stylesheet is actually applied to anything, which
# app.py's startup already guarantees.
_PICKER_CSS = """
.xeneon-widget-picker-surface {
  background-color: rgba(15, 15, 17, 0.97);
}
.xeneon-widget-picker-header {
  padding: 18px 24px 14px 24px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}
.xeneon-widget-picker-close:hover {
  background-color: alpha(@accent_color, 0.25);
}
.xeneon-widget-picker-body {
  padding: 18px 24px 24px 24px;
}
.xeneon-widget-picker-family {
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.06em;
  opacity: 0.55;
}
.xeneon-widget-picker-tile {
  border-radius: 12px;
  border: 1px solid rgba(255, 255, 255, 0.08);
}
.xeneon-widget-picker-tile:hover {
  border-color: @accent_color;
}
.xeneon-widget-picker-tile-name {
  font-size: 11px;
  font-weight: 600;
  color: #ffffff;
  background-color: rgba(0, 0, 0, 0.5);
  padding: 3px 8px;
  border-radius: 999px;
}
"""


def _ensure_picker_css():
    global _picker_css_installed
    if _picker_css_installed:
        return
    Gtk.StyleContext.add_provider_for_display(
        Gdk.Display.get_default(), _picker_provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )
    _picker_provider.load_from_string(_PICKER_CSS)
    _picker_css_installed = True


class _PreviewTile(Gtk.Overlay):
    """One entry in the picker: the real widget content, live and at its
    true pixel size (see CATALOG's preview factories) inside a plain card -
    no delete/configure/move chrome, since nothing has been placed on a
    page yet. `content.set_can_target(False)` so the tile's own click
    gesture always gets the click instead of something inside the preview
    (e.g. the shortcuts "+" button, or the audio transport buttons)
    swallowing it - same technique grid.py's own header label already uses
    to stay click-through."""

    def __init__(self, title_key: str, size: tuple[int, int], spawn, build_preview, on_activate):
        super().__init__()
        self.add_css_class("xeneon-widget-picker-tile")
        self.set_size_request(*size)
        # CSS `overflow` isn't a real GTK CSS property - clipping to the
        # tile's own rounded corners (see .xeneon-widget-picker-tile's
        # border-radius) needs this actual widget property instead.
        self.set_overflow(Gtk.Overflow.HIDDEN)

        card = Gtk.Box()
        card.add_css_class("card")
        content = build_preview()
        content.set_hexpand(True)
        content.set_vexpand(True)
        content.set_can_target(False)
        card.append(content)
        self.set_child(card)

        name_label = Gtk.Label(label=i18n._(title_key))
        name_label.add_css_class("xeneon-widget-picker-tile-name")
        name_label.set_halign(Gtk.Align.START)
        name_label.set_valign(Gtk.Align.END)
        name_label.set_margin_start(6)
        name_label.set_margin_bottom(6)
        self.add_overlay(name_label)

        click = Gtk.GestureClick()
        click.connect("released", lambda *_a: on_activate(size, spawn))
        self.add_controller(click)


class WidgetPicker(Gtk.Revealer):
    """Full-screen replacement for the old Ctrl++ popover: slides down from
    the top to cover the whole window (see window.py, which adds this as
    an overlay above everything else) and lists every CATALOG entry as its
    own real, live preview, grouped smallest to largest (see
    _grouped_catalog). At the panel's actual 720px height a single SIZE_L
    entry already fills nearly the whole screen, so every family can't be
    shown at once no matter how this is laid out - the body scrolls
    instead of trying to cram everything above the fold."""

    def __init__(self, on_pick):
        super().__init__()
        self._on_pick = on_pick
        self.set_transition_type(Gtk.RevealerTransitionType.SLIDE_DOWN)
        self.set_transition_duration(550)
        self.set_halign(Gtk.Align.FILL)
        self.set_valign(Gtk.Align.FILL)
        self.set_hexpand(True)
        self.set_vexpand(True)
        self.add_css_class("xeneon-widget-picker")
        _ensure_picker_css()

        surface = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        surface.add_css_class("xeneon-widget-picker-surface")

        header = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
        header.add_css_class("xeneon-widget-picker-header")
        self._title_label = Gtk.Label()
        self._title_label.add_css_class("title-2")
        self._title_label.set_halign(Gtk.Align.START)
        self._title_label.set_hexpand(True)
        header.append(self._title_label)
        self._close_button = Gtk.Button()
        self._close_button.add_css_class("flat")
        self._close_button.add_css_class("circular")
        self._close_button.add_css_class("xeneon-widget-picker-close")
        self._close_button.set_icon_name("window-close-symbolic")
        self._close_button.connect("clicked", lambda _b: self.close())
        header.append(self._close_button)
        surface.append(header)

        self._scroller = Gtk.ScrolledWindow()
        self._scroller.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        self._scroller.set_vexpand(True)
        self._body = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=20)
        self._body.add_css_class("xeneon-widget-picker-body")
        self._scroller.set_child(self._body)
        surface.append(self._scroller)

        self.set_child(surface)

        key_controller = Gtk.EventControllerKey()
        key_controller.connect("key-pressed", self._on_key_pressed)
        self.add_controller(key_controller)

        self._build_source_id: int | None = None
        self._build_queue: list[tuple[Gtk.FlowBox, tuple]] = []

        self._retranslate()
        i18n.on_change(self._retranslate)

    def _on_key_pressed(self, _controller, keyval, _keycode, _state) -> bool:
        if keyval == Gdk.KEY_Escape and self.get_reveal_child():
            self.close()
            return True
        return False

    def is_open(self) -> bool:
        return self.get_reveal_child()

    def open(self):
        self._rebuild_body()
        self.set_reveal_child(True)
        self.grab_focus()

    def close(self):
        self.set_reveal_child(False)
        # Drops every preview widget the moment the picker closes rather
        # than leaving them alive off-screen - several of them (weather,
        # agenda, cpu_temp, temp_gauge) hold their own GLib timers/
        # background threads (see each content class's own "destroy"
        # handler), and there's no reason to keep those polling while
        # nobody can see them.
        self._cancel_build_queue()
        self._clear_body()

    def _clear_body(self):
        child = self._body.get_first_child()
        while child is not None:
            next_child = child.get_next_sibling()
            self._body.remove(child)
            child = next_child

    def _cancel_build_queue(self):
        if self._build_source_id is not None:
            GLib.source_remove(self._build_source_id)
            self._build_source_id = None
        self._build_queue = []

    def _rebuild_body(self):
        self._cancel_build_queue()
        self._clear_body()
        # Section headers and (empty) flow rows are built right away - cheap
        # widgets, no live content - but each tile's own preview (a real
        # ClockContent/WeatherContent/AgendaContent/... - see CATALOG) is
        # queued and built one at a time on the idle loop instead of all at
        # once here. Building all ~13 of them synchronously (weather's HTTP
        # fetch kicking off, agenda's EDS connection, MPRIS DBus watches for
        # both audio entries, a hwmon read for each temp entry...) was
        # enough to visibly stall the main loop for a frame or more right as
        # the picker opens - long enough that the compositor could mistake
        # the fullscreened window for unresponsive and reveal the desktop's
        # own top panel over the Xeneon Edge until the window recovers.
        # Spreading construction across idle callbacks keeps every single
        # iteration cheap, so the window never stops acking frames.
        for family, entries in _grouped_catalog():
            section = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
            label = Gtk.Label(label=i18n._(_FAMILY_LABEL_KEYS[family]))
            label.add_css_class("xeneon-widget-picker-family")
            label.set_halign(Gtk.Align.START)
            section.append(label)

            flow = Gtk.FlowBox()
            flow.set_selection_mode(Gtk.SelectionMode.NONE)
            flow.set_homogeneous(False)
            flow.set_row_spacing(GAP)
            flow.set_column_spacing(GAP)
            flow.set_halign(Gtk.Align.START)
            flow.set_max_children_per_line(1000)
            section.append(flow)
            self._body.append(section)

            for entry in entries:
                self._build_queue.append((flow, entry))

        self._build_source_id = GLib.idle_add(self._pump_build_queue)

    def _pump_build_queue(self) -> bool:
        if not self._build_queue:
            self._build_source_id = None
            return GLib.SOURCE_REMOVE
        flow, (title_key, size, spawn, build_preview) = self._build_queue.pop(0)
        tile_wrap = Gtk.FlowBoxChild()
        tile_wrap.set_focusable(False)
        tile_wrap.set_child(_PreviewTile(title_key, size, spawn, build_preview, self._on_tile_activated))
        flow.append(tile_wrap)
        return GLib.SOURCE_CONTINUE

    def _on_tile_activated(self, size: tuple[int, int], spawn):
        self._on_pick(size, spawn)
        self.close()

    def _retranslate(self):
        self._title_label.set_label(i18n._("widgets.add_menu.title"))
