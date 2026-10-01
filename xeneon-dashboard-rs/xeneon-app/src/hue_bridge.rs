// SPDX-License-Identifier: GPL-3.0-or-later
//! Philips Hue bridge discovery, pairing, and raw resource fetching -
//! app-level infrastructure shared by every Hue widget instance (see
//! `Config::hue_bridge_ip`/`hue_username`/`hue_clientkey` in xeneon-core's
//! own doc comment for why this lives once at the app level rather than
//! per-widget) and driven from Settings (`settings_page.rs`'s own Hue
//! section, not yet built - this module is step 1: get discovery, pairing
//! and a resource read working end to end before any UI touches it).
//!
//! Discovery mirrors the approach the GNOME Shell extension
//! `vchlum/smart-home` uses (its `avahi.js`/`plugins/philipshue-bridge/
//! api.js`, read for reference before writing this): local mDNS as the
//! primary path, with Philips' own cloud discovery endpoint
//! (`https://discovery.meethue.com/`) as a fallback for machines with no
//! mDNS responder running. Both run every time `discover_all` is called
//! and their results are merged/deduped by IP - same as the reference
//! extension - rather than picking one and giving up if it comes back
//! empty.
//!
//! The local path talks to `avahi-daemon` directly over D-Bus
//! (`org.freedesktop.Avahi.Server`, system bus), not by shelling out to
//! the `avahi-browse` CLI like an earlier version of this module did - a
//! subprocess depends on a host binary being on `PATH`, invisible from
//! inside a Flatpak sandbox (see `feedback_xeneon_flatpak_sandboxing` in
//! the project's own memory notes; the same class of bug was already fixed
//! once for the GNOME-extension-detection code in the old Python version).
//! `ServiceBrowserNew`/`ItemNew`/`AllForNow` are asynchronous by nature
//! (mDNS is a broadcast protocol, there's no synchronous "list everything
//! right now" call) - `discover_avahi` below pumps a short-lived private
//! `GMainContext` by hand to turn that into the same blocking
//! `Vec<String>` contract this module's callers already expect, rather
//! than making `discover_all`/`discover_hue_bridges` async all the way up
//! through `settings_page.rs`.
//!
//! All bridge-facing HTTP goes over `https://<ip>/...` to a Hue bridge's
//! own self-signed certificate, which a normal TLS verifier will always
//! reject. `bridge_tls_config()` below disables certificate verification
//! for exactly those requests - `ureq` 3.x supports this natively
//! (`TlsConfig::builder().disable_verification(true)`, already available
//! through the `rustls` feature this crate enables by default, no new
//! dependency needed). This is only ever attached to requests whose URL is
//! `https://<ip we were told is a Hue bridge>/...` - never to a general-
//! purpose request - the same scoping the reference extension's own
//! per-purpose `Soup.Session` achieves with its `TlsDatabaseBridge`.
//!
//! Pairing (`pair`) still goes through the legacy `/api` endpoint (`POST
//! {"devicetype", "generateclientkey": true}`) even though everything
//! after that uses the newer CLIP v2 REST API (`/clip/v2/resource`) - that
//! split is a Hue bridge quirk, not a choice made here (the reference
//! extension's `createUser`/`getAll` show the same split). The physical
//! link button on the bridge must have been pressed within the last ~30
//! seconds for `pair` to succeed; until then the bridge answers with a
//! well-known error type `101`, surfaced here as
//! `PairError::LinkButtonNotPressed` so the settings UI can show "press
//! the button" and retry rather than a generic failure.

use gtk::gio;
use gtk::glib;
use gtk::glib::prelude::*;
use log::{debug, warn};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use ureq::tls::TlsConfig;

use crate::widgets::system_info::read_hostname;

const HUE_MDNS_SERVICE: &str = "_hue._tcp";
const CLOUD_DISCOVERY_URL: &str = "https://discovery.meethue.com/";

// --- Avahi D-Bus discovery (see the module doc comment) ---
const AVAHI_BUS_NAME: &str = "org.freedesktop.Avahi";
const AVAHI_SERVER_PATH: &str = "/";
const AVAHI_SERVER_INTERFACE: &str = "org.freedesktop.Avahi.Server";
const AVAHI_SERVICE_BROWSER_INTERFACE: &str = "org.freedesktop.Avahi.ServiceBrowser";
/// `AVAHI_IF_UNSPEC`/`AVAHI_PROTO_INET` from `avahi-common/defs.h` - browse
/// every network interface, IPv4 only (this module never resolves IPv6
/// bridges, matching the old `avahi-browse`-based parser's own filter).
const AVAHI_IF_UNSPEC: i32 = -1;
const AVAHI_PROTO_INET: i32 = 0;
const AVAHI_DOMAIN: &str = "local";
const AVAHI_DBUS_CALL_TIMEOUT_MS: i32 = 2000;
/// How long to keep the service browser open collecting `ItemNew` replies
/// before giving up and returning whatever was found - mirrors
/// `avahi-browse -t`'s own "dump what's there, then stop" behavior rather
/// than waiting indefinitely for an `AllForNow` a misbehaving avahi-daemon
/// might never send.
const AVAHI_BROWSE_WINDOW: Duration = Duration::from_millis(1500);
const AVAHI_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Local network calls (avahi already resolved the IP, or the user typed
/// it in) - short, since a real bridge on the LAN answers in milliseconds
/// and a long timeout here would just make a wrong/unreachable IP feel
/// like a hang.
const BRIDGE_TIMEOUT_SECONDS: u64 = 3;
/// The one call that leaves the LAN (`discoverBridgesCloud`'s equivalent) -
/// a little more slack than the bridge calls.
const CLOUD_TIMEOUT_SECONDS: u64 = 5;
/// Hue's `devicetype` field is capped at 40 characters by the bridge -
/// trimming the hostname keeps `xeneon#<hostname>` safely under that no
/// matter how long the machine's hostname is.
const MAX_DEVICETYPE_HOSTNAME_CHARS: usize = 24;

/// A bridge found on the network, not yet paired with. `name` is the
/// bridge's own reported name (from `/api/config`) when it answered -
/// `None` if the IP came from discovery but the bridge didn't respond to
/// the follow-up probe (e.g. it went offline between being discovered and
/// being probed), shown as a plain IP in that case rather than dropped,
/// since the IP alone is still enough for the user to pick it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredBridge {
    pub ip: String,
    pub name: Option<String>,
}

