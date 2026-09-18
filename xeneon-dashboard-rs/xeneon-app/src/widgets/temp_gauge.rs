// SPDX-License-Identifier: GPL-3.0-or-later
//! System temperature widget (SQ footprint): the same hwmon sensor reading
//! as `cpu_temp.rs`'s SSX widget, drawn instead as a round dial (a Cairo
//! arc on a `gtk::DrawingArea`) with the value/unit/caption stacked as a
//! text overlay on top of it. Ported from `TempGaugeContent`/
//! `TempGaugeSettings` in `xeneon_dashboard/widgets/cpu_temp.py` on the
//! Python side.
//!
//! This is a deliberately independent widget from `cpu_temp::spawn`/
//! `restore` - each owns its own sensor pin/custom-label state rather than
//! sharing one - but both read through the same free functions in
//! `cpu_temp.rs` (`all_sensors`, `auto_pick_sensor`, `sensor_display_name`),
//! so the actual hwmon-parsing/auto-pick logic only exists once. The
//! sensor-picker *UI* (dropdown + custom-label entry) is small enough
//! (~60 lines) that it's duplicated here rather than factored into a
//! shared widget - see the Python module's own docstring for the same
//! call: not worth the extra indirection for two call sites.
//!
//! Content size, text color and the arc's own color are all user-editable
//! (see `build_settings`) and scale together off one slider - the same
//! `content_scale` technique as `WeatherContent` on the Python side, not
//! the heavier dual-footprint scaling `AudioContent` needs for its L/SQ
//! split (this widget only ever ships at one footprint, SQ, so a single
//! manual scale slider is all that's needed).

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

// Audit finding 2026-09-18: this used to be a private copy (with a
// WHITE parse-failure fallback, unlike appearance_popover.rs's BLACK) -
// unified on BLACK everywhere per the user's call, then deduped onto
// the one shared implementation.
use crate::appearance_popover::{hex_to_rgba, rgba_to_hex};
use crate::i18n_runtime as i18n;
use crate::widgets::cpu_temp::{all_sensors, auto_pick_sensor, sensor_display_name, SensorReading};
use crate::widgets::registry::WidgetInstance;

const REFRESH_INTERVAL_SECONDS: u32 = 2;

/// Gauge geometry, in degrees, clockwise from due east (Cairo's own angle
/// convention - 0 deg = 3 o'clock): a 270 deg dial starting at 135 deg (the
/// 7-8 o'clock position) and sweeping down to 45 deg (4-5 o'clock),
/// leaving the bottom wedge open like a speedometer.
const GAUGE_START_DEG: f64 = 135.0;
const GAUGE_SWEEP_DEG: f64 = 270.0;

/// The dial always maps this fixed Celsius range to its sweep, regardless
/// of the °C/°F display toggle (which only changes the printed number) -
/// 0-100 covers the range a CPU/GPU/NVMe temperature actually moves in.
const GAUGE_MIN_C: f64 = 0.0;
const GAUGE_MAX_C: f64 = 100.0;

/// Every size at `content_scale == 1.0` (100%) - everything below grows or
/// shrinks together off the settings panel's slider.
const BASE_ARC_RADIUS_PX: f64 = 110.0;
const BASE_ARC_THICKNESS_PX: f64 = 20.0;
const BASE_UNIT_FONT_PX: f64 = 20.0;
const BASE_VALUE_FONT_PX: f64 = 48.0;
const BASE_CAPTION_FONT_PX: f64 = 16.0;
const BASE_COLUMN_SPACING_PX: f64 = 4.0;

const MIN_CONTENT_SCALE: f64 = 0.5;
const MAX_CONTENT_SCALE: f64 = 2.0;
const DEFAULT_CONTENT_SCALE: f64 = 1.0;

const DEFAULT_TEXT_HEX: &str = "#ffffff";
const DEFAULT_BAR_HEX: &str = "#e0218a";

