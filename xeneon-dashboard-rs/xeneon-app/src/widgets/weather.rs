//! Weather widget, step 1 of the port from `widgets/weather.py` (Python):
//! current conditions (icon, temperature, humidity/pressure/wind/UV index)
//! for a fixed default location (Paris - free-text city search, matching
//! `WeatherSettings` in the Python original, is step 2; the per-instance
//! content-size slider is step 3). Laid out as three centered columns -
//! temperature (large, bold) with the city name below; the condition icon
//! (large) with wind speed below; humidity/pressure/UV index stacked as
//! small icon+value rows - exactly mirroring the Python original's final
//! design (see CLAUDE.md's `xeneon_dashboard/widgets/weather.py`).
//!
//! Data comes from Open-Meteo (no API key). Unlike Clock or the audio
//! widget's MPRIS calls (local D-Bus, a synchronous call is cheap enough
//! to run on the main thread - see `audio.rs`'s own doc comment), this is
//! a real network round-trip with unpredictable latency, so it must never
//! block the GTK main thread. `gio::spawn_blocking` runs the blocking
//! `ureq` call on GIO's own thread pool (already a transitive dependency
//! of gtk4/libadwaita, no new crate beyond `ureq` itself for the HTTP call
//! - the user chose "ureq + thread" over reqwest/soup3 specifically to
//! avoid extra dependency weight), and `glib::spawn_future_local` awaits
//! that join handle back on the main thread's `GMainContext` - the
//! `.await` runs the rest of the closure right back on the GTK thread, so
//! touching `WeatherState`'s widgets afterward is safe. This is the first
//! `async`/`.await` in this codebase; it was chosen over a raw
//! `std::thread::spawn` + manual marshalling because this glib version no
//! longer has `MainContext::channel()` (removed upstream in favour of
//! exactly this futures-based pattern), and `MainContext::invoke()`
//! requires its closure to be `Send`, which an `Rc<WeatherState>` isn't -
//! `spawn_blocking`/`spawn_future_local` sidesteps both problems for free
//! using only glib/gio, already mandatory dependencies. A generation
//! counter (bumped on every new fetch) makes a still-in-flight response
//! from a superseded request a no-op when it lands - mirrors
//! `WeatherContent._fetch_generation` in the Python original.

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use crate::i18n_runtime as i18n;
use crate::widgets::registry::WidgetInstance;

const FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";
const REQUEST_TIMEOUT_SECONDS: u64 = 8;
const REFRESH_INTERVAL_SECONDS: u32 = 15 * 60;

const TEMP_FONT: &str = "Sans 72";
const CITY_FONT: &str = "Sans 18";
const STAT_FONT: &str = "Sans 15";

const ICON_PIXEL_SIZE: i32 = 104;
const STAT_ICON_PIXEL_SIZE: i32 = 18;
const COLUMN_SPACING: i32 = 32;
const TEMP_COLUMN_SPACING: i32 = 4;
const ICON_COLUMN_SPACING: i32 = 8;
const STATS_COLUMN_SPACING: i32 = 8;
const STAT_ROW_SPACING: i32 = 4;

