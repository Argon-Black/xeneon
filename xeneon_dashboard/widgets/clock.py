import datetime
from zoneinfo import ZoneInfo

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, GLib, Gtk, Pango

from xeneon_dashboard import i18n

DEFAULT_TEXT_HEX = "#ffffff"
DEFAULT_TIME_FONT = "Sans 72"
DEFAULT_LABEL_FONT = "Sans 18"
DEFAULT_CITY_INDEX = 6  # Los Angeles, to match the reference look

# Segment box padding as a fraction of the chosen digit font size, not a
# fixed pixel amount - so the box keeps the same proportions around the
# digit whether the user picks a tiny or a huge font.
SEGMENT_MARGIN_H_RATIO = 0.375
SEGMENT_MARGIN_V_RATIO = 0.21

# Predefined city choices, each a (i18n key, IANA timezone) pair. Free-form
# tz search would cover more ground but isn't worth the extra UI for a
# dashboard clock - this list spans the timezones people actually ask for.
CITIES = [
    ("widgets.clock.cities.paris", "Europe/Paris"),
    ("widgets.clock.cities.london", "Europe/London"),
    ("widgets.clock.cities.berlin", "Europe/Berlin"),
    ("widgets.clock.cities.moscow", "Europe/Moscow"),
    ("widgets.clock.cities.new_york", "America/New_York"),
    ("widgets.clock.cities.chicago", "America/Chicago"),
    ("widgets.clock.cities.los_angeles", "America/Los_Angeles"),
    ("widgets.clock.cities.sao_paulo", "America/Sao_Paulo"),
    ("widgets.clock.cities.dubai", "Asia/Dubai"),
    ("widgets.clock.cities.tokyo", "Asia/Tokyo"),
    ("widgets.clock.cities.beijing", "Asia/Shanghai"),
    ("widgets.clock.cities.sydney", "Australia/Sydney"),
]

DAY_KEYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
MONTH_KEYS = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"]


def _rgba_to_hex(rgba: Gdk.RGBA) -> str:
    r, g, b = (round(c * 255) for c in (rgba.red, rgba.green, rgba.blue))
    return f"#{r:02x}{g:02x}{b:02x}"


def _hex_to_rgba(hex_str: str) -> Gdk.RGBA:
    rgba = Gdk.RGBA()
    rgba.parse(hex_str)
    return rgba


def _expand_wrapper(widget: Gtk.Widget) -> Gtk.Box:
    """Wraps widget in a box that claims all the leftover space a
    CenterBox start/end slot would otherwise leave unclaimed, so widget's
    own valign=CENTER centers it within that space instead of widget
    sitting flush against the CenterBox's outer edge."""
    wrapper = Gtk.Box()
    wrapper.set_vexpand(True)
    wrapper.append(widget)
    return wrapper


