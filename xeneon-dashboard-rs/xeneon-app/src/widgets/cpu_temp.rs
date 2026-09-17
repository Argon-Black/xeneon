//! System temperature widget (SSX footprint): a one-line "CPU 52°C"
//! readout of one hwmon sensor's current temperature. Ported from
//! `CpuTempContent`/`CpuTempSettings` in `xeneon_dashboard/widgets/
//! cpu_temp.py` on the Python side - see that file's module docstring for
//! the full rationale, summarized here.
//!
//! Reading is done straight from the kernel's hwmon sysfs tree
//! (`/sys/class/hwmon/hwmon*/{name,tempN_input,tempN_label}`), the same
//! interface the `sensors` CLI and `psutil` both read on top of - no crate
//! needed for this, just a handful of plain file reads (see
//! `all_sensors()`), matching this project's minimal-dependencies
//! preference. Reading it is a fast local syscall (no network, no D-Bus),
//! so this widget polls straight from the GLib main loop on a timer
//! instead of a worker thread, exactly like `ClockState`'s per-second tick.
//!
//! Machines expose CPU temperature under very different hwmon chip names
//! (`k10temp` on AMD, `coretemp` on Intel, `cpu_thermal` on many ARM
//! boards...), and some boards report several plausible entries (per-core,
//! per-CCD) under the same chip. `auto_pick_sensor()` guesses the most
//! CPU-like one from a priority list (mirrors `_auto_pick_sensor()` in the
//! Python original byte-for-byte in the priority order), but the guess can
//! be wrong on an unfamiliar board - so the settings panel also lets the
//! user pin one specific sensor manually. Once pinned manually, the raw
//! hwmon label rarely means anything to a human ("edge", "Composite"...),
//! so the caption becomes a free-text field the user fills in themselves
//! (`custom_label`) instead of being stuck with "CPU"
//! (`CpuTempState::display_label`).
//!
//! This module is written to be reused by a second, SQ-footprint "gauge"
//! variant of the same sensor reading (planned as a follow-up step, not
//! yet ported) - `all_sensors`/`auto_pick_sensor`/`sensor_display_name`
//! are all `pub` and free of any SSX-specific UI assumption for exactly
//! that reason, mirroring how the Python module ended up sharing those
//! same three functions between `CpuTempContent` and `TempGaugeContent`.

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::Once;

use crate::i18n_runtime as i18n;
use crate::widgets::registry::WidgetInstance;

/// Where the kernel exposes hardware sensors on Linux. Not configurable -
/// this is a fixed kernel ABI path, not something that varies by distro.
const HWMON_ROOT: &str = "/sys/class/hwmon";

/// How often to re-read the sensors and refresh the display. Cheap local
/// file reads, so this can be fairly frequent without any real cost.
const REFRESH_INTERVAL_SECONDS: u32 = 2;

/// Chip name -> priority, tried in this order; a chip not listed here can
/// still be auto-picked (see `auto_pick_sensor`'s fallback) but only once
/// nothing from this list is present at all.
const CHIP_PRIORITY: [&str; 5] = ["k10temp", "zenpower", "coretemp", "cpu_thermal", "acpitz"];

/// Within a chosen chip, a label matching one of these (compared
/// lowercased) is preferred over an arbitrary entry - "Tctl"/"Tdie" (AMD's
/// overall control temperature) and "Package id 0" (Intel's package-wide
/// sensor) are the ones that actually track "the CPU" as a whole, as
/// opposed to one specific core or CCD.
const LABEL_PRIORITY: [&str; 3] = ["tctl", "tdie", "package id 0"];

/// Same font size for both the "CPU" caption and the value next to it (see
/// `build_content` - they sit on one line) so neither reads as more
/// important than the other; only the weight (set via the CSS below)
/// tells them apart. Installed once, display-wide, like `dummy.rs`'s
/// `ensure_css_installed` - simpler than a per-instance CssProvider and
/// good enough since this widget only ever renders at one fixed size
/// (SSX), unlike e.g. Weather's resizable content.
const FONT_PX: i32 = 22;

static INSTALL_CSS: Once = Once::new();

fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!(
            ".xeneon-cputemp-label {{ font-size: {FONT_PX}px; color: rgba(255, 255, 255, 0.75); }}\n\
             .xeneon-cputemp-value {{ font-size: {FONT_PX}px; font-weight: 700; color: #ffffff; }}"
        ));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

/// One hwmon reading: `(chip name, label, current temperature in °C)`.
/// `label` is `""` when the hwmon entry has no matching `tempN_label` file
/// (e.g. `acpitz`, which reports nothing but a bare number).
pub type SensorReading = (String, String, f64);