pub enum PairError {
    /// The bridge is reachable but its physical link button hasn't been
    /// pressed recently enough (Hue's own error type `101`) - not a real
    /// failure, just "not yet", so the caller should show the button
    /// prompt and retry rather than surfacing this as an error.
    LinkButtonNotPressed,
    Network(String),
}

pub struct Paired {
    pub username: String,
    pub clientkey: String,
}

/// The devicetype string sent to `/api` when pairing - identifies this app
/// to the bridge (shown in the bridge's own "whitelist" of paired apps).
/// Mirrors the reference extension's `gnome-smart-home#<hostname>`.
fn device_type() -> String {
    let hostname = read_hostname();
    let trimmed: String = hostname.chars().take(MAX_DEVICETYPE_HOSTNAME_CHARS).collect();
    format!("xeneon#{trimmed}")
}

/// TLS config that skips certificate verification - see the module doc
/// comment for why this is safe here (only ever attached to a request
/// whose URL is already known to be a Hue bridge's own `https://<ip>/...`,
/// never to an arbitrary or user-supplied URL) and never used for anything
/// else, e.g. `discover_cloud`'s call to Philips' own cloud service keeps
/// normal certificate verification.
///
/// Every call site pairs this with `.max_redirects(0)` on the same
/// request builder (audit finding 2026-09-29, HIGH): with verification
/// disabled, whatever actually answers at that IP is trusted
/// unconditionally, so if it replies with a redirect, `ureq`'s default
/// of following up to 10 hops - forwarding any custom header, including
/// `hue-application-key` on the authenticated calls, to wherever the
/// `Location` points - would hand the long-lived bridge token to an
/// attacker-controlled host. The Hue CLIP API never legitimately
/// redirects, so refusing to follow any is a pure hardening with no
/// functional cost; with `max_redirects(0)` the raw (unfollowed) 3xx
/// response is returned rather than erroring, and every caller here
/// already treats a non-JSON/unexpected body as a normal failure.
fn bridge_tls_config() -> TlsConfig {
    TlsConfig::builder().disable_verification(true).build()
}

/// Local mDNS discovery via Avahi's own D-Bus API (see the module doc
/// comment) - returns an empty list (not an error) both when no bridge is
/// found and when avahi-daemon itself isn't reachable (not installed, not
/// running, or - once packaged - outside what the sandbox's system-bus
/// proxy allows), since either way `discover_all` should just fall back to
/// cloud discovery rather than the caller having to tell those cases
/// apart.
fn discover_avahi() -> Vec<String> {
    // A private `GMainContext`, used only for the duration of this call -
    // needed because `ItemNew`/`AllForNow` are ordinary async D-Bus
    // signals, dispatched through whichever context was thread-default
    // when the connection was created; a `gio::spawn_blocking` worker
    // thread (where this runs, see `settings_page.rs`) has no main loop of
    // its own running by default, so without this the signals would simply
    // never arrive no matter how long this function waited.
    let context = glib::MainContext::new();
    match context.with_thread_default(|| discover_avahi_on_context(&context)) {
        Ok(ips) => ips,
        Err(err) => {
            debug!("failed to acquire a private GMainContext for Avahi discovery: {err}");
            Vec::new()
        }
    }
}

/// The actual discovery logic, run with `context` already pushed as this
/// thread's default (see `discover_avahi`).
fn discover_avahi_on_context(context: &glib::MainContext) -> Vec<String> {
    let connection = match gio::bus_get_sync(gio::BusType::System, None::<&gio::Cancellable>) {
        Ok(connection) => connection,
        Err(err) => {
            debug!("no system bus connection for Avahi discovery: {err}");
            return Vec::new();
        }
    };

    let browser_reply = match connection.call_sync(
        Some(AVAHI_BUS_NAME),
        AVAHI_SERVER_PATH,
        AVAHI_SERVER_INTERFACE,
        "ServiceBrowserNew",
        Some(&(AVAHI_IF_UNSPEC, AVAHI_PROTO_INET, HUE_MDNS_SERVICE, AVAHI_DOMAIN, 0u32).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        AVAHI_DBUS_CALL_TIMEOUT_MS,
        None::<&gio::Cancellable>,
    ) {
        Ok(reply) => reply,
        Err(err) => {
            // The expected case on most machines with no avahi-daemon
            // running - not worth a warning, cloud discovery covers it.
            debug!("Avahi ServiceBrowserNew unavailable: {err}");
            return Vec::new();
        }
    };
    let Some((browser_path,)) = browser_reply.get::<(glib::variant::ObjectPath,)>() else {
        warn!("Avahi ServiceBrowserNew reply had an unexpected shape");
        return Vec::new();
    };
    let browser_path = browser_path.to_string();

    let found: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let all_for_now = Rc::new(Cell::new(false));

    // Both subscriptions unsubscribe automatically when dropped at the end
    // of this function (`SignalSubscription`'s own `Drop` impl) - no manual
    // cleanup needed, just keep them alive until then.
    let _item_new_subscription = {
        let found = found.clone();
        // A separate clone from the receiver below, on purpose: capturing
        // `connection` itself in this `move` closure would conflict with
        // the borrow `subscribe_to_signal` needs on it to make this very
        // call.
        let connection_for_resolve = connection.clone();
        connection.subscribe_to_signal(
            Some(AVAHI_BUS_NAME),
            Some(AVAHI_SERVICE_BROWSER_INTERFACE),
            Some("ItemNew"),
            Some(&browser_path),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let Some((interface, protocol, name, service_type, domain, _flags)) =
                    signal.parameters.get::<(i32, i32, String, String, String, u32)>()
                else {
                    return;
                };
                if let Some(ip) =
                    resolve_hue_ipv4(&connection_for_resolve, interface, protocol, &name, &service_type, &domain)
                {
                    found.borrow_mut().push(ip);
                }
            },
        )
    };
    let _all_for_now_subscription = {
        let all_for_now = all_for_now.clone();
        connection.subscribe_to_signal(
            Some(AVAHI_BUS_NAME),
            Some(AVAHI_SERVICE_BROWSER_INTERFACE),
            Some("AllForNow"),
            Some(&browser_path),
            None,
            gio::DBusSignalFlags::NONE,
            move |_signal| all_for_now.set(true),
        )
    };

    // `iteration(false)` never blocks, so this loop's own `sleep` is what
    // caps CPU use while waiting - matches `avahi-browse -t`'s short
    // "dump what's there" window instead of blocking indefinitely on a
    // `AllForNow` that might never come.
    let deadline = std::time::Instant::now() + AVAHI_BROWSE_WINDOW;
    while !all_for_now.get() && std::time::Instant::now() < deadline {
        context.iteration(false);
        std::thread::sleep(AVAHI_POLL_INTERVAL);
    }

    let _ = connection.call_sync(
        Some(AVAHI_BUS_NAME),
        &browser_path,
        AVAHI_SERVICE_BROWSER_INTERFACE,
        "Free",
        None,
        None,
        gio::DBusCallFlags::NONE,
        AVAHI_DBUS_CALL_TIMEOUT_MS,
        None::<&gio::Cancellable>,
    );

    let mut ips = found.borrow().clone();
    ips.sort();
    ips.dedup();
    ips
}

