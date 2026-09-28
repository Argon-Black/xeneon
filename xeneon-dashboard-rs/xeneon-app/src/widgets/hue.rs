// SPDX-License-Identifier: GPL-3.0-or-later
//! Philips Hue widget (SQ footprint): up to four lights, each shown as a
//! rounded "chip" row - a soft-background icon button (tap toggles on/off),
//! the light's name and a status line, and a full-width brightness bar
//! (tap or drag sets the level, and turns the light on if it was off) -
//! matching the mockup agreed with the user before this widget was built
//! (see the design discussion in the memory system: a simple interrupteur
//! row didn't leave room for a real brightness control, so the richer
//! "chip" layout was chosen instead, at the cost of fitting only 4 rows
//! per card instead of 5).
//!
//! Reads and writes go through `hue_bridge.rs`'s shared bridge connection
//! (`Config::hue_bridge_ip`/`hue_username`, paired once from Settings -
//! see that module's own doc comment for why this is shared app-wide
//! rather than per-widget) rather than anything this widget owns itself.
//! An unconfigured bridge, or a bridge that's unreachable/paired-but-
//! empty, shows a short message and a button that jumps straight to
//! Settings (`hue_bridge::open_settings`) instead of the four rows.
//!
//! Step 3 (this pass) adds the settings panel: a "par pièce"/"par lumière"
//! mode toggle plus a checklist (capped at `MAX_ROWS`) of whichever the
//! bridge currently reports, persisted as `SelectionMode` + a list of ids
//! (`hue_bridge::Light::id` in light mode, `hue_bridge::Room::id` in room
//! mode - see `resolve_selection`). A room row addresses the room's
//! `grouped_light` resource (every light in the room moves together, one
//! PUT) rather than any single light in it - see `hue_bridge::Room`'s own
//! doc comment. A freshly-spawned card (or one saved before step 3, whose
//! `selected` list is empty/missing) falls back to `pick_default_lights`'s
//! "first four lights, alphabetically" behavior, in light mode, so
//! existing placed widgets keep showing *something* sensible after this
//! update rather than going blank.
//!
//! **Deliberately not built yet**: step 4 adds a color/color-temperature
//! popover on tapping a light's name (currently a no-op) - not wired to
//! anything here, since what it should open depends on `BulbType`, not yet
//! decided in detail. A room row's status/icon also doesn't aggregate its
//! member lights' actual colors (see `room_to_entry`) - it always renders
//! as a plain dimmable accent-colored entry, a deliberate simplification
//! since color aggregation across a whole room is its own small design
//! question, not needed for the picker mechanics this step is about.
//!
//! Bulb-type detection (`hue_bridge::BulbType`) drives the status line's
//! wording (`"72% · Couleur"` / `"100% · 2700K"` / `"45%"`) and, for a
//! `Dimmable` bulb with no color data at all, nothing else changes yet -
//! tapping its name will eventually need to do nothing (or show a brief
//! "no color control" hint) rather than opening an empty popover, per the
//! design discussion, once step 4 actually builds that popover.
//!
//! Icons are two bundled SVGs (`assets/hue-bulb-icon.svg`,
//! `assets/hue-bulb-off-icon.svg`), rasterized and cached, loaded and
//! tinted the same way `network_sq.rs`'s Wi-Fi/Ethernet icon is - a plain
//! `str::replace` of the source fill color, not GTK's symbolic-icon
//! recoloring (see that module's own doc comment on why: a freedesktop
//! `-symbolic` icon silently failed to render under this machine's actual
//! icon theme). The "on" icon is tinted to each light's own
//! `display_color_hex`; the "off" icon is a fixed, untinted asset (a faint
//! bulb with a slash through it).
//!
//! The brightness bar is a `gtk::DrawingArea` painted with Cairo (a track
//! plus a colored fill, both pill-shaped) driven by a `gtk::GestureDrag` -
//! a plain tap is a drag with ~0 movement, so one gesture handles both
//! "tap to set" and "drag to set" without needing two separate
//! controllers. `drag_update` only redraws locally (immediate visual
//! feedback, no network call per pixel of movement); the actual PUT to the
//! bridge fires once, in `drag_end`.

use gtk::prelude::*;
use log::warn;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Once;

use crate::config_store;
use crate::hue_bridge::{self, BulbType, Light, Room};
use crate::i18n_runtime as i18n;
use crate::widgets::registry::WidgetInstance;

/// Real-hardware round trips (a bridge on the same LAN) are fast, but this
/// still touches the network on every tick - slower than a `/proc` read,
/// same reasoning `weather.rs`/`system_sq.rs` already apply to their own
/// refresh intervals, just a shorter interval than either since a light
/// someone just flipped with a physical switch should show up reasonably
/// promptly.
const REFRESH_INTERVAL_SECONDS: u32 = 4;
/// How many rows fit in an SQ card with this row's height (see the module
/// doc comment on why this design fits 4, not 5) - also the hard cap
/// `pick_default_lights` and, later, step 3's settings panel both respect.
const MAX_ROWS: usize = 4;

