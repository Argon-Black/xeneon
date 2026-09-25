// SPDX-License-Identifier: GPL-3.0-or-later
//! Network throughput widget (SQ footprint): a small network icon and the
//! interface name in the top-left corner, the down/up rates on their own
//! line below that, and a scrolling in/out history graph filling the rest
//! of the card - matching the mockup shown to the user before this widget
//! was built (SSX/SX/S shipped a "text first" version of their own
//! layouts and iterated from there; this is that iteration for SQ).
//!
//! Same interface Auto/pin + rename-only settings as SX/S/SQ's earlier
//! text-only version, plus (per the user's own follow-up request) four
//! appearance settings: the icon+name's size and color together (one
//! `content_scale` slider, one color picker - see `apply_content_scale`),
//! and separate color pickers for the down and up rate, which double as
//! the graph's trace colors (see `refresh`/`draw_graph`) so the two stay
//! in sync rather than needing to be set twice. Persistence and the
//! color-picker rows themselves mirror `temp_gauge.rs`'s own
//! `text_color`/`bar_color`/`content_scale` settings - the closest
//! existing precedent for "small set of appearance knobs on top of the
//! functional settings".
//!
//! The rate line still uses the single-`gtk::Label`-with-markup approach
//! `network_sx.rs`'s doc comment explains (colored/bold/monospace spans
//! for the arrows, fixed-width via `format_rate_fixed` so digit-width
//! changes don't shift anything) - only the interface name moved out to
//! its own header row, since the mockup put it in a corner rather than
//! centered.
//!
//! The header icon switches between Wi-Fi and Ethernet glyphs depending on
//! what the currently-effective interface actually is (`network::
//! is_wireless`, a `/sys/class/net/<name>/{wireless,phy80211}` check), and
//! a small green shield badge appears at the header's far right whenever
//! *any* interface on the machine looks like a VPN tunnel (`network::
//! vpn_active`, scanning every interface's name for the `wg`/`tun`/`tap`/
//! `ppp` prefix heuristic - see that function's own doc comment). This
//! used to check only the card's own displayed interface, on the
//! assumption that `Auto` mode tracking the default route would already
//! catch a connected VPN, since one typically takes it over - true for a
//! full-tunnel VPN, but a real split-tunnel OpenVPN test proved it wrong:
//! `tun0` was up and passing traffic while the default route stayed on
//! the physical interface, so `Auto` kept showing that one and the badge
//! never lit up. Scanning every interface instead means the badge now
//! answers "is a VPN active on this machine", independent of which
//! interface this particular card happens to be showing. The badge's own
//! green is fixed, not user-customizable - it's a status color (VPN
//! on/off), not part of this card's decorative palette.
//!
//! All icons are small bundled SVGs (`assets/network-icon.svg`,
//! `assets/ethernet-icon.svg`, `assets/vpn-icon.svg`), rasterized and
//! cached, loaded the same way `audio.rs` loads its empty-state
//! illustration - deliberately *not* `gtk::Image::from_icon_name`
//! freedesktop icons. `network.rs`'s SSX card originally tried that
//! (`network-receive-symbolic`) and it silently failed to render: the
//! user's active icon theme turned out to ship KDE/Breeze-style symbolic
//! SVGs (`currentColor` + a `ColorScheme-Text` CSS class) rather than the
//! GNOME/Adwaita convention GTK4's symbolic-icon recoloring expects, so
//! nothing painted. A bundled SVG with a plain, known fill color baked in
//! at source sidesteps that whole pipeline - and since the Wi-Fi/Ethernet
//! icon's color is now user-chosen, `load_tinted_icon` goes one step
//! further and substitutes that fill color into the SVG *text* itself
//! before rasterizing (a plain `str::replace`, since these bundled SVGs
//! only ever use that one fill color), rather than trying to recolor the
//! rendered bitmap after the fact.
//!
//! The graph is a `gtk::DrawingArea` painted with Cairo (already pulled in
//! by `gtk4`, no new crate) - two auto-scaled line+fill traces over the
//! last `MAX_HISTORY_SAMPLES` rate samples, in the user's chosen down/up
//! colors. Samples are only recorded when a real rate was computed (not
//! on a "…"/unavailable tick), so a momentary hiccup doesn't draw a fake
//! dip to zero.

use gtk::prelude::*;
use log::{debug, warn};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Once;
use std::time::Instant;

use crate::appearance_popover::{hex_to_rgba, rgba_to_hex};
use crate::i18n_runtime as i18n;
use crate::widgets::network::{default_interface, format_rate_fixed, is_wireless, read_interfaces, vpn_active, InterfaceCounters};
use crate::widgets::registry::WidgetInstance;

const REFRESH_INTERVAL_SECONDS: u32 = 2;
const VALUES_FONT_PX: i32 = 22;

const DEFAULT_NAME_COLOR_HEX: &str = "#ffffff";
/// Same blue/coral pairing as `network.rs`'s SSX card and `network_sx.rs`'s
/// SX card (see `network::Direction::color_hex`) - just the *default*
/// now, since both are user-editable here (see `set_down_color`/
/// `set_up_color`).
const DEFAULT_DOWN_COLOR_HEX: &str = "#5da9e8";
const DEFAULT_UP_COLOR_HEX: &str = "#e8875d";