/// Resolves one `ItemNew` result (interface/protocol/name/type/domain, the
/// same five fields `ServiceBrowserNew` was given back scoped to one
/// discovered instance) into its IPv4 address via Avahi's `ResolveService`,
/// or `None` for anything that isn't a resolvable IPv4 record (an IPv6-only
/// responder, a service that vanished between being announced and resolved,
/// etc.) - the D-Bus equivalent of the old text-based `parse_avahi_line`,
/// except every field already arrives typed instead of needing to be
/// parsed out of a semicolon-separated line.
fn resolve_hue_ipv4(
    connection: &gio::DBusConnection,
    interface: i32,
    protocol: i32,
    name: &str,
    service_type: &str,
    domain: &str,
) -> Option<String> {
    let reply = connection
        .call_sync(
            Some(AVAHI_BUS_NAME),
            AVAHI_SERVER_PATH,
            AVAHI_SERVER_INTERFACE,
            "ResolveService",
            Some(&(interface, protocol, name, service_type, domain, AVAHI_PROTO_INET, 0u32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            AVAHI_DBUS_CALL_TIMEOUT_MS,
            None::<&gio::Cancellable>,
        )
        .ok()?;
    // Reply shape: (interface, protocol, name, type, domain, host_name,
    // address_protocol, address, port, txt, flags) - only
    // address_protocol/address are needed here.
    let (_, _, _, _, _, _, address_protocol, address, ..) =
        reply.get::<(i32, i32, String, String, String, String, i32, String, u16, Vec<Vec<u8>>, u32)>()?;
    if address_protocol == AVAHI_PROTO_INET && !address.is_empty() {
        Some(address)
    } else {
        None
    }
}

/// Philips' own cloud discovery (`N-UPnP`) - lists bridges registered on
/// whatever local network reached `discovery.meethue.com`. Needs a moment
/// of real internet access to run (unlike every other call in this
/// module, which stays entirely on the LAN), but only to *find* the
/// bridge's IP - pairing and all control afterward is local-only.
fn discover_cloud() -> Vec<String> {
    let response = ureq::get(CLOUD_DISCOVERY_URL)
        .config()
        .timeout_global(Some(Duration::from_secs(CLOUD_TIMEOUT_SECONDS)))
        .build()
        .call();

    let mut response = match response {
        Ok(response) => response,
        Err(err) => {
            debug!("cloud Hue discovery unavailable: {err}");
            return Vec::new();
        }
    };

    let body = match response.body_mut().read_to_string() {
        Ok(body) => body,
        Err(err) => {
            warn!("failed to read cloud discovery response: {err}");
            return Vec::new();
        }
    };

    let json: serde_json::Value = match serde_json::from_str(&body) {
        Ok(json) => json,
        Err(err) => {
            warn!("failed to parse cloud discovery response: {err}");
            return Vec::new();
        }
    };

    json.as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("internalipaddress")?.as_str().map(str::to_string))
        .collect()
}

/// Asks a candidate IP directly whether it's a Hue bridge, via the one
/// endpoint (`/api/config`) that answers without any pairing - returns its
/// reported name on success, `None` if the IP didn't answer at all (not a
/// bridge, or offline since being discovered).
fn probe_bridge_name(ip: &str) -> Option<String> {
    let response = ureq::get(format!("https://{ip}/api/config"))
        .config()
        .tls_config(bridge_tls_config())
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(BRIDGE_TIMEOUT_SECONDS)))
        .build()
        .call();

    let mut response = response.ok()?;
    let body = response.body_mut().read_to_string().ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    json.get("name").and_then(|v| v.as_str()).map(str::to_string)
}

/// Whether `ip` (a dotted-quad string) falls in a private (RFC1918)
/// range - `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`. A Hue bridge
/// is a LAN device; nothing claiming to be one from outside those ranges
/// is trusted. Audit finding 2026-09-29 (Medium): `discover_cloud`'s
/// response is unvalidated data from an external network response
/// (`discovery.meethue.com`), previously fed straight into
/// `probe_bridge_name` - which disables TLS certificate verification for
/// whatever answers at that address - with no check that it's even
/// plausibly a local device. Also used by `settings_page.rs`'s
/// `is_valid_ipv4` to close the same gap for a manually-typed IP.
pub fn is_private_ipv4(ip: &str) -> bool {
    let octets: Vec<u8> = ip.split('.').filter_map(|part| part.parse::<u8>().ok()).collect();
    if octets.len() != 4 {
        return false;
    }
    let (a, b) = (octets[0], octets[1]);
    a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168)
}

