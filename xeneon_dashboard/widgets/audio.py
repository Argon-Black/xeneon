"""Audio widget: a Plexamp-inspired "now playing" card - album art fills
the background, title/artist/progress/transport sit over a dark gradient
at the bottom. Unlike clock/weather, it doesn't own any data of its own:
it's a generic controller for whatever media player is currently running,
via MPRIS (org.mpris.MediaPlayer2), the standard D-Bus interface Linux
media players expose for exactly this (media keys, GNOME's own
now-playing indicator...). Plexamp's desktop build implements it, and so
does everything else worth controlling from a dashboard (Spotify, VLC,
browsers) - no per-player integration code needed.

True window embedding was ruled out earlier (no Wayland protocol lets one
app host another's surface); this sidesteps that entirely by drawing our
own UI and only exchanging D-Bus messages with whichever player is active.

MPRIS mechanics used here:
- player discovery: an initial org.freedesktop.DBus.ListNames, then a
  live NameOwnerChanged subscription (filtered to the
  org.mpris.MediaPlayer2.* namespace) so a player starting or quitting
  updates the list without polling;
- one MprisPlayer (a pair of GDBusProxy, root + Player interface) per
  discovered player - GDBusProxy caches property values and keeps them
  fresh via PropertiesChanged automatically, which covers everything
  except Position (excluded from that signal by the MPRIS spec itself,
  since it'd fire continuously) - that one is fetched with an explicit
  Properties.Get when needed and ticked locally the rest of the time;
- "now playing" auto-follow: with several players open, the one actually
  Playing wins; the user can pin a specific one instead from this
  widget's own settings (see AudioSettings), persisted like any other
  plugin setting.

Offered in two footprints (SIZE_L and SIZE_SQ, see grid.py) through the
same AudioContent/AudioSettings/MprisPlayer code - only a scale factor
derived from the widget's actual size differs (see _apply_scale), the
same way WeatherContent's content_scale grows or shrinks its whole layout
in proportion. Every font size, icon size, and gap is defined once here
(the BASE_* constants, calibrated for SIZE_L) and multiplied by that
factor, so there's no separate "compact" branch to keep in sync.
"""

import threading

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
gi.require_version("GdkPixbuf", "2.0")
gi.require_version("Gio", "2.0")
from gi.repository import Gdk, GdkPixbuf, Gio, GLib, Gtk, Pango

from xeneon_dashboard import i18n
from xeneon_dashboard.grid import SIZE_L

MPRIS_PREFIX = "org.mpris.MediaPlayer2."
MPRIS_PATH = "/org/mpris/MediaPlayer2"
MPRIS_ROOT_INTERFACE = "org.mpris.MediaPlayer2"
MPRIS_PLAYER_INTERFACE = "org.mpris.MediaPlayer2.Player"
DBUS_CALL_TIMEOUT_MS = 1500

# Purely spatial sizes (margins, padding, gaps) still scale with the
# widget's own footprint (scale == 1.0 at SIZE_L - see
# AudioContent._apply_scale), same technique as WeatherContent's
# BASE_*/content_scale.
BASE_BADGE_MARGIN_PX = 16
BASE_BADGE_GAP_PX = 6
BASE_BADGE_PADDING_V_PX = 4
BASE_BADGE_PADDING_LEFT_PX = 8
BASE_BADGE_PADDING_RIGHT_PX = 12
BASE_BADGE_DOT_PX = 8
BASE_BOTTOM_MARGIN_H_PX = 24
BASE_BOTTOM_MARGIN_BOTTOM_PX = 20
BASE_BOTTOM_SPACING_PX = 4
BASE_ARTIST_MARGIN_BOTTOM_PX = 10
BASE_PROGRESS_ROW_SPACING_PX = 8
BASE_TIME_FONT_PX = 11

