//! The clock widget: city name, a flip-clock-style HH:MM:SS, and the date
//! below in short or long form - full port of `ClockContent`/
//! `ClockSettings` from widgets/clock.py, the Python app's own
//! reference-pattern plugin (see CLAUDE.md). Time/label each have their
//! own font and color; either the city or the date line can be hidden.
//! Ticks every second via a GLib timeout, cleaned up on destroy.

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::i18n_runtime as i18n;
use crate::widgets::registry::WidgetInstance;

const DEFAULT_TEXT_HEX: &str = "#ffffff";
const DEFAULT_TIME_FONT: &str = "Sans 72";
const DEFAULT_LABEL_FONT: &str = "Sans 18";
const DEFAULT_CITY_INDEX: usize = 6; // Los Angeles, to match the reference look

// Segment box padding as a fraction of the chosen digit font size, not a
// fixed pixel amount - so the box keeps the same proportions around the
// digit whether the user picks a tiny or a huge font.
const SEGMENT_MARGIN_H_RATIO: f64 = 0.375;
const SEGMENT_MARGIN_V_RATIO: f64 = 0.21;

// Predefined city choices, each a (i18n key, IANA timezone) pair. Free-form
// tz search would cover more ground but isn't worth the extra UI for a
// dashboard clock - this list spans the timezones people actually ask for.
const CITIES: [(&str, &str); 12] = [
    ("widgets.clock.cities.paris", "Europe/Paris"),
    ("widgets.clock.cities.london", "Europe/London"),
    ("widgets.clock.cities.berlin", "Europe/Berlin"),
    ("widgets.clock.cities.moscow", "Europe/Moscow"),
    ("widgets.clock.cities.new_york", "America/New_York"),
    ("widgets.clock.cities.chicago", "America/Chicago"),
    ("widgets.clock.cities.los_angeles", "America/Los_Angeles"),
    ("widgets.clock.cities.sao_paulo", "America/Sao_Paulo"),
    ("widgets.clock.cities.dubai", "Asia/Dubai"),
    ("widgets.clock.cities.tokyo", "Asia/Tokyo"),
    ("widgets.clock.cities.beijing", "Asia/Shanghai"),
    ("widgets.clock.cities.sydney", "Australia/Sydney"),
];

const DAY_KEYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
const MONTH_KEYS: [&str; 12] =
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

fn rgba_to_hex(rgba: &gtk::gdk::RGBA) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (rgba.red() * 255.0).round() as u8,
        (rgba.green() * 255.0).round() as u8,
        (rgba.blue() * 255.0).round() as u8
    )
}

fn hex_to_rgba(hex: &str) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::parse(hex).unwrap_or_else(|_| gtk::gdk::RGBA::WHITE)
}

fn markup(text: &str, font_desc: &gtk::pango::FontDescription, color: &gtk::gdk::RGBA) -> String {
    format!(
        "<span font_desc=\"{}\" foreground=\"{}\">{}</span>",
        gtk::glib::markup_escape_text(&font_desc.to_string()),
        rgba_to_hex(color),
        gtk::glib::markup_escape_text(text)
    )
}

/// Wraps `widget` in a box that claims all the leftover space a CenterBox
/// start/end slot would otherwise leave unclaimed, so `widget`'s own
/// valign=CENTER centers it within that space instead of sitting flush
/// against the CenterBox's outer edge.
fn expand_wrapper(widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let wrapper = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    wrapper.set_vexpand(true);
    wrapper.append(widget);
    wrapper
}

struct ClockState {
    city_label: gtk::Label,
    hour_label: gtk::Label,
    minute_label: gtk::Label,
    second_label: gtk::Label,
    colon1_label: gtk::Label,
    colon2_label: gtk::Label,
    ampm_label: gtk::Label,
    date_label: gtk::Label,

    time_font_desc: RefCell<gtk::pango::FontDescription>,
    time_color: RefCell<gtk::gdk::RGBA>,
    label_font_desc: RefCell<gtk::pango::FontDescription>,
    label_color: RefCell<gtk::gdk::RGBA>,
    timezone: RefCell<chrono_tz::Tz>,
    city_key: RefCell<String>,
    date_format_long: Cell<bool>,
    hour_format_12h: Cell<bool>,
    city_visible: Cell<bool>,
    date_visible: Cell<bool>,
}

