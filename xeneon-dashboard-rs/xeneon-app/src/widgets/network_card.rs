// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared implementation behind the SQ and M network-throughput cards
//! (`network_sq.rs`/`network_m.rs`) - same icon+name header, VPN pill
//! badge, down/up rate line, and scrolling Cairo graph; the two only ever
//! differed in footprint-driven sizing (how many history samples fit, how
//! wide the rate line can grow, whether the header icon has a user-facing
//! size slider at all) and CSS class naming. Parameterized by
//! `NetworkCardVariant` instead of copy-pasted a third time (dedup audit
//! finding 2026-09-29, step 4 of the network-widget-family cleanup -
//! steps 1-3 gave the family a shared icon cache, `make_row`/`is_manual`,
//! and `interface_picker`; this is the last, biggest piece:
//! `network_sq.rs`/`network_m.rs` alone were ~700 duplicated lines).
//!
//! Every per-card knob lives on `NetworkCardVariant` and is read through
//! `V::*`; `network_sq.rs`'s and `network_m.rs`'s own files shrink down to
//! a one-line variant `impl` plus two one-line `spawn`/`restore` wrappers
//! (kept as real wrapper functions, not re-exports, so
//! `registry.rs`'s `crate::widgets::network_sq::spawn` function-pointer
//! references don't need to change).
//!
//! SQ's header icon never resizes - one fixed `content_scale` of `1.0`,
//! no slider in its settings panel at all (audit finding 2026-09-29: the
//! header icon+title are this card's *label*, not its *content*). M keeps
//! a user-facing slider. Both share the same icon-size formula
//! (`BASE_HEADER_ICON_PX * content_scale`); SQ's `content_scale` simply
//! never leaves `1.0` and is never written to/read from its saved JSON
//! (`NetworkCardVariant::SUPPORTS_CONTENT_SCALE` gates both the slider
//! row and the `to_dict`/`apply_dict`/`reset` fields), so this refactor
//! doesn't change SQ's on-disk format at all.
//!
//! Unifying the two also surfaced two places where `network_m.rs` had
//! quietly drifted from a fix `network_sq.rs` already got on 2026-09-29
//! and this module now applies to both: the VPN badge used
//! `set_visible()` rather than `set_opacity()` (reintroducing the exact
//! header-row-height bug that fix addressed - see `refresh`'s own
//! comment below), and the name-label CSS rule was missing
//! `font-weight: 500` (the header-title-consistency fix, matching
//! `hue.rs`'s/`system_sq.rs`'s own header titles). A third drift - an
//! extra manual spacer `gtk::Box` in M's header, redundant with the one
//! `card_header::build_card_header` already appends - was just dead
//! layout code, not a visible bug, and is simply gone now that both
//! cards share the same header-building code.

use gtk::prelude::*;
use log::{debug, warn};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Once;
use std::time::Instant;

use crate::appearance_popover::{hex_to_rgba, rgba_to_hex};
use crate::i18n_runtime as i18n;
use crate::widgets::card_header;
use crate::widgets::icon_cache;
use crate::widgets::interface_picker;
use crate::widgets::make_row;
use crate::widgets::network::{default_interface, format_rate_fixed, is_wireless, read_interfaces, vpn_active, InterfaceCounters};
use crate::widgets::registry::WidgetInstance;

const REFRESH_INTERVAL_SECONDS: u32 = 2;
const VALUES_FONT_PX: i32 = 22;

const DEFAULT_NAME_COLOR_HEX: &str = "#ffffff";
/// Same blue/coral pairing both cards always used (see
/// `network::Direction::color_hex`) - just the *default* now, since both
/// are user-editable (see `NetworkCardState::set_down_color`/`set_up_color`).
const DEFAULT_DOWN_COLOR_HEX: &str = "#5da9e8";
const DEFAULT_UP_COLOR_HEX: &str = "#e8875d";

/// Fixed on both cards - matches `hue.rs`'s `.xeneon-hue-title`/
/// `system_sq.rs`'s `HOSTNAME_FONT_PX` at their own fixed size.
const NAME_FONT_PX: i32 = 17;
/// Icon size at `content_scale == 1.0` - see this module's own doc
/// comment on why SQ's `content_scale` never moves off that.
const BASE_HEADER_ICON_PX: f64 = 18.0;

