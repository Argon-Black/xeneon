// SPDX-License-Identifier: GPL-3.0-or-later
//! Network throughput widget (SQ footprint): a small network icon and the
//! interface name in the top-left corner, the down/up rates on their own
//! line below that, and a scrolling in/out history graph filling the rest
//! of the card - matching the mockup shown to the user before this widget
//! was built (SSX/SX/S shipped a "text first" version of their own
//! layouts and iterated from there; this is that iteration for SQ).
//!
//! Same interface Auto/pin + rename-only settings as SX/S/SQ's earlier
//! text-only version. The rate line still uses the single-`gtk::Label`-
//! with-markup approach `network_sx.rs`'s doc comment explains (colored/
//! bold/monospace spans for the arrows, fixed-width via `format_rate_fixed`
//! so digit-width changes don't shift anything) - only the interface name
//! moved out to its own header row, since the mockup put it in a corner
//! rather than centered.
//!
//! The header icon switches between Wi-Fi and Ethernet glyphs depending on
//! what the currently-effective interface actually is (`network::
//! is_wireless`, a `/sys/class/net/<name>/{wireless,phy80211}` check), and
//! a small green shield badge appears at the header's far right whenever
//! that interface looks like a VPN tunnel (`network::is_vpn_like`, a name-
//! prefix heuristic - `wg`/`tun`/`tap`/`ppp`). The badge is tied to
//! *this card's own displayed interface* being a VPN, not "is some VPN
//! active anywhere on the machine" - in `Auto` mode those usually end up
//! meaning the same thing anyway, since a connected VPN typically takes
//! over the default route (which is exactly what `Auto` follows), but a
//! card pinned to a specific physical interface won't light up just
//! because an unrelated VPN happens to be up elsewhere.
//!
//! All three icons are small bundled SVGs (`assets/network-icon.svg`,
//! `assets/ethernet-icon.svg`, `assets/vpn-icon.svg`), rasterized once
//! each and cached, loaded the same way `audio.rs` loads its empty-state
//! illustration - deliberately *not* `gtk::Image::from_icon_name`
//! freedesktop icons. `network.rs`'s SSX card originally tried that
//! (`network-receive-symbolic`) and it silently failed to render: the
//! user's active icon theme turned out to ship KDE/Breeze-style symbolic
//! SVGs (`currentColor` + a `ColorScheme-Text` CSS class) rather than the
//! GNOME/Adwaita convention GTK4's symbolic-icon recoloring expects, so
//! nothing painted. A bundled SVG with its fill color baked in at source
//! and rendered as a plain raster texture sidesteps that whole pipeline.
//!
//! The graph is a `gtk::DrawingArea` painted with Cairo (already pulled in
//! by `gtk4`, no new crate) - two auto-scaled line+fill traces over the
//! last `MAX_HISTORY_SAMPLES` rate samples, blue for down and coral for
//! up, matching the rest of this widget's color convention. Samples are
//! only recorded when a real rate was computed (not on a "…"/unavailable
//! tick), so a momentary hiccup doesn't draw a fake dip to zero.

use gtk::prelude::*;
use log::{debug, warn};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Once;
use std::time::Instant;

use crate::i18n_runtime as i18n;
use crate::widgets::network::{default_interface, format_rate_fixed, is_vpn_like, is_wireless, read_interfaces, InterfaceCounters};
use crate::widgets::registry::WidgetInstance;

const REFRESH_INTERVAL_SECONDS: u32 = 2;
const NAME_FONT_PX: i32 = 17;
const VALUES_FONT_PX: i32 = 22;

/// Same blue/coral pairing as `network.rs`'s SSX card and `network_sx.rs`'s
/// SX card (see `network::Direction::color_hex`).
const DOWN_COLOR_HEX: &str = "#5da9e8";
const UP_COLOR_HEX: &str = "#e8875d";
const DOWN_COLOR_RGB: (f64, f64, f64) = (0.365, 0.663, 0.910);
const UP_COLOR_RGB: (f64, f64, f64) = (0.910, 0.529, 0.365);

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
/// Rasterized well above the ~18px these icons actually display at, so
/// they stay crisp rather than looking like upscaled bitmaps - same
/// reasoning as `audio.rs`'s `EMPTY_STATE_ICON_RASTER_PX`, just a much
/// smaller target size since these are small corner icons, not hero art.
const ICON_RASTER_PX: i32 = 96;
const HEADER_ICON_DISPLAY_PX: i32 = 18;
const VPN_BADGE_DISPLAY_PX: i32 = 16;

thread_local! {
    // Keyed by source path, loaded once per icon and reused by every
    // instance of this widget - same caching shape as `audio.rs`'s
    // `EMPTY_STATE_TEXTURE`, just holding three icons instead of one
    // (the header icon switches between Wi-Fi/Ethernet at runtime, so a
    // single cached texture the way the first version of this widget had
    // isn't enough any more). A failed load is cached too, so a missing/
    // corrupt file doesn't get retried on every single widget
    // construction or every `refresh()` tick.
    static ICON_TEXTURES: RefCell<std::collections::HashMap<&'static str, Option<gtk::gdk::Texture>>> =
        RefCell::new(std::collections::HashMap::new());
}

