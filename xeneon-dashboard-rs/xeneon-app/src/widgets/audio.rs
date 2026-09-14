//! Audio widget: a Plexamp-inspired "now playing" card, ported from
//! `widgets/audio.py`. Unlike Clock, it owns no data of its own - it's a
//! generic controller for whatever media player is currently running, via
//! MPRIS (`org.mpris.MediaPlayer2`), the standard D-Bus interface Linux
//! media players expose for this (media keys, GNOME's own now-playing
//! indicator...). No new crate needed for D-Bus: `gtk::gio::DBusProxy` is
//! already pulled in transitively by `gtk4`/`libadwaita` (see
//! `appearance_css.rs`'s use of `gio::File` for the same reason) -
//! consistent with this project's minimal-dependencies preference.
//!
//! **Step 1+2 scope** (see the step breakdown agreed with the user): MPRIS
//! discovery, display (source badge, album art, title, artist), a
//! click-to-seek progress bar, and transport controls
//! (previous/play-pause/next) - always at `SIZE_L`. `Position` isn't
//! covered by MPRIS's own `PropertiesChanged` signal (excluded by the spec
//! itself, since it'd fire continuously during playback) - handled the
//! same way as the Python original: fetched on demand (player picked/
//! track changed/seeked) and ticked locally once a second the rest of the
//! time. Deliberately still deferred to later steps:
//! - `AudioSettings` (pin a specific player instead of auto-following
//!   whichever is `Playing`) and its `to_dict`/`apply_dict` persistence;
//! - the `SIZE_SQ`/`SIZE_M` variants and the font/control-size tuning that
//!   only matters once more than one size exists.
//!
//! Title/artist/badge font sizes (22/20/20px) and the 20/30-character
//! truncation limits are carried over as already-decided values - the
//! Python side went through several rounds of mockups and live testing to
//! land on them, so there's no reason to re-derive them from scratch here.

use gtk::gio;
use gtk::glib;
use gtk::glib::prelude::*;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use crate::i18n_runtime as i18n;
use crate::widgets::registry::{self, WidgetInstance};

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_ROOT_INTERFACE: &str = "org.mpris.MediaPlayer2";
const MPRIS_PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
const DBUS_CALL_TIMEOUT_MS: i32 = 1500;

// Already validated on the Python side (see the module doc comment) -
// kept as plain fixed values since SIZE_SQ (which needs these to scale)
// isn't ported yet.
const MAX_TITLE_CHARS: usize = 20;
const MAX_ARTIST_CHARS: usize = 30;

fn format_seconds(value: f64) -> String {
    let total = value.max(0.0) as i64;
    format!("{}:{:02}", total / 60, total % 60)
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut short: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    while short.ends_with(char::is_whitespace) {
        short.pop();
    }
    short.push('…');
    short
}

// Resolved relative to this crate's own source directory, like
// `i18n_runtime::init`'s `locales_dir` - reliable under `cargo run`/
// `cargo build` regardless of the process's current working directory.
const EMPTY_STATE_ICON_PATH: &str = "assets/audio-empty.svg";

// `Texture::from_file` (what a raster file needs) rasterizes an SVG with
// no explicit target size at its own `viewBox` - 256px here - then GTK
// upscales that fixed bitmap ~3x to cover a SIZE_L card, which is exactly
// as blurry as stretching a 256px PNG would be: a vector source doesn't
// help once it's been turned into a bitmap at the wrong resolution.
// Rasterizing once at this larger, fixed size up front (via
// `Pixbuf::from_file_at_size`, which asks the SVG loader to render
// directly at that resolution) instead keeps it crisp - comfortably
// above SIZE_L's own footprint, so even the largest widget never
// upscales it, and any smaller size (SQ/M, once ported) only ever
// downscales, which doesn't blur.
const EMPTY_STATE_ICON_RASTER_PX: i32 = 768;