const ICON_ON_PATH: &str = "assets/hue-bulb-icon.svg";
const ICON_OFF_PATH: &str = "assets/hue-bulb-off-icon.svg";
const ICON_SOURCE_FILL: &str = "#ffffff";
/// Same reasoning as `network_sq.rs`'s own `ICON_RASTER_PX`: well above
/// the on-screen size so the icon stays crisp rather than an upscaled
/// bitmap.
const ICON_RASTER_PX: i32 = 96;
const ROW_ICON_PX: i32 = 20;
const HEADER_ICON_PX: i32 = 18;
/// Fixed - matches `network_sq.rs`/`system_sq.rs`'s own badge accent,
/// used here for the header icon and the "N/N allumées" badge, neither of
/// which is user-customizable yet (no settings panel at all in step 2).
const ACCENT_COLOR_HEX: &str = "#f2a541";
const ROW_HEIGHT_PX: i32 = 60;
const ROW_ICON_CIRCLE_PX: i32 = 40;
const BAR_HEIGHT_PX: i32 = 36;

thread_local! {
    // The "off" icon never changes color, so it only ever needs one
    // cached texture - same shape as `network_sq.rs`'s `ICON_TEXTURES`.
    static OFF_ICON_TEXTURE: RefCell<Option<gtk::gdk::Texture>> = const { RefCell::new(None) };
    // The "on" icon is tinted per light, so its cache is keyed by the hex
    // color - same shape as `network_sq.rs`'s `TINTED_ICON_TEXTURES`.
    static ON_ICON_TEXTURES: RefCell<HashMap<String, Option<gtk::gdk::Texture>>> = RefCell::new(HashMap::new());
}

fn load_off_icon() -> Option<gtk::gdk::Texture> {
    OFF_ICON_TEXTURE.with(|cache| {
        if let Some(texture) = cache.borrow().as_ref() {
            return Some(texture.clone());
        }
        let full_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ICON_OFF_PATH);
        let texture = gtk::gdk_pixbuf::Pixbuf::from_file_at_size(&full_path, ICON_RASTER_PX, ICON_RASTER_PX)
            .map(|pixbuf| gtk::gdk::Texture::for_pixbuf(&pixbuf))
            .inspect_err(|err| warn!("failed to load {}: {err}", full_path.display()))
            .ok();
        *cache.borrow_mut() = texture.clone();
        texture
    })
}

fn load_on_icon(hex_color: &str) -> Option<gtk::gdk::Texture> {
    ON_ICON_TEXTURES.with(|cache| {
        if let Some(texture) = cache.borrow().get(hex_color) {
            return texture.clone();
        }
        let full_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ICON_ON_PATH);
        let texture = std::fs::read_to_string(&full_path)
            .inspect_err(|err| warn!("failed to read {}: {err}", full_path.display()))
            .ok()
            .and_then(|svg_text| {
                let tinted = svg_text.replace(ICON_SOURCE_FILL, hex_color);
                let stream = gtk::gio::MemoryInputStream::from_bytes(&gtk::glib::Bytes::from_owned(tinted.into_bytes()));
                gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(&stream, ICON_RASTER_PX, ICON_RASTER_PX, true, gtk::gio::Cancellable::NONE)
                    .map(|pixbuf| gtk::gdk::Texture::for_pixbuf(&pixbuf))
                    .inspect_err(|err| warn!("failed to rasterize {} tinted {hex_color}: {err}", full_path.display()))
                    .ok()
            });
        cache.borrow_mut().insert(hex_color.to_string(), texture.clone());
        texture
    })
}

static INSTALL_CSS: Once = Once::new();

fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!(
            ".xeneon-hue-title {{ font-weight: 500; color: #ffffff; }}\n\
             .xeneon-hue-badge {{ background-color: rgba(242, 165, 65, 0.15); \
             border-radius: 13px; padding: 5px 12px; }}\n\
             .xeneon-hue-badge-label {{ font-size: 14px; font-weight: 500; color: {ACCENT_COLOR_HEX}; }}\n\
             .xeneon-hue-row {{ background-color: rgba(255, 255, 255, 0.05); border-radius: 14px; }}\n\
             .xeneon-hue-name {{ font-size: 14px; font-weight: 500; color: #ffffff; }}\n\
             .xeneon-hue-empty-message {{ color: rgba(255, 255, 255, 0.6); font-size: 14px; }}"
        ));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

/// Paints one row's icon-circle background: a soft, low-opacity fill in
/// `rgb` when the light behind this row is on, a plain neutral gray when
/// it's off or the row is unused (fewer than `MAX_ROWS` lights available) -
/// mirrors the mockup's "tinted circle behind a colored bulb glyph" look.
fn draw_icon_circle(cr: &gtk::cairo::Context, width: f64, height: f64, on: bool, rgb: (f64, f64, f64)) {
    let radius = width.min(height) / 2.0;
    cr.arc(width / 2.0, height / 2.0, radius, 0.0, std::f64::consts::TAU);
    if on {
        let (r, g, b) = rgb;
        cr.set_source_rgba(r, g, b, 0.18);
    } else {
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.06);
    }
    let _ = cr.fill();
}

/// Paints the brightness bar: a faint full-width pill track, then a
/// colored pill fill over `fraction` of it - same round-capped-stroke
/// technique `system_sq.rs::draw_bar` uses for its thin gauges, just at
/// this bar's own much taller height so it reads as a real slider.
fn draw_bar(cr: &gtk::cairo::Context, width: f64, height: f64, fraction: f64, rgb: (f64, f64, f64)) {
    let radius = height / 2.0;
    let y = height / 2.0;

    cr.set_line_cap(gtk::cairo::LineCap::Round);
    cr.set_line_width(height);
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.08);
    cr.move_to(radius, y);
    cr.line_to((width - radius).max(radius), y);
    let _ = cr.stroke();

    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.0 {
        let (r, g, b) = rgb;
        cr.set_source_rgba(r, g, b, 0.35);
        let end_x = (radius + (width - 2.0 * radius) * fraction).max(radius);
        cr.move_to(radius, y);
        cr.line_to(end_x, y);
        let _ = cr.stroke();
    }
}

