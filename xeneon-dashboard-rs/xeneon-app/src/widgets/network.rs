// SPDX-License-Identifier: GPL-3.0-or-later
//! Network throughput widget (SSX footprint): a one-line "↓ wlan0 12.4M"
//! (or "↑ ...") readout of one network interface's current in or out rate,
//! ported to this project's own design after a quick visual mockup was
//! agreed with the user. Same overall shape as `cpu_temp.rs`'s SSX widget
//! (caption + value, `Auto`/pinned choice in settings, a timer-driven
//! `refresh()`) - see that module's doc comment for the general pattern
//! this one reuses.
//!
//! Unlike hwmon temperatures, there is no single kernel file that already
//! reports "the current rate" - `/proc/net/dev` only exposes cumulative
//! byte counters since the interface came up, so this widget has to keep
//! its own previous sample and divide the delta by the elapsed wall-clock
//! time on every tick (see `NetworkState::refresh`). Still no crate needed
//! for any of this - `/proc/net/dev` (byte counters) and `/proc/net/route`
//! (which interface currently owns the default route, used to auto-pick
//! "the" network interface) are both plain kernel text files, matching
//! this project's minimal-dependencies preference the same way
//! `cpu_temp.rs` reads straight from `/sys/class/hwmon`.
//!
//! Two things are user-configurable in settings, per the user's own
//! request: which interface to read (`Auto`, tracking whatever currently
//! holds the default route, or one pinned by name), and which direction to
//! show (`in`/received or `out`/sent) - since this SSX card is too narrow
//! to show both numbers at once, showing only one direction (instead of
//! e.g. alternating) lets the user place two SSX instances side by side,
//! one pinned to each direction, which is the whole reason the setting
//! exists rather than being a fixed "always show download" choice.
//!
//! `read_interfaces`/`default_interface`/`format_rate_compact` are `pub`
//! and free of any SSX-specific UI assumption, the same way `cpu_temp.rs`
//! keeps `all_sensors`/`auto_pick_sensor` reusable - this widget is
//! expected to grow SX/S/SQ/M size variants next (see the agreed mockup),
//! and those will read through these same three functions rather than
//! re-parsing `/proc/net/dev` themselves.

use gtk::prelude::*;
use log::{debug, warn};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Once;
use std::time::Instant;

use crate::i18n_runtime as i18n;
use crate::widgets::registry::WidgetInstance;

/// Where the kernel exposes per-interface byte/packet counters on Linux -
/// a fixed kernel ABI path, not something that varies by distro.
const NET_DEV_PATH: &str = "/proc/net/dev";
/// Where the kernel exposes the routing table, used only to find which
/// interface currently owns the default route (destination `00000000`) -
/// see `default_interface`.
const NET_ROUTE_PATH: &str = "/proc/net/route";

/// Never offered as a real interface, in the auto-pick fallback, or in the
/// settings dropdown - the loopback device never carries the traffic this
/// widget is meant to show.
const LOOPBACK_IFACE: &str = "lo";

/// How often to re-read the counters, recompute the rate and refresh the
/// display. Matches `cpu_temp.rs`'s `REFRESH_INTERVAL_SECONDS` - frequent
/// enough to feel live, infrequent enough that the rate (a delta over this
/// interval) isn't too noisy tick to tick.
const REFRESH_INTERVAL_SECONDS: u32 = 2;

/// Same font size as `cpu_temp.rs`'s `FONT_PX` so this SSX card reads at
/// the same visual weight as its neighbours - installed once, display-wide,
/// like every other SSX-footprint widget's CSS.
const FONT_PX: i32 = 22;

static INSTALL_CSS: Once = Once::new();

fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!(
            ".xeneon-network-caption {{ font-size: {FONT_PX}px; color: rgba(255, 255, 255, 0.75); }}\n\
             .xeneon-network-value {{ font-size: {FONT_PX}px; font-weight: 700; color: #ffffff; \
             font-family: monospace; }}"
        ));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

/// One interface's cumulative counters, straight from `/proc/net/dev`:
/// `(name, bytes received, bytes sent)`. Cumulative since the interface
/// came up, not a rate - `NetworkState::refresh` turns two samples of this
/// into a rate by dividing the delta by the elapsed time.
pub type InterfaceCounters = (String, u64, u64);