thread_local! {
    // Loaded once and reused by every AudioContent instance rather than
    // re-decoding the SVG from disk per widget - it never changes, so
    // there's nothing to invalidate. `RefCell<Option<...>>` rather than
    // a plain `OnceCell` because a failed load (missing file, no SVG
    // loader available) still needs to cache the "gave up" result too,
    // not retry on every single widget construction.
    static EMPTY_STATE_TEXTURE: RefCell<Option<Option<gtk::gdk::Texture>>> = RefCell::new(None);
}

/// The illustration shown in place of the album art when no MPRIS player
/// is active - `None` if the SVG failed to load (missing file, or no SVG
/// support in the system's gdk-pixbuf), in which case the empty state
/// just falls back to text only.
fn empty_state_texture() -> Option<gtk::gdk::Texture> {
    EMPTY_STATE_TEXTURE.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(EMPTY_STATE_ICON_PATH);
            let texture = gtk::gdk_pixbuf::Pixbuf::from_file_at_size(&path, EMPTY_STATE_ICON_RASTER_PX, EMPTY_STATE_ICON_RASTER_PX)
                .map(|pixbuf| gtk::gdk::Texture::for_pixbuf(&pixbuf))
                .inspect_err(|err| eprintln!("xeneon-dashboard: failed to load {}: {err}", path.display()))
                .ok();
            *cell = Some(texture);
        }
        cell.clone().unwrap()
    })
}

static INSTALL_CSS: Once = Once::new();

/// One shared, display-wide stylesheet - like `dummy.rs`'s `ensure_css_installed`,
/// good enough while every size shares the same fixed font sizes (see the
/// module doc comment; this'll need per-instance scoped CSS, the same way
/// `appearance_css.rs` does it, once `SIZE_SQ` needs different numbers).
fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        css.load_from_string(
            ".xeneon-audio-gradient {\
               background-image: linear-gradient(to bottom, rgba(0,0,0,0) 30%, rgba(0,0,0,0.78) 100%);\
             }\n\
             .xeneon-audio-badge {\
               background-color: rgba(0,0,0,0.45); border-radius: 999px; padding: 4px 12px 4px 8px;\
             }\n\
             .xeneon-audio-badge-dot {\
               background-color: #3fd67a; border-radius: 999px; min-width: 8px; min-height: 8px;\
             }\n\
             .xeneon-audio-badge-label { color: #ffffff; font-size: 20px; }\n\
             .xeneon-audio-title { color: #ffffff; font-size: 22px; font-weight: 700; }\n\
             .xeneon-audio-subtitle { color: rgba(255, 255, 255, 0.75); font-size: 20px; }\n\
             .xeneon-audio-time { color: rgba(255, 255, 255, 0.75); font-size: 15px; }\n\
             .xeneon-audio-empty { color: rgba(255, 255, 255, 0.6); }\n\
             .xeneon-audio-transport { color: #ffffff; }\n\
             .xeneon-audio-play-button {\
               background-color: rgba(15, 15, 15, 0.85); color: #ffffff;\
               min-width: 56px; min-height: 56px; border-radius: 999px;\
             }\n\
             .xeneon-audio-play-button:hover { background-color: rgba(0, 0, 0, 0.95); }\n\
             .xeneon-audio-progress trough { min-height: 4px; }\n",
        );
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

/// `GDBusProxy`'s `g-properties-changed`/`g-signal` GObject-signal
/// bindings (`DBusProxyExtManual` in the `gio` crate) require the callback
/// to be `Send + Sync`, even though in practice - for a proxy created
/// here on the GTK main thread against the default `GMainContext`, which
/// is all this single-threaded app ever does - GLib only ever dispatches
/// these signals back on that same thread. This wrapper asserts that
/// known-safe invariant so an `Rc`-based callback can satisfy the bound.
/// Never send a value wrapped in this across an actual thread boundary.
struct MainThreadOnly<T>(T);
unsafe impl<T> Send for MainThreadOnly<T> {}
unsafe impl<T> Sync for MainThreadOnly<T> {}

