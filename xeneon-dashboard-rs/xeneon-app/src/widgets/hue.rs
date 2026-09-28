// SPDX-License-Identifier: GPL-3.0-or-later
//! Philips Hue widget, two footprints (see `CardVariant`): SQ shows up to
//! four lights/rooms, each a rounded "chip" row - a soft-background icon
//! button (tap toggles on/off), the light's name and a status line, and a
//! full-width brightness bar (tap or drag sets the level, and turns the
//! light on if it was off); SX is the same row design, exactly one of
//! them, scaled up to fill its own wider/shorter card with no header -
//! matching the mockups agreed with the user before each was built (see
//! the design discussion in the memory system: a simple interrupteur row
//! didn't leave room for a real brightness control, so the richer "chip"
//! layout was chosen instead, at the cost of fitting only 4 rows per SQ
//! card instead of 5; SX came later, once SQ had already shipped and been
//! used hands-on for a while).
//!
//! Reads and writes go through `hue_bridge.rs`'s shared bridge connection
//! (`Config::hue_bridge_ip`/`hue_username`, paired once from Settings -
//! see that module's own doc comment for why this is shared app-wide
//! rather than per-widget) rather than anything this widget owns itself.
//! An unconfigured bridge, or a bridge that's unreachable/paired-but-
//! empty, shows a short message and a button that jumps straight to
//! Settings (`hue_bridge::open_settings`) instead of the card's rows.
//!
//! Step 3 (this pass) adds the settings panel: a "par pièce"/"par lumière"
//! mode toggle plus a checklist (capped at `CardVariant::max_rows`) of whichever the
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
//! Step 4 adds the color/color-temperature popover on tapping a light's
//! name - `open_color_popover`, branching on `BulbType`: a
//! saturation/value square plus a hue strip below it for `Color` (picked
//! color converted to the bridge's CIE xy via `hue_bridge::rgb_to_xy`),
//! a warm-to-cool `build_gradient_strip` for `ColorTemperature` (picked
//! position converted to mirek), and a no-op for `Dimmable` (no color data
//! to edit at all) - matching the design discussion's "nothing to open
//! for a plain white bulb" decision. This popover went through three
//! cuts on real hands-on feedback: a `gtk::ColorDialogButton` (color) and
//! a plain Kelvin `gtk::Scale` (temperature) first, then both replaced
//! with direct gradient strips (a color dialog needs an extra tap just to
//! see any colors at all, and a bare Kelvin number means nothing without
//! already knowing what e.g. 2700K looks like), then the color strip
//! itself grew into a full SV square (a single hue strip could only pick
//! a fully-saturated color, never a pastel or near-white). Every picker
//! here debounces its network PUT (`GRADIENT_STRIP_DEBOUNCE` after the
//! last drag tick) since they all fire on every tick for an instant local
//! preview, and the bridge rate-limits rapid requests. A room row's
//! status/icon still doesn't aggregate its member lights' actual colors
//! (see `room_to_entry`) - it always renders as a plain dimmable
//! accent-colored entry, so the popover never opens for a room row
//! either, a deliberate simplification since color aggregation across a
//! whole room is its own small design question,
//! separate from this step's picker mechanics.
//!
//! Bulb-type detection (`hue_bridge::BulbType`) drives the status line's
//! wording (`"72% · Couleur"` / `"100% · 2700K"` / `"45%"`) and, as of
//! this step, the name-tap popover's own content.
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
/// Cap on the settings panel's scrollable checklist height - tall enough
/// to show most households' room/light count without scrolling at all
/// (the popover has the vertical room for it), a scrollbar only kicking in
/// past that. Bumped from an earlier, much tighter 220px after real
/// hands-on feedback: that cap scrolled far sooner than the popover's own
/// available space actually needed it to.
const SETTINGS_LIST_MAX_HEIGHT_PX: i32 = 340;

const ICON_ON_PATH: &str = "assets/hue-bulb-icon.svg";
const ICON_OFF_PATH: &str = "assets/hue-bulb-off-icon.svg";
const ICON_SOURCE_FILL: &str = "#ffffff";
/// Same reasoning as `network_sq.rs`'s own `ICON_RASTER_PX`: well above
/// the on-screen size so the icon stays crisp rather than an upscaled
/// bitmap.
const ICON_RASTER_PX: i32 = 96;
const HEADER_ICON_PX: i32 = 18;
/// Fixed - matches `network_sq.rs`/`system_sq.rs`'s own badge accent,
/// used here for the header icon and the "N/N allumées" badge, neither of
/// which is user-customizable yet (no settings panel at all in step 2).
const ACCENT_COLOR_HEX: &str = "#f2a541";

