// SPDX-License-Identifier: GPL-3.0-or-later
//! Network throughput widget (SX footprint): "wlan0  ↓12.4M  ↑1.2M" - both
//! directions on one line, unlike `network.rs`'s SSX card which is only
//! wide enough for one. Sized/positioned per the mockup agreed with the
//! user; this is the second of the SSX/SX/S/SQ/M variants planned there.
//!
//! This is a deliberately independent widget from `network::spawn`/
//! `restore` - each owns its own interface pin/custom-label/sample state
//! rather than sharing one - but both read through the same free functions
//! in `network.rs` (`read_interfaces`, `default_interface`,
//! `format_rate_fixed`), so the actual `/proc/net/dev`/`/proc/net/route`
//! parsing and rate formatting only exist once. The interface-picker
//! *UI* (dropdown + custom-label entry) is shared with the rest of the
//! network-widget family via `interface_picker::build` (audit finding
//! 2026-09-29 - it used to be duplicated here too, same call as
//! `cpu_temp.rs`'s/`temp_gauge.rs`'s own sensor picker still makes, see
//! that module's doc comment for why *that* one stays duplicated: only
//! 2 call sites, below the threshold that made this one worth sharing).
//!
//! Only one thing is user-configurable here (unlike SSX's interface *and*
//! direction): which interface to read. There's no direction setting
//! because this card is wide enough to show both at once - the whole
//! reason SSX needed one was that it couldn't.
//!
//! Everything (name, both arrows, both rates) is drawn as a *single*
//! `gtk::Label` with Pango markup for the colored/bold arrow spans, not
//! separate widgets side by side - `network.rs`'s SSX card originally used
//! a separate `gtk::Image` for its direction icon and a plain multi-widget
//! `gtk::Box` row, and ran into two real problems that this design avoids
//! outright: the icon didn't render under the user's icon theme, and a
//! long interface name could widen the row past the card and read as
//! "off-center". One Label has neither failure mode - Pango lays out and
//! centers the whole line itself, and every character (arrows included)
//! goes through the same text path already proven to render correctly.

use gtk::prelude::*;
use log::{debug, warn};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Once;
use std::time::Instant;

use crate::i18n_runtime as i18n;
use crate::widgets::interface_picker;
use crate::widgets::network::{default_interface, format_rate_fixed, read_interfaces, InterfaceCounters};
use crate::widgets::registry::WidgetInstance;

const REFRESH_INTERVAL_SECONDS: u32 = 2;
const FONT_PX: i32 = 20;

/// Same blue/coral pairing as `network.rs`'s SSX card (see
/// `network::Direction::color_hex`) - kept as separate constants here
/// rather than importing that private enum, since this widget never needs
/// a `Direction` value of its own (it always shows both).
const DOWN_COLOR_HEX: &str = "#5da9e8";
const UP_COLOR_HEX: &str = "#e8875d";

/// Unlike SSX's cap (which bounds only its own separate caption widget),
/// this one bounds Pango's layout of the *entire* line - name, both
/// arrows and both rates together, since it's all one `gtk::Label` (see
/// this module's doc comment). Sized for the longest line real content
/// ever produces (`enp0s31f6  ↓ 999.9M  ↑ 999.9M` is 29 characters) plus
/// a little slack, so it only ever kicks in for an unusually long custom
/// label - a first version of this cap was mistakenly sized for the name
/// alone (14 chars) and ended up ellipsizing the upload rate off of every
/// normal-length line.
const MAX_LINE_WIDTH_CHARS: i32 = 34;

static INSTALL_CSS: Once = Once::new();

fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!(".xeneon-network-sx-label {{ font-size: {FONT_PX}px; color: rgba(255, 255, 255, 0.75); }}"));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

/// All of this widget's live state - one instance per placed widget.
/// Interface pin/custom-label fields mirror `network::NetworkState`
/// exactly (see that module's doc comment on `custom_label` for why
/// renaming only applies once pinned); `last_sample` tracks *both* byte
/// counters at once instead of one direction's, since this card always
/// shows both.
struct NetworkSxState {
    label: gtk::Label,