// Per-instance scaled font rules, keyed by each instance's own unique CSS
// class - one gauge widget's font sizing never bleeds into another's.
// Audit finding 2026-09-18: deduped onto `appearance_css::CssRuleRegistry`,
// the same registry pattern this and 3 other call sites used to hand-roll
// separately (this one previously as its own `GaugeCss` struct).
thread_local! {
    static GAUGE_CSS: crate::appearance_css::CssRuleRegistry =
        crate::appearance_css::CssRuleRegistry::new(gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

fn make_row(widgets: &[&gtk::Widget]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for w in widgets {
        row.append(*w);
    }
    row
}

fn is_manual(entries: &[Option<(String, String)>], index: usize) -> bool {
    entries.get(index).map(|entry| entry.is_some()).unwrap_or(false)
}

/// All of this widget's live state - one instance per placed widget.
/// Fields mirror `TempGaugeContent`'s own instance attributes in the
/// Python original, plus the extra `css_class`/`column`/`gauge_area`
/// handles the Rust side needs to reach back into the widget tree from
/// plain setter methods (Python just closes over `self`).
struct TempGaugeState {
    css_class: String,
    gauge_area: gtk::DrawingArea,
    column: gtk::Box,
    unit_label: gtk::Label,
    value_label: gtk::Label,
    caption_label: gtk::Label,

    sensor_chip: RefCell<Option<String>>,
    sensor_label: RefCell<Option<String>>,
    custom_label: RefCell<Option<String>>,
    unit_fahrenheit: Cell<bool>,
    available_sensors: RefCell<Vec<SensorReading>>,
    text_color: RefCell<gtk::gdk::RGBA>,
    bar_color: RefCell<gtk::gdk::RGBA>,
    content_scale: Cell<f64>,
}

impl TempGaugeState {
    // --- sensor selection: same behavior as CpuTempState in cpu_temp.rs,
    // kept as an independent copy rather than a shared type - see this
    // module's doc comment for why. ---

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

    fn display_label(&self) -> String {
        if self.sensor_chip.borrow().is_some() {
            if let Some(custom) = self.custom_label.borrow().as_ref() {
                if !custom.is_empty() {
                    return custom.clone();
                }
            }
        }
        i18n::t("widgets.temp_gauge.label")
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

    // --- gauge-specific appearance ---

    fn set_text_color(&self, rgba: gtk::gdk::RGBA) {
        *self.text_color.borrow_mut() = rgba;
        self.apply_content_scale(); // text color lives in the same CSS rule as the font sizes
    }

    fn set_bar_color(&self, rgba: gtk::gdk::RGBA) {
        *self.bar_color.borrow_mut() = rgba;
        self.gauge_area.queue_draw();
    }

    fn set_content_scale(&self, scale: f64) {
        self.content_scale.set(scale.clamp(MIN_CONTENT_SCALE, MAX_CONTENT_SCALE));
        self.apply_content_scale();
    }

    /// Rebuilds this instance's CSS rule (font sizes + text color) from
    /// `content_scale`/`text_color`, and rescales the column spacing to
    /// match - mirrors `_apply_content_scale()` in cpu_temp.py.
    fn apply_content_scale(&self) {
        let scale = self.content_scale.get();
        let text_hex = rgba_to_hex(&self.text_color.borrow());
        let rule = format!(
            ".{class} .xeneon-tempgauge-unit {{ font-size: {u}px; color: {text_hex}; }}\n\
             .{class} .xeneon-tempgauge-value {{ font-size: {v}px; font-weight: 700; color: {text_hex}; }}\n\
             .{class} .xeneon-tempgauge-caption {{ font-size: {c}px; color: {text_hex}; }}",
            class = self.css_class,
            u = (BASE_UNIT_FONT_PX * scale).round() as i32,
            v = (BASE_VALUE_FONT_PX * scale).round() as i32,
            c = (BASE_CAPTION_FONT_PX * scale).round() as i32,
        );
        GAUGE_CSS.with(|registry| registry.set_rule(&self.css_class, rule));
        self.column.set_spacing((BASE_COLUMN_SPACING_PX * scale).round() as i32);
        self.gauge_area.queue_draw();
    }

    /// Re-reads every hwmon sensor, redraws the text overlay, and queues a
    /// repaint of the arc - called on every tick, every setter above, and
    /// a language change, same as `CpuTempState::refresh`.
    fn refresh(&self) {
        *self.available_sensors.borrow_mut() = all_sensors();
        self.caption_label.set_label(&self.display_label());
        let fahrenheit = self.unit_fahrenheit.get();
        self.unit_label.set_label(if fahrenheit { "°F" } else { "°C" });
        match self.current_value() {
            None => {
                self.value_label.set_text("--");
                self.value_label.set_tooltip_text(Some(&i18n::t("widgets.temp_gauge.unavailable")));
            }
            Some(celsius) => {
                let display = if fahrenheit { celsius * 9.0 / 5.0 + 32.0 } else { celsius };
                // Two decimal places on purpose (unlike the plain SSX
                // widget's rounded integer) - matches the dial-gadget
                // look this widget is going for (e.g. AIDA64/HWiNFO
                // "82.00").
                self.value_label.set_text(&format!("{display:.2}"));
                self.value_label.set_tooltip_text(None);
            }
        }
        self.gauge_area.queue_draw();
    }

    fn to_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "sensor_chip": self.sensor_chip.borrow().clone(),
            "sensor_label": self.sensor_label.borrow().clone(),
            "custom_label": self.custom_label.borrow().clone(),
            "unit_fahrenheit": self.unit_fahrenheit.get(),
            "text_color": rgba_to_hex(&self.text_color.borrow()),
            "bar_color": rgba_to_hex(&self.bar_color.borrow()),
            "content_scale": self.content_scale.get(),
        })
    }

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
        if let Some(v) = data.get("text_color").and_then(|v| v.as_str()) {
            self.set_text_color(hex_to_rgba(v));
        }
        if let Some(v) = data.get("bar_color").and_then(|v| v.as_str()) {
            self.set_bar_color(hex_to_rgba(v));
        }
        if let Some(v) = data.get("content_scale").and_then(|v| v.as_f64()) {
            self.set_content_scale(v);
        }
    }
}