/// Converts a `#rrggbb` string to the `(r, g, b)` 0.0-1.0 floats Cairo
/// wants - this widget's colors always come from `hue_bridge`'s own
/// `display_color_hex`/swatch constants (never user input), so a malformed
/// string just falls back to a neutral gray rather than needing to surface
/// a parse error anywhere.
fn hex_to_rgb(hex: &str) -> (f64, f64, f64) {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return (0.6, 0.6, 0.6);
    }
    let component = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).unwrap_or(153) as f64 / 255.0;
    (component(0..2), component(2..4), component(4..6))
}

/// Status line text for one entry - wording depends on `BulbType` (see the
/// module doc comment): a color light names its capability generically
/// ("Couleur") rather than trying to name the actual hue, a
/// color-temperature light shows its Kelvin value, a dimmable light (and
/// every room entry, which always carries `BulbType::Dimmable` - see
/// `room_to_entry`) shows only the percentage. Any type shows the plain
/// "off" text while `on` is `false`, since a bridge keeps reporting the
/// brightness/color a light will return to rather than resetting it to
/// zero.
fn status_text(entry: &CardEntry) -> String {
    if !entry.on {
        return i18n::t("widgets.hue.status.off");
    }
    let percent = entry.brightness_percent.round() as i64;
    match entry.bulb_type {
        BulbType::Color => i18n::t_args("widgets.hue.status.color", &[("percent", &percent.to_string())]),
        BulbType::ColorTemperature => {
            let kelvin = entry.mirek.map(|mirek| (1_000_000.0 / mirek).round() as i64).unwrap_or(0);
            i18n::t_args("widgets.hue.status.temperature", &[("percent", &percent.to_string()), ("kelvin", &kelvin.to_string())])
        }
        BulbType::Dimmable => i18n::t_args("widgets.hue.status.dimmable", &[("percent", &percent.to_string())]),
    }
}

/// Picks which lights a card shows when its own selection is empty - the
/// first `MAX_ROWS` lights, alphabetically, the same for every such card.
/// Used both as the very first cut's only behavior (step 2) and, now, as
/// light-mode's fallback when nothing has been explicitly picked yet (a
/// freshly spawned card, or one saved before step 3 existed) - see the
/// module doc comment. Sorting by name (rather than bridge order, closer
/// to "creation order" and not meaningful to a user) at least makes the
/// arbitrary choice deterministic and easy to reason about while testing.
fn pick_default_lights(mut lights: Vec<Light>) -> Vec<Light> {
    lights.sort_by(|a, b| a.name.cmp(&b.name));
    lights.truncate(MAX_ROWS);
    lights
}

/// Whether a card shows individually-picked lights or whole rooms (each
/// addressed through its `grouped_light` resource) - persisted as a plain
/// `"light"`/`"room"` string (see `HueState::to_dict`/`apply_dict`) rather
/// than deriving `serde::Serialize` on the enum, matching how every other
/// widget in this codebase hand-writes its own `to_dict`/`apply_dict`
/// instead of deriving one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionMode {
    Light,
    Room,
}

/// What a row is currently bound to and, therefore, which bridge endpoint
/// a toggle/drag on it should PUT to - `hue_bridge::set_light` for a
/// single light, `hue_bridge::set_grouped_light` for a whole room. Carries
/// the id itself so the toggle/drag handlers (see `build_content`) don't
/// need a second lookup back into `HueState::all_lights`/`all_rooms` to
/// find out which bridge call to make.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CardTarget {
    Light(String),
    Room(String),
}

/// One row's worth of display data, regardless of whether it came from a
/// single light or a whole room - lets `apply_entry_to_row` (and
/// everything downstream of it: the draw funcs, the status text) stay
/// oblivious to `SelectionMode`, same "normalize once, render generically"
/// shape `hue_bridge::Light` itself already gives the widget for a single
/// light.
struct CardEntry {
    target: CardTarget,
    name: String,
    on: bool,
    brightness_percent: f64,
    bulb_type: BulbType,
    display_color_hex: String,
    mirek: Option<f64>,
}

fn light_to_entry(light: &Light) -> CardEntry {
    CardEntry {
        target: CardTarget::Light(light.id.clone()),
        name: light.name.clone(),
        on: light.on,
        brightness_percent: light.brightness_percent,
        bulb_type: light.bulb_type,
        display_color_hex: light.display_color_hex.clone(),
        mirek: light.mirek,
    }
}

/// A room always renders as a plain dimmable, accent-colored entry - see
/// the module doc comment on why this step doesn't attempt to aggregate
/// its member lights' actual colors into one on-card swatch.
fn room_to_entry(room: &Room) -> CardEntry {
    CardEntry {
        target: CardTarget::Room(room.grouped_light_id.clone()),
        name: room.name.clone(),
        on: room.on,
        brightness_percent: room.brightness_percent,
        bulb_type: BulbType::Dimmable,
        display_color_hex: ACCENT_COLOR_HEX.to_string(),
        mirek: None,
    }
}

