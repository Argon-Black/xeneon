"""Weather widget: current conditions (icon, temperature, condition text)
for one location picked in WeatherSettings, refreshed periodically from
Open-Meteo (https://open-meteo.com - free, no API key). Two separate
endpoints are used: geocoding (turns a typed city name into
latitude/longitude, see WeatherSettings._on_search) and forecast (turns a
lat/lon into current conditions, see WeatherContent._fetch_weather).

Both are plain HTTP GET requests run on a background thread (urllib, not
requests - there's no other network use in this app, so pulling in a
dependency for one file isn't worth it) and marshalled back to the main
thread via GLib.idle_add, since touching GTK widgets off the main thread
isn't safe. Each request carries its own generation counter (bumped by the
next request, checked when the response comes back) so a slow, stale
response - from before the user picked a different city, flipped the unit,
or closed the widget entirely - never overwrites what's currently shown."""

import json
import threading
import urllib.parse
import urllib.request

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("Gdk", "4.0")
from gi.repository import Adw, Gdk, GLib, Gtk

from xeneon_dashboard import i18n

GEOCODING_URL = "https://geocoding-api.open-meteo.com/v1/search"
FORECAST_URL = "https://api.open-meteo.com/v1/forecast"
REQUEST_TIMEOUT_SECONDS = 8
REFRESH_INTERVAL_SECONDS = 15 * 60

# Shown before the user ever picks a location, so a freshly added widget
# displays real weather instead of a blank placeholder.
DEFAULT_LOCATION = {
    "name": "Paris",
    "admin1": "Île-de-France",
    "country": "France",
    "latitude": 48.8566,
    "longitude": 2.3522,
}

# WMO weather code (Open-Meteo's "current.weather_code") -> (i18n condition
# key suffix, day icon, night icon). Grouped by how the icon actually reads
# at a glance, not one entry per WMO code - several adjacent codes (e.g. 61
# "slight rain" / 63 "moderate rain") share the same icon and are only
# distinguished by their condition text.
_WEATHER_CODES = {
    0: ("clear", "weather-clear-symbolic", "weather-clear-night-symbolic"),
    1: ("mainly_clear", "weather-few-clouds-symbolic", "weather-few-clouds-night-symbolic"),
    2: ("partly_cloudy", "weather-few-clouds-symbolic", "weather-few-clouds-night-symbolic"),
    3: ("overcast", "weather-overcast-symbolic", "weather-overcast-symbolic"),
    45: ("fog", "weather-fog-symbolic", "weather-fog-symbolic"),
    48: ("fog", "weather-fog-symbolic", "weather-fog-symbolic"),
    51: ("drizzle", "weather-showers-scattered-symbolic", "weather-showers-scattered-symbolic"),
    53: ("drizzle", "weather-showers-scattered-symbolic", "weather-showers-scattered-symbolic"),
    55: ("drizzle", "weather-showers-scattered-symbolic", "weather-showers-scattered-symbolic"),
    56: ("freezing_drizzle", "weather-showers-scattered-symbolic", "weather-showers-scattered-symbolic"),
    57: ("freezing_drizzle", "weather-showers-scattered-symbolic", "weather-showers-scattered-symbolic"),
    61: ("rain", "weather-showers-symbolic", "weather-showers-symbolic"),
    63: ("rain", "weather-showers-symbolic", "weather-showers-symbolic"),
    65: ("heavy_rain", "weather-showers-symbolic", "weather-showers-symbolic"),
    66: ("freezing_rain", "weather-showers-symbolic", "weather-showers-symbolic"),
    67: ("freezing_rain", "weather-showers-symbolic", "weather-showers-symbolic"),
    71: ("snow", "weather-snow-symbolic", "weather-snow-symbolic"),
    73: ("snow", "weather-snow-symbolic", "weather-snow-symbolic"),
    75: ("heavy_snow", "weather-snow-symbolic", "weather-snow-symbolic"),
    77: ("snow_grains", "weather-snow-symbolic", "weather-snow-symbolic"),
    80: ("rain_showers", "weather-showers-symbolic", "weather-showers-symbolic"),
    81: ("rain_showers", "weather-showers-symbolic", "weather-showers-symbolic"),
    82: ("heavy_rain_showers", "weather-showers-symbolic", "weather-showers-symbolic"),
    85: ("snow_showers", "weather-snow-symbolic", "weather-snow-symbolic"),
    86: ("snow_showers", "weather-snow-symbolic", "weather-snow-symbolic"),
    95: ("thunderstorm", "weather-storm-symbolic", "weather-storm-symbolic"),
    96: ("thunderstorm_hail", "weather-storm-symbolic", "weather-storm-symbolic"),
    99: ("thunderstorm_hail", "weather-storm-symbolic", "weather-storm-symbolic"),
}
_UNKNOWN_CODE = ("unknown", "weather-severe-alert-symbolic", "weather-severe-alert-symbolic")

