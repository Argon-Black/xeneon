// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared implementation behind the S and SX text-only network-throughput
//! cards (`network_s.rs`/`network_sx.rs`): "wlan0  ↓12.4M/s  ↑1.2M/s"
//! drawn as a single `gtk::Label` with Pango markup (see
//! `network_sx.rs`'s own doc comment for why - avoids both an icon that
//! didn't render under the user's icon theme and a long-name-induced
//! off-center row that a multi-widget layout ran into). Dedup audit
//! finding 2026-09-29, the smaller/lower-risk follow-up to step 4's
//! `network_card.rs` (SQ/M) - S and SX were ~280 duplicated lines
//! differing only in font size, line-width cap, and whether each rate
//! carries a `/s` suffix (SX drops it; this card is narrower and every
//! character counts there).
//!
//! Parameterized by `NetworkTextCardVariant`; `network_s.rs`/
//! `network_sx.rs` now each just implement that trait and forward
//! `spawn()`/`restore()` to the generic `network_text_card::spawn::<V>`/
//! `restore::<V>`, so `registry.rs`'s function-pointer references to them
//! don't need to change.

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

/// Same blue/coral pairing every other network card uses (see
/// `network::Direction::color_hex`).
const DOWN_COLOR_HEX: &str = "#5da9e8";
const UP_COLOR_HEX: &str = "#e8875d";

/// Per-card-footprint knobs - everything that actually differs between
/// `network_s.rs` and `network_sx.rs`. `ID` drives the label's CSS class
/// (`.xeneon-network-{ID}-label`).
pub trait NetworkTextCardVariant: 'static {
    const ID: &'static str;
    const FONT_PX: i32;
    /// Bounds Pango's layout of the *entire* line - name, both arrows and
    /// both rates together, since it's all one `gtk::Label`.
    const MAX_LINE_WIDTH_CHARS: i32;
    /// Whether each rate carries a `/s` suffix - true for S (wide enough
    /// to spare), false for SX (narrower, every character counts).
    const SHOW_RATE_UNIT_SUFFIX: bool;
}

/// All of this widget's live state - one instance per placed widget,
/// generic over which card footprint it's running as.
struct NetworkTextCardState<V: NetworkTextCardVariant> {
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

    _variant: std::marker::PhantomData<V>,
}

impl<V: NetworkTextCardVariant> NetworkTextCardState<V> {
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
        self.refresh();
    }

    fn set_custom_label(&self, text: Option<String>) {
        *self.custom_label.borrow_mut() = text.filter(|s| !s.is_empty());
        self.refresh();
    }

    /// Back to this widget's out-of-the-box defaults (just the interface
    /// pin/custom label - there's no other appearance setting on either
    /// card) - goes through the same setters as everything else, so
    /// every side effect happens exactly like a normal edit would.
    fn reset(&self) {
        self.set_interface(None);
        self.set_custom_label(None);
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
    /// whichever interface is currently effective, and redraws the label.
    /// Called on every tick, every setter above, and a language change.
    fn refresh(&self) {
        let interfaces = read_interfaces();
        *self.available_interfaces.borrow_mut() = interfaces.iter().map(|(name, _, _)| name.clone()).collect();

        let effective_name = self.effective_interface_name(&interfaces);
        let name_text = self.display_label(effective_name.as_deref());
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
                self.label.set_markup(&gtk::glib::markup_escape_text(&format!("{name_text}  --")));
                self.label.set_tooltip_text(Some(&i18n::t("widgets.network.unavailable")));
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

                // Fixed-width, monospace - without this, "6.1K" becoming
                // "12.4M" a couple of seconds later changes this label's
                // total natural width, and since it's centered as a
                // whole, the entire line visibly shifts even though only
                // one number changed. See `format_rate_fixed`'s own doc
                // comment.
                let (down_text, up_text) = if V::SHOW_RATE_UNIT_SUFFIX {
                    match rates {
                        Some((down, up)) => (format!("{}/s", format_rate_fixed(down)), format!("{}/s", format_rate_fixed(up))),
                        None => (format!("{:>6}/s", "…"), format!("{:>6}/s", "…")),
                    }
                } else {
                    match rates {
                        Some((down, up)) => (format_rate_fixed(down), format_rate_fixed(up)),
                        None => (format!("{:>6}", "…"), format!("{:>6}", "…")),
                    }
                };
                // Each arrow+rate pair gets its own colored, bold,
                // monospace span (monospace so the space-padding above
                // actually reserves constant pixel width); the name is
                // escaped since a user-typed custom label isn't
                // guaranteed markup-safe.
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

impl<V: NetworkTextCardVariant> interface_picker::InterfacePickable for NetworkTextCardState<V> {
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

fn ensure_css_installed<V: NetworkTextCardVariant>() {
    static INSTALL_CSS: Once = Once::new();
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let id = V::ID;
        let font_px = V::FONT_PX;
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!(".xeneon-network-{id}-label {{ font-size: {font_px}px; color: rgba(255, 255, 255, 0.75); }}"));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

fn build_content<V: NetworkTextCardVariant>() -> (Rc<NetworkTextCardState<V>>, gtk::Widget) {
    ensure_css_installed::<V>();

    let label = gtk::Label::new(None);
    label.add_css_class(&format!("xeneon-network-{}-label", V::ID));
    label.set_halign(gtk::Align::Center);
    label.set_valign(gtk::Align::Center);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(V::MAX_LINE_WIDTH_CHARS);

    let state = Rc::new(NetworkTextCardState::<V> {
        label: label.clone(),
        interface_name: RefCell::new(None),
        custom_label: RefCell::new(None),
        available_interfaces: RefCell::new(Vec::new()),
        last_sample: RefCell::new(None),
        logged_unavailable: Cell::new(false),
        _variant: std::marker::PhantomData,
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
/// rename field once one is pinned - the picker itself is shared with the
/// rest of the network-widget family, see `interface_picker`'s own doc
/// comment.
fn build_settings<V: NetworkTextCardVariant>(state: Rc<NetworkTextCardState<V>>) -> (gtk::Widget, Box<dyn Fn()>) {
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
    // model directly.
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

pub fn spawn<V: NetworkTextCardVariant>() -> WidgetInstance {
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

pub fn restore<V: NetworkTextCardVariant>(data: &serde_json::Value) -> WidgetInstance {
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