/// Icon+name size, at `content_scale == 1.0` (100%) - everything scales
/// together off the settings panel's slider, same technique as
/// `temp_gauge.rs`'s `BASE_*`/`content_scale`. Deliberately doesn't touch
/// the rate line, the graph, or the VPN badge - the user asked for the
/// icon and interface label to be resizable, not the whole card.
const BASE_HEADER_ICON_PX: f64 = 18.0;
const BASE_NAME_FONT_PX: f64 = 17.0;
const MIN_CONTENT_SCALE: f64 = 0.5;
const MAX_CONTENT_SCALE: f64 = 2.0;
const DEFAULT_CONTENT_SCALE: f64 = 1.25;

/// Bounds the rate line's width - it no longer includes the interface name
/// (moved to its own header row), so unlike the earlier text-only version
/// of this widget, a long custom label can't affect it at all; this only
/// guards the fixed-width `↓ 999.9M/s  ↑ 999.9M/s` content itself.
const MAX_VALUES_WIDTH_CHARS: i32 = 24;

/// Two minutes of history at the refresh interval - long enough to see a
/// trend, short enough that the graph still redraws cheaply every tick
/// (a plain Vec-backed deque of two `f64`s per sample, nothing fancier).
const MAX_HISTORY_SAMPLES: usize = 60;

const WIFI_ICON_PATH: &str = "assets/network-icon.svg";
const ETHERNET_ICON_PATH: &str = "assets/ethernet-icon.svg";
const VPN_ICON_PATH: &str = "assets/vpn-icon.svg";
/// The fill color these bundled SVGs use for their main glyph - what
/// `load_tinted_icon` looks for and replaces with the user's chosen
/// color. The VPN badge (loaded via the plain, untinted `load_icon_texture`)
/// doesn't use this color at all, so tinting never touches it.
const ICON_SOURCE_FILL: &str = "#ffffff";
/// Rasterized well above the ~18-36px range these icons actually display
/// at (`BASE_HEADER_ICON_PX` times the content-scale range), so they stay
/// crisp rather than looking like upscaled bitmaps - same reasoning as
/// `audio.rs`'s `EMPTY_STATE_ICON_RASTER_PX`, just a much smaller target
/// size since these are small corner icons, not hero art.
const ICON_RASTER_PX: i32 = 96;
/// Bigger than the header's own Wi-Fi/Ethernet icon (`BASE_HEADER_ICON_PX`
/// at 100% scale) - a pill badge with a label reads as a unit even at
/// this size, where a bare small icon didn't.
const VPN_BADGE_ICON_PX: i32 = 20;
const VPN_BADGE_FONT_PX: i32 = 15;
const VPN_COLOR_HEX: &str = "#5DCAA5";

thread_local! {
    // The VPN badge's fixed green never changes, so it only ever needs
    // one cached texture per icon path - same caching shape as
    // `audio.rs`'s `EMPTY_STATE_TEXTURE`. A failed load is cached too, so
    // a missing/corrupt file doesn't get retried on every widget
    // construction.
    static ICON_TEXTURES: RefCell<HashMap<&'static str, Option<gtk::gdk::Texture>>> = RefCell::new(HashMap::new());
    // The Wi-Fi/Ethernet icon's color is user-chosen (per-instance, and
    // changeable at any time), so its cache is keyed by (path, color)
    // instead - still shared across every instance that happens to pick
    // the same color, rather than re-rasterizing on every single
    // `refresh()`/settings-change tick.
    static TINTED_ICON_TEXTURES: RefCell<HashMap<(&'static str, String), Option<gtk::gdk::Texture>>> = RefCell::new(HashMap::new());
}

/// Loads and caches the bundled SVG at `path` unmodified - used only for
/// the VPN badge, whose color is fixed. `None` if it failed to load, in
/// which case the caller just shows no icon rather than a broken image.
fn load_icon_texture(path: &'static str) -> Option<gtk::gdk::Texture> {
    ICON_TEXTURES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(texture) = cache.get(path) {
            return texture.clone();
        }
        let full_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        let texture = gtk::gdk_pixbuf::Pixbuf::from_file_at_size(&full_path, ICON_RASTER_PX, ICON_RASTER_PX)
            .map(|pixbuf| gtk::gdk::Texture::for_pixbuf(&pixbuf))
            .inspect_err(|err| warn!("failed to load {}: {err}", full_path.display()))
            .ok();
        cache.insert(path, texture.clone());
        texture
    })
}

/// Loads the bundled SVG at `path`, substitutes `ICON_SOURCE_FILL` for
/// `hex_color` in its source text, then rasterizes and caches the result -
/// used for the Wi-Fi/Ethernet header icon, whose color the user picks.
/// `None` if the file couldn't be read or the (already-tinted) SVG
/// couldn't be rasterized.
fn load_tinted_icon(path: &'static str, hex_color: &str) -> Option<gtk::gdk::Texture> {
    let key = (path, hex_color.to_string());
    TINTED_ICON_TEXTURES.with(|cache| {
        if let Some(texture) = cache.borrow().get(&key) {
            return texture.clone();
        }
        let full_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
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
        cache.borrow_mut().insert(key, texture.clone());
        texture
    })
}