/// One MPRIS-speaking player, addressed by its D-Bus well-known name
/// (e.g. `org.mpris.MediaPlayer2.plexamp`). Wraps two `DBusProxy` - the
/// player's root interface (just for `Identity`, the human-readable name)
/// and its `Player` interface (playback state/metadata). Both auto-refresh
/// their property cache from `PropertiesChanged`, except `Position`
/// (excluded from that signal by the MPRIS spec itself, since it'd fire
/// continuously) - not needed at all in this read-only step.
struct MprisPlayer {
    bus_name: String,
    proxy: gio::DBusProxy,
    root_proxy: gio::DBusProxy,
}

impl MprisPlayer {
    fn new(bus_name: &str) -> Result<Self, glib::Error> {
        let proxy = gio::DBusProxy::for_bus_sync(
            gio::BusType::Session,
            gio::DBusProxyFlags::NONE,
            None,
            bus_name,
            MPRIS_PATH,
            MPRIS_PLAYER_INTERFACE,
            None::<&gio::Cancellable>,
        )?;
        proxy.set_default_timeout(DBUS_CALL_TIMEOUT_MS);
        let root_proxy = gio::DBusProxy::for_bus_sync(
            gio::BusType::Session,
            gio::DBusProxyFlags::NONE,
            None,
            bus_name,
            MPRIS_PATH,
            MPRIS_ROOT_INTERFACE,
            None::<&gio::Cancellable>,
        )?;
        root_proxy.set_default_timeout(DBUS_CALL_TIMEOUT_MS);
        Ok(Self { bus_name: bus_name.to_string(), proxy, root_proxy })
    }

    fn identity(&self) -> String {
        self.root_proxy
            .cached_property("Identity")
            .and_then(|v| v.get::<String>())
            .unwrap_or_else(|| self.bus_name.clone())
    }

    fn playback_status(&self) -> String {
        self.proxy
            .cached_property("PlaybackStatus")
            .and_then(|v| v.get::<String>())
            .unwrap_or_else(|| "Stopped".to_string())
    }

    fn metadata(&self) -> HashMap<String, glib::Variant> {
        self.proxy
            .cached_property("Metadata")
            .and_then(|v| v.get::<HashMap<String, glib::Variant>>())
            .unwrap_or_default()
    }

    /// Not cached like the properties above - `Position` is explicitly
    /// excluded from `PropertiesChanged` by the MPRIS spec (it would fire
    /// continuously during playback), so it has to be fetched with its
    /// own `Properties.Get` call whenever an accurate value is needed
    /// (see `AudioState::tick`'s doc comment for how it's kept live the
    /// rest of the time).
    fn position_seconds(&self) -> f64 {
        let params = (MPRIS_PLAYER_INTERFACE, "Position").to_variant();
        let Ok(result) = self.proxy.call_sync(
            "org.freedesktop.DBus.Properties.Get",
            Some(&params),
            gio::DBusCallFlags::NONE,
            DBUS_CALL_TIMEOUT_MS,
            None::<&gio::Cancellable>,
        ) else {
            return 0.0;
        };
        // Properties.Get replies with a single out-parameter of type
        // variant ("(v)") wrapping the property's own value ("x",
        // microseconds, per MPRIS) - hence the nested .get::<...>().
        result
            .get::<(glib::Variant,)>()
            .and_then(|(inner,)| inner.get::<i64>())
            .map(|microseconds| microseconds as f64 / 1_000_000.0)
            .unwrap_or(0.0)
    }

    fn call(&self, method_name: &str, params: Option<&glib::Variant>) {
        let _ = self.proxy.call_sync(method_name, params, gio::DBusCallFlags::NONE, DBUS_CALL_TIMEOUT_MS, None::<&gio::Cancellable>);
    }

    fn play_pause(&self) {
        self.call("PlayPause", None);
    }

    fn next_track(&self) {
        self.call("Next", None);
    }

    fn previous_track(&self) {
        self.call("Previous", None);
    }

    fn seek(&self, offset_seconds: f64) {
        let offset_microseconds = (offset_seconds * 1_000_000.0) as i64;
        let params = (offset_microseconds,).to_variant();
        self.call("Seek", Some(&params));
    }
}