const WIFI_ICON_PATH: &str = "assets/network-icon.svg";
const ETHERNET_ICON_PATH: &str = "assets/ethernet-icon.svg";
/// Bigger than the header's own Wi-Fi/Ethernet icon (`BASE_HEADER_ICON_PX`)
/// - a pill badge with a label reads as a unit even at this size, where a
/// bare small icon didn't.
const VPN_ICON_PATH: &str = "assets/vpn-icon.svg";
const VPN_BADGE_ICON_PX: i32 = 20;
const VPN_BADGE_FONT_PX: i32 = 15;
const VPN_COLOR_HEX: &str = "#5DCAA5";

/// Per-card-footprint knobs - everything that actually differs between
/// `network_sq.rs` and `network_m.rs`. `ID` drives every CSS class name
/// (`.xeneon-network-{ID}-values`, instance class `xeneon-network{ID}-N`,
/// title class `xeneon-network-{ID}-name`), so the two cards' styling
/// never collides even though they share one implementation.
pub trait NetworkCardVariant: 'static {
    const ID: &'static str;
    /// Bounds the rate line's width - wider on M, which has roughly twice
    /// SQ's horizontal room and doesn't include the interface name in
    /// this line either way (that moved to its own header row on both).
    const MAX_VALUES_WIDTH_CHARS: i32;
    /// How many history samples the graph keeps - more on M, which has
    /// more horizontal room to spread them across.
    const MAX_HISTORY_SAMPLES: usize;
    /// Whether the settings panel shows a header-icon size slider at all
    /// - `false` for SQ (fixed at 1.0, audit finding 2026-09-29), `true`
    /// for M.
    const SUPPORTS_CONTENT_SCALE: bool;
    const DEFAULT_CONTENT_SCALE: f64;
    const MIN_CONTENT_SCALE: f64;
    const MAX_CONTENT_SCALE: f64;
}

/// All of this widget's live state - one instance per placed widget,
/// generic over which card footprint it's running as.
struct NetworkCardState<V: NetworkCardVariant> {
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
    /// `V::MAX_HISTORY_SAMPLES` - only ever pushed to on a tick that
    /// produced a real rate (see `refresh`), so a momentary "no second
    /// sample yet" tick doesn't draw a fake dip to zero on the graph.
    history: RefCell<VecDeque<(f64, f64)>>,
    logged_unavailable: Cell<bool>,

    name_color: RefCell<gtk::gdk::RGBA>,
    down_color: RefCell<gtk::gdk::RGBA>,
    up_color: RefCell<gtk::gdk::RGBA>,
    /// Always present, even for a variant with `SUPPORTS_CONTENT_SCALE ==
    /// false` - it just starts at `V::DEFAULT_CONTENT_SCALE` (1.0 for SQ)
    /// and nothing ever calls `set_content_scale` on it again, since no
    /// slider exists and `apply_dict` skips the field entirely for that
    /// variant.
    content_scale: Cell<f64>,
    /// Whether the currently-effective interface is wireless, as of the
    /// last `refresh()` - cached here (rather than re-derived) so a
    /// settings-only change (content scale, icon color) can re-render the
    /// icon at the right size/color/type without needing a fresh
    /// `/proc`/`/sys` read of its own.
    current_wireless: Cell<bool>,

    _variant: PhantomData<V>,
}

impl<V: NetworkCardVariant> NetworkCardState<V> {
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
    /// graph repaint - happens exactly like a normal edit would. Called
    /// from the appearance popover's reset button via
    /// `WidgetInstance::on_reset`, wired up in `spawn`/`restore` below.
    fn reset(&self) {
        self.set_interface(None);
        self.set_custom_label(None);
        self.set_name_color(hex_to_rgba(DEFAULT_NAME_COLOR_HEX));
        self.set_down_color(hex_to_rgba(DEFAULT_DOWN_COLOR_HEX));
        self.set_up_color(hex_to_rgba(DEFAULT_UP_COLOR_HEX));
        if V::SUPPORTS_CONTENT_SCALE {
            self.set_content_scale(V::DEFAULT_CONTENT_SCALE);
        }
    }

    fn set_content_scale(&self, scale: f64) {
        self.content_scale.set(scale.clamp(V::MIN_CONTENT_SCALE, V::MAX_CONTENT_SCALE));
        self.apply_content_scale();
    }