/// Every interface currently listed in `/proc/net/dev`, excluding
/// loopback - empty (rather than panicking/erroring) if the file can't be
/// read at all, mirroring `cpu_temp.rs::all_sensors`'s defensive shape for
/// a kernel interface that's theoretically always there but shouldn't take
/// the widget down if it somehow isn't.
///
/// `/proc/net/dev`'s format is two header lines followed by one line per
/// interface: `<name>: <8 receive fields> <8 transmit fields>`, e.g.
/// `  wlan0: 123456   78 0 0 0 0 0 0   9012 34 0 0 0 0 0 0`. The name and
/// the first receive/transmit fields (bytes) are all this widget needs -
/// packets/errors/drops/etc. are ignored.
pub fn read_interfaces() -> Vec<InterfaceCounters> {
    static NET_DEV_MISSING_WARNED: Once = Once::new();

    let Ok(contents) = std::fs::read_to_string(NET_DEV_PATH) else {
        NET_DEV_MISSING_WARNED
            .call_once(|| warn!("{NET_DEV_PATH} not readable - no network widgets will show live data"));
        return Vec::new();
    };

    let mut interfaces = Vec::new();
    // Skip the two fixed header lines ("Inter-|   Receive..." and
    // " face |bytes    packets...") - the per-interface lines are
    // everything after that, one per line.
    for line in contents.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else { continue };
        let name = name.trim();
        if name.is_empty() || name == LOOPBACK_IFACE {
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // 8 receive fields then 8 transmit fields - bytes is the first of
        // each group (index 0 and index 8).
        let (Some(rx_str), Some(tx_str)) = (fields.first(), fields.get(8)) else { continue };
        let (Ok(rx_bytes), Ok(tx_bytes)) = (rx_str.parse::<u64>(), tx_str.parse::<u64>()) else { continue };
        interfaces.push((name.to_string(), rx_bytes, tx_bytes));
    }
    interfaces
}

/// The interface currently holding the default route (destination
/// `00000000` in `/proc/net/route`, i.e. `0.0.0.0/0`), if any - used as
/// this widget's `Auto` pick, since "the interface actually carrying
/// traffic to the internet" is a much more useful default here than an
/// arbitrary priority list of names (unlike `cpu_temp.rs`'s
/// `CHIP_PRIORITY`, interface names vary too wildly across machines -
/// `wlan0`, `enp3s0`, `wlp2s0`... - for a name-based guess to be reliable).
/// Picks the first match when several default routes exist (a VPN and the
/// underlying physical interface can both have one); which one "wins" in
/// that case isn't meaningful enough to sort by metric for.
pub fn default_interface() -> Option<String> {
    let contents = std::fs::read_to_string(NET_ROUTE_PATH).ok()?;
    for line in contents.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (Some(iface), Some(destination)) = (fields.first(), fields.get(1)) else { continue };
        if *destination == "00000000" {
            return Some((*iface).to_string());
        }
    }
    None
}

/// True if `name` is a wireless (Wi-Fi) interface - detected via the
/// kernel's own marker for this, `/sys/class/net/<name>/wireless` (the
/// older per-interface wireless-extensions directory) or the
/// `/sys/class/net/<name>/phy80211` symlink (present for anything driven
/// by the modern cfg80211/nl80211 stack, which covers virtually every
/// Wi-Fi adapter in use today) - either existing is enough. A plain
/// file-existence check, matching this module's `/proc`/`/sys`-only, no-
/// new-crate approach; `false` for anything not found under
/// `/sys/class/net` at all; used to pick between the Wi-Fi/Ethernet icon
/// in the SQ/M cards.
pub fn is_wireless(name: &str) -> bool {
    let base = std::path::Path::new("/sys/class/net").join(name);
    base.join("wireless").exists() || base.join("phy80211").exists()
}