# Every size in the layout at content_scale == 1.0 (100%, see
# WeatherContent.set_content_scale) - font sizes, icon pixel sizes, and the
# spacing between elements all scale off these together, so "bigger" grows
# the whole layout in proportion instead of just the text.
BASE_TEMP_FONT_PX = 72
BASE_CITY_FONT_PX = 18
BASE_STAT_FONT_PX = 15
BASE_ICON_PX = 104
BASE_STAT_ICON_PX = 18
BASE_COLUMN_SPACING = 32
BASE_TEMP_COLUMN_SPACING = 4
BASE_ICON_COLUMN_SPACING = 8
BASE_STATS_COLUMN_SPACING = 8
BASE_STAT_ROW_SPACING = 4

MIN_CONTENT_SCALE = 0.5
MAX_CONTENT_SCALE = 2.0
DEFAULT_CONTENT_SCALE = 1.75

# One shared provider for every instance's own scale-dependent font rule,
# keyed by its unique css class - same pattern as WidgetAppearance
# (widget_appearance.py) and ShortcutsContent's backdrop (shortcuts.py) -
# so scaling one weather widget never touches another one's sizing.
_provider = Gtk.CssProvider()
_installed = False
_rules: dict[str, str] = {}


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


def _http_get_json(url: str, params: dict) -> dict:
    query = urllib.parse.urlencode(params)
    request = urllib.request.Request(f"{url}?{query}", headers={"User-Agent": "xeneon-dashboard"})
    with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT_SECONDS) as response:
        return json.loads(response.read().decode("utf-8"))


def format_location(location: dict) -> str:
    """"City, Region, Country" - skips a region that just repeats the city
    name (common for big cities in the geocoding API's results) and any
    part that's missing, so it degrades gracefully for an older saved
    location that only has some of these fields."""
    parts = [location.get("name", "")]
    admin1 = location.get("admin1")
    if admin1 and admin1 != location.get("name"):
        parts.append(admin1)
    country = location.get("country")
    if country:
        parts.append(country)
    return ", ".join(part for part in parts if part)