/// Runs both discovery methods, merges the IPs (deduped), keeps only the
/// ones in a private range (see `is_private_ipv4`), and probes each
/// surviving one for its bridge name - the one function Settings'
/// discovery button actually calls. Blocking (network + a subprocess),
/// so the caller must run this off the GTK main thread
/// (`gio::spawn_blocking`, same as every blocking call `weather.rs`
/// makes - see that module's own doc comment).
pub fn discover_all() -> Vec<DiscoveredBridge> {
    let mut ips = discover_avahi();
    for ip in discover_cloud() {
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    ips.retain(|ip| is_private_ipv4(ip));

    ips.into_iter().map(|ip| DiscoveredBridge { name: probe_bridge_name(&ip), ip }).collect()
}

/// Pairs with the bridge at `ip` - only succeeds if its physical link
/// button was pressed within the last ~30 seconds (Hue's own rule, not
/// this app's). Blocking, same threading contract as `discover_all`.
pub fn pair(ip: &str) -> Result<Paired, PairError> {
    let body = serde_json::json!({
        "devicetype": device_type(),
        "generateclientkey": true,
    });

    let response = ureq::post(format!("https://{ip}/api"))
        .config()
        .tls_config(bridge_tls_config())
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(BRIDGE_TIMEOUT_SECONDS)))
        .build()
        .send_json(&body);

    let mut response = response.map_err(|err| PairError::Network(err.to_string()))?;
    let body = response.body_mut().read_to_string().map_err(|err| PairError::Network(err.to_string()))?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|err| PairError::Network(err.to_string()))?;

    let entry = json.as_array().and_then(|a| a.first()).ok_or_else(|| PairError::Network("empty response".to_string()))?;

    if let Some(success) = entry.get("success") {
        let username = success.get("username").and_then(|v| v.as_str()).ok_or_else(|| PairError::Network("missing username".to_string()))?;
        let clientkey = success.get("clientkey").and_then(|v| v.as_str()).unwrap_or_default();
        return Ok(Paired { username: username.to_string(), clientkey: clientkey.to_string() });
    }

    if let Some(error) = entry.get("error") {
        let error_type = error.get("type").and_then(|v| v.as_i64());
        if error_type == Some(101) {
            return Err(PairError::LinkButtonNotPressed);
        }
        let description = error.get("description").and_then(|v| v.as_str()).unwrap_or("unknown error");
        return Err(PairError::Network(description.to_string()));
    }

    Err(PairError::Network("unexpected response shape".to_string()))
}

/// Raw CLIP v2 resource list (`/clip/v2/resource`) - every light, room,
/// zone, grouped_light etc. the bridge knows about, as the bridge's own
/// JSON shape. Left untyped for now: step 1's goal is a validated
/// discover -> pair -> read pipeline, not the light/room model the actual
/// widget will want - that typed layer belongs with the widget itself
/// (step 2), once its exact field needs (color mode, on/off, brightness,
/// room membership) are being written against real code, not guessed
/// ahead of time.
/// The Hue CLIP v2 API answers every request - success or a
/// resource-level failure alike - with HTTP 200 and a body shaped
/// `{"errors": [...], "data": [...]}`; a resource that no longer exists
/// (deleted on the bridge since this app's last fetch), an out-of-range
/// value, or similar comes back this way rather than as an HTTP error
/// status. Audit finding 2026-09-29 (PLAUSIBLE): every `set_*` PUT below
/// used to only check the transport-level `Result` from `ureq`, never
/// this body, so a bridge-level failure silently read as success. Called
/// by each of them after `send_json` succeeds transport-wise.
fn check_clip_errors(body: &str) -> Result<(), String> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|err| err.to_string())?;
    let errors = json.get("errors").and_then(|v| v.as_array()).map(Vec::as_slice).unwrap_or(&[]);
    if errors.is_empty() {
        return Ok(());
    }
    let messages: Vec<String> =
        errors.iter().filter_map(|e| e.get("description").and_then(|d| d.as_str()).map(str::to_string)).collect();
    Err(if messages.is_empty() { "bridge reported an error with no description".to_string() } else { messages.join("; ") })
}

pub fn fetch_resources(ip: &str, username: &str) -> Result<serde_json::Value, String> {
    let response = ureq::get(format!("https://{ip}/clip/v2/resource"))
        .header("hue-application-key", username)
        .config()
        .tls_config(bridge_tls_config())
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(BRIDGE_TIMEOUT_SECONDS)))
        .build()
        .call();

    let mut response = response.map_err(|err| err.to_string())?;
    let body = response.body_mut().read_to_string().map_err(|err| err.to_string())?;
    serde_json::from_str(&body).map_err(|err| err.to_string())
}

/// What a light can be told to do - which of these three depends on
/// whether the light resource carries a `color` field (full color, "gamut
/// C" in Hue's own terms), a `color_temperature` field but no `color`
/// (white ambiance, warm-to-cool only), or neither (a plain dimmable white
/// bulb, brightness only). Presence of those fields, not the bridge's
/// `metadata.archetype` string, is what actually determines capability -
/// same check the reference GNOME extension effectively makes by just
/// reading whichever fields are there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulbType {
    Color,
    ColorTemperature,
    Dimmable,
}