impl ClockState {
    fn apply_segment_margins(&self) {
        let size = self.time_font_desc.borrow().size();
        let base_pt =
            if size > 0 { size as f64 / gtk::pango::SCALE as f64 } else { 12.0 };
        let h = ((base_pt * SEGMENT_MARGIN_H_RATIO).round() as i32).max(2);
        let v = ((base_pt * SEGMENT_MARGIN_V_RATIO).round() as i32).max(2);
        for label in [&self.hour_label, &self.minute_label, &self.second_label] {
            label.set_margin_start(h);
            label.set_margin_end(h);
            label.set_margin_top(v);
            label.set_margin_bottom(v);
        }
    }

    fn refresh(&self) {
        let tz = *self.timezone.borrow();
        let now = chrono::Utc::now().with_timezone(&tz);

        let label_font = self.label_font_desc.borrow();
        let label_color = self.label_color.borrow();
        self.city_label.set_markup(&markup(&i18n::t(&self.city_key.borrow()), &label_font, &label_color));

        let hour_12 = self.hour_format_12h.get();
        let hour_value = if hour_12 {
            let h = now.hour() % 12;
            if h == 0 { 12 } else { h }
        } else {
            now.hour()
        };
        if hour_12 {
            let ampm_key = if now.hour() < 12 { "widgets.clock.am" } else { "widgets.clock.pm" };
            self.ampm_label.set_markup(&markup(&i18n::t(ampm_key), &label_font, &label_color));
        }

        let time_font = self.time_font_desc.borrow();
        let time_color = self.time_color.borrow();
        self.hour_label.set_markup(&markup(&format!("{hour_value:02}"), &time_font, &time_color));
        self.minute_label.set_markup(&markup(&format!("{:02}", now.minute()), &time_font, &time_color));
        self.second_label.set_markup(&markup(&format!("{:02}", now.second()), &time_font, &time_color));
        self.colon1_label.set_markup(&markup(":", &time_font, &time_color));
        self.colon2_label.set_markup(&markup(":", &time_font, &time_color));

        let day_key = DAY_KEYS[now.weekday().num_days_from_monday() as usize];
        let month_key = MONTH_KEYS[(now.month() - 1) as usize];
        let long = self.date_format_long.get();
        let day_name = i18n::t(&format!("widgets.clock.days_{}.{day_key}", if long { "long" } else { "short" }));
        let month_name = i18n::t(&format!("widgets.clock.months_{}.{month_key}", if long { "long" } else { "short" }));
        let day_num = now.day().to_string();
        let year = now.year().to_string();
        let date_text = if long {
            i18n::t_args("widgets.clock.date_long", &[("day", &day_name), ("day_num", &day_num), ("month", &month_name), ("year", &year)])
        } else {
            i18n::t_args("widgets.clock.date_short", &[("day", &day_name), ("day_num", &day_num), ("month", &month_name)])
        };
        self.date_label.set_markup(&markup(&date_text, &label_font, &label_color));
    }

    fn set_time_font_desc(&self, font_desc: gtk::pango::FontDescription) {
        *self.time_font_desc.borrow_mut() = font_desc;
        self.apply_segment_margins();
        self.refresh();
    }

    fn set_time_color(&self, rgba: gtk::gdk::RGBA) {
        *self.time_color.borrow_mut() = rgba;
        self.refresh();
    }

    fn set_label_font_desc(&self, font_desc: gtk::pango::FontDescription) {
        *self.label_font_desc.borrow_mut() = font_desc;
        self.refresh();
    }

    fn set_label_color(&self, rgba: gtk::gdk::RGBA) {
        *self.label_color.borrow_mut() = rgba;
        self.refresh();
    }

    fn set_timezone(&self, tz_name: &str, city_key: &str) {
        if let Ok(tz) = tz_name.parse::<chrono_tz::Tz>() {
            *self.timezone.borrow_mut() = tz;
            *self.city_key.borrow_mut() = city_key.to_string();
            self.refresh();
        }
    }