/// The fraction (0.0-1.0) of the dial's sweep that should be filled for a
/// given Celsius reading, clamped to the gauge's fixed 0-100 range -
/// mirrors `_gauge_fraction()` in cpu_temp.py. `None` (no sensor data)
/// draws an empty dial rather than an error state.
fn gauge_fraction(celsius: Option<f64>) -> f64 {
    let Some(celsius) = celsius else { return 0.0 };
    let span = GAUGE_MAX_C - GAUGE_MIN_C;
    ((celsius - GAUGE_MIN_C) / span).clamp(0.0, 1.0)
}

/// Paints the dial itself: a faint full-sweep track, then a colored arc
/// over the filled fraction - both with round line caps so the ends read
/// as a continuous pill rather than a sharp-cut bar. Mirrors
/// `TempGaugeContent._on_draw()` in cpu_temp.py. The text overlay (value/
/// unit/caption) is ordinary GTK labels layered on top by `build_content`,
/// not drawn here.
fn draw_gauge(state: &TempGaugeState, cr: &gtk::cairo::Context, width: i32, height: i32) {
    let cx = width as f64 / 2.0;
    let cy = height as f64 / 2.0;
    let scale = state.content_scale.get();
    let radius = BASE_ARC_RADIUS_PX * scale;
    let thickness = BASE_ARC_THICKNESS_PX * scale;
    let start = GAUGE_START_DEG.to_radians();
    let end = (GAUGE_START_DEG + GAUGE_SWEEP_DEG).to_radians();

    cr.set_line_cap(gtk::cairo::LineCap::Round);
    cr.set_line_width(thickness);

    cr.set_source_rgba(1.0, 1.0, 1.0, 0.12);
    cr.arc(cx, cy, radius, start, end);
    let _ = cr.stroke();

    let fraction = gauge_fraction(state.current_value());
    if fraction > 0.0 {
        let bar = state.bar_color.borrow();
        cr.set_source_rgba(bar.red() as f64, bar.green() as f64, bar.blue() as f64, bar.alpha() as f64);
        cr.arc(cx, cy, radius, start, start + (end - start) * fraction);
        let _ = cr.stroke();
    }
}