/// What differs between this widget's two footprints (SQ: up to 4 lights,
/// header + badge; SX: exactly 1, no header, everything scaled up to fill
/// the wider/shorter card) - everything else (the row's icon/name/status/
/// bar widgets, the drag/click gestures, the color popover, the settings
/// panel) is identical code, just fed a different `CardVariant`. Mirrors
/// `audio.rs`'s own single-file, size-parameter approach to its L/M
/// variants, rather than a second near-duplicate module
/// (`network_sq.rs`/`network_sx.rs`'s own split): the difference here
/// really is chrome and a row count, not independent enough logic per
/// size to justify two files.
#[derive(Clone, Copy)]
struct CardVariant {
    max_rows: usize,
    show_header: bool,
    /// Whether each row gets its own rounded "chip" background
    /// (`.xeneon-hue-row`) - `true` for SQ, where several rows share one
    /// card and need visual separation; `false` for SX, where the single
    /// row already fills the whole (already-rounded) card, so a second
    /// rounded rect behind it would just look like an inset border.
    show_chip_background: bool,
    /// Extra horizontal margin on top of the card's own - `0` for SX
    /// (the card's own margin is the only inset needed), non-zero for SQ
    /// (breathing room between each chip and the card's rounded corners).
    row_inner_margin: i32,
    /// Whether `rows_box` centers its (typically just one, for SX)
    /// visible row vertically in whatever space `build_content` doesn't
    /// give the header - left `false` for SQ, where multiple rows should
    /// stack from the top like a list, not center as a block.
    center_rows_vertically: bool,
    /// `-1` means no fixed height - let the row size to its own content
    /// (SX, which has just one row and more vertical room than SQ's four
    /// packed rows need to share).
    row_height_px: i32,
    row_icon_circle_px: i32,
    row_icon_px: i32,
    bar_height_px: i32,
    name_column_width_px: i32,
    row_spacing: i32,
    name_css_class: &'static str,
    /// Pango markup `size` keyword for the status line - see
    /// `apply_entry_to_row`.
    status_size_keyword: &'static str,
    card_margin_start: i32,
    card_margin_end: i32,
    card_margin_top: i32,
    card_margin_bottom: i32,
}

const SQ_VARIANT: CardVariant = CardVariant {
    max_rows: 4,
    show_header: true,
    show_chip_background: true,
    row_inner_margin: 10,
    center_rows_vertically: false,
    row_height_px: 60,
    row_icon_circle_px: 40,
    row_icon_px: 20,
    bar_height_px: 36,
    name_column_width_px: 104,
    row_spacing: 10,
    name_css_class: "xeneon-hue-name",
    status_size_keyword: "small",
    card_margin_start: 14,
    card_margin_end: 14,
    card_margin_top: 12,
    card_margin_bottom: 10,
};

/// A single light/room, full card width, no header - the mockup agreed
/// with the user for this footprint (SX, built after the SQ card had
/// already shipped): the same "chip" row design, just one of them, scaled
/// up to fill the wider/shorter SX card on its own instead of sharing an
/// SQ card with up to three others.
const SX_VARIANT: CardVariant = CardVariant {
    max_rows: 1,
    show_header: false,
    show_chip_background: false,
    row_inner_margin: 0,
    center_rows_vertically: true,
    row_height_px: -1,
    row_icon_circle_px: 58,
    row_icon_px: 26,
    bar_height_px: 44,
    name_column_width_px: 118,
    row_spacing: 14,
    name_css_class: "xeneon-hue-name-lg",
    status_size_keyword: "medium",
    card_margin_start: 16,
    card_margin_end: 16,
    card_margin_top: 0,
    card_margin_bottom: 0,
};

/// Size of the color/temperature popover's gradient "nuancier" strip (see
/// `build_gradient_strip`) - roomy enough for a precise tap/drag on a
/// touchscreen, same pill height as the on-card brightness bar for a
/// consistent look between the two.
const GRADIENT_STRIP_WIDTH_PX: i32 = 220;
const GRADIENT_STRIP_HEIGHT_PX: i32 = 36;
/// The color popover's saturation/value square - same width as the strip
/// below it (`GRADIENT_STRIP_WIDTH_PX`) so the two line up.
const SV_SQUARE_WIDTH_PX: i32 = 220;
const SV_SQUARE_HEIGHT_PX: i32 = 170;
/// How long a gradient strip (or the SV square) waits after the last drag
/// tick before actually sending its PUT - see `open_color_popover`'s own
/// doc comment on why this needs debouncing at all.
const GRADIENT_STRIP_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);

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
            ".xeneon-hue-title {{ font-size: 17px; font-weight: 500; color: #ffffff; }}\n\
             .xeneon-hue-badge {{ background-color: rgba(242, 165, 65, 0.15); \
             border-radius: 13px; padding: 5px 12px; }}\n\
             .xeneon-hue-badge-label {{ font-size: 14px; font-weight: 500; color: {ACCENT_COLOR_HEX}; }}\n\
             .xeneon-hue-row {{ background-color: rgba(255, 255, 255, 0.05); border-radius: 14px; }}\n\
             .xeneon-hue-name {{ font-size: 14px; font-weight: 500; color: #ffffff; }}\n\
             .xeneon-hue-name-lg {{ font-size: 16px; font-weight: 500; color: #ffffff; }}\n\
             .xeneon-hue-empty-message {{ color: rgba(255, 255, 255, 0.6); font-size: 14px; }}"
        ));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