static INSTALL_CSS: Once = Once::new();

fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!(
            ".xeneon-network-sq-values {{ font-size: {VALUES_FONT_PX}px; color: rgba(255, 255, 255, 0.75); }}\n\
             .xeneon-network-sq-vpn-badge {{ background-color: rgba(93, 202, 165, 0.15); \
             border-radius: 13px; padding: 5px 12px; }}\n\
             .xeneon-network-sq-vpn-badge-label {{ font-size: {VPN_BADGE_FONT_PX}px; font-weight: 700; \
             color: {VPN_COLOR_HEX}; }}"
        ));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

// Per-instance scaled font/color rule for the header name label, keyed by
// each instance's own unique CSS class - one card's icon+name size/color
// never bleeds into another's. Same registry pattern as `temp_gauge.rs`'s
// `GAUGE_CSS` (see `appearance_css::CssRuleRegistry`'s own doc comment).
thread_local! {
    static NAME_CSS: crate::appearance_css::CssRuleRegistry =
        crate::appearance_css::CssRuleRegistry::new(gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

/// All of this widget's live state - one instance per placed widget.
/// Interface pin/custom-label/sample fields mirror `network_sx::
/// NetworkSxState`; `history`/`graph_area` and the appearance fields below
/// are what's new here.
struct NetworkSqState {
    css_class: String,
    icon_image: gtk::Image,
    name_label: gtk::Label,
    vpn_badge: gtk::Box,
    values_label: gtk::Label,
    graph_area: gtk::DrawingArea,

    interface_name: RefCell<Option<String>>,
    custom_label: RefCell<Option<String>>,
    available_interfaces: RefCell<Vec<String>>,
    last_sample: RefCell<Option<(String, u64, u64, Instant)>>,
    /// `(down bytes/sec, up bytes/sec)`, oldest first, capped at
    /// `MAX_HISTORY_SAMPLES` - only ever pushed to on a tick that produced
    /// a real rate (see `refresh`), so a momentary "no second sample yet"
    /// tick doesn't draw a fake dip to zero on the graph.
    history: RefCell<VecDeque<(f64, f64)>>,
    logged_unavailable: Cell<bool>,

    name_color: RefCell<gtk::gdk::RGBA>,
    down_color: RefCell<gtk::gdk::RGBA>,
    up_color: RefCell<gtk::gdk::RGBA>,
    content_scale: Cell<f64>,
    /// Whether the currently-effective interface is wireless, as of the
    /// last `refresh()` - cached here (rather than re-derived) so a
    /// settings-only change (content scale, icon color) can re-render the
    /// icon at the right size/color/type without needing a fresh
    /// `/proc`/`/sys` read of its own.
    current_wireless: Cell<bool>,
}

impl NetworkSqState {
    /// Same auto-pick logic as `network::NetworkState::effective_interface_name`.
    fn effective_interface_name(&self, interfaces: &[InterfaceCounters]) -> Option<String> {
        if let Some(pinned) = self.interface_name.borrow().clone() {
            return Some(pinned);
        }
        if let Some(default) = default_interface() {
            if interfaces.iter().any(|(name, _, _)| *name == default) {
                return Some(default);
            }
        }
        interfaces.first().map(|(name, _, _)| name.clone())
    }

    fn set_interface(&self, name: Option<String>) {
        match &name {
            Some(name) => debug!("interface pinned to {name}"),
            None => debug!("interface set back to auto-pick"),
        }
        *self.interface_name.borrow_mut() = name;
        // A different interface means a different traffic history -
        // starting the graph over avoids a misleading jump between two
        // unrelated interfaces' rates on the same trace.
        self.history.borrow_mut().clear();
        self.refresh();
    }

    fn set_custom_label(&self, text: Option<String>) {
        *self.custom_label.borrow_mut() = text.filter(|s| !s.is_empty());
        self.refresh();
    }

    fn set_name_color(&self, rgba: gtk::gdk::RGBA) {
        *self.name_color.borrow_mut() = rgba;
        self.apply_content_scale(); // font-size and color live in the same CSS rule, and the icon needs re-tinting too
    }

    fn set_down_color(&self, rgba: gtk::gdk::RGBA) {
        *self.down_color.borrow_mut() = rgba;
        self.refresh(); // rebuilds the rate line's markup and repaints the graph in the new color
    }

    fn set_up_color(&self, rgba: gtk::gdk::RGBA) {
        *self.up_color.borrow_mut() = rgba;
        self.refresh();
    }

    /// Back to this widget's out-of-the-box defaults - goes through the
    /// same setters as everything else (not a shortcut that pokes fields
    /// directly), so every side effect - CSS rule, icon re-tint, markup,
    /// graph repaint - happens exactly like a normal edit would. Mirrors
    /// `ClockState::reset`; called from the appearance popover's reset
    /// button via `WidgetInstance::on_reset`, wired up in `spawn`/`restore`
    /// below - previously `on_reset` was left `None` here, which is why
    /// resetting a card's appearance left its interface pin/custom label/
    /// colors/size untouched.
    fn reset(&self) {
        self.set_interface(None);
        self.set_custom_label(None);
        self.set_name_color(hex_to_rgba(DEFAULT_NAME_COLOR_HEX));
        self.set_down_color(hex_to_rgba(DEFAULT_DOWN_COLOR_HEX));
        self.set_up_color(hex_to_rgba(DEFAULT_UP_COLOR_HEX));
        self.set_content_scale(DEFAULT_CONTENT_SCALE);
    }

    fn set_content_scale(&self, scale: f64) {
        self.content_scale.set(scale.clamp(MIN_CONTENT_SCALE, MAX_CONTENT_SCALE));
        self.apply_content_scale();
    }

    /// Rebuilds this instance's name-label CSS rule (font size + color)
    /// from `content_scale`/`name_color`, and re-renders the header icon
    /// at the matching size/color - mirrors `TempGaugeState::
    /// apply_content_scale`. Called from every setter that touches either
    /// of those two settings, not just `set_content_scale` itself.
    fn apply_content_scale(&self) {
        let scale = self.content_scale.get();
        let name_hex = rgba_to_hex(&self.name_color.borrow());
        let rule = format!(
            ".{class} .xeneon-network-sq-name {{ font-size: {size}px; color: {name_hex}; }}",
            class = self.css_class,
            size = (BASE_NAME_FONT_PX * scale).round() as i32,
        );
        NAME_CSS.with(|registry| registry.set_rule(&self.css_class, rule));
        self.icon_image.set_pixel_size((BASE_HEADER_ICON_PX * scale).round() as i32);
        self.apply_icon(self.current_wireless.get());
    }

    /// Loads (and applies) the correctly-tinted Wi-Fi or Ethernet texture
    /// for `wireless` - split out from `refresh()` so a settings-only
    /// color/scale change can re-tint without needing a fresh interface
    /// read.
    fn apply_icon(&self, wireless: bool) {
        let hex = rgba_to_hex(&self.name_color.borrow());
        let path = if wireless { WIFI_ICON_PATH } else { ETHERNET_ICON_PATH };
        if let Some(texture) = load_tinted_icon(path, &hex) {
            self.icon_image.set_paintable(Some(&texture));
        }
    }

    /// Same shape as `network::NetworkState::display_label`.
    fn display_label(&self, effective_name: Option<&str>) -> String {
        let name = if self.interface_name.borrow().is_some() {
            self.custom_label
                .borrow()
                .as_ref()
                .filter(|s| !s.is_empty())
                .cloned()
                .or_else(|| effective_name.map(str::to_string))
        } else {
            effective_name.map(str::to_string)
        };
        name.unwrap_or_else(|| i18n::t("widgets.network.title"))
    }

    /// Re-reads every interface's counters, recomputes both rates for
    /// whichever interface is currently effective, updates the header/
    /// values labels, records a history sample, and queues a graph
    /// repaint. Called on every tick, every setter above, and a language
    /// change - same call sites as `network_sx::NetworkSxState::refresh`.
    fn refresh(&self) {
        let interfaces = read_interfaces();
        *self.available_interfaces.borrow_mut() = interfaces.iter().map(|(name, _, _)| name.clone()).collect();

        let effective_name = self.effective_interface_name(&interfaces);
        self.name_label.set_text(&self.display_label(effective_name.as_deref()));

        // Wi-Fi vs Ethernet icon - derived from whichever interface is
        // currently effective, re-checked every tick since `Auto` mode
        // can switch interfaces without any setting changing. `None` (no
        // interface at all) falls back to the Ethernet icon, same as
        // `is_wireless` would answer for an interface that doesn't exist.
        let wireless = effective_name.as_deref().is_some_and(is_wireless);
        self.current_wireless.set(wireless);
        self.apply_icon(wireless);
        // The VPN badge, on the other hand, checks *every* interface, not
        // just the effective one - see `network::vpn_active`'s doc
        // comment for why: a split-tunnel VPN can be up and passing
        // traffic without ever taking over the default route, in which
        // case `Auto` keeps displaying the physical interface and this
        // card would otherwise never show the badge at all.
        self.vpn_badge.set_visible(vpn_active(&interfaces));

        let counters = effective_name.as_ref().and_then(|name| interfaces.iter().find(|(n, _, _)| n == name));

        match counters {
            None => {
                if !self.logged_unavailable.replace(true) {
                    warn!(
                        "no reading for interface {} ({} interfaces seen)",
                        effective_name.as_deref().unwrap_or("<none>"),
                        self.available_interfaces.borrow().len()
                    );
                }
                *self.last_sample.borrow_mut() = None;
                self.values_label.set_text("--");
                self.values_label.set_tooltip_text(Some(&i18n::t("widgets.network.unavailable")));
            }
            Some((name, rx_bytes, tx_bytes)) => {
                if self.logged_unavailable.replace(false) {
                    debug!("interface reading recovered: {name}");
                }
                let now = Instant::now();

                let mut last_sample = self.last_sample.borrow_mut();
                let rates = match last_sample.as_ref() {
                    Some((prev_name, prev_rx, prev_tx, prev_when))
                        if prev_name == name && *rx_bytes >= *prev_rx && *tx_bytes >= *prev_tx =>
                    {
                        let elapsed = now.duration_since(*prev_when).as_secs_f64();
                        (elapsed > 0.0).then(|| {
                            ((*rx_bytes - *prev_rx) as f64 / elapsed, (*tx_bytes - *prev_tx) as f64 / elapsed)
                        })
                    }
                    _ => None,
                };
                *last_sample = Some((name.clone(), *rx_bytes, *tx_bytes, now));
                drop(last_sample);

                if let Some((down, up)) = rates {
                    let mut history = self.history.borrow_mut();
                    history.push_back((down, up));
                    while history.len() > MAX_HISTORY_SAMPLES {
                        history.pop_front();
                    }
                }

                // Fixed-width, monospace - same stability reasoning as
                // `network_sx.rs`/`network_s.rs` (see `format_rate_fixed`'s
                // doc comment). Colors come from `down_color`/`up_color`
                // (user-editable), not a fixed constant.
                let (down_text, up_text) = match rates {
                    Some((down, up)) => (format!("{}/s", format_rate_fixed(down)), format!("{}/s", format_rate_fixed(up))),
                    None => (format!("{:>6}/s", "…"), format!("{:>6}/s", "…")),
                };
                let down_hex = rgba_to_hex(&self.down_color.borrow());
                let up_hex = rgba_to_hex(&self.up_color.borrow());
                self.values_label.set_markup(&format!(
                    "<span color=\"{down_hex}\" weight=\"bold\" font_family=\"monospace\">↓ {}</span>   \
                     <span color=\"{up_hex}\" weight=\"bold\" font_family=\"monospace\">↑ {}</span>",
                    gtk::glib::markup_escape_text(&down_text),
                    gtk::glib::markup_escape_text(&up_text),
                ));
                self.values_label.set_tooltip_text(Some(name));
            }
        }

        self.graph_area.queue_draw();
    }

    fn to_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "interface": self.interface_name.borrow().clone(),
            "custom_label": self.custom_label.borrow().clone(),
            "name_color": rgba_to_hex(&self.name_color.borrow()),
            "down_color": rgba_to_hex(&self.down_color.borrow()),
            "up_color": rgba_to_hex(&self.up_color.borrow()),
            "content_scale": self.content_scale.get(),
        })
    }

    fn apply_dict(&self, data: &serde_json::Value) {
        if data.get("interface").is_some() {
            let name = data.get("interface").and_then(|v| v.as_str()).map(str::to_string);
            self.set_interface(name);
        }
        if data.get("custom_label").is_some() {
            let label = data.get("custom_label").and_then(|v| v.as_str()).map(str::to_string);
            self.set_custom_label(label);
        }
        if let Some(v) = data.get("name_color").and_then(|v| v.as_str()) {
            self.set_name_color(hex_to_rgba(v));
        }
        if let Some(v) = data.get("down_color").and_then(|v| v.as_str()) {
            self.set_down_color(hex_to_rgba(v));
        }
        if let Some(v) = data.get("up_color").and_then(|v| v.as_str()) {
            self.set_up_color(hex_to_rgba(v));
        }
        if let Some(v) = data.get("content_scale").and_then(|v| v.as_f64()) {
            self.set_content_scale(v);
        }
    }
}