# The "now playing" info itself - source label, title, artist, transport -
# is what a glance at the widget actually needs to read, so none of it
# shrinks with the widget's footprint the way the spacing above does: it's
# the same fixed size at SIZE_SQ as at SIZE_L. Overflow is handled by
# truncating the text instead (see _truncate, MAX_TITLE_CHARS/MAX_ARTIST_CHARS).
BADGE_FONT_PX = 20
TITLE_FONT_PX = 22
ARTIST_FONT_PX = 20
TRANSPORT_SPACING_PX = 20
TRANSPORT_MARGIN_TOP_PX = 8
TRANSPORT_ICON_PX = 20
PLAY_ICON_PX = 22
PLAY_BUTTON_PX = 44

MAX_TITLE_CHARS = 20
MAX_ARTIST_CHARS = 30


def _format_seconds(value: float) -> str:
    total = max(0, int(value))
    return f"{total // 60}:{total % 60:02d}"


def _truncate(text: str, max_chars: int) -> str:
    if len(text) <= max_chars:
        return text
    return text[: max_chars - 1].rstrip() + "…"


# Static look that never changes with scale (colors, gradient, hover) -
# shared by every instance. Scale-dependent sizes (fonts, icon pixel
# sizes, the play button's diameter...) live in a second, per-instance
# provider instead (see _rules/_reload_scale_css below), the same split
# WidgetAppearance/ShortcutsContent use for their own per-instance CSS.
_provider = Gtk.CssProvider()
_css_installed = False


def _ensure_css():
    global _css_installed
    if _css_installed:
        return
    _provider.load_from_string(
        ".xeneon-audio-gradient {"
        " background-image: linear-gradient(to bottom, rgba(0,0,0,0) 30%, rgba(0,0,0,0.78) 100%);"
        " }"
        ".xeneon-audio-badge { background-color: rgba(0,0,0,0.45); border-radius: 999px; }"
        ".xeneon-audio-badge-dot { background-color: #3fd67a; border-radius: 999px; }"
        ".xeneon-audio-badge-label { color: #ffffff; }"
        ".xeneon-audio-title { color: #ffffff; font-weight: 700; }"
        ".xeneon-audio-subtitle { color: rgba(255, 255, 255, 0.75); }"
        ".xeneon-audio-time { color: rgba(255, 255, 255, 0.75); }"
        ".xeneon-audio-empty { color: rgba(255, 255, 255, 0.6); }"
        ".xeneon-audio-transport { color: #ffffff; }"
        ".xeneon-audio-play-button {"
        " background-color: rgba(15, 15, 15, 0.85); color: #ffffff; border-radius: 999px;"
        " }"
        ".xeneon-audio-play-button:hover { background-color: rgba(0, 0, 0, 0.95); }"
        ".xeneon-audio-progress trough { min-height: 4px; }"
    )
    Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), _provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
    _css_installed = True


# Per-instance scaled rules (font sizes, badge padding/dot size, play
# button diameter), keyed by each AudioContent's own unique class so a
# SIZE_L instance and a SIZE_SQ instance on the same page never fight over
# the same selector - same pattern as WeatherContent's _rules/_reload_css.
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