    /// `None` means "auto-pick the default-route interface".
    interface_name: RefCell<Option<String>>,
    custom_label: RefCell<Option<String>>,
    available_interfaces: RefCell<Vec<String>>,
    /// `(interface name, rx bytes, tx bytes, when)` - both counters from
    /// the same sample, so a pin change (jumping to an unrelated pair of
    /// counters) is detected the same way `network::NetworkState` detects
    /// one.
    last_sample: RefCell<Option<(String, u64, u64, Instant)>>,
    logged_unavailable: Cell<bool>,
}

impl NetworkSxState {
    /// Same auto-pick logic as `network::NetworkState::effective_interface_name`:
    /// the user's pin if set, otherwise whichever interface currently holds
    /// the default route, otherwise the first interface seen at all.
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

    /// Back to this widget's out-of-the-box defaults (just the interface
    /// pin/custom label - there's no other appearance setting here) -
    /// goes through the same setters as everything else, so every side
    /// effect happens exactly like a normal edit would. Audit finding
    /// 2026-09-29: `on_reset` was left `None` in `spawn`/`restore`
    /// below, so resetting a card's appearance left its interface pin/
    /// custom label untouched - same gap `network_sq.rs`'s own `reset`
    /// doc comment already describes having been fixed there.
    fn reset(&self) {
        self.set_interface(None);
        self.set_custom_label(None);
    }