class WeatherContent(Gtk.Box):
    """The weather widget's display, laid out as three centered columns:
    temperature (large, bold) with the city name below it; the condition
    icon (large) with wind speed below it; and humidity/pressure/UV index
    stacked as small icon+value rows. The condition text itself (e.g.
    "Partly cloudy") isn't shown as its own label - the icon already
    conveys it, and repeating it as text crowded the layout - but it's
    still reachable as the icon's tooltip. Location and unit are
    live-editable through WeatherSettings, which holds a reference to this
    instance. content_scale (see set_content_scale) grows or shrinks every
    font, icon and gap together, not a CSS transform: scale() - a real
    zoom would blur at fractional scales and wouldn't reflow the layout,
    while this keeps everything crisp and lets the columns actually
    spread out or tighten up as the size changes."""

    _next_id = 0

    def __init__(self):
        super().__init__(orientation=Gtk.Orientation.HORIZONTAL)
        _ensure_css_installed()
        WeatherContent._next_id += 1
        self._css_class = f"xeneon-weather-{WeatherContent._next_id}"
        self.add_css_class(self._css_class)
        self.set_halign(Gtk.Align.CENTER)
        self.set_valign(Gtk.Align.CENTER)

        self.location = dict(DEFAULT_LOCATION)
        self.unit_fahrenheit = False
        self.content_scale = DEFAULT_CONTENT_SCALE
        self._status = "loading"  # "loading" | "ok" | "error"
        self._current_celsius: float | None = None
        self._weather_code: int | None = None
        self._is_day = 1
        self._humidity: float | None = None
        self._pressure: float | None = None
        self._wind_speed: float | None = None
        self._uv_index: float | None = None
        self._fetch_generation = 0
        self._refresh_timeout_id: int | None = None

        self._temp_label = Gtk.Label()
        self._temp_label.add_css_class("xeneon-weather-temp")
        self._city_label = Gtk.Label()
        self._city_label.add_css_class("xeneon-weather-city")
        self._temp_column = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        self._temp_column.set_valign(Gtk.Align.CENTER)
        self._temp_column.append(self._temp_label)
        self._temp_column.append(self._city_label)
        self.append(self._temp_column)

        self._icon = Gtk.Image()
        self._wind_row, self._wind_icon, self._wind_label = self._make_stat_row("weather-windy-symbolic")
        self._icon_column = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        self._icon_column.set_halign(Gtk.Align.CENTER)
        self._icon_column.set_valign(Gtk.Align.CENTER)
        self._icon_column.append(self._icon)
        self._icon_column.append(self._wind_row)
        self.append(self._icon_column)

        self._humidity_row, self._humidity_icon, self._humidity_label = self._make_stat_row("weather-showers-symbolic")
        self._pressure_row, self._pressure_icon, self._pressure_label = self._make_stat_row("speedometer-symbolic")
        self._uv_row, self._uv_icon, self._uv_label = self._make_stat_row("brightness-high-symbolic")
        self._stats_column = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        self._stats_column.set_valign(Gtk.Align.CENTER)
        self._stats_column.append(self._humidity_row)
        self._stats_column.append(self._pressure_row)
        self._stats_column.append(self._uv_row)
        self.append(self._stats_column)

        self._refresh_location_label()
        self._apply_content_scale()
        self._render()
        self._fetch_weather()
        self._refresh_timeout_id = GLib.timeout_add_seconds(REFRESH_INTERVAL_SECONDS, self._on_refresh_tick)

        self.connect("destroy", self._on_destroy)
        i18n.on_change(self._retranslate)

    def _make_stat_row(self, icon_name: str) -> tuple[Gtk.Box, Gtk.Image, Gtk.Label]:
        """One small icon + value pair (humidity/pressure/wind/UV index) -
        unlike the big condition icon, this icon's name never changes,
        only its size (see _apply_content_scale) and the label next to it
        do."""
        row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
        row.set_halign(Gtk.Align.START)
        icon = Gtk.Image.new_from_icon_name(icon_name)
        row.append(icon)
        label = Gtk.Label()
        label.add_css_class("xeneon-weather-stat")
        row.append(label)
        return row, icon, label

    def set_content_scale(self, scale: float):
        self.content_scale = max(MIN_CONTENT_SCALE, min(MAX_CONTENT_SCALE, scale))
        self._apply_content_scale()

    def _apply_content_scale(self):
        scale = self.content_scale
        _rules[self._css_class] = (
            f".{self._css_class} .xeneon-weather-temp {{"
            f" font-size: {round(BASE_TEMP_FONT_PX * scale)}px; font-weight: 700; color: #ffffff; }}"
            f".{self._css_class} .xeneon-weather-city {{"
            f" font-size: {round(BASE_CITY_FONT_PX * scale)}px; color: #ffffff; }}"
            f".{self._css_class} .xeneon-weather-stat {{"
            f" font-size: {round(BASE_STAT_FONT_PX * scale)}px; color: #ffffff; }}"
        )
        _reload_css()

        self._icon.set_pixel_size(round(BASE_ICON_PX * scale))
        stat_icon_px = round(BASE_STAT_ICON_PX * scale)
        for icon in (self._wind_icon, self._humidity_icon, self._pressure_icon, self._uv_icon):
            icon.set_pixel_size(stat_icon_px)

        self.set_spacing(round(BASE_COLUMN_SPACING * scale))
        self._temp_column.set_spacing(round(BASE_TEMP_COLUMN_SPACING * scale))
        self._icon_column.set_spacing(round(BASE_ICON_COLUMN_SPACING * scale))
        self._stats_column.set_spacing(round(BASE_STATS_COLUMN_SPACING * scale))
        for row in (self._wind_row, self._humidity_row, self._pressure_row, self._uv_row):
            row.set_spacing(round(BASE_STAT_ROW_SPACING * scale))

    def _on_destroy(self, *_args):
        # Bumping the generation makes any response still in flight a no-op
        # once it reaches _on_weather_fetched (see that method) - without
        # this, a slow reply arriving after the widget is gone would touch
        # already-destroyed GTK widgets.
        self._fetch_generation += 1
        if self._refresh_timeout_id is not None:
            GLib.source_remove(self._refresh_timeout_id)
            self._refresh_timeout_id = None

    def _on_refresh_tick(self) -> bool:
        self._fetch_weather()
        return GLib.SOURCE_CONTINUE

    def set_location(self, location: dict):
        self.location = location
        self._refresh_location_label()
        self._status = "loading"
        self._render()
        self._fetch_weather()

    def set_unit_fahrenheit(self, enabled: bool):
        self.unit_fahrenheit = enabled
        self._render()

    def _refresh_location_label(self):
        self._city_label.set_text(self.location.get("name", ""))

    def _fetch_weather(self):
        self._fetch_generation += 1
        generation = self._fetch_generation
        latitude = self.location.get("latitude")
        longitude = self.location.get("longitude")

        def worker():
            try:
                data = _http_get_json(
                    FORECAST_URL,
                    {
                        "latitude": latitude,
                        "longitude": longitude,
                        "current": "temperature_2m,weather_code,is_day,relative_humidity_2m,"
                        "surface_pressure,wind_speed_10m,uv_index",
                        "timezone": "auto",
                    },
                )
                GLib.idle_add(self._on_weather_fetched, generation, data.get("current"), None)
            except (OSError, ValueError) as exc:
                GLib.idle_add(self._on_weather_fetched, generation, None, exc)

        threading.Thread(target=worker, daemon=True).start()

    def _on_weather_fetched(self, generation: int, current: dict | None, error) -> bool:
        if generation != self._fetch_generation:
            return GLib.SOURCE_REMOVE
        if error is not None or not current or current.get("temperature_2m") is None:
            self._status = "error"
        else:
            self._status = "ok"
            self._current_celsius = current.get("temperature_2m")
            self._weather_code = current.get("weather_code")
            self._is_day = current.get("is_day", 1)
            self._humidity = current.get("relative_humidity_2m")
            self._pressure = current.get("surface_pressure")
            self._wind_speed = current.get("wind_speed_10m")
            self._uv_index = current.get("uv_index")
        self._render()
        return GLib.SOURCE_REMOVE

    def _render(self):
        if self._status == "ok" and self._current_celsius is not None:
            value = self._current_celsius * 9 / 5 + 32 if self.unit_fahrenheit else self._current_celsius
            unit_symbol = "°F" if self.unit_fahrenheit else "°C"
            self._temp_label.set_text(f"{round(value)}{unit_symbol}")
            condition_key, day_icon, night_icon = _WEATHER_CODES.get(self._weather_code, _UNKNOWN_CODE)
            self._icon.set_from_icon_name(day_icon if self._is_day else night_icon)
            self._icon.set_tooltip_text(i18n._(f"widgets.weather.conditions.{condition_key}"))
            self._humidity_label.set_text(f"{round(self._humidity)}%" if self._humidity is not None else "--%")
            self._pressure_label.set_text(f"{round(self._pressure)} hPa" if self._pressure is not None else "-- hPa")
            self._wind_label.set_text(f"{round(self._wind_speed)} km/h" if self._wind_speed is not None else "-- km/h")
            self._uv_label.set_text(f"{round(self._uv_index)}" if self._uv_index is not None else "--")
            return

        tooltip_key = "widgets.weather.error" if self._status == "error" else "widgets.weather.loading"
        icon_name = "weather-severe-alert-symbolic" if self._status == "error" else "weather-clear-symbolic"
        self._temp_label.set_text("--°")
        self._icon.set_from_icon_name(icon_name)
        self._icon.set_tooltip_text(i18n._(tooltip_key))
        self._humidity_label.set_text("--%")
        self._pressure_label.set_text("-- hPa")
        self._wind_label.set_text("-- km/h")
        self._uv_label.set_text("--")

    def _retranslate(self):
        self._render()

    def to_dict(self) -> dict:
        return {
            "location": self.location,
            "unit_fahrenheit": self.unit_fahrenheit,
            "content_scale": self.content_scale,
        }

    def apply_dict(self, data: dict) -> None:
        """Restores a state previously returned by to_dict(). Only touches
        fields actually present, so a partial/older dict still applies
        cleanly."""
        if not data:
            return
        location = data.get("location")
        if location and location.get("latitude") is not None and location.get("longitude") is not None:
            self.set_location(location)
        if "unit_fahrenheit" in data:
            self.set_unit_fahrenheit(data["unit_fahrenheit"])
        if "content_scale" in data:
            self.set_content_scale(data["content_scale"])


