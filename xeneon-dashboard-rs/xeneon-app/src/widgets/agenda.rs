//! Agenda widget, steps 1-2 of the port from `widgets/agenda.py`: a month
//! calendar (today's date big on the left, the current month's grid on
//! the right) with a small colored bar under any day that has at least
//! one event, recurring events included. Visual layout/sizing is carried
//! over as-is from the Python original's already-finalized design
//! (colors, spacing, the today-square/weekend-color/bar CSS) rather than
//! re-derived here - see CLAUDE.md's `xeneon_dashboard/widgets/agenda.py`
//! for the fixes this encodes (GtkBox not centering a non-expanding child
//! in extra space, the hexpand-propagation firewall on the grid column,
//! etc.).
//!
//! **What's not ported yet** (later steps, same breakdown style as
//! `weather.rs`/`audio.rs`):
//! - No `AgendaSettings` yet (calendar picker, weekend color, content
//!   size slider) - every enabled calendar is used, weekend color is
//!   fixed, content size is fixed at the Python original's own default
//!   (170%).
//! - No hover tooltip / click popover with the day's event details.
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

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use crate::widgets::registry::{self, WidgetInstance};

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

// Left panel font sizes already include the Python original's own default
// content_scale (170%) baked in directly - there's no slider yet to make
// this a variable (see the module doc comment), so these are the plain
// pixel sizes to render, not a base-times-scale computation.
const WEEKDAY_FONT_PX: i32 = 26;
const DAYNUM_FONT_PX: i32 = 150;
const FULLDATE_FONT_PX: i32 = 24;

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
             line-height: 0.8; letter-spacing: 1px; text-transform: uppercase; font-size: {WEEKDAY_FONT_PX}px; }}
             .xeneon-agenda-daynum {{ font-weight: 700; color: #ffffff; line-height: 0.85; font-size: {DAYNUM_FONT_PX}px; }}
             .xeneon-agenda-fulldate {{ color: #ffffff; opacity: 0.75; line-height: 0.8; font-size: {FULLDATE_FONT_PX}px; }}
             .xeneon-agenda-header-cell {{ font-size: 11px; font-weight: 600; color: #ffffff; opacity: 0.55; \
             letter-spacing: 1px; text-transform: uppercase; }}
             .xeneon-agenda-cell-num {{ font-size: 20px; font-weight: 700; color: #ffffff; }}
             .xeneon-agenda-cell-num.dim {{ opacity: 0.3; }}
             .xeneon-agenda-today {{ background-color: {TODAY_BG_HEX}; border-radius: 6px; min-width: 28px; min-height: 28px; }}
             .xeneon-agenda-bar {{ min-width: 14px; min-height: 4px; border-radius: 2px; }}"
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

/// Registers (if new) and returns the CSS class that renders a bar in
/// `color` - `color` is a hex string (e.g. `"#448aff"`), sanitized into a
/// valid class name by dropping the `#`.
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
    color: String,
}

/// Lists every calendar-capable, enabled source EDS currently knows
/// about. Runs inside the same `spawn_blocking` closure as the event
/// fetch (see the module doc comment) - always fresh, never cached, so a
/// calendar enabled/disabled since the last refresh is picked up
/// automatically.
fn list_calendar_sources(connection: &gio::DBusConnection) -> Vec<CalendarSource> {
    let Ok(reply) = connection.call_sync(
        Some(SOURCES_BUS_NAME),
        SOURCE_MANAGER_PATH,
        "org.freedesktop.DBus.Introspectable",
        "Introspect",
        None,
        None,
        gio::DBusCallFlags::NONE,
        DBUS_CALL_TIMEOUT_MS,
        None::<&gio::Cancellable>,
    ) else {
        return Vec::new();
    };
    let Some((xml,)) = reply.get::<(String,)>() else { return Vec::new() };

    let mut sources = Vec::new();
    for name in parse_child_node_names(&xml) {
        let path = format!("{SOURCE_MANAGER_PATH}/{name}");
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
            continue;
        };
        let Some((props,)) = props_reply.get::<(HashMap<String, glib::Variant>,)>() else { continue };
        let Some(uid) = props.get("UID").and_then(|v| v.get::<String>()) else { continue };
        let Some(data) = props.get("Data").and_then(|v| v.get::<String>()) else { continue };

        let sections = parse_keyfile_sections(&data);
        let Some(calendar_section) = sections.get("Calendar") else { continue }; // not a calendar source
        let enabled = sections
            .get("Data Source")
            .and_then(|s| s.get("Enabled"))
            .map(|v| v == "true")
            .unwrap_or(false);
        if !enabled {
            continue;
        }
        let color = calendar_section.get("Color").cloned().unwrap_or_else(|| DEFAULT_DOT_HEX.to_string());
        sources.push(CalendarSource { uid, color });
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
/// full pipeline: list enabled calendars, open each, ask for every event
/// between `start_date` and `end_date` (inclusive), and bucket the ones
/// that parse cleanly by display date. A calendar that fails to connect
/// (account disabled, offline, revoked token...) is skipped rather than
/// failing the whole fetch.
fn fetch_month_events(start_date: NaiveDate, end_date: NaiveDate) -> HashMap<NaiveDate, Vec<EventInfo>> {
    let mut events_by_date: HashMap<NaiveDate, Vec<EventInfo>> = HashMap::new();
    let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) else {
        return events_by_date;
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

    for source in list_calendar_sources(&connection) {
        let Ok(open_reply) = connection.call_sync(
            Some(CALENDAR_FACTORY_BUS_NAME),
            CALENDAR_FACTORY_PATH,
            CALENDAR_FACTORY_INTERFACE,
            "OpenCalendar",
            Some(&(source.uid.as_str(),).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            DBUS_CALL_TIMEOUT_MS,
            None::<&gio::Cancellable>,
        ) else {
            continue;
        };
        let Some((object_path, bus_name)) = open_reply.get::<(glib::variant::ObjectPath, String)>() else {
            continue;
        };
        let object_path: String = object_path.into();

        // `Open()`'s own return value (backend property strings) isn't
        // needed - only that the call succeeds before querying.
        if connection
            .call_sync(
                Some(&bus_name),
                &object_path,
                CALENDAR_INTERFACE,
                "Open",
                None,
                None,
                gio::DBusCallFlags::NONE,
                DBUS_CALL_TIMEOUT_MS,
                None::<&gio::Cancellable>,
            )
            .is_err()
        {
            continue;
        }

        let Ok(list_reply) = connection.call_sync(
            Some(&bus_name),
            &object_path,
            CALENDAR_INTERFACE,
            "GetObjectList",
            Some(&(query.as_str(),).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            DBUS_CALL_TIMEOUT_MS,
            None::<&gio::Cancellable>,
        ) else {
            continue;
        };
        let Some((ics_objects,)) = list_reply.get::<(Vec<String>,)>() else { continue };

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
    events_by_date
}

struct GridCell {
    number_wrap: gtk::Box,
    number_label: gtk::Label,
    bars_row: gtk::Box,
}

struct AgendaState {
    weekday_label: gtk::Label,
    daynum_label: gtk::Label,
    fulldate_label: gtk::Label,
    header_labels: Vec<gtk::Label>,
    cells: Vec<GridCell>,
    grid_days: RefCell<Vec<NaiveDate>>,
    fetch_generation: Cell<u32>,
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

        for (col, label) in self.header_labels.iter().enumerate() {
            let text = crate::i18n_runtime::t(&format!("widgets.agenda.days_short.{}", DAY_KEYS[col]));
            set_label_color(label, &text, WEEKEND_COLS.contains(&col).then_some(WEEKEND_HEX));
        }

        let days = month_grid(today.year(), today.month());
        for (index, (cell, day)) in self.cells.iter().zip(days.iter()).enumerate() {
            let col = index % 7;
            let is_today = *day == today;
            let color = if is_today {
                Some(TODAY_TEXT_HEX)
            } else if WEEKEND_COLS.contains(&col) {
                Some(WEEKEND_HEX)
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

    /// Clears and redraws every cell's event bars from freshly fetched
    /// data. Mirrors `_apply_marks()` in the Python original (there
    /// called that since it only had colors; here it's the same function
    /// that drives the bars from full event info, ready for step 4's
    /// hover/click to reuse the same fetched data).
    fn apply_events(&self, events_by_date: &HashMap<NaiveDate, Vec<EventInfo>>) {
        let days = self.grid_days.borrow();
        for (cell, day) in self.cells.iter().zip(days.iter()) {
            while let Some(child) = cell.bars_row.first_child() {
                cell.bars_row.remove(&child);
            }
            let mut colors: Vec<&str> = Vec::new();
            if let Some(day_events) = events_by_date.get(day) {
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
        }
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

    let state = state.clone();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || fetch_month_events(start_date, end_date)).await;
        if generation != state.fetch_generation.get() {
            return;
        }
        if let Ok(events_by_date) = result {
            state.apply_events(&events_by_date);
        }
    });
}

fn build_content() -> (Rc<AgendaState>, gtk::Widget) {
    ensure_css_installed();

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 20);
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

            grid.attach(&cell, col, row + 1, 1, 1);
            cells.push(GridCell { number_wrap, number_label, bars_row });
        }
    }

    let state = Rc::new(AgendaState {
        weekday_label,
        daynum_label,
        fulldate_label,
        header_labels,
        cells,
        grid_days: RefCell::new(Vec::new()),
        fetch_generation: Cell::new(0),
    });

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

pub fn spawn() -> WidgetInstance {
    let (_state, content) = build_content();
    registry::instance_without_settings(content)
}

pub fn restore(_data: &serde_json::Value) -> WidgetInstance {
    // No persisted state yet (see module doc comment) - a saved "agenda"
    // widget just rebuilds fresh, same as a newly spawned one.
    spawn()
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
}
