// SPDX-License-Identifier: GPL-3.0-or-later
//! Weather widget, full port of `widgets/weather.py`: current conditions
//! (icon, temperature, humidity/pressure/wind/UV index), `WeatherSettings`
//! (free-text city search via Open-Meteo's geocoding endpoint, a °C/°F
//! toggle, and a content-size slider), laid out as three centered columns
//! - temperature (large, bold) with the city name below; the condition
//! icon (large) with wind speed below; humidity/pressure/UV index stacked
//! as small icon+value rows - exactly mirroring the Python original's
//! final design (see CLAUDE.md's `xeneon_dashboard/widgets/weather.py`).
//!
//! Every font, icon and gap grows or shrinks together off
//! `WeatherState::content_scale` (see `apply_content_scale`) - the same
//! technique `temp_gauge.rs` borrowed from this same Python widget (see
//! that file's own doc comment), just realized differently here: since
//! this widget already renders its text as inline Pango markup spans
//! (`markup()`) rather than through `add_css_class`-styled `Label`s, the
//! font size is just a number baked into that markup string on every
//! `render()` - no per-instance CSS provider/class plumbing needed at all
//! for the text (unlike `temp_gauge.rs`'s `GaugeCss`, or the Python
//! original's own `_rules`/`_reload_css`). Icon pixel sizes and box
//! spacing still scale imperatively via `set_pixel_size`/`set_spacing`,
//! same as everywhere else this technique is used.
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

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use log::{debug, warn};
use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use crate::i18n_runtime as i18n;
use crate::widgets::registry::WidgetInstance;

const FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";
const GEOCODING_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
const GEOCODING_RESULT_COUNT: u32 = 8;
const REQUEST_TIMEOUT_SECONDS: u64 = 8;
const REFRESH_INTERVAL_SECONDS: u32 = 15 * 60;

// Every size below is at `content_scale == 1.0` (100%) - font points, icon
// pixel sizes, and box spacing all scale off these together (see
// `WeatherState::apply_content_scale`), so "bigger" grows the whole
// layout in proportion instead of just the text.
const BASE_TEMP_FONT_PT: f64 = 72.0;
const BASE_CITY_FONT_PT: f64 = 18.0;
const BASE_STAT_FONT_PT: f64 = 15.0;

const BASE_ICON_PIXEL_SIZE: f64 = 104.0;
const BASE_STAT_ICON_PIXEL_SIZE: f64 = 18.0;
const BASE_COLUMN_SPACING: f64 = 32.0;
const BASE_TEMP_COLUMN_SPACING: f64 = 4.0;
const BASE_ICON_COLUMN_SPACING: f64 = 8.0;
const BASE_STATS_COLUMN_SPACING: f64 = 8.0;
const BASE_STAT_ROW_SPACING: f64 = 4.0;

const MIN_CONTENT_SCALE: f64 = 0.5;
const MAX_CONTENT_SCALE: f64 = 2.0;
const DEFAULT_CONTENT_SCALE: f64 = 1.0;

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