class ClockContent(Gtk.CenterBox):
    """The clock widget's display: city name, a flip-clock-style HH:MM:SS,
    and the date below in short or long form. The time and the city/date
    each have their own font and color (city and date share one style,
    since they're both secondary to the time); either the city or the date
    line can be hidden entirely. All of this is live-editable through
    ClockSettings, which holds a reference to this instance.

    A CenterBox (not a plain Box with valign=CENTER) is what actually
    centers the column vertically - relying on valign/vexpand alone judged
    unreliable once the digit boxes grew large enough to leave real slack
    to distribute."""

    def __init__(self):
        super().__init__()
        self.set_orientation(Gtk.Orientation.VERTICAL)
        self.set_halign(Gtk.Align.CENTER)
        # valign stays FILL (the default) on purpose: the CenterBox needs
        # the widget's *entire* available height to center the segments
        # row against, otherwise it first gets squeezed to its own natural
        # size and centered as a block, then centers the segments a second
        # time within that already-shrunk block - two nested centerings
        # whose slack doesn't cancel out, leaving the segments off-center
        # overall even though each individual step looks centered.

        self.time_font_desc = Pango.FontDescription.from_string(DEFAULT_TIME_FONT)
        self.time_color = Gdk.RGBA()
        self.time_color.parse(DEFAULT_TEXT_HEX)
        self.label_font_desc = Pango.FontDescription.from_string(DEFAULT_LABEL_FONT)
        self.label_color = Gdk.RGBA()
        self.label_color.parse(DEFAULT_TEXT_HEX)
        self.timezone = ZoneInfo(CITIES[DEFAULT_CITY_INDEX][1])
        self._city_key = CITIES[DEFAULT_CITY_INDEX][0]
        self.date_format_long = False
        self.hour_format_12h = False
        self._city_visible = True
        self._date_visible = True

        # The segment row (the gray digit tiles) is the CenterBox's center
        # widget, so *it* sits at the exact vertical middle of the widget -
        # city/date go in the start/end slots instead of being stacked into
        # that same centered column, since averaging all three together
        # would pull the tiles off-center whenever city/date's combined
        # height isn't perfectly symmetric around them. Each is further
        # wrapped in its own vexpand box so it centers within *its* leftover
        # space (above the tiles for the city, below for the date) instead
        # of sitting flush against the widget's outer edge.
        self._city_label = Gtk.Label()
        self._city_label.set_hexpand(True)
        self._city_label.set_halign(Gtk.Align.CENTER)
        self._city_label.set_valign(Gtk.Align.CENTER)
        self.set_start_widget(_expand_wrapper(self._city_label))

        self._segments_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=4)
        self._segments_box.set_halign(Gtk.Align.CENTER)
        hour_box, self._hour_label = self._make_segment()
        self._colon1_label = self._make_colon()
        minute_box, self._minute_label = self._make_segment()
        self._colon2_label = self._make_colon()
        second_box, self._second_label = self._make_segment()
        self._ampm_label = Gtk.Label()
        self._ampm_label.set_valign(Gtk.Align.CENTER)
        self._ampm_label.set_visible(False)
        for segment in (hour_box, self._colon1_label, minute_box, self._colon2_label, second_box, self._ampm_label):
            self._segments_box.append(segment)
        self.set_center_widget(self._segments_box)

        self._date_label = Gtk.Label()
        self._date_label.set_hexpand(True)
        self._date_label.set_halign(Gtk.Align.CENTER)
        self._date_label.set_valign(Gtk.Align.CENTER)
        self.set_end_widget(_expand_wrapper(self._date_label))

        self._apply_segment_margins()
        self._tick()
        self._timeout_id = GLib.timeout_add_seconds(1, self._tick)
        self.connect("destroy", self._on_destroy)
        i18n.on_change(self._retranslate)

    def _make_segment(self) -> tuple[Gtk.Box, Gtk.Label]:
        # The ".card" background goes on the wrapping box, not the label
        # itself: a widget's own margin is transparent space *outside* its
        # background, so setting it directly on the label would only push
        # neighbours away without growing the visible tile. Sizing the box
        # around a margined label makes the margin read as padding instead,
        # since the box's own natural size includes its child's margin.
        label = Gtk.Label()
        label.set_width_chars(2)
        label.set_justify(Gtk.Justification.CENTER)
        box = Gtk.Box()
        box.add_css_class("card")
        box.append(label)
        return box, label

    def _apply_segment_margins(self):
        base_pt = (self.time_font_desc.get_size() or 12 * Pango.SCALE) / Pango.SCALE
        h = max(2, round(base_pt * SEGMENT_MARGIN_H_RATIO))
        v = max(2, round(base_pt * SEGMENT_MARGIN_V_RATIO))
        for label in (self._hour_label, self._minute_label, self._second_label):
            label.set_margin_start(h)
            label.set_margin_end(h)
            label.set_margin_top(v)
            label.set_margin_bottom(v)

    def _make_colon(self) -> Gtk.Label:
        label = Gtk.Label(label=":")
        return label

    def _on_destroy(self, *_args):
        if self._timeout_id is not None:
            GLib.source_remove(self._timeout_id)
            self._timeout_id = None

    def set_time_font_desc(self, font_desc: Pango.FontDescription):
        self.time_font_desc = font_desc
        self._apply_segment_margins()
        self._refresh()

    def set_time_color(self, rgba: Gdk.RGBA):
        self.time_color = rgba
        self._refresh()

    def set_label_font_desc(self, font_desc: Pango.FontDescription):
        self.label_font_desc = font_desc
        self._refresh()

    def set_label_color(self, rgba: Gdk.RGBA):
        self.label_color = rgba
        self._refresh()

    def set_timezone(self, tz_name: str, city_key: str):
        self.timezone = ZoneInfo(tz_name)
        self._city_key = city_key
        self._refresh()

    def set_date_format_long(self, long_format: bool):
        self.date_format_long = long_format
        self._refresh()

    def set_hour_format_12h(self, enabled: bool):
        self.hour_format_12h = enabled
        self._ampm_label.set_visible(enabled)
        self._refresh()

    def set_city_visible(self, visible: bool):
        self._city_visible = visible
        self._city_label.set_visible(visible)

    def set_date_visible(self, visible: bool):
        self._date_visible = visible
        self._date_label.set_visible(visible)

    def reset(self):
        """Back to this widget's out-of-the-box defaults. Goes through the
        same setters as everything else (not a shortcut that pokes fields
        directly) so every side effect - segment margins, visibility,
        re-rendering - happens exactly like a normal edit would."""
        self.set_time_font_desc(Pango.FontDescription.from_string(DEFAULT_TIME_FONT))
        self.set_time_color(_hex_to_rgba(DEFAULT_TEXT_HEX))
        self.set_label_font_desc(Pango.FontDescription.from_string(DEFAULT_LABEL_FONT))
        self.set_label_color(_hex_to_rgba(DEFAULT_TEXT_HEX))
        default_key, default_tz = CITIES[DEFAULT_CITY_INDEX]
        self.set_timezone(default_tz, default_key)
        self.set_date_format_long(False)
        self.set_hour_format_12h(False)
        self.set_city_visible(True)
        self.set_date_visible(True)

    def _markup(self, text: str, font_desc: Pango.FontDescription, color: Gdk.RGBA) -> str:
        return (
            f'<span font_desc="{GLib.markup_escape_text(font_desc.to_string())}" '
            f'foreground="{_rgba_to_hex(color)}">{GLib.markup_escape_text(text)}</span>'
        )

    def _tick(self) -> bool:
        self._refresh()
        return GLib.SOURCE_CONTINUE

    def _refresh(self):
        now = datetime.datetime.now(self.timezone)

        self._city_label.set_markup(self._markup(i18n._(self._city_key), self.label_font_desc, self.label_color))

        if self.hour_format_12h:
            hour_value = now.hour % 12 or 12
            ampm_key = "widgets.clock.am" if now.hour < 12 else "widgets.clock.pm"
            self._ampm_label.set_markup(self._markup(i18n._(ampm_key), self.label_font_desc, self.label_color))
        else:
            hour_value = now.hour

        for label, value in (
            (self._hour_label, f"{hour_value:02d}"),
            (self._minute_label, f"{now.minute:02d}"),
            (self._second_label, f"{now.second:02d}"),
            (self._colon1_label, ":"),
            (self._colon2_label, ":"),
        ):
            label.set_markup(self._markup(value, self.time_font_desc, self.time_color))

        day_name = i18n._(f"widgets.clock.days_{'long' if self.date_format_long else 'short'}.{DAY_KEYS[now.weekday()]}")
        month_name = i18n._(f"widgets.clock.months_{'long' if self.date_format_long else 'short'}.{MONTH_KEYS[now.month - 1]}")
        if self.date_format_long:
            date_text = i18n._("widgets.clock.date_long", day=day_name, day_num=now.day, month=month_name, year=now.year)
        else:
            date_text = i18n._("widgets.clock.date_short", day=day_name, day_num=now.day, month=month_name)
        self._date_label.set_markup(self._markup(date_text, self.label_font_desc, self.label_color))

    def _retranslate(self):
        self._refresh()

    def to_dict(self) -> dict:
        return {
            "time_font": self.time_font_desc.to_string(),
            "time_color": _rgba_to_hex(self.time_color),
            "label_font": self.label_font_desc.to_string(),
            "label_color": _rgba_to_hex(self.label_color),
            "city_key": self._city_key,
            "date_format_long": self.date_format_long,
            "hour_format_12h": self.hour_format_12h,
            "city_visible": self._city_visible,
            "date_visible": self._date_visible,
        }

    def apply_dict(self, data: dict) -> None:
        """Restores a state previously returned by to_dict(). Only touches
        fields actually present, so a partial/older dict still applies
        cleanly."""
        if not data:
            return
        if "time_font" in data:
            self.set_time_font_desc(Pango.FontDescription.from_string(data["time_font"]))
        if "time_color" in data:
            self.set_time_color(_hex_to_rgba(data["time_color"]))
        if "label_font" in data:
            self.set_label_font_desc(Pango.FontDescription.from_string(data["label_font"]))
        if "label_color" in data:
            self.set_label_color(_hex_to_rgba(data["label_color"]))
        city_key = data.get("city_key")
        if city_key:
            tz_name = next((tz for key, tz in CITIES if key == city_key), None)
            if tz_name:
                self.set_timezone(tz_name, city_key)
        if "date_format_long" in data:
            self.set_date_format_long(data["date_format_long"])
        if "hour_format_12h" in data:
            self.set_hour_format_12h(data["hour_format_12h"])
        if "city_visible" in data:
            self.set_city_visible(data["city_visible"])
        if "date_visible" in data:
            self.set_date_visible(data["date_visible"])