struct AudioState {
    players: RefCell<HashMap<String, Rc<MprisPlayer>>>,
    active: RefCell<Option<Rc<MprisPlayer>>>,
    // Kept so `NameOwnerChanged` watching stops - and its closure's
    // `Rc<AudioState>` clone is dropped - when the widget is torn down
    // (see the `connect_destroy` handler in `build_content`), rather than
    // silently watching D-Bus forever for a widget that no longer exists.
    subscription: RefCell<Option<gio::SignalSubscription>>,
    // The 1Hz local position tick - removed on destroy for the same
    // reason `subscription` is cleared there (see that field's comment).
    tick_id: RefCell<Option<glib::SourceId>>,

    // `Position` isn't part of MPRIS's `PropertiesChanged` signal (see
    // `MprisPlayer::position_seconds`'s doc comment), so it's tracked
    // locally instead: resynced from the real value whenever it might
    // have jumped (player picked, track changed, seeked - see `tick`'s
    // doc comment) and ticked forward by hand the rest of the time.
    local_position: Cell<f64>,
    length_seconds: Cell<f64>,

    background: gtk::Picture,
    badge: gtk::Box,
    badge_label: gtk::Label,
    bottom: gtk::Box,
    title_label: gtk::Label,
    artist_label: gtk::Label,
    empty_label: gtk::Label,
    elapsed_label: gtk::Label,
    duration_label: gtk::Label,
    progress_scale: gtk::Scale,
    prev_button: gtk::Button,
    play_button: gtk::Button,
    // The play/pause icon toggles at runtime (see refresh_active_display)
    // - kept as its own handle rather than going through
    // `play_button.set_icon_name()`, which would replace the button's
    // child with GTK's own auto-managed image at the *default* icon
    // size, silently undoing the explicit `set_pixel_size` this image
    // was given in `build_content`.
    play_icon: gtk::Image,
    next_button: gtk::Button,
}

impl AudioState {
    fn set_has_player(&self, has_player: bool) {
        self.badge.set_visible(has_player);
        self.bottom.set_visible(has_player);
        self.empty_label.set_visible(!has_player);
        if !has_player {
            // Reuses the same `background` Picture album art uses (rather
            // than a separate icon widget layered on top) specifically so
            // it's guaranteed to fill the card exactly the same way art
            // already does - a separate overlay-child box ended up short
            // of the card's full bounds, leaving a gap at the bottom
            // (visible as the card's own background color showing
            // through once someone picks a non-default one).
            match empty_state_texture() {
                Some(texture) => self.background.set_paintable(Some(&texture)),
                None => self.background.set_paintable(gtk::gdk::Paintable::NONE),
            }
        }
    }

    /// First `Playing` player wins; with none playing, falls back to
    /// whichever was discovered first. No "pin a specific player" option
    /// yet - that's `AudioSettings`, deferred to a later step.
    fn pick_active_player(self: &Rc<Self>) {
        let candidate = {
            let players = self.players.borrow();
            players
                .values()
                .find(|p| p.playback_status() == "Playing")
                .or_else(|| players.values().next())
                .cloned()
        };
        let changed = {
            let active = self.active.borrow();
            match (active.as_ref(), candidate.as_ref()) {
                (Some(a), Some(c)) => a.bus_name != c.bus_name,
                (None, None) => false,
                _ => true,
            }
        };
        if changed {
            *self.active.borrow_mut() = candidate;
            self.refresh_active_display(true);
        }
    }