/// Paints one row's icon-circle background: a soft, low-opacity fill in
/// `rgb` when the light behind this row is on, a plain neutral gray when
/// it's off or the row is unused (fewer than `CardVariant::max_rows` lights available) -
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
/// first `max_rows` lights, alphabetically, the same for every such card.
/// Used both as the very first cut's only behavior (step 2) and, now, as
/// light-mode's fallback when nothing has been explicitly picked yet (a
/// freshly spawned card, or one saved before step 3 existed) - see the
/// module doc comment. Sorting by name (rather than bridge order, closer
/// to "creation order" and not meaningful to a user) at least makes the
/// arbitrary choice deterministic and easy to reason about while testing.
fn pick_default_lights(mut lights: Vec<Light>, max_rows: usize) -> Vec<Light> {
    lights.sort_by(|a, b| a.name.cmp(&b.name));
    lights.truncate(max_rows);
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
/// the bridge's latest data into the up-to-`max_rows` entries to actually
/// show - the one place `HueState::refresh` needs to call to go from "raw
/// bridge data" to "what this specific card displays". An id in
/// `selected_ids` naming a light/room that no longer exists (deleted or
/// renamed on the bridge since this card was configured) is simply
/// skipped, same "stale setting doesn't error, just quietly does less"
/// tolerance `system_sq.rs`'s disk-path setting already has.
fn resolve_selection(mode: SelectionMode, selected_ids: &[String], all_lights: &[Light], all_rooms: &[Room], max_rows: usize) -> Vec<CardEntry> {
    match mode {
        SelectionMode::Light if selected_ids.is_empty() => {
            pick_default_lights(all_lights.to_vec(), max_rows).iter().map(light_to_entry).collect()
        }
        SelectionMode::Light => selected_ids
            .iter()
            .filter_map(|id| all_lights.iter().find(|light| &light.id == id))
            .take(max_rows)
            .map(light_to_entry)
            .collect(),
        SelectionMode::Room if selected_ids.is_empty() => {
            let mut rooms: Vec<&Room> = all_rooms.iter().collect();
            rooms.sort_by(|a, b| a.name.cmp(&b.name));
            rooms.truncate(max_rows);
            rooms.iter().map(|room| room_to_entry(room)).collect()
        }
        SelectionMode::Room => selected_ids
            .iter()
            .filter_map(|id| all_rooms.iter().find(|room| &room.id == id))
            .take(max_rows)
            .map(room_to_entry)
            .collect(),
    }
}

/// One row's live widgets plus which light/room it currently shows -
/// `None` while unused (fewer than `variant.max_rows` entries selected) or
/// before the first successful fetch, in which case the row stays hidden.
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
    /// Read by the name-tap handler (see `build_content`) to decide what
    /// kind of popover to open, if any - `BulbType::Dimmable`'s initial
    /// value here is an arbitrary placeholder (no gesture fires before
    /// `apply_entry_to_row` has run at least once, since the row stays
    /// hidden until then), not a meaningful default.
    bulb_type: Cell<BulbType>,
    /// Only meaningful (`Some`) for a `ColorTemperature` entry - the
    /// temperature popover's slider needs the light's *current* mirek to
    /// show the right starting position, same reason `Light`/`CardEntry`
    /// themselves carry it.
    mirek: Cell<Option<f64>>,
    /// This row's own copy of `CardVariant::status_size_keyword` - fixed
    /// for the row's whole lifetime (a row never changes card variant), so
    /// `apply_entry_to_row` doesn't need `variant` threaded all the way
    /// down to it just for this one field.
    status_size_keyword: &'static str,
}

fn build_row(variant: &CardVariant) -> LightRow {
    let container = gtk::Box::new(gtk::Orientation::Horizontal, variant.row_spacing);
    if variant.show_chip_background {
        container.add_css_class("xeneon-hue-row");
    }
    container.set_size_request(-1, variant.row_height_px);
    container.set_margin_start(variant.row_inner_margin);
    container.set_margin_end(variant.row_inner_margin);

    let icon_overlay = gtk::Overlay::new();
    icon_overlay.set_valign(gtk::Align::Center);
    let icon_area = gtk::DrawingArea::new();
    icon_area.set_size_request(variant.row_icon_circle_px, variant.row_icon_circle_px);
    icon_overlay.set_child(Some(&icon_area));
    let icon_image = gtk::Image::new();
    icon_image.set_pixel_size(variant.row_icon_px);
    icon_image.set_halign(gtk::Align::Center);
    icon_image.set_valign(gtk::Align::Center);
    icon_image.set_can_target(false);
    icon_overlay.add_overlay(&icon_image);
    container.append(&icon_overlay);

    let text_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text_box.set_valign(gtk::Align::Center);
    text_box.set_hexpand(false);
    text_box.set_size_request(variant.name_column_width_px, -1);
    let name_label = gtk::Label::new(None);
    name_label.add_css_class(variant.name_css_class);
    name_label.set_halign(gtk::Align::Start);
    name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text_box.append(&name_label);
    let status_label = gtk::Label::new(None);
    status_label.set_halign(gtk::Align::Start);
    text_box.append(&status_label);
    container.append(&text_box);

    let bar_area = gtk::DrawingArea::new();
    bar_area.set_hexpand(true);
    bar_area.set_size_request(-1, variant.bar_height_px);
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
        bulb_type: Cell::new(BulbType::Dimmable),
        mirek: Cell::new(None),
        status_size_keyword: variant.status_size_keyword,
    }
}

/// All of this widget's live state - one instance per placed card. Owns
/// the bridge connection's *view* only (the connection itself lives in
/// `Config`/`hue_bridge.rs`, shared) - this struct remembers this card's
/// own selection settings, the last full fetch (so the settings panel has
/// something to build its checklist from without a fetch of its own - see
/// `on_data_changed`), and the up-to-`CardVariant::max_rows` entries currently shown.
struct HueState {
    variant: CardVariant,
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
                    let entries = resolve_selection(state.mode.get(), &state.selected_ids.borrow(), &lights, &rooms, state.variant.max_rows);
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
    row.bulb_type.set(entry.bulb_type);
    row.mirek.set(entry.mirek);

    row.name_label.set_label(&entry.name);
    // Pango's `<span color="...">` only accepts `#rrggbb`/`#rrggbbaa` or a
    // named color, never a CSS `rgba(...)` function - `#ffffff66` (~40%
    // alpha) is the off-state equivalent of the on-state's plain hex.
    let status_hex = if entry.on { entry.display_color_hex.clone() } else { "#ffffff66".to_string() };
    row.status_label.set_markup(&format!(
        "<span size=\"{}\" color=\"{}\">{}</span>",
        row.status_size_keyword,
        gtk::glib::markup_escape_text(&status_hex),
        gtk::glib::markup_escape_text(&status_text(entry))
    ));

    let texture = if entry.on { load_on_icon(&entry.display_color_hex) } else { load_off_icon() };
    row.icon_image.set_paintable(texture.as_ref());
    row.icon_area.queue_draw();
    row.bar_area.queue_draw();
}