/// True if `name` looks like a VPN tunnel interface, by name prefix -
/// WireGuard (`wg`), OpenVPN/generic TUN devices (`tun`), TAP-mode VPNs
/// (`tap`), and legacy PPP-based VPNs like OpenConnect/PPTP (`ppp`). A
/// heuristic, not a guarantee: nothing in `/proc`/`/sys` labels an
/// interface "this is a VPN", so this is inferred the same way a human
/// would glance at `ip addr` and recognize the name - it covers every
/// common Linux VPN client without needing to shell out to or link
/// against any of them, at the cost of a false negative for a VPN
/// deliberately renamed to something else (rare) or a false positive for
/// a non-VPN tunnel that happens to share one of these prefixes (rarer
/// still, on a typical desktop). Used to show the SQ/M cards' VPN badge
/// for whichever interface the card is currently displaying - see
/// `network_sq.rs`'s module doc comment for why that's tied to the
/// displayed interface rather than "is any VPN active on this machine".
pub fn is_vpn_like(name: &str) -> bool {
    const VPN_PREFIXES: [&str; 4] = ["wg", "tun", "tap", "ppp"];
    VPN_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// True if *any* currently-listed interface looks like a VPN tunnel -
/// system-wide, not tied to whichever interface a card happens to be
/// displaying. `interfaces` is passed in (the same `read_interfaces()`
/// snapshot a caller's `refresh()` already read) rather than re-read here,
/// so checking this doesn't cost a second `/proc/net/dev` read.
///
/// This is what the SQ/M cards' VPN badge actually checks now - it used
/// to test only the card's own effective interface, which looked right in
/// `Auto` mode (a connected VPN usually takes over the default route,
/// which `Auto` follows) until a real split-tunnel VPN proved that
/// assumption wrong: `tun0` was up and passing traffic, but the default
/// route stayed on the physical interface, so `Auto` kept displaying that
/// one and the badge never lit up even though a VPN plainly was active.
/// NetworkManager's own D-Bus API (`ActiveConnection.Vpn`) was considered
/// as a more "authoritative" alternative and rejected: it only knows
/// about connections NetworkManager itself manages, so a VPN started by a
/// standalone `openvpn`/`wg-quick` invocation or a plain systemd unit -
/// exactly how this was tested - is invisible to it too. The name
/// heuristic has a real blind spot of its own (see `is_vpn_like`'s own
/// doc comment) but it's the one that actually saw this case.
pub fn vpn_active(interfaces: &[InterfaceCounters]) -> bool {
    interfaces.iter().any(|(name, _, _)| is_vpn_like(name))
}

/// Human-readable rate, e.g. `12.4M`, `512K`, `48B` - binary units (1024,
/// not 1000) to match what `free`/`df`/GNOME System Monitor already show,
/// one decimal place once past bytes/sec since a bare integer megabyte
/// count is too coarse to see the rate actually moving tick to tick. No
/// `/s` suffix (kept for the SX/S/SQ/M variants, where there's room) - this
/// SSX card is narrow enough that the direction arrow already implies "per
/// second, right now".
pub fn format_rate_compact(bytes_per_second: f64) -> String {
    let value = bytes_per_second.max(0.0);
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    if value < KIB {
        format!("{value:.0}B")
    } else if value < MIB {
        format!("{:.1}K", value / KIB)
    } else if value < GIB {
        format!("{:.1}M", value / MIB)
    } else {
        format!("{:.1}G", value / GIB)
    }
}

/// `format_rate_compact`, right-padded with leading spaces to a fixed
/// 6-character width (enough for up to `999.9M`, comfortably past what a
/// home network pushes) - meant for a value sitting inside an otherwise
/// static, centered line (the SSX card's value, or one of SX's two
/// side-by-side rates). Without this, "6.1K" becoming "12.4M" a couple of
/// seconds later changes the line's total width, and since the line is
/// centered as a whole, *everything* on it visibly shifts even though only
/// the number itself actually changed - a real complaint from watching
/// this widget update live, not a hypothetical. Padding alone isn't
/// enough on a proportional font (digit widths can still differ from a
/// plain space's), so callers must also render this in a monospace font -
/// see `network.rs`'s `.xeneon-network-value` CSS rule and `network_sx.rs`'s
/// `font_family="monospace"` markup spans.
pub fn format_rate_fixed(bytes_per_second: f64) -> String {
    format!("{:>6}", format_rate_compact(bytes_per_second))
}

/// Which byte counter to track - received (`In`, the download direction)
/// or sent (`Out`, upload). A plain setting rather than e.g. an
/// alternating display so two SSX instances can be placed side by side,
/// one per direction (see this module's doc comment).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    In,
    Out,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Direction::In => "in",
            Direction::Out => "out",
        }
    }

    /// Unknown/missing saved value falls back to `In` (download) - the
    /// more commonly-watched direction, and the safer default for a
    /// widget added to an existing saved page by an app update.
    fn from_str(value: &str) -> Direction {
        if value == "out" {
            Direction::Out
        } else {
            Direction::In
        }
    }

    /// A plain Unicode arrow, prefixed onto the caption text, instead of a
    /// `gtk::Image::from_icon_name` (what this widget used at first) -
    /// switched after the freedesktop `network-receive-symbolic`/
    /// `network-transmit-symbolic` icons turned out not to render at all
    /// under the user's own icon theme (a third-party theme whose
    /// `-symbolic` SVGs GTK4's icon recoloring didn't like - a known class
    /// of real-world breakage with non-Adwaita symbolic icon sets). A
    /// character glyph, drawn by the same Label/Pango path already proven
    /// to work for the rest of this card's text, has no such dependency.
    fn arrow(self) -> &'static str {
        match self {
            Direction::In => "↓",
            Direction::Out => "↑",
        }
    }

    /// Colors the arrow alone (via a Pango markup `<span>` around just that
    /// character - see `NetworkState::refresh`), not the whole caption, so
    /// the direction reads at a glance without competing with the
    /// interface name/custom label's own neutral color. Same blue/coral
    /// pairing as the mockup shown to the user before this widget was
    /// built (download = blue, upload = coral) - arbitrary but consistent
    /// with the "cool = incoming, warm = outgoing" convention most
    /// bandwidth monitors already use.
    fn color_hex(self) -> &'static str {
        match self {
            Direction::In => "#5da9e8",
            Direction::Out => "#e8875d",
        }
    }
}