/// One light, already resolved against its owning room - the shape the
/// widget actually wants, as opposed to `fetch_resources`'s raw bridge
/// JSON. `display_color_hex` is an approximation for the on-card dot/icon
/// tint, not a color-accurate conversion - good enough to tell a violet
/// light from a warm-white one at a glance, not meant for anything more
/// precise.
#[derive(Debug, Clone)]
pub struct Light {
    pub id: String,
    pub name: String,
    pub room_name: Option<String>,
    pub on: bool,
    /// 0.0-100.0, whatever the bridge last reported - meaningless while
    /// `on` is `false` (Hue keeps reporting the brightness a light will
    /// return to, not 0).
    pub brightness_percent: f64,
    pub bulb_type: BulbType,
    pub display_color_hex: String,
    /// Only `Some` for `BulbType::ColorTemperature` - the widget's status
    /// text (e.g. "68% · 2700K") converts this to Kelvin itself rather
    /// than storing a pre-formatted string here, keeping this a plain data
    /// type with no display concerns of its own.
    pub mirek: Option<f64>,
}

/// A room, resolved enough for the widget to show and control it as one
/// unit (step 3's "par pièce" mode): its own light ids (for the settings
/// picker to describe what's inside, and to compute an aggregate on-count
/// elsewhere if ever needed), plus the room's `grouped_light` resource -
/// the bridge's own way to address "every light in this room" in one PUT -
/// and that group's current on/brightness state, read exactly like a
/// single light's own `on`/`dimming` fields.
///
/// `grouped_light_id` is `None` for a room with no lights at all (the
/// bridge still creates the room resource, but never a `grouped_light`
/// service for it) - such a room is filtered out before it ever reaches
/// the widget (see `parse_rooms`), since there'd be nothing to control.
#[derive(Debug, Clone)]
pub struct Room {
    pub id: String,
    pub name: String,
    pub light_ids: Vec<String>,
    pub grouped_light_id: String,
    pub on: bool,
    pub brightness_percent: f64,
}

/// Converts a CIE xy chromaticity + brightness into an approximate sRGB
/// hex color - the standard conversion Philips' own SDK documents and
/// every third-party Hue integration reimplements (XYZ via the xy+Y
/// values, then the Wide RGB D65 matrix, then gamma-correct and clamp).
/// Only used for `Light::display_color_hex`'s cosmetic dot/icon tint, not
/// for anything sent back to the bridge, so the minor inaccuracy real
/// color-management would fix here doesn't matter.
pub fn xy_to_hex(x: f64, y: f64, brightness_percent: f64) -> String {
    let y_val = (brightness_percent / 100.0).clamp(0.01, 1.0);
    let z = 1.0 - x - y;
    let x_val = (y_val / y.max(0.0001)) * x;
    let z_val = (y_val / y.max(0.0001)) * z;

    let r = x_val * 1.656_492 - y_val * 0.354_851 - z_val * 0.255_038;
    let g = -x_val * 0.707_196 + y_val * 1.655_397 + z_val * 0.036_152;
    let b = x_val * 0.051_713 - y_val * 0.121_364 + z_val * 1.011_530;

    let gamma_correct = |c: f64| {
        let c = if c <= 0.0 { 0.0 } else { c };
        if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
    };
    let to_byte = |c: f64| (gamma_correct(c).clamp(0.0, 1.0) * 255.0).round() as u8;

    format!("#{:02x}{:02x}{:02x}", to_byte(r), to_byte(g), to_byte(b))
}

/// Approximates a color-temperature light's on-card tint by linearly
/// interpolating between a warm and a cool swatch across the mirek range
/// Hue bulbs actually support (roughly 153-500 mirek, ~6500K-2000K) -
/// deliberately not a real blackbody-radiation calculation (see
/// `xy_to_hex`'s own doc comment on why cosmetic accuracy isn't the goal
/// here), just enough to make a warm light look warm and a cool one look
/// cool next to each other on the same card.
pub fn mirek_to_hex(mirek: f64) -> String {
    const WARM: (f64, f64, f64) = (0xf2 as f64, 0xa5 as f64, 0x41 as f64); // network_sq/system_sq's own accent amber
    const COOL: (f64, f64, f64) = (0xbc as f64, 0xdc as f64, 0xff as f64);
    const MIN_MIREK: f64 = 153.0;
    const MAX_MIREK: f64 = 500.0;

    let t = ((mirek - MIN_MIREK) / (MAX_MIREK - MIN_MIREK)).clamp(0.0, 1.0);
    let lerp = |a: f64, b: f64| (a + (b - a) * t).round() as u8;
    format!("#{:02x}{:02x}{:02x}", lerp(COOL.0, WARM.0), lerp(COOL.1, WARM.1), lerp(COOL.2, WARM.2))
}

/// Plain white/dimmable bulbs have no chromaticity data at all to derive a
/// color from - a fixed warm swatch (matching `mirek_to_hex`'s own warm
/// end) reads better than an arbitrary neutral gray, since every Hue
/// "White" bulb is physically ~2700K.
const DIMMABLE_COLOR_HEX: &str = "#f2a541";