/// Reads one file and trims it, or `None` if it can't be read - covers a
/// hwmon entry vanishing between `read_dir` and the actual read (a USB
/// sensor unplugged mid-scan, say), same defensive shape as
/// `_read_stripped()` in the Python original.
fn read_stripped(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Every `(chip, label, celsius)` triple exposed under `/sys/class/hwmon`
/// right now, across every chip on the machine - not just CPU-looking
/// ones, so the settings panel can offer the full list for the user to
/// pick from manually. Empty (rather than panicking/erroring) if hwmon
/// isn't there at all - mirrors `_all_sensors()` in cpu_temp.py exactly,
/// same three files per entry (`name`, `tempN_input`, `tempN_label`).
pub fn all_sensors() -> Vec<SensorReading> {
    let mut sensors = Vec::new();
    let Ok(hwmon_entries) = std::fs::read_dir(HWMON_ROOT) else {
        return sensors;
    };
    for hwmon_entry in hwmon_entries.flatten() {
        let hwmon_dir = hwmon_entry.path();
        let Some(chip) = read_stripped(&hwmon_dir.join("name")) else { continue };
        if chip.is_empty() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&hwmon_dir) else { continue };
        // Sorted for a stable, deterministic order in the settings
        // dropdown - matches the Python original's `sorted(filenames)`.
        let mut filenames: Vec<String> =
            files.flatten().map(|f| f.file_name().to_string_lossy().into_owned()).collect();
        filenames.sort();
        for filename in filenames {
            if !(filename.starts_with("temp") && filename.ends_with("_input")) {
                continue;
            }
            let Some(raw) = read_stripped(&hwmon_dir.join(&filename)) else { continue };
            // hwmon reports temperatures in millidegrees Celsius.
            let Ok(millidegrees) = raw.parse::<i64>() else { continue };
            let celsius = millidegrees as f64 / 1000.0;
            let label_filename = filename.replace("_input", "_label");
            let label = read_stripped(&hwmon_dir.join(&label_filename)).unwrap_or_default();
            sensors.push((chip.clone(), label, celsius));
        }
    }
    sensors
}

/// Guesses the most CPU-like sensor from `sensors`, trying each chip in
/// `CHIP_PRIORITY` order and, within a chosen chip, preferring a label
/// from `LABEL_PRIORITY` over whichever entry happened to come first.
/// Falls back to the very first sensor found if nothing on the priority
/// list is present at all (better than showing nothing). Mirrors
/// `_auto_pick_sensor()` in cpu_temp.py.
pub fn auto_pick_sensor(sensors: &[SensorReading]) -> Option<(String, String)> {
    for chip_name in CHIP_PRIORITY {
        let candidates: Vec<&SensorReading> = sensors.iter().filter(|(chip, _, _)| chip == chip_name).collect();
        if candidates.is_empty() {
            continue;
        }
        for (chip, label, _celsius) in &candidates {
            if LABEL_PRIORITY.contains(&label.to_lowercase().as_str()) {
                return Some((chip.clone(), label.clone()));
            }
        }
        let (chip, label, _celsius) = candidates[0];
        return Some((chip.clone(), label.clone()));
    }
    sensors.first().map(|(chip, label, _celsius)| (chip.clone(), label.clone()))
}

/// Human-facing name for one sensor entry in the settings dropdown -
/// `"chip (label)"`, or just `"chip"` when the entry has no label at all.
pub fn sensor_display_name(chip: &str, label: &str) -> String {
    if label.is_empty() {
        chip.to_string()
    } else {
        format!("{chip} ({label})")
    }
}

/// True if `entries[index]` is a real pinned sensor (`Some`) rather than
/// the "Auto" placeholder (`None` at index 0) - used to gate the
/// custom-label entry's sensitivity both right after a sensor change and
/// whenever the dropdown's whole model gets rebuilt (see `refresh_sensor_
/// model` and the dropdown's own `connect_selected_notify` in
/// `build_settings`).
fn is_manual(entries: &[Option<(String, String)>], index: usize) -> bool {
    entries.get(index).map(|entry| entry.is_some()).unwrap_or(false)
}

/// All of this widget's live state - one instance per placed widget,
/// shared (via `Rc`) between its content (the two labels) and its
/// settings panel. Mirrors `CpuTempContent`'s instance attributes in the
/// Python original; `RefCell`/`Cell` stand in for Python's plain mutable
/// attributes since Rust needs interior mutability to let both the
/// content and the settings panel hold a reference to the same state.
struct CpuTempState {
    caption_label: gtk::Label,
    value_label: gtk::Label,