/// All of this widget's live state - one instance per placed widget.
/// Mirrors `CpuTempState` in shape: a caption/value pair for content, an
/// `Auto`-or-pinned choice (here, network interface instead of hwmon
/// sensor), and a snapshot of what's currently available for the settings
/// dropdown to list.
struct NetworkState {
    caption_label: gtk::Label,
    value_label: gtk::Label,

    /// `None` means "auto-pick the default-route interface" (see
    /// `effective_interface_name`) - set independently from `direction`,
    /// since either can change without the other.
    interface_name: RefCell<Option<String>>,
    /// Free-text override for the caption, only ever shown/edited while an
    /// interface is pinned manually (see `display_label`) - mirrors
    /// `CpuTempState::custom_label`: in `Auto` mode the caption already
    /// shows the real, meaningful interface name, so there's nothing to
    /// override there; once pinned, the user can still rename it to
    /// whatever they'd recognize faster ("Salon", "Box"...).
    custom_label: RefCell<Option<String>>,
    direction: Cell<Direction>,
    /// Snapshot of every non-loopback interface seen on the most recent
    /// `refresh()`, for the settings dropdown to list - re-read here
    /// rather than re-scanned by the settings panel itself, same reasoning
    /// as `CpuTempState::available_sensors`.
    available_interfaces: RefCell<Vec<String>>,
    /// The previous sample this rate was computed from -
    /// `(interface name, direction, bytes, when)`. Name and direction are
    /// kept alongside the byte count so a pin/direction change (which
    /// jumps to an unrelated counter) is detected and treated as "no rate
    /// yet" for one tick, instead of showing a bogus huge spike computed
    /// against the previous interface's/direction's counter.
    last_sample: RefCell<Option<(String, Direction, u64, Instant)>>,
    /// Whether the last `refresh()` already logged "no interface" - same
    /// once-per-disappearance logging as `CpuTempState::logged_unavailable`.
    logged_unavailable: Cell<bool>,
}