/// Parses the raw `/clip/v2/resource` payload into the light list the
/// widget actually wants - resolving each light's owning room along the
/// way (`room -> children (devices) -> device's own `services` -> light
/// id`, the only path CLIP v2 exposes from a room down to its lights;
/// there's no direct "room contains these light ids" field on the room
/// resource itself).
pub fn parse_lights(resources: &serde_json::Value) -> Vec<Light> {
    let data = resources.get("data").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    // device id -> light id, from every device's own `services` list.
    let mut device_to_light: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for entry in &data {
        if entry.get("type").and_then(|v| v.as_str()) != Some("device") {
            continue;
        }
        let Some(device_id) = entry.get("id").and_then(|v| v.as_str()) else { continue };
        let services = entry.get("services").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        for service in services {
            if service.get("rtype").and_then(|v| v.as_str()) == Some("light") {
                if let Some(light_id) = service.get("rid").and_then(|v| v.as_str()) {
                    device_to_light.insert(device_id.to_string(), light_id.to_string());
                }
            }
        }
    }

    // light id -> room name, from every room's own `children` (devices).
    let mut light_to_room: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for entry in &data {
        if entry.get("type").and_then(|v| v.as_str()) != Some("room") {
            continue;
        }
        let Some(room_name) = entry.get("metadata").and_then(|m| m.get("name")).and_then(|v| v.as_str()) else { continue };
        let children = entry.get("children").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        for child in children {
            if child.get("rtype").and_then(|v| v.as_str()) != Some("device") {
                continue;
            }
            let Some(device_id) = child.get("rid").and_then(|v| v.as_str()) else { continue };
            if let Some(light_id) = device_to_light.get(device_id) {
                light_to_room.insert(light_id.clone(), room_name.to_string());
            }
        }
    }

    data.iter()
        .filter(|entry| entry.get("type").and_then(|v| v.as_str()) == Some("light"))
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?.to_string();
            let name = entry.get("metadata")?.get("name")?.as_str()?.to_string();
            let on = entry.get("on").and_then(|v| v.get("on")).and_then(|v| v.as_bool()).unwrap_or(false);
            let brightness_percent = entry.get("dimming").and_then(|v| v.get("brightness")).and_then(|v| v.as_f64()).unwrap_or(0.0);

            let color = entry.get("color").and_then(|v| v.get("xy"));
            let mirek = entry.get("color_temperature").and_then(|v| v.get("mirek")).and_then(|v| v.as_f64());

            let (bulb_type, display_color_hex) = match (color, mirek) {
                (Some(xy), _) => {
                    let x = xy.get("x").and_then(|v| v.as_f64()).unwrap_or(0.33);
                    let y = xy.get("y").and_then(|v| v.as_f64()).unwrap_or(0.33);
                    (BulbType::Color, xy_to_hex(x, y, brightness_percent.max(20.0)))
                }
                (None, Some(mirek)) => (BulbType::ColorTemperature, mirek_to_hex(mirek)),
                (None, None) => (BulbType::Dimmable, DIMMABLE_COLOR_HEX.to_string()),
            };

            // Only kept for `ColorTemperature` - an extended-color light
            // often reports both `color` and `color_temperature`, but it
            // took the `Color` branch above, and this field's own doc
            // comment promises it's `None` for anything but a
            // color-temperature-only bulb.
            let stored_mirek = if bulb_type == BulbType::ColorTemperature { mirek } else { None };

            Some(Light {
                room_name: light_to_room.get(&id).cloned(),
                id,
                name,
                on,
                brightness_percent,
                bulb_type,
                display_color_hex,
                mirek: stored_mirek,
            })
        })
        .collect()
}

/// Resolves every room with at least one light into a `Room`, in
/// room-discovery order (whatever order the bridge listed `room` resources
/// in) - a light with no resolved room (rare: only possible if a device
/// belongs to no room at all) simply doesn't appear in any
/// `Room::light_ids`, not dropped from `lights` itself. A room with no
/// `grouped_light` service (only possible for a room with zero lights) is
/// skipped entirely - see `Room`'s own doc comment on why.
pub fn parse_rooms(resources: &serde_json::Value, lights: &[Light]) -> Vec<Room> {
    let data = resources.get("data").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    // grouped_light id -> (on, brightness), same shape `parse_lights`
    // reads off a plain light resource's own `on`/`dimming` fields.
    let mut grouped_light_state: std::collections::HashMap<String, (bool, f64)> = std::collections::HashMap::new();
    for entry in &data {
        if entry.get("type").and_then(|v| v.as_str()) != Some("grouped_light") {
            continue;
        }
        let Some(id) = entry.get("id").and_then(|v| v.as_str()) else { continue };
        let on = entry.get("on").and_then(|v| v.get("on")).and_then(|v| v.as_bool()).unwrap_or(false);
        let brightness = entry.get("dimming").and_then(|v| v.get("brightness")).and_then(|v| v.as_f64()).unwrap_or(0.0);
        grouped_light_state.insert(id.to_string(), (on, brightness));
    }

    data.iter()
        .filter(|entry| entry.get("type").and_then(|v| v.as_str()) == Some("room"))
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?.to_string();
            let name = entry.get("metadata")?.get("name")?.as_str()?.to_string();
            let grouped_light_id = entry
                .get("services")?
                .as_array()?
                .iter()
                .find(|service| service.get("rtype").and_then(|v| v.as_str()) == Some("grouped_light"))?
                .get("rid")?
                .as_str()?
                .to_string();
            let (on, brightness_percent) = grouped_light_state.get(&grouped_light_id).copied().unwrap_or((false, 0.0));
            let light_ids = lights.iter().filter(|light| light.room_name.as_deref() == Some(name.as_str())).map(|light| light.id.clone()).collect();
            Some(Room { id, name, light_ids, grouped_light_id, on, brightness_percent })
        })
        .collect()
}

/// Turns one light on/off and (in the same request) sets its brightness -
/// combined into a single PUT rather than two, since setting brightness on
/// an off light doesn't implicitly turn it on (confirmed against the CLIP
/// v2 API), and the card's brightness-bar tap needs exactly that: tapping
/// a bar on an off light should both turn it on and set the tapped level.
/// `on` and `brightness_percent` are still independent (a plain on/off
/// toggle passes the light's last-known brightness back unchanged) -
/// combining the request is only about it always being one PUT, not about
/// the two fields being coupled.
pub fn set_light(ip: &str, username: &str, light_id: &str, on: bool, brightness_percent: f64) -> Result<(), String> {
    let body = serde_json::json!({
        "on": {"on": on},
        "dimming": {"brightness": brightness_percent.clamp(0.0, 100.0)},
    });

    let mut response = ureq::put(format!("https://{ip}/clip/v2/resource/light/{light_id}"))
        .header("hue-application-key", username)
        .config()
        .tls_config(bridge_tls_config())
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(BRIDGE_TIMEOUT_SECONDS)))
        .build()
        .send_json(&body)
        .map_err(|err| err.to_string())?;

    let body = response.body_mut().read_to_string().map_err(|err| err.to_string())?;
    check_clip_errors(&body)
}

