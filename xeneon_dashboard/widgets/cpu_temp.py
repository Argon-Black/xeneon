"""System temperature widgets: two ways to show one hwmon sensor's current
temperature, refreshed every few seconds straight from the kernel's hwmon
sensors under /sys/class/hwmon/hwmon*/ (each directory's "name" file gives
the chip, "tempN_input" its millidegree-C readings, "tempN_label" an
optional human label) - the same interface the `sensors` CLI and psutil
both read, but a handful of plain file reads here isn't worth a whole
dependency for. Reading it is a fast local syscall, unlike the network
calls WeatherContent/AudioContent make, so both poll straight from the
GLib main loop instead of a worker thread.

CpuTempContent (SSX) is a plain "CPU 52°C" line; TempGaugeContent (SQ) is
the same sensor reading drawn as a round dial (Cairo, see its _on_draw).
They're independent Gtk widgets - each owns its own sensor pin/custom-label
state rather than sharing one - but both read through the same module-level
helpers below (_all_sensors, _auto_pick_sensor, sensor_display_name), so
the actual hwmon-parsing/auto-pick logic only exists once.

Both default to the CPU (auto-picked, see _auto_pick_sensor) since that's
what most people want to watch, but any hwmon sensor on the machine - GPU,
NVMe, motherboard... - can be pinned instead via their own Settings class.
Machines expose CPU temperature under very different hwmon chip names
(k10temp on AMD, coretemp on Intel, cpu_thermal on many ARM boards...), and
some boards report several plausible entries (per-core, per-CCD) under the
same chip; _auto_pick_sensor() guesses the most CPU-like one from a
priority list, but the guess can be wrong on an unfamiliar board - hence
the manual pin. Once a sensor is pinned manually, its hwmon label rarely
means anything to a human ("edge", "Composite"...), so the caption becomes
a free-text field the user fills in themselves instead of being stuck with
"CPU" (see each content class's _display_label / each Settings class's
custom-label entry)."""

import math
import os

import cairo
import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, GLib, Gtk

from xeneon_dashboard import i18n

HWMON_ROOT = "/sys/class/hwmon"
REFRESH_INTERVAL_SECONDS = 2


def _rgba_to_hex(rgba: Gdk.RGBA) -> str:
    r, g, b = (round(c * 255) for c in (rgba.red, rgba.green, rgba.blue))
    return f"#{r:02x}{g:02x}{b:02x}"


def _hex_to_rgba(hex_str: str) -> Gdk.RGBA:
    rgba = Gdk.RGBA()
    rgba.parse(hex_str)
    return rgba

# Chip name -> priority (lower is preferred), tried in order; a chip not
# listed here can still be picked (see _auto_pick_sensor's fallback) but
# only once nothing better is found.
_CHIP_PRIORITY = ["k10temp", "zenpower", "coretemp", "cpu_thermal", "acpitz"]

# Within a chosen chip, a label matching one of these (case-insensitive) is
# preferred over an arbitrary entry - "Tctl"/"Tdie" (AMD's overall control
# temperature) and "Package id 0" (Intel's package-wide sensor) are the
# ones that actually track "the CPU", as opposed to a single core or CCD.
_LABEL_PRIORITY = ["tctl", "tdie", "package id 0"]

# Same size for both the "CPU" caption and the value next to it (they sit
# on one line - see CpuTempContent) so neither reads as more important
# than the other; only the weight tells them apart.
_FONT_PX = 22

_provider = Gtk.CssProvider()
_css_installed = False


def _ensure_css():
    global _css_installed
    if _css_installed:
        return
    _provider.load_from_string(
        f".xeneon-cputemp-label {{ font-size: {_FONT_PX}px; color: rgba(255, 255, 255, 0.75); }}"
        f".xeneon-cputemp-value {{ font-size: {_FONT_PX}px; font-weight: 700; color: #ffffff; }}"
    )
    Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), _provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
    _css_installed = True