    fn set_date_format_long(&self, long: bool) {
        self.date_format_long.set(long);
        self.refresh();
    }

    fn set_hour_format_12h(&self, enabled: bool) {
        self.hour_format_12h.set(enabled);
        self.ampm_label.set_visible(enabled);
        self.refresh();
    }

    fn set_city_visible(&self, visible: bool) {
        self.city_visible.set(visible);
        self.city_label.set_visible(visible);
    }

    fn set_date_visible(&self, visible: bool) {
        self.date_visible.set(visible);
        self.date_label.set_visible(visible);
    }

    /// Back to this widget's out-of-the-box defaults - goes through the
    /// same setters as everything else (not a shortcut that pokes fields
    /// directly) so every side effect - segment margins, visibility,
    /// re-rendering - happens exactly like a normal edit would. Mirrors
    /// `ClockContent.reset()`; called from the appearance popover's reset
    /// button via `WidgetInstance::on_reset`, same as the Python original.
    fn reset(&self) {
        self.set_time_font_desc(gtk::pango::FontDescription::from_string(DEFAULT_TIME_FONT));
        self.set_time_color(hex_to_rgba(DEFAULT_TEXT_HEX));
        self.set_label_font_desc(gtk::pango::FontDescription::from_string(DEFAULT_LABEL_FONT));
        self.set_label_color(hex_to_rgba(DEFAULT_TEXT_HEX));
        let (default_city_key, default_tz) = CITIES[DEFAULT_CITY_INDEX];
        self.set_timezone(default_tz, default_city_key);
        self.set_date_format_long(false);
        self.set_hour_format_12h(false);
        self.set_city_visible(true);
        self.set_date_visible(true);
    }

    fn to_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "time_font": self.time_font_desc.borrow().to_string(),
            "time_color": rgba_to_hex(&self.time_color.borrow()),
            "label_font": self.label_font_desc.borrow().to_string(),
            "label_color": rgba_to_hex(&self.label_color.borrow()),
            "city_key": self.city_key.borrow().clone(),
            "date_format_long": self.date_format_long.get(),
            "hour_format_12h": self.hour_format_12h.get(),
            "city_visible": self.city_visible.get(),
            "date_visible": self.date_visible.get(),
        })
    }

    /// Only touches fields actually present, so a partial/older saved
    /// dict still applies cleanly - mirrors `ClockContent.apply_dict()`.
    fn apply_dict(&self, data: &serde_json::Value) {
        if let Some(v) = data.get("time_font").and_then(|v| v.as_str()) {
            self.set_time_font_desc(gtk::pango::FontDescription::from_string(v));
        }
        if let Some(v) = data.get("time_color").and_then(|v| v.as_str()) {
            self.set_time_color(hex_to_rgba(v));
        }
        if let Some(v) = data.get("label_font").and_then(|v| v.as_str()) {
            self.set_label_font_desc(gtk::pango::FontDescription::from_string(v));
        }
        if let Some(v) = data.get("label_color").and_then(|v| v.as_str()) {
            self.set_label_color(hex_to_rgba(v));
        }
        if let Some(city_key) = data.get("city_key").and_then(|v| v.as_str()) {
            if let Some((_, tz_name)) = CITIES.iter().find(|(key, _)| *key == city_key) {
                self.set_timezone(tz_name, city_key);
            }
        }
        if let Some(v) = data.get("date_format_long").and_then(|v| v.as_bool()) {
            self.set_date_format_long(v);
        }
        if let Some(v) = data.get("hour_format_12h").and_then(|v| v.as_bool()) {
            self.set_hour_format_12h(v);
        }
        if let Some(v) = data.get("city_visible").and_then(|v| v.as_bool()) {
            self.set_city_visible(v);
        }
        if let Some(v) = data.get("date_visible").and_then(|v| v.as_bool()) {
            self.set_date_visible(v);
        }
    }
}

use chrono::{Datelike, Timelike};