impl NetworkState {
    /// The interface name actually driving the display right now: the
    /// user's pin if set, otherwise whichever interface currently holds
    /// the default route, otherwise (no default route - e.g. offline) the
    /// first interface seen at all. `interfaces` is passed in rather than
    /// re-read so a single `refresh()` call only ever reads
    /// `/proc/net/dev` once.
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
        self.refresh();
    }

    fn set_custom_label(&self, text: Option<String>) {
        *self.custom_label.borrow_mut() = text.filter(|s| !s.is_empty());
        self.refresh();
    }

    fn set_direction(&self, direction: Direction) {
        self.direction.set(direction);
        self.refresh();
    }

    /// The caption's name portion for `effective_name` (the interface name
    /// freshly computed by `refresh()` for this tick - passed in rather
    /// than recomputed here since, unlike `CpuTempState::display_label`,
    /// it depends on a live `/proc/net/route` read, not just already-
    /// stored state). Just the name - the direction arrow is added
    /// separately by `refresh()`, as its own colored markup span (see
    /// `Direction::color_hex`), not part of this plain string. While
    /// pinned, a non-empty `custom_label` wins over the interface name;
    /// otherwise (auto-picking, or pinned with no custom label set) the
    /// real interface name is shown - this is also the answer to "does
    /// Auto track whatever's actually in use": yes, and now it's visible
    /// on the card instead of only in the tooltip. Falls back to this
    /// widget's own generic title only when no interface name is known at
    /// all (nothing plugged in / no route yet).
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

    /// Re-reads every interface's counters, recomputes the rate for
    /// whichever interface/direction is currently effective, and redraws
    /// the caption + value. Called on every tick (see `build_content`'s
    /// timer), on every setter above, and on a language change (for the
    /// tooltip's translated text) - same call sites as `CpuTempState::
    /// refresh`.
    fn refresh(&self) {
        let interfaces = read_interfaces();
        *self.available_interfaces.borrow_mut() = interfaces.iter().map(|(name, _, _)| name.clone()).collect();

        let effective_name = self.effective_interface_name(&interfaces);
        let direction = self.direction.get();
        // Markup, not plain text: the arrow gets its own color (see
        // `Direction::color_hex`) while the name stays the caption's
        // ordinary muted color. The name is escaped since, unlike the
        // interface names `/proc/net/dev` hands back, a user-typed custom
        // label could contain `&`/`<`/`>` and would otherwise be parsed as
        // (broken) markup instead of displayed literally.
        let name = self.display_label(effective_name.as_deref());
        self.caption_label.set_markup(&format!(
            "<span color=\"{}\" weight=\"bold\">{}</span> {}",
            direction.color_hex(),
            direction.arrow(),
            gtk::glib::markup_escape_text(&name)
        ));

        let counters = effective_name.as_ref().and_then(|name| interfaces.iter().find(|(n, _, _)| n == name));

        match counters {
            None => {
                // Logged once per disappearance, not every tick - mirrors
                // `CpuTempState::refresh`'s own reasoning: this is the
                // "no network interfaces at all" case (or a pinned one
                // that's since vanished), worth a trace when it first
                // happens but not on every 2s poll after that.
                if !self.logged_unavailable.replace(true) {
                    warn!(
                        "no reading for interface {} ({} interfaces seen)",
                        effective_name.as_deref().unwrap_or("<none>"),
                        self.available_interfaces.borrow().len()
                    );
                }
                *self.last_sample.borrow_mut() = None;
                self.value_label.set_text("--");
                self.value_label.set_tooltip_text(Some(&i18n::t("widgets.network.unavailable")));
            }
            Some((name, rx_bytes, tx_bytes)) => {
                if self.logged_unavailable.replace(false) {
                    debug!("interface reading recovered: {name}");
                }
                let current_bytes = if direction == Direction::In { *rx_bytes } else { *tx_bytes };
                let now = Instant::now();

                let mut last_sample = self.last_sample.borrow_mut();
                // Only trust the delta when it's against the *same*
                // interface and direction as last tick, and the counter
                // moved forward (a reset/replug can make it jump back to
                // 0) - anything else shows "…" for this one tick rather
                // than a nonsense spike, then resolves itself next tick.
                let rate = match last_sample.as_ref() {
                    Some((prev_name, prev_direction, prev_bytes, prev_when))
                        if prev_name == name && *prev_direction == direction && current_bytes >= *prev_bytes =>
                    {
                        let elapsed = now.duration_since(*prev_when).as_secs_f64();
                        (elapsed > 0.0).then(|| (current_bytes - *prev_bytes) as f64 / elapsed)
                    }
                    _ => None,
                };
                *last_sample = Some((name.clone(), direction, current_bytes, now));
                drop(last_sample);

                // "…" (one tick after a pin/direction change or a counter
                // reset, before there's a second sample to diff) is also
                // padded to the same width - otherwise that one tick would
                // itself cause the shift this padding exists to prevent.
                self.value_label.set_text(&rate.map(format_rate_fixed).unwrap_or_else(|| format!("{:>6}", "…")));
                self.value_label.set_tooltip_text(Some(name));
            }
        }
    }

    fn to_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "interface": self.interface_name.borrow().clone(),
            "custom_label": self.custom_label.borrow().clone(),
            "direction": self.direction.get().as_str(),
        })
    }

    /// Only touches fields actually present, so a partial/older saved dict
    /// still applies cleanly - mirrors `CpuTempState::apply_dict`.
    fn apply_dict(&self, data: &serde_json::Value) {
        if data.get("interface").is_some() {
            let name = data.get("interface").and_then(|v| v.as_str()).map(str::to_string);
            self.set_interface(name);
        }
        if data.get("custom_label").is_some() {
            let label = data.get("custom_label").and_then(|v| v.as_str()).map(str::to_string);
            self.set_custom_label(label);
        }
        if let Some(value) = data.get("direction").and_then(|v| v.as_str()) {
            self.set_direction(Direction::from_str(value));
        }
    }
}