/// WMO weather code (Open-Meteo's `current.weather_code`) -> (i18n
/// condition key suffix, day icon, night icon) - same grouping as the
/// Python original's `_WEATHER_CODES`: several adjacent codes share an
/// icon and are only distinguished by their condition text.
const WEATHER_CODES: &[(i64, &str, &str, &str)] = &[
    (0, "clear", "weather-clear-symbolic", "weather-clear-night-symbolic"),
    (1, "mainly_clear", "weather-few-clouds-symbolic", "weather-few-clouds-night-symbolic"),
    (2, "partly_cloudy", "weather-few-clouds-symbolic", "weather-few-clouds-night-symbolic"),
    (3, "overcast", "weather-overcast-symbolic", "weather-overcast-symbolic"),
    (45, "fog", "weather-fog-symbolic", "weather-fog-symbolic"),
    (48, "fog", "weather-fog-symbolic", "weather-fog-symbolic"),
    (51, "drizzle", "weather-showers-scattered-symbolic", "weather-showers-scattered-symbolic"),
    (53, "drizzle", "weather-showers-scattered-symbolic", "weather-showers-scattered-symbolic"),
    (55, "drizzle", "weather-showers-scattered-symbolic", "weather-showers-scattered-symbolic"),
    (56, "freezing_drizzle", "weather-showers-scattered-symbolic", "weather-showers-scattered-symbolic"),
    (57, "freezing_drizzle", "weather-showers-scattered-symbolic", "weather-showers-scattered-symbolic"),
    (61, "rain", "weather-showers-symbolic", "weather-showers-symbolic"),
    (63, "rain", "weather-showers-symbolic", "weather-showers-symbolic"),
    (65, "heavy_rain", "weather-showers-symbolic", "weather-showers-symbolic"),
    (66, "freezing_rain", "weather-showers-symbolic", "weather-showers-symbolic"),
    (67, "freezing_rain", "weather-showers-symbolic", "weather-showers-symbolic"),
    (71, "snow", "weather-snow-symbolic", "weather-snow-symbolic"),
    (73, "snow", "weather-snow-symbolic", "weather-snow-symbolic"),
    (75, "heavy_snow", "weather-snow-symbolic", "weather-snow-symbolic"),
    (77, "snow_grains", "weather-snow-symbolic", "weather-snow-symbolic"),
    (80, "rain_showers", "weather-showers-symbolic", "weather-showers-symbolic"),
    (81, "rain_showers", "weather-showers-symbolic", "weather-showers-symbolic"),
    (82, "heavy_rain_showers", "weather-showers-symbolic", "weather-showers-symbolic"),
    (85, "snow_showers", "weather-snow-symbolic", "weather-snow-symbolic"),
    (86, "snow_showers", "weather-snow-symbolic", "weather-snow-symbolic"),
    (95, "thunderstorm", "weather-storm-symbolic", "weather-storm-symbolic"),
    (96, "thunderstorm_hail", "weather-storm-symbolic", "weather-storm-symbolic"),
    (99, "thunderstorm_hail", "weather-storm-symbolic", "weather-storm-symbolic"),
];
const UNKNOWN_CODE: (&str, &str, &str) =
    ("unknown", "weather-severe-alert-symbolic", "weather-severe-alert-symbolic");

fn weather_code_info(code: i64) -> (&'static str, &'static str, &'static str) {
    WEATHER_CODES
        .iter()
        .find(|(c, ..)| *c == code)
        .map(|(_, key, day, night)| (*key, *day, *night))
        .unwrap_or(UNKNOWN_CODE)
}

fn markup(text: &str, font_desc: &str) -> String {
    format!(
        "<span font_desc=\"{}\" foreground=\"#ffffff\">{}</span>",
        glib::markup_escape_text(font_desc),
        glib::markup_escape_text(text)
    )
}

/// One geocoded place: name/region/country for display, lat/lon for the
/// forecast query. Mirrors the Python original's plain `dict` location -
/// step 2 (free-text search) will produce these from Open-Meteo's
/// geocoding endpoint; step 1 only ever uses `Location::default_paris()`.
#[derive(Clone)]
struct Location {
    name: String,
    admin1: String,
    country: String,
    latitude: f64,
    longitude: f64,
}

impl Location {
    /// Shown before the user ever picks a location (step 2), so a freshly
    /// added widget displays real weather instead of a blank placeholder -
    /// same default as the Python original's `DEFAULT_LOCATION`.
    fn default_paris() -> Self {
        Self {
            name: "Paris".to_string(),
            admin1: "Île-de-France".to_string(),
            country: "France".to_string(),
            latitude: 48.8566,
            longitude: 2.3522,
        }
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "admin1": self.admin1,
            "country": self.country,
            "latitude": self.latitude,
            "longitude": self.longitude,
        })
    }

    fn from_json(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            name: value.get("name")?.as_str()?.to_string(),
            admin1: value.get("admin1").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            country: value.get("country").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            latitude: value.get("latitude")?.as_f64()?,
            longitude: value.get("longitude")?.as_f64()?,
        })
    }
}