    fn add_player(self: &Rc<Self>, bus_name: &str) {
        if self.players.borrow().contains_key(bus_name) {
            return;
        }
        let Ok(player) = MprisPlayer::new(bus_name) else { return };
        let player = Rc::new(player);

        // See `MainThreadOnly`'s own doc comment for why this wrapper is
        // needed just to move an `Rc` into this particular signal.
        let state_wrapped = MainThreadOnly(self.clone());
        let watched_bus_name = bus_name.to_string();
        player.proxy.connect_g_properties_changed(move |_proxy, _changed, _invalidated| {
            // Rust 2021's disjoint closure capture would otherwise
            // capture only the `.0` field (the bare `Rc`, not
            // `Send`/`Sync`) instead of the whole wrapper - this
            // whole-value reference forces capturing `state_wrapped`
            // itself, which is what's actually asserted `Send`/`Sync`.
            let state_wrapped = &state_wrapped;
            state_wrapped.0.on_player_properties_changed(&watched_bus_name);
        });

        // MPRIS's own `Seeked` signal - fired when the position jumps for
        // any reason `PropertiesChanged` wouldn't catch (see
        // `MprisPlayer::position_seconds`), notably the user dragging the
        // seek bar in the player's *own* UI rather than this widget's.
        let seeked_state_wrapped = MainThreadOnly(self.clone());
        let seeked_bus_name = bus_name.to_string();
        player.proxy.connect_g_signal(None, move |_proxy, _sender, signal_name, _parameters| {
            let seeked_state_wrapped = &seeked_state_wrapped; // see the comment above
            if signal_name == "Seeked" {
                seeked_state_wrapped.0.on_player_seeked(&seeked_bus_name);
            }
        });

        self.players.borrow_mut().insert(bus_name.to_string(), player);
        self.pick_active_player();
    }

    fn remove_player(self: &Rc<Self>, bus_name: &str) {
        if self.players.borrow_mut().remove(bus_name).is_none() {
            return;
        }
        let was_active = self.active.borrow().as_ref().is_some_and(|p| p.bus_name == bus_name);
        if was_active {
            *self.active.borrow_mut() = None;
        }
        self.pick_active_player();
    }

    fn on_player_properties_changed(self: &Rc<Self>, bus_name: &str) {
        // A property change (e.g. PlaybackStatus flipping to Playing) can
        // itself hand the "now playing" spot to a different player, so
        // re-pick first - then refresh the display if the player that
        // actually changed (e.g. a track change) is the one shown.
        self.pick_active_player();
        let is_active = self.active.borrow().as_ref().is_some_and(|p| p.bus_name == bus_name);
        if is_active {
            self.refresh_active_display(true);
        }
    }

    fn on_player_seeked(&self, bus_name: &str) {
        let player = self.active.borrow().clone();
        let Some(player) = player.filter(|p| p.bus_name == bus_name) else { return };
        self.local_position.set(player.position_seconds());
        self.progress_scale.set_value(self.local_position.get());
        self.update_time_labels();
    }

    /// Initial scan via `org.freedesktop.DBus.ListNames` - live
    /// add/remove after this comes from the `NameOwnerChanged`
    /// subscription set up in `build_content`.
    fn discover_players(self: &Rc<Self>) {
        let Ok(bus_proxy) = gio::DBusProxy::for_bus_sync(
            gio::BusType::Session,
            gio::DBusProxyFlags::NONE,
            None,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            None::<&gio::Cancellable>,
        ) else {
            return;
        };
        let Ok(result) =
            bus_proxy.call_sync("ListNames", None, gio::DBusCallFlags::NONE, DBUS_CALL_TIMEOUT_MS, None::<&gio::Cancellable>)
        else {
            return;
        };
        let Some((names,)) = result.get::<(Vec<String>,)>() else { return };
        for name in &names {
            if name.starts_with(MPRIS_PREFIX) {
                self.add_player(name);
            }
        }
        self.pick_active_player();
    }