/// True if `entries[index]` is a real pinned interface (`Some`) rather
/// than the "Auto" placeholder (`None` at index 0) - used to gate the
/// custom-label entry's sensitivity, same role as `cpu_temp.rs`'s own
/// `is_manual`.
fn is_manual(entries: &[Option<String>], index: usize) -> bool {
    entries.get(index).map(|entry| entry.is_some()).unwrap_or(false)
}

fn make_row(widgets: &[&gtk::Widget]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for w in widgets {
        row.append(*w);
    }
    row
}

/// Builds the widget's whole on-card display: the direction arrow +
/// interface name (or custom label) as one caption, and the rate next to
/// it, centered - the same caption+value shape as `cpu_temp.rs`'s
/// `build_content`. The direction is a character glyph baked into the
/// caption text (see `Direction::arrow`), not a separate icon widget -
/// see that method's doc comment for why a real `gtk::Image` icon was
/// dropped.
fn build_content() -> (Rc<NetworkState>, gtk::Widget) {
    ensure_css_installed();

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    root.set_halign(gtk::Align::Center);
    root.set_valign(gtk::Align::Center);

    let caption_label = gtk::Label::new(None);
    caption_label.add_css_class("xeneon-network-caption");
    caption_label.set_halign(gtk::Align::Center);
    caption_label.set_valign(gtk::Align::Center);
    // Unlike `cpu_temp.rs`'s fixed-length "CPU" caption, this one is a
    // real interface name (or a user-typed custom label) - either can be
    // long enough (`enp0s31f6`, or a custom label the user didn't think to
    // keep short) to overflow this SSX card's ~196px width. Without a cap,
    // an overflowing label pushes the whole row wider than the card, and
    // `root`'s centering then centers that oversized, partly-clipped row
    // instead of the visible content - reading as "off-center" even though
    // the box math is correct. Capping the caption's width (the arrow +
    // space take up 2 of these characters) keeps the row's natural size
    // within the card, so centering always looks right.
    caption_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    caption_label.set_max_width_chars(8);
    root.append(&caption_label);

    let value_label = gtk::Label::new(None);
    value_label.add_css_class("xeneon-network-value");
    value_label.set_halign(gtk::Align::Center);
    value_label.set_valign(gtk::Align::Center);
    root.append(&value_label);

    let state = Rc::new(NetworkState {
        caption_label,
        value_label,
        interface_name: RefCell::new(None),
        custom_label: RefCell::new(None),
        direction: Cell::new(Direction::In),
        available_interfaces: RefCell::new(Vec::new()),
        last_sample: RefCell::new(None),
        logged_unavailable: Cell::new(false),
    });
    // First read happens synchronously, same reasoning as
    // `CpuTempState::build_content`: by the time `build_settings` runs
    // right after this returns, `available_interfaces` already has real
    // data instead of starting empty.
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

/// Builds the settings panel: which interface drives the display (`Auto`,
/// or one pinned by name) and which direction to show (`In`/`Out`) - same
/// two-section shape as `cpu_temp.rs`'s `build_settings` (sensor picker +
/// unit toggle), with "interface" and "direction" standing in for "sensor"
/// and "unit".
fn build_settings(state: Rc<NetworkState>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(240, -1);

    let interface_label_widget = gtk::Label::new(Some(&i18n::t("widgets.network.settings.interface")));
    interface_label_widget.set_halign(gtk::Align::Start);
    root.append(&interface_label_widget);

    // `None` at index 0 always stands for "Auto" (see `NetworkState::
    // effective_interface_name`); the rest come from whatever's currently
    // up, plus the pinned interface if for some reason it's not already in
    // that list (e.g. a saved config carried over from a different
    // machine, or an interface that's since disappeared).
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

    // Kept insensitive rather than hidden while on Auto, so its position
    // in the popover doesn't jump around when switching back and forth -
    // same reasoning and layout as `cpu_temp.rs::build_settings`'s
    // `custom_label_label`/`custom_label_entry`.
    // Set directly (not left to the `i18n::on_change` listener below,
    // which only fires on a *later* language change) so the label reads
    // correctly on first open, not just after switching languages once.
    let custom_label_label = gtk::Label::new(Some(&i18n::t("widgets.network.settings.custom_label")));
    custom_label_label.set_halign(gtk::Align::Start);
    root.append(&custom_label_label);
    let custom_label_entry = gtk::Entry::new();
    custom_label_entry.set_text(state.custom_label.borrow().as_deref().unwrap_or(""));
    root.append(&custom_label_entry);

    // Rebuilds the dropdown's translated option names + selection, and the
    // custom-label entry's sensitivity, from current state - called once
    // now and again on every language change (see the `i18n::on_change`
    // registration near the end of this function), same pattern as
    // `cpu_temp.rs::build_settings`'s `refresh_sensor_model`.
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

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let direction_label = gtk::Label::new(Some(&i18n::t("widgets.network.settings.direction")));
    direction_label.set_hexpand(true);
    direction_label.set_halign(gtk::Align::Start);
    let in_button = gtk::ToggleButton::with_label(&i18n::t("widgets.network.settings.direction_in"));
    let out_button = gtk::ToggleButton::with_label(&i18n::t("widgets.network.settings.direction_out"));
    out_button.set_group(Some(&in_button));
    in_button.set_active(state.direction.get() == Direction::In);
    out_button.set_active(state.direction.get() == Direction::Out);
    root.append(&make_row(&[direction_label.upcast_ref(), in_button.upcast_ref(), out_button.upcast_ref()]));

    // --- signal wiring: controls push one-way into `state` ---
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
    out_button.connect_toggled({
        let state = state.clone();
        move |b| {
            if b.is_active() {
                state.set_direction(Direction::Out);
            }
        }
    });
    in_button.connect_toggled({
        let state = state.clone();
        move |b| {
            if b.is_active() {
                state.set_direction(Direction::In);
            }
        }
    });

    // --- retranslation ---
    i18n::on_change({
        let interface_label_widget = interface_label_widget.clone();
        let custom_label_label = custom_label_label.clone();
        let direction_label = direction_label.clone();
        let in_button = in_button.clone();
        let out_button = out_button.clone();
        let refresh_interface_model = refresh_interface_model.clone();
        move || {
            interface_label_widget.set_label(&i18n::t("widgets.network.settings.interface"));
            custom_label_label.set_label(&i18n::t("widgets.network.settings.custom_label"));
            direction_label.set_label(&i18n::t("widgets.network.settings.direction"));
            in_button.set_label(&i18n::t("widgets.network.settings.direction_in"));
            out_button.set_label(&i18n::t("widgets.network.settings.direction_out"));
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