fn build_content() -> (Rc<ClockState>, gtk::Widget) {
    let root = gtk::CenterBox::new();
    root.set_orientation(gtk::Orientation::Vertical);
    root.set_halign(gtk::Align::Center);
    // valign stays FILL (the default) on purpose: the CenterBox needs the
    // widget's *entire* available height to center the segments row
    // against - see the equivalent comment in ClockContent.__init__.

    let city_label = gtk::Label::new(None);
    city_label.set_hexpand(true);
    city_label.set_halign(gtk::Align::Center);
    city_label.set_valign(gtk::Align::Center);
    root.set_start_widget(Some(&expand_wrapper(&city_label)));

    let segments_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    segments_box.set_halign(gtk::Align::Center);

    let make_segment = || {
        let label = gtk::Label::new(None);
        label.set_width_chars(2);
        label.set_justify(gtk::Justification::Center);
        let card = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        card.add_css_class("card");
        card.append(&label);
        (card, label)
    };
    let (hour_box, hour_label) = make_segment();
    let colon1_label = gtk::Label::new(Some(":"));
    let (minute_box, minute_label) = make_segment();
    let colon2_label = gtk::Label::new(Some(":"));
    let (second_box, second_label) = make_segment();
    let ampm_label = gtk::Label::new(None);
    ampm_label.set_valign(gtk::Align::Center);
    ampm_label.set_visible(false);
    let segment_children: [gtk::Widget; 6] = [
        hour_box.clone().upcast(),
        colon1_label.clone().upcast(),
        minute_box.clone().upcast(),
        colon2_label.clone().upcast(),
        second_box.clone().upcast(),
        ampm_label.clone().upcast(),
    ];
    for w in &segment_children {
        segments_box.append(w);
    }
    root.set_center_widget(Some(&segments_box));

    let date_label = gtk::Label::new(None);
    date_label.set_hexpand(true);
    date_label.set_halign(gtk::Align::Center);
    date_label.set_valign(gtk::Align::Center);
    root.set_end_widget(Some(&expand_wrapper(&date_label)));

    let (default_city_key, default_tz) = CITIES[DEFAULT_CITY_INDEX];
    let state = Rc::new(ClockState {
        city_label,
        hour_label,
        minute_label,
        second_label,
        colon1_label,
        colon2_label,
        ampm_label,
        date_label,
        time_font_desc: RefCell::new(gtk::pango::FontDescription::from_string(DEFAULT_TIME_FONT)),
        time_color: RefCell::new(hex_to_rgba(DEFAULT_TEXT_HEX)),
        label_font_desc: RefCell::new(gtk::pango::FontDescription::from_string(DEFAULT_LABEL_FONT)),
        label_color: RefCell::new(hex_to_rgba(DEFAULT_TEXT_HEX)),
        timezone: RefCell::new(default_tz.parse().expect("built-in timezone name")),
        city_key: RefCell::new(default_city_key.to_string()),
        date_format_long: Cell::new(false),
        hour_format_12h: Cell::new(false),
        city_visible: Cell::new(true),
        date_visible: Cell::new(true),
    });
    state.apply_segment_margins();
    state.refresh();

    let timeout_id = gtk::glib::timeout_add_seconds_local(1, {
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

fn build_settings(state: Rc<ClockState>) -> (gtk::Widget, Box<dyn Fn()>) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(260, -1);

    let time_font_label = gtk::Label::new(Some(&i18n::t("widgets.clock.settings.time_font")));
    time_font_label.set_halign(gtk::Align::Start);
    root.append(&time_font_label);
    let time_font_button = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
    time_font_button.set_level(gtk::FontLevel::Font);
    time_font_button.set_font_desc(&state.time_font_desc.borrow());
    time_font_button.set_hexpand(true);
    let time_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    time_color_button.set_rgba(&state.time_color.borrow());
    root.append(&make_row(&[time_font_button.upcast_ref(), time_color_button.upcast_ref()]));

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let label_font_label = gtk::Label::new(Some(&i18n::t("widgets.clock.settings.label_font")));
    label_font_label.set_halign(gtk::Align::Start);
    root.append(&label_font_label);
    let label_font_button = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
    label_font_button.set_level(gtk::FontLevel::Font);
    label_font_button.set_font_desc(&state.label_font_desc.borrow());
    label_font_button.set_hexpand(true);
    let label_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    label_color_button.set_rgba(&state.label_color.borrow());
    root.append(&make_row(&[label_font_button.upcast_ref(), label_color_button.upcast_ref()]));

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let city_label = gtk::Label::new(Some(&i18n::t("widgets.clock.settings.city")));
    city_label.set_halign(gtk::Align::Start);
    let city_visible_switch = gtk::Switch::new();
    city_visible_switch.set_active(state.city_visible.get());
    city_visible_switch.set_valign(gtk::Align::Center);
    let city_names: Vec<String> = CITIES.iter().map(|(key, _)| i18n::t(key)).collect();
    let city_dropdown = gtk::DropDown::new(
        Some(gtk::StringList::new(&city_names.iter().map(String::as_str).collect::<Vec<_>>())),
        gtk::Expression::NONE,
    );
    city_dropdown.set_hexpand(true);
    let current_city_index =
        CITIES.iter().position(|(key, _)| *key == *state.city_key.borrow()).unwrap_or(0);
    city_dropdown.set_selected(current_city_index as u32);
    root.append(&make_row(&[city_label.upcast_ref(), city_visible_switch.upcast_ref(), city_dropdown.upcast_ref()]));

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let date_label = gtk::Label::new(Some(&i18n::t("widgets.clock.settings.date")));
    date_label.set_hexpand(true);
    date_label.set_halign(gtk::Align::Start);
    let date_visible_switch = gtk::Switch::new();
    date_visible_switch.set_active(state.date_visible.get());
    date_visible_switch.set_valign(gtk::Align::Center);
    root.append(&make_row(&[date_label.upcast_ref(), date_visible_switch.upcast_ref()]));

    let date_format_label = gtk::Label::new(Some(&i18n::t("widgets.clock.settings.date_format")));
    date_format_label.set_hexpand(true);
    date_format_label.set_halign(gtk::Align::Start);
    let short_button = gtk::ToggleButton::with_label(&i18n::t("widgets.clock.settings.date_format_short"));
    let long_button = gtk::ToggleButton::with_label(&i18n::t("widgets.clock.settings.date_format_long"));
    long_button.set_group(Some(&short_button));
    short_button.set_active(!state.date_format_long.get());
    long_button.set_active(state.date_format_long.get());
    root.append(&make_row(&[date_format_label.upcast_ref(), short_button.upcast_ref(), long_button.upcast_ref()]));

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let hour_format_label = gtk::Label::new(Some(&i18n::t("widgets.clock.settings.hour_format")));
    hour_format_label.set_hexpand(true);
    hour_format_label.set_halign(gtk::Align::Start);
    let h24_button = gtk::ToggleButton::with_label(&i18n::t("widgets.clock.settings.hour_format_24h"));
    let h12_button = gtk::ToggleButton::with_label(&i18n::t("widgets.clock.settings.hour_format_12h"));
    h12_button.set_group(Some(&h24_button));
    h24_button.set_active(!state.hour_format_12h.get());
    h12_button.set_active(state.hour_format_12h.get());
    root.append(&make_row(&[hour_format_label.upcast_ref(), h24_button.upcast_ref(), h12_button.upcast_ref()]));

    // --- signal wiring: controls push one-way into `state` ---
    time_font_button.connect_font_desc_notify({
        let state = state.clone();
        move |b| {
            if let Some(desc) = b.font_desc() {
                state.set_time_font_desc(desc);
            }
        }
    });
    time_color_button.connect_rgba_notify({
        let state = state.clone();
        move |b| state.set_time_color(b.rgba())
    });
    label_font_button.connect_font_desc_notify({
        let state = state.clone();
        move |b| {
            if let Some(desc) = b.font_desc() {
                state.set_label_font_desc(desc);
            }
        }
    });
    label_color_button.connect_rgba_notify({
        let state = state.clone();
        move |b| state.set_label_color(b.rgba())
    });
    city_dropdown.connect_selected_notify({
        let state = state.clone();
        move |dropdown| {
            let index = dropdown.selected() as usize;
            if let Some((key, tz)) = CITIES.get(index) {
                state.set_timezone(tz, key);
            }
        }
    });
    city_visible_switch.connect_active_notify({
        let state = state.clone();
        move |s| state.set_city_visible(s.is_active())
    });
    date_visible_switch.connect_active_notify({
        let state = state.clone();
        move |s| state.set_date_visible(s.is_active())
    });
    long_button.connect_toggled({
        let state = state.clone();
        move |b| state.set_date_format_long(b.is_active())
    });
    h12_button.connect_toggled({
        let state = state.clone();
        move |b| state.set_hour_format_12h(b.is_active())
    });

    // --- retranslation: labels/tooltips + the city dropdown's own
    // translated entries (unlike the settings page's language row, these
    // city names really are ordinary translated strings) ---
    i18n::on_change({
        let city_dropdown = city_dropdown.clone();
        let time_font_label = time_font_label.clone();
        let label_font_label = label_font_label.clone();
        let city_label = city_label.clone();
        let date_label = date_label.clone();
        let date_format_label = date_format_label.clone();
        let short_button = short_button.clone();
        let long_button = long_button.clone();
        let hour_format_label = hour_format_label.clone();
        let h24_button = h24_button.clone();
        let h12_button = h12_button.clone();
        move || {
            time_font_label.set_label(&i18n::t("widgets.clock.settings.time_font"));
            label_font_label.set_label(&i18n::t("widgets.clock.settings.label_font"));
            city_label.set_label(&i18n::t("widgets.clock.settings.city"));
            date_label.set_label(&i18n::t("widgets.clock.settings.date"));
            date_format_label.set_label(&i18n::t("widgets.clock.settings.date_format"));
            short_button.set_label(&i18n::t("widgets.clock.settings.date_format_short"));
            long_button.set_label(&i18n::t("widgets.clock.settings.date_format_long"));
            hour_format_label.set_label(&i18n::t("widgets.clock.settings.hour_format"));
            h24_button.set_label(&i18n::t("widgets.clock.settings.hour_format_24h"));
            h12_button.set_label(&i18n::t("widgets.clock.settings.hour_format_12h"));
            let selected = city_dropdown.selected();
            let names: Vec<String> = CITIES.iter().map(|(key, _)| i18n::t(key)).collect();
            city_dropdown.set_model(Some(&gtk::StringList::new(&names.iter().map(String::as_str).collect::<Vec<_>>())));
            city_dropdown.set_selected(selected);
        }
    });

    // Re-reads every control's displayed value from `state` - needed
    // after `state.reset()` (called from the appearance popover's reset
    // button, see `on_reset` below) changes the model directly, since a
    // control otherwise only pushes edits one-way and doesn't notice a
    // programmatic change underneath it. Mirrors `sync_from_content()`.
    let resync: Box<dyn Fn()> = Box::new(move || {
        // Extracted into owned locals *before* touching any control:
        // each setter below fires that control's own "changed" signal
        // synchronously, which calls back into `state.set_*` - a
        // `borrow_mut()` on the very same RefCell a `.borrow()` here would
        // still be holding as a live temporary, which panics. Read
        // everything out first, then apply with no outstanding borrow.
        let time_font = state.time_font_desc.borrow().clone();
        let time_color = state.time_color.borrow().clone();
        let label_font = state.label_font_desc.borrow().clone();
        let label_color = state.label_color.borrow().clone();
        let city_index = CITIES.iter().position(|(key, _)| *key == *state.city_key.borrow()).unwrap_or(0);
        let city_visible = state.city_visible.get();
        let date_visible = state.date_visible.get();
        let date_format_long = state.date_format_long.get();
        let hour_format_12h = state.hour_format_12h.get();

        time_font_button.set_font_desc(&time_font);
        time_color_button.set_rgba(&time_color);
        label_font_button.set_font_desc(&label_font);
        label_color_button.set_rgba(&label_color);
        city_visible_switch.set_active(city_visible);
        city_dropdown.set_selected(city_index as u32);
        date_visible_switch.set_active(date_visible);
        short_button.set_active(!date_format_long);
        long_button.set_active(date_format_long);
        h24_button.set_active(!hour_format_12h);
        h12_button.set_active(hour_format_12h);
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
    }
}