/// One successful fetch's worth of current conditions - a plain `Send`
/// data type (no GTK objects), since it's built on GIO's blocking thread
/// pool and has to cross back to the main thread.
struct CurrentWeather {
    temperature_c: f64,
    weather_code: i64,
    is_day: bool,
    humidity: Option<f64>,
    pressure: Option<f64>,
    wind_speed: Option<f64>,
    uv_index: Option<f64>,
}

/// Blocking HTTP GET + JSON parse - runs on GIO's thread pool via
/// `gio::spawn_blocking`, never on the GTK main thread. Mirrors
/// `_http_get_json`/`_fetch_weather` in the Python original.
fn fetch_current_weather(location: &Location) -> Result<CurrentWeather, String> {
    let mut response = ureq::get(FORECAST_URL)
        .query("latitude", location.latitude.to_string())
        .query("longitude", location.longitude.to_string())
        .query(
            "current",
            "temperature_2m,weather_code,is_day,relative_humidity_2m,surface_pressure,wind_speed_10m,uv_index",
        )
        .query("timezone", "auto")
        .config()
        .timeout_global(Some(Duration::from_secs(REQUEST_TIMEOUT_SECONDS)))
        .build()
        .call()
        .map_err(|e| e.to_string())?;

    let body = response.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let current = json.get("current").ok_or("missing \"current\" in response")?;

    let temperature_c = current
        .get("temperature_2m")
        .and_then(|v| v.as_f64())
        .ok_or("missing temperature_2m")?;
    let weather_code = current.get("weather_code").and_then(|v| v.as_i64()).unwrap_or(-1);
    let is_day = current.get("is_day").and_then(|v| v.as_i64()).map(|v| v != 0).unwrap_or(true);

    Ok(CurrentWeather {
        temperature_c,
        weather_code,
        is_day,
        humidity: current.get("relative_humidity_2m").and_then(|v| v.as_f64()),
        pressure: current.get("surface_pressure").and_then(|v| v.as_f64()),
        wind_speed: current.get("wind_speed_10m").and_then(|v| v.as_f64()),
        uv_index: current.get("uv_index").and_then(|v| v.as_f64()),
    })
}

#[derive(Clone, Copy, PartialEq)]
enum FetchStatus {
    Loading,
    Ok,
    Error,
}

struct WeatherState {
    temp_label: gtk::Label,
    city_label: gtk::Label,
    icon: gtk::Image,
    wind_label: gtk::Label,
    humidity_label: gtk::Label,
    pressure_label: gtk::Label,
    uv_label: gtk::Label,

    location: RefCell<Location>,
    unit_fahrenheit: Cell<bool>,
    status: Cell<FetchStatus>,
    current_celsius: Cell<Option<f64>>,
    weather_code: Cell<Option<i64>>,
    is_day: Cell<bool>,
    humidity: Cell<Option<f64>>,
    pressure: Cell<Option<f64>>,
    wind_speed: Cell<Option<f64>>,
    uv_index: Cell<Option<f64>>,
    fetch_generation: Cell<u64>,
}

fn fmt_stat(value: Option<f64>, suffix: &str, placeholder: &str) -> String {
    match value {
        Some(v) => format!("{}{}", v.round() as i64, suffix),
        None => placeholder.to_string(),
    }
}

impl WeatherState {
    fn set_location(&self, location: Location) {
        self.city_label.set_markup(&markup(&location.name, CITY_FONT));
        *self.location.borrow_mut() = location;
        self.status.set(FetchStatus::Loading);
        self.render();
    }

    fn set_unit_fahrenheit(&self, enabled: bool) {
        self.unit_fahrenheit.set(enabled);
        self.render();
    }

