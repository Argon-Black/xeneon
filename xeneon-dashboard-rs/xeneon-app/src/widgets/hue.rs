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
//! **Deliberately not built yet** (this is step 2 of a 4-step plan agreed
//! with the user): which lights/rooms appear on a given card is not yet
//! configurable - `pick_default_lights` below just takes the first four
//! lights, sorted by name, every card the same. Step 3 adds a settings
//! panel to choose up to four specific lights or rooms per instance. Step
//! 4 adds a color/color-temperature popover on tapping a light's name
//! (currently a no-op) - deliberately not wired to anything here, since
//! what it should open depends on `BulbType`, not yet decided in detail.
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
use crate::hue_bridge::{self, BulbType, Light};
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

/// Status line text for one light - wording depends on `BulbType` (see the
/// module doc comment): a color light names its capability generically
/// ("Couleur") rather than trying to name the actual hue, a
/// color-temperature light shows its Kelvin value, a dimmable light shows
/// only the percentage. Any type shows the plain "off" text while `on` is
/// `false`, since a bridge keeps reporting the brightness/color a light
/// will return to rather than resetting it to zero.
fn status_text(light: &Light) -> String {
    if !light.on {
        return i18n::t("widgets.hue.status.off");
    }
    let percent = light.brightness_percent.round() as i64;
    match light.bulb_type {
        BulbType::Color => i18n::t_args("widgets.hue.status.color", &[("percent", &percent.to_string())]),
        BulbType::ColorTemperature => {
            let kelvin = light.mirek.map(|mirek| (1_000_000.0 / mirek).round() as i64).unwrap_or(0);
            i18n::t_args("widgets.hue.status.temperature", &[("percent", &percent.to_string()), ("kelvin", &kelvin.to_string())])
        }
        BulbType::Dimmable => i18n::t_args("widgets.hue.status.dimmable", &[("percent", &percent.to_string())]),
    }
}

/// Picks which lights a freshly-spawned card shows, until step 3's
/// settings panel exists to make this a real choice - the first
/// `MAX_ROWS` lights, alphabetically, the same for every card. Sorting by
/// name (rather than bridge order, which is closer to "creation order" and
/// not meaningful to a user) at least makes the arbitrary choice
/// deterministic and easy to reason about while testing.
fn pick_default_lights(mut lights: Vec<Light>) -> Vec<Light> {
    lights.sort_by(|a, b| a.name.cmp(&b.name));
    lights.truncate(MAX_ROWS);
    lights
}

/// One row's live widgets plus the light id it currently shows - `None`
/// while unused (fewer than `MAX_ROWS` lights exist) or before the first
/// successful fetch, in which case the row stays hidden.
struct LightRow {
    container: gtk::Box,
    icon_area: gtk::DrawingArea,
    icon_image: gtk::Image,
    name_label: gtk::Label,
    status_label: gtk::Label,
    bar_area: gtk::DrawingArea,
    light_id: RefCell<Option<String>>,
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
        light_id: RefCell::new(None),
        on: Cell::new(false),
        fraction: Cell::new(0.0),
        rgb: Cell::new((0.6, 0.6, 0.6)),
    }
}

/// All of this widget's live state - one instance per placed card. Owns
/// the bridge connection's *view* only (the connection itself lives in
/// `Config`/`hue_bridge.rs`, shared) - this struct just remembers which
/// four lights are currently shown and their last-fetched values.
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
}

impl HueState {
    fn show_empty(&self, message_key: &str) {
        self.rows_box.set_visible(false);
        self.empty_box.set_visible(true);
        self.empty_message.set_label(&i18n::t(message_key));
        self.badge_label.set_label("");
    }

    fn show_rows(&self, lights: &[Light]) {
        self.rows_box.set_visible(true);
        self.empty_box.set_visible(false);

        let on_count = lights.iter().filter(|l| l.on).count();
        self.badge_label.set_label(&i18n::t_args(
            "widgets.hue.badge",
            &[("on", &on_count.to_string()), ("total", &lights.len().to_string())],
        ));

        for (row, light) in self.rows.iter().zip(lights.iter()) {
            apply_light_to_row(row, light);
            row.container.set_visible(true);
        }
        for row in self.rows.iter().skip(lights.len()) {
            *row.light_id.borrow_mut() = None;
            row.container.set_visible(false);
        }
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
                    let lights = pick_default_lights(hue_bridge::parse_lights(&resources));
                    if lights.is_empty() {
                        state.show_empty("widgets.hue.empty.no_lights");
                    } else {
                        state.show_rows(&lights);
                    }
                }
                _ => state.show_empty("widgets.hue.empty.unreachable"),
            }
        });
    }
}

/// Updates one row's widgets to show `light`'s current state - shared by
/// `HueState::show_rows` (a full refresh) and the toggle/drag handlers'
/// optimistic local update (see `build_content`'s gesture wiring), so both
/// paths render a light exactly the same way.
fn apply_light_to_row(row: &Rc<LightRow>, light: &Light) {
    *row.light_id.borrow_mut() = Some(light.id.clone());
    row.on.set(light.on);
    row.fraction.set((light.brightness_percent / 100.0).clamp(0.0, 1.0));
    row.rgb.set(hex_to_rgb(&light.display_color_hex));

    row.name_label.set_label(&light.name);
    let status_hex = if light.on { light.display_color_hex.clone() } else { "rgba(255,255,255,0.4)".to_string() };
    row.status_label.set_markup(&format!(
        "<span size=\"small\" color=\"{}\">{}</span>",
        gtk::glib::markup_escape_text(&status_hex),
        gtk::glib::markup_escape_text(&status_text(light))
    ));

    let texture = if light.on { load_on_icon(&light.display_color_hex) } else { load_off_icon() };
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

    let state = Rc::new(HueState { badge_label, rows_box, empty_box, empty_message, rows, generation: Cell::new(0) });

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
                let Some(light_id) = row.light_id.borrow().clone() else { return };
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
                    let _ = gtk::gio::spawn_blocking(move || hue_bridge::set_light(&ip, &username, &light_id, new_on, brightness)).await;
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

                let Some(light_id) = row.light_id.borrow().clone() else { return };
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
                    let _ = gtk::gio::spawn_blocking(move || hue_bridge::set_light(&ip, &username, &light_id, new_on, brightness)).await;
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

pub fn spawn() -> WidgetInstance {
    let (_state, content) = build_content();
    WidgetInstance {
        content,
        settings: None,
        to_dict: Box::new(|| serde_json::Value::Null),
        on_reset: None,
        on_change_ready: None,
    }
}

pub fn restore(_data: &serde_json::Value) -> WidgetInstance {
    spawn()
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
