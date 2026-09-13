"""Agenda widget: a month calendar (today's date big on the left, the full
month grid on the right, like the reference look this follows) with a
small colored bar under any day that has at least one event - pulled from
whichever calendars are registered with Evolution Data Server, the same
source registry GNOME's Réglages > Comptes en ligne panel manages. A
CalDAV/Exchange/Google account just needs its "Calendrier" toggle enabled
there once; this widget picks up its events with nothing else to
configure - no login form, no credentials stored by this app at all,
GOA/EDS already handles auth and keeps the password in the keyring.

A month grid (rather than a list of upcoming events) reads useful even on
a day with nothing coming up soon - the common case for most people most
of the time - where a bare "no upcoming events" list would just sit empty.

Recurring events are expanded into their actual occurrences by
ECal.Client.generate_instances_sync - a plain get_object_list would return
the recurrence rule itself, not each date it actually falls on. Like
weather.py, connecting to a calendar and querying it are blocking calls
(local D-Bus round-trips to evolution-data-server, sometimes fanning out to
a real network request against Exchange/CalDAV), so they run on a
background thread and results are marshalled back to the main thread via
GLib.idle_add."""

import calendar
import collections
import datetime
import threading

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
gi.require_version("EDataServer", "1.2")
gi.require_version("ECal", "2.0")
gi.require_version("ICalGLib", "3.0")
from gi.repository import ECal, EDataServer, Gdk, GLib, Gtk

from xeneon_dashboard import i18n

REFRESH_INTERVAL_SECONDS = 5 * 60
CONNECT_TIMEOUT_SECONDS = 5
DEFAULT_DOT_HEX = "#62a0ea"
TODAY_BG_HEX = "#ffffff"
TODAY_TEXT_HEX = "#000000"
DEFAULT_WEEKEND_HEX = "#ffa726"
WEEKEND_COLS = {5, 6}  # Saturday, Sunday - Monday-first columns, see DAY_KEYS
WEEKS_SHOWN = 6  # padded out to a constant row count so the grid's height never shifts month to month
GRID_RIGHT_MARGIN_PX = 24  # matches the breathing room the vertical centering already gives top/bottom

# Left panel (weekday/day-number/full-date) font sizes at content_scale ==
# 1.0 - the only part this widget's own "content size" setting affects
# (see AgendaSettings), same idea as WeatherContent.content_scale. The
# month grid on the right has its own fixed, bolder sizing instead (not
# scaled) since it needs to stay legible and fit its 7-column layout
# regardless of how big the user wants the date panel.
BASE_WEEKDAY_FONT_PX = 15
BASE_DAYNUM_FONT_PX = 88
BASE_FULLDATE_FONT_PX = 14
BASE_LEFT_SPACING_PX = 0
MIN_CONTENT_SCALE = 0.5
MAX_CONTENT_SCALE = 2.0
DEFAULT_CONTENT_SCALE = 1.75

DAY_KEYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
MONTH_KEYS = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"]

# One entry per grid cell (see _build_grid_skeleton) - grouped in a
# namedtuple rather than a growing plain tuple so the zip()s in
# _apply_static_labels/_apply_marks that walk every cell stay readable.
_CellWidgets = collections.namedtuple(
    "_CellWidgets", "cell number_wrap number_label bars_row popover popover_box"
)

# Static rules, installed once - font weight/color/shape that never change
# at runtime. Per-instance rules (left panel font sizes, which do change
# with content_scale) live in a second, reloadable provider below - same
# split as weather.py's _ensure_css_installed() vs _rules/_reload_css().
_static_provider = Gtk.CssProvider()
_static_installed = False