/// True if `entries[index]` is a real pinned interface (`Some`) rather
/// than the "Auto" placeholder (`None` at index 0).
fn is_manual(entries: &[Option<String>], index: usize) -> bool {
    entries.get(index).map(|entry| entry.is_some()).unwrap_or(false)
}

/// Paints one trace (line + a faint fill below it) for `samples`, scaled
/// so its own peak sits at `top_fraction` of the drawing area's height -
/// called twice per repaint, once for down and once for up, each against
/// its own peak (not a shared one), so a quiet upload doesn't flatten
/// into a barely-visible line just because download is far busier, or
/// vice versa.
fn draw_trace(cr: &gtk::cairo::Context, width: f64, height: f64, samples: &[f64], color: gtk::gdk::RGBA, top_fraction: f64) {
    let peak = samples.iter().cloned().fold(0.0_f64, f64::max).max(1.0);
    let n = samples.len();
    if n < 2 {
        return;
    }
    let step_x = width / (n - 1) as f64;
    let point = |i: usize, value: f64| {
        let x = step_x * i as f64;
        let y = height - (value / peak) * height * top_fraction;
        (x, y)
    };

    let (r, g, b) = (color.red() as f64, color.green() as f64, color.blue() as f64);

    // Fill: the line's path, then straight down to the baseline and back
    // along it to the start - a classic area-chart fill, drawn first so
    // the stroked line sits on top of it.
    cr.new_path();
    let (x0, y0) = point(0, samples[0]);
    cr.move_to(x0, y0);
    for (i, value) in samples.iter().enumerate().skip(1) {
        let (x, y) = point(i, *value);
        cr.line_to(x, y);
    }
    let (x_last, _) = point(n - 1, 0.0);
    cr.line_to(x_last, height);
    cr.line_to(x0, height);
    cr.close_path();
    cr.set_source_rgba(r, g, b, 0.18);
    let _ = cr.fill();

    cr.new_path();
    cr.move_to(x0, y0);
    for (i, value) in samples.iter().enumerate().skip(1) {
        let (x, y) = point(i, *value);
        cr.line_to(x, y);
    }
    cr.set_line_width(2.0);
    cr.set_line_join(gtk::cairo::LineJoin::Round);
    cr.set_source_rgba(r, g, b, 0.95);
    let _ = cr.stroke();
}