/// Resolves the card's current settings (`mode` + `selected_ids`) against
/// the bridge's latest data into the up-to-`MAX_ROWS` entries to actually
/// show - the one place `HueState::refresh` needs to call to go from "raw
/// bridge data" to "what this specific card displays". An id in
/// `selected_ids` naming a light/room that no longer exists (deleted or
/// renamed on the bridge since this card was configured) is simply
/// skipped, same "stale setting doesn't error, just quietly does less"
/// tolerance `system_sq.rs`'s disk-path setting already has.
fn resolve_selection(mode: SelectionMode, selected_ids: &[String], all_lights: &[Light], all_rooms: &[Room]) -> Vec<CardEntry> {
    match mode {
        SelectionMode::Light if selected_ids.is_empty() => pick_default_lights(all_lights.to_vec()).iter().map(light_to_entry).collect(),
        SelectionMode::Light => selected_ids
            .iter()
            .filter_map(|id| all_lights.iter().find(|light| &light.id == id))
            .take(MAX_ROWS)
            .map(light_to_entry)
            .collect(),
        SelectionMode::Room if selected_ids.is_empty() => {
            let mut rooms: Vec<&Room> = all_rooms.iter().collect();
            rooms.sort_by(|a, b| a.name.cmp(&b.name));
            rooms.truncate(MAX_ROWS);
            rooms.iter().map(|room| room_to_entry(room)).collect()
        }
        SelectionMode::Room => selected_ids
            .iter()
            .filter_map(|id| all_rooms.iter().find(|room| &room.id == id))
            .take(MAX_ROWS)
            .map(room_to_entry)
            .collect(),
    }
}

/// One row's live widgets plus which light/room it currently shows -
/// `None` while unused (fewer than `MAX_ROWS` entries selected) or before
/// the first successful fetch, in which case the row stays hidden.
struct LightRow {
    container: gtk::Box,
    icon_area: gtk::DrawingArea,
    icon_image: gtk::Image,
    name_label: gtk::Label,
    status_label: gtk::Label,
    bar_area: gtk::DrawingArea,
    target: RefCell<Option<CardTarget>>,
    on: Cell<bool>,
    fraction: Cell<f64>,
    rgb: Cell<(f64, f64, f64)>,
}

fn build_row() -> LightRow {
    let container = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    container.add_css_class("xeneon-hue-row");
    container.set_size_request(-1, ROW_HEIGHT_PX);
    container.set_margin_start(10);
    container.set_margin_end(10);

    let icon_overlay = gtk::Overlay::new();
    icon_overlay.set_valign(gtk::Align::Center);
    let icon_area = gtk::DrawingArea::new();
    icon_area.set_size_request(ROW_ICON_CIRCLE_PX, ROW_ICON_CIRCLE_PX);
    icon_overlay.set_child(Some(&icon_area));
    let icon_image = gtk::Image::new();
    icon_image.set_pixel_size(ROW_ICON_PX);
    icon_image.set_halign(gtk::Align::Center);
    icon_image.set_valign(gtk::Align::Center);
    icon_image.set_can_target(false);
    icon_overlay.add_overlay(&icon_image);
    container.append(&icon_overlay);

    let text_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text_box.set_valign(gtk::Align::Center);
    text_box.set_hexpand(false);
    text_box.set_size_request(104, -1);
    let name_label = gtk::Label::new(None);
    name_label.add_css_class("xeneon-hue-name");
    name_label.set_halign(gtk::Align::Start);
    name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text_box.append(&name_label);
    let status_label = gtk::Label::new(None);
    status_label.set_halign(gtk::Align::Start);
    text_box.append(&status_label);
    container.append(&text_box);

    let bar_area = gtk::DrawingArea::new();
    bar_area.set_hexpand(true);
    bar_area.set_size_request(-1, BAR_HEIGHT_PX);
    bar_area.set_valign(gtk::Align::Center);
    container.append(&bar_area);

    LightRow {
        container,
        icon_area,
        icon_image,
        name_label,
        status_label,
        bar_area,
        target: RefCell::new(None),
        on: Cell::new(false),
        fraction: Cell::new(0.0),
        rgb: Cell::new((0.6, 0.6, 0.6)),
    }
}

/// All of this widget's live state - one instance per placed card. Owns
/// the bridge connection's *view* only (the connection itself lives in
/// `Config`/`hue_bridge.rs`, shared) - this struct remembers this card's
/// own selection settings, the last full fetch (so the settings panel has
/// something to build its checklist from without a fetch of its own - see
/// `on_data_changed`), and the up-to-`MAX_ROWS` entries currently shown.
struct HueState {
    badge_label: gtk::Label,
    rows_box: gtk::Box,
    empty_box: gtk::Box,
    empty_message: gtk::Label,
    rows: Vec<Rc<LightRow>>,
    /// Bumped on every `refresh()` call, checked when its async fetch
    /// lands - a refresh superseded by a newer one (or by a toggle/drag
    /// that already knows the answer) discards its result rather than
    /// overwriting fresher data, same technique `weather.rs`'s own
    /// `fetch_generation` uses.
    generation: Cell<u64>,