def _ensure_static_css_installed():
    global _static_installed
    if _static_installed:
        return
    Gtk.StyleContext.add_provider_for_display(
        Gdk.Display.get_default(), _static_provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )
    _static_provider.load_from_string(
        ".xeneon-agenda-weekday { font-weight: 600; color: #ffffff; opacity: 0.75; line-height: 0.8;"
        " letter-spacing: 1px; text-transform: uppercase; }"
        ".xeneon-agenda-daynum { font-weight: 700; color: #ffffff; line-height: 0.85; }"
        ".xeneon-agenda-fulldate { color: #ffffff; opacity: 0.75; line-height: 0.8; }"
        ".xeneon-agenda-header-cell { font-size: 11px; font-weight: 600; color: #ffffff; opacity: 0.55;"
        " letter-spacing: 1px; text-transform: uppercase; }"
        ".xeneon-agenda-cell-num { font-size: 20px; font-weight: 700; color: #ffffff; }"
        ".xeneon-agenda-cell-num.dim { opacity: 0.3; }"
        f".xeneon-agenda-today {{ background-color: {TODAY_BG_HEX}; border-radius: 6px;"
        " min-width: 28px; min-height: 28px; }"
        ".xeneon-agenda-bar { min-width: 14px; min-height: 4px; border-radius: 2px; }"
    )
    _static_installed = True


# Per-instance, content_scale-dependent rules - one entry per AgendaContent,
# keyed by its own unique css class, reloaded whenever any instance's scale
# changes. Same pattern as weather.py's WeatherContent.
_scale_provider = Gtk.CssProvider()
_scale_installed = False
_scale_rules: dict[str, str] = {}


def _ensure_scale_css_installed():
    global _scale_installed
    if _scale_installed:
        return
    Gtk.StyleContext.add_provider_for_display(
        Gdk.Display.get_default(), _scale_provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )
    _scale_installed = True


def _reload_scale_css():
    _scale_provider.load_from_string("\n".join(rule for rule in _scale_rules.values() if rule))


def _rgba_to_hex(rgba: Gdk.RGBA) -> str:
    r, g, b = (round(c * 255) for c in (rgba.red, rgba.green, rgba.blue))
    return f"#{r:02x}{g:02x}{b:02x}"


def _hex_to_rgba(hex_str: str) -> Gdk.RGBA:
    rgba = Gdk.RGBA()
    rgba.parse(hex_str)
    return rgba


def list_calendar_sources() -> list[EDataServer.Source]:
    """Every calendar EDS currently knows about, GOA-backed or local -
    read fresh each time (this is a cheap local registry lookup, not a
    network call) so a settings popover always reflects accounts added or
    removed since the widget was created."""
    registry = EDataServer.SourceRegistry.new_sync(None)
    return list(registry.list_sources(EDataServer.SOURCE_EXTENSION_CALENDAR))


def source_color(source: EDataServer.Source) -> str:
    ext = source.get_extension(EDataServer.SOURCE_EXTENSION_CALENDAR)
    return ext.dup_color() or DEFAULT_DOT_HEX


def month_grid(year: int, month: int) -> list[datetime.date]:
    """Every date shown on the grid for `year`/`month`, Monday-first, in
    reading order - always exactly 7 * WEEKS_SHOWN days (padding with
    extra trailing days from next month on a short month) so the widget's
    layout never reflows between a 4-week and a 6-week month."""
    weeks = calendar.Calendar(firstweekday=0).monthdatescalendar(year, month)
    while len(weeks) < WEEKS_SHOWN:
        next_day = weeks[-1][-1] + datetime.timedelta(days=1)
        weeks.append([next_day + datetime.timedelta(days=i) for i in range(7)])
    return [day for week in weeks[:WEEKS_SHOWN] for day in week]


def _fetch_month_events(selected_uids: set[str], start_date: datetime.date, end_date: datetime.date) -> dict:
    """Runs on a background thread. Connects to each selected, currently
    enabled calendar and collects, for every date between `start_date` and
    `end_date` (inclusive), every event instance that day - used both to
    mark which days get a colored bar (see _apply_marks) and to fill in
    the hover tooltip / click popover for a given day (see
    AgendaContent._on_cell_clicked). A source that fails to connect
    (account disabled, offline, revoked token...) is skipped rather than
    failing the whole fetch - a dashboard widget missing one calendar's
    events out of several is still far more useful than a blank one."""
    start = int(datetime.datetime.combine(start_date, datetime.time.min).timestamp())
    end = int(datetime.datetime.combine(end_date + datetime.timedelta(days=1), datetime.time.min).timestamp())

    events_by_date: dict[datetime.date, list[dict]] = {}
    for source in list_calendar_sources():
        if source.get_uid() not in selected_uids or not source.get_enabled():
            continue
        try:
            client = ECal.Client.connect_sync(source, ECal.ClientSourceType.EVENTS, CONNECT_TIMEOUT_SECONDS, None)
        except GLib.Error:
            continue

        color = source_color(source)

        def collect(icalcomp, instance_start, _instance_end, _user_data, _cancellable, _color=color):
            day = datetime.datetime.fromtimestamp(instance_start.as_timet_with_zone(None)).date()
            events_by_date.setdefault(day, []).append(
                {
                    "start": instance_start.as_timet_with_zone(None),
                    "all_day": bool(instance_start.is_date()),
                    "summary": icalcomp.get_summary() or "",
                    "color": _color,
                }
            )
            return True

        try:
            client.generate_instances_sync(start, end, None, collect, None)
        except GLib.Error:
            continue

    for day_events in events_by_date.values():
        day_events.sort(key=lambda event: event["start"])
    return events_by_date