    /// The plain-text name portion - a non-empty `custom_label` wins while
    /// pinned, otherwise the real interface name, falling back to this
    /// widget's own generic title only when no interface name is known at
    /// all. Same shape as `network::NetworkState::display_label`.
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
    /// whichever interface is currently effective, and redraws the label.
    /// Called on every tick, every setter above, and a language change -
    /// same call sites as `network::NetworkState::refresh`.
    fn refresh(&self) {
        let interfaces = read_interfaces();
        *self.available_interfaces.borrow_mut() = interfaces.iter().map(|(name, _, _)| name.clone()).collect();

        let effective_name = self.effective_interface_name(&interfaces);
        let name_text = self.display_label(effective_name.as_deref());
        let counters = effective_name.as_ref().and_then(|name| interfaces.iter().find(|(n, _, _)| n == name));

        match counters {
            None => {
                // Logged once per disappearance, not every tick - same
                // reasoning as `network::NetworkState::refresh`.
                if !self.logged_unavailable.replace(true) {
                    warn!(
                        "no reading for interface {} ({} interfaces seen)",
                        effective_name.as_deref().unwrap_or("<none>"),
                        self.available_interfaces.borrow().len()
                    );
                }
                *self.last_sample.borrow_mut() = None;
                self.label.set_markup(&gtk::glib::markup_escape_text(&format!("{name_text}  --")));
                self.label.set_tooltip_text(Some(&i18n::t("widgets.network.unavailable")));
            }
            Some((name, rx_bytes, tx_bytes)) => {
                if self.logged_unavailable.replace(false) {
                    debug!("interface reading recovered: {name}");
                }
                let now = Instant::now();

                let mut last_sample = self.last_sample.borrow_mut();
                // Only trust the delta when it's against the *same*
                // interface as last tick, and both counters moved forward
                // (a reset/replug can make either jump back to 0) -
                // anything else shows "…" for this one tick rather than a
                // nonsense spike.
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

                // Fixed-width, monospace - without this, "6.1K" becoming
                // "12.4M" a couple of seconds later changes this label's
                // total natural width, and since it's centered as a whole,
                // the entire line visibly shifts even though only one
                // number changed (a real complaint from watching this
                // widget update live). See `format_rate_fixed`'s own doc
                // comment.
                let (down_text, up_text) = match rates {
                    Some((down, up)) => (format_rate_fixed(down), format_rate_fixed(up)),
                    None => (format!("{:>6}", "…"), format!("{:>6}", "…")),
                };
                // Same markup approach as `network::NetworkState::refresh`:
                // the name stays the label's ordinary muted, proportional-
                // font color; each arrow+rate pair gets its own colored,
                // bold, monospace span (monospace so the space-padding
                // above actually reserves constant pixel width - a
                // proportional font's digits aren't guaranteed as wide as
                // a plain space). The name is escaped since a user-typed
                // custom label isn't guaranteed markup-safe.
                self.label.set_markup(&format!(
                    "{}  <span color=\"{DOWN_COLOR_HEX}\" weight=\"bold\" font_family=\"monospace\">↓ {}</span>  \
                     <span color=\"{UP_COLOR_HEX}\" weight=\"bold\" font_family=\"monospace\">↑ {}</span>",
                    gtk::glib::markup_escape_text(&name_text),
                    gtk::glib::markup_escape_text(&down_text),
                    gtk::glib::markup_escape_text(&up_text),
                ));
                self.label.set_tooltip_text(Some(name));
            }
        }
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

impl interface_picker::InterfacePickable for NetworkSxState {
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

fn build_content() -> (Rc<NetworkSxState>, gtk::Widget) {
    ensure_css_installed();

    let label = gtk::Label::new(None);
    label.add_css_class("xeneon-network-sx-label");
    label.set_halign(gtk::Align::Center);
    label.set_valign(gtk::Align::Center);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(MAX_LINE_WIDTH_CHARS);

    let state = Rc::new(NetworkSxState {
        label: label.clone(),
        interface_name: RefCell::new(None),
        custom_label: RefCell::new(None),
        available_interfaces: RefCell::new(Vec::new()),
        last_sample: RefCell::new(None),
        logged_unavailable: Cell::new(false),
    });
    // First read happens synchronously, same reasoning as
    // `network::NetworkState::build_content`.
    state.refresh();

    let timeout_id = gtk::glib::timeout_add_seconds_local(REFRESH_INTERVAL_SECONDS, {
        let state = state.clone();
        move || {
            state.refresh();
            gtk::glib::ControlFlow::Continue
        }
    });
    label.connect_destroy({
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

    (state, label.upcast())
}

/// Builds the settings panel: which interface drives the display, and the
/// rename field once one is pinned - same shape as `network::build_settings`
/// minus the direction toggle (not needed here, see this module's doc
/// comment). The picker itself (dropdown + custom-label entry) is shared
/// with the rest of the network-widget family - see `interface_picker`'s
/// own doc comment (audit finding 2026-09-29).
fn build_settings(state: Rc<NetworkSxState>) -> (gtk::Widget, Box<dyn Fn()>) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(240, -1);

    let picker = interface_picker::build(state.clone());
    root.append(&picker.interface_label);
    root.append(&picker.interface_dropdown);
    root.append(&picker.custom_label_label);
    root.append(&picker.custom_label_entry);

    i18n::on_change({
        let interface_label = picker.interface_label.clone();
        let custom_label_label = picker.custom_label_label.clone();
        let refresh = picker.refresh.clone();
        move || {
            interface_label.set_label(&i18n::t("widgets.network.settings.interface"));
            custom_label_label.set_label(&i18n::t("widgets.network.settings.custom_label"));
            refresh();
        }
    });

    // Re-reads the custom-label entry's text from `state` - needed after
    // `state.reset()` (called from the appearance popover's reset
    // button, see `on_reset` in `spawn`/`restore` below) changes the
    // model directly. Mirrors `network_sq.rs::build_settings`'s own
    // `resync`.
    let resync: Box<dyn Fn()> = Box::new({
        let state = state.clone();
        let custom_label_entry = picker.custom_label_entry.clone();
        let refresh = picker.refresh.clone();
        move || {
            let custom_label = state.custom_label.borrow().clone();
            custom_label_entry.set_text(custom_label.as_deref().unwrap_or(""));
            refresh();
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