    mode: Cell<SelectionMode>,
    /// `hue_bridge::Light::id`s in light mode, `hue_bridge::Room::id`s in
    /// room mode - meaning depends entirely on `mode`, same as
    /// `resolve_selection`'s own parameter.
    selected_ids: RefCell<Vec<String>>,
    /// The bridge's full light/room list as of the last successful fetch -
    /// what `resolve_selection` picks from, and what the settings panel's
    /// checklist is built from (see `on_data_changed`). Empty until the
    /// very first fetch lands.
    all_lights: RefCell<Vec<Light>>,
    all_rooms: RefCell<Vec<Room>>,
    /// Called at the end of every successful `refresh()` - lets the
    /// settings panel (built once, before any fetch has necessarily
    /// completed) rebuild its checklist as soon as real data arrives,
    /// without polling or a fetch of its own. Never called on a failed
    /// fetch, so the panel keeps showing its last-known-good list rather
    /// than flashing empty on a momentary network hiccup.
    data_listeners: RefCell<Vec<Box<dyn Fn()>>>,
}

impl HueState {
    fn show_empty(&self, message_key: &str) {
        self.rows_box.set_visible(false);
        self.empty_box.set_visible(true);
        self.empty_message.set_label(&i18n::t(message_key));
        self.badge_label.set_label("");
    }

    fn show_rows(&self, entries: &[CardEntry]) {
        self.rows_box.set_visible(true);
        self.empty_box.set_visible(false);

        let on_count = entries.iter().filter(|e| e.on).count();
        self.badge_label.set_label(&i18n::t_args(
            "widgets.hue.badge",
            &[("on", &on_count.to_string()), ("total", &entries.len().to_string())],
        ));

        for (row, entry) in self.rows.iter().zip(entries.iter()) {
            apply_entry_to_row(row, entry);
            row.container.set_visible(true);
        }
        for row in self.rows.iter().skip(entries.len()) {
            *row.target.borrow_mut() = None;
            row.container.set_visible(false);
        }
    }

    fn on_data_changed(&self, listener: impl Fn() + 'static) {
        self.data_listeners.borrow_mut().push(Box::new(listener));
    }

    /// Re-reads the bridge (if configured) and updates every row -
    /// runs on every timer tick, right after a toggle/brightness change,
    /// and once up front in `build_content`.
    fn refresh(self: &Rc<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);

        let config = config_store::get();
        let (Some(ip), Some(username)) = (config.hue_bridge_ip, config.hue_username) else {
            self.show_empty("widgets.hue.empty.not_configured");
            return;
        };

        let state = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || hue_bridge::fetch_resources(&ip, &username)).await;
            if generation != state.generation.get() {
                return;
            }
            match result {
                Ok(Ok(resources)) => {
                    let lights = hue_bridge::parse_lights(&resources);
                    let rooms = hue_bridge::parse_rooms(&resources, &lights);
                    let entries = resolve_selection(state.mode.get(), &state.selected_ids.borrow(), &lights, &rooms);
                    *state.all_lights.borrow_mut() = lights;
                    *state.all_rooms.borrow_mut() = rooms;

                    if entries.is_empty() {
                        state.show_empty("widgets.hue.empty.no_lights");
                    } else {
                        state.show_rows(&entries);
                    }
                    for listener in state.data_listeners.borrow().iter() {
                        listener();
                    }
                }
                _ => state.show_empty("widgets.hue.empty.unreachable"),
            }
        });
    }

    fn to_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "mode": match self.mode.get() { SelectionMode::Light => "light", SelectionMode::Room => "room" },
            "selected": self.selected_ids.borrow().clone(),
        })
    }

    fn apply_dict(self: &Rc<Self>, data: &serde_json::Value) {
        if let Some(mode) = data.get("mode").and_then(|v| v.as_str()) {
            self.mode.set(if mode == "room" { SelectionMode::Room } else { SelectionMode::Light });
        }
        if let Some(selected) = data.get("selected").and_then(|v| v.as_array()) {
            *self.selected_ids.borrow_mut() = selected.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
        }
    }
}

/// Updates one row's widgets to show `entry`'s current state - shared by
/// `HueState::show_rows` (a full refresh) and the toggle/drag handlers'
/// optimistic local update (see `build_content`'s gesture wiring), so both
/// paths render an entry exactly the same way.
fn apply_entry_to_row(row: &Rc<LightRow>, entry: &CardEntry) {
    *row.target.borrow_mut() = Some(entry.target.clone());
    row.on.set(entry.on);
    row.fraction.set((entry.brightness_percent / 100.0).clamp(0.0, 1.0));
    row.rgb.set(hex_to_rgb(&entry.display_color_hex));

    row.name_label.set_label(&entry.name);
    // Pango's `<span color="...">` only accepts `#rrggbb`/`#rrggbbaa` or a
    // named color, never a CSS `rgba(...)` function - `#ffffff66` (~40%
    // alpha) is the off-state equivalent of the on-state's plain hex.
    let status_hex = if entry.on { entry.display_color_hex.clone() } else { "#ffffff66".to_string() };
    row.status_label.set_markup(&format!(
        "<span size=\"small\" color=\"{}\">{}</span>",
        gtk::glib::markup_escape_text(&status_hex),
        gtk::glib::markup_escape_text(&status_text(entry))
    ));

    let texture = if entry.on { load_on_icon(&entry.display_color_hex) } else { load_off_icon() };
    row.icon_image.set_paintable(texture.as_ref());
    row.icon_area.queue_draw();
    row.bar_area.queue_draw();
}