    /// Rebuilds this instance's name-label CSS rule (fixed font size, but
    /// still a per-instance rule since `color` isn't fixed) and
    /// re-renders the header icon at `content_scale`'s own size/matching
    /// color. Called from every setter that touches any of those three
    /// settings, not just `set_content_scale` itself - for a variant with
    /// no slider, `content_scale` just never changes, so this only ever
    /// re-applies the same fixed size.
    fn apply_content_scale(&self) {
        let scale = self.content_scale.get();
        let name_hex = rgba_to_hex(&self.name_color.borrow());
        // font-weight: 500 to match hue.rs's/system_sq.rs's own header
        // title rule exactly (audit finding 2026-09-29).
        let rule = format!(
            ".{class} .xeneon-network-{id}-name {{ font-size: {NAME_FONT_PX}px; font-weight: 500; color: {name_hex}; }}",
            class = self.css_class,
            id = V::ID,
        );
        thread_local! {
            static NAME_CSS: crate::appearance_css::CssRuleRegistry =
                crate::appearance_css::CssRuleRegistry::new(gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
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
        if let Some(texture) = icon_cache::load_tinted_icon(path, &hex) {
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
    /// change.
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
        // Audit finding 2026-09-29: `set_visible(false)` removes the
        // badge from layout entirely, shrinking the whole header row's
        // height whenever no VPN is active - since the icon/title are
        // valign-centered within that row, a shorter row (no badge)
        // centers them higher than hue.rs's/system_sq.rs's own header,
        // whose badge is always shown and so never shrinks their row.
        // Opacity keeps the same layout space reserved either way; only
        // the badge's own visual presence (and its now-conditional
        // tooltip, which would otherwise still fire over an invisible
        // badge) changes.
        let vpn_on = vpn_active(&interfaces);
        self.vpn_badge.set_opacity(if vpn_on { 1.0 } else { 0.0 });
        self.vpn_badge.set_tooltip_text(vpn_on.then(|| i18n::t("widgets.network.vpn_active")).as_deref());

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
                    while history.len() > V::MAX_HISTORY_SAMPLES {
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
        let mut value = serde_json::json!({
            "interface": self.interface_name.borrow().clone(),
            "custom_label": self.custom_label.borrow().clone(),
            "name_color": rgba_to_hex(&self.name_color.borrow()),
            "down_color": rgba_to_hex(&self.down_color.borrow()),
            "up_color": rgba_to_hex(&self.up_color.borrow()),
        });
        if V::SUPPORTS_CONTENT_SCALE {
            value["content_scale"] = serde_json::json!(self.content_scale.get());
        }
        value
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
        // For a variant with no content-scale slider (SQ), this key may
        // still be present in a JSON file saved before that slider was
        // removed (2026-09-29) - ignored here rather than causing an
        // error, same as any other unrecognized key.
        if V::SUPPORTS_CONTENT_SCALE {
            if let Some(v) = data.get("content_scale").and_then(|v| v.as_f64()) {
                self.set_content_scale(v);
            }
        }
    }
}

impl<V: NetworkCardVariant> interface_picker::InterfacePickable for NetworkCardState<V> {
    fn interface_name(&self) -> Option<String> {
        self.interface_name.borrow().clone()
    }
    fn available_interfaces(&self) -> Vec<String> {
        self.available_interfaces.borrow().clone()
    }
    fn custom_label(&self) -> Option<String> {
        self.custom_label.borrow().clone()
    }
    fn set_interface(&self, name: Option<String>) {
        self.set_interface(name)
    }
    fn set_custom_label(&self, label: Option<String>) {
        self.set_custom_label(label)
    }
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
fn draw_graph<V: NetworkCardVariant>(state: &NetworkCardState<V>, cr: &gtk::cairo::Context, width: i32, height: i32) {
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

fn ensure_css_installed<V: NetworkCardVariant>() {
    static INSTALL_CSS: Once = Once::new();
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let id = V::ID;
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!(
            ".xeneon-network-{id}-values {{ font-size: {VALUES_FONT_PX}px; color: rgba(255, 255, 255, 0.75); }}\n\
             .xeneon-network-{id}-vpn-badge {{ background-color: rgba(93, 202, 165, 0.15); \
             border-radius: 13px; padding: 5px 12px; }}\n\
             .xeneon-network-{id}-vpn-badge-label {{ font-size: {VPN_BADGE_FONT_PX}px; font-weight: 700; \
             color: {VPN_COLOR_HEX}; }}"
        ));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

fn build_content<V: NetworkCardVariant>() -> (Rc<NetworkCardState<V>>, gtk::Widget) {
    ensure_css_installed::<V>();

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let css_class = format!("xeneon-network{id}-{n}", id = V::ID, n = NEXT_ID.fetch_add(1, Ordering::Relaxed));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.add_css_class(&css_class);
    root.set_margin_start(14);
    root.set_margin_end(14);
    root.set_margin_top(12);
    root.set_margin_bottom(10);

    // Paintable set in the first `refresh()` call below, once the
    // effective interface (and therefore Wi-Fi vs Ethernet) is known -
    // starts empty rather than defaulting to one or the other. Size is
    // set in `apply_content_scale`, called right after construction.
    let title_css_class = format!("xeneon-network-{}-name", V::ID);
    let (header, icon_image, name_label) = card_header::build_card_header(6, &title_css_class);

    // A pill (icon + "VPN" label), not a bare icon - a small icon on its
    // own didn't read clearly at this size; the label makes it
    // unambiguous at a glance, matching the mockup shown to the user.
    let vpn_badge = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    vpn_badge.add_css_class(&format!("xeneon-network-{}-vpn-badge", V::ID));
    vpn_badge.set_valign(gtk::Align::Center);
    let vpn_badge_icon = gtk::Image::new();
    if let Some(texture) = icon_cache::load_icon_texture(VPN_ICON_PATH) {
        vpn_badge_icon.set_paintable(Some(&texture));
    }
    vpn_badge_icon.set_pixel_size(VPN_BADGE_ICON_PX);
    vpn_badge.append(&vpn_badge_icon);
    let vpn_badge_label = gtk::Label::new(Some(&i18n::t("widgets.network.vpn_badge_label")));
    vpn_badge_label.add_css_class(&format!("xeneon-network-{}-vpn-badge-label", V::ID));
    vpn_badge.append(&vpn_badge_label);
    // Transparent (not hidden) until the first `refresh()` call below
    // decides whether any interface actually looks like a VPN - stays
    // visible for layout purposes so it keeps reserving its own space
    // in the header row even while invisible (see `refresh`'s own
    // comment on why `set_visible(false)` doesn't work for this). Its
    // tooltip is set/cleared there too, not here.
    vpn_badge.set_opacity(0.0);
    header.append(&vpn_badge);

    root.append(&header);

    let values_label = gtk::Label::new(None);
    values_label.add_css_class(&format!("xeneon-network-{}-values", V::ID));
    values_label.set_halign(gtk::Align::Start);
    values_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    values_label.set_max_width_chars(V::MAX_VALUES_WIDTH_CHARS);
    root.append(&values_label);

    let graph_area = gtk::DrawingArea::new();
    graph_area.set_hexpand(true);
    graph_area.set_vexpand(true);
    graph_area.set_margin_top(6);
    root.append(&graph_area);

    let state = Rc::new(NetworkCardState::<V> {
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
        history: RefCell::new(VecDeque::with_capacity(V::MAX_HISTORY_SAMPLES)),
        logged_unavailable: Cell::new(false),
        name_color: RefCell::new(hex_to_rgba(DEFAULT_NAME_COLOR_HEX)),
        down_color: RefCell::new(hex_to_rgba(DEFAULT_DOWN_COLOR_HEX)),
        up_color: RefCell::new(hex_to_rgba(DEFAULT_UP_COLOR_HEX)),
        content_scale: Cell::new(V::DEFAULT_CONTENT_SCALE),
        current_wireless: Cell::new(true),
        _variant: PhantomData,
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

/// Builds the settings panel: which interface drives the display, the
/// rename field once one is pinned (shared with the rest of the
/// network-widget family - see `interface_picker`'s own doc comment),
/// then the appearance settings - icon+name color, icon+name size (only
/// when `V::SUPPORTS_CONTENT_SCALE`), download color, upload color.
fn build_settings<V: NetworkCardVariant>(state: Rc<NetworkCardState<V>>) -> (gtk::Widget, Box<dyn Fn()>) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(260, -1);

    let picker = interface_picker::build(state.clone());
    root.append(&picker.interface_label);
    root.append(&picker.interface_dropdown);
    root.append(&picker.custom_label_label);
    root.append(&picker.custom_label_entry);

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let name_color_label = gtk::Label::new(Some(&i18n::t("widgets.network.settings.name_color")));
    name_color_label.set_hexpand(true);
    name_color_label.set_halign(gtk::Align::Start);
    let name_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    name_color_button.set_rgba(&state.name_color.borrow());
    root.append(&make_row(&[name_color_label.upcast_ref(), name_color_button.upcast_ref()]));

    let scale_row = if V::SUPPORTS_CONTENT_SCALE {
        let scale_label = gtk::Label::new(Some(&i18n::t("widgets.network.settings.content_scale")));
        scale_label.set_halign(gtk::Align::Start);
        root.append(&scale_label);
        let scale_slider = gtk::Scale::with_range(
            gtk::Orientation::Horizontal,
            V::MIN_CONTENT_SCALE * 100.0,
            V::MAX_CONTENT_SCALE * 100.0,
            1.0,
        );
        scale_slider.set_value(state.content_scale.get() * 100.0);
        scale_slider.set_draw_value(true);
        scale_slider.set_value_pos(gtk::PositionType::Right);
        root.append(&scale_slider);
        Some((scale_label, scale_slider))
    } else {
        None
    };

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
    if let Some((_, scale_slider)) = &scale_row {
        scale_slider.connect_value_changed({
            let state = state.clone();
            move |s| state.set_content_scale(s.value() / 100.0)
        });
    }
    down_color_button.connect_rgba_notify({
        let state = state.clone();
        move |b| state.set_down_color(b.rgba())
    });
    up_color_button.connect_rgba_notify({
        let state = state.clone();
        move |b| state.set_up_color(b.rgba())
    });

    i18n::on_change({
        let interface_label = picker.interface_label.clone();
        let custom_label_label = picker.custom_label_label.clone();
        let name_color_label = name_color_label.clone();
        let scale_label = scale_row.as_ref().map(|(label, _)| label.clone());
        let down_color_label = down_color_label.clone();
        let up_color_label = up_color_label.clone();
        let refresh = picker.refresh.clone();
        move || {
            interface_label.set_label(&i18n::t("widgets.network.settings.interface"));
            custom_label_label.set_label(&i18n::t("widgets.network.settings.custom_label"));
            name_color_label.set_label(&i18n::t("widgets.network.settings.name_color"));
            if let Some(label) = &scale_label {
                label.set_label(&i18n::t("widgets.network.settings.content_scale"));
            }
            down_color_label.set_label(&i18n::t("widgets.network.settings.down_color"));
            up_color_label.set_label(&i18n::t("widgets.network.settings.up_color"));
            refresh();
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
        let custom_label_entry = picker.custom_label_entry.clone();
        let name_color_button = name_color_button.clone();
        let scale_slider = scale_row.as_ref().map(|(_, slider)| slider.clone());
        let down_color_button = down_color_button.clone();
        let up_color_button = up_color_button.clone();
        let refresh = picker.refresh.clone();
        move || {
            let custom_label = state.custom_label.borrow().clone();
            let name_color = *state.name_color.borrow();
            let down_color = *state.down_color.borrow();
            let up_color = *state.up_color.borrow();

            custom_label_entry.set_text(custom_label.as_deref().unwrap_or(""));
            name_color_button.set_rgba(&name_color);
            if let Some(slider) = &scale_slider {
                slider.set_value(state.content_scale.get() * 100.0);
            }
            down_color_button.set_rgba(&down_color);
            up_color_button.set_rgba(&up_color);
            // Also resyncs the interface dropdown's selection and the
            // custom-label controls' sensitivity from `state.interface_name`.
            refresh();
        }
    });

    (root.upcast(), resync)
}

pub fn spawn<V: NetworkCardVariant>() -> WidgetInstance {
    let (state, content) = build_content::<V>();
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

pub fn restore<V: NetworkCardVariant>(data: &serde_json::Value) -> WidgetInstance {
    let (state, content) = build_content::<V>();
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
