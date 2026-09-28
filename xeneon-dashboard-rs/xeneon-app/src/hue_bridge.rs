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
//! api.js`, read for reference before writing this): local mDNS via the
//! `avahi-browse` system command as the primary path (same
//! shell-out-to-a-system-tool idiom this project already prefers over a
//! new library dependency, see `CLAUDE.md`'s "minimal deps" note and
//! `system_info.rs`'s own `/proc`/`/sys` reads), with Philips' own cloud
//! discovery endpoint (`https://discovery.meethue.com/`) as a fallback for
//! machines without `avahi-tools` installed. Both run every time
//! `discover_all` is called and their results are merged/deduped by IP -
//! same as the reference extension - rather than picking one and giving up
//! if it comes back empty.
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

use log::{debug, warn};
use std::process::Command;
use std::time::Duration;
use ureq::tls::TlsConfig;

use crate::widgets::system_info::read_hostname;

const HUE_MDNS_SERVICE: &str = "_hue._tcp";
const CLOUD_DISCOVERY_URL: &str = "https://discovery.meethue.com/";
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
fn bridge_tls_config() -> TlsConfig {
    TlsConfig::builder().disable_verification(true).build()
}

/// Parses one `avahi-browse -r -k -p -t` output line into the bridge's
/// IPv4 address, or `None` for a line that isn't a resolved IPv4 record
/// (avahi also reports IPv6 and unresolved/removed entries on other lines,
/// see `avahi-browse(1)`'s `-p` machine-readable format). Split out as its
/// own pure function so it's unit-testable without actually running
/// `avahi-browse`.
///
/// Field layout (semicolon-separated): interface;protocol;name;type;
/// domain;hostname;address;port;txt - same fields the reference GNOME
/// extension's `Avahi._parseLine` reads (field 2 = protocol, 7 = address).
fn parse_avahi_line(line: &str) -> Option<String> {
    let fields: Vec<&str> = line.split(';').collect();
    if fields.len() <= 8 {
        return None;
    }
    if fields[2] != "IPv4" {
        return None;
    }
    let ip = fields[7];
    if ip.is_empty() { None } else { Some(ip.to_string()) }
}

/// Local mDNS discovery via the `avahi-browse` system command - returns an
/// empty list (not an error) both when no bridge is found and when
/// `avahi-browse` itself isn't installed, since either way `discover_all`
/// should just fall back to cloud discovery rather than the caller having
/// to distinguish "not installed" from "installed but nothing found".
fn discover_avahi() -> Vec<String> {
    let output = match Command::new("avahi-browse").args(["-r", "-k", "-p", "-t", HUE_MDNS_SERVICE]).output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            debug!("avahi-browse not installed, skipping local Hue discovery");
            return Vec::new();
        }
        Err(err) => {
            warn!("failed to run avahi-browse: {err}");
            return Vec::new();
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut ips: Vec<String> = stdout.lines().filter_map(parse_avahi_line).collect();
    ips.sort();
    ips.dedup();
    ips
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
        .timeout_global(Some(Duration::from_secs(BRIDGE_TIMEOUT_SECONDS)))
        .build()
        .call();

    let mut response = response.ok()?;
    let body = response.body_mut().read_to_string().ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    json.get("name").and_then(|v| v.as_str()).map(str::to_string)
}

/// Runs both discovery methods, merges the IPs (deduped), and probes each
/// one for its bridge name - the one function Settings' discovery button
/// actually calls. Blocking (network + a subprocess), so the caller must
/// run this off the GTK main thread (`gio::spawn_blocking`, same as every
/// blocking call `weather.rs` makes - see that module's own doc comment).
pub fn discover_all() -> Vec<DiscoveredBridge> {
    let mut ips = discover_avahi();
    for ip in discover_cloud() {
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }

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
pub fn fetch_resources(ip: &str, username: &str) -> Result<serde_json::Value, String> {
    let response = ureq::get(format!("https://{ip}/clip/v2/resource"))
        .header("hue-application-key", username)
        .config()
        .tls_config(bridge_tls_config())
        .timeout_global(Some(Duration::from_secs(BRIDGE_TIMEOUT_SECONDS)))
        .build()
        .call();

    let mut response = response.map_err(|err| err.to_string())?;
    let body = response.body_mut().read_to_string().map_err(|err| err.to_string())?;
    serde_json::from_str(&body).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_resolved_ipv4_line() {
        let line = "=;eth0;IPv4;Philips hue;_hue._tcp;local;Philips-hue.local;192.168.1.42;443;\"bridgeid=001788FFFE123456\"";
        assert_eq!(parse_avahi_line(line), Some("192.168.1.42".to_string()));
    }

    #[test]
    fn ignores_ipv6_line() {
        let line = "=;eth0;IPv6;Philips hue;_hue._tcp;local;Philips-hue.local;fe80::1;443;\"bridgeid=001788FFFE123456\"";
        assert_eq!(parse_avahi_line(line), None);
    }

    #[test]
    fn ignores_short_or_unrelated_lines() {
        assert_eq!(parse_avahi_line(""), None);
        assert_eq!(parse_avahi_line("+;eth0;IPv4;Philips hue;_hue._tcp;local"), None);
    }

    #[test]
    fn devicetype_stays_under_bridge_limit() {
        assert!(device_type().len() <= 40);
    }
}