class MprisPlayer:
    """One MPRIS-speaking player, addressed by its D-Bus well-known name
    (e.g. "org.mpris.MediaPlayer2.plexamp"). Wraps two GDBusProxy - the
    player's root interface (just for Identity, the human-readable name)
    and its Player interface (playback state/metadata/controls). Both
    auto-refresh their property cache from PropertiesChanged, except
    Position (see module docstring), fetched on demand via get_position_seconds()."""

    def __init__(self, bus_name: str, on_properties_changed, on_seeked):
        self.bus_name = bus_name
        self._on_properties_changed = on_properties_changed
        self._on_seeked = on_seeked

        self._proxy = Gio.DBusProxy.new_for_bus_sync(
            Gio.BusType.SESSION, Gio.DBusProxyFlags.NONE, None, bus_name, MPRIS_PATH, MPRIS_PLAYER_INTERFACE, None
        )
        self._proxy.set_default_timeout(DBUS_CALL_TIMEOUT_MS)
        self._proxy.connect("g-properties-changed", lambda *_a: self._on_properties_changed(self))
        self._proxy.connect("g-signal", self._on_signal)

        self._root_proxy = Gio.DBusProxy.new_for_bus_sync(
            Gio.BusType.SESSION, Gio.DBusProxyFlags.NONE, None, bus_name, MPRIS_PATH, MPRIS_ROOT_INTERFACE, None
        )
        self._root_proxy.set_default_timeout(DBUS_CALL_TIMEOUT_MS)

    def _on_signal(self, _proxy, _sender_name, signal_name, _parameters):
        if signal_name == "Seeked":
            self._on_seeked(self)

    @property
    def identity(self) -> str:
        variant = self._root_proxy.get_cached_property("Identity")
        return variant.unpack() if variant else self.bus_name

    @property
    def playback_status(self) -> str:
        variant = self._proxy.get_cached_property("PlaybackStatus")
        return variant.unpack() if variant else "Stopped"

    @property
    def metadata(self) -> dict:
        variant = self._proxy.get_cached_property("Metadata")
        return variant.unpack() if variant else {}

    def get_position_seconds(self) -> float:
        try:
            result = self._proxy.call_sync(
                "org.freedesktop.DBus.Properties.Get",
                GLib.Variant("(ss)", (MPRIS_PLAYER_INTERFACE, "Position")),
                Gio.DBusCallFlags.NONE,
                DBUS_CALL_TIMEOUT_MS,
                None,
            )
            (position_us,) = result.unpack()
            return position_us / 1_000_000
        except GLib.Error:
            return 0.0

    def play_pause(self):
        self._call("PlayPause")

    def next(self):
        self._call("Next")

    def previous(self):
        self._call("Previous")

    def seek(self, offset_seconds: float):
        self._call("Seek", GLib.Variant("(x)", (int(offset_seconds * 1_000_000),)))

    def _call(self, method_name: str, parameters=None):
        try:
            self._proxy.call_sync(method_name, parameters, Gio.DBusCallFlags.NONE, DBUS_CALL_TIMEOUT_MS, None)
        except GLib.Error:
            pass