    /// `None` means "auto-pick" (see `effective_sensor`) - set together
    /// with `sensor_label` by `set_sensor`, never independently.
    sensor_chip: RefCell<Option<String>>,
    sensor_label: RefCell<Option<String>>,
    /// Free-text override for the caption, only ever shown/edited while a
    /// sensor is pinned manually (see `display_label`) - hwmon labels for
    /// anything that isn't the CPU ("edge", "Composite"...) rarely mean
    /// anything to a human, so the user names it themselves instead of
    /// being stuck with "CPU".
    custom_label: RefCell<Option<String>>,
    unit_fahrenheit: Cell<bool>,
    /// Snapshot of `all_sensors()` from the most recent `refresh()` -
    /// re-read here (not just used transiently) so the settings panel can
    /// list what's currently on the machine without re-scanning hwmon
    /// itself.
    available_sensors: RefCell<Vec<SensorReading>>,
}

impl CpuTempState {
    /// The `(chip, label)` actually driving the display right now: the
    /// user's pin if set, otherwise the current auto-pick. Exposed so the
    /// settings panel can highlight which dropdown entry is really in
    /// effect.
    fn effective_sensor(&self) -> (Option<String>, Option<String>) {
        if let Some(chip) = self.sensor_chip.borrow().clone() {
            (Some(chip), self.sensor_label.borrow().clone())
        } else {
            match auto_pick_sensor(&self.available_sensors.borrow()) {
                Some((chip, label)) => (Some(chip), Some(label)),
                None => (None, None),
            }
        }
    }

    fn current_value(&self) -> Option<f64> {
        let (chip, label) = self.effective_sensor();
        let chip = chip?;
        let label = label.unwrap_or_default();
        self.available_sensors
            .borrow()
            .iter()
            .find(|(c, l, _celsius)| *c == chip && *l == label)
            .map(|(_c, _l, celsius)| *celsius)
    }

    /// "CPU" while auto-picking - fixed, not user-editable, since that's
    /// what this widget defaults to. Once a sensor is pinned manually, the
    /// user's own `custom_label` takes over, falling back to the same
    /// "CPU" text only until they've actually typed something.
    fn display_label(&self) -> String {
        if self.sensor_chip.borrow().is_some() {
            if let Some(custom) = self.custom_label.borrow().as_ref() {
                if !custom.is_empty() {
                    return custom.clone();
                }
            }
        }
        i18n::t("widgets.cpu_temp.label")
    }

    fn set_sensor(&self, chip: Option<String>, label: Option<String>) {
        *self.sensor_chip.borrow_mut() = chip;
        *self.sensor_label.borrow_mut() = label;
        self.refresh();
    }

    fn set_custom_label(&self, text: Option<String>) {
        *self.custom_label.borrow_mut() = text.filter(|s| !s.is_empty());
        self.refresh();
    }

    fn set_unit_fahrenheit(&self, enabled: bool) {
        self.unit_fahrenheit.set(enabled);
        self.refresh();
    }

    /// Re-reads every hwmon sensor, then redraws both labels from the
    /// fresh snapshot. Called on every tick (see `build_content`'s timer),
    /// on every setter above, and on a language change - cheap enough
    /// (a handful of small file reads) that there's no need to
    /// distinguish "just retranslate" from "actually re-poll" the way
    /// e.g. Weather has to for its network fetch.
    fn refresh(&self) {
        *self.available_sensors.borrow_mut() = all_sensors();
        self.caption_label.set_label(&self.display_label());
        match self.current_value() {
            None => {
                self.value_label.set_text("--°");
                self.value_label.set_tooltip_text(Some(&i18n::t("widgets.cpu_temp.unavailable")));
            }
            Some(celsius) => {
                let fahrenheit = self.unit_fahrenheit.get();
                let display = if fahrenheit { celsius * 9.0 / 5.0 + 32.0 } else { celsius };
                let unit_symbol = if fahrenheit { "°F" } else { "°C" };
                self.value_label.set_text(&format!("{}{unit_symbol}", display.round() as i64));
                self.value_label.set_tooltip_text(None);
            }
        }
    }