/// Draws both traces over the whole `DrawingArea` - down on top of up
/// (drawn second, so its typically-smaller fill doesn't get buried under
/// down's), each independently auto-scaled by `draw_trace`, in the user's
/// chosen down/up colors. Blank (no traces at all) until there are at
/// least two history samples, since a single point has no line to draw.
fn draw_graph(state: &NetworkSqState, cr: &gtk::cairo::Context, width: i32, height: i32) {
    let history = state.history.borrow();
    if history.len() < 2 {
        return;
    }
    let down: Vec<f64> = history.iter().map(|(d, _)| *d).collect();
    let up: Vec<f64> = history.iter().map(|(_, u)| *u).collect();
    drop(history);

    // Leaves a little headroom at the top so a peak doesn't touch the
    // card's edge.
    const TOP_FRACTION: f64 = 0.85;
    draw_trace(cr, width as f64, height as f64, &up, *state.up_color.borrow(), TOP_FRACTION);
    draw_trace(cr, width as f64, height as f64, &down, *state.down_color.borrow(), TOP_FRACTION);
}

fn build_content() -> (Rc<NetworkSqState>, gtk::Widget) {
    ensure_css_installed();

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let css_class = format!("xeneon-networksq-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.add_css_class(&css_class);
    root.set_margin_start(14);
    root.set_margin_end(14);
    root.set_margin_top(12);
    root.set_margin_bottom(10);

    // No `set_halign(Start)` here (unlike the earlier version of this
    // widget) - left at the default `Fill` so this row spans the full
    // card width, which the spacer below needs to push the VPN badge to
    // the far right edge rather than right up against the name.
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);

    // Paintable set in the first `refresh()` call below, once the
    // effective interface (and therefore Wi-Fi vs Ethernet) is known -
    // starts empty rather than defaulting to one or the other. Size is
    // set in `apply_content_scale`, called from the same first `refresh()`.
    let icon_image = gtk::Image::new();
    header.append(&icon_image);

    let name_label = gtk::Label::new(None);
    name_label.add_css_class("xeneon-network-sq-name");
    name_label.set_halign(gtk::Align::Start);
    header.append(&name_label);

    let header_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    header_spacer.set_hexpand(true);
    header.append(&header_spacer);

    // A pill (icon + "VPN" label), not a bare icon - a small icon on its
    // own didn't read clearly at this size; the label makes it
    // unambiguous at a glance, matching the mockup shown to the user.
    let vpn_badge = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    vpn_badge.add_css_class("xeneon-network-sq-vpn-badge");
    vpn_badge.set_valign(gtk::Align::Center);
    let vpn_badge_icon = gtk::Image::new();
    if let Some(texture) = load_icon_texture(VPN_ICON_PATH) {
        vpn_badge_icon.set_paintable(Some(&texture));
    }
    vpn_badge_icon.set_pixel_size(VPN_BADGE_ICON_PX);
    vpn_badge.append(&vpn_badge_icon);
    let vpn_badge_label = gtk::Label::new(Some(&i18n::t("widgets.network.vpn_badge_label")));
    vpn_badge_label.add_css_class("xeneon-network-sq-vpn-badge-label");
    vpn_badge.append(&vpn_badge_label);
    vpn_badge.set_tooltip_text(Some(&i18n::t("widgets.network.vpn_active")));
    // Hidden until the first `refresh()` call below decides whether any
    // interface actually looks like a VPN.
    vpn_badge.set_visible(false);
    header.append(&vpn_badge);

    root.append(&header);

    let values_label = gtk::Label::new(None);
    values_label.add_css_class("xeneon-network-sq-values");
    values_label.set_halign(gtk::Align::Start);
    values_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    values_label.set_max_width_chars(MAX_VALUES_WIDTH_CHARS);
    root.append(&values_label);

    let graph_area = gtk::DrawingArea::new();
    graph_area.set_hexpand(true);
    graph_area.set_vexpand(true);
    graph_area.set_margin_top(6);
    root.append(&graph_area);

    let state = Rc::new(NetworkSqState {
        css_class,
        icon_image,
        name_label,
        vpn_badge,
        values_label,
        graph_area: graph_area.clone(),
        interface_name: RefCell::new(None),
        custom_label: RefCell::new(None),
        available_interfaces: RefCell::new(Vec::new()),
        last_sample: RefCell::new(None),
        history: RefCell::new(VecDeque::with_capacity(MAX_HISTORY_SAMPLES)),
        logged_unavailable: Cell::new(false),
        name_color: RefCell::new(hex_to_rgba(DEFAULT_NAME_COLOR_HEX)),
        down_color: RefCell::new(hex_to_rgba(DEFAULT_DOWN_COLOR_HEX)),
        up_color: RefCell::new(hex_to_rgba(DEFAULT_UP_COLOR_HEX)),
        content_scale: Cell::new(DEFAULT_CONTENT_SCALE),
        current_wireless: Cell::new(true),
    });

    graph_area.set_draw_func({
        let state = state.clone();
        move |_area, cr, width, height| draw_graph(&state, cr, width, height)
    });

    // `apply_content_scale()` sets the name CSS rule and the icon's
    // initial size/color; `refresh()` (which also calls `apply_icon`)
    // fills in the real interface data right after.
    state.apply_content_scale();
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
    i18n::on_change({
        let state = state.clone();
        move || state.refresh()
    });

    (state, root.upcast())
}

