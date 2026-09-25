// SPDX-License-Identifier: GPL-3.0-or-later
//! Network throughput widget (S footprint): "wlan0  ↓12.4M/s  ↑1.2M/s" -
//! same information as `network_sx.rs`'s SX card, at the full-column width
//! this card gets instead of a half-column. Sized/positioned per the
//! mockup agreed with the user; this is the third of the SSX/SX/S/SQ/M
//! variants planned there - the mockup's inline sparkline and VPN badge
//! are follow-up steps, not built yet (same "text first, iterate" scope
//! the SSX/SX steps already used).
//!
//! A near-duplicate of `network_sx.rs` - same interface Auto/pin +
//! rename-only settings, same single-`gtk::Label`-with-markup design for
//! the same reasons (see that module's doc comment) - with two real
//! differences: there's room for the `/s` suffix on each rate (SX drops it
//! for space), and the ellipsize cap is far more generous since this card
//! is roughly twice as wide. Reuses `network.rs`'s `read_interfaces`/
//! `default_interface`/`format_rate_fixed`, same as SX does.

use gtk::prelude::*;
use log::{debug, warn};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Once;
use std::time::Instant;

use crate::i18n_runtime as i18n;
use crate::widgets::network::{default_interface, format_rate_fixed, read_interfaces, InterfaceCounters};
use crate::widgets::registry::WidgetInstance;

const REFRESH_INTERVAL_SECONDS: u32 = 2;
const FONT_PX: i32 = 22;

/// Same blue/coral pairing as `network.rs`'s SSX card and `network_sx.rs`'s
/// SX card (see `network::Direction::color_hex`).
const DOWN_COLOR_HEX: &str = "#5da9e8";
const UP_COLOR_HEX: &str = "#e8875d";

/// Far more generous than SX's 34-character cap - this card is roughly
/// twice as wide, while real content only grows by the two `/s` suffixes
/// (4 characters). Still only a safety net for an unusually long custom
/// label; normal content never gets close.
const MAX_LINE_WIDTH_CHARS: i32 = 50;

static INSTALL_CSS: Once = Once::new();

fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!(".xeneon-network-s-label {{ font-size: {FONT_PX}px; color: rgba(255, 255, 255, 0.75); }}"));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

/// All of this widget's live state - one instance per placed widget. Same
/// shape as `network_sx::NetworkSxState` - see that struct's doc comment.
struct NetworkSState {
    label: gtk::Label,

    interface_name: RefCell<Option<String>>,
    custom_label: RefCell<Option<String>>,
    available_interfaces: RefCell<Vec<String>>,
    last_sample: RefCell<Option<(String, u64, u64, Instant)>>,
    logged_unavailable: Cell<bool>,
}

impl NetworkSState {
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

    /// Same shape as `network::NetworkState::display_label`/
    /// `network_sx::NetworkSxState::display_label`.
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
    /// whichever interface is currently effective, and redraws the label -
    /// same call sites and reasoning as `network_sx::NetworkSxState::refresh`.
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

                // Fixed-width, monospace, `/s` suffix included in the
                // constant part so it doesn't disturb the padding - same
                // stability reasoning as `network_sx.rs`'s own rate
                // formatting (see `format_rate_fixed`'s doc comment).
                let (down_text, up_text) = match rates {
                    Some((down, up)) => (format!("{}/s", format_rate_fixed(down)), format!("{}/s", format_rate_fixed(up))),
                    None => (format!("{:>6}/s", "…"), format!("{:>6}/s", "…")),
                };
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

/// True if `entries[index]` is a real pinned interface (`Some`) rather
/// than the "Auto" placeholder (`None` at index 0).
fn is_manual(entries: &[Option<String>], index: usize) -> bool {
    entries.get(index).map(|entry| entry.is_some()).unwrap_or(false)
}

fn build_content() -> (Rc<NetworkSState>, gtk::Widget) {
    ensure_css_installed();

    let label = gtk::Label::new(None);
    label.add_css_class("xeneon-network-s-label");
    label.set_halign(gtk::Align::Center);
    label.set_valign(gtk::Align::Center);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(MAX_LINE_WIDTH_CHARS);

    let state = Rc::new(NetworkSState {
        label: label.clone(),
        interface_name: RefCell::new(None),
        custom_label: RefCell::new(None),
        available_interfaces: RefCell::new(Vec::new()),
        last_sample: RefCell::new(None),
        logged_unavailable: Cell::new(false),
    });
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
/// rename field once one is pinned - identical to `network_sx::build_settings`.
fn build_settings(state: Rc<NetworkSState>) -> gtk::Widget {
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