    fn to_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "sensor_chip": self.sensor_chip.borrow().clone(),
            "sensor_label": self.sensor_label.borrow().clone(),
            "custom_label": self.custom_label.borrow().clone(),
            "unit_fahrenheit": self.unit_fahrenheit.get(),
        })
    }

    /// Only touches fields actually present, so a partial/older saved
    /// dict still applies cleanly - mirrors `CpuTempContent.apply_dict()`.
    fn apply_dict(&self, data: &serde_json::Value) {
        if data.get("sensor_chip").is_some() {
            let chip = data.get("sensor_chip").and_then(|v| v.as_str()).map(str::to_string);
            let label = data.get("sensor_label").and_then(|v| v.as_str()).map(str::to_string);
            self.set_sensor(chip, label);
        }
        if data.get("custom_label").is_some() {
            let label = data.get("custom_label").and_then(|v| v.as_str()).map(str::to_string);
            self.set_custom_label(label);
        }
        if let Some(v) = data.get("unit_fahrenheit").and_then(|v| v.as_bool()) {
            self.set_unit_fahrenheit(v);
        }
    }
}

fn make_row(widgets: &[&gtk::Widget]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for w in widgets {
        row.append(*w);
    }
    row
}

/// Builds the widget's whole on-card display: "CPU" and the value ("52°C")
/// side by side on one line, same font size, centered - mirrors
/// `CpuTempContent.__init__` in the Python original.
fn build_content() -> (Rc<CpuTempState>, gtk::Widget) {
    ensure_css_installed();

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    root.set_halign(gtk::Align::Center);
    root.set_valign(gtk::Align::Center);

    let caption_label = gtk::Label::new(None);
    caption_label.add_css_class("xeneon-cputemp-label");
    caption_label.set_valign(gtk::Align::Center);
    root.append(&caption_label);

    let value_label = gtk::Label::new(None);
    value_label.add_css_class("xeneon-cputemp-value");
    value_label.set_valign(gtk::Align::Center);
    root.append(&value_label);

    let state = Rc::new(CpuTempState {
        caption_label,
        value_label,
        sensor_chip: RefCell::new(None),
        sensor_label: RefCell::new(None),
        custom_label: RefCell::new(None),
        unit_fahrenheit: Cell::new(false),
        available_sensors: RefCell::new(Vec::new()),
    });
    // First read happens synchronously here so `available_sensors` (and
    // therefore the settings panel's dropdown, built right after this
    // function returns) already has real data instead of starting empty
    // and waiting for the first timer tick.
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

/// Builds the settings panel shown in the configure popover: which sensor
/// drives the display (Auto, or one pinned entry), the free-text caption
/// override (only meaningful/sensitive once a sensor is pinned - see
/// `is_manual`), and a °C/°F toggle. Mirrors `CpuTempSettings.__init__` in
/// the Python original.
///
/// The sensor list is built once here, from whatever `state.available_
/// sensors` already holds at construction time - unlike e.g. a media
/// player list, hwmon chips don't appear or disappear while the app is
/// running, so (like the Python original) there's nothing to poll for
/// after this.
fn build_settings(state: Rc<CpuTempState>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(240, -1);

    let sensor_label_widget = gtk::Label::new(Some(&i18n::t("widgets.cpu_temp.settings.sensor")));
    sensor_label_widget.set_halign(gtk::Align::Start);
    root.append(&sensor_label_widget);

    // `None` at index 0 always stands for "Auto" (see `CpuTempState::
    // effective_sensor`); the rest come from whatever's on the machine
    // right now, plus the currently-pinned sensor if for some reason it's
    // not already in that list (e.g. a saved config carried over from a
    // different machine, or a sensor that's since disappeared).
    let mut initial_entries: Vec<Option<(String, String)>> = vec![None];
    let current_chip = state.sensor_chip.borrow().clone();
    let current_label = state.sensor_label.borrow().clone();
    for (chip, label, _celsius) in state.available_sensors.borrow().iter() {
        let entry = (chip.clone(), label.clone());
        if !initial_entries.iter().any(|e| e.as_ref() == Some(&entry)) {
            initial_entries.push(Some(entry));
        }
    }
    if let Some(chip) = &current_chip {
        let entry = (chip.clone(), current_label.clone().unwrap_or_default());
        if !initial_entries.iter().any(|e| e.as_ref() == Some(&entry)) {
            initial_entries.push(Some(entry));
        }
    }
    let entries = Rc::new(RefCell::new(initial_entries));

    let sensor_dropdown = gtk::DropDown::new(Some(gtk::StringList::new(&[])), gtk::Expression::NONE);
    sensor_dropdown.set_hexpand(true);
    root.append(&sensor_dropdown);

    // Kept insensitive rather than hidden while on Auto, so its position
    // in the popover doesn't jump around when switching back and forth.
    let custom_label_label = gtk::Label::new(None);
    custom_label_label.set_halign(gtk::Align::Start);
    root.append(&custom_label_label);
    let custom_label_entry = gtk::Entry::new();
    custom_label_entry.set_text(state.custom_label.borrow().as_deref().unwrap_or(""));
    root.append(&custom_label_entry);

    // Rebuilds the dropdown's translated option names + selection, and
    // the custom-label entry's sensitivity, from current state - called
    // once now and again on every language change (see the `i18n::
    // on_change` registration near the end of this function).
    let refresh_sensor_model: Rc<dyn Fn()> = {
        let entries = entries.clone();
        let state = state.clone();
        let sensor_dropdown = sensor_dropdown.clone();
        let custom_label_label = custom_label_label.clone();
        let custom_label_entry = custom_label_entry.clone();
        Rc::new(move || {
            let entries_ref = entries.borrow();
            let names: Vec<String> = entries_ref
                .iter()
                .map(|entry| match entry {
                    None => i18n::t("widgets.cpu_temp.settings.sensor_auto"),
                    Some((chip, label)) => sensor_display_name(chip, label),
                })
                .collect();
            let current = state
                .sensor_chip
                .borrow()
                .clone()
                .map(|chip| (chip, state.sensor_label.borrow().clone().unwrap_or_default()));
            let selected_index = entries_ref.iter().position(|e| e.as_ref() == current.as_ref()).unwrap_or(0);
            let manual = is_manual(&entries_ref, selected_index);
            drop(entries_ref);

            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            sensor_dropdown.set_model(Some(&gtk::StringList::new(&name_refs)));
            sensor_dropdown.set_selected(selected_index as u32);
            custom_label_label.set_sensitive(manual);
            custom_label_entry.set_sensitive(manual);
        })
    };
    refresh_sensor_model();

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let unit_label = gtk::Label::new(Some(&i18n::t("widgets.cpu_temp.settings.unit")));
    unit_label.set_hexpand(true);
    unit_label.set_halign(gtk::Align::Start);
    let celsius_button = gtk::ToggleButton::with_label(&i18n::t("widgets.cpu_temp.settings.unit_celsius"));
    let fahrenheit_button = gtk::ToggleButton::with_label(&i18n::t("widgets.cpu_temp.settings.unit_fahrenheit"));
    fahrenheit_button.set_group(Some(&celsius_button));
    celsius_button.set_active(!state.unit_fahrenheit.get());
    fahrenheit_button.set_active(state.unit_fahrenheit.get());
    root.append(&make_row(&[unit_label.upcast_ref(), celsius_button.upcast_ref(), fahrenheit_button.upcast_ref()]));

    // --- signal wiring: controls push one-way into `state` ---
    sensor_dropdown.connect_selected_notify({
        let state = state.clone();
        let entries = entries.clone();
        let custom_label_label = custom_label_label.clone();
        let custom_label_entry = custom_label_entry.clone();
        move |dropdown| {
            let index = dropdown.selected() as usize;
            let entries_ref = entries.borrow();
            if let Some(entry) = entries_ref.get(index) {
                match entry {
                    None => state.set_sensor(None, None),
                    Some((chip, label)) => state.set_sensor(Some(chip.clone()), Some(label.clone())),
                }
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
    fahrenheit_button.connect_toggled({
        let state = state.clone();
        move |b| state.set_unit_fahrenheit(b.is_active())
    });

    // --- retranslation ---
    i18n::on_change({
        let sensor_label_widget = sensor_label_widget.clone();
        let custom_label_label = custom_label_label.clone();
        let custom_label_entry = custom_label_entry.clone();
        let unit_label = unit_label.clone();
        let celsius_button = celsius_button.clone();
        let fahrenheit_button = fahrenheit_button.clone();
        let refresh_sensor_model = refresh_sensor_model.clone();
        move || {
            sensor_label_widget.set_label(&i18n::t("widgets.cpu_temp.settings.sensor"));
            custom_label_label.set_label(&i18n::t("widgets.cpu_temp.settings.custom_label"));
            custom_label_entry.set_placeholder_text(Some(&i18n::t("widgets.cpu_temp.label")));
            unit_label.set_label(&i18n::t("widgets.cpu_temp.settings.unit"));
            celsius_button.set_label(&i18n::t("widgets.cpu_temp.settings.unit_celsius"));
            fahrenheit_button.set_label(&i18n::t("widgets.cpu_temp.settings.unit_fahrenheit"));
            refresh_sensor_model();
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
        // No plugin-specific reset behavior (like the Python original -
        // CpuTempContent has no `reset()`): the generic appearance reset
        // the popover already provides is enough here.
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