fn build_content(variant: CardVariant) -> (Rc<HueState>, gtk::Widget) {
    ensure_css_installed();

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let css_class = format!("xeneon-hue-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.add_css_class(&css_class);
    root.set_margin_start(variant.card_margin_start);
    root.set_margin_end(variant.card_margin_end);
    root.set_margin_top(variant.card_margin_top);
    root.set_margin_bottom(variant.card_margin_bottom);

    // No header/badge on a variant with no room for one (SX) - a lone
    // light's own name already says what this card is, and a "1/1
    // allumées" badge would be a strange thing to read for a single
    // light.
    let badge_label = gtk::Label::new(None);
    if variant.show_header {
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let header_icon = gtk::Image::new();
        header_icon.set_pixel_size(HEADER_ICON_PX);
        header_icon.set_paintable(load_on_icon(ACCENT_COLOR_HEX).as_ref());
        // Explicit on every header child (icon, title, badge) rather than
        // relying on the row's own default `Fill` alignment - an audit
        // across this card's, `network_sq.rs`'s and `system_sq.rs`'s
        // headers found them not reliably centering the same way without
        // it, throwing the three cards' header rows out of vertical
        // alignment with each other.
        header_icon.set_valign(gtk::Align::Center);
        header.append(&header_icon);
        let title_label = gtk::Label::new(Some(&i18n::t("widgets.hue.title")));
        title_label.add_css_class("xeneon-hue-title");
        title_label.set_valign(gtk::Align::Center);
        header.append(&title_label);
        let header_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header_spacer.set_hexpand(true);
        header.append(&header_spacer);
        let badge = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        badge.add_css_class("xeneon-hue-badge");
        badge.set_valign(gtk::Align::Center);
        badge_label.add_css_class("xeneon-hue-badge-label");
        badge.append(&badge_label);
        header.append(&badge);
        root.append(&header);
    }

    let rows_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    if variant.show_header {
        rows_box.set_margin_top(4);
    }
    if variant.center_rows_vertically {
        rows_box.set_valign(gtk::Align::Center);
        rows_box.set_vexpand(true);
    }
    let mut rows = Vec::with_capacity(variant.max_rows);
    for _ in 0..variant.max_rows {
        let row = Rc::new(build_row(&variant));
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
        variant,
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
                    if let Ok(Err(err)) = gtk::gio::spawn_blocking(move || put_target(&ip, &username, &target, new_on, brightness)).await {
                        warn!("failed to set Hue on/brightness: {err}");
                    }
                    state.refresh();
                });
            }
        });
        row.icon_area.add_controller(toggle_click);

        // Name tap: opens the color/temperature popover, for whichever
        // `BulbType` this row currently shows - a no-op for `Dimmable`
        // (see `open_color_popover`'s own early return), matching the
        // module doc comment's "not opening an empty popover" decision.
        let name_click = gtk::GestureClick::new();
        name_click.connect_released({
            let row = row.clone();
            let state = state.clone();
            move |gesture, _n_press, _x, _y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                open_color_popover(&row, &state);
            }
        });
        row.name_label.add_controller(name_click);

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
                    if let Ok(Err(err)) = gtk::gio::spawn_blocking(move || put_target(&ip, &username, &target, new_on, brightness)).await {
                        warn!("failed to set Hue on/brightness: {err}");
                    }
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

/// The `(resource_kind, id)` pair `hue_bridge::set_color_xy`/
/// `set_color_temperature_mirek` want, for whichever kind of `target` this
/// row is currently bound to.
fn target_kind_id(target: &CardTarget) -> (&'static str, String) {
    match target {
        CardTarget::Light(id) => ("light", id.clone()),
        CardTarget::Room(id) => ("grouped_light", id.clone()),
    }
}

/// Updates a row's own display (icon/bar color) the moment the user picks
/// a new color/temperature in its popover - before the PUT has even been
/// sent, let alone reconciled by the next `refresh()`, so the on-card
/// preview tracks the popover instantly rather than lagging a full
/// round-trip behind it.
fn preview_color(row: &Rc<LightRow>, hex: &str) {
    row.rgb.set(hex_to_rgb(hex));
    let texture = load_on_icon(hex);
    row.icon_image.set_paintable(texture.as_ref());
    row.icon_area.queue_draw();
    row.bar_area.queue_draw();
}

/// Converts an HSV color (`h` degrees, `s`/`v` 0.0-1.0) to sRGB 0.0-1.0
/// components - the SV-square picker's own color math (see
/// `open_color_popover`'s `BulbType::Color` branch): the square's x/y axes
/// are saturation/value at a fixed hue, and this is what turns a marker
/// position back into an actual color to preview and send.
fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    let c = v * s;
    let h_prime = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (h_prime.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match h_prime as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    (r1 + m, g1 + m, b1 + m)
}

/// The inverse of `hsv_to_rgb` - used once, to position the color
/// popover's square marker (and the hue strip's own marker, via its `h`
/// component) on whatever color the light's current `display_color_hex`
/// is already closest to when the popover opens, rather than always
/// starting at a fixed default. An exact roundtrip isn't the goal (this
/// widget's colors are already an approximation - see
/// `hue_bridge::xy_to_hex`'s own doc comment), just a reasonable starting
/// position.
fn rgb_to_hsv((r, g, b): (f64, f64, f64)) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let hue = if delta <= 0.0001 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let saturation = if max <= 0.0 { 0.0 } else { delta / max };
    (hue.rem_euclid(360.0), saturation, max)
}