class AudioContent(Gtk.Overlay):
    _next_id = 0

    def __init__(self, size: tuple[int, int] = SIZE_L):
        super().__init__()
        _ensure_css()
        _ensure_scale_css_installed()
        AudioContent._next_id += 1
        self._css_class = f"xeneon-audio-{AudioContent._next_id}"
        self.add_css_class(self._css_class)
        # How SIZE_SQ (or any future footprint) shrinks every font, icon
        # and gap in proportion instead of needing its own layout - see
        # _apply_scale and the BASE_* constants above.
        self._scale = min(size[0] / SIZE_L[0], size[1] / SIZE_L[1])
        self._players: dict[str, MprisPlayer] = {}
        self._active: MprisPlayer | None = None
        self._preferred_bus_name: str | None = None
        self._local_position = 0.0
        self._length_seconds = 0.0
        self._art_generation = 0
        self._tick_id: int | None = None
        self._connection = None
        self._subscription_id: int | None = None

        self._background = Gtk.Picture()
        self._background.set_content_fit(Gtk.ContentFit.COVER)
        self.set_child(self._background)

        gradient = Gtk.Box()
        gradient.add_css_class("xeneon-audio-gradient")
        gradient.set_can_target(False)
        gradient.set_hexpand(True)
        gradient.set_vexpand(True)
        self.add_overlay(gradient)

        self._badge = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
        self._badge.add_css_class("xeneon-audio-badge")
        self._badge.set_halign(Gtk.Align.START)
        self._badge.set_valign(Gtk.Align.START)
        self._badge_dot = Gtk.Box()
        self._badge_dot.add_css_class("xeneon-audio-badge-dot")
        self._badge_dot.set_valign(Gtk.Align.CENTER)
        self._badge.append(self._badge_dot)
        self._badge_label = Gtk.Label()
        self._badge_label.add_css_class("xeneon-audio-badge-label")
        self._badge.append(self._badge_label)
        self.add_overlay(self._badge)

        self._empty_label = Gtk.Label()
        self._empty_label.add_css_class("xeneon-audio-empty")
        self._empty_label.set_halign(Gtk.Align.CENTER)
        self._empty_label.set_valign(Gtk.Align.CENTER)
        self.add_overlay(self._empty_label)

        self._bottom = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        self._bottom.set_valign(Gtk.Align.END)

        self._title_label = Gtk.Label()
        self._title_label.add_css_class("xeneon-audio-title")
        self._title_label.set_halign(Gtk.Align.START)
        self._title_label.set_ellipsize(Pango.EllipsizeMode.END)
        self._bottom.append(self._title_label)

        self._artist_label = Gtk.Label()
        self._artist_label.add_css_class("xeneon-audio-subtitle")
        self._artist_label.set_halign(Gtk.Align.START)
        self._artist_label.set_ellipsize(Pango.EllipsizeMode.END)
        self._bottom.append(self._artist_label)

        self._progress_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
        self._elapsed_label = Gtk.Label(label="0:00")
        self._elapsed_label.add_css_class("xeneon-audio-time")
        self._progress_row.append(self._elapsed_label)
        self._progress_scale = Gtk.Scale(orientation=Gtk.Orientation.HORIZONTAL)
        self._progress_scale.add_css_class("xeneon-audio-progress")
        self._progress_scale.set_hexpand(True)
        self._progress_scale.set_draw_value(False)
        self._progress_scale.set_range(0, 1)
        self._progress_scale.connect("change-value", self._on_seek_requested)
        self._progress_row.append(self._progress_scale)
        self._duration_label = Gtk.Label(label="0:00")
        self._duration_label.add_css_class("xeneon-audio-time")
        self._progress_row.append(self._duration_label)
        self._bottom.append(self._progress_row)

        self._transport_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
        self._transport_row.set_halign(Gtk.Align.CENTER)
        self._prev_button = self._make_transport_button("media-skip-backward-symbolic")
        self._prev_button.connect("clicked", lambda _b: self._active and self._active.previous())
        self._play_button = self._make_transport_button("media-playback-start-symbolic")
        self._play_button.add_css_class("xeneon-audio-play-button")
        self._play_button.remove_css_class("xeneon-audio-transport")
        self._play_button.connect("clicked", lambda _b: self._active and self._active.play_pause())
        self._next_button = self._make_transport_button("media-skip-forward-symbolic")
        self._next_button.connect("clicked", lambda _b: self._active and self._active.next())
        self._transport_row.append(self._prev_button)
        self._transport_row.append(self._play_button)
        self._transport_row.append(self._next_button)
        self._bottom.append(self._transport_row)

        self.add_overlay(self._bottom)

        self._apply_scale()
        self._set_has_player(False)
        self.connect("destroy", self._on_destroy)
        i18n.on_change(self._retranslate)
        self._retranslate()

        self._connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        self._subscription_id = self._connection.signal_subscribe(
            "org.freedesktop.DBus",
            "org.freedesktop.DBus",
            "NameOwnerChanged",
            "/org/freedesktop/DBus",
            None,
            Gio.DBusSignalFlags.NONE,
            self._on_name_owner_changed,
        )
        self._discover_players()
        self._tick_id = GLib.timeout_add_seconds(1, self._tick)

    def _make_transport_button(self, icon_name: str) -> Gtk.Button:
        button = Gtk.Button()
        button.add_css_class("flat")
        button.add_css_class("circular")
        button.add_css_class("xeneon-audio-transport")
        button.set_child(Gtk.Image.new_from_icon_name(icon_name))
        return button

    def _apply_scale(self):
        scale = self._scale
        _scale_rules[self._css_class] = (
            f".{self._css_class} .xeneon-audio-badge {{"
            f" padding: {round(BASE_BADGE_PADDING_V_PX * scale)}px {round(BASE_BADGE_PADDING_RIGHT_PX * scale)}px"
            f" {round(BASE_BADGE_PADDING_V_PX * scale)}px {round(BASE_BADGE_PADDING_LEFT_PX * scale)}px; }}"
            f".{self._css_class} .xeneon-audio-badge-dot {{"
            f" min-width: {round(BASE_BADGE_DOT_PX * scale)}px; min-height: {round(BASE_BADGE_DOT_PX * scale)}px; }}"
            f".{self._css_class} .xeneon-audio-badge-label {{ font-size: {BADGE_FONT_PX}px; }}"
            f".{self._css_class} .xeneon-audio-title {{ font-size: {TITLE_FONT_PX}px; }}"
            f".{self._css_class} .xeneon-audio-subtitle {{ font-size: {ARTIST_FONT_PX}px; }}"
            f".{self._css_class} .xeneon-audio-time {{ font-size: {round(BASE_TIME_FONT_PX * scale)}px; }}"
            f".{self._css_class} .xeneon-audio-play-button {{"
            f" min-width: {PLAY_BUTTON_PX}px; min-height: {PLAY_BUTTON_PX}px; }}"
        )
        _reload_scale_css()

        self._badge.set_margin_start(round(BASE_BADGE_MARGIN_PX * scale))
        self._badge.set_margin_top(round(BASE_BADGE_MARGIN_PX * scale))
        self._badge.set_spacing(round(BASE_BADGE_GAP_PX * scale))

        self._bottom.set_margin_start(round(BASE_BOTTOM_MARGIN_H_PX * scale))
        self._bottom.set_margin_end(round(BASE_BOTTOM_MARGIN_H_PX * scale))
        self._bottom.set_margin_bottom(round(BASE_BOTTOM_MARGIN_BOTTOM_PX * scale))
        self._bottom.set_spacing(round(BASE_BOTTOM_SPACING_PX * scale))

        self._artist_label.set_margin_bottom(round(BASE_ARTIST_MARGIN_BOTTOM_PX * scale))

        self._progress_row.set_spacing(round(BASE_PROGRESS_ROW_SPACING_PX * scale))

        self._transport_row.set_spacing(TRANSPORT_SPACING_PX)
        self._transport_row.set_margin_top(TRANSPORT_MARGIN_TOP_PX)

        self._prev_button.get_child().set_pixel_size(TRANSPORT_ICON_PX)
        self._next_button.get_child().set_pixel_size(TRANSPORT_ICON_PX)
        self._play_button.get_child().set_pixel_size(PLAY_ICON_PX)

    @property
    def players(self) -> dict:
        return self._players

    @property
    def preferred_player(self) -> str | None:
        return self._preferred_bus_name

    def set_preferred_player(self, bus_name: str | None):
        self._preferred_bus_name = bus_name
        self._pick_active_player()

    def to_dict(self) -> dict:
        return {"preferred_player": self._preferred_bus_name}

    def apply_dict(self, data: dict) -> None:
        if not data:
            return
        if "preferred_player" in data:
            self.set_preferred_player(data["preferred_player"])

    def _on_destroy(self, *_args):
        if self._tick_id is not None:
            GLib.source_remove(self._tick_id)
            self._tick_id = None
        if self._subscription_id is not None and self._connection is not None:
            self._connection.signal_unsubscribe(self._subscription_id)
            self._subscription_id = None

    def _discover_players(self):
        try:
            proxy = Gio.DBusProxy.new_for_bus_sync(
                Gio.BusType.SESSION,
                Gio.DBusProxyFlags.NONE,
                None,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                None,
            )
            result = proxy.call_sync("ListNames", None, Gio.DBusCallFlags.NONE, DBUS_CALL_TIMEOUT_MS, None)
            (names,) = result.unpack()
        except GLib.Error:
            names = []
        for name in names:
            if name.startswith(MPRIS_PREFIX):
                self._add_player(name)
        self._pick_active_player()

    def _on_name_owner_changed(self, _connection, _sender, _path, _interface, _signal, params):
        name, _old_owner, new_owner = params.unpack()
        if not name.startswith(MPRIS_PREFIX):
            return
        if new_owner:
            self._add_player(name)
        else:
            self._remove_player(name)

    def _add_player(self, bus_name: str):
        if bus_name in self._players:
            return
        try:
            player = MprisPlayer(bus_name, self._on_player_properties_changed, self._on_player_seeked)
        except GLib.Error:
            return
        self._players[bus_name] = player
        self._pick_active_player()

    def _remove_player(self, bus_name: str):
        if bus_name not in self._players:
            return
        del self._players[bus_name]
        if self._active is not None and self._active.bus_name == bus_name:
            self._active = None
        self._pick_active_player()

    def _on_player_properties_changed(self, player: MprisPlayer):
        self._pick_active_player()
        if player is self._active:
            self._refresh_active_display(resync_position=True)

    def _on_player_seeked(self, player: MprisPlayer):
        if player is self._active:
            self._local_position = player.get_position_seconds()
            self._progress_scale.set_value(self._local_position)
            self._update_time_labels()

    def _pick_active_player(self):
        candidate_name = None
        if self._preferred_bus_name and self._preferred_bus_name in self._players:
            candidate_name = self._preferred_bus_name
        else:
            candidate_name = next(
                (name for name, player in self._players.items() if player.playback_status == "Playing"), None
            )
            if candidate_name is None and self._players:
                candidate_name = next(iter(self._players))
        candidate = self._players.get(candidate_name) if candidate_name else None
        if candidate is not self._active:
            self._active = candidate
            self._refresh_active_display(resync_position=True)

    def _set_has_player(self, has_player: bool):
        self._badge.set_visible(has_player)
        self._bottom.set_visible(has_player)
        self._empty_label.set_visible(not has_player)
        if not has_player:
            self._background.set_paintable(None)

    def _refresh_active_display(self, resync_position: bool = False):
        player = self._active
        self._set_has_player(player is not None)
        if player is None:
            return

        self._badge_label.set_label(player.identity)

        metadata = player.metadata
        title = metadata.get("xesam:title") or i18n._("widgets.audio.unknown_title")
        artists = metadata.get("xesam:artist") or []
        artist_text = ", ".join(a for a in artists if a) or i18n._("widgets.audio.unknown_artist")
        self._title_label.set_label(_truncate(title, MAX_TITLE_CHARS))
        self._artist_label.set_label(_truncate(artist_text, MAX_ARTIST_CHARS))

        length_us = metadata.get("mpris:length") or 0
        self._length_seconds = length_us / 1_000_000
        self._progress_scale.set_range(0, max(self._length_seconds, 1))
        self._progress_scale.set_sensitive(self._length_seconds > 0)

        status = player.playback_status
        playing = status == "Playing"
        self._play_button.get_child().set_from_icon_name(
            "media-playback-pause-symbolic" if playing else "media-playback-start-symbolic"
        )
        self._play_button.set_tooltip_text(i18n._("widgets.audio.pause" if playing else "widgets.audio.play"))

        if resync_position:
            self._local_position = player.get_position_seconds()
        self._progress_scale.set_value(self._local_position)
        self._update_time_labels()

        self._load_art(metadata.get("mpris:artUrl"))

    def _update_time_labels(self):
        self._elapsed_label.set_label(_format_seconds(self._local_position))
        self._duration_label.set_label(_format_seconds(self._length_seconds))

    def _tick(self) -> bool:
        if self._active is not None and self._active.playback_status == "Playing":
            self._local_position = min(self._local_position + 1, self._length_seconds)
            self._progress_scale.set_value(self._local_position)
            self._update_time_labels()
        return GLib.SOURCE_CONTINUE

    def _on_seek_requested(self, _scale, _scroll_type, value: float) -> bool:
        if self._active is not None:
            value = max(0.0, min(value, self._length_seconds))
            self._active.seek(value - self._local_position)
            self._local_position = value
            self._update_time_labels()
        return False

    def _load_art(self, art_url: str | None):
        self._art_generation += 1
        generation = self._art_generation
        if not art_url:
            GLib.idle_add(self._apply_art, None, generation)
            return

        def worker():
            pixbuf = None
            try:
                gfile = Gio.File.new_for_uri(art_url)
                _ok, contents, _etag = gfile.load_contents()
                loader = GdkPixbuf.PixbufLoader()
                loader.write(contents)
                loader.close()
                pixbuf = loader.get_pixbuf()
            except GLib.Error:
                pixbuf = None
            GLib.idle_add(self._apply_art, pixbuf, generation)

        threading.Thread(target=worker, daemon=True).start()

    def _apply_art(self, pixbuf, generation: int):
        if generation != self._art_generation:
            return GLib.SOURCE_REMOVE
        self._background.set_paintable(Gdk.Texture.new_for_pixbuf(pixbuf) if pixbuf is not None else None)
        return GLib.SOURCE_REMOVE

    def _retranslate(self):
        self._empty_label.set_label(i18n._("widgets.audio.empty"))
        self._prev_button.set_tooltip_text(i18n._("widgets.audio.previous"))
        self._next_button.set_tooltip_text(i18n._("widgets.audio.next"))
        self._refresh_active_display()