fn markup(text: &str, base_pt: f64, scale: f64) -> String {
    let font_desc = format!("Sans {}", (base_pt * scale).round() as i32);
    format!(
        "<span font_desc=\"{}\" foreground=\"#ffffff\">{}</span>",
        glib::markup_escape_text(&font_desc),
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

/// "City, Region, Country" - skips a region that just repeats the city
/// name (common for big cities in the geocoding API's results) and any
/// part that's empty, so it degrades gracefully for an older saved
/// location missing some of these fields. Mirrors the Python original's
/// `format_location()`.
fn format_location(location: &Location) -> String {
    let mut parts = vec![location.name.as_str()];
    if !location.admin1.is_empty() && location.admin1 != location.name {
        parts.push(&location.admin1);
    }
    if !location.country.is_empty() {
        parts.push(&location.country);
    }
    parts.join(", ")
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

/// Blocking geocoding lookup (city name -> candidate locations) - runs on
/// GIO's thread pool via `gio::spawn_blocking`, same as
/// `fetch_current_weather`. Mirrors `WeatherSettings._on_search` in the
/// Python original.
fn search_locations(query: &str, language: &str) -> Result<Vec<Location>, String> {
    let mut response = ureq::get(GEOCODING_URL)
        .query("name", query)
        .query("count", GEOCODING_RESULT_COUNT.to_string())
        .query("language", language)
        .query("format", "json")
        .config()
        .timeout_global(Some(Duration::from_secs(REQUEST_TIMEOUT_SECONDS)))
        .build()
        .call()
        .map_err(|e| e.to_string())?;

    let body = response.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let results = json.get("results").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    Ok(results
        .iter()
        .filter_map(|entry| {
            Some(Location {
                name: entry.get("name")?.as_str()?.to_string(),
                admin1: entry.get("admin1").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                country: entry.get("country").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                latitude: entry.get("latitude")?.as_f64()?,
                longitude: entry.get("longitude")?.as_f64()?,
            })
        })
        .collect())
}

#[derive(Clone, Copy, PartialEq)]
enum FetchStatus {
    Loading,
    Ok,
    Error,
}

struct WeatherState {
    root: gtk::Box,
    temp_column: gtk::Box,
    icon_column: gtk::Box,
    stats_column: gtk::Box,

    temp_label: gtk::Label,
    city_label: gtk::Label,
    icon: gtk::Image,
    wind_row: gtk::Box,
    wind_icon: gtk::Image,
    wind_label: gtk::Label,
    humidity_row: gtk::Box,
    humidity_icon: gtk::Image,
    humidity_label: gtk::Label,
    pressure_row: gtk::Box,
    pressure_icon: gtk::Image,
    pressure_label: gtk::Label,
    uv_row: gtk::Box,
    uv_icon: gtk::Image,
    uv_label: gtk::Label,

    location: RefCell<Location>,
    unit_fahrenheit: Cell<bool>,
    content_scale: Cell<f64>,
    app_id: RefCell<Option<String>>,
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

/// Looks a saved `app_id` back up in the live installed-apps list - same
/// approach as `ShortcutIcon::app_info()` in shortcuts.rs (see that
/// module's doc comment: this project's `gio` binding has no
/// `DesktopAppInfo::new(id)` shortcut, so a saved id is resolved by
/// scanning `gio::AppInfo::all()` instead). `None` once the app has been
/// uninstalled since it was picked.
fn app_display_name(app_id: &str) -> Option<String> {
    gio::AppInfo::all()
        .into_iter()
        .find(|a| a.id().as_deref() == Some(app_id))
        .map(|a| a.display_name().to_string())
}

impl WeatherState {
    fn set_location(&self, location: Location) {
        debug!("location set to {}", format_location(&location));
        *self.location.borrow_mut() = location;
        self.refresh_city_label();
        self.status.set(FetchStatus::Loading);
        self.render();
    }

    fn set_unit_fahrenheit(&self, enabled: bool) {
        self.unit_fahrenheit.set(enabled);
        self.render();
    }

    fn set_app_id(&self, app_id: Option<String>) {
        *self.app_id.borrow_mut() = app_id;
        self.refresh_temp_interactivity();
    }

    /// The temperature is only clickable/hinted when an app is actually
    /// configured to launch - an unclickable label showing a "click for
    /// more info" tooltip would be a lie. Re-applied on every language
    /// change too, since the tooltip text is translated.
    fn refresh_temp_interactivity(&self) {
        if self.app_id.borrow().is_some() {
            self.temp_label.set_cursor_from_name(Some("pointer"));
            self.temp_label.set_tooltip_text(Some(&i18n::t("widgets.weather.more_info")));
        } else {
            self.temp_label.set_cursor_from_name(None);
            self.temp_label.set_tooltip_text(None);
        }
    }

    fn launch_app(&self) {
        let Some(app_id) = self.app_id.borrow().clone() else { return };
        let Some(info) = gio::AppInfo::all().into_iter().find(|a| a.id().as_deref() == Some(app_id.as_str())) else {
            warn!("weather app {app_id} is no longer installed");
            return;
        };
        if let Err(err) = info.launch(&[], None::<&gio::AppLaunchContext>) {
            warn!("failed to launch weather app {app_id}: {err}");
        }
    }

    fn refresh_city_label(&self) {
        let scale = self.content_scale.get();
        self.city_label.set_markup(&markup(&self.location.borrow().name, BASE_CITY_FONT_PT, scale));
    }

    /// Grows or shrinks every font, icon and gap together - see the
    /// module doc comment for why text goes through `markup()`'s own
    /// scale-aware font size rather than a CSS class like `temp_gauge.rs`.
    fn set_content_scale(&self, scale: f64) {
        self.content_scale.set(scale.clamp(MIN_CONTENT_SCALE, MAX_CONTENT_SCALE));
        self.apply_content_scale();
    }

    fn apply_content_scale(&self) {
        let scale = self.content_scale.get();
        self.icon.set_pixel_size((BASE_ICON_PIXEL_SIZE * scale).round() as i32);
        for icon in [&self.wind_icon, &self.humidity_icon, &self.pressure_icon, &self.uv_icon] {
            icon.set_pixel_size((BASE_STAT_ICON_PIXEL_SIZE * scale).round() as i32);
        }
        self.root.set_spacing((BASE_COLUMN_SPACING * scale).round() as i32);
        self.temp_column.set_spacing((BASE_TEMP_COLUMN_SPACING * scale).round() as i32);
        self.icon_column.set_spacing((BASE_ICON_COLUMN_SPACING * scale).round() as i32);
        self.stats_column.set_spacing((BASE_STATS_COLUMN_SPACING * scale).round() as i32);
        for row in [&self.wind_row, &self.humidity_row, &self.pressure_row, &self.uv_row] {
            row.set_spacing((BASE_STAT_ROW_SPACING * scale).round() as i32);
        }
        self.refresh_city_label();
        self.render();
    }

    fn render(&self) {
        let scale = self.content_scale.get();
        match self.status.get() {
            FetchStatus::Ok => {
                let celsius = self.current_celsius.get().unwrap_or(0.0);
                let value =
                    if self.unit_fahrenheit.get() { celsius * 9.0 / 5.0 + 32.0 } else { celsius };
                let unit_symbol = if self.unit_fahrenheit.get() { "°F" } else { "°C" };
                self.temp_label.set_markup(&markup(
                    &format!("{}{}", value.round() as i64, unit_symbol),
                    BASE_TEMP_FONT_PT,
                    scale,
                ));

                let code = self.weather_code.get().unwrap_or(-1);
                let (condition_key, day_icon, night_icon) = weather_code_info(code);
                self.icon.set_icon_name(Some(if self.is_day.get() { day_icon } else { night_icon }));
                self.icon
                    .set_tooltip_text(Some(&i18n::t(&format!("widgets.weather.conditions.{condition_key}"))));

                self.humidity_label.set_markup(&markup(&fmt_stat(self.humidity.get(), "%", "--%"), BASE_STAT_FONT_PT, scale));
                self.pressure_label.set_markup(&markup(
                    &fmt_stat(self.pressure.get(), " hPa", "-- hPa"),
                    BASE_STAT_FONT_PT,
                    scale,
                ));
                self.wind_label.set_markup(&markup(
                    &fmt_stat(self.wind_speed.get(), " km/h", "-- km/h"),
                    BASE_STAT_FONT_PT,
                    scale,
                ));
                self.uv_label.set_markup(&markup(&fmt_stat(self.uv_index.get(), "", "--"), BASE_STAT_FONT_PT, scale));
            }
            FetchStatus::Loading | FetchStatus::Error => {
                let is_error = self.status.get() == FetchStatus::Error;
                let icon_name = if is_error { "weather-severe-alert-symbolic" } else { "weather-clear-symbolic" };
                let tooltip_key = if is_error { "widgets.weather.error" } else { "widgets.weather.loading" };

                self.temp_label.set_markup(&markup("--°", BASE_TEMP_FONT_PT, scale));
                self.icon.set_icon_name(Some(icon_name));
                self.icon.set_tooltip_text(Some(&i18n::t(tooltip_key)));
                self.humidity_label.set_markup(&markup("--%", BASE_STAT_FONT_PT, scale));
                self.pressure_label.set_markup(&markup("-- hPa", BASE_STAT_FONT_PT, scale));
                self.wind_label.set_markup(&markup("-- km/h", BASE_STAT_FONT_PT, scale));
                self.uv_label.set_markup(&markup("--", BASE_STAT_FONT_PT, scale));
            }
        }
    }

    fn to_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "location": self.location.borrow().to_json(),
            "unit_fahrenheit": self.unit_fahrenheit.get(),
            "content_scale": self.content_scale.get(),
            "app_id": self.app_id.borrow().clone(),
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
        if let Some(v) = data.get("content_scale").and_then(|v| v.as_f64()) {
            self.set_content_scale(v);
        }
        if let Some(v) = data.get("app_id").and_then(|v| v.as_str()) {
            self.set_app_id(Some(v.to_string()));
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
    let location_name = location.name.clone();
    let state = state.clone();
    debug!("fetching weather for {location_name}");
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || fetch_current_weather(&location)).await;
        if generation != state.fetch_generation.get() {
            debug!("weather fetch for {location_name} superseded, discarding");
            return;
        }
        match result {
            Ok(Ok(weather)) => {
                debug!(
                    "weather fetch for {location_name} ok: {:.1}°C code={} humidity={:?} pressure={:?} wind={:?} uv={:?}",
                    weather.temperature_c, weather.weather_code, weather.humidity, weather.pressure, weather.wind_speed, weather.uv_index
                );
                state.current_celsius.set(Some(weather.temperature_c));
                state.weather_code.set(Some(weather.weather_code));
                state.is_day.set(weather.is_day);
                state.humidity.set(weather.humidity);
                state.pressure.set(weather.pressure);
                state.wind_speed.set(weather.wind_speed);
                state.uv_index.set(weather.uv_index);
                state.status.set(FetchStatus::Ok);
            }
            // The underlying error (network failure, bad JSON, missing
            // field...) used to be discarded entirely here - nothing
            // told you *why* the widget was stuck showing "--°".
            Ok(Err(err)) => {
                warn!("weather fetch for {location_name} failed: {err}");
                state.status.set(FetchStatus::Error);
            }
            Err(_) => {
                warn!("weather fetch for {location_name} task panicked");
                state.status.set(FetchStatus::Error);
            }
        }
        state.render();
    });
}

fn make_stat_row(icon_name: &str) -> (gtk::Box, gtk::Image, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.set_halign(gtk::Align::Start);
    let icon = gtk::Image::from_icon_name(icon_name);
    row.append(&icon);
    let label = gtk::Label::new(None);
    row.append(&label);
    (row, icon, label)
}

fn build_content() -> (Rc<WeatherState>, gtk::Widget) {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.set_halign(gtk::Align::Center);
    root.set_valign(gtk::Align::Center);

    let temp_label = gtk::Label::new(None);
    let city_label = gtk::Label::new(None);
    let temp_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    temp_column.set_valign(gtk::Align::Center);
    temp_column.append(&temp_label);
    temp_column.append(&city_label);
    root.append(&temp_column);

    let icon = gtk::Image::new();
    let (wind_row, wind_icon, wind_label) = make_stat_row("weather-windy-symbolic");
    let icon_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    icon_column.set_halign(gtk::Align::Center);
    icon_column.set_valign(gtk::Align::Center);
    icon_column.append(&icon);
    icon_column.append(&wind_row);
    root.append(&icon_column);

    let (humidity_row, humidity_icon, humidity_label) = make_stat_row("weather-showers-symbolic");
    let (pressure_row, pressure_icon, pressure_label) = make_stat_row("speedometer-symbolic");
    let (uv_row, uv_icon, uv_label) = make_stat_row("brightness-high-symbolic");
    let stats_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    stats_column.set_valign(gtk::Align::Center);
    stats_column.append(&humidity_row);
    stats_column.append(&pressure_row);
    stats_column.append(&uv_row);
    root.append(&stats_column);

    let default_location = Location::default_paris();

    let state = Rc::new(WeatherState {
        root: root.clone(),
        temp_column,
        icon_column,
        stats_column,
        temp_label,
        city_label,
        icon,
        wind_row,
        wind_icon,
        wind_label,
        humidity_row,
        humidity_icon,
        humidity_label,
        pressure_row,
        pressure_icon,
        pressure_label,
        uv_row,
        uv_icon,
        uv_label,
        location: RefCell::new(default_location),
        unit_fahrenheit: Cell::new(false),
        content_scale: Cell::new(DEFAULT_CONTENT_SCALE),
        app_id: RefCell::new(None),
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
    state.apply_content_scale();
    state.refresh_temp_interactivity();
    trigger_fetch(&state);

    // Simple GestureClick, no drag distance to watch - same reasoning as
    // shortcuts.rs's own icon-launch gesture: the whole widget's own move
    // button is a separate control, so a plain click here is unambiguous.
    // launch_app() itself is a no-op when no app is configured.
    let temp_click = gtk::GestureClick::new();
    temp_click.connect_released({
        let state = state.clone();
        move |_, _, _, _| state.launch_app()
    });
    state.temp_label.add_controller(temp_click);

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
        move || {
            state.render();
            state.refresh_temp_interactivity();
        }
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

/// Modal "pick an installed app" dialog - a trimmed copy of
/// `shortcuts.rs::open_add_dialog`'s own app-browsing half (search entry +
/// icon list), not shared with it: this project's widgets each keep their
/// own compact picker UI rather than factor out one shared dialog for two
/// call sites - same call `temp_gauge.rs`'s doc comment makes for its own
/// duplicated sensor picker ("not worth the extra indirection"). Weather
/// only ever needs "pick one app to launch", none of `open_add_dialog`'s
/// custom command/URL tab or col/row placement.
fn open_app_picker(parent: &gtk::Window, on_pick: impl Fn(String) + 'static) {
    let dialog = adw::Window::new();
    dialog.set_transient_for(Some(parent));
    dialog.set_modal(true);
    dialog.set_default_size(380, 480);
    dialog.set_title(Some(&i18n::t("widgets.weather.settings.app_picker_title")));

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&adw::HeaderBar::new());

    let root_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root_box.set_margin_start(12);
    root_box.set_margin_end(12);
    root_box.set_margin_top(6);
    root_box.set_margin_bottom(12);

    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some(&i18n::t("widgets.weather.settings.app_search_placeholder")));
    root_box.append(&search_entry);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_vexpand(true);
    scroller.set_min_content_height(280);
    let apps_list = gtk::ListBox::new();
    apps_list.add_css_class("boxed-list");
    apps_list.set_selection_mode(gtk::SelectionMode::None);
    apps_list.set_activate_on_single_click(true);
    scroller.set_child(Some(&apps_list));
    root_box.append(&scroller);

    toolbar_view.set_content(Some(&root_box));
    dialog.set_content(Some(&toolbar_view));

    let mut all_apps: Vec<gio::AppInfo> = gio::AppInfo::all().into_iter().filter(|a| a.should_show()).collect();
    all_apps.sort_by_key(|a| a.display_name().to_string().to_lowercase());
    let all_apps = Rc::new(all_apps);
    // Rebuilt by `populate` on every keystroke, in the same order as the
    // rows currently shown - `row.index()` in `row-activated` looks back
    // into this to find which app was picked (see
    // `shortcuts.rs::open_add_dialog` for the same reasoning).
    let visible_apps: Rc<RefCell<Vec<gio::AppInfo>>> = Rc::new(RefCell::new(Vec::new()));

    let populate = {
        let apps_list = apps_list.clone();
        let all_apps = all_apps.clone();
        let visible_apps = visible_apps.clone();
        move |query: &str| {
            while let Some(child) = apps_list.first_child() {
                apps_list.remove(&child);
            }
            let query = query.trim().to_lowercase();
            let mut visible = Vec::new();
            for app in all_apps.iter() {
                let name = app.display_name();
                if !query.is_empty() && !name.to_lowercase().contains(&query) {
                    continue;
                }
                let row = adw::ActionRow::new();
                row.set_title(&gtk::glib::markup_escape_text(&name));
                row.set_activatable(true);
                if let Some(icon) = app.icon() {
                    let image = gtk::Image::from_gicon(&icon);
                    image.set_pixel_size(28);
                    row.add_prefix(&image);
                }
                apps_list.append(&row);
                visible.push(app.clone());
            }
            *visible_apps.borrow_mut() = visible;
        }
    };
    populate("");

    search_entry.connect_search_changed({
        let populate = populate.clone();
        move |entry| populate(&entry.text())
    });

    apps_list.connect_row_activated({
        let visible_apps = visible_apps.clone();
        let dialog = dialog.clone();
        move |_, row| {
            let index = row.index();
            if index < 0 {
                return;
            }
            if let Some(app) = visible_apps.borrow().get(index as usize) {
                if let Some(id) = app.id() {
                    on_pick(id.to_string());
                }
            }
            dialog.close();
        }
    });

    dialog.present();
}

fn current_app_display(state: &WeatherState) -> String {
    match state.app_id.borrow().as_deref() {
        Some(id) => app_display_name(id).unwrap_or_else(|| id.to_string()),
        None => i18n::t("widgets.weather.settings.app_none"),
    }
}

/// The weather widget's own settings, shown next to the generic
/// appearance controls in the same configure popover (see
/// `clock.rs::build_settings` for the reference layout this follows): a
/// free-text city search (queries Open-Meteo's geocoding endpoint, see
/// `search_locations`) and a °C/°F unit toggle. No resync/`on_reset` is
/// wired - nothing outside these controls' own signal handlers ever
/// mutates `state.location`/`unit_fahrenheit` (matches the Python
/// original: `_spawn_weather` never passes `on_reset` either).
fn build_settings(state: Rc<WeatherState>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(280, -1);

    let location_title = gtk::Label::new(Some(&i18n::t("widgets.weather.settings.location")));
    location_title.set_halign(gtk::Align::Start);
    root.append(&location_title);

    let current_location_label = gtk::Label::new(Some(&format_location(&state.location.borrow())));
    current_location_label.set_halign(gtk::Align::Start);
    current_location_label.set_wrap(true);
    current_location_label.add_css_class("dim-label");
    root.append(&current_location_label);

    let search_entry = gtk::SearchEntry::new();
    search_entry.set_hexpand(true);
    let search_button = gtk::Button::with_label(&i18n::t("widgets.weather.settings.search_button"));
    root.append(&make_row(&[search_entry.upcast_ref(), search_button.upcast_ref()]));

    let status_label = gtk::Label::new(None);
    status_label.set_halign(gtk::Align::Start);
    status_label.add_css_class("dim-label");
    status_label.set_visible(false);
    root.append(&status_label);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_min_content_height(160);
    scroller.set_max_content_height(160);
    scroller.set_vexpand(false);
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    let results_list = gtk::ListBox::new();
    results_list.add_css_class("boxed-list");
    results_list.set_selection_mode(gtk::SelectionMode::None);
    results_list.set_activate_on_single_click(true);
    scroller.set_child(Some(&results_list));
    root.append(&scroller);

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let unit_label = gtk::Label::new(Some(&i18n::t("widgets.weather.settings.unit")));
    unit_label.set_hexpand(true);
    unit_label.set_halign(gtk::Align::Start);
    let celsius_button = gtk::ToggleButton::with_label(&i18n::t("widgets.weather.settings.unit_celsius"));
    let fahrenheit_button = gtk::ToggleButton::with_label(&i18n::t("widgets.weather.settings.unit_fahrenheit"));
    fahrenheit_button.set_group(Some(&celsius_button));
    celsius_button.set_active(!state.unit_fahrenheit.get());
    fahrenheit_button.set_active(state.unit_fahrenheit.get());
    root.append(&make_row(&[unit_label.upcast_ref(), celsius_button.upcast_ref(), fahrenheit_button.upcast_ref()]));

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let scale_label = gtk::Label::new(Some(&i18n::t("widgets.weather.settings.content_scale")));
    scale_label.set_halign(gtk::Align::Start);
    root.append(&scale_label);
    let scale_slider =
        gtk::Scale::with_range(gtk::Orientation::Horizontal, MIN_CONTENT_SCALE * 100.0, MAX_CONTENT_SCALE * 100.0, 1.0);
    scale_slider.set_value(state.content_scale.get() * 100.0);
    scale_slider.set_draw_value(true);
    scale_slider.set_value_pos(gtk::PositionType::Right);
    root.append(&scale_slider);

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let app_title = gtk::Label::new(Some(&i18n::t("widgets.weather.settings.app")));
    app_title.set_halign(gtk::Align::Start);
    root.append(&app_title);

    let app_name_label = gtk::Label::new(Some(&current_app_display(&state)));
    app_name_label.set_halign(gtk::Align::Start);
    app_name_label.set_wrap(true);
    app_name_label.add_css_class("dim-label");
    root.append(&app_name_label);

    let app_choose_button = gtk::Button::with_label(&i18n::t("widgets.weather.settings.app_choose"));
    let app_clear_button = gtk::Button::with_label(&i18n::t("widgets.weather.settings.app_clear"));
    root.append(&make_row(&[app_choose_button.upcast_ref(), app_clear_button.upcast_ref()]));

    // --- search wiring ---
    // Holds the results a response actually produced, indexed the same
    // way as the rows built from them - `ListBoxRow::index()` in
    // `row-activated` is how the handler finds its way back to the
    // matching `Location` without attaching data to the row itself.
    let results: Rc<RefCell<Vec<Location>>> = Rc::new(RefCell::new(Vec::new()));
    let search_generation = Rc::new(Cell::new(0u64));

    let clear_results = {
        let results_list = results_list.clone();
        move || {
            while let Some(child) = results_list.first_child() {
                results_list.remove(&child);
            }
        }
    };

    let set_status = {
        let status_label = status_label.clone();
        move |text: Option<String>| match text {
            Some(text) => {
                status_label.set_label(&text);
                status_label.set_visible(true);
            }
            None => status_label.set_visible(false),
        }
    };

    let run_search = {
        let search_entry = search_entry.clone();
        let results = results.clone();
        let results_list = results_list.clone();
        let search_generation = search_generation.clone();
        let clear_results = clear_results.clone();
        let set_status = set_status.clone();
        move || {
            let query = search_entry.text().trim().to_string();
            if query.is_empty() {
                return;
            }
            clear_results();
            set_status(Some(i18n::t("widgets.weather.settings.searching")));
            let generation = search_generation.get() + 1;
            search_generation.set(generation);
            let language = i18n::current_language();
            debug!("geocoding search for {query:?}");

            let query_log = query.clone();
            let results = results.clone();
            let results_list = results_list.clone();
            let search_generation = search_generation.clone();
            let set_status = set_status.clone();
            glib::spawn_future_local(async move {
                let outcome = gio::spawn_blocking(move || search_locations(&query, &language)).await;
                if generation != search_generation.get() {
                    debug!("geocoding search for {query_log:?} superseded, discarding");
                    return;
                }
                // The underlying error used to be indistinguishable from
                // a genuine "no results" - both just showed the same
                // "no results" status text, with nothing in the logs to
                // tell a real failure (network, bad JSON...) apart.
                let found = match outcome {
                    Ok(Ok(found)) if !found.is_empty() => found,
                    Ok(Ok(_empty)) => {
                        debug!("geocoding search for {query_log:?}: no results");
                        set_status(Some(i18n::t("widgets.weather.settings.no_results")));
                        return;
                    }
                    Ok(Err(err)) => {
                        warn!("geocoding search for {query_log:?} failed: {err}");
                        set_status(Some(i18n::t("widgets.weather.settings.no_results")));
                        return;
                    }
                    Err(_) => {
                        warn!("geocoding search for {query_log:?} task panicked");
                        set_status(Some(i18n::t("widgets.weather.settings.no_results")));
                        return;
                    }
                };
                set_status(None);
                for location in &found {
                    let row = adw::ActionRow::new();
                    row.set_title(&gtk::glib::markup_escape_text(&format_location(location)));
                    row.set_activatable(true);
                    results_list.append(&row);
                }
                *results.borrow_mut() = found;
            });
        }
    };

    search_entry.connect_activate({
        let run_search = run_search.clone();
        move |_| run_search()
    });
    search_button.connect_clicked({
        let run_search = run_search.clone();
        move |_| run_search()
    });
    results_list.connect_row_activated({
        let state = state.clone();
        let results = results.clone();
        let current_location_label = current_location_label.clone();
        let search_entry = search_entry.clone();
        let clear_results = clear_results.clone();
        let set_status = set_status.clone();
        move |_list, row| {
            let index = row.index();
            if index < 0 {
                return;
            }
            if let Some(location) = results.borrow().get(index as usize).cloned() {
                state.set_location(location);
                trigger_fetch(&state);
                current_location_label.set_label(&format_location(&state.location.borrow()));
                clear_results();
                search_entry.set_text("");
                set_status(None);
            }
        }
    });
    fahrenheit_button.connect_toggled({
        let state = state.clone();
        move |b| state.set_unit_fahrenheit(b.is_active())
    });
    scale_slider.connect_value_changed({
        let state = state.clone();
        move |s| state.set_content_scale(s.value() / 100.0)
    });
    app_choose_button.connect_clicked({
        let state = state.clone();
        let app_name_label = app_name_label.clone();
        move |button| {
            let Some(parent) = button.root().and_downcast::<gtk::Window>() else { return };
            let state = state.clone();
            let app_name_label = app_name_label.clone();
            open_app_picker(&parent, move |app_id| {
                state.set_app_id(Some(app_id));
                app_name_label.set_label(&current_app_display(&state));
            });
        }
    });
    app_clear_button.connect_clicked({
        let state = state.clone();
        let app_name_label = app_name_label.clone();
        move |_| {
            state.set_app_id(None);
            app_name_label.set_label(&current_app_display(&state));
        }
    });

    // --- retranslation ---
    i18n::on_change({
        let location_title = location_title.clone();
        let search_entry = search_entry.clone();
        let search_button = search_button.clone();
        let unit_label = unit_label.clone();
        let celsius_button = celsius_button.clone();
        let fahrenheit_button = fahrenheit_button.clone();
        let scale_label = scale_label.clone();
        let app_title = app_title.clone();
        let app_name_label = app_name_label.clone();
        let app_choose_button = app_choose_button.clone();
        let app_clear_button = app_clear_button.clone();
        let state = state.clone();
        move || {
            location_title.set_label(&i18n::t("widgets.weather.settings.location"));
            search_entry.set_placeholder_text(Some(&i18n::t("widgets.weather.settings.search_placeholder")));
            search_button.set_label(&i18n::t("widgets.weather.settings.search_button"));
            unit_label.set_label(&i18n::t("widgets.weather.settings.unit"));
            celsius_button.set_label(&i18n::t("widgets.weather.settings.unit_celsius"));
            fahrenheit_button.set_label(&i18n::t("widgets.weather.settings.unit_fahrenheit"));
            scale_label.set_label(&i18n::t("widgets.weather.settings.content_scale"));
            app_title.set_label(&i18n::t("widgets.weather.settings.app"));
            // Only the "no app chosen" placeholder text is actually
            // translated - a real app's display name isn't - but
            // recomputing via current_app_display() either way is simpler
            // than special-casing which branch needs the refresh.
            app_name_label.set_label(&current_app_display(&state));
            app_choose_button.set_label(&i18n::t("widgets.weather.settings.app_choose"));
            app_clear_button.set_label(&i18n::t("widgets.weather.settings.app_clear"));
        }
    });
    search_entry.set_placeholder_text(Some(&i18n::t("widgets.weather.settings.search_placeholder")));

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

/// Widget picker tile for this kind - `WidgetDescriptor::preview`, not
/// `spawn`. A live `spawn()` here fires a real Open-Meteo network
/// request (`build_content`'s initial fetch) just to show a throwaway
/// preview tile, every single time the picker opens - audit finding
/// 2026-09-18. Static icon instead, same idea as youtube.rs's own
/// `preview()`.
pub fn preview() -> gtk::Widget {
    let icon = gtk::Image::from_icon_name("weather-clear-symbolic");
    icon.set_pixel_size(48);
    icon.upcast()
}