/// Builds a horizontal gradient "nuancier" strip - a `DrawingArea` picker
/// shared by both branches of `open_color_popover`: paints a smooth Cairo
/// gradient through `stops` (each an `(offset, rgb)` pair, `offset` in
/// 0.0-1.0) plus a small marker at the currently selected fraction, and
/// reports every drag position - a tap included, same "a tap is a
/// ~0-movement drag" technique the on-card brightness bar already uses -
/// through `on_pick`. Purely mechanical (geometry, drawing, the drag
/// gesture): `on_pick` is where a fraction actually turns into a color or
/// a mirek value and, eventually, a PUT to the bridge.
fn build_gradient_strip(width: i32, height: i32, stops: Vec<(f64, (f64, f64, f64))>, initial_fraction: f64, on_pick: impl Fn(f64) + 'static) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_size_request(width, height);
    let fraction = Rc::new(Cell::new(initial_fraction.clamp(0.0, 1.0)));
    let stops = Rc::new(stops);

    area.set_draw_func({
        let fraction = fraction.clone();
        let stops = stops.clone();
        move |_area, cr, width, height| {
            let (width, height) = (width as f64, height as f64);
            let radius = height / 2.0;
            let left = radius;
            let right = (width - radius).max(left);

            let gradient = gtk::cairo::LinearGradient::new(left, 0.0, right, 0.0);
            for (offset, (r, g, b)) in stops.iter() {
                gradient.add_color_stop_rgb(*offset, *r, *g, *b);
            }
            let _ = cr.set_source(&gradient);
            cr.set_line_cap(gtk::cairo::LineCap::Round);
            cr.set_line_width(height);
            cr.move_to(left, height / 2.0);
            cr.line_to(right, height / 2.0);
            let _ = cr.stroke();

            let marker_x = left + (right - left) * fraction.get();
            let marker_radius = radius * 0.55;
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
            cr.arc(marker_x, height / 2.0, marker_radius, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
            cr.set_line_width(1.5);
            cr.arc(marker_x, height / 2.0, marker_radius, 0.0, std::f64::consts::TAU);
            let _ = cr.stroke();
        }
    });

    let on_pick: Rc<dyn Fn(f64)> = Rc::new(on_pick);
    let drag_start_x: Rc<Cell<f64>> = Rc::new(Cell::new(0.0));
    let handle_move: Rc<dyn Fn(f64)> = Rc::new({
        let area = area.clone();
        let fraction = fraction.clone();
        let drag_start_x = drag_start_x.clone();
        let on_pick = on_pick.clone();
        move |offset_x: f64| {
            let width = area.width() as f64;
            if width <= 0.0 {
                return;
            }
            let x = (drag_start_x.get() + offset_x).clamp(0.0, width);
            let new_fraction = x / width;
            fraction.set(new_fraction);
            area.queue_draw();
            on_pick(new_fraction);
        }
    });

    let drag = gtk::GestureDrag::new();
    drag.connect_drag_begin({
        let drag_start_x = drag_start_x.clone();
        move |gesture, x, _y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            drag_start_x.set(x);
        }
    });
    drag.connect_drag_update({
        let handle_move = handle_move.clone();
        move |_gesture, offset_x, _offset_y| handle_move(offset_x)
    });
    drag.connect_drag_end({
        let handle_move = handle_move.clone();
        move |_gesture, offset_x, _offset_y| handle_move(offset_x)
    });
    area.add_controller(drag);

    area
}

