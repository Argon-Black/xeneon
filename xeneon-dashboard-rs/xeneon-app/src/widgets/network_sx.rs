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
//! `format_rate_compact`), so the actual `/proc/net/dev`/`/proc/net/route`
//! parsing and rate formatting only exist once. The interface-picker *UI*
//! (dropdown + custom-label entry) is small enough that it's duplicated
//! here rather than factored into a shared widget - same call as
//! `cpu_temp.rs`/`temp_gauge.rs` make for their own sensor picker, see
//! `temp_gauge.rs`'s module doc comment for the reasoning.
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
use crate::widgets::network::{default_interface, format_rate_compact, read_interfaces, InterfaceCounters};
use crate::widgets::registry::WidgetInstance;

const REFRESH_INTERVAL_SECONDS: u32 = 2;
const FONT_PX: i32 = 20;

/// Same blue/coral pairing as `network.rs`'s SSX card (see
/// `network::Direction::color_hex`) - kept as separate constants here
/// rather than importing that private enum, since this widget never needs
/// a `Direction` value of its own (it always shows both).
const DOWN_COLOR_HEX: &str = "#5da9e8";
const UP_COLOR_HEX: &str = "#e8875d";

/// Generous compared to SSX's 6-character cap - this card is more than
/// twice as wide, and unlike SSX the name isn't the only thing on the
/// line, so it still needs to leave room for both rates. Only matters for
/// an unusually long custom label; real interface names are always well
/// under this.
const MAX_NAME_WIDTH_CHARS: i32 = 14;

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

                let (down_text, up_text) = match rates {
                    Some((down, up)) => (format_rate_compact(down), format_rate_compact(up)),
                    None => ("…".to_string(), "…".to_string()),
                };
                // Same markup approach as `network::NetworkState::refresh`:
                // the name stays the label's ordinary muted color, each
                // arrow+rate pair gets its own colored/bold span. The name
                // is escaped since a user-typed custom label isn't
                // guaranteed markup-safe.
                self.label.set_markup(&format!(
                    "{}  <span color=\"{DOWN_COLOR_HEX}\" weight=\"bold\">↓ {}</span>  \
                     <span color=\"{UP_COLOR_HEX}\" weight=\"bold\">↑ {}</span>",
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

fn build_content() -> (Rc<NetworkSxState>, gtk::Widget) {
    ensure_css_installed();

    let label = gtk::Label::new(None);
    label.add_css_class("xeneon-network-sx-label");
    label.set_halign(gtk::Align::Center);
    label.set_valign(gtk::Align::Center);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(MAX_NAME_WIDTH_CHARS);

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
/// comment).
fn build_settings(state: Rc<NetworkSxState>) -> gtk::Widget {
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