fn build_content() -> (Rc<HueState>, gtk::Widget) {
    ensure_css_installed();

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let css_class = format!("xeneon-hue-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.add_css_class(&css_class);
    root.set_margin_start(14);
    root.set_margin_end(14);
    root.set_margin_top(12);
    root.set_margin_bottom(10);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let header_icon = gtk::Image::new();
    header_icon.set_pixel_size(HEADER_ICON_PX);
    header_icon.set_paintable(load_on_icon(ACCENT_COLOR_HEX).as_ref());
    header.append(&header_icon);
    let title_label = gtk::Label::new(Some(&i18n::t("widgets.hue.title")));
    title_label.add_css_class("xeneon-hue-title");
    header.append(&title_label);
    let header_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    header_spacer.set_hexpand(true);
    header.append(&header_spacer);
    let badge = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    badge.add_css_class("xeneon-hue-badge");
    let badge_label = gtk::Label::new(None);
    badge_label.add_css_class("xeneon-hue-badge-label");
    badge.append(&badge_label);
    header.append(&badge);
    root.append(&header);

    let rows_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    rows_box.set_margin_top(4);
    let mut rows = Vec::with_capacity(MAX_ROWS);
    for _ in 0..MAX_ROWS {
        let row = Rc::new(build_row());
        row.container.set_visible(false);
        rows_box.append(&row.container);
        rows.push(row);
    }
    root.append(&rows_box);

    let empty_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
    empty_box.set_valign(gtk::Align::Center);
    empty_box.set_vexpand(true);
    empty_box.set_visible(false);
    let empty_message = gtk::Label::new(None);
    empty_message.add_css_class("xeneon-hue-empty-message");
    empty_message.set_justify(gtk::Justification::Center);
    empty_message.set_wrap(true);
    empty_box.append(&empty_message);
    let empty_button = gtk::Button::with_label(&i18n::t("widgets.hue.empty.open_settings"));
    empty_button.set_halign(gtk::Align::Center);
    empty_button.connect_clicked(|_| hue_bridge::open_settings());
    empty_box.append(&empty_button);
    root.append(&empty_box);

    let state = Rc::new(HueState {
        badge_label,
        rows_box,
        empty_box,
        empty_message,
        rows,
        generation: Cell::new(0),
        mode: Cell::new(SelectionMode::Light),
        selected_ids: RefCell::new(Vec::new()),
        all_lights: RefCell::new(Vec::new()),
        all_rooms: RefCell::new(Vec::new()),
        data_listeners: RefCell::new(Vec::new()),
    });

    for row in &state.rows {
        row.icon_area.set_draw_func({
            let row = row.clone();
            move |_area, cr, width, height| draw_icon_circle(cr, width as f64, height as f64, row.on.get(), row.rgb.get())
        });
        row.bar_area.set_draw_func({
            let row = row.clone();
            move |_area, cr, width, height| draw_bar(cr, width as f64, height as f64, row.fraction.get(), row.rgb.get())
        });

        // Icon tap: toggles on/off. A plain `GestureClick`, not a drag -
        // there's nothing to drag here, just a button-shaped area.
        let toggle_click = gtk::GestureClick::new();
        toggle_click.connect_released({
            let row = row.clone();
            let state = state.clone();
            move |gesture, _n_press, _x, _y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let Some(target) = row.target.borrow().clone() else { return };
                let new_on = !row.on.get();
                // Optimistic local update first (instant visual feedback),
                // then the real PUT, then a full refresh to reconcile -
                // see the module doc comment on this row's own
                // `on`/`fraction`/`rgb` cells being the single source the
                // draw funcs read from.
                row.on.set(new_on);
                let texture = if new_on { load_on_icon(&hex_from_rgb(row.rgb.get())) } else { load_off_icon() };
                row.icon_image.set_paintable(texture.as_ref());
                row.icon_area.queue_draw();

                let config = config_store::get();
                let (Some(ip), Some(username)) = (config.hue_bridge_ip, config.hue_username) else { return };
                let brightness = row.fraction.get() * 100.0;
                let state = state.clone();
                gtk::glib::spawn_future_local(async move {
                    let _ = gtk::gio::spawn_blocking(move || put_target(&ip, &username, &target, new_on, brightness)).await;
                    state.refresh();
                });
            }
        });
        row.icon_area.add_controller(toggle_click);

        // Brightness bar: a `GestureDrag` doubles as tap-to-set (a tap is
        // just a drag with ~0 movement) - see the module doc comment.
        let drag = gtk::GestureDrag::new();
        let drag_start_x: Rc<Cell<f64>> = Rc::new(Cell::new(0.0));
        drag.connect_drag_begin({
            let drag_start_x = drag_start_x.clone();
            move |gesture, x, _y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                drag_start_x.set(x);
            }
        });
        drag.connect_drag_update({
            let row = row.clone();
            let drag_start_x = drag_start_x.clone();
            move |_gesture, offset_x, _offset_y| {
                let width = row.bar_area.width() as f64;
                if width <= 0.0 {
                    return;
                }
                let x = (drag_start_x.get() + offset_x).clamp(0.0, width);
                row.fraction.set(x / width);
                row.bar_area.queue_draw();
            }
        });
        drag.connect_drag_end({
            let row = row.clone();
            let state = state.clone();
            let drag_start_x = drag_start_x.clone();
            move |_gesture, offset_x, _offset_y| {
                let width = row.bar_area.width() as f64;
                if width <= 0.0 {
                    return;
                }
                let x = (drag_start_x.get() + offset_x).clamp(0.0, width);
                let fraction = x / width;
                row.fraction.set(fraction);
                row.bar_area.queue_draw();

                let Some(target) = row.target.borrow().clone() else { return };
                let brightness = fraction * 100.0;
                // Tapping/dragging the bar always turns the light on, even
                // from 0% - matches the mockup's "tapping the bar also
                // switches it on" requirement.
                let new_on = true;
                row.on.set(new_on);
                let texture = load_on_icon(&hex_from_rgb(row.rgb.get()));
                row.icon_image.set_paintable(texture.as_ref());
                row.icon_area.queue_draw();

                let config = config_store::get();
                let (Some(ip), Some(username)) = (config.hue_bridge_ip, config.hue_username) else { return };
                let state = state.clone();
                gtk::glib::spawn_future_local(async move {
                    let _ = gtk::gio::spawn_blocking(move || put_target(&ip, &username, &target, new_on, brightness)).await;
                    state.refresh();
                });
            }
        });
        row.bar_area.add_controller(drag);
    }

    state.refresh();

    let timeout_id = gtk::glib::timeout_add_seconds_local(REFRESH_INTERVAL_SECONDS, {
        let state = state.clone();
        move || {
            state.refresh();
            gtk::glib::ControlFlow::Continue
        }
    });
    root.connect_destroy({
        let timeout_id = RefCell::new(Some(timeout_id));
        move |_| {
            if let Some(id) = timeout_id.borrow_mut().take() {
                id.remove();
            }
        }
    });

    (state, root.upcast())
}