/// Same as `set_light`, but addresses a room's `grouped_light` resource
/// instead - every light in the room picks up the change at once. Kept as
/// a separate function rather than a `group: bool` parameter on
/// `set_light`: the two hit different resource paths
/// (`/resource/light/<id>` vs `/resource/grouped_light/<id>`) and nothing
/// else about the request differs, so a shared body-building helper would
/// save a few lines at the cost of a less obvious call site - not a good
/// trade for two call sites this small.
pub fn set_grouped_light(ip: &str, username: &str, grouped_light_id: &str, on: bool, brightness_percent: f64) -> Result<(), String> {
    let body = serde_json::json!({
        "on": {"on": on},
        "dimming": {"brightness": brightness_percent.clamp(0.0, 100.0)},
    });

    let mut response = ureq::put(format!("https://{ip}/clip/v2/resource/grouped_light/{grouped_light_id}"))
        .header("hue-application-key", username)
        .config()
        .tls_config(bridge_tls_config())
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(BRIDGE_TIMEOUT_SECONDS)))
        .build()
        .send_json(&body)
        .map_err(|err| err.to_string())?;

    let body = response.body_mut().read_to_string().map_err(|err| err.to_string())?;
    check_clip_errors(&body)
}

/// A single-light PUT (`resource_kind = "light"`) or a whole-room one
/// (`resource_kind = "grouped_light"`) touching only `color` - unlike
/// `set_light`/`set_grouped_light`, deliberately doesn't also send
/// `on`/`dimming`: the color/temperature popover (step 4) only opens for
/// an already-on light (see `widgets/hue.rs`'s own gesture wiring), so
/// there's nothing useful to also set there. One function for both
/// resource kinds (rather than `set_light_color`/`set_grouped_light_color`
/// pair, mirroring `set_light`/`set_grouped_light`'s own split) since the
/// request bodies are now identical enough (both singleton `color` PUTs)
/// that a `resource_kind: &str` parameter reads more clearly than two
/// near-duplicate functions would.
pub fn set_color_xy(ip: &str, username: &str, resource_kind: &str, id: &str, x: f64, y: f64) -> Result<(), String> {
    let body = serde_json::json!({"color": {"xy": {"x": x, "y": y}}});

    let mut response = ureq::put(format!("https://{ip}/clip/v2/resource/{resource_kind}/{id}"))
        .header("hue-application-key", username)
        .config()
        .tls_config(bridge_tls_config())
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(BRIDGE_TIMEOUT_SECONDS)))
        .build()
        .send_json(&body)
        .map_err(|err| err.to_string())?;

    let body = response.body_mut().read_to_string().map_err(|err| err.to_string())?;
    check_clip_errors(&body)
}

/// Same shape as `set_color_xy`, for a color-temperature-only bulb
/// (`resource_kind` still either `"light"` or `"grouped_light"`). Unlike
/// `xy` (genuinely fractional in the CLIP v2 schema), `mirek` is declared
/// as a JSON integer there - sent here as a real `u32`, not `mirek.round()`
/// left as an `f64`, so the request body reads `366` rather than `366.0`.
/// A JSON number with a trailing `.0` is still spec-legal where an integer
/// is expected (no fractional part), but embedded firmware parsers don't
/// always agree with the spec on that, and this costs nothing to just get
/// exactly right rather than rely on the bridge being lenient about it.
pub fn set_color_temperature_mirek(ip: &str, username: &str, resource_kind: &str, id: &str, mirek: f64) -> Result<(), String> {
    let mirek = mirek.round().clamp(1.0, u32::MAX as f64) as u32;
    let body = serde_json::json!({"color_temperature": {"mirek": mirek}});

    let mut response = ureq::put(format!("https://{ip}/clip/v2/resource/{resource_kind}/{id}"))
        .header("hue-application-key", username)
        .config()
        .tls_config(bridge_tls_config())
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(BRIDGE_TIMEOUT_SECONDS)))
        .build()
        .send_json(&body)
        .map_err(|err| err.to_string())?;

    let body = response.body_mut().read_to_string().map_err(|err| err.to_string())?;
    check_clip_errors(&body)
}

/// Converts an sRGB color (0.0-1.0 components, e.g. from a
/// `gtk::ColorDialogButton`) to the CIE xy chromaticity the bridge's
/// `color` field wants - the inverse of `xy_to_hex`'s own forward
/// conversion, same Wide RGB D65 matrix (inverted) and gamma handling.
/// Falls back to the D65 white point for a color at/near pure black,
/// where the forward matrix has no meaningful direction to report.
pub fn rgb_to_xy(r: f64, g: f64, b: f64) -> (f64, f64) {
    let inverse_gamma = |c: f64| if c > 0.04045 { ((c + 0.055) / 1.055).powf(2.4) } else { c / 12.92 };
    let (r, g, b) = (inverse_gamma(r), inverse_gamma(g), inverse_gamma(b));

    let x = r * 0.664_511 + g * 0.154_324 + b * 0.162_028;
    let y = r * 0.283_881 + g * 0.668_433 + b * 0.047_685;
    let z = r * 0.000_088 + g * 0.072_310 + b * 0.986_039;

    let sum = x + y + z;
    if sum <= 0.0 {
        return (0.3127, 0.3290); // CIE D65 white point
    }
    (x / sum, y / sum)
}

thread_local! {
    // Set once by main.rs right after `PageIndicator::new`, same pattern
    // (and same reason) as `ha_page.rs`'s own `REVEAL_INDICATOR` - lets a
    // Hue widget card's "no bridge configured" empty state jump straight
    // to Settings without needing carousel/page-indicator access of its
    // own. `None` in the narrow startup window before that wiring runs
    // (nothing to click yet) and forever `None` in a context with no real
    // window at all (unit tests).
    static OPEN_SETTINGS: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}

pub fn set_open_settings(callback: impl Fn() + 'static) {
    OPEN_SETTINGS.with(|cell| *cell.borrow_mut() = Some(Box::new(callback)));
}