class ClockSettings(Gtk.Box):
    """The clock's own settings, shown to the right of the generic
    appearance controls in the same configure popover: separate font/color
    for the time and for the city/date, city/timezone with a show/hide
    switch, and date format (short/long) with its own show/hide switch."""

    def __init__(self, content: ClockContent):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self._content = content
        self.set_size_request(260, -1)

        self._time_font_label = Gtk.Label()
        self._time_font_label.set_halign(Gtk.Align.START)
        self.append(self._time_font_label)
        self._time_font_button = Gtk.FontDialogButton.new(Gtk.FontDialog.new())
        self._time_font_button.set_level(Gtk.FontLevel.FONT)
        self._time_font_button.set_font_desc(content.time_font_desc)
        self._time_font_button.set_hexpand(True)
        self._time_font_button.connect("notify::font-desc", self._on_time_font_changed)
        self._time_color_button = Gtk.ColorDialogButton.new(Gtk.ColorDialog.new())
        self._time_color_button.set_rgba(content.time_color)
        self._time_color_button.connect("notify::rgba", self._on_time_color_changed)
        self.append(self._make_row(self._time_font_button, self._time_color_button))

        self.append(Gtk.Separator())

        self._label_font_label = Gtk.Label()
        self._label_font_label.set_halign(Gtk.Align.START)
        self.append(self._label_font_label)
        self._label_font_button = Gtk.FontDialogButton.new(Gtk.FontDialog.new())
        self._label_font_button.set_level(Gtk.FontLevel.FONT)
        self._label_font_button.set_font_desc(content.label_font_desc)
        self._label_font_button.set_hexpand(True)
        self._label_font_button.connect("notify::font-desc", self._on_label_font_changed)
        self._label_color_button = Gtk.ColorDialogButton.new(Gtk.ColorDialog.new())
        self._label_color_button.set_rgba(content.label_color)
        self._label_color_button.connect("notify::rgba", self._on_label_color_changed)
        self.append(self._make_row(self._label_font_button, self._label_color_button))

        self.append(Gtk.Separator())

        self._city_label = Gtk.Label()
        self._city_label.set_halign(Gtk.Align.START)
        self._city_visible_switch = Gtk.Switch()
        self._city_visible_switch.set_active(content._city_visible)
        self._city_visible_switch.set_valign(Gtk.Align.CENTER)
        self._city_visible_switch.connect("notify::active", self._on_city_visible_toggled)
        city_names = [i18n._(key) for key, _tz in CITIES]
        self._city_dropdown = Gtk.DropDown.new(Gtk.StringList.new(city_names), None)
        self._city_dropdown.set_hexpand(True)
        current_index = next((i for i, (key, _tz) in enumerate(CITIES) if key == content._city_key), 0)
        self._city_dropdown.set_selected(current_index)
        self._city_dropdown.connect("notify::selected", self._on_city_changed)
        self.append(self._make_row(self._city_label, self._city_visible_switch, self._city_dropdown))

        self.append(Gtk.Separator())

        self._date_label = Gtk.Label()
        self._date_label.set_hexpand(True)
        self._date_label.set_halign(Gtk.Align.START)
        self._date_visible_switch = Gtk.Switch()
        self._date_visible_switch.set_active(content._date_visible)
        self._date_visible_switch.set_valign(Gtk.Align.CENTER)
        self._date_visible_switch.connect("notify::active", self._on_date_visible_toggled)
        self.append(self._make_row(self._date_label, self._date_visible_switch))

        self._date_format_label = Gtk.Label()
        self._date_format_label.set_hexpand(True)
        self._date_format_label.set_halign(Gtk.Align.START)
        self._short_button = Gtk.ToggleButton()
        self._long_button = Gtk.ToggleButton()
        self._long_button.set_group(self._short_button)
        self._short_button.set_active(not content.date_format_long)
        self._long_button.set_active(content.date_format_long)
        self._long_button.connect("toggled", self._on_date_format_toggled)
        self.append(self._make_row(self._date_format_label, self._short_button, self._long_button))

        self.append(Gtk.Separator())

        self._hour_format_label = Gtk.Label()
        self._hour_format_label.set_hexpand(True)
        self._hour_format_label.set_halign(Gtk.Align.START)
        self._24h_button = Gtk.ToggleButton()
        self._12h_button = Gtk.ToggleButton()
        self._12h_button.set_group(self._24h_button)
        self._24h_button.set_active(not content.hour_format_12h)
        self._12h_button.set_active(content.hour_format_12h)
        self._12h_button.connect("toggled", self._on_hour_format_toggled)
        self.append(self._make_row(self._hour_format_label, self._24h_button, self._12h_button))

        self._retranslate()
        i18n.on_change(self._retranslate)

    def _make_row(self, *widgets: Gtk.Widget) -> Gtk.Box:
        row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        for widget in widgets:
            row.append(widget)
        return row

    def sync_from_content(self):
        """Re-reads every control's displayed value from self._content -
        needed after content.reset() (or any other change made to it from
        outside these controls' own signal handlers) changes the model
        directly, since a control otherwise only pushes edits one-way and
        doesn't notice a programmatic change underneath it."""
        content = self._content
        self._time_font_button.set_font_desc(content.time_font_desc)
        self._time_color_button.set_rgba(content.time_color)
        self._label_font_button.set_font_desc(content.label_font_desc)
        self._label_color_button.set_rgba(content.label_color)
        self._city_visible_switch.set_active(content._city_visible)
        current_index = next((i for i, (key, _tz) in enumerate(CITIES) if key == content._city_key), 0)
        self._city_dropdown.set_selected(current_index)
        self._date_visible_switch.set_active(content._date_visible)
        self._short_button.set_active(not content.date_format_long)
        self._long_button.set_active(content.date_format_long)
        self._24h_button.set_active(not content.hour_format_12h)
        self._12h_button.set_active(content.hour_format_12h)

    def _on_time_font_changed(self, button, _pspec):
        self._content.set_time_font_desc(button.get_font_desc())

    def _on_time_color_changed(self, button, _pspec):
        self._content.set_time_color(button.get_rgba())

    def _on_label_font_changed(self, button, _pspec):
        self._content.set_label_font_desc(button.get_font_desc())

    def _on_label_color_changed(self, button, _pspec):
        self._content.set_label_color(button.get_rgba())

    def _on_city_changed(self, dropdown, _pspec):
        index = dropdown.get_selected()
        if 0 <= index < len(CITIES):
            key, tz_name = CITIES[index]
            self._content.set_timezone(tz_name, key)

    def _on_city_visible_toggled(self, switch, _pspec):
        self._content.set_city_visible(switch.get_active())

    def _on_date_visible_toggled(self, switch, _pspec):
        self._content.set_date_visible(switch.get_active())

    def _on_date_format_toggled(self, button):
        self._content.set_date_format_long(button.get_active())

    def _on_hour_format_toggled(self, button):
        self._content.set_hour_format_12h(button.get_active())

    def _retranslate(self):
        self._time_font_label.set_label(i18n._("widgets.clock.settings.time_font"))
        self._label_font_label.set_label(i18n._("widgets.clock.settings.label_font"))
        self._city_label.set_label(i18n._("widgets.clock.settings.city"))
        self._date_label.set_label(i18n._("widgets.clock.settings.date"))
        self._date_format_label.set_label(i18n._("widgets.clock.settings.date_format"))
        self._short_button.set_label(i18n._("widgets.clock.settings.date_format_short"))
        self._long_button.set_label(i18n._("widgets.clock.settings.date_format_long"))
        self._hour_format_label.set_label(i18n._("widgets.clock.settings.hour_format"))
        self._24h_button.set_label(i18n._("widgets.clock.settings.hour_format_24h"))
        self._12h_button.set_label(i18n._("widgets.clock.settings.hour_format_12h"))
        selected = self._city_dropdown.get_selected()
        self._city_dropdown.set_model(Gtk.StringList.new([i18n._(key) for key, _tz in CITIES]))
        self._city_dropdown.set_selected(selected)