/// Sends one on/brightness PUT to whichever bridge endpoint `target`
/// addresses - a single light or a whole room's `grouped_light` - shared
/// by the icon-toggle and brightness-drag handlers in `build_content` so
/// neither has to duplicate this branch itself.
fn put_target(ip: &str, username: &str, target: &CardTarget, on: bool, brightness_percent: f64) -> Result<(), String> {
    match target {
        CardTarget::Light(id) => hue_bridge::set_light(ip, username, id, on, brightness_percent),
        CardTarget::Room(grouped_light_id) => hue_bridge::set_grouped_light(ip, username, grouped_light_id, on, brightness_percent),
    }
}

/// Re-derives a `#rrggbb` string from the `(r, g, b)` 0.0-1.0 floats
/// `LightRow::rgb` stores - the inverse of `hex_to_rgb`, needed because
/// the optimistic toggle handlers only have the row's already-converted
/// float triple on hand, not the original hex string `apply_light_to_row`
/// last received (the row doesn't keep that string around, only its
/// Cairo-ready float form).
fn hex_from_rgb((r, g, b): (f64, f64, f64)) -> String {
    let to_byte = |c: f64| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", to_byte(r), to_byte(g), to_byte(b))
}

/// Builds the settings panel: a "par pièce"/"par lumière" mode toggle,
/// then a scrollable checklist of whichever the current mode offers -
/// rooms or lights, up to `MAX_ROWS` checked at once (the rest disabled
/// once that cap is reached, rather than showing an error). The list is
/// rebuilt from `state.all_lights`/`all_rooms` both once up front (using
/// whatever's already cached - empty on a card added seconds ago, whose
/// first fetch hasn't landed yet) and every time `state.on_data_changed`
/// fires, so it fills in on its own once real data arrives rather than
/// needing the popover reopened.
fn build_settings(state: Rc<HueState>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(240, -1);

    let mode_label = gtk::Label::new(Some(&i18n::t("widgets.hue.settings.mode.title")));
    mode_label.set_hexpand(true);
    mode_label.set_halign(gtk::Align::Start);
    let room_button = gtk::ToggleButton::with_label(&i18n::t("widgets.hue.settings.mode.room"));
    let light_button = gtk::ToggleButton::with_label(&i18n::t("widgets.hue.settings.mode.light"));
    light_button.set_group(Some(&room_button));
    room_button.set_active(state.mode.get() == SelectionMode::Room);
    light_button.set_active(state.mode.get() == SelectionMode::Light);
    let mode_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    mode_row.append(&mode_label);
    mode_row.append(&room_button);
    mode_row.append(&light_button);
    root.append(&mode_row);

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_propagate_natural_height(true);
    scroller.set_max_content_height(220);
    let list_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    scroller.set_child(Some(&list_box));
    root.append(&scroller);

    let loading_label = gtk::Label::new(Some(&i18n::t("widgets.hue.settings.loading")));
    loading_label.add_css_class("dim-label");
    loading_label.set_halign(gtk::Align::Start);

    // Rebuilds `list_box` from whichever the current mode offers -
    // `Rc<dyn Fn()>` (not a plain closure) since it needs to be called
    // both immediately below and from `state.on_data_changed`'s own
    // `'static` listener list, and it recursively needs to call itself
    // indirectly through the checkbox toggle handlers below (unchecking
    // one once the cap is no longer reached should re-enable the rest).
    let rebuild: Rc<dyn Fn()> = {
        let state = state.clone();
        let list_box = list_box.clone();
        let loading_label = loading_label.clone();
        Rc::new(move || {
            while let Some(child) = list_box.first_child() {
                list_box.remove(&child);
            }

            // (id, label) pairs - a room's label is just its name; a
            // light's label includes its room, when known, so two
            // same-named lights in different rooms (or an unassigned
            // one) stay distinguishable in the list.
            let items: Vec<(String, String)> = match state.mode.get() {
                SelectionMode::Room => {
                    let mut rooms = state.all_rooms.borrow().clone();
                    rooms.sort_by(|a, b| a.name.cmp(&b.name));
                    // "(N)" rather than a translated "N lights" - a bare
                    // count sidesteps French/English singular-plural
                    // agreement for a detail this minor, while `light_ids`
                    // (otherwise unread) still earns its place here.
                    rooms.into_iter().map(|room| (room.id, format!("{} ({})", room.name, room.light_ids.len()))).collect()
                }
                SelectionMode::Light => {
                    let mut lights = state.all_lights.borrow().clone();
                    lights.sort_by(|a, b| a.name.cmp(&b.name));
                    lights
                        .into_iter()
                        .map(|light| {
                            let label = match &light.room_name {
                                Some(room_name) => format!("{} · {room_name}", light.name),
                                None => light.name.clone(),
                            };
                            (light.id, label)
                        })
                        .collect()
                }
            };

            if items.is_empty() {
                list_box.append(&loading_label);
                return;
            }

            let selected = state.selected_ids.borrow().clone();
            let check_buttons: Rc<RefCell<Vec<gtk::CheckButton>>> = Rc::new(RefCell::new(Vec::new()));
            for (id, label) in items {
                let check = gtk::CheckButton::with_label(&label);
                check.set_active(selected.contains(&id));
                list_box.append(&check);
                check_buttons.borrow_mut().push(check.clone());

                check.connect_toggled({
                    let state = state.clone();
                    let id = id.clone();
                    let check_buttons = check_buttons.clone();
                    move |check| {
                        let mut selected = state.selected_ids.borrow_mut();
                        if check.is_active() {
                            if !selected.contains(&id) {
                                selected.push(id.clone());
                            }
                        } else {
                            selected.retain(|existing| existing != &id);
                        }
                        let at_cap = selected.len() >= MAX_ROWS;
                        drop(selected);

                        // Disables every unchecked box once the cap is
                        // reached (rather than rejecting a click past it),
                        // and re-enables them all the moment a selection
                        // drops back under the cap - simpler than an error
                        // message, and makes the limit discoverable just
                        // by trying to check a 5th box.
                        for other in check_buttons.borrow().iter() {
                            if !other.is_active() {
                                other.set_sensitive(!at_cap);
                            }
                        }

                        state.refresh();
                    }
                });
            }

            // The cap may already be reached from a restored selection -
            // apply the same disabling pass once up front, not just from
            // inside a toggle handler.
            let at_cap = state.selected_ids.borrow().len() >= MAX_ROWS;
            if at_cap {
                for check in check_buttons.borrow().iter() {
                    if !check.is_active() {
                        check.set_sensitive(false);
                    }
                }
            }
        })
    };
    rebuild();
    state.on_data_changed({
        let rebuild = rebuild.clone();
        move || rebuild()
    });

    room_button.connect_toggled({
        let state = state.clone();
        let rebuild = rebuild.clone();
        move |button| {
            if !button.is_active() {
                return;
            }
            state.mode.set(SelectionMode::Room);
            state.selected_ids.borrow_mut().clear();
            rebuild();
            state.refresh();
        }
    });
    light_button.connect_toggled({
        let state = state.clone();
        let rebuild = rebuild.clone();
        move |button| {
            if !button.is_active() {
                return;
            }
            state.mode.set(SelectionMode::Light);
            state.selected_ids.borrow_mut().clear();
            rebuild();
            state.refresh();
        }
    });

    root.upcast()
}