class AgendaContent(Gtk.Box):
    """The agenda widget's display: today's weekday/date spelled out big on
    the left (its size adjustable via AgendaSettings' content-size slider,
    like WeatherContent), the current month's grid on the right at a fixed
    size - Monday-first, today circled, weekend day names/numbers in
    `weekend_color`, and a small colored bar under any day with at least
    one event on a calendar selected in AgendaSettings (all enabled ones by
    default, the first time this widget is added). The whole thing centers
    itself in the card rather than stretching edge to edge."""

    def __init__(self):
        super().__init__(orientation=Gtk.Orientation.HORIZONTAL, spacing=20)
        _ensure_static_css_installed()
        _ensure_scale_css_installed()
        self.set_hexpand(True)
        self.set_vexpand(True)
        self.set_valign(Gtk.Align.CENTER)

        AgendaContent._next_id = getattr(AgendaContent, "_next_id", 0) + 1
        self._css_class = f"xeneon-agenda-{AgendaContent._next_id}"
        self.add_css_class(self._css_class)

        # A freshly added widget has no saved selection yet, so it starts
        # from "every calendar enabled right now" rather than an empty,
        # bar-less grid - see set_selected_uids()/apply_dict().
        self.selected_uids: set[str] = {s.get_uid() for s in list_calendar_sources() if s.get_enabled()}
        self.weekend_color = _hex_to_rgba(DEFAULT_WEEKEND_HEX)
        self.content_scale = DEFAULT_CONTENT_SCALE
        self._fetch_generation = 0
        self._refresh_timeout_id: int | None = None
        self._events_by_date: dict[datetime.date, list[dict]] = {}

        self._left = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=BASE_LEFT_SPACING_PX)
        self._left.set_valign(Gtk.Align.CENTER)
        self._left.set_halign(Gtk.Align.CENTER)
        # Grows into whatever width is left over once the separator and the
        # (fixed-size, see `right` below) grid have taken theirs, instead of
        # the left/right sides being trimmed evenly off a centered block -
        # so the grid keeps a consistent margin on all four sides and the
        # date panel absorbs the rest, rather than that space sitting idle.
        self._left.set_hexpand(True)
        self._weekday_label = Gtk.Label()
        self._weekday_label.add_css_class("xeneon-agenda-weekday")
        self._weekday_label.set_halign(Gtk.Align.CENTER)
        self._left.append(self._weekday_label)
        self._daynum_label = Gtk.Label()
        self._daynum_label.add_css_class("xeneon-agenda-daynum")
        self._daynum_label.set_halign(Gtk.Align.CENTER)
        self._left.append(self._daynum_label)
        self._fulldate_label = Gtk.Label()
        self._fulldate_label.add_css_class("xeneon-agenda-fulldate")
        self._fulldate_label.set_halign(Gtk.Align.CENTER)
        self._left.append(self._fulldate_label)
        self.append(self._left)

        self.append(Gtk.Separator())

        right = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
        right.set_margin_end(GRID_RIGHT_MARGIN_PX)
        # Explicit (not just "happens to be False") so that number_label's
        # own hexpand/vexpand below - needed to center it inside the
        # "today" square, see _build_grid_skeleton - can't propagate up
        # through cell/grid and make `right` compete with `self._left` for
        # the outer box's leftover width (see _left.set_hexpand(True)).
        right.set_hexpand(False)
        self._grid = Gtk.Grid()
        self._grid.set_hexpand(False)
        self._grid.set_column_homogeneous(True)
        self._grid.set_row_homogeneous(True)
        self._grid.set_column_spacing(29)
        self._grid.set_row_spacing(8)
        right.append(self._grid)
        self.append(right)

        self._cells: list[tuple[Gtk.Widget, Gtk.Label, Gtk.Box]] = []
        self._build_grid_skeleton()
        self._apply_content_scale()
        self._refresh()
        self._refresh_timeout_id = GLib.timeout_add_seconds(REFRESH_INTERVAL_SECONDS, self._on_refresh_tick)
        self.connect("destroy", self._on_destroy)
        i18n.on_change(self._retranslate)

    def _build_grid_skeleton(self):
        """Builds the 7x(1+WEEKS_SHOWN) grid of widgets once - a weekday
        header row, then one cell per day. Refreshing later only updates
        these same labels/bars in place (see _apply_static_labels/
        _apply_marks) rather than tearing the grid down and rebuilding it,
        so the layout never flashes empty on a periodic refresh."""
        self._header_labels: list[Gtk.Label] = []
        for col in range(7):
            label = Gtk.Label()
            label.add_css_class("xeneon-agenda-header-cell")
            label.set_halign(Gtk.Align.CENTER)
            self._grid.attach(label, col, 0, 1, 1)
            self._header_labels.append(label)

        for row in range(WEEKS_SHOWN):
            for col in range(7):
                cell = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=0)
                cell.set_halign(Gtk.Align.CENTER)
                cell.set_valign(Gtk.Align.CENTER)

                number_wrap = Gtk.Box()
                number_wrap.set_halign(Gtk.Align.CENTER)
                number_wrap.set_valign(Gtk.Align.CENTER)
                # Clearance above only, so the "today" background (see
                # xeneon-agenda-today) never touches the row above -
                # row_homogeneous sizes every row off the tallest one, and
                # without this the today square was the tallest thing in
                # its own row with nothing forcing extra room above it.
                # Nothing below: the event bar (bars_row) sits right under
                # the number on purpose.
                number_wrap.set_margin_top(3)
                number_label = Gtk.Label()
                number_label.add_css_class("xeneon-agenda-cell-num")
                # Without this, the label just sits top-left once the
                # "today" class (see xeneon-agenda-today) forces number_wrap
                # bigger than the label's own natural size - GtkBox doesn't
                # center a non-expanding child in leftover space on its own.
                number_label.set_halign(Gtk.Align.CENTER)
                number_label.set_valign(Gtk.Align.CENTER)
                # GtkBox only honors a child's halign/valign for centering
                # within *extra granted space* - without also expanding,
                # the child's own "cell" in the box is exactly its natural
                # size (no slack for CENTER to do anything), which is why
                # the label stayed pinned to one corner of the enlarged
                # "today" square despite the alignment above.
                number_label.set_hexpand(True)
                number_label.set_vexpand(True)
                number_wrap.append(number_label)
                cell.append(number_wrap)

                bars_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=3)
                bars_row.set_halign(Gtk.Align.CENTER)
                bars_row.set_size_request(-1, 4)
                cell.append(bars_row)

                # Click shows that day's events in a popup - see
                # _on_cell_clicked(). Hover shows the same thing via the
                # cell's native tooltip instead (see _apply_marks), which
                # needs no gesture of its own. Both stay silent for a day
                # with no events (see _apply_marks) rather than popping up
                # empty.
                popover = Gtk.Popover()
                popover.set_parent(cell)
                popover.set_autohide(True)
                popover_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
                popover_box.set_margin_top(8)
                popover_box.set_margin_bottom(8)
                popover_box.set_margin_start(10)
                popover_box.set_margin_end(10)
                popover.set_child(popover_box)

                index = len(self._cells)
                click = Gtk.GestureClick()
                click.connect("released", lambda _g, _n, _x, _y, _index=index: self._on_cell_clicked(_index))
                cell.add_controller(click)

                self._grid.attach(cell, col, row + 1, 1, 1)
                self._cells.append(_CellWidgets(cell, number_wrap, number_label, bars_row, popover, popover_box))

    def _on_destroy(self, *_args):
        # Bumps the generation so a response still in flight when the
        # widget is destroyed becomes a no-op in _on_month_events_fetched instead
        # of touching already-destroyed GTK widgets - same pattern as
        # weather.py's WeatherContent.
        self._fetch_generation += 1
        if self._refresh_timeout_id is not None:
            GLib.source_remove(self._refresh_timeout_id)
            self._refresh_timeout_id = None
        _scale_rules.pop(self._css_class, None)
        _reload_scale_css()

    def _on_refresh_tick(self) -> bool:
        self._refresh()
        return GLib.SOURCE_CONTINUE

    def set_selected_uids(self, uids: set[str]):
        self.selected_uids = uids
        self._refresh()

    def set_weekend_color(self, rgba: Gdk.RGBA):
        self.weekend_color = rgba
        self._apply_static_labels(datetime.date.today())

    def set_content_scale(self, scale: float):
        self.content_scale = max(MIN_CONTENT_SCALE, min(MAX_CONTENT_SCALE, scale))
        self._apply_content_scale()

    def _apply_content_scale(self):
        scale = self.content_scale
        _scale_rules[self._css_class] = (
            f".{self._css_class} .xeneon-agenda-weekday {{ font-size: {round(BASE_WEEKDAY_FONT_PX * scale)}px; }}"
            f".{self._css_class} .xeneon-agenda-daynum {{ font-size: {round(BASE_DAYNUM_FONT_PX * scale)}px; }}"
            f".{self._css_class} .xeneon-agenda-fulldate {{ font-size: {round(BASE_FULLDATE_FONT_PX * scale)}px; }}"
        )
        _reload_scale_css()
        self._left.set_spacing(round(BASE_LEFT_SPACING_PX * scale))

    def _refresh(self):
        today = datetime.date.today()
        self._apply_static_labels(today)
        self._fetch_month_events()

    def _set_label_color(self, label: Gtk.Label, text: str, color_hex: str | None):
        if color_hex is None:
            label.set_label(text)
        else:
            label.set_markup(f'<span foreground="{color_hex}">{GLib.markup_escape_text(text)}</span>')

    def _apply_static_labels(self, today: datetime.date):
        self._weekday_label.set_label(i18n._(f"widgets.agenda.days_long.{DAY_KEYS[today.weekday()]}"))
        self._daynum_label.set_label(str(today.day))
        month_name = i18n._(f"widgets.agenda.months_long.{MONTH_KEYS[today.month - 1]}")
        self._fulldate_label.set_label(i18n._("widgets.agenda.date_full", month=month_name, year=today.year))

        weekend_hex = _rgba_to_hex(self.weekend_color)
        for col, label in enumerate(self._header_labels):
            text = i18n._(f"widgets.agenda.days_short.{DAY_KEYS[col]}")
            self._set_label_color(label, text, weekend_hex if col in WEEKEND_COLS else None)

        days = month_grid(today.year, today.month)
        self._grid_days = days
        for index, (cell_widgets, day) in enumerate(zip(self._cells, days)):
            col = index % 7
            is_today = day == today
            # Today's white square (see xeneon-agenda-today) needs dark
            # text regardless of weekend coloring - checked first so it
            # always wins over the weekend color for that one cell.
            color = TODAY_TEXT_HEX if is_today else (weekend_hex if col in WEEKEND_COLS else None)
            self._set_label_color(cell_widgets.number_label, str(day.day), color)
            in_month = day.month == today.month
            cell_widgets.number_label.remove_css_class("dim")
            if not in_month:
                cell_widgets.number_label.add_css_class("dim")
            if is_today:
                cell_widgets.number_wrap.add_css_class("xeneon-agenda-today")
            else:
                cell_widgets.number_wrap.remove_css_class("xeneon-agenda-today")

    def _fetch_month_events(self):
        self._fetch_generation += 1
        generation = self._fetch_generation
        selected_uids = set(self.selected_uids)
        days = self._grid_days
        start_date, end_date = days[0], days[-1]

        def worker():
            events_by_date = _fetch_month_events(selected_uids, start_date, end_date)
            GLib.idle_add(self._on_month_events_fetched, generation, events_by_date)

        threading.Thread(target=worker, daemon=True).start()

    def _on_month_events_fetched(self, generation: int, events_by_date: dict) -> bool:
        if generation != self._fetch_generation:
            return GLib.SOURCE_REMOVE
        self._events_by_date = events_by_date
        self._apply_marks(events_by_date)
        return GLib.SOURCE_REMOVE

    def _apply_marks(self, events_by_date: dict):
        for cell_widgets, day in zip(self._cells, self._grid_days):
            bars_row = cell_widgets.bars_row
            child = bars_row.get_first_child()
            while child is not None:
                next_child = child.get_next_sibling()
                bars_row.remove(child)
                child = next_child

            day_events = events_by_date.get(day, [])
            colors = list(dict.fromkeys(event["color"] for event in day_events))
            for color in colors[:3]:
                bar = Gtk.Box()
                bar.add_css_class("xeneon-agenda-bar")
                css = Gtk.CssProvider()
                css.load_from_string(f"box {{ background-color: {color}; }}")
                bar.get_style_context().add_provider(css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
                bars_row.append(bar)

            # Hover (native GTK tooltip) - stays unset for a day with no
            # events, so nothing shows on hover either (see also
            # _on_cell_clicked for the click path).
            cell_widgets.cell.set_tooltip_text(self._events_tooltip_text(day_events) if day_events else None)
            self._populate_popover(cell_widgets.popover_box, day_events)

    def _format_event_when(self, event: dict) -> str:
        if event["all_day"]:
            return i18n._("widgets.agenda.all_day")
        moment = datetime.datetime.fromtimestamp(event["start"])
        return f"{moment.hour:02d}:{moment.minute:02d}"

    def _events_tooltip_text(self, day_events: list[dict]) -> str:
        return "\n".join(f"{self._format_event_when(event)} · {event['summary']}" for event in day_events)

    def _populate_popover(self, popover_box: Gtk.Box, day_events: list[dict]):
        child = popover_box.get_first_child()
        while child is not None:
            next_child = child.get_next_sibling()
            popover_box.remove(child)
            child = next_child
        for event in day_events:
            row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
            dot = Gtk.Box()
            dot.add_css_class("xeneon-agenda-bar")
            dot.set_size_request(10, 10)
            css = Gtk.CssProvider()
            css.load_from_string(f"box {{ background-color: {event['color']}; border-radius: 5px; }}")
            dot.get_style_context().add_provider(css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
            dot.set_valign(Gtk.Align.START)
            dot.set_margin_top(5)
            row.append(dot)

            column = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2)
            when_label = Gtk.Label(label=self._format_event_when(event))
            when_label.add_css_class("dim-label")
            when_label.set_halign(Gtk.Align.START)
            column.append(when_label)
            summary_label = Gtk.Label(label=event["summary"])
            summary_label.set_halign(Gtk.Align.START)
            summary_label.set_wrap(True)
            summary_label.set_xalign(0)
            column.append(summary_label)
            row.append(column)

            popover_box.append(row)

    def _on_cell_clicked(self, index: int):
        day = self._grid_days[index]
        cell_widgets = self._cells[index]
        if not self._events_by_date.get(day):
            return
        cell_widgets.popover.popup()

    def _retranslate(self):
        self._apply_static_labels(datetime.date.today())

    def to_dict(self) -> dict:
        return {
            "selected_uids": sorted(self.selected_uids),
            "weekend_color": _rgba_to_hex(self.weekend_color),
            "content_scale": self.content_scale,
        }

    def apply_dict(self, data: dict) -> None:
        """Restores a state previously returned by to_dict(). Only touches
        fields actually present, so a partial/older dict still applies
        cleanly."""
        if not data:
            return
        if "selected_uids" in data:
            self.set_selected_uids(set(data["selected_uids"]))
        if "weekend_color" in data:
            self.set_weekend_color(_hex_to_rgba(data["weekend_color"]))
        if "content_scale" in data:
            self.set_content_scale(data["content_scale"])


class AgendaSettings(Gtk.Box):
    """The agenda widget's own settings, shown to the right of the generic
    appearance controls in the same configure popover (see ClockSettings
    in widgets/clock.py for the reference layout this follows): one
    checkbox per calendar EDS currently knows about (from GNOME Online
    Accounts or added locally), the weekend day color, and a content-size
    slider for the left date panel (see WeatherSettings' own scale slider
    in widgets/weather.py for the reference this follows)."""

    def __init__(self, content: AgendaContent):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self._content = content
        self.set_size_request(280, -1)

        self._calendars_label = Gtk.Label()
        self._calendars_label.set_halign(Gtk.Align.START)
        self.append(self._calendars_label)

        scroller = Gtk.ScrolledWindow()
        scroller.set_min_content_height(180)
        scroller.set_max_content_height(180)
        scroller.set_vexpand(False)
        scroller.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        self._sources_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        scroller.set_child(self._sources_box)
        self.append(scroller)

        self._no_calendars_label = Gtk.Label()
        self._no_calendars_label.add_css_class("dim-label")
        self._no_calendars_label.set_wrap(True)
        self._no_calendars_label.set_visible(False)
        self.append(self._no_calendars_label)

        self._checkbuttons: list[tuple[Gtk.CheckButton, str]] = []
        self._rebuild_sources()

        self.append(Gtk.Separator())

        self._weekend_label = Gtk.Label()
        self._weekend_label.set_hexpand(True)
        self._weekend_label.set_halign(Gtk.Align.START)
        self._weekend_color_button = Gtk.ColorDialogButton.new(Gtk.ColorDialog.new())
        self._weekend_color_button.set_rgba(content.weekend_color)
        self._weekend_color_button.connect("notify::rgba", self._on_weekend_color_changed)
        weekend_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        weekend_row.append(self._weekend_label)
        weekend_row.append(self._weekend_color_button)
        self.append(weekend_row)

        self.append(Gtk.Separator())

        self._scale_label = Gtk.Label()
        self._scale_label.set_halign(Gtk.Align.START)
        self.append(self._scale_label)
        self._scale_slider = Gtk.Scale(orientation=Gtk.Orientation.HORIZONTAL)
        self._scale_slider.set_range(round(MIN_CONTENT_SCALE * 100), round(MAX_CONTENT_SCALE * 100))
        self._scale_slider.set_value(content.content_scale * 100)
        self._scale_slider.set_draw_value(True)
        self._scale_slider.set_value_pos(Gtk.PositionType.RIGHT)
        self._scale_slider.connect("value-changed", self._on_scale_changed)
        self.append(self._scale_slider)

        self._retranslate()
        i18n.on_change(self._retranslate)

    def _rebuild_sources(self):
        child = self._sources_box.get_first_child()
        while child is not None:
            next_child = child.get_next_sibling()
            self._sources_box.remove(child)
            child = next_child
        self._checkbuttons.clear()

        sources = list_calendar_sources()
        self._no_calendars_label.set_visible(not sources)
        for source in sources:
            uid = source.get_uid()
            row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
            dot = Gtk.Box()
            dot.add_css_class("xeneon-agenda-bar")
            dot.set_size_request(10, 10)
            css = Gtk.CssProvider()
            css.load_from_string(f"box {{ background-color: {source_color(source)}; border-radius: 5px; }}")
            dot.get_style_context().add_provider(css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
            row.append(dot)
            check = Gtk.CheckButton(label=source.get_display_name())
            check.set_active(uid in self._content.selected_uids)
            check.set_sensitive(source.get_enabled())
            check.connect("toggled", self._on_source_toggled)
            row.append(check)
            self._sources_box.append(row)
            self._checkbuttons.append((check, uid))

    def _on_source_toggled(self, _check):
        selected = {uid for check, uid in self._checkbuttons if check.get_active()}
        self._content.set_selected_uids(selected)

    def _on_weekend_color_changed(self, button, _pspec):
        self._content.set_weekend_color(button.get_rgba())

    def _on_scale_changed(self, scale):
        self._content.set_content_scale(scale.get_value() / 100)

    def sync_from_content(self):
        """Re-reads every control's displayed value from self._content -
        see ClockSettings.sync_from_content() (widgets/clock.py) for why
        this exists."""
        self._rebuild_sources()
        self._weekend_color_button.set_rgba(self._content.weekend_color)
        self._scale_slider.set_value(self._content.content_scale * 100)

    def _retranslate(self):
        self._calendars_label.set_label(i18n._("widgets.agenda.settings.calendars"))
        self._no_calendars_label.set_label(i18n._("widgets.agenda.settings.no_calendars"))
        self._weekend_label.set_label(i18n._("widgets.agenda.settings.weekend_color"))
        self._scale_label.set_label(i18n._("widgets.agenda.settings.content_scale"))