/// Builds the widget's on-card display: the dial (a `DrawingArea` filling
/// the whole card) with the unit/value/caption column overlaid and
/// centered on top of it - mirrors `TempGaugeContent.__init__`.
fn build_content() -> (Rc<TempGaugeState>, gtk::Widget) {
    // No separate "ensure installed" call needed here - CssRuleRegistry
    // installs its provider lazily on the first set_rule() call, and
    // apply_content_scale() below (called synchronously a few lines
    // down) makes exactly that call before this widget is shown.
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let css_class = format!("xeneon-tempgauge-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));

    let root = gtk::Overlay::new();
    root.add_css_class(&css_class);

    let gauge_area = gtk::DrawingArea::new();
    gauge_area.set_hexpand(true);
    gauge_area.set_vexpand(true);
    root.set_child(Some(&gauge_area));

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.set_halign(gtk::Align::Center);
    column.set_valign(gtk::Align::Center);
    // The overlay column must never intercept a click meant for anything
    // below it (there's nothing clickable here, but this matches the
    // header-label pattern used for every other widget's floating title).
    column.set_can_target(false);

    let unit_label = gtk::Label::new(None);
    unit_label.add_css_class("xeneon-tempgauge-unit");
    column.append(&unit_label);
    let value_label = gtk::Label::new(None);
    value_label.add_css_class("xeneon-tempgauge-value");
    column.append(&value_label);
    let caption_label = gtk::Label::new(None);
    caption_label.add_css_class("xeneon-tempgauge-caption");
    column.append(&caption_label);
    root.add_overlay(&column);

    let state = Rc::new(TempGaugeState {
        css_class,
        gauge_area: gauge_area.clone(),
        column,
        unit_label,
        value_label,
        caption_label,
        sensor_chip: RefCell::new(None),
        sensor_label: RefCell::new(None),
        custom_label: RefCell::new(None),
        unit_fahrenheit: Cell::new(false),
        available_sensors: RefCell::new(Vec::new()),
        text_color: RefCell::new(hex_to_rgba(DEFAULT_TEXT_HEX)),
        bar_color: RefCell::new(hex_to_rgba(DEFAULT_BAR_HEX)),
        content_scale: Cell::new(DEFAULT_CONTENT_SCALE),
    });

    gauge_area.set_draw_func({
        let state = state.clone();
        move |_area, cr, width, height| draw_gauge(&state, cr, width, height)
    });

    // First pass synchronously here, same reasoning as CpuTempState::
    // build_content in cpu_temp.rs: by the time build_settings() runs
    // right after this returns, available_sensors already has real data.
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

/// Builds the settings panel: the same sensor picker/custom-label pair as
/// `cpu_temp::build_settings` (see this module's doc comment for why it's
/// duplicated rather than shared), plus what's specific to the dial - text
/// color, arc color, and the content-size slider. Mirrors
/// `TempGaugeSettings.__init__`.
fn build_settings(state: Rc<TempGaugeState>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(260, -1);

    let sensor_label_widget = gtk::Label::new(Some(&i18n::t("widgets.temp_gauge.settings.sensor")));
    sensor_label_widget.set_halign(gtk::Align::Start);
    root.append(&sensor_label_widget);

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

    let custom_label_label = gtk::Label::new(None);
    custom_label_label.set_halign(gtk::Align::Start);
    root.append(&custom_label_label);
    let custom_label_entry = gtk::Entry::new();
    custom_label_entry.set_text(state.custom_label.borrow().as_deref().unwrap_or(""));
    root.append(&custom_label_entry);

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
                    None => i18n::t("widgets.temp_gauge.settings.sensor_auto"),
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

    let unit_label = gtk::Label::new(Some(&i18n::t("widgets.temp_gauge.settings.unit")));
    unit_label.set_hexpand(true);
    unit_label.set_halign(gtk::Align::Start);
    let celsius_button = gtk::ToggleButton::with_label(&i18n::t("widgets.temp_gauge.settings.unit_celsius"));
    let fahrenheit_button = gtk::ToggleButton::with_label(&i18n::t("widgets.temp_gauge.settings.unit_fahrenheit"));
    fahrenheit_button.set_group(Some(&celsius_button));
    celsius_button.set_active(!state.unit_fahrenheit.get());
    fahrenheit_button.set_active(state.unit_fahrenheit.get());
    root.append(&make_row(&[unit_label.upcast_ref(), celsius_button.upcast_ref(), fahrenheit_button.upcast_ref()]));

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let text_color_label = gtk::Label::new(Some(&i18n::t("widgets.temp_gauge.settings.text_color")));
    text_color_label.set_hexpand(true);
    text_color_label.set_halign(gtk::Align::Start);
    let text_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    text_color_button.set_rgba(&state.text_color.borrow());
    root.append(&make_row(&[text_color_label.upcast_ref(), text_color_button.upcast_ref()]));

    let bar_color_label = gtk::Label::new(Some(&i18n::t("widgets.temp_gauge.settings.bar_color")));
    bar_color_label.set_hexpand(true);
    bar_color_label.set_halign(gtk::Align::Start);
    let bar_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    bar_color_button.set_rgba(&state.bar_color.borrow());
    root.append(&make_row(&[bar_color_label.upcast_ref(), bar_color_button.upcast_ref()]));

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let scale_label = gtk::Label::new(Some(&i18n::t("widgets.temp_gauge.settings.content_scale")));
    scale_label.set_halign(gtk::Align::Start);
    root.append(&scale_label);
    let scale_slider =
        gtk::Scale::with_range(gtk::Orientation::Horizontal, MIN_CONTENT_SCALE * 100.0, MAX_CONTENT_SCALE * 100.0, 1.0);
    scale_slider.set_value(state.content_scale.get() * 100.0);
    scale_slider.set_draw_value(true);
    scale_slider.set_value_pos(gtk::PositionType::Right);
    root.append(&scale_slider);

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
    text_color_button.connect_rgba_notify({
        let state = state.clone();
        move |b| state.set_text_color(b.rgba())
    });
    bar_color_button.connect_rgba_notify({
        let state = state.clone();
        move |b| state.set_bar_color(b.rgba())
    });
    scale_slider.connect_value_changed({
        let state = state.clone();
        move |s| state.set_content_scale(s.value() / 100.0)
    });

    // --- retranslation ---
    i18n::on_change({
        let sensor_label_widget = sensor_label_widget.clone();
        let custom_label_label = custom_label_label.clone();
        let custom_label_entry = custom_label_entry.clone();
        let unit_label = unit_label.clone();
        let celsius_button = celsius_button.clone();
        let fahrenheit_button = fahrenheit_button.clone();
        let text_color_label = text_color_label.clone();
        let bar_color_label = bar_color_label.clone();
        let scale_label = scale_label.clone();
        let refresh_sensor_model = refresh_sensor_model.clone();
        move || {
            sensor_label_widget.set_label(&i18n::t("widgets.temp_gauge.settings.sensor"));
            custom_label_label.set_label(&i18n::t("widgets.temp_gauge.settings.custom_label"));
            custom_label_entry.set_placeholder_text(Some(&i18n::t("widgets.temp_gauge.label")));
            unit_label.set_label(&i18n::t("widgets.temp_gauge.settings.unit"));
            celsius_button.set_label(&i18n::t("widgets.temp_gauge.settings.unit_celsius"));
            fahrenheit_button.set_label(&i18n::t("widgets.temp_gauge.settings.unit_fahrenheit"));
            text_color_label.set_label(&i18n::t("widgets.temp_gauge.settings.text_color"));
            bar_color_label.set_label(&i18n::t("widgets.temp_gauge.settings.bar_color"));
            scale_label.set_label(&i18n::t("widgets.temp_gauge.settings.content_scale"));
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