def _read_stripped(path: str) -> str | None:
    try:
        with open(path, encoding="utf-8") as handle:
            return handle.read().strip()
    except OSError:
        return None


def _all_sensors() -> list[tuple[str, str, float]]:
    """Every (chip, label, current_celsius) triple exposed under
    /sys/class/hwmon right now, across every chip on the machine - not
    just CPU-looking ones, so CpuTempSettings can offer the full list for
    the user to pick from manually. Empty (rather than raising) if hwmon
    isn't there at all (e.g. this ran on a non-Linux platform)."""
    sensors: list[tuple[str, str, float]] = []
    try:
        entries = os.listdir(HWMON_ROOT)
    except OSError:
        return sensors
    for entry in entries:
        hwmon_dir = os.path.join(HWMON_ROOT, entry)
        chip = _read_stripped(os.path.join(hwmon_dir, "name"))
        if not chip:
            continue
        try:
            filenames = os.listdir(hwmon_dir)
        except OSError:
            continue
        for filename in sorted(filenames):
            if not (filename.startswith("temp") and filename.endswith("_input")):
                continue
            raw = _read_stripped(os.path.join(hwmon_dir, filename))
            if raw is None:
                continue
            try:
                celsius = int(raw) / 1000.0
            except ValueError:
                continue
            label_path = os.path.join(hwmon_dir, filename.replace("_input", "_label"))
            label = _read_stripped(label_path) or ""
            sensors.append((chip, label, celsius))
    return sensors


def _auto_pick_sensor(sensors: list[tuple[str, str, float]]) -> tuple[str, str] | None:
    for chip_name in _CHIP_PRIORITY:
        candidates = [(chip, label) for chip, label, _value in sensors if chip == chip_name]
        if not candidates:
            continue
        for chip, label in candidates:
            if label.lower() in _LABEL_PRIORITY:
                return (chip, label)
        return candidates[0]
    return (sensors[0][0], sensors[0][1]) if sensors else None


def sensor_display_name(chip: str, label: str) -> str:
    return f"{chip} ({label})" if label else chip