pub fn spawn() -> WidgetInstance {
    let (state, content) = build_content();
    let settings = build_settings(state.clone());
    WidgetInstance {
        content,
        settings: Some(settings),
        to_dict: Box::new(move || state.to_dict()),
        on_reset: None,
        on_change_ready: None,
    }
}

pub fn restore(data: &serde_json::Value) -> WidgetInstance {
    let (state, content) = build_content();
    // Applied *before* `build_settings` (which reads `state.mode`/
    // `selected_ids` to set the toggle buttons' initial state) and before
    // `build_content`'s own first `refresh()` fetch can possibly have
    // landed yet (the network round trip can't complete within this still-
    // synchronous function call) - so the very first real data to arrive
    // already resolves against the restored selection, not the default
    // one `build_content` started with.
    state.apply_dict(data);
    let settings = build_settings(state.clone());
    WidgetInstance {
        content,
        settings: Some(settings),
        to_dict: Box::new(move || state.to_dict()),
        on_reset: None,
        on_change_ready: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_through_rgb() {
        assert_eq!(hex_from_rgb(hex_to_rgb("#f2a541")), "#f2a541");
    }

    #[test]
    fn malformed_hex_falls_back_to_gray_not_a_panic() {
        assert_eq!(hex_to_rgb("nope"), (0.6, 0.6, 0.6));
    }

    #[test]
    fn default_selection_is_capped_and_alphabetical() {
        let make = |name: &str| Light {
            id: name.to_string(),
            name: name.to_string(),
            room_name: None,
            on: false,
            brightness_percent: 0.0,
            bulb_type: BulbType::Dimmable,
            display_color_hex: "#ffffff".to_string(),
            mirek: None,
        };
        let lights = vec![make("Zorro"), make("Alpha"), make("Mike"), make("Bravo"), make("Charlie")];
        let picked = pick_default_lights(lights);
        assert_eq!(picked.len(), MAX_ROWS);
        assert_eq!(picked.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), vec!["Alpha", "Bravo", "Charlie", "Mike"]);
    }
}