/// Opens the color or color-temperature popover for `row`'s current
/// light/room - a no-op for `BulbType::Dimmable` (no color data to edit at
/// all) and while no light/room is bound yet (an unused row). See the
/// module doc comment for what's deliberately *not* here: room entries
/// always report `Dimmable` (see `room_to_entry`), so this never opens for
/// a room row either, until room-level color aggregation is designed.
fn open_color_popover(row: &Rc<LightRow>, state: &Rc<HueState>) {
    let Some(target) = row.target.borrow().clone() else { return };
    let bulb_type = row.bulb_type.get();
    if bulb_type == BulbType::Dimmable {
        return;
    }

    let popover = gtk::Popover::new();
    popover.set_parent(&row.name_label);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);

    match bulb_type {
        BulbType::Color => {
            let label = gtk::Label::new(Some(&i18n::t("widgets.hue.color_popover.color")));
            label.set_halign(gtk::Align::Start);
            content.append(&label);

            // A saturation/value square (fixed hue - white to the left,
            // black at the bottom, the pure hue at the top right) plus a
            // hue strip below it to change which hue the square itself is
            // built from - the standard "SV square + hue bar" picker
            // layout, direct and no dialog to open first, per the user's
            // own feedback on the first two cuts of this popover (a
            // `ColorDialogButton` needed an extra tap to see any colors at
            // all; a hue-only strip couldn't reach pastels or near-white
            // at all). `hue`/`saturation_value` are shared, mutable state
            // between the square and the strip below it - each one both
            // reads and writes into them, since picking a new hue on the
            // strip has to repaint the square (same saturation/value, new
            // base color) and dragging the square has to keep whatever hue
            // the strip last picked.
            let (h0, s0, v0) = rgb_to_hsv(row.rgb.get());
            let hue = Rc::new(Cell::new(h0));
            let saturation_value = Rc::new(Cell::new((s0, v0)));

            // Shared by the square and the strip - both just update
            // `hue`/`saturation_value` and call this, rather than each
            // separately re-deriving the resulting color, debouncing, and
            // sending its own PUT.
            let commit: Rc<dyn Fn()> = {
                let row = row.clone();
                let state = state.clone();
                let target = target.clone();
                let hue = hue.clone();
                let saturation_value = saturation_value.clone();
                let generation: Rc<Cell<u64>> = Rc::new(Cell::new(0));
                Rc::new(move || {
                    let (s, v) = saturation_value.get();
                    let (r, g, b) = hsv_to_rgb(hue.get(), s, v);
                    preview_color(&row, &hex_from_rgb((r, g, b)));

                    let this_generation = generation.get() + 1;
                    generation.set(this_generation);
                    let config = config_store::get();
                    let (Some(ip), Some(username)) = (config.hue_bridge_ip, config.hue_username) else { return };
                    let (resource_kind, id) = target_kind_id(&target);
                    let (x, y) = hue_bridge::rgb_to_xy(r, g, b);
                    let state = state.clone();
                    let generation = generation.clone();
                    gtk::glib::spawn_future_local(async move {
                        // Same debounce as the temperature strip below -
                        // see its own doc comment for why.
                        gtk::glib::timeout_future(GRADIENT_STRIP_DEBOUNCE).await;
                        if generation.get() != this_generation {
                            return;
                        }
                        if let Ok(Err(err)) = gtk::gio::spawn_blocking(move || hue_bridge::set_color_xy(&ip, &username, resource_kind, &id, x, y)).await {
                            warn!("failed to set Hue color: {err}");
                        }
                        state.refresh();
                    });
                })
            };

            let square = gtk::DrawingArea::new();
            square.set_size_request(SV_SQUARE_WIDTH_PX, SV_SQUARE_HEIGHT_PX);
            square.set_draw_func({
                let hue = hue.clone();
                let saturation_value = saturation_value.clone();
                move |_area, cr, width, height| {
                    let (width, height) = (width as f64, height as f64);

                    let (base_r, base_g, base_b) = hsv_to_rgb(hue.get(), 1.0, 1.0);
                    cr.set_source_rgb(base_r, base_g, base_b);
                    cr.rectangle(0.0, 0.0, width, height);
                    let _ = cr.fill();

                    // Saturation axis (x): opaque white fading to
                    // transparent, left to right.
                    let white = gtk::cairo::LinearGradient::new(0.0, 0.0, width, 0.0);
                    white.add_color_stop_rgba(0.0, 1.0, 1.0, 1.0, 1.0);
                    white.add_color_stop_rgba(1.0, 1.0, 1.0, 1.0, 0.0);
                    let _ = cr.set_source(&white);
                    cr.rectangle(0.0, 0.0, width, height);
                    let _ = cr.fill();

                    // Value axis (y): transparent fading to opaque black,
                    // top to bottom.
                    let black = gtk::cairo::LinearGradient::new(0.0, 0.0, 0.0, height);
                    black.add_color_stop_rgba(0.0, 0.0, 0.0, 0.0, 0.0);
                    black.add_color_stop_rgba(1.0, 0.0, 0.0, 0.0, 1.0);
                    let _ = cr.set_source(&black);
                    cr.rectangle(0.0, 0.0, width, height);
                    let _ = cr.fill();

                    let (s, v) = saturation_value.get();
                    let marker_x = s * width;
                    let marker_y = (1.0 - v) * height;
                    let marker_radius = 7.0;
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
                    cr.arc(marker_x, marker_y, marker_radius, 0.0, std::f64::consts::TAU);
                    let _ = cr.fill();
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
                    cr.set_line_width(1.5);
                    cr.arc(marker_x, marker_y, marker_radius, 0.0, std::f64::consts::TAU);
                    let _ = cr.stroke();
                }
            });

            let square_drag_start: Rc<Cell<(f64, f64)>> = Rc::new(Cell::new((0.0, 0.0)));
            let handle_square_move: Rc<dyn Fn(f64, f64)> = Rc::new({
                let square = square.clone();
                let saturation_value = saturation_value.clone();
                let square_drag_start = square_drag_start.clone();
                let commit = commit.clone();
                move |offset_x: f64, offset_y: f64| {
                    let width = square.width() as f64;
                    let height = square.height() as f64;
                    if width <= 0.0 || height <= 0.0 {
                        return;
                    }
                    let (start_x, start_y) = square_drag_start.get();
                    let x = (start_x + offset_x).clamp(0.0, width);
                    let y = (start_y + offset_y).clamp(0.0, height);
                    saturation_value.set((x / width, 1.0 - y / height));
                    square.queue_draw();
                    commit();
                }
            });
            let square_drag = gtk::GestureDrag::new();
            square_drag.connect_drag_begin({
                let square_drag_start = square_drag_start.clone();
                move |gesture, x, y| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    square_drag_start.set((x, y));
                }
            });
            square_drag.connect_drag_update({
                let handle_square_move = handle_square_move.clone();
                move |_gesture, offset_x, offset_y| handle_square_move(offset_x, offset_y)
            });
            square_drag.connect_drag_end({
                let handle_square_move = handle_square_move.clone();
                move |_gesture, offset_x, offset_y| handle_square_move(offset_x, offset_y)
            });
            square.add_controller(square_drag);
            content.append(&square);

            // A 6-stop rainbow (red/yellow/green/cyan/blue/magenta/red) at
            // full saturation and value - picking a point on it only ever
            // changes `hue`, so it repaints the square (new base color,
            // same marker position) rather than moving the square's own
            // marker.
            let stops: Vec<(f64, (f64, f64, f64))> = (0..=6).map(|i| (i as f64 / 6.0, hsv_to_rgb(i as f64 * 60.0, 1.0, 1.0))).collect();
            let strip = build_gradient_strip(GRADIENT_STRIP_WIDTH_PX, GRADIENT_STRIP_HEIGHT_PX, stops, h0 / 360.0, {
                let hue = hue.clone();
                let square = square.clone();
                let commit = commit.clone();
                move |fraction| {
                    hue.set(fraction * 360.0);
                    square.queue_draw();
                    commit();
                }
            });
            content.append(&strip);
        }
        BulbType::ColorTemperature => {
            let label = gtk::Label::new(Some(&i18n::t("widgets.hue.color_popover.temperature")));
            label.set_halign(gtk::Align::Start);
            content.append(&label);

            // Warm (left) to cool (right) - a "nuancier" the user can pick
            // by eye rather than a bare Kelvin number (their own feedback
            // on the first cut, a plain `gtk::Scale`: "je ne connais pas
            // les valeurs"). Sampled straight from `mirek_to_hex` at each
            // stop, so this strip always matches exactly what the on-card
            // swatch would show for that same mirek value - no separate
            // color math to keep in sync.
            const MIN_MIREK: f64 = 153.0;
            const MAX_MIREK: f64 = 500.0;
            let mirek_at_fraction = |fraction: f64| MAX_MIREK + (MIN_MIREK - MAX_MIREK) * fraction;
            let stops: Vec<(f64, (f64, f64, f64))> =
                (0..=8).map(|i| { let f = i as f64 / 8.0; (f, hex_to_rgb(&hue_bridge::mirek_to_hex(mirek_at_fraction(f)))) }).collect();

            let current_mirek = row.mirek.get().unwrap_or(300.0);
            let initial_fraction = ((current_mirek - MAX_MIREK) / (MIN_MIREK - MAX_MIREK)).clamp(0.0, 1.0);

            let generation: Rc<Cell<u64>> = Rc::new(Cell::new(0));
            let strip = build_gradient_strip(GRADIENT_STRIP_WIDTH_PX, GRADIENT_STRIP_HEIGHT_PX, stops, initial_fraction, {
                let row = row.clone();
                let state = state.clone();
                let target = target.clone();
                let generation = generation.clone();
                move |fraction| {
                    let mirek = mirek_at_fraction(fraction).clamp(MIN_MIREK, MAX_MIREK);
                    row.mirek.set(Some(mirek));
                    preview_color(&row, &hue_bridge::mirek_to_hex(mirek));

                    let this_generation = generation.get() + 1;
                    generation.set(this_generation);
                    let config = config_store::get();
                    let (Some(ip), Some(username)) = (config.hue_bridge_ip, config.hue_username) else { return };
                    let (resource_kind, id) = target_kind_id(&target);
                    let state = state.clone();
                    let generation = generation.clone();
                    gtk::glib::spawn_future_local(async move {
                        // The PUT only fires once dragging pauses for
                        // `GRADIENT_STRIP_DEBOUNCE` - the strip fires on
                        // every drag tick (for an instant local preview),
                        // and the bridge rate-limits rapid requests (see
                        // `hue_bridge.rs`'s own doc comment on the
                        // reference extension's 429 handling). Same
                        // generation-counter debounce idiom as
                        // `settings_page.rs`'s `ha_ping_generation`/
                        // `hue_pair_generation`, applied to a delay
                        // instead of an in-flight request.
                        gtk::glib::timeout_future(GRADIENT_STRIP_DEBOUNCE).await;
                        if generation.get() != this_generation {
                            return; // superseded by a later tick - that one will send its own PUT
                        }
                        if let Ok(Err(err)) = gtk::gio::spawn_blocking(move || hue_bridge::set_color_temperature_mirek(&ip, &username, resource_kind, &id, mirek)).await {
                            warn!("failed to set Hue color temperature: {err}");
                        }
                        state.refresh();
                    });
                }
            });
            content.append(&strip);
        }
        BulbType::Dimmable => unreachable!("returned above"),
    }

    popover.set_child(Some(&content));
    popover.popup();
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
/// rooms or lights, up to `CardVariant::max_rows` checked at once (the rest disabled
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
    scroller.set_max_content_height(SETTINGS_LIST_MAX_HEIGHT_PX);
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

            // Room mode is one flat, unheaded group of (id, label) pairs -
            // a room's label is just its name (plus how many lights it
            // has). Light mode groups lights by their own room, each
            // group headed by that room's name, so scanning a long list
            // doesn't mean reading every light's own room suffix one by
            // one - lights with no resolved room fall into one final
            // "Sans pièce"/"No room" group instead of being dropped or
            // scattered. Groups (and lights within a group) are sorted by
            // name; the unassigned group always sorts last, regardless of
            // its own name, since it isn't a real room a user chose.
            let groups: Vec<(Option<String>, Vec<(String, String)>)> = match state.mode.get() {
                SelectionMode::Room => {
                    let mut rooms = state.all_rooms.borrow().clone();
                    rooms.sort_by(|a, b| a.name.cmp(&b.name));
                    // "(N)" rather than a translated "N lights" - a bare
                    // count sidesteps French/English singular-plural
                    // agreement for a detail this minor, while `light_ids`
                    // (otherwise unread) still earns its place here.
                    let items = rooms.into_iter().map(|room| (room.id, format!("{} ({})", room.name, room.light_ids.len()))).collect();
                    vec![(None, items)]
                }
                SelectionMode::Light => {
                    let mut by_room: std::collections::HashMap<Option<String>, Vec<(String, String)>> = std::collections::HashMap::new();
                    for light in state.all_lights.borrow().iter() {
                        by_room.entry(light.room_name.clone()).or_default().push((light.id.clone(), light.name.clone()));
                    }
                    let unassigned = by_room.remove(&None);

                    let mut room_names: Vec<String> = by_room.keys().filter_map(|name| name.clone()).collect();
                    room_names.sort();

                    let mut groups: Vec<(Option<String>, Vec<(String, String)>)> = room_names
                        .into_iter()
                        .map(|name| {
                            let mut lights = by_room.remove(&Some(name.clone())).unwrap_or_default();
                            lights.sort_by(|a, b| a.1.cmp(&b.1));
                            (Some(name), lights)
                        })
                        .collect();
                    if let Some(mut lights) = unassigned {
                        lights.sort_by(|a, b| a.1.cmp(&b.1));
                        groups.push((Some(i18n::t("widgets.hue.settings.unassigned")), lights));
                    }
                    groups
                }
            };

            if groups.iter().all(|(_, items)| items.is_empty()) {
                list_box.append(&loading_label);
                return;
            }

            let selected = state.selected_ids.borrow().clone();
            let check_buttons: Rc<RefCell<Vec<gtk::CheckButton>>> = Rc::new(RefCell::new(Vec::new()));
            for (group_index, (header, items)) in groups.into_iter().enumerate() {
                if let Some(header_text) = header {
                    if group_index > 0 {
                        list_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
                    }
                    let header_label = gtk::Label::new(Some(&header_text));
                    header_label.add_css_class("dim-label");
                    header_label.add_css_class("caption");
                    header_label.set_halign(gtk::Align::Start);
                    list_box.append(&header_label);
                }

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
                            let at_cap = selected.len() >= state.variant.max_rows;
                            drop(selected);

                            // Disables every unchecked box once the cap is
                            // reached (rather than rejecting a click past
                            // it), and re-enables them all the moment a
                            // selection drops back under the cap - simpler
                            // than an error message, and makes the limit
                            // discoverable just by trying to check a 5th
                            // box.
                            for other in check_buttons.borrow().iter() {
                                if !other.is_active() {
                                    other.set_sensitive(!at_cap);
                                }
                            }

                            state.refresh();
                        }
                    });
                }
            }

            // The cap may already be reached from a restored selection -
            // apply the same disabling pass once up front, not just from
            // inside a toggle handler.
            let at_cap = state.selected_ids.borrow().len() >= state.variant.max_rows;
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