    /// `resync_position` re-fetches the real `Position` from D-Bus rather
    /// than trusting the locally-ticked value - needed whenever it might
    /// have jumped for a reason this widget didn't cause itself (a
    /// different/newly-active player, a track change), but skippable for
    /// a same-track refresh like a language change (see `retranslate`).
    fn refresh_active_display(&self, resync_position: bool) {
        let player = self.active.borrow().clone();
        let Some(player) = player else {
            self.set_has_player(false);
            return;
        };
        self.set_has_player(true);
        self.badge_label.set_label(&player.identity());

        let metadata = player.metadata();
        let title = metadata
            .get("xesam:title")
            .and_then(|v| v.get::<String>())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| i18n::t("widgets.audio.unknown_title"));
        let artists: Vec<String> = metadata.get("xesam:artist").and_then(|v| v.get::<Vec<String>>()).unwrap_or_default();
        let artist_text =
            if artists.is_empty() { i18n::t("widgets.audio.unknown_artist") } else { artists.join(", ") };
        self.title_label.set_label(&truncate(&title, MAX_TITLE_CHARS));
        self.artist_label.set_label(&truncate(&artist_text, MAX_ARTIST_CHARS));

        let length_seconds = metadata.get("mpris:length").and_then(|v| v.get::<i64>()).unwrap_or(0) as f64 / 1_000_000.0;
        self.length_seconds.set(length_seconds);
        self.progress_scale.set_range(0.0, length_seconds.max(1.0));
        self.progress_scale.set_sensitive(length_seconds > 0.0);