class CpuTempContent(Gtk.Box):
    """The widget's whole display: "CPU" and the value ("52°C") side by
    side on one line, same font size, centered. Sensor choice and °C/°F
    unit are live-editable through CpuTempSettings, which holds a
    reference to this instance."""

    def __init__(self):
        super().__init__(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        _ensure_css()
        self.set_halign(Gtk.Align.CENTER)
        self.set_valign(Gtk.Align.CENTER)

        # None means "auto-pick" (see _auto_pick_sensor) - an explicit
        # (chip, label) pin overrides that once the user picks one in
        # CpuTempSettings.
        self._sensor_chip: str | None = None
        self._sensor_label: str | None = None
        # Free-text override for the caption, only ever shown/edited while
        # a sensor is pinned manually (see _display_label) - hwmon labels
        # for anything that isn't the CPU ("edge", "Composite"...) rarely
        # mean anything to a human, so the user names it themselves instead
        # (e.g. "GPU", "NVMe") rather than being stuck with "CPU".
        self._custom_label: str | None = None
        self.unit_fahrenheit = False
        self._available_sensors: list[tuple[str, str, float]] = []

        self._caption_label = Gtk.Label()
        self._caption_label.add_css_class("xeneon-cputemp-label")
        self._caption_label.set_valign(Gtk.Align.CENTER)
        self.append(self._caption_label)

        self._value_label = Gtk.Label()
        self._value_label.add_css_class("xeneon-cputemp-value")
        self._value_label.set_valign(Gtk.Align.CENTER)
        self.append(self._value_label)

        self._retranslate()
        self._timeout_id = GLib.timeout_add_seconds(REFRESH_INTERVAL_SECONDS, self._tick)
        self.connect("destroy", self._on_destroy)
        i18n.on_change(self._retranslate)

    def _on_destroy(self, *_args):
        if self._timeout_id is not None:
            GLib.source_remove(self._timeout_id)
            self._timeout_id = None

    def _tick(self) -> bool:
        self._refresh()
        return GLib.SOURCE_CONTINUE

    def available_sensors(self) -> list[tuple[str, str, float]]:
        return self._available_sensors

    def effective_sensor(self) -> tuple[str | None, str | None]:
        """The (chip, label) actually driving the display right now: the
        user's pin if set, otherwise the current auto-pick - exposed so
        CpuTempSettings can show which entry is really in effect."""
        if self._sensor_chip is not None:
            return self._sensor_chip, self._sensor_label
        return _auto_pick_sensor(self._available_sensors) or (None, None)

    def set_sensor(self, chip: str | None, label: str | None):
        self._sensor_chip = chip
        self._sensor_label = label
        self._refresh()

    def set_custom_label(self, text: str | None):
        self._custom_label = text or None
        self._refresh()

    def set_unit_fahrenheit(self, enabled: bool):
        self.unit_fahrenheit = enabled
        self._refresh()

    def _current_value(self) -> float | None:
        chip, label = self.effective_sensor()
        if chip is None:
            return None
        return next((value for c, l, value in self._available_sensors if c == chip and l == label), None)

    def _display_label(self) -> str:
        """"CPU" while auto-picking (see effective_sensor) - fixed, not
        user-editable, since that's what this widget defaults to. Once a
        sensor is pinned manually, the user's own custom_label takes over
        (see CpuTempSettings), falling back to the same "CPU" text only
        until they've actually typed something."""
        if self._sensor_chip is not None and self._custom_label:
            return self._custom_label
        return i18n._("widgets.cpu_temp.label")

    def _refresh(self):
        self._available_sensors = _all_sensors()
        self._caption_label.set_label(self._display_label())
        value = self._current_value()
        if value is None:
            self._value_label.set_text("--°")
            self._value_label.set_tooltip_text(i18n._("widgets.cpu_temp.unavailable"))
            return
        display = value * 9 / 5 + 32 if self.unit_fahrenheit else value
        unit_symbol = "°F" if self.unit_fahrenheit else "°C"
        self._value_label.set_text(f"{round(display)}{unit_symbol}")
        self._value_label.set_tooltip_text(None)

    def _retranslate(self):
        self._refresh()

    def to_dict(self) -> dict:
        return {
            "sensor_chip": self._sensor_chip,
            "sensor_label": self._sensor_label,
            "custom_label": self._custom_label,
            "unit_fahrenheit": self.unit_fahrenheit,
        }

    def apply_dict(self, data: dict) -> None:
        """Restores a state previously returned by to_dict(). Only touches
        fields actually present, so a partial/older dict still applies
        cleanly."""
        if not data:
            return
        if "sensor_chip" in data:
            self.set_sensor(data.get("sensor_chip"), data.get("sensor_label"))
        if "custom_label" in data:
            self.set_custom_label(data.get("custom_label"))
        if "unit_fahrenheit" in data:
            self.set_unit_fahrenheit(data["unit_fahrenheit"])


class CpuTempSettings(Gtk.Box):
    """The widget's own settings: which sensor drives the display (auto, or
    one pinned entry, see CpuTempContent.set_sensor), a free-text override
    for its caption once a sensor is pinned manually (most hwmon labels -
    "edge", "Composite"... - mean nothing to a human, so the user names it
    themselves instead of being stuck with "CPU"), and a °C/°F toggle. The
    sensor list is built once from whatever CpuTempContent already found at
    construction time - unlike AudioSettings' player list, hwmon chips
    don't appear/disappear at runtime, so there's nothing to poll for
    here."""

    def __init__(self, content: CpuTempContent):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self._content = content
        self.set_size_request(240, -1)

        self._sensor_label_widget = Gtk.Label()
        self._sensor_label_widget.set_halign(Gtk.Align.START)
        self.append(self._sensor_label_widget)

        self._entries: list[tuple[str, str] | None] = [None]
        current_chip, current_label = content._sensor_chip, content._sensor_label
        for chip, label, _value in content.available_sensors():
            entry = (chip, label)
            if entry not in self._entries:
                self._entries.append(entry)
        if current_chip is not None and (current_chip, current_label) not in self._entries:
            self._entries.append((current_chip, current_label))

        self._sensor_dropdown = Gtk.DropDown.new(Gtk.StringList.new([]), None)
        self._sensor_dropdown.set_hexpand(True)
        self._sensor_dropdown.connect("notify::selected", self._on_sensor_changed)
        self.append(self._sensor_dropdown)

        # Only meaningful once a sensor is pinned manually (see
        # CpuTempContent._display_label) - kept insensitive rather than
        # hidden while on Auto, so its position in the popover doesn't jump
        # around when switching back and forth.
        self._custom_label_label = Gtk.Label()
        self._custom_label_label.set_halign(Gtk.Align.START)
        self.append(self._custom_label_label)
        self._custom_label_entry = Gtk.Entry()
        self._custom_label_entry.set_text(content._custom_label or "")
        self._custom_label_entry.connect("changed", self._on_custom_label_changed)
        self.append(self._custom_label_entry)

        self._refresh_sensor_model()

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

        self._retranslate()
        i18n.on_change(self._retranslate)

    def _refresh_sensor_model(self):
        names = []
        for entry in self._entries:
            if entry is None:
                names.append(i18n._("widgets.cpu_temp.settings.sensor_auto"))
            else:
                names.append(sensor_display_name(*entry))
        current = (self._content._sensor_chip, self._content._sensor_label) if self._content._sensor_chip else None
        selected_index = self._entries.index(current) if current in self._entries else 0
        self._sensor_dropdown.set_model(Gtk.StringList.new(names))
        self._sensor_dropdown.set_selected(selected_index)
        self._update_custom_label_sensitivity(selected_index)

    def _update_custom_label_sensitivity(self, index: int):
        is_manual = 0 <= index < len(self._entries) and self._entries[index] is not None
        self._custom_label_label.set_sensitive(is_manual)
        self._custom_label_entry.set_sensitive(is_manual)

    def sync_from_content(self):
        self._refresh_sensor_model()
        self._custom_label_entry.set_text(self._content._custom_label or "")
        self._celsius_button.set_active(not self._content.unit_fahrenheit)
        self._fahrenheit_button.set_active(self._content.unit_fahrenheit)

    def _on_sensor_changed(self, dropdown, _pspec):
        index = dropdown.get_selected()
        if 0 <= index < len(self._entries):
            entry = self._entries[index]
            if entry is None:
                self._content.set_sensor(None, None)
            else:
                self._content.set_sensor(*entry)
            self._update_custom_label_sensitivity(index)

    def _on_custom_label_changed(self, entry: Gtk.Entry):
        self._content.set_custom_label(entry.get_text())

    def _on_unit_toggled(self, button):
        self._content.set_unit_fahrenheit(button.get_active())

    def _retranslate(self):
        self._sensor_label_widget.set_label(i18n._("widgets.cpu_temp.settings.sensor"))
        self._custom_label_label.set_label(i18n._("widgets.cpu_temp.settings.custom_label"))
        self._custom_label_entry.set_placeholder_text(i18n._("widgets.cpu_temp.label"))
        self._unit_label.set_label(i18n._("widgets.cpu_temp.settings.unit"))
        self._celsius_button.set_label(i18n._("widgets.cpu_temp.settings.unit_celsius"))
        self._fahrenheit_button.set_label(i18n._("widgets.cpu_temp.settings.unit_fahrenheit"))
        self._refresh_sensor_model()


# Gauge geometry, in degrees, clockwise from due east (cairo's own angle
# convention - 0 deg = 3 o'clock) - a 270 deg dial starting at 135 deg (the
# 7-8 o'clock position) and sweeping down to 45 deg (4-5 o'clock), leaving
# the bottom wedge open like a speedometer.
GAUGE_START_DEG = 135.0
GAUGE_SWEEP_DEG = 270.0

# The dial always maps this fixed Celsius range to its sweep, regardless of
# the °C/°F display toggle (which only changes the printed number) - 0-100
# covers the range a CPU/GPU/NVMe temperature actually moves in.
GAUGE_MIN_C = 0.0
GAUGE_MAX_C = 100.0

# Every size at content_scale == 1.0 (100%, see TempGaugeContent.
# set_content_scale) - same technique as WeatherContent's BASE_*/
# content_scale: everything grows or shrinks together off one slider
# instead of needing a second footprint the way AudioContent's SIZE_L/
# SIZE_SQ split does.
BASE_ARC_RADIUS_PX = 110
BASE_ARC_THICKNESS_PX = 20
BASE_UNIT_FONT_PX = 20
BASE_VALUE_FONT_PX = 48
BASE_CAPTION_FONT_PX = 16
BASE_COLUMN_SPACING_PX = 4

MIN_CONTENT_SCALE = 0.5
MAX_CONTENT_SCALE = 2.0
DEFAULT_CONTENT_SCALE = 1.0

DEFAULT_TEXT_HEX = "#ffffff"
DEFAULT_BAR_HEX = "#e0218a"

# Per-instance scaled font rules, keyed by each instance's own unique css
# class - same pattern as WeatherContent's _rules/_reload_css, so one gauge
# widget's font sizing never bleeds into another's.
_gauge_provider = Gtk.CssProvider()
_gauge_css_installed = False
_gauge_rules: dict[str, str] = {}


def _ensure_gauge_css_installed():
    global _gauge_css_installed
    if _gauge_css_installed:
        return
    Gtk.StyleContext.add_provider_for_display(
        Gdk.Display.get_default(), _gauge_provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )
    _gauge_css_installed = True


def _reload_gauge_css():
    _gauge_provider.load_from_string("\n".join(rule for rule in _gauge_rules.values() if rule))


class TempGaugeContent(Gtk.Overlay):
    """A round dial gauge for one hwmon sensor's temperature - same sensor
    auto-pick/manual-pin/custom-label behavior as CpuTempContent (see that
    class and the module docstring), but drawn as an arc (Cairo, on a
    Gtk.DrawingArea) instead of a plain text line, with the value/unit/
    caption stacked as an overlay on top of it. Content size, text color
    and the arc's own color are all user-editable through
    TempGaugeSettings, which holds a reference to this instance."""

    _next_id = 0

    def __init__(self):
        super().__init__()
        _ensure_gauge_css_installed()
        TempGaugeContent._next_id += 1
        self._css_class = f"xeneon-tempgauge-{TempGaugeContent._next_id}"
        self.add_css_class(self._css_class)

        self._sensor_chip: str | None = None
        self._sensor_label: str | None = None
        self._custom_label: str | None = None
        self.unit_fahrenheit = False
        self._available_sensors: list[tuple[str, str, float]] = []
        self.text_color = _hex_to_rgba(DEFAULT_TEXT_HEX)
        self.bar_color = _hex_to_rgba(DEFAULT_BAR_HEX)
        self.content_scale = DEFAULT_CONTENT_SCALE

        self._gauge_area = Gtk.DrawingArea()
        self._gauge_area.set_hexpand(True)
        self._gauge_area.set_vexpand(True)
        self._gauge_area.set_draw_func(self._on_draw)
        self.set_child(self._gauge_area)

        column = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        column.set_halign(Gtk.Align.CENTER)
        column.set_valign(Gtk.Align.CENTER)
        column.set_can_target(False)
        self._unit_label = Gtk.Label()
        self._unit_label.add_css_class("xeneon-tempgauge-unit")
        column.append(self._unit_label)
        self._value_label = Gtk.Label()
        self._value_label.add_css_class("xeneon-tempgauge-value")
        column.append(self._value_label)
        self._caption_label = Gtk.Label()
        self._caption_label.add_css_class("xeneon-tempgauge-caption")
        column.append(self._caption_label)
        self.add_overlay(column)
        self._column = column

        self._apply_content_scale()
        self._retranslate()
        self._timeout_id = GLib.timeout_add_seconds(REFRESH_INTERVAL_SECONDS, self._tick)
        self.connect("destroy", self._on_destroy)
        i18n.on_change(self._retranslate)

    def _on_destroy(self, *_args):
        if self._timeout_id is not None:
            GLib.source_remove(self._timeout_id)
            self._timeout_id = None

    def _tick(self) -> bool:
        self._refresh()
        return GLib.SOURCE_CONTINUE

    def available_sensors(self) -> list[tuple[str, str, float]]:
        return self._available_sensors

    def effective_sensor(self) -> tuple[str | None, str | None]:
        if self._sensor_chip is not None:
            return self._sensor_chip, self._sensor_label
        return _auto_pick_sensor(self._available_sensors) or (None, None)

    def set_sensor(self, chip: str | None, label: str | None):
        self._sensor_chip = chip
        self._sensor_label = label
        self._refresh()

    def set_custom_label(self, text: str | None):
        self._custom_label = text or None
        self._refresh()

    def set_unit_fahrenheit(self, enabled: bool):
        self.unit_fahrenheit = enabled
        self._refresh()

    def set_text_color(self, rgba: Gdk.RGBA):
        self.text_color = rgba
        self._apply_content_scale()

    def set_bar_color(self, rgba: Gdk.RGBA):
        self.bar_color = rgba
        self._gauge_area.queue_draw()

    def set_content_scale(self, scale: float):
        self.content_scale = max(MIN_CONTENT_SCALE, min(MAX_CONTENT_SCALE, scale))
        self._apply_content_scale()

    def _apply_content_scale(self):
        scale = self.content_scale
        text_hex = _rgba_to_hex(self.text_color)
        _gauge_rules[self._css_class] = (
            f".{self._css_class} .xeneon-tempgauge-unit {{"
            f" font-size: {round(BASE_UNIT_FONT_PX * scale)}px; color: {text_hex}; }}"
            f".{self._css_class} .xeneon-tempgauge-value {{"
            f" font-size: {round(BASE_VALUE_FONT_PX * scale)}px; font-weight: 700; color: {text_hex}; }}"
            f".{self._css_class} .xeneon-tempgauge-caption {{"
            f" font-size: {round(BASE_CAPTION_FONT_PX * scale)}px; color: {text_hex}; }}"
        )
        _reload_gauge_css()
        self._column.set_spacing(round(BASE_COLUMN_SPACING_PX * scale))
        self._gauge_area.queue_draw()

    def _current_value(self) -> float | None:
        chip, label = self.effective_sensor()
        if chip is None:
            return None
        return next((value for c, l, value in self._available_sensors if c == chip and l == label), None)

    def _display_label(self) -> str:
        if self._sensor_chip is not None and self._custom_label:
            return self._custom_label
        return i18n._("widgets.temp_gauge.label")

    def _gauge_fraction(self, celsius: float | None) -> float:
        if celsius is None:
            return 0.0
        span = GAUGE_MAX_C - GAUGE_MIN_C
        return max(0.0, min(1.0, (celsius - GAUGE_MIN_C) / span))

    def _refresh(self):
        self._available_sensors = _all_sensors()
        self._caption_label.set_label(self._display_label())
        celsius = self._current_value()
        unit_symbol = "°F" if self.unit_fahrenheit else "°C"
        self._unit_label.set_label(unit_symbol)
        if celsius is None:
            self._value_label.set_text("--")
            self._value_label.set_tooltip_text(i18n._("widgets.temp_gauge.unavailable"))
        else:
            display = celsius * 9 / 5 + 32 if self.unit_fahrenheit else celsius
            self._value_label.set_text(f"{display:.2f}")
            self._value_label.set_tooltip_text(None)
        self._gauge_area.queue_draw()

    def _retranslate(self):
        self._refresh()

    def _on_draw(self, _area, cr, width, height):
        cx, cy = width / 2, height / 2
        radius = BASE_ARC_RADIUS_PX * self.content_scale
        thickness = BASE_ARC_THICKNESS_PX * self.content_scale
        start = math.radians(GAUGE_START_DEG)
        end = math.radians(GAUGE_START_DEG + GAUGE_SWEEP_DEG)

        cr.set_line_cap(cairo.LINE_CAP_ROUND)
        cr.set_line_width(thickness)

        cr.set_source_rgba(1, 1, 1, 0.12)
        cr.arc(cx, cy, radius, start, end)
        cr.stroke()

        fraction = self._gauge_fraction(self._current_value())
        if fraction > 0:
            bar = self.bar_color
            cr.set_source_rgba(bar.red, bar.green, bar.blue, bar.alpha)
            cr.arc(cx, cy, radius, start, start + (end - start) * fraction)
            cr.stroke()

    def to_dict(self) -> dict:
        return {
            "sensor_chip": self._sensor_chip,
            "sensor_label": self._sensor_label,
            "custom_label": self._custom_label,
            "unit_fahrenheit": self.unit_fahrenheit,
            "text_color": _rgba_to_hex(self.text_color),
            "bar_color": _rgba_to_hex(self.bar_color),
            "content_scale": self.content_scale,
        }

    def apply_dict(self, data: dict) -> None:
        """Restores a state previously returned by to_dict(). Only touches
        fields actually present, so a partial/older dict still applies
        cleanly."""
        if not data:
            return
        if "sensor_chip" in data:
            self.set_sensor(data.get("sensor_chip"), data.get("sensor_label"))
        if "custom_label" in data:
            self.set_custom_label(data.get("custom_label"))
        if "unit_fahrenheit" in data:
            self.set_unit_fahrenheit(data["unit_fahrenheit"])
        if "text_color" in data:
            self.set_text_color(_hex_to_rgba(data["text_color"]))
        if "bar_color" in data:
            self.set_bar_color(_hex_to_rgba(data["bar_color"]))
        if "content_scale" in data:
            self.set_content_scale(data["content_scale"])


class TempGaugeSettings(Gtk.Box):
    """TempGaugeContent's own settings: the same sensor picker/custom-label
    pair as CpuTempSettings (see that class), plus what's specific to the
    dial - text color, arc color, and a content-size slider (see
    WeatherSettings' identical slider for content_scale)."""

    def __init__(self, content: TempGaugeContent):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self._content = content
        self.set_size_request(260, -1)

        self._sensor_label_widget = Gtk.Label()
        self._sensor_label_widget.set_halign(Gtk.Align.START)
        self.append(self._sensor_label_widget)

        self._entries: list[tuple[str, str] | None] = [None]
        current_chip, current_label = content._sensor_chip, content._sensor_label
        for chip, label, _value in content.available_sensors():
            entry = (chip, label)
            if entry not in self._entries:
                self._entries.append(entry)
        if current_chip is not None and (current_chip, current_label) not in self._entries:
            self._entries.append((current_chip, current_label))

        self._sensor_dropdown = Gtk.DropDown.new(Gtk.StringList.new([]), None)
        self._sensor_dropdown.set_hexpand(True)
        self._sensor_dropdown.connect("notify::selected", self._on_sensor_changed)
        self.append(self._sensor_dropdown)

        self._custom_label_label = Gtk.Label()
        self._custom_label_label.set_halign(Gtk.Align.START)
        self.append(self._custom_label_label)
        self._custom_label_entry = Gtk.Entry()
        self._custom_label_entry.set_text(content._custom_label or "")
        self._custom_label_entry.connect("changed", self._on_custom_label_changed)
        self.append(self._custom_label_entry)

        self._refresh_sensor_model()

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

        self._text_color_label = Gtk.Label()
        self._text_color_label.set_hexpand(True)
        self._text_color_label.set_halign(Gtk.Align.START)
        self._text_color_button = Gtk.ColorDialogButton.new(Gtk.ColorDialog.new())
        self._text_color_button.set_rgba(content.text_color)
        self._text_color_button.connect("notify::rgba", self._on_text_color_changed)
        text_color_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        text_color_row.append(self._text_color_label)
        text_color_row.append(self._text_color_button)
        self.append(text_color_row)

        self._bar_color_label = Gtk.Label()
        self._bar_color_label.set_hexpand(True)
        self._bar_color_label.set_halign(Gtk.Align.START)
        self._bar_color_button = Gtk.ColorDialogButton.new(Gtk.ColorDialog.new())
        self._bar_color_button.set_rgba(content.bar_color)
        self._bar_color_button.connect("notify::rgba", self._on_bar_color_changed)
        bar_color_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        bar_color_row.append(self._bar_color_label)
        bar_color_row.append(self._bar_color_button)
        self.append(bar_color_row)

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

    def _refresh_sensor_model(self):
        names = []
        for entry in self._entries:
            if entry is None:
                names.append(i18n._("widgets.temp_gauge.settings.sensor_auto"))
            else:
                names.append(sensor_display_name(*entry))
        current = (self._content._sensor_chip, self._content._sensor_label) if self._content._sensor_chip else None
        selected_index = self._entries.index(current) if current in self._entries else 0
        self._sensor_dropdown.set_model(Gtk.StringList.new(names))
        self._sensor_dropdown.set_selected(selected_index)
        self._update_custom_label_sensitivity(selected_index)

    def _update_custom_label_sensitivity(self, index: int):
        is_manual = 0 <= index < len(self._entries) and self._entries[index] is not None
        self._custom_label_label.set_sensitive(is_manual)
        self._custom_label_entry.set_sensitive(is_manual)

    def sync_from_content(self):
        self._refresh_sensor_model()
        self._custom_label_entry.set_text(self._content._custom_label or "")
        self._celsius_button.set_active(not self._content.unit_fahrenheit)
        self._fahrenheit_button.set_active(self._content.unit_fahrenheit)
        self._text_color_button.set_rgba(self._content.text_color)
        self._bar_color_button.set_rgba(self._content.bar_color)
        self._scale_slider.set_value(self._content.content_scale * 100)

    def _on_sensor_changed(self, dropdown, _pspec):
        index = dropdown.get_selected()
        if 0 <= index < len(self._entries):
            entry = self._entries[index]
            if entry is None:
                self._content.set_sensor(None, None)
            else:
                self._content.set_sensor(*entry)
            self._update_custom_label_sensitivity(index)

    def _on_custom_label_changed(self, entry: Gtk.Entry):
        self._content.set_custom_label(entry.get_text())

    def _on_unit_toggled(self, button):
        self._content.set_unit_fahrenheit(button.get_active())

    def _on_text_color_changed(self, button, _pspec):
        self._content.set_text_color(button.get_rgba())

    def _on_bar_color_changed(self, button, _pspec):
        self._content.set_bar_color(button.get_rgba())

    def _on_scale_changed(self, scale):
        self._content.set_content_scale(scale.get_value() / 100)

    def _retranslate(self):
        self._sensor_label_widget.set_label(i18n._("widgets.temp_gauge.settings.sensor"))
        self._custom_label_label.set_label(i18n._("widgets.temp_gauge.settings.custom_label"))
        self._custom_label_entry.set_placeholder_text(i18n._("widgets.temp_gauge.label"))
        self._unit_label.set_label(i18n._("widgets.temp_gauge.settings.unit"))
        self._celsius_button.set_label(i18n._("widgets.temp_gauge.settings.unit_celsius"))
        self._fahrenheit_button.set_label(i18n._("widgets.temp_gauge.settings.unit_fahrenheit"))
        self._text_color_label.set_label(i18n._("widgets.temp_gauge.settings.text_color"))
        self._bar_color_label.set_label(i18n._("widgets.temp_gauge.settings.bar_color"))
        self._scale_label.set_label(i18n._("widgets.temp_gauge.settings.content_scale"))
        self._refresh_sensor_model()