class WeatherSettings(Gtk.Box):
    """The weather widget's own settings, shown to the right of the
    generic appearance controls in the same configure popover (see
    ClockSettings in widgets/clock.py for the reference layout this
    follows): a free-text city search (queries Open-Meteo's geocoding API,
    see WeatherContent's module docstring), a °C/°F unit toggle, and a
    content size slider (see WeatherContent.set_content_scale)."""

    def __init__(self, content: WeatherContent):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self._content = content
        self._search_generation = 0
        self.set_size_request(280, -1)

        self._location_title = Gtk.Label()
        self._location_title.set_halign(Gtk.Align.START)
        self.append(self._location_title)

        self._current_location_label = Gtk.Label()
        self._current_location_label.set_halign(Gtk.Align.START)
        self._current_location_label.set_wrap(True)
        self._current_location_label.add_css_class("dim-label")
        self.append(self._current_location_label)

        search_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        self._search_entry = Gtk.SearchEntry()
        self._search_entry.set_hexpand(True)
        self._search_entry.connect("activate", self._on_search)
        search_row.append(self._search_entry)
        self._search_button = Gtk.Button()
        self._search_button.connect("clicked", self._on_search)
        search_row.append(self._search_button)
        self.append(search_row)

        self._status_label = Gtk.Label()
        self._status_label.set_halign(Gtk.Align.START)
        self._status_label.add_css_class("dim-label")
        self._status_label.set_visible(False)
        self.append(self._status_label)

        scroller = Gtk.ScrolledWindow()
        scroller.set_min_content_height(160)
        scroller.set_max_content_height(160)
        scroller.set_vexpand(False)
        scroller.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        self._results_list = Gtk.ListBox()
        self._results_list.add_css_class("boxed-list")
        self._results_list.set_selection_mode(Gtk.SelectionMode.NONE)
        self._results_list.set_activate_on_single_click(True)
        self._results_list.connect("row-activated", self._on_result_activated)
        scroller.set_child(self._results_list)
        self.append(scroller)

        self.append(Gtk.Separator())

        self._unit_label = Gtk.Label()
        self._unit_label.set_hexpand(True)
        self._unit_label.set_halign(Gtk.Align.START)
        self._celsius_button = Gtk.ToggleButton()
        self._fahrenheit_button = Gtk.ToggleButton()
        self._fahrenheit_button.set_group(self._celsius_button)
        self._celsius_button.set_active(not content.unit_fahrenheit)
        self._fahrenheit_button.set_active(content.unit_fahrenheit)
        self._fahrenheit_button.connect("toggled", self._on_unit_toggled)
        unit_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        unit_row.append(self._unit_label)
        unit_row.append(self._celsius_button)
        unit_row.append(self._fahrenheit_button)
        self.append(unit_row)

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

        self._update_current_location_label()
        self._retranslate()
        i18n.on_change(self._retranslate)

    def sync_from_content(self):
        """Re-reads the controls from self._content - see
        ClockSettings.sync_from_content() (widgets/clock.py) for why this
        exists: needed after content is changed directly from outside
        these controls' own signal handlers."""
        self._celsius_button.set_active(not self._content.unit_fahrenheit)
        self._fahrenheit_button.set_active(self._content.unit_fahrenheit)
        self._scale_slider.set_value(self._content.content_scale * 100)
        self._update_current_location_label()

    def _update_current_location_label(self):
        self._current_location_label.set_text(format_location(self._content.location))

    def _clear_results(self):
        child = self._results_list.get_first_child()
        while child is not None:
            next_child = child.get_next_sibling()
            self._results_list.remove(child)
            child = next_child

    def _on_search(self, _widget):
        query = self._search_entry.get_text().strip()
        if not query:
            return
        self._clear_results()
        self._set_status(i18n._("widgets.weather.settings.searching"))
        self._search_generation += 1
        generation = self._search_generation

        def worker():
            try:
                data = _http_get_json(
                    GEOCODING_URL, {"name": query, "count": 8, "language": i18n.get_language(), "format": "json"}
                )
                GLib.idle_add(self._on_search_results, generation, data.get("results"), None)
            except (OSError, ValueError) as exc:
                GLib.idle_add(self._on_search_results, generation, None, exc)

        threading.Thread(target=worker, daemon=True).start()

    def _on_search_results(self, generation: int, results: list | None, error) -> bool:
        if generation != self._search_generation:
            return GLib.SOURCE_REMOVE
        if error is not None or not results:
            self._set_status(i18n._("widgets.weather.settings.no_results"))
            return GLib.SOURCE_REMOVE
        self._set_status(None)
        for result in results:
            location = {
                "name": result.get("name", ""),
                "admin1": result.get("admin1", ""),
                "country": result.get("country", ""),
                "latitude": result.get("latitude"),
                "longitude": result.get("longitude"),
            }
            row = Adw.ActionRow(title=GLib.markup_escape_text(format_location(location)), activatable=True)
            row.location = location
            self._results_list.append(row)
        return GLib.SOURCE_REMOVE

    def _on_result_activated(self, _listbox, row):
        location = getattr(row, "location", None)
        if location is None:
            return
        self._content.set_location(location)
        self._update_current_location_label()
        self._clear_results()
        self._search_entry.set_text("")
        self._set_status(None)

    def _set_status(self, text: str | None):
        if text is None:
            self._status_label.set_visible(False)
            return
        self._status_label.set_text(text)
        self._status_label.set_visible(True)

    def _on_unit_toggled(self, button):
        self._content.set_unit_fahrenheit(button.get_active())

    def _on_scale_changed(self, scale):
        self._content.set_content_scale(scale.get_value() / 100)

    def _retranslate(self):
        self._location_title.set_label(i18n._("widgets.weather.settings.location"))
        self._search_entry.set_placeholder_text(i18n._("widgets.weather.settings.search_placeholder"))
        self._search_button.set_label(i18n._("widgets.weather.settings.search_button"))
        self._unit_label.set_label(i18n._("widgets.weather.settings.unit"))
        self._celsius_button.set_label(i18n._("widgets.weather.settings.unit_celsius"))
        self._fahrenheit_button.set_label(i18n._("widgets.weather.settings.unit_fahrenheit"))
        self._scale_label.set_label(i18n._("widgets.weather.settings.content_scale"))
