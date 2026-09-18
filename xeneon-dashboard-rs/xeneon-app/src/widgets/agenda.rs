// SPDX-License-Identifier: GPL-3.0-or-later
//! Agenda widget, full port of `widgets/agenda.py` (done in 4 steps, same
//! breakdown style as `weather.rs`/`audio.rs`): a month calendar (today's
//! date big on the left, the current month's grid on the right) with a
//! small colored bar under any day that has at least one event -
//! recurring events included (step 2) - `AgendaSettings` (calendar
//! picker, weekend color, content-size slider for the left panel only -
//! step 3), and a hover tooltip / click popover with the day's event
//! details (step 4). Visual layout/sizing is carried over as-is from the
//! Python original's already-finalized design (colors, spacing, the
//! today-square/weekend-color/bar CSS) rather than re-derived here - see
//! CLAUDE.md's `xeneon_dashboard/widgets/agenda.py` for the fixes this
//! encodes (GtkBox not centering a non-expanding child in extra space,
//! the hexpand-propagation firewall on the grid column, etc.).
//!
//! ## Recurring events (step 2)
//!
//! Raw D-Bus `GetObjectList` (see below) returns a recurring event's
//! *master* component as-is - `RRULE:FREQ=WEEKLY;BYDAY=WE` and a single
//! `DTSTART`, not one component per occurrence - confirmed empirically
//! against a real weekly-recurring event before writing any of this. The
//! Python original gets occurrence expansion for free from libecal
//! (`ECal.Client.generate_instances_sync`); there's no equivalent
//! shortcut over raw D-Bus, so `expand_recurring` does it explicitly via
//! the `rrule` crate (RFC5545 recurrence is real algorithmic complexity -
//! BYDAY/INTERVAL/UNTIL/EXDATE/RECURRENCE-ID - not something worth
//! re-deriving by hand, hence the one new dependency). Rather than
//! translating the parsed iCal properties into `rrule`'s own builder API,
//! `expand_recurring` just re-joins the event's own `DTSTART`/`RRULE`/
//! `RDATE`/`EXDATE`/`EXRULE` lines and hands that block to
//! `RRuleSet::from_str`, which already speaks this exact iCalendar
//! subset. A rule shape `rrule` can't parse falls back to treating the
//! event as non-recurring (its own master date, if in range) rather than
//! disappearing entirely.
//!
//! ## Talking to Evolution Data Server without EDataServer/ECal bindings
//!
//! There's no GObject-Introspection crate for `EDataServer`/`ECal` on
//! crates.io, and pulling in raw `gir`-generated bindings for a single
//! widget isn't worth it. Instead this talks to EDS's own D-Bus services
//! directly via `gio::DBusConnection` - the same "no new crate, GTK
//! already pulls in `gio`" approach `audio.rs` uses for MPRIS, just against
//! a different, lower-level service:
//! - `org.gnome.evolution.dataserver.Sources5` /
//!   `.../SourceManager` - introspected for its child `Source_N` nodes,
//!   each queried with a plain `Properties.GetAll` for `UID` and `Data`.
//!   `Data` is a GKeyFile-formatted blob (the same format as
//!   `~/.config/evolution/sources/*.source`) - parsed by hand
//!   (`parse_keyfile_sections`) rather than pulling in a keyfile crate,
//!   since it's a handful of `[Section]`/`key=value` lines and nothing
//!   more.
//! - `org.gnome.evolution.dataserver.Calendar8` /
//!   `.../CalendarFactory.OpenCalendar(uid)` returns an object path *and
//!   its own bus name* (EDS spins up one subprocess per open backend) -
//!   `Calendar.Open()` then `GetObjectList(sexp)` on that returns raw
//!   iCalendar `VEVENT` text for every component matching an
//!   `occur-in-time-range?` query, same query language libecal itself
//!   uses. Only the handful of properties this widget needs (`SUMMARY`,
//!   `DTSTART` and its `VALUE=DATE`/`TZID` params) are picked out by hand
//!   (`parse_summary_and_dtstart`) - not a full iCalendar parser.
//!
//! All of the above happens inside one `gio::spawn_blocking` closure per
//! refresh (discovery is cheap, but opening a calendar backed by a real
//! CalDAV/Exchange account can genuinely hit the network) - never on the
//! GTK main thread - and the result comes back via
//! `glib::spawn_future_local`, exactly the pattern `weather.rs`'s module
//! doc comment explains in full. A generation counter makes a
//! superseded/stale response a no-op, same as everywhere else this
//! pattern is used.
//!
//! ## Settings, hover and click (steps 3-4)
//!
//! `AgendaSettings`' calendar checklist re-lists sources synchronously
//! (see `list_calendar_sources`'s own doc comment on why that's fine) each
//! time it's opened, rather than caching what `build_content` saw - an
//! account added or removed since the widget was created should show up
//! without recreating the widget. `apply_events` keeps the last fetch's
//! result in `AgendaState::events_by_date` purely so a later click
//! (`on_cell_clicked`) can look a day up without a fresh D-Bus round trip;
//! the hover tooltip and the popover's content are both built from that
//! same fetch, in that same function, so there's only one place that
//! decides "this day has events" (a day with none gets neither).

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use log::{debug, warn};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Once;

// Audit finding 2026-09-18: this used to be a private copy (with a
// WHITE parse-failure fallback, unlike appearance_popover.rs's BLACK) -
// unified on BLACK everywhere per the user's call, then deduped onto
// the one shared implementation.
use crate::appearance_popover::{hex_to_rgba, rgba_to_hex};
use crate::widgets::registry::WidgetInstance;

const SOURCES_BUS_NAME: &str = "org.gnome.evolution.dataserver.Sources5";
const SOURCE_MANAGER_PATH: &str = "/org/gnome/evolution/dataserver/SourceManager";
const SOURCE_INTERFACE: &str = "org.gnome.evolution.dataserver.Source";
const CALENDAR_FACTORY_BUS_NAME: &str = "org.gnome.evolution.dataserver.Calendar8";
const CALENDAR_FACTORY_PATH: &str = "/org/gnome/evolution/dataserver/CalendarFactory";
const CALENDAR_FACTORY_INTERFACE: &str = "org.gnome.evolution.dataserver.CalendarFactory";
const CALENDAR_INTERFACE: &str = "org.gnome.evolution.dataserver.Calendar";
const DBUS_CALL_TIMEOUT_MS: i32 = 5000;

const REFRESH_INTERVAL_SECONDS: u32 = 5 * 60;
const DEFAULT_DOT_HEX: &str = "#62a0ea";
const TODAY_BG_HEX: &str = "#ffffff";
const TODAY_TEXT_HEX: &str = "#000000";
const WEEKEND_HEX: &str = "#ffa726";
const WEEKEND_COLS: [usize; 2] = [5, 6]; // Saturday, Sunday - Monday-first columns
const WEEKS_SHOWN: i64 = 6; // fixed row count so the grid's height never shifts month to month
const GRID_RIGHT_MARGIN_PX: i32 = 24; // matches the breathing room vertical centering gives top/bottom
// Generous upper bound for how many occurrences of one recurring event
// `expand_recurring` will ever materialize for a single ~6-week grid -
// well beyond even a daily-recurring event's ~42 occurrences, just a
// backstop against a pathological rule (e.g. no COUNT/UNTIL at all).
const MAX_OCCURRENCES_PER_EVENT: u16 = 500;