        let playing = player.playback_status() == "Playing";
        self.play_icon.set_icon_name(Some(if playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" }));
        self.play_button.set_tooltip_text(Some(&i18n::t(if playing { "widgets.audio.pause" } else { "widgets.audio.play" })));

        if resync_position {
            self.local_position.set(player.position_seconds());
        }
        self.progress_scale.set_value(self.local_position.get());
        self.update_time_labels();

        let art_url = metadata.get("mpris:artUrl").and_then(|v| v.get::<String>()).filter(|s| !s.is_empty());
        self.load_art(art_url);
    }

    fn update_time_labels(&self) {
        self.elapsed_label.set_label(&format_seconds(self.local_position.get()));
        self.duration_label.set_label(&format_seconds(self.length_seconds.get()));
    }

    /// Runs every second (see the timer set up in `build_content`),
    /// advancing the locally-tracked position while the active player is
    /// actually playing - the cheap way to keep the progress bar moving
    /// without polling `Position` over D-Bus once a second forever (see
    /// `MprisPlayer::position_seconds`'s doc comment for why it can't
    /// just be read from a live-updating property instead).
    fn tick(&self) {
        let Some(player) = self.active.borrow().clone() else { return };
        if player.playback_status() != "Playing" {
            return;
        }
        let new_position = (self.local_position.get() + 1.0).min(self.length_seconds.get());
        self.local_position.set(new_position);
        self.progress_scale.set_value(new_position);
        self.update_time_labels();
    }

    /// Handles the progress `Scale`'s `change-value` signal - fired only
    /// for an actual user click/drag on the trough or slider, never for
    /// a programmatic `set_value` call (like `tick`'s or this same
    /// method's own), so there's no feedback loop to guard against.
    fn on_seek_requested(&self, requested_value: f64) {
        let Some(player) = self.active.borrow().clone() else { return };
        let value = requested_value.clamp(0.0, self.length_seconds.get());
        player.seek(value - self.local_position.get());
        self.local_position.set(value);
        self.update_time_labels();
    }

    /// Loads synchronously on the main thread - almost always instant
    /// since MPRIS art URLs are typically a local cached thumbnail
    /// (`file://`). Worth moving to a background thread (as the Python
    /// original does) only if a slow/http `artUrl` is ever seen janking
    /// the UI in practice.
    fn load_art(&self, art_url: Option<String>) {
        let Some(url) = art_url else {
            self.background.set_paintable(gtk::gdk::Paintable::NONE);
            return;
        };
        let file = gio::File::for_uri(&url);
        match gtk::gdk::Texture::from_file(&file) {
            Ok(texture) => self.background.set_paintable(Some(&texture)),
            Err(_) => self.background.set_paintable(gtk::gdk::Paintable::NONE),
        }
    }

    fn retranslate(&self) {
        self.empty_label.set_label(&i18n::t("widgets.audio.empty"));
        // Re-run in full rather than just the empty-state label: the
        // "unknown title/unknown artist" fallbacks and the play/pause
        // tooltip are translated text too, and need updating just as live
        // if that's what's showing. No reason to re-fetch Position for a
        // same-track refresh, hence `false`.
        self.refresh_active_display(false);
    }
}

fn build_content() -> (Rc<AudioState>, gtk::Widget) {
    ensure_css_installed();

    let overlay = gtk::Overlay::new();

    let background = gtk::Picture::new();
    background.set_content_fit(gtk::ContentFit::Cover);
    overlay.set_child(Some(&background));

    let gradient = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    gradient.add_css_class("xeneon-audio-gradient");
    gradient.set_can_target(false);
    gradient.set_hexpand(true);
    gradient.set_vexpand(true);
    overlay.add_overlay(&gradient);

    let badge = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    badge.add_css_class("xeneon-audio-badge");
    badge.set_halign(gtk::Align::Start);
    badge.set_valign(gtk::Align::Start);
    badge.set_margin_start(16);
    badge.set_margin_top(16);
    let badge_dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    badge_dot.add_css_class("xeneon-audio-badge-dot");
    badge_dot.set_valign(gtk::Align::Center);
    badge.append(&badge_dot);
    let badge_label = gtk::Label::new(None);
    badge_label.add_css_class("xeneon-audio-badge-label");
    badge.append(&badge_label);
    overlay.add_overlay(&badge);

    // Just a caption over the empty-state icon (set on `background`
    // itself in `set_has_player` - see that method's comment for why).
    let empty_label = gtk::Label::new(None);
    empty_label.add_css_class("xeneon-audio-empty");
    empty_label.set_halign(gtk::Align::Center);
    empty_label.set_valign(gtk::Align::End);
    empty_label.set_margin_bottom(20);
    overlay.add_overlay(&empty_label);

    let bottom = gtk::Box::new(gtk::Orientation::Vertical, 4);
    bottom.set_valign(gtk::Align::End);
    bottom.set_margin_start(24);
    bottom.set_margin_end(24);
    bottom.set_margin_bottom(20);

    let title_label = gtk::Label::new(None);
    title_label.add_css_class("xeneon-audio-title");
    title_label.set_halign(gtk::Align::Start);
    title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    bottom.append(&title_label);

    let artist_label = gtk::Label::new(None);
    artist_label.add_css_class("xeneon-audio-subtitle");
    artist_label.set_halign(gtk::Align::Start);
    artist_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    artist_label.set_margin_bottom(10);
    bottom.append(&artist_label);

    let progress_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let elapsed_label = gtk::Label::new(Some("0:00"));
    elapsed_label.add_css_class("xeneon-audio-time");
    progress_row.append(&elapsed_label);
    let progress_scale = gtk::Scale::new(gtk::Orientation::Horizontal, gtk::Adjustment::NONE);
    progress_scale.add_css_class("xeneon-audio-progress");
    progress_scale.set_hexpand(true);
    progress_scale.set_draw_value(false);
    progress_scale.set_range(0.0, 1.0);
    progress_row.append(&progress_scale);
    let duration_label = gtk::Label::new(Some("0:00"));
    duration_label.add_css_class("xeneon-audio-time");
    progress_row.append(&duration_label);
    bottom.append(&progress_row);

    let transport_row = gtk::Box::new(gtk::Orientation::Horizontal, 20);
    transport_row.set_halign(gtk::Align::Center);
    transport_row.set_margin_top(8);
    // Built from a plain `Image` (not `Button::from_icon_name`, which
    // only offers GTK's default icon size) so the icon glyph itself can
    // be sized independently of the button's own min-width/height.
    let make_transport_button = |icon_name: &str, pixel_size: i32| {
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class("circular");
        button.add_css_class("xeneon-audio-transport");
        let image = gtk::Image::from_icon_name(icon_name);
        image.set_pixel_size(pixel_size);
        button.set_child(Some(&image));
        (button, image)
    };
    let (prev_button, _prev_icon) = make_transport_button("media-skip-backward-symbolic", 26);
    let (play_button, play_icon) = make_transport_button("media-playback-start-symbolic", 30);
    play_button.add_css_class("xeneon-audio-play-button");
    play_button.remove_css_class("xeneon-audio-transport");
    let (next_button, _next_icon) = make_transport_button("media-skip-forward-symbolic", 26);
    transport_row.append(&prev_button);
    transport_row.append(&play_button);
    transport_row.append(&next_button);
    bottom.append(&transport_row);

    overlay.add_overlay(&bottom);

    let state = Rc::new(AudioState {
        players: RefCell::new(HashMap::new()),
        active: RefCell::new(None),
        subscription: RefCell::new(None),
        tick_id: RefCell::new(None),
        local_position: Cell::new(0.0),
        length_seconds: Cell::new(0.0),
        background,
        badge,
        badge_label,
        bottom,
        title_label,
        artist_label,
        empty_label,
        elapsed_label,
        duration_label,
        progress_scale,
        prev_button,
        play_button,
        play_icon,
        next_button,
    });
    state.set_has_player(false);

    state.progress_scale.connect_change_value({
        let state = state.clone();
        move |_scale, _scroll_type, value| {
            state.on_seek_requested(value);
            glib::Propagation::Proceed
        }
    });
    state.prev_button.connect_clicked({
        let state = state.clone();
        move |_| {
            if let Some(player) = state.active.borrow().clone() {
                player.previous_track();
            }
        }
    });
    state.play_button.connect_clicked({
        let state = state.clone();
        move |_| {
            if let Some(player) = state.active.borrow().clone() {
                player.play_pause();
            }
        }
    });
    state.next_button.connect_clicked({
        let state = state.clone();
        move |_| {
            if let Some(player) = state.active.borrow().clone() {
                player.next_track();
            }
        }
    });

    let tick_id = glib::timeout_add_seconds_local(1, {
        let state = state.clone();
        move || {
            state.tick();
            glib::ControlFlow::Continue
        }
    });
    *state.tick_id.borrow_mut() = Some(tick_id);

    // Live add/remove of players (someone opening/quitting Plexamp,
    // Spotify, a browser tab...) - the initial scan below only covers
    // what's already running at the time this widget is built.
    if let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) {
        let subscription_state = state.clone();
        let subscription = connection.subscribe_to_signal(
            Some("org.freedesktop.DBus"),
            Some("org.freedesktop.DBus"),
            Some("NameOwnerChanged"),
            Some("/org/freedesktop/DBus"),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let Some((name, _old_owner, new_owner)) = signal.parameters.get::<(String, String, String)>() else {
                    return;
                };
                if !name.starts_with(MPRIS_PREFIX) {
                    return;
                }
                if new_owner.is_empty() {
                    subscription_state.remove_player(&name);
                } else {
                    subscription_state.add_player(&name);
                }
            },
        );
        *state.subscription.borrow_mut() = Some(subscription);
    }

    state.discover_players();

    i18n::on_change({
        let state = state.clone();
        move || state.retranslate()
    });

    overlay.connect_destroy({
        let state = state.clone();
        move |_| {
            // Breaks the Rc cycles created above: every MprisPlayer's
            // GDBusProxy holds a `g-properties-changed` closure wrapping
            // a clone of this same `Rc<AudioState>` (see `add_player`),
            // and the `NameOwnerChanged` subscription's closure does too
            // - without this, both would keep this whole widget (and its
            // live D-Bus subscriptions) alive forever after it's removed
            // from the grid, instead of actually being dropped.
            state.players.borrow_mut().clear();
            *state.subscription.borrow_mut() = None;
            *state.active.borrow_mut() = None;
            if let Some(id) = state.tick_id.borrow_mut().take() {
                id.remove();
            }
        }
    });

    (state, overlay.upcast())
}

pub fn spawn() -> WidgetInstance {
    let (_state, content) = build_content();
    registry::instance_without_settings(content)
}

pub fn restore(_data: &serde_json::Value) -> WidgetInstance {
    // No persisted content yet (see the module doc comment) - restoring
    // is identical to spawning fresh.
    spawn()
}