pub fn open_settings() {
    OPEN_SETTINGS.with(|cell| {
        if let Some(callback) = cell.borrow().as_ref() {
            callback();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devicetype_stays_under_bridge_limit() {
        assert!(device_type().len() <= 40);
    }

    #[test]
    fn private_ipv4_ranges_are_accepted() {
        assert!(is_private_ipv4("10.0.0.1"));
        assert!(is_private_ipv4("10.255.255.255"));
        assert!(is_private_ipv4("172.16.0.1"));
        assert!(is_private_ipv4("172.31.255.255"));
        assert!(is_private_ipv4("192.168.1.42"));
    }

    #[test]
    fn public_or_malformed_addresses_are_rejected() {
        assert!(!is_private_ipv4("8.8.8.8"));
        assert!(!is_private_ipv4("172.32.0.1")); // just outside 172.16.0.0/12
        assert!(!is_private_ipv4("172.15.255.255")); // just below the range
        assert!(!is_private_ipv4("193.168.1.1")); // not 192.168.*
        assert!(!is_private_ipv4("not.an.ip.address"));
        assert!(!is_private_ipv4("10.0.0"));
        assert!(!is_private_ipv4("300.0.0.1"));
    }

    /// A small but complete CLIP v2 fixture: one room ("Salon") with one
    /// device holding one color light plus its `grouped_light` service,
    /// and one color-temperature light with no room at all (an unassigned
    /// accessory, which does happen in practice) - enough to exercise
    /// every branch of `parse_lights` (color vs color-temperature vs
    /// missing room) and `parse_rooms` in one fixture rather than a
    /// separate one per case.
    fn fixture_resources() -> serde_json::Value {
        serde_json::json!({
            "data": [
                {
                    "type": "room",
                    "id": "room-1",
                    "metadata": {"name": "Salon"},
                    "children": [{"rid": "device-1", "rtype": "device"}],
                    "services": [{"rid": "grouped-1", "rtype": "grouped_light"}]
                },
                {
                    "type": "device",
                    "id": "device-1",
                    "services": [{"rid": "light-1", "rtype": "light"}]
                },
                {
                    "type": "light",
                    "id": "light-1",
                    "metadata": {"name": "Lampe Salon"},
                    "on": {"on": true},
                    "dimming": {"brightness": 72.0},
                    "color": {"xy": {"x": 0.3, "y": 0.15}}
                },
                {
                    "type": "light",
                    "id": "light-2",
                    "metadata": {"name": "Lampe Bureau Grégory"},
                    "on": {"on": false},
                    "dimming": {"brightness": 40.0},
                    "color_temperature": {"mirek": 366.0}
                },
                {
                    "type": "grouped_light",
                    "id": "grouped-1",
                    "on": {"on": true},
                    "dimming": {"brightness": 72.0}
                }
            ]
        })
    }

    #[test]
    fn parses_color_light_with_resolved_room() {
        let lights = parse_lights(&fixture_resources());
        let salon = lights.iter().find(|l| l.id == "light-1").unwrap();
        assert_eq!(salon.name, "Lampe Salon");
        assert_eq!(salon.room_name.as_deref(), Some("Salon"));
        assert!(salon.on);
        assert_eq!(salon.bulb_type, BulbType::Color);
    }

    #[test]
    fn parses_color_temperature_light_with_no_room() {
        let lights = parse_lights(&fixture_resources());
        let bureau = lights.iter().find(|l| l.id == "light-2").unwrap();
        assert_eq!(bureau.room_name, None);
        assert!(!bureau.on);
        assert_eq!(bureau.bulb_type, BulbType::ColorTemperature);
    }

    #[test]
    fn resolves_room_with_its_grouped_light_state() {
        let lights = parse_lights(&fixture_resources());
        let rooms = parse_rooms(&fixture_resources(), &lights);
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].name, "Salon");
        assert_eq!(rooms[0].light_ids, vec!["light-1".to_string()]);
        assert_eq!(rooms[0].grouped_light_id, "grouped-1");
        assert!(rooms[0].on);
        assert_eq!(rooms[0].brightness_percent, 72.0);
    }

    #[test]
    fn room_with_no_grouped_light_service_is_skipped() {
        let resources = serde_json::json!({
            "data": [{
                "type": "room",
                "id": "room-2",
                "metadata": {"name": "Cave"},
                "children": [],
                "services": []
            }]
        });
        assert!(parse_rooms(&resources, &[]).is_empty());
    }

    #[test]
    fn mirek_extremes_land_close_to_their_named_swatch() {
        assert_eq!(mirek_to_hex(500.0), "#f2a541");
        assert_eq!(mirek_to_hex(153.0), "#bcdcff");
    }

    #[test]
    fn rgb_to_xy_lands_near_the_matching_named_primary() {
        // Not an exact round trip with `xy_to_hex` (different color
        // spaces, gamut clamping, rounding) - just checks a primary color
        // resolves to chromaticity in roughly the right direction, since
        // that's the only property the color popover (step 4) actually
        // depends on.
        let (x, y) = rgb_to_xy(1.0, 0.0, 0.0);
        assert!(x > y, "pure red should skew toward the x (red) axis, got ({x}, {y})");
        let (x, y) = rgb_to_xy(0.0, 0.0, 1.0);
        assert!(x < 0.2 && y < 0.2, "pure blue should sit near the xy origin, got ({x}, {y})");
    }

    #[test]
    fn dimmable_light_has_no_color_data_at_all() {
        let resources = serde_json::json!({
            "data": [{
                "type": "light",
                "id": "light-3",
                "metadata": {"name": "Lampe Cave"},
                "on": {"on": true},
                "dimming": {"brightness": 100.0}
            }]
        });
        let lights = parse_lights(&resources);
        assert_eq!(lights[0].bulb_type, BulbType::Dimmable);
        assert_eq!(lights[0].display_color_hex, DIMMABLE_COLOR_HEX);
    }
}