/// Shared by every `spawn_*`/`restore_*` pair below - only `variant` and,
/// for restore, the saved `data` differ between the SQ and SX registry
/// entries (see the module doc comment on why both sizes live in this one
/// file/`CardVariant` rather than a second near-duplicate module).
fn spawn_variant(variant: CardVariant) -> WidgetInstance {
    let (state, content) = build_content(variant);
    let settings = build_settings(state.clone());
    WidgetInstance {
        content,
        settings: Some(settings),
        to_dict: Box::new(move || state.to_dict()),
        on_reset: None,
        on_change_ready: None,
    }
}

fn restore_variant(variant: CardVariant, data: &serde_json::Value) -> WidgetInstance {
    let (state, content) = build_content(variant);
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

pub fn spawn_sq() -> WidgetInstance {
    spawn_variant(SQ_VARIANT)
}

pub fn restore_sq(data: &serde_json::Value) -> WidgetInstance {
    restore_variant(SQ_VARIANT, data)
}

pub fn spawn_sx() -> WidgetInstance {
    spawn_variant(SX_VARIANT)
}

pub fn restore_sx(data: &serde_json::Value) -> WidgetInstance {
    restore_variant(SX_VARIANT, data)
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
        let picked = pick_default_lights(lights, SQ_VARIANT.max_rows);
        assert_eq!(picked.len(), SQ_VARIANT.max_rows);
        assert_eq!(picked.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), vec!["Alpha", "Bravo", "Charlie", "Mike"]);
    }

    #[test]
    fn sx_variant_default_selection_is_a_single_light() {
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
        let lights = vec![make("Zorro"), make("Alpha")];
        let picked = pick_default_lights(lights, SX_VARIANT.max_rows);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].name, "Alpha");
    }

    #[test]
    fn resolve_selection_respects_the_variant_cap_even_with_more_ids_selected() {
        let make = |id: &str| Light {
            id: id.to_string(),
            name: id.to_string(),
            room_name: None,
            on: false,
            brightness_percent: 0.0,
            bulb_type: BulbType::Dimmable,
            display_color_hex: "#ffffff".to_string(),
            mirek: None,
        };
        let lights = vec![make("a"), make("b"), make("c")];
        // A saved selection naming more ids than this card's variant
        // allows (e.g. edited by hand, or left over from switching a card
        // from SQ to SX) shouldn't show more rows than the variant has.
        let selected = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let entries = resolve_selection(SelectionMode::Light, &selected, &lights, &[], SX_VARIANT.max_rows);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn hsv_primaries_match_expected_rgb() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), (1.0, 0.0, 0.0));
        assert_eq!(hsv_to_rgb(120.0, 1.0, 1.0), (0.0, 1.0, 0.0));
        assert_eq!(hsv_to_rgb(240.0, 1.0, 1.0), (0.0, 0.0, 1.0));
    }

    #[test]
    fn rgb_to_hsv_round_trips_through_hsv_to_rgb() {
        for (h, s, v) in [(0.0, 1.0, 1.0), (60.0, 0.6, 0.8), (180.0, 1.0, 0.5), (300.0, 0.3, 1.0)] {
            let rgb = hsv_to_rgb(h, s, v);
            let (h2, s2, v2) = rgb_to_hsv(rgb);
            assert!((h2 - h).abs() < 0.01, "hue: expected ~{h}, got {h2}");
            assert!((s2 - s).abs() < 0.01, "saturation: expected ~{s}, got {s2}");
            assert!((v2 - v).abs() < 0.01, "value: expected ~{v}, got {v2}");
        }
    }

    #[test]
    fn rgb_to_hsv_reports_zero_saturation_for_a_neutral_gray() {
        let (_, s, v) = rgb_to_hsv((0.5, 0.5, 0.5));
        assert_eq!(s, 0.0);
        assert_eq!(v, 0.5);
    }
}