class AudioSettings(Gtk.Box):
    """Lets the user pin one specific player instead of auto-following
    whichever is currently Playing (see AudioContent._pick_active_player).
    The dropdown's options are re-read from content.players on a timer
    since players can appear/disappear at any time, not just while this
    popover happens to be open."""

    REFRESH_INTERVAL_SECONDS = 2

    def __init__(self, content: AudioContent):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self._content = content
        self.set_size_request(220, -1)
        self._entries: list[str | None] = [None]

        self._label = Gtk.Label()
        self._label.set_halign(Gtk.Align.START)
        self.append(self._label)

        self._dropdown = Gtk.DropDown.new(Gtk.StringList.new([]), None)
        self._dropdown.set_hexpand(True)
        self._dropdown.connect("notify::selected", self._on_selected)
        self.append(self._dropdown)

        self._refresh_id = GLib.timeout_add_seconds(self.REFRESH_INTERVAL_SECONDS, self._refresh_options)
        self.connect("destroy", self._on_destroy)

        self._retranslate()
        i18n.on_change(self._retranslate)

    def _on_destroy(self, *_args):
        if self._refresh_id is not None:
            GLib.source_remove(self._refresh_id)
            self._refresh_id = None

    def _refresh_options(self) -> bool:
        names = [i18n._("widgets.audio.settings.player_auto")]
        entries: list[str | None] = [None]
        for bus_name, player in self._content.players.items():
            names.append(player.identity)
            entries.append(bus_name)
        self._entries = entries
        current = self._content.preferred_player
        selected_index = entries.index(current) if current in entries else 0
        self._dropdown.set_model(Gtk.StringList.new(names))
        self._dropdown.set_selected(selected_index)
        return GLib.SOURCE_CONTINUE

    def _on_selected(self, dropdown, _pspec):
        index = dropdown.get_selected()
        if 0 <= index < len(self._entries):
            self._content.set_preferred_player(self._entries[index])

    def _retranslate(self):
        self._label.set_label(i18n._("widgets.audio.settings.player"))
        self._refresh_options()