    fn render(&self) {
        match self.status.get() {
            FetchStatus::Ok => {
                let celsius = self.current_celsius.get().unwrap_or(0.0);
                let value =
                    if self.unit_fahrenheit.get() { celsius * 9.0 / 5.0 + 32.0 } else { celsius };
                let unit_symbol = if self.unit_fahrenheit.get() { "°F" } else { "°C" };
                self.temp_label
                    .set_markup(&markup(&format!("{}{}", value.round() as i64, unit_symbol), TEMP_FONT));

                let code = self.weather_code.get().unwrap_or(-1);
                let (condition_key, day_icon, night_icon) = weather_code_info(code);
                self.icon.set_icon_name(Some(if self.is_day.get() { day_icon } else { night_icon }));
                self.icon
                    .set_tooltip_text(Some(&i18n::t(&format!("widgets.weather.conditions.{condition_key}"))));

                self.humidity_label.set_markup(&markup(&fmt_stat(self.humidity.get(), "%", "--%"), STAT_FONT));
                self.pressure_label
                    .set_markup(&markup(&fmt_stat(self.pressure.get(), " hPa", "-- hPa"), STAT_FONT));
                self.wind_label
                    .set_markup(&markup(&fmt_stat(self.wind_speed.get(), " km/h", "-- km/h"), STAT_FONT));
                self.uv_label.set_markup(&markup(&fmt_stat(self.uv_index.get(), "", "--"), STAT_FONT));
            }
            FetchStatus::Loading | FetchStatus::Error => {
                let is_error = self.status.get() == FetchStatus::Error;
                let icon_name = if is_error { "weather-severe-alert-symbolic" } else { "weather-clear-symbolic" };
                let tooltip_key = if is_error { "widgets.weather.error" } else { "widgets.weather.loading" };

                self.temp_label.set_markup(&markup("--°", TEMP_FONT));
                self.icon.set_icon_name(Some(icon_name));
                self.icon.set_tooltip_text(Some(&i18n::t(tooltip_key)));
                self.humidity_label.set_markup(&markup("--%", STAT_FONT));
                self.pressure_label.set_markup(&markup("-- hPa", STAT_FONT));
                self.wind_label.set_markup(&markup("-- km/h", STAT_FONT));
                self.uv_label.set_markup(&markup("--", STAT_FONT));
            }
        }
    }

    fn to_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "location": self.location.borrow().to_json(),
            "unit_fahrenheit": self.unit_fahrenheit.get(),
        })
    }

    /// Only touches fields actually present, so a partial/older saved
    /// dict still applies cleanly - mirrors `WeatherContent.apply_dict()`.
    fn apply_dict(self: &Rc<Self>, data: &serde_json::Value) {
        if let Some(location) = data.get("location").and_then(Location::from_json) {
            self.set_location(location);
            trigger_fetch(self);
        }
        if let Some(v) = data.get("unit_fahrenheit").and_then(|v| v.as_bool()) {
            self.set_unit_fahrenheit(v);
        }
    }
}

/// Kicks off a background fetch for `state`'s current location, bumping
/// the generation counter first so any response still in flight from a
/// previous call becomes a no-op when it lands (a superseded location, or
/// the widget having been torn down and this `Rc` only kept alive by the
/// future itself). See the module doc comment for why this is
/// `spawn_blocking` + `spawn_future_local` rather than a raw thread.
fn trigger_fetch(state: &Rc<WeatherState>) {
    let generation = state.fetch_generation.get() + 1;
    state.fetch_generation.set(generation);
    state.status.set(FetchStatus::Loading);
    state.render();

    let location = state.location.borrow().clone();
    let state = state.clone();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || fetch_current_weather(&location)).await;
        if generation != state.fetch_generation.get() {
            return;
        }
        match result {
            Ok(Ok(weather)) => {
                state.current_celsius.set(Some(weather.temperature_c));
                state.weather_code.set(Some(weather.weather_code));
                state.is_day.set(weather.is_day);
                state.humidity.set(weather.humidity);
                state.pressure.set(weather.pressure);
                state.wind_speed.set(weather.wind_speed);
                state.uv_index.set(weather.uv_index);
                state.status.set(FetchStatus::Ok);
            }
            _ => state.status.set(FetchStatus::Error),
        }
        state.render();
    });
}