// Left panel (weekday/day-number/full-date) font sizes at content_scale
// == 1.0 - the only part `AgendaSettings`' content-size slider affects
// (per an explicit instruction from the user while building the Python
// original: the slider is just for the left panel, not the grid). The
// month grid keeps its own fixed, bolder sizing (see the static CSS
// below) regardless of this scale.
const BASE_WEEKDAY_FONT_PX: f64 = 15.0;
const BASE_DAYNUM_FONT_PX: f64 = 88.0;
const BASE_FULLDATE_FONT_PX: f64 = 14.0;
const MIN_CONTENT_SCALE: f64 = 0.5;
const MAX_CONTENT_SCALE: f64 = 2.0;
const DEFAULT_CONTENT_SCALE: f64 = 1.7;

const DAY_KEYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
const MONTH_KEYS: [&str; 12] =
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

static INSTALL_CSS: Once = Once::new();

/// Installs every static (non-scaled - see module doc comment) CSS rule
/// once, display-wide. Mirrors `_ensure_static_css_installed()` in the
/// Python original almost line for line.
fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!(
            ".xeneon-agenda-weekday {{ font-weight: 600; color: #ffffff; opacity: 0.75; \
             line-height: 0.8; letter-spacing: 1px; text-transform: uppercase; }}
             .xeneon-agenda-daynum {{ font-weight: 700; color: #ffffff; line-height: 0.85; }}
             .xeneon-agenda-fulldate {{ color: #ffffff; opacity: 0.75; line-height: 0.8; }}
             .xeneon-agenda-header-cell {{ font-size: 11px; font-weight: 600; color: #ffffff; opacity: 0.55; \
             letter-spacing: 1px; text-transform: uppercase; }}
             .xeneon-agenda-cell-num {{ font-size: 20px; font-weight: 700; color: #ffffff; }}
             .xeneon-agenda-cell-num.dim {{ opacity: 0.3; }}
             .xeneon-agenda-today {{ background-color: {TODAY_BG_HEX}; border-radius: 6px; min-width: 28px; min-height: 28px; }}
             .xeneon-agenda-bar {{ min-width: 14px; min-height: 4px; border-radius: 2px; }}
             .xeneon-agenda-popover-dot {{ min-width: 10px; min-height: 10px; border-radius: 5px; }}"
        ));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

thread_local! {
    // A bar's color is a calendar's own color (a handful of distinct
    // values reused across many bars, not one-per-widget) - one rule per
    // distinct color, keyed by a sanitized class name, in a single
    // display-wide provider that grows as new colors are seen. Avoids
    // giving every bar its own `CssProvider` via the now-deprecated
    // `StyleContext::add_provider` - same reasoning as `audio.rs`'s
    // `SCALE_RULES`/`ensure_scale_provider`/`reload_scale_css`, just keyed
    // by color instead of by widget instance.
    static COLOR_RULES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static COLOR_PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };

    // Per-instance, content_scale-dependent rules (left panel font sizes
    // only - see the module doc comment) keyed by each AgendaState's own
    // unique class, same pattern as `audio.rs`'s own `SCALE_RULES`/
    // `ensure_scale_provider`/`reload_scale_css` (not shared with
    // `COLOR_RULES` above since these reload far more often - every
    // scale-slider tick - and there's no reason to re-parse every
    // instance's color rules each time just because one instance's scale
    // changed).
    static SCALE_RULES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static SCALE_PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
    static NEXT_INSTANCE_ID: Cell<u64> = const { Cell::new(0) };
}

fn ensure_color_provider() -> gtk::CssProvider {
    COLOR_PROVIDER.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            let provider = gtk::CssProvider::new();
            if let Some(display) = gtk::gdk::Display::default() {
                gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            }
            *cell = Some(provider);
        }
        cell.as_ref().unwrap().clone()
    })
}

/// Registers (if new) and returns the CSS class that renders a bar (or
/// popover dot) in `color` - `color` is a hex string (e.g. `"#448aff"`),
/// sanitized into a valid class name by dropping the `#`.
fn bar_color_class(color: &str) -> String {
    let sanitized: String = color.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let class = format!("xeneon-agenda-bar-{sanitized}");
    COLOR_RULES.with(|rules| {
        let mut rules = rules.borrow_mut();
        if rules.contains_key(&class) {
            return;
        }
        rules.insert(class.clone(), format!(".{class} {{ background-color: {color}; }}"));
        let css: String = rules.values().cloned().collect::<Vec<_>>().join("\n");
        ensure_color_provider().load_from_string(&css);
    });
    class
}

fn ensure_scale_provider() -> gtk::CssProvider {
    SCALE_PROVIDER.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            let provider = gtk::CssProvider::new();
            if let Some(display) = gtk::gdk::Display::default() {
                gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            }
            *cell = Some(provider);
        }
        cell.as_ref().unwrap().clone()
    })
}

fn reload_scale_css() {
    SCALE_RULES.with(|rules| {
        let css: String = rules.borrow().values().cloned().collect::<Vec<_>>().join("\n");
        ensure_scale_provider().load_from_string(&css);
    });
}

fn next_instance_css_class() -> String {
    NEXT_INSTANCE_ID.with(|id| {
        let value = id.get() + 1;
        id.set(value);
        format!("xeneon-agenda-{value}")
    })
}

/// One event instance, already resolved to a display date/time - no raw
/// iCal/timezone details survive past `fetch_month_events`. Only `color`
/// (bar tint) and `time` (sort key) are read yet - `all_day`/`summary`
/// are already carried through ready for step 4 (hover tooltip / click
/// popover with each event's title and time), so that step doesn't need
/// to touch the fetch/parse pipeline at all, only `apply_events`.
#[derive(Clone)]
#[allow(dead_code)]
struct EventInfo {
    all_day: bool,
    /// Local (hour, minute); `None` when `all_day`.
    time: Option<(u32, u32)>,
    summary: String,
    color: String,
}

/// Parses a GKeyFile-formatted blob (an EDS source's `Data` D-Bus
/// property - same format as `~/.config/evolution/sources/*.source`)
/// into `{section: {key: value}}`. Deliberately minimal: no comments,
/// continuation lines or type coercion beyond what `[Data Source]`/
/// `[Calendar]` actually need.
fn parse_keyfile_sections(data: &str) -> HashMap<String, HashMap<String, String>> {
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current = String::new();
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = name.to_string();
            sections.entry(current.clone()).or_default();
        } else if let Some((key, value)) = line.split_once('=') {
            sections.entry(current.clone()).or_default().insert(key.to_string(), value.to_string());
        }
    }
    sections
}