/// Loads and caches the bundled SVG at `path` (relative to the crate
/// root, e.g. `WIFI_ICON_PATH`) as a raster texture - `None` if it failed
/// to load, in which case the caller just shows no icon rather than a
/// broken image.
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

static INSTALL_CSS: Once = Once::new();

fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!(
            ".xeneon-network-sq-name {{ font-size: {NAME_FONT_PX}px; color: rgba(255, 255, 255, 0.75); }}\n\
             .xeneon-network-sq-values {{ font-size: {VALUES_FONT_PX}px; color: rgba(255, 255, 255, 0.75); }}"
        ));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

/// All of this widget's live state - one instance per placed widget.
/// Interface pin/custom-label/sample fields mirror `network_sx::
/// NetworkSxState` exactly; `history` and `graph_area` are what's new here.
struct NetworkSqState {
    icon_image: gtk::Image,
    name_label: gtk::Label,
    vpn_badge: gtk::Image,
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

        // Wi-Fi vs Ethernet icon, and the VPN badge - both derived from
        // whichever interface is currently effective, re-checked every
        // tick since `Auto` mode can switch to a different interface (or
        // a VPN can come up/go down) without any setting changing. `None`
        // (no interface at all) falls back to the Ethernet icon and no
        // VPN badge, same as `is_wireless`/`is_vpn_like` would answer for
        // an interface that doesn't exist.
        let wireless = effective_name.as_deref().is_some_and(is_wireless);
        let vpn = effective_name.as_deref().is_some_and(is_vpn_like);
        let icon_path = if wireless { WIFI_ICON_PATH } else { ETHERNET_ICON_PATH };
        if let Some(texture) = load_icon_texture(icon_path) {
            self.icon_image.set_paintable(Some(&texture));
        }
        self.vpn_badge.set_visible(vpn);

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
                // doc comment).
                let (down_text, up_text) = match rates {
                    Some((down, up)) => (format!("{}/s", format_rate_fixed(down)), format!("{}/s", format_rate_fixed(up))),
                    None => (format!("{:>6}/s", "…"), format!("{:>6}/s", "…")),
                };
                self.values_label.set_markup(&format!(
                    "<span color=\"{DOWN_COLOR_HEX}\" weight=\"bold\" font_family=\"monospace\">↓ {}</span>   \
                     <span color=\"{UP_COLOR_HEX}\" weight=\"bold\" font_family=\"monospace\">↑ {}</span>",
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
fn draw_trace(cr: &gtk::cairo::Context, width: f64, height: f64, samples: &[f64], color: (f64, f64, f64), top_fraction: f64) {
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

    let (r, g, b) = color;

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
/// down's), each independently auto-scaled by `draw_trace`. Blank (no
/// traces at all) until there are at least two history samples, since a
/// single point has no line to draw.
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
    draw_trace(cr, width as f64, height as f64, &up, UP_COLOR_RGB, TOP_FRACTION);
    draw_trace(cr, width as f64, height as f64, &down, DOWN_COLOR_RGB, TOP_FRACTION);
}

fn build_content() -> (Rc<NetworkSqState>, gtk::Widget) {
    ensure_css_installed();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
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
    // starts empty rather than defaulting to one or the other.
    let icon_image = gtk::Image::new();
    icon_image.set_pixel_size(HEADER_ICON_DISPLAY_PX);
    header.append(&icon_image);

    let name_label = gtk::Label::new(None);
    name_label.add_css_class("xeneon-network-sq-name");
    name_label.set_halign(gtk::Align::Start);
    header.append(&name_label);

    let header_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    header_spacer.set_hexpand(true);
    header.append(&header_spacer);

    let vpn_badge = gtk::Image::new();
    if let Some(texture) = load_icon_texture(VPN_ICON_PATH) {
        vpn_badge.set_paintable(Some(&texture));
    }
    vpn_badge.set_pixel_size(VPN_BADGE_DISPLAY_PX);
    vpn_badge.set_tooltip_text(Some(&i18n::t("widgets.network.vpn_active")));
    // Hidden until the first `refresh()` call below decides whether the
    // effective interface actually looks like a VPN.
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
    });

    graph_area.set_draw_func({
        let state = state.clone();
        move |_area, cr, width, height| draw_graph(&state, cr, width, height)
    });

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

/// Builds the settings panel: which interface drives the display, and the
/// rename field once one is pinned - identical to `network_sx::build_settings`.
fn build_settings(state: Rc<NetworkSqState>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(240, -1);

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

    i18n::on_change({
        let interface_label_widget = interface_label_widget.clone();
        let custom_label_label = custom_label_label.clone();
        let refresh_interface_model = refresh_interface_model.clone();
        move || {
            interface_label_widget.set_label(&i18n::t("widgets.network.settings.interface"));
            custom_label_label.set_label(&i18n::t("widgets.network.settings.custom_label"));
            refresh_interface_model();
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