fn make_stat_row(icon_name: &str) -> (gtk::Box, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, STAT_ROW_SPACING);
    row.set_halign(gtk::Align::Start);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(STAT_ICON_PIXEL_SIZE);
    row.append(&icon);
    let label = gtk::Label::new(None);
    row.append(&label);
    (row, label)
}

fn build_content() -> (Rc<WeatherState>, gtk::Widget) {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, COLUMN_SPACING);
    root.set_halign(gtk::Align::Center);
    root.set_valign(gtk::Align::Center);

    let temp_label = gtk::Label::new(None);
    let city_label = gtk::Label::new(None);
    let temp_column = gtk::Box::new(gtk::Orientation::Vertical, TEMP_COLUMN_SPACING);
    temp_column.set_valign(gtk::Align::Center);
    temp_column.append(&temp_label);
    temp_column.append(&city_label);
    root.append(&temp_column);

    let icon = gtk::Image::new();
    icon.set_pixel_size(ICON_PIXEL_SIZE);
    let (wind_row, wind_label) = make_stat_row("weather-windy-symbolic");
    let icon_column = gtk::Box::new(gtk::Orientation::Vertical, ICON_COLUMN_SPACING);
    icon_column.set_halign(gtk::Align::Center);
    icon_column.set_valign(gtk::Align::Center);
    icon_column.append(&icon);
    icon_column.append(&wind_row);
    root.append(&icon_column);

    let (humidity_row, humidity_label) = make_stat_row("weather-showers-symbolic");
    let (pressure_row, pressure_label) = make_stat_row("speedometer-symbolic");
    let (uv_row, uv_label) = make_stat_row("brightness-high-symbolic");
    let stats_column = gtk::Box::new(gtk::Orientation::Vertical, STATS_COLUMN_SPACING);
    stats_column.set_valign(gtk::Align::Center);
    stats_column.append(&humidity_row);
    stats_column.append(&pressure_row);
    stats_column.append(&uv_row);
    root.append(&stats_column);

    let default_location = Location::default_paris();
    city_label.set_markup(&markup(&default_location.name, CITY_FONT));

    let state = Rc::new(WeatherState {
        temp_label,
        city_label,
        icon,
        wind_label,
        humidity_label,
        pressure_label,
        uv_label,
        location: RefCell::new(default_location),
        unit_fahrenheit: Cell::new(false),
        status: Cell::new(FetchStatus::Loading),
        current_celsius: Cell::new(None),
        weather_code: Cell::new(None),
        is_day: Cell::new(true),
        humidity: Cell::new(None),
        pressure: Cell::new(None),
        wind_speed: Cell::new(None),
        uv_index: Cell::new(None),
        fetch_generation: Cell::new(0),
    });
    state.render();
    trigger_fetch(&state);

    let timeout_id = glib::timeout_add_seconds_local(REFRESH_INTERVAL_SECONDS, {
        let state = state.clone();
        move || {
            trigger_fetch(&state);
            glib::ControlFlow::Continue
        }
    });
    root.connect_destroy({
        let timeout_id = RefCell::new(Some(timeout_id));
        let state = state.clone();
        move |_| {
            if let Some(id) = timeout_id.borrow_mut().take() {
                id.remove();
            }
            // Any fetch still in flight will see a generation mismatch
            // (nothing else bumps it after this) and skip touching these
            // widgets once its `spawn_future_local` future resumes.
            state.fetch_generation.set(state.fetch_generation.get() + 1);
        }
    });
    i18n::on_change({
        let state = state.clone();
        move || state.render()
    });

    (state, root.upcast())
}

pub fn spawn() -> WidgetInstance {
    let (state, content) = build_content();
    WidgetInstance {
        content,
        settings: None,
        to_dict: Box::new(move || state.to_dict()),
        on_reset: None,
    }
}

pub fn restore(data: &serde_json::Value) -> WidgetInstance {
    let (state, content) = build_content();
    state.apply_dict(data);
    WidgetInstance {
        content,
        settings: None,
        to_dict: Box::new(move || state.to_dict()),
        on_reset: None,
    }
}