fn make_row(widgets: &[&gtk::Widget]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for w in widgets {
        row.append(*w);
    }
    row
}

/// Builds the settings panel: which interface drives the display, the
/// rename field once one is pinned (same as `network_sx::build_settings`),
/// then the four appearance settings this card adds on top - icon+name
/// color, icon+name size, download color, upload color - mirroring
/// `temp_gauge.rs`'s own color-picker/scale-slider rows.
fn build_settings(state: Rc<NetworkSqState>) -> (gtk::Widget, Box<dyn Fn()>) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(260, -1);

    let interface_label_widget = gtk::Label::new(Some(&i18n::t("widgets.network.settings.interface")));
    interface_label_widget.set_halign(gtk::Align::Start);
    root.append(&interface_label_widget);

    let mut initial_entries: Vec<Option<String>> = vec![None];
    let current_interface = state.interface_name.borrow().clone();
    for name in state.available_interfaces.borrow().iter() {
        if !initial_entries.iter().any(|e| e.as_deref() == Some(name.as_str())) {
            initial_entries.push(Some(name.clone()));
        }
    }
    if let Some(name) = &current_interface {
        if !initial_entries.iter().any(|e| e.as_deref() == Some(name.as_str())) {
            initial_entries.push(Some(name.clone()));
        }
    }
    let entries = Rc::new(RefCell::new(initial_entries));

    let interface_dropdown = gtk::DropDown::new(Some(gtk::StringList::new(&[])), gtk::Expression::NONE);
    interface_dropdown.set_hexpand(true);
    root.append(&interface_dropdown);

    let custom_label_label = gtk::Label::new(Some(&i18n::t("widgets.network.settings.custom_label")));
    custom_label_label.set_halign(gtk::Align::Start);
    root.append(&custom_label_label);
    let custom_label_entry = gtk::Entry::new();
    custom_label_entry.set_text(state.custom_label.borrow().as_deref().unwrap_or(""));
    root.append(&custom_label_entry);

    let refresh_interface_model: Rc<dyn Fn()> = {
        let entries = entries.clone();
        let state = state.clone();
        let interface_dropdown = interface_dropdown.clone();
        let custom_label_label = custom_label_label.clone();
        let custom_label_entry = custom_label_entry.clone();
        Rc::new(move || {
            let entries_ref = entries.borrow();
            let names: Vec<String> = entries_ref
                .iter()
                .map(|entry| match entry {
                    None => i18n::t("widgets.network.settings.interface_auto"),
                    Some(name) => name.clone(),
                })
                .collect();
            let current = state.interface_name.borrow().clone();
            let selected_index = entries_ref.iter().position(|e| *e == current).unwrap_or(0);
            let manual = is_manual(&entries_ref, selected_index);
            drop(entries_ref);

            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            interface_dropdown.set_model(Some(&gtk::StringList::new(&name_refs)));
            interface_dropdown.set_selected(selected_index as u32);
            custom_label_label.set_sensitive(manual);
            custom_label_entry.set_sensitive(manual);
        })
    };
    refresh_interface_model();

    interface_dropdown.connect_selected_notify({
        let state = state.clone();
        let entries = entries.clone();
        let custom_label_label = custom_label_label.clone();
        let custom_label_entry = custom_label_entry.clone();
        move |dropdown| {
            let index = dropdown.selected() as usize;
            let entries_ref = entries.borrow();
            if let Some(entry) = entries_ref.get(index) {
                state.set_interface(entry.clone());
            }
            let manual = is_manual(&entries_ref, index);
            drop(entries_ref);
            custom_label_label.set_sensitive(manual);
            custom_label_entry.set_sensitive(manual);
        }
    });
    custom_label_entry.connect_changed({
        let state = state.clone();
        move |entry| state.set_custom_label(Some(entry.text().to_string()))
    });

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let name_color_label = gtk::Label::new(Some(&i18n::t("widgets.network.settings.name_color")));
    name_color_label.set_hexpand(true);
    name_color_label.set_halign(gtk::Align::Start);
    let name_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    name_color_button.set_rgba(&state.name_color.borrow());
    root.append(&make_row(&[name_color_label.upcast_ref(), name_color_button.upcast_ref()]));

    let scale_label = gtk::Label::new(Some(&i18n::t("widgets.network.settings.content_scale")));
    scale_label.set_halign(gtk::Align::Start);
    root.append(&scale_label);
    let scale_slider =
        gtk::Scale::with_range(gtk::Orientation::Horizontal, MIN_CONTENT_SCALE * 100.0, MAX_CONTENT_SCALE * 100.0, 1.0);
    scale_slider.set_value(state.content_scale.get() * 100.0);
    scale_slider.set_draw_value(true);
    scale_slider.set_value_pos(gtk::PositionType::Right);
    root.append(&scale_slider);

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let down_color_label = gtk::Label::new(Some(&i18n::t("widgets.network.settings.down_color")));
    down_color_label.set_hexpand(true);
    down_color_label.set_halign(gtk::Align::Start);
    let down_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    down_color_button.set_rgba(&state.down_color.borrow());
    root.append(&make_row(&[down_color_label.upcast_ref(), down_color_button.upcast_ref()]));

    let up_color_label = gtk::Label::new(Some(&i18n::t("widgets.network.settings.up_color")));
    up_color_label.set_hexpand(true);
    up_color_label.set_halign(gtk::Align::Start);
    let up_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    up_color_button.set_rgba(&state.up_color.borrow());
    root.append(&make_row(&[up_color_label.upcast_ref(), up_color_button.upcast_ref()]));

    name_color_button.connect_rgba_notify({
        let state = state.clone();
        move |b| state.set_name_color(b.rgba())
    });
    scale_slider.connect_value_changed({
        let state = state.clone();
        move |s| state.set_content_scale(s.value() / 100.0)
    });
    down_color_button.connect_rgba_notify({
        let state = state.clone();
        move |b| state.set_down_color(b.rgba())
    });
    up_color_button.connect_rgba_notify({
        let state = state.clone();
        move |b| state.set_up_color(b.rgba())
    });

    i18n::on_change({
        let interface_label_widget = interface_label_widget.clone();
        let custom_label_label = custom_label_label.clone();
        let name_color_label = name_color_label.clone();
        let scale_label = scale_label.clone();
        let down_color_label = down_color_label.clone();
        let up_color_label = up_color_label.clone();
        let refresh_interface_model = refresh_interface_model.clone();
        move || {
            interface_label_widget.set_label(&i18n::t("widgets.network.settings.interface"));
            custom_label_label.set_label(&i18n::t("widgets.network.settings.custom_label"));
            name_color_label.set_label(&i18n::t("widgets.network.settings.name_color"));
            scale_label.set_label(&i18n::t("widgets.network.settings.content_scale"));
            down_color_label.set_label(&i18n::t("widgets.network.settings.down_color"));
            up_color_label.set_label(&i18n::t("widgets.network.settings.up_color"));
            refresh_interface_model();
        }
    });

    // Re-reads every control's displayed value from `state` - needed
    // after `state.reset()` (called from the appearance popover's reset
    // button, see `on_reset` in `spawn`/`restore` below) changes the
    // model directly, since a control otherwise only pushes edits one-way
    // and doesn't notice a programmatic change underneath it. Mirrors
    // `clock.rs::build_settings`'s own `resync`, including its ordering:
    // every value is read out of `state` into an owned local *before*
    // touching any control, since each setter below fires that control's
    // own "changed" signal synchronously, which calls back into
    // `state.set_*` - a `borrow_mut()` on the very same `RefCell` a
    // `.borrow()` here would still be holding as a live temporary, which
    // panics.
    let resync: Box<dyn Fn()> = Box::new({
        let state = state.clone();
        let custom_label_entry = custom_label_entry.clone();
        let name_color_button = name_color_button.clone();
        let scale_slider = scale_slider.clone();
        let down_color_button = down_color_button.clone();
        let up_color_button = up_color_button.clone();
        let refresh_interface_model = refresh_interface_model.clone();
        move || {
            let custom_label = state.custom_label.borrow().clone();
            let name_color = *state.name_color.borrow();
            let down_color = *state.down_color.borrow();
            let up_color = *state.up_color.borrow();
            let content_scale = state.content_scale.get();

            custom_label_entry.set_text(custom_label.as_deref().unwrap_or(""));
            name_color_button.set_rgba(&name_color);
            scale_slider.set_value(content_scale * 100.0);
            down_color_button.set_rgba(&down_color);
            up_color_button.set_rgba(&up_color);
            // Also resyncs the interface dropdown's selection and the
            // custom-label controls' sensitivity from `state.interface_name`.
            refresh_interface_model();
        }
    });

    (root.upcast(), resync)
}

pub fn spawn() -> WidgetInstance {
    let (state, content) = build_content();
    let (settings, resync) = build_settings(state.clone());
    let on_reset = {
        let state = state.clone();
        move || {
            state.reset();
            resync();
        }
    };
    WidgetInstance {
        content,
        settings: Some(settings),
        to_dict: Box::new(move || state.to_dict()),
        on_reset: Some(Box::new(on_reset)),
        on_change_ready: None,
    }
}

pub fn restore(data: &serde_json::Value) -> WidgetInstance {
    let (state, content) = build_content();
    state.apply_dict(data);
    let (settings, resync) = build_settings(state.clone());
    let on_reset = {
        let state = state.clone();
        move || {
            state.reset();
            resync();
        }
    };
    WidgetInstance {
        content,
        settings: Some(settings),
        to_dict: Box::new(move || state.to_dict()),
        on_reset: Some(Box::new(on_reset)),
        on_change_ready: None,
    }
}