/// Every `<node name="...">` directly under the introspected XML - used
/// to enumerate `SourceManager`'s `Source_N` children without needing the
/// much more awkward-to-decode `ObjectManager.GetManagedObjects` reply
/// (a deeply nested `a{oa{sa{sv}}}`) for what's otherwise a handful of
/// individual `Properties.GetAll` calls anyway.
fn parse_child_node_names(xml: &str) -> Vec<String> {
    let mut names = Vec::new();
    for part in xml.split("<node").skip(1) {
        let tag_end = part.find('>').unwrap_or(part.len());
        let tag = &part[..tag_end];
        if let Some(idx) = tag.find("name=\"") {
            let rest = &tag[idx + "name=\"".len()..];
            if let Some(end) = rest.find('"') {
                let name = &rest[..end];
                if !name.is_empty() {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}

/// A calendar EDS knows about - enough to open it and color its bars.
/// `enabled` mirrors the Python original's `source.get_enabled()`
/// (`[Data Source] Enabled`, the whole account's own toggle - not
/// `[Calendar] Selected`, GNOME Calendar's separate per-calendar
/// checkbox, which this widget doesn't consult, same as the Python
/// original).
struct CalendarSource {
    uid: String,
    display_name: String,
    color: String,
    enabled: bool,
}

/// Lists every calendar-capable source EDS currently knows about,
/// enabled or not - `AgendaSettings` needs the disabled ones too, shown
/// but unselectable (`check.set_sensitive(source.enabled)`), same as the
/// Python original. Callers that only want usable calendars (the event
/// fetch) filter on `.enabled` themselves.
///
/// Cheap enough (local D-Bus, no network) to call synchronously on the
/// GTK main thread when building/refreshing `AgendaSettings` - same
/// reasoning as `audio.rs`'s MPRIS discovery - as well as from inside the
/// event fetch's `spawn_blocking` closure. Always fresh, never cached, so
/// a calendar enabled/disabled or added/removed since the last call is
/// picked up automatically.
fn list_calendar_sources(connection: &gio::DBusConnection) -> Vec<CalendarSource> {
    // A failure here means *no* calendar will ever be found - previously
    // indistinguishable from "the user genuinely has no calendars
    // configured", the common, non-broken case.
    let reply = match connection.call_sync(
        Some(SOURCES_BUS_NAME),
        SOURCE_MANAGER_PATH,
        "org.freedesktop.DBus.Introspectable",
        "Introspect",
        None,
        None,
        gio::DBusCallFlags::NONE,
        DBUS_CALL_TIMEOUT_MS,
        None::<&gio::Cancellable>,
    ) {
        Ok(reply) => reply,
        Err(err) => {
            warn!("failed to introspect {SOURCE_MANAGER_PATH} ({SOURCES_BUS_NAME} not running?): {err}");
            return Vec::new();
        }
    };
    let Some((xml,)) = reply.get::<(String,)>() else {
        warn!("introspect reply for {SOURCE_MANAGER_PATH} had an unexpected shape");
        return Vec::new();
    };

    let mut sources = Vec::new();
    for name in parse_child_node_names(&xml) {
        let path = format!("{SOURCE_MANAGER_PATH}/{name}");
        // Not every child node is a calendar (mail accounts, address
        // books...) - a GetAll failure here is a real communication
        // problem though, worth a trace, unlike the "not a calendar
        // source" filters below which are just routine skips.
        let Ok(props_reply) = connection.call_sync(
            Some(SOURCES_BUS_NAME),
            &path,
            "org.freedesktop.DBus.Properties",
            "GetAll",
            Some(&(SOURCE_INTERFACE,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            DBUS_CALL_TIMEOUT_MS,
            None::<&gio::Cancellable>,
        ) else {
            debug!("GetAll failed for source node {name}, skipping");
            continue;
        };
        let Some((props,)) = props_reply.get::<(HashMap<String, glib::Variant>,)>() else { continue };
        let Some(uid) = props.get("UID").and_then(|v| v.get::<String>()) else { continue };
        let Some(data) = props.get("Data").and_then(|v| v.get::<String>()) else { continue };

        let sections = parse_keyfile_sections(&data);
        let Some(calendar_section) = sections.get("Calendar") else { continue }; // not a calendar source
        let data_source = sections.get("Data Source");
        let enabled = data_source.and_then(|s| s.get("Enabled")).map(|v| v == "true").unwrap_or(false);
        let display_name = data_source.and_then(|s| s.get("DisplayName")).cloned().unwrap_or_else(|| uid.clone());
        let color = calendar_section.get("Color").cloned().unwrap_or_else(|| DEFAULT_DOT_HEX.to_string());
        sources.push(CalendarSource { uid, display_name, color, enabled });
    }
    sources
}

/// Unfolds RFC5545 line-folding (a continuation line starts with a single
/// space) before scanning for properties - none of the sample data seen
/// while building this actually folds `SUMMARY`/`DTSTART`, but a long
/// enough summary could, and this is cheap insurance against it.
fn unfold_ical_lines(ics: &str) -> String {
    let mut out = String::with_capacity(ics.len());
    for line in ics.split("\r\n").flat_map(|l| l.split('\n')) {
        if let Some(rest) = line.strip_prefix(' ') {
            out.push_str(rest);
        } else {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
        }
    }
    out
}

/// Converts one `DTSTART` property line's params + value into a display
/// date/time, or `None` if it can't be parsed (malformed data - skipped
/// rather than crashing the fetch over one bad component).
fn parse_dtstart(prefix: &str, value: &str) -> Option<(NaiveDate, Option<(u32, u32)>)> {
    if prefix.contains("VALUE=DATE") {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
        return Some((date, None));
    }

    let (naive_str, is_utc) = match value.strip_suffix('Z') {
        Some(stripped) => (stripped, true),
        None => (value, false),
    };
    let naive = NaiveDateTime::parse_from_str(naive_str, "%Y%m%dT%H%M%S").ok()?;

    let local = if is_utc {
        chrono::Utc.from_utc_datetime(&naive).with_timezone(&Local)
    } else if let Some(tzid) = prefix.split(';').find_map(|part| part.strip_prefix("TZID=")) {
        let tz: chrono_tz::Tz = tzid.parse().ok()?;
        let zoned = tz.from_local_datetime(&naive).single()?;
        zoned.with_timezone(&Local)
    } else {
        // No TZID and no trailing Z - a "floating" time, meant to be read
        // in whatever timezone the viewer is in. Already what we want:
        // display it as-is, in the system's local timezone.
        Local.from_local_datetime(&naive).single()?
    };
    Some((local.date_naive(), Some((local.time().hour(), local.time().minute()))))
}

/// Picks `SUMMARY` and `DTSTART` (with its `VALUE=DATE`/`TZID` params) out
/// of one already-unfolded `VEVENT` block - not a full iCalendar parser,
/// just these two properties. For a recurring event this is only its
/// *master* date - `expand_recurring` below is what turns that into every
/// actual occurrence.
fn parse_summary_and_dtstart(unfolded: &str) -> Option<(String, NaiveDate, Option<(u32, u32)>)> {
    let mut summary = String::new();
    let mut dtstart = None;
    for line in unfolded.lines() {
        if let Some((prefix, value)) = line.split_once(':') {
            let name = prefix.split(';').next().unwrap_or(prefix);
            if name == "SUMMARY" {
                summary = value.to_string();
            } else if name == "DTSTART" {
                dtstart = parse_dtstart(prefix, value);
            }
        }
    }
    let (date, time) = dtstart?;
    Some((summary, date, time))
}

/// Whether an already-unfolded `VEVENT` block has an `RRULE` property -
/// the only signal used to decide whether an event needs `expand_recurring`
/// at all (most calendar entries don't).
fn has_rrule(unfolded: &str) -> bool {
    unfolded.lines().any(|line| {
        line.split_once(':').map(|(prefix, _)| prefix.split(';').next().unwrap_or(prefix)) == Some("RRULE")
    })
}

/// Every date (and, for a non-all-day event, local time) this recurring
/// event actually falls on between `start_utc` and `end_utc` - the piece
/// raw D-Bus `GetObjectList` doesn't do for us (see the module doc
/// comment). Built by handing the event's own `DTSTART`/`RRULE`/`RDATE`/
/// `EXDATE`/`EXRULE` lines, verbatim, to `rrule::RRuleSet`'s own iCalendar
/// parser (`RRuleSet: FromStr`) - it accepts exactly this kind of
/// newline-joined property block, so there's no need to hand-translate
/// them into `rrule`'s builder API first. `None` if the block doesn't
/// parse (a rule shape `rrule` doesn't support, or malformed data) -
/// the caller falls back to just the master's own date in that case,
/// same as a non-recurring event.
fn expand_recurring(
    unfolded: &str,
    start_utc: chrono::DateTime<chrono::Utc>,
    end_utc: chrono::DateTime<chrono::Utc>,
) -> Option<Vec<(NaiveDate, Option<(u32, u32)>)>> {
    let recurrence_block: String = unfolded
        .lines()
        .filter(|line| {
            let name = line.split_once(':').map(|(prefix, _)| prefix.split(';').next().unwrap_or(prefix));
            matches!(name, Some("DTSTART" | "RRULE" | "EXRULE" | "RDATE" | "EXDATE"))
        })
        .collect::<Vec<_>>()
        .join("\n");

    let rrule_set: rrule::RRuleSet = recurrence_block.parse().ok()?;
    let after = start_utc.with_timezone(&rrule::Tz::UTC);
    let before = end_utc.with_timezone(&rrule::Tz::UTC);
    let result = rrule_set.after(after).before(before).all(MAX_OCCURRENCES_PER_EVENT);
    Some(
        result
            .dates
            .into_iter()
            .map(|dt| {
                let local = dt.with_timezone(&Local);
                (local.date_naive(), Some((local.time().hour(), local.time().minute())))
            })
            .collect(),
    )
}

/// Every date shown on the grid for `year`/`month`, Monday-first, in
/// reading order - always exactly `7 * WEEKS_SHOWN` days so the widget's
/// layout never reflows between a short and a long month.
fn month_grid(year: i32, month: u32) -> Vec<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid year/month");
    let offset = first.weekday().num_days_from_monday() as i64;
    let start = first - chrono::Duration::days(offset);
    (0..7 * WEEKS_SHOWN).map(|i| start + chrono::Duration::days(i)).collect()
}

/// Runs on GIO's blocking thread pool (see the module doc comment) - the
/// full pipeline: list calendars, keep the ones both enabled and in
/// `selected_uids` (`AgendaSettings`' checkboxes - every enabled calendar
/// the first time a widget is added, see `AgendaState::default_selected_uids`),
/// open each, ask for every event between `start_date` and `end_date`
/// (inclusive), and bucket the ones that parse cleanly by display date. A
/// calendar that fails to connect (account disabled, offline, revoked
/// token...) is skipped rather than failing the whole fetch.
fn fetch_month_events(
    selected_uids: &HashSet<String>,
    start_date: NaiveDate,
    end_date: NaiveDate,
) -> HashMap<NaiveDate, Vec<EventInfo>> {
    let mut events_by_date: HashMap<NaiveDate, Vec<EventInfo>> = HashMap::new();
    let connection = match gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) {
        Ok(connection) => connection,
        Err(err) => {
            warn!("failed to connect to the session bus: {err}");
            return events_by_date;
        }
    };

    let start_utc = Local
        .from_local_datetime(&start_date.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|| chrono::Utc.from_utc_datetime(&start_date.and_hms_opt(0, 0, 0).unwrap()));
    let end_utc = Local
        .from_local_datetime(&(end_date + chrono::Duration::days(1)).and_hms_opt(0, 0, 0).unwrap())
        .single()
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|| {
            chrono::Utc.from_utc_datetime(&(end_date + chrono::Duration::days(1)).and_hms_opt(0, 0, 0).unwrap())
        });
    let query = format!(
        "(occur-in-time-range? (make-time \"{}\") (make-time \"{}\"))",
        start_utc.format("%Y%m%dT%H%M%SZ"),
        end_utc.format("%Y%m%dT%H%M%SZ"),
    );

    let mut calendars_queried = 0u32;
    for source in list_calendar_sources(&connection) {
        if !source.enabled || !selected_uids.contains(&source.uid) {
            continue;
        }
        // Every early `continue` from here on means this one calendar
        // contributes zero events to the fetch - by design (see the
        // module doc comment: "a calendar that fails to connect... is
        // skipped rather than failing the whole fetch"), but previously
        // with no trace of *which* calendar or *why*, indistinguishable
        // from that calendar genuinely having nothing on it this month.
        let open_reply = match connection.call_sync(
            Some(CALENDAR_FACTORY_BUS_NAME),
            CALENDAR_FACTORY_PATH,
            CALENDAR_FACTORY_INTERFACE,
            "OpenCalendar",
            Some(&(source.uid.as_str(),).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            DBUS_CALL_TIMEOUT_MS,
            None::<&gio::Cancellable>,
        ) {
            Ok(reply) => reply,
            Err(err) => {
                warn!("{}: OpenCalendar failed: {err}", source.display_name);
                continue;
            }
        };
        // `OpenCalendar`'s object-path out-param is typed plain `s`, not
        // `o` (confirmed against a real reply's signature, `"(ss)"`, while
        // debugging why no events ever came back) - `glib::variant::ObjectPath`
        // (GVariant type `o`) doesn't match it, so `.get()` silently failed
        // and every calendar got skipped. Logged now in case a future EDS
        // version shifts the shape again.
        let Some((object_path, bus_name)) = open_reply.get::<(String, String)>() else {
            warn!("{}: OpenCalendar reply had an unexpected shape", source.display_name);
            continue;
        };

        // `Open()`'s own return value (backend property strings) isn't
        // needed - only that the call succeeds before querying.
        if let Err(err) = connection.call_sync(
            Some(&bus_name),
            &object_path,
            CALENDAR_INTERFACE,
            "Open",
            None,
            None,
            gio::DBusCallFlags::NONE,
            DBUS_CALL_TIMEOUT_MS,
            None::<&gio::Cancellable>,
        ) {
            warn!("{}: Open failed (account disabled, offline, revoked token?): {err}", source.display_name);
            continue;
        }

        let list_reply = match connection.call_sync(
            Some(&bus_name),
            &object_path,
            CALENDAR_INTERFACE,
            "GetObjectList",
            Some(&(query.as_str(),).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            DBUS_CALL_TIMEOUT_MS,
            None::<&gio::Cancellable>,
        ) {
            Ok(reply) => reply,
            Err(err) => {
                warn!("{}: GetObjectList failed: {err}", source.display_name);
                continue;
            }
        };
        let Some((ics_objects,)) = list_reply.get::<(Vec<String>,)>() else {
            warn!("{}: GetObjectList reply had an unexpected shape", source.display_name);
            continue;
        };
        calendars_queried += 1;

        for ics in ics_objects {
            let unfolded = unfold_ical_lines(&ics);
            let Some((summary, master_date, master_time)) = parse_summary_and_dtstart(&unfolded) else {
                continue;
            };
            let all_day = master_time.is_none();

            let occurrences: Vec<(NaiveDate, Option<(u32, u32)>)> = if has_rrule(&unfolded) {
                expand_recurring(&unfolded, start_utc, end_utc).unwrap_or_else(|| {
                    // The rule didn't parse (a shape `rrule` doesn't
                    // support, or malformed data) - fall back to treating
                    // it like a non-recurring event rather than dropping
                    // it entirely.
                    debug!("{}: RRULE didn't parse, showing only its master date", source.display_name);
                    if (start_date..=end_date).contains(&master_date) {
                        vec![(master_date, master_time)]
                    } else {
                        Vec::new()
                    }
                })
            } else if (start_date..=end_date).contains(&master_date) {
                vec![(master_date, master_time)]
            } else {
                Vec::new()
            };

            for (date, time) in occurrences {
                events_by_date.entry(date).or_default().push(EventInfo {
                    all_day,
                    time: if all_day { None } else { time },
                    summary: summary.clone(),
                    color: source.color.clone(),
                });
            }
        }
    }

    for day_events in events_by_date.values_mut() {
        day_events.sort_by_key(|e| e.time.unwrap_or((0, 0)));
    }
    debug!(
        "fetched {calendars_queried} calendar(s), {} event(s) across {} day(s) ({start_date}..{end_date})",
        events_by_date.values().map(Vec::len).sum::<usize>(),
        events_by_date.len()
    );
    events_by_date
}

struct GridCell {
    /// The whole day cell - carries the click gesture and (when non-empty)
    /// the hover tooltip; also `popover`'s parent.
    cell: gtk::Box,
    number_wrap: gtk::Box,
    number_label: gtk::Label,
    bars_row: gtk::Box,
    /// Opened by a click when the day has at least one event (see
    /// `AgendaState::on_cell_clicked`) - stays unpopulated/unopened
    /// otherwise, same as the hover tooltip.
    popover: gtk::Popover,
    popover_box: gtk::Box,
}

struct AgendaState {
    /// Unique per-instance CSS class - only the left-panel font sizes
    /// (`content_scale`) need this; the grid's own styling is static, one
    /// shared set of rules for every instance (see `ensure_css_installed`).
    css_class: String,
    weekday_label: gtk::Label,
    daynum_label: gtk::Label,
    fulldate_label: gtk::Label,
    header_labels: Vec<gtk::Label>,
    cells: Vec<GridCell>,
    grid_days: RefCell<Vec<NaiveDate>>,
    fetch_generation: Cell<u32>,
    /// Which calendars' events to fetch/display - defaults to every
    /// enabled calendar the first time a widget is added (see
    /// `build_content`), user-editable afterward via `AgendaSettings`.
    selected_uids: RefCell<HashSet<String>>,
    weekend_color: RefCell<gtk::gdk::RGBA>,
    content_scale: Cell<f64>,
    /// The last successful fetch's result, kept around so a click on a
    /// cell (`on_cell_clicked`) can look up that day's events without
    /// re-fetching - populated by `apply_events`.
    events_by_date: RefCell<HashMap<NaiveDate, Vec<EventInfo>>>,
}

fn set_label_color(label: &gtk::Label, text: &str, color_hex: Option<&str>) {
    match color_hex {
        None => label.set_label(text),
        Some(hex) => label.set_markup(&format!(
            "<span foreground=\"{hex}\">{}</span>",
            glib::markup_escape_text(text)
        )),
    }
}

impl AgendaState {
    /// Rebuilds every static label (weekday/day-number/full-date, header
    /// row, grid cell numbers/today-highlight/weekend-color) from
    /// `today` - everything except the event bars, which need the
    /// (separately fetched) event data (`apply_events`). Mirrors
    /// `_apply_static_labels()` in the Python original.
    fn apply_static_labels(&self, today: NaiveDate) {
        let weekday_key = format!("widgets.agenda.days_long.{}", DAY_KEYS[today.weekday().num_days_from_monday() as usize]);
        self.weekday_label.set_label(&crate::i18n_runtime::t(&weekday_key));
        self.daynum_label.set_label(&today.day().to_string());
        let month_key = format!("widgets.agenda.months_long.{}", MONTH_KEYS[today.month0() as usize]);
        let month_name = crate::i18n_runtime::t(&month_key);
        self.fulldate_label.set_label(
            &crate::i18n_runtime::t("widgets.agenda.date_full")
                .replace("{month}", &month_name)
                .replace("{year}", &today.year().to_string()),
        );

        let weekend_hex = rgba_to_hex(&self.weekend_color.borrow());
        for (col, label) in self.header_labels.iter().enumerate() {
            let text = crate::i18n_runtime::t(&format!("widgets.agenda.days_short.{}", DAY_KEYS[col]));
            set_label_color(label, &text, WEEKEND_COLS.contains(&col).then_some(weekend_hex.as_str()));
        }

        let days = month_grid(today.year(), today.month());
        for (index, (cell, day)) in self.cells.iter().zip(days.iter()).enumerate() {
            let col = index % 7;
            let is_today = *day == today;
            let color = if is_today {
                Some(TODAY_TEXT_HEX)
            } else if WEEKEND_COLS.contains(&col) {
                Some(weekend_hex.as_str())
            } else {
                None
            };
            set_label_color(&cell.number_label, &day.day().to_string(), color);
            cell.number_label.remove_css_class("dim");
            if day.month() != today.month() {
                cell.number_label.add_css_class("dim");
            }
            if is_today {
                cell.number_wrap.add_css_class("xeneon-agenda-today");
            } else {
                cell.number_wrap.remove_css_class("xeneon-agenda-today");
            }
        }
        *self.grid_days.borrow_mut() = days;
    }

    /// Clears and redraws every cell's event bars, hover tooltip and click
    /// popover from freshly fetched data, and remembers it in
    /// `events_by_date` for `on_cell_clicked` to reuse later without
    /// re-fetching. Mirrors `_apply_marks()` in the Python original.
    fn apply_events(&self, events_by_date: HashMap<NaiveDate, Vec<EventInfo>>) {
        let days = self.grid_days.borrow();
        for (cell, day) in self.cells.iter().zip(days.iter()) {
            while let Some(child) = cell.bars_row.first_child() {
                cell.bars_row.remove(&child);
            }
            let day_events = events_by_date.get(day);

            let mut colors: Vec<&str> = Vec::new();
            if let Some(day_events) = day_events {
                for event in day_events {
                    if !colors.contains(&event.color.as_str()) {
                        colors.push(&event.color);
                    }
                }
            }
            for color in colors.into_iter().take(3) {
                let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                bar.add_css_class("xeneon-agenda-bar");
                bar.add_css_class(&bar_color_class(color));
                cell.bars_row.append(&bar);
            }

            // Hover (native GTK tooltip) - left unset for a day with no
            // events, so nothing shows on hover either. The click popover
            // (see `on_cell_clicked`) follows the same "nothing to show,
            // show nothing" rule.
            cell.cell.set_tooltip_text(day_events.map(|events| format_events_tooltip(events)).as_deref());
            populate_popover(&cell.popover_box, day_events.map(Vec::as_slice).unwrap_or(&[]));
        }
        drop(days);
        *self.events_by_date.borrow_mut() = events_by_date;
    }

    fn set_selected_uids(self: &Rc<Self>, uids: HashSet<String>) {
        *self.selected_uids.borrow_mut() = uids;
        refresh(self);
    }

    fn set_weekend_color(&self, rgba: gtk::gdk::RGBA) {
        *self.weekend_color.borrow_mut() = rgba;
        self.apply_static_labels(Local::now().date_naive());
    }

    fn set_content_scale(&self, scale: f64) {
        self.content_scale.set(scale.clamp(MIN_CONTENT_SCALE, MAX_CONTENT_SCALE));
        self.apply_content_scale();
    }

    fn apply_content_scale(&self) {
        let scale = self.content_scale.get();
        SCALE_RULES.with(|rules| {
            rules.borrow_mut().insert(
                self.css_class.clone(),
                format!(
                    ".{class} .xeneon-agenda-weekday {{ font-size: {weekday}px; }}\n\
                     .{class} .xeneon-agenda-daynum {{ font-size: {daynum}px; }}\n\
                     .{class} .xeneon-agenda-fulldate {{ font-size: {fulldate}px; }}",
                    class = self.css_class,
                    weekday = (BASE_WEEKDAY_FONT_PX * scale).round() as i32,
                    daynum = (BASE_DAYNUM_FONT_PX * scale).round() as i32,
                    fulldate = (BASE_FULLDATE_FONT_PX * scale).round() as i32,
                ),
            );
        });
        reload_scale_css();
    }

    /// Opens that cell's popover if (and only if) its day has at least
    /// one event - see `apply_events`, which already populated
    /// `popover_box` and left it empty otherwise.
    fn on_cell_clicked(&self, index: usize) {
        let Some(day) = self.grid_days.borrow().get(index).copied() else { return };
        let Some(cell) = self.cells.get(index) else { return };
        if self.events_by_date.borrow().get(&day).is_none_or(Vec::is_empty) {
            return;
        }
        cell.popover.popup();
    }

    fn to_dict(&self) -> serde_json::Value {
        let mut uids: Vec<String> = self.selected_uids.borrow().iter().cloned().collect();
        uids.sort();
        serde_json::json!({
            "selected_uids": uids,
            "weekend_color": rgba_to_hex(&self.weekend_color.borrow()),
            "content_scale": self.content_scale.get(),
        })
    }

    fn apply_dict(self: &Rc<Self>, data: &serde_json::Value) {
        if let Some(uids) = data.get("selected_uids").and_then(|v| v.as_array()) {
            let uids: HashSet<String> = uids.iter().filter_map(|v| v.as_str()).map(str::to_string).collect();
            self.set_selected_uids(uids);
        }
        if let Some(hex) = data.get("weekend_color").and_then(|v| v.as_str()) {
            self.set_weekend_color(hex_to_rgba(hex));
        }
        if let Some(scale) = data.get("content_scale").and_then(|v| v.as_f64()) {
            self.set_content_scale(scale);
        }
    }
}

fn format_events_tooltip(events: &[EventInfo]) -> String {
    events.iter().map(|event| format!("{} · {}", format_event_when(event), event.summary)).collect::<Vec<_>>().join("\n")
}

fn format_event_when(event: &EventInfo) -> String {
    if event.all_day {
        crate::i18n_runtime::t("widgets.agenda.all_day")
    } else if let Some((hour, minute)) = event.time {
        format!("{hour:02}:{minute:02}")
    } else {
        String::new()
    }
}

/// Rebuilds a cell's click-popover content from that day's events (empty
/// when there are none, matching the hover tooltip's "nothing to show,
/// show nothing" rule - `on_cell_clicked` never actually opens an empty
/// one, but there's no reason to leave stale content sitting in it either).
fn populate_popover(popover_box: &gtk::Box, events: &[EventInfo]) {
    while let Some(child) = popover_box.first_child() {
        popover_box.remove(&child);
    }
    for event in events {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("xeneon-agenda-popover-dot");
        dot.add_css_class(&bar_color_class(&event.color));
        dot.set_valign(gtk::Align::Start);
        dot.set_margin_top(5);
        row.append(&dot);

        let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let when_label = gtk::Label::new(Some(&format_event_when(event)));
        when_label.add_css_class("dim-label");
        when_label.set_halign(gtk::Align::Start);
        column.append(&when_label);
        let summary_label = gtk::Label::new(Some(&event.summary));
        summary_label.set_halign(gtk::Align::Start);
        summary_label.set_wrap(true);
        summary_label.set_xalign(0.0);
        column.append(&summary_label);
        row.append(&column);

        popover_box.append(&row);
    }
}

fn refresh(state: &Rc<AgendaState>) {
    let today = Local::now().date_naive();
    state.apply_static_labels(today);

    let generation = state.fetch_generation.get() + 1;
    state.fetch_generation.set(generation);
    let (start_date, end_date) = {
        let days = state.grid_days.borrow();
        (days[0], *days.last().unwrap())
    };
    let selected_uids = state.selected_uids.borrow().clone();

    let state = state.clone();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || fetch_month_events(&selected_uids, start_date, end_date)).await;
        if generation != state.fetch_generation.get() {
            debug!("agenda fetch superseded, discarding");
            return;
        }
        match result {
            Ok(events_by_date) => state.apply_events(events_by_date),
            Err(_) => warn!("agenda fetch task panicked"),
        }
    });
}

fn build_content() -> (Rc<AgendaState>, gtk::Widget) {
    ensure_css_installed();
    let css_class = next_instance_css_class();

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 20);
    root.add_css_class(&css_class);
    root.set_hexpand(true);
    root.set_vexpand(true);
    root.set_valign(gtk::Align::Center);

    let left = gtk::Box::new(gtk::Orientation::Vertical, 0);
    left.set_valign(gtk::Align::Center);
    left.set_halign(gtk::Align::Center);
    // Grows into whatever width is left over once the separator and the
    // fixed-size grid (see `right` below) have taken theirs - see
    // CLAUDE.md's agenda.py notes on why this is hexpand rather than a
    // centered-as-one-block layout.
    left.set_hexpand(true);
    let weekday_label = gtk::Label::new(None);
    weekday_label.add_css_class("xeneon-agenda-weekday");
    weekday_label.set_halign(gtk::Align::Center);
    left.append(&weekday_label);
    let daynum_label = gtk::Label::new(None);
    daynum_label.add_css_class("xeneon-agenda-daynum");
    daynum_label.set_halign(gtk::Align::Center);
    left.append(&daynum_label);
    let fulldate_label = gtk::Label::new(None);
    fulldate_label.add_css_class("xeneon-agenda-fulldate");
    fulldate_label.set_halign(gtk::Align::Center);
    left.append(&fulldate_label);
    root.append(&left);

    root.append(&gtk::Separator::new(gtk::Orientation::Vertical));

    let right = gtk::Box::new(gtk::Orientation::Vertical, 4);
    right.set_margin_end(GRID_RIGHT_MARGIN_PX);
    // Explicit, not just "happens to be false": without this, the
    // cells' own hexpand/vexpand below (needed to center the number
    // inside the "today" square) would propagate up and make `right`
    // compete with `left` for the outer box's leftover width.
    right.set_hexpand(false);
    let grid = gtk::Grid::new();
    grid.set_hexpand(false);
    grid.set_column_homogeneous(true);
    grid.set_row_homogeneous(true);
    grid.set_column_spacing(29);
    grid.set_row_spacing(8);
    right.append(&grid);
    root.append(&right);

    let mut header_labels = Vec::with_capacity(7);
    for col in 0..7 {
        let label = gtk::Label::new(None);
        label.add_css_class("xeneon-agenda-header-cell");
        label.set_halign(gtk::Align::Center);
        grid.attach(&label, col, 0, 1, 1);
        header_labels.push(label);
    }

    let mut cells = Vec::with_capacity((7 * WEEKS_SHOWN) as usize);
    for row in 0..WEEKS_SHOWN as i32 {
        for col in 0..7 {
            let cell = gtk::Box::new(gtk::Orientation::Vertical, 0);
            cell.set_halign(gtk::Align::Center);
            cell.set_valign(gtk::Align::Center);

            let number_wrap = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            number_wrap.set_halign(gtk::Align::Center);
            number_wrap.set_valign(gtk::Align::Center);
            // Clearance above only, so the "today" background never
            // touches the row above (row_homogeneous sizes every row off
            // the tallest one). Nothing below: the event bar sits right
            // under the number on purpose.
            number_wrap.set_margin_top(3);
            let number_label = gtk::Label::new(None);
            number_label.add_css_class("xeneon-agenda-cell-num");
            // GtkBox only centers a child within *extra granted space* -
            // without hexpand/vexpand too, the label's own "cell" in the
            // box is exactly its natural size (nothing for Center to do),
            // which is why - without this - the number sat pinned to one
            // corner of the enlarged "today" square instead of centered.
            number_label.set_halign(gtk::Align::Center);
            number_label.set_valign(gtk::Align::Center);
            number_label.set_hexpand(true);
            number_label.set_vexpand(true);
            number_wrap.append(&number_label);
            cell.append(&number_wrap);

            let bars_row = gtk::Box::new(gtk::Orientation::Horizontal, 3);
            bars_row.set_halign(gtk::Align::Center);
            bars_row.set_size_request(-1, 4);
            cell.append(&bars_row);

            // Click shows that day's events in a popup (see
            // `AgendaState::on_cell_clicked`, wired below once `state`
            // exists). Hover shows the same thing via the cell's native
            // tooltip instead (see `apply_events`), which needs no
            // gesture of its own. Both stay silent for a day with no
            // events rather than popping up empty.
            let popover = gtk::Popover::new();
            popover.set_parent(&cell);
            popover.set_autohide(true);
            let popover_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
            popover_box.set_margin_top(8);
            popover_box.set_margin_bottom(8);
            popover_box.set_margin_start(10);
            popover_box.set_margin_end(10);
            popover.set_child(Some(&popover_box));

            grid.attach(&cell, col, row + 1, 1, 1);
            cells.push(GridCell { cell, number_wrap, number_label, bars_row, popover, popover_box });
        }
    }

    // Cheap, local D-Bus discovery (see `list_calendar_sources`'s own doc
    // comment) - a freshly added widget has no saved selection yet, so it
    // starts from "every enabled calendar right now" rather than an
    // empty, bar-less grid. `apply_dict` (a saved widget) overwrites this
    // right after construction if it has its own saved selection.
    let default_selected_uids: HashSet<String> = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>)
        .map(|connection| {
            list_calendar_sources(&connection).into_iter().filter(|s| s.enabled).map(|s| s.uid).collect()
        })
        .unwrap_or_default();

    let state = Rc::new(AgendaState {
        css_class,
        weekday_label,
        daynum_label,
        fulldate_label,
        header_labels,
        cells,
        grid_days: RefCell::new(Vec::new()),
        fetch_generation: Cell::new(0),
        selected_uids: RefCell::new(default_selected_uids),
        weekend_color: RefCell::new(hex_to_rgba(WEEKEND_HEX)),
        content_scale: Cell::new(DEFAULT_CONTENT_SCALE),
        events_by_date: RefCell::new(HashMap::new()),
    });

    for (index, cell) in state.cells.iter().enumerate() {
        let click = gtk::GestureClick::new();
        click.connect_released({
            let state = state.clone();
            move |_gesture, _n_press, _x, _y| state.on_cell_clicked(index)
        });
        cell.cell.add_controller(click);
    }

    state.apply_content_scale();
    refresh(&state);
    let timeout_id = glib::timeout_add_seconds_local(REFRESH_INTERVAL_SECONDS, {
        let state = state.clone();
        move || {
            refresh(&state);
            glib::ControlFlow::Continue
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
    crate::i18n_runtime::on_change({
        let state = state.clone();
        move || state.apply_static_labels(Local::now().date_naive())
    });

    (state, root.upcast())
}

/// `AgendaSettings`: one checkbox per calendar EDS currently knows about
/// (from GNOME Online Accounts or added locally), the weekend day color,
/// and a content-size slider for the left date panel only (see
/// `WeatherSettings`' own scale slider in `weather.rs` for the reference
/// this follows). No `on_reset` (matches the Python original - the
/// generic appearance reset is all this widget's reset button does).
fn build_settings(state: Rc<AgendaState>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(280, -1);

    let calendars_label = gtk::Label::new(Some(&crate::i18n_runtime::t("widgets.agenda.settings.calendars")));
    calendars_label.set_halign(gtk::Align::Start);
    root.append(&calendars_label);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_min_content_height(180);
    scroller.set_max_content_height(180);
    scroller.set_vexpand(false);
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    let sources_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    scroller.set_child(Some(&sources_box));
    root.append(&scroller);

    let no_calendars_label = gtk::Label::new(Some(&crate::i18n_runtime::t("widgets.agenda.settings.no_calendars")));
    no_calendars_label.add_css_class("dim-label");
    no_calendars_label.set_wrap(true);
    no_calendars_label.set_visible(false);
    root.append(&no_calendars_label);

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let weekend_label = gtk::Label::new(Some(&crate::i18n_runtime::t("widgets.agenda.settings.weekend_color")));
    weekend_label.set_hexpand(true);
    weekend_label.set_halign(gtk::Align::Start);
    let weekend_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    weekend_color_button.set_rgba(&state.weekend_color.borrow());
    let weekend_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    weekend_row.append(&weekend_label);
    weekend_row.append(&weekend_color_button);
    root.append(&weekend_row);

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let scale_label = gtk::Label::new(Some(&crate::i18n_runtime::t("widgets.agenda.settings.content_scale")));
    scale_label.set_halign(gtk::Align::Start);
    root.append(&scale_label);
    let scale_slider =
        gtk::Scale::with_range(gtk::Orientation::Horizontal, MIN_CONTENT_SCALE * 100.0, MAX_CONTENT_SCALE * 100.0, 1.0);
    scale_slider.set_value(state.content_scale.get() * 100.0);
    scale_slider.set_draw_value(true);
    scale_slider.set_value_pos(gtk::PositionType::Right);
    root.append(&scale_slider);

    // Rebuilt from a fresh D-Bus listing every time settings opens (or
    // `sync_from_content` asks for a resync) - see `list_calendar_sources`'s
    // own doc comment on why this is cheap enough to do synchronously here.
    let rebuild_sources: Rc<dyn Fn()> = {
        let state = state.clone();
        let sources_box = sources_box.clone();
        let no_calendars_label = no_calendars_label.clone();
        Rc::new(move || {
            while let Some(child) = sources_box.first_child() {
                sources_box.remove(&child);
            }
            let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) else {
                no_calendars_label.set_visible(true);
                return;
            };
            let sources = list_calendar_sources(&connection);
            no_calendars_label.set_visible(sources.is_empty());
            let selected = state.selected_uids.borrow().clone();
            let checkbuttons: Rc<RefCell<Vec<(gtk::CheckButton, String)>>> = Rc::new(RefCell::new(Vec::new()));
            for source in sources {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                dot.add_css_class("xeneon-agenda-bar");
                dot.add_css_class(&bar_color_class(&source.color));
                dot.set_size_request(10, 10);
                row.append(&dot);
                let check = gtk::CheckButton::with_label(&source.display_name);
                check.set_active(selected.contains(&source.uid));
                check.set_sensitive(source.enabled);
                row.append(&check);
                sources_box.append(&row);
                checkbuttons.borrow_mut().push((check, source.uid));
            }
            for (check, _) in checkbuttons.borrow().iter() {
                check.connect_toggled({
                    let state = state.clone();
                    let checkbuttons = checkbuttons.clone();
                    move |_| {
                        let selected: HashSet<String> = checkbuttons
                            .borrow()
                            .iter()
                            .filter(|(c, _)| c.is_active())
                            .map(|(_, uid)| uid.clone())
                            .collect();
                        state.set_selected_uids(selected);
                    }
                });
            }
        })
    };
    rebuild_sources();

    weekend_color_button.connect_rgba_notify({
        let state = state.clone();
        move |b| state.set_weekend_color(b.rgba())
    });
    scale_slider.connect_value_changed({
        let state = state.clone();
        move |s| state.set_content_scale(s.value() / 100.0)
    });

    crate::i18n_runtime::on_change({
        let calendars_label = calendars_label.clone();
        let no_calendars_label = no_calendars_label.clone();
        let weekend_label = weekend_label.clone();
        let scale_label = scale_label.clone();
        let rebuild_sources = rebuild_sources.clone();
        move || {
            calendars_label.set_label(&crate::i18n_runtime::t("widgets.agenda.settings.calendars"));
            no_calendars_label.set_label(&crate::i18n_runtime::t("widgets.agenda.settings.no_calendars"));
            weekend_label.set_label(&crate::i18n_runtime::t("widgets.agenda.settings.weekend_color"));
            scale_label.set_label(&crate::i18n_runtime::t("widgets.agenda.settings.content_scale"));
            // Calendar display names aren't translated, but re-listing is
            // cheap and keeps this in one place rather than special-casing
            // just the labels above.
            rebuild_sources();
        }
    });

    root.upcast()
}

pub fn spawn() -> WidgetInstance {
    let (state, content) = build_content();
    let settings = build_settings(state.clone());
    WidgetInstance { content, settings: Some(settings), to_dict: Box::new(move || state.to_dict()), on_reset: None, on_change_ready: None }
}

pub fn restore(data: &serde_json::Value) -> WidgetInstance {
    let (state, content) = build_content();
    state.apply_dict(data);
    let settings = build_settings(state.clone());
    WidgetInstance { content, settings: Some(settings), to_dict: Box::new(move || state.to_dict()), on_reset: None, on_change_ready: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// A real weekly-recurring event pulled from a live calendar while
    /// building this (see the module doc comment) - EDS's raw
    /// `GetObjectList` returns exactly this master component, unexpanded,
    /// for any query whose range overlaps its series.
    const ONGLE_ICS: &str = "BEGIN:VEVENT\r\n\
        CREATED:20260721T054157Z\r\n\
        LAST-MODIFIED:20260915T151527Z\r\n\
        DTSTAMP:20260915T151527Z\r\n\
        SUMMARY:Ongle\r\n\
        RRULE:FREQ=WEEKLY;BYDAY=WE\r\n\
        DTSTART;VALUE=DATE:20260722\r\n\
        DTEND;VALUE=DATE:20260723\r\n\
        UID:620e6840-24e0-4ba0-82f1-36ae53d35a8a\r\n\
        END:VEVENT\r\n";

    #[test]
    fn expands_a_weekly_recurring_all_day_event() {
        let unfolded = unfold_ical_lines(ONGLE_ICS);
        assert!(has_rrule(&unfolded));
        let (summary, master_date, master_time) = parse_summary_and_dtstart(&unfolded).unwrap();
        assert_eq!(summary, "Ongle");
        assert_eq!(master_date, NaiveDate::from_ymd_opt(2026, 7, 22).unwrap());
        assert!(master_time.is_none());

        let start_utc = chrono::Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        let end_utc = chrono::Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let occurrences = expand_recurring(&unfolded, start_utc, end_utc).unwrap();
        let dates: Vec<NaiveDate> = occurrences.into_iter().map(|(date, _)| date).collect();

        // Every Wednesday in September 2026 - matches what the Python
        // original (via libecal) returns for the same real event/range.
        assert_eq!(
            dates,
            vec![
                NaiveDate::from_ymd_opt(2026, 9, 2).unwrap(),
                NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(),
                NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(),
                NaiveDate::from_ymd_opt(2026, 9, 23).unwrap(),
                NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
            ]
        );
    }

    /// Not a real test (hits the live session bus / real calendars) -
    /// temporary diagnostic for a user-reported bug: run with
    /// `cargo test -p xeneon-app agenda::tests::debug_real_fetch -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn debug_real_fetch() {
        let today = Local::now().date_naive();
        let days = month_grid(today.year(), today.month());
        let (start, end) = (days[0], *days.last().unwrap());
        println!("grid range: {start} .. {end}");

        let connection = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).unwrap();
        let sources = list_calendar_sources(&connection);
        println!("sources found: {}", sources.len());
        for s in &sources {
            println!("  uid={} enabled={} color={} name={}", s.uid, s.enabled, s.color, s.display_name);
        }

        let selected: HashSet<String> = sources.iter().filter(|s| s.enabled).map(|s| s.uid.clone()).collect();
        println!("selected uids: {selected:?}");
        let events = fetch_month_events(&selected, start, end);
        println!("days with events: {}", events.len());
        let mut dates: Vec<&NaiveDate> = events.keys().collect();
        dates.sort();
        for date in dates {
            for e in &events[date] {
                println!("  {date} time={:?} all_day={} summary={:?} color={}", e.time, e.all_day, e.summary, e.color);
            }
        }
    }
}
