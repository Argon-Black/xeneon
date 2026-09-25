// SPDX-License-Identifier: GPL-3.0-or-later
//! System info widget (SQ footprint): hostname + distro badge in a header
//! row, three linear gauges (CPU load, memory, disk usage) below that, and
//! a small footer with uptime, kernel release, shell + architecture and the
//! CPU model - built on top of `system_info.rs`'s read-only data functions
//! and laid out to match the mockup agreed with the user before this widget
//! was built (same "agree a mockup, then build to it" flow `network_sq.rs`'s
//! own doc comment describes).
//!
//! Structurally this mirrors `network_sq.rs`: a header row, then content
//! filling the rest of the card, all built with plain GTK boxes/labels plus
//! a couple of small `gtk::DrawingArea`s painted with Cairo for the gauges -
//! no new widget-drawing technique introduced here. The only functional
//! setting is which mount point the disk gauge reads, since `/` isn't
//! always the partition a user cares about (a separate `/home`, a data
//! drive...).
//!
//! Appearance customization, per the user's own follow-up requests: a
//! single `content_scale` slider (mirrors `temp_gauge.rs`/`network_sq.rs`'s
//! own, defaulting to 130% here) that grows/shrinks the hostname and every
//! gauge's label/value text and the footer lines, plus three independent
//! color pickers, one per gauge, so CPU/memory/disk can each be recolored
//! without needing to match `network_sq.rs`'s down/up palette. The scaled
//! font sizes are applied the same way `network_sq.rs`'s `NAME_CSS`/
//! `apply_content_scale` do: a per-instance CSS class (`FONT_CSS` below)
//! scoping a `font-size` rule to just this card, since a plain shared class
//! can't hold a different pixel size per instance.
//!
//! The hostname's base size (`BASE_HOSTNAME_FONT_PX`) and the OS badge's
//! fixed size (`OS_BADGE_FONT_PX`) deliberately match `network_sq.rs`'s own
//! `BASE_NAME_FONT_PX`/`VPN_BADGE_FONT_PX` - after comparing both cards
//! side by side, the user asked for "LAN"/"Aorus" and the VPN/OS badges to
//! read at the same size across both widgets rather than each picking its
//! own. The OS badge, like `network_sq.rs`'s VPN badge, stays a fixed size
//! rather than joining `content_scale`: neither badge grows with the rest
//! of its card's text.
//!
//! CPU load needs two `/proc/stat` samples to turn into a percentage (see
//! `system_info::CpuTimes::usage_percent_since`), so this widget keeps the
//! previous sample itself (`previous_cpu_times`) the same way
//! `network.rs`'s `NetworkState` keeps a previous byte-counter sample to
//! compute a rate. The very first tick after the widget is placed has
//! nothing to compare against yet, so it shows 0% rather than waiting an
//! extra tick - a one-time, harmless dip that self-corrects on the next
//! refresh.

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Once;

use crate::appearance_css::CssRuleRegistry;
use crate::appearance_popover::{hex_to_rgba, rgba_to_hex};
use crate::i18n_runtime as i18n;
use crate::widgets::registry::WidgetInstance;
use crate::widgets::system_info::{
    read_cpu_model, read_cpu_times, read_disk_usage, read_hostname, read_kernel_release, read_memory,
    read_os_short_name, read_shell_name, read_uptime_seconds, CpuTimes,
};

/// Slower than `cpu_temp.rs`/`network.rs`'s 2s - this widget touches more
/// files per tick (`/proc/stat`, `/proc/meminfo`, `/proc/uptime`, a
/// `statvfs` call, plus the mostly-static hostname/os/kernel/shell/cpu
/// model reads) and none of its values are the kind that need sub-5-second
/// freshness to be useful.
const REFRESH_INTERVAL_SECONDS: u32 = 5;

/// Used whenever the user clears the settings entry back to empty, or on
/// first spawn before any value has been saved.
const DEFAULT_DISK_PATH: &str = "/";

/// Same blue as `network_sq.rs`'s default down-rate/trace color
/// (`DEFAULT_DOWN_COLOR_HEX`) so the two cards read as one palette by
/// default - still just a starting point now that all three are
/// user-editable (see `SystemSqState::cpu_color`).
const DEFAULT_CPU_COLOR_HEX: &str = "#5da9e8";
/// Same coral as `network_sq.rs`'s default up-rate/trace color
/// (`DEFAULT_UP_COLOR_HEX`).
const DEFAULT_MEM_COLOR_HEX: &str = "#e8875d";
/// A third color not already used by a neighbouring widget, picked to read
/// clearly against the same dark card background.
const DEFAULT_DISK_COLOR_HEX: &str = "#8fd3c7";

/// Fixed - same value as `network_sq.rs`'s `VPN_BADGE_FONT_PX`, and not
/// part of `content_scale` for the same reason that badge isn't either
/// (see the module doc comment).
const OS_BADGE_FONT_PX: i32 = 15;
/// Base sizes at `content_scale == 1.0` (100%) for every bit of text that
/// *is* resizable - the hostname, the three gauges' labels/values, and the
/// four footer lines. Same "BASE_* times content_scale" technique as
/// `temp_gauge.rs`. `BASE_HOSTNAME_FONT_PX` matches `network_sq.rs`'s
/// `BASE_NAME_FONT_PX` (see the module doc comment).
const BASE_HOSTNAME_FONT_PX: f64 = 17.0;
const BASE_GAUGE_LABEL_FONT_PX: f64 = 14.0;
const BASE_GAUGE_VALUE_FONT_PX: f64 = 15.0;
const BASE_FOOTER_FONT_PX: f64 = 12.0;
const MIN_CONTENT_SCALE: f64 = 0.5;
const MAX_CONTENT_SCALE: f64 = 2.0;
/// Higher than `network_sq.rs`'s own 125% default - chosen by the user
/// directly rather than derived from anything, after comparing both cards
/// side by side.
const DEFAULT_CONTENT_SCALE: f64 = 1.3;
/// Height of each gauge's `DrawingArea`, in pixels - also doubles as the
/// bar's stroke thickness (see `draw_bar`), matching the mockup's 10px
/// bars. Not scaled by `content_scale`: the user asked to resize text, and
/// growing the bars too would risk three gauges no longer fitting the card.
const GAUGE_BAR_HEIGHT_PX: i32 = 10;

static INSTALL_CSS: Once = Once::new();

fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        // Color only for the resizable classes below - their `font-size`
        // comes from each instance's own `FONT_CSS` rule instead (see
        // `SystemSqState::apply_content_scale`), since a single shared
        // class can't hold a different pixel size per card. The OS badge
        // stays fixed-size here, same as `network_sq.rs`'s VPN badge.
        css.load_from_string(&format!(
            ".xeneon-sysinfo-hostname {{ font-weight: 500; color: #ffffff; }}\n\
             .xeneon-sysinfo-os-badge {{ background-color: rgba(255, 255, 255, 0.08); \
             border-radius: 10px; padding: 3px 10px; }}\n\
             .xeneon-sysinfo-os-badge-label {{ font-size: {OS_BADGE_FONT_PX}px; color: rgba(255, 255, 255, 0.7); }}\n\
             .xeneon-sysinfo-gauge-label {{ color: rgba(255, 255, 255, 0.78); }}\n\
             .xeneon-sysinfo-gauge-value {{ font-weight: 500; color: #ffffff; }}\n\
             .xeneon-sysinfo-footer {{ color: rgba(255, 255, 255, 0.55); }}"
        ));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

// Per-instance scaled font-size rule for the resizable text, keyed by each
// instance's own unique CSS class - one card's content scale never bleeds
// into another's. Same registry/pattern as `network_sq.rs`'s `NAME_CSS`.
thread_local! {
    static FONT_CSS: CssRuleRegistry = CssRuleRegistry::new(gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

/// One metric row: a label/value line above a thin gauge bar. Returns the
/// row itself plus the three pieces `refresh()` needs to update on every
/// tick - the label (only the disk row's label text ever changes, to show
/// its configured mount point, but all three are handed back for a
/// consistent shape), the value text, and the bar's `DrawingArea`.
fn build_metric_row() -> (gtk::Box, gtk::Label, gtk::Label, gtk::DrawingArea) {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let label = gtk::Label::new(None);
    label.add_css_class("xeneon-sysinfo-gauge-label");
    label.set_halign(gtk::Align::Start);
    header.append(&label);

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    header.append(&spacer);

    let value = gtk::Label::new(None);
    value.add_css_class("xeneon-sysinfo-gauge-value");
    header.append(&value);
    row.append(&header);

    let bar = gtk::DrawingArea::new();
    bar.set_content_height(GAUGE_BAR_HEIGHT_PX);
    bar.set_hexpand(true);
    row.append(&bar);

    (row, label, value, bar)
}

/// Pulls the plain `(r, g, b)` floats `draw_bar` wants out of a
/// `gtk::gdk::RGBA` - its components are `f32`, hence the cast, same as
/// `temp_gauge.rs::draw_gauge` does inline for its own bar color.
fn rgba_components(rgba: &gtk::gdk::RGBA) -> (f64, f64, f64) {
    (rgba.red() as f64, rgba.green() as f64, rgba.blue() as f64)
}

/// Paints one gauge bar: a faint full-width track, then a colored fill over
/// `fraction` (0.0-1.0) of it - both drawn as a round-capped stroke rather
/// than a filled rectangle, the same technique `temp_gauge.rs::draw_gauge`
/// uses for its circular dial, just along a straight line here so both ends
/// read as a pill instead of a sharp-cut bar.
fn draw_bar(cr: &gtk::cairo::Context, width: f64, height: f64, fraction: f64, color: (f64, f64, f64)) {
    let thickness = height;
    let radius = thickness / 2.0;
    let y = height / 2.0;
    let left = radius;
    let right = (width - radius).max(left);

    cr.set_line_cap(gtk::cairo::LineCap::Round);
    cr.set_line_width(thickness);

    cr.set_source_rgba(1.0, 1.0, 1.0, 0.12);
    cr.move_to(left, y);
    cr.line_to(right, y);
    let _ = cr.stroke();

    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.0 {
        let (r, g, b) = color;
        cr.set_source_rgb(r, g, b);
        let end_x = left + (right - left) * fraction;
        cr.move_to(left, y);
        cr.line_to(end_x.max(left), y);
        let _ = cr.stroke();
    }
}

/// Formats a byte count as a one-decimal (or whole-number past 10) GiB
/// figure - one decimal reads as more precise for small values ("1.2 Go")
/// while a whole-capacity drive ("512 Go") doesn't need a decimal to be
/// useful, mirroring how most disk-usage UIs drop the decimal past single
/// digits.
fn format_gib(bytes: u64) -> String {
    let gib = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    if gib >= 10.0 {
        format!("{gib:.0} Go")
    } else {
        format!("{gib:.1} Go")
    }
}

/// Formats seconds-since-boot as a compact "3 j 4 h" (or "4 h 12 min", or
/// just "12 min" for a machine up less than an hour) - only the two most
/// significant units are shown, since a widget this small has no room for
/// (and the user has no real use for) e.g. exact seconds. Unit
/// abbreviations come from `widgets.system_info.unit_{day,hour,minute}` so
/// they read correctly in every shipped language, not just French.
fn format_uptime(total_seconds: f64) -> String {
    let total_seconds = total_seconds.max(0.0) as u64;
    let days = total_seconds / 86_400;
    let hours = (total_seconds % 86_400) / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let unit_day = i18n::t("widgets.system_info.unit_day");
    let unit_hour = i18n::t("widgets.system_info.unit_hour");
    let unit_minute = i18n::t("widgets.system_info.unit_minute");
    if days > 0 {
        format!("{days} {unit_day} {hours} {unit_hour}")
    } else if hours > 0 {
        format!("{hours} {unit_hour} {minutes} {unit_minute}")
    } else {
        format!("{minutes} {unit_minute}")
    }
}

/// All of this widget's live state - one instance per placed widget, shared
/// (via `Rc`) between its content and its settings panel, same overall
/// shape as `CpuTempState`/`NetworkState`.
struct SystemSqState {
    css_class: String,

    hostname_label: gtk::Label,
    os_badge_label: gtk::Label,

    cpu_value_label: gtk::Label,
    cpu_bar: gtk::DrawingArea,
    mem_label: gtk::Label,
    mem_value_label: gtk::Label,
    mem_bar: gtk::DrawingArea,
    disk_label: gtk::Label,
    disk_value_label: gtk::Label,
    disk_bar: gtk::DrawingArea,

    uptime_label: gtk::Label,
    kernel_label: gtk::Label,
    shell_arch_label: gtk::Label,
    cpu_model_label: gtk::Label,

    disk_path: RefCell<String>,
    /// The previous tick's `/proc/stat` sample - `None` until the first
    /// `refresh()` has run once, at which point the *next* tick can finally
    /// compute a real percentage (see `CpuTimes::usage_percent_since`'s own
    /// doc comment on why a lone sample can't).
    previous_cpu_times: RefCell<Option<CpuTimes>>,
    cpu_fraction: Cell<f64>,
    mem_fraction: Cell<f64>,
    disk_fraction: Cell<f64>,

    content_scale: Cell<f64>,
    cpu_color: RefCell<gtk::gdk::RGBA>,
    mem_color: RefCell<gtk::gdk::RGBA>,
    disk_color: RefCell<gtk::gdk::RGBA>,
}

impl SystemSqState {
    fn set_disk_path(&self, path: String) {
        let trimmed = path.trim();
        let effective = if trimmed.is_empty() { DEFAULT_DISK_PATH.to_string() } else { trimmed.to_string() };
        *self.disk_path.borrow_mut() = effective;
        self.refresh();
    }

    fn set_content_scale(&self, scale: f64) {
        self.content_scale.set(scale.clamp(MIN_CONTENT_SCALE, MAX_CONTENT_SCALE));
        self.apply_content_scale();
    }

    /// Rebuilds this instance's scaled-text CSS rule from `content_scale` -
    /// called from `set_content_scale` and once up front in `build_content`.
    /// Mirrors `network_sq.rs::apply_content_scale`, minus the icon
    /// re-tinting this widget has no icon to redo.
    fn apply_content_scale(&self) {
        let scale = self.content_scale.get();
        let rule = format!(
            ".{class} .xeneon-sysinfo-hostname {{ font-size: {hostname}px; }}\n\
             .{class} .xeneon-sysinfo-gauge-label {{ font-size: {label}px; }}\n\
             .{class} .xeneon-sysinfo-gauge-value {{ font-size: {value}px; }}\n\
             .{class} .xeneon-sysinfo-footer {{ font-size: {footer}px; }}",
            class = self.css_class,
            hostname = (BASE_HOSTNAME_FONT_PX * scale).round() as i32,
            label = (BASE_GAUGE_LABEL_FONT_PX * scale).round() as i32,
            value = (BASE_GAUGE_VALUE_FONT_PX * scale).round() as i32,
            footer = (BASE_FOOTER_FONT_PX * scale).round() as i32,
        );
        FONT_CSS.with(|registry| registry.set_rule(&self.css_class, rule));
    }

    fn set_cpu_color(&self, rgba: gtk::gdk::RGBA) {
        *self.cpu_color.borrow_mut() = rgba;
        self.cpu_bar.queue_draw();
    }

    fn set_mem_color(&self, rgba: gtk::gdk::RGBA) {
        *self.mem_color.borrow_mut() = rgba;
        self.mem_bar.queue_draw();
    }

    fn set_disk_color(&self, rgba: gtk::gdk::RGBA) {
        *self.disk_color.borrow_mut() = rgba;
        self.disk_bar.queue_draw();
    }

    /// Back to this widget's appearance defaults (content scale + the three
    /// gauge colors) - goes through the normal setters, not a shortcut that
    /// pokes fields directly, so the CSS rule and each bar's repaint happen
    /// exactly like a manual edit would. Deliberately leaves `disk_path`
    /// untouched: that's a functional setting (which mount point to read),
    /// not an appearance one, the same way resetting `network_sq.rs`'s
    /// appearance doesn't unpin a manually-chosen interface... except it
    /// does - see that module's own `reset()`. This widget draws that line
    /// differently on purpose: unlike a wrong interface pin, a wrong disk
    /// path doesn't stop the gauge from showing *a* real, meaningful value,
    /// so there's less reason for "reset appearance" to also silently
    /// change what the widget is measuring.
    fn reset(&self) {
        self.set_content_scale(DEFAULT_CONTENT_SCALE);
        self.set_cpu_color(hex_to_rgba(DEFAULT_CPU_COLOR_HEX));
        self.set_mem_color(hex_to_rgba(DEFAULT_MEM_COLOR_HEX));
        self.set_disk_color(hex_to_rgba(DEFAULT_DISK_COLOR_HEX));
    }

    /// Re-reads every value and redraws all three gauges - called on every
    /// timer tick (see `build_content`) and immediately after the disk path
    /// setting changes, same "just re-poll everything, it's cheap enough"
    /// approach `cpu_temp.rs::refresh` documents for its own hwmon reads.
    fn refresh(&self) {
        self.hostname_label.set_label(&read_hostname());
        self.os_badge_label.set_label(&read_os_short_name());

        match read_cpu_times() {
            Some(current) => {
                let percent = match self.previous_cpu_times.borrow().as_ref() {
                    Some(previous) => current.usage_percent_since(previous),
                    // First tick ever: nothing to compare against yet:
                    None => 0.0,
                };
                *self.previous_cpu_times.borrow_mut() = Some(current);
                self.cpu_fraction.set(percent / 100.0);
                self.cpu_value_label.set_label(&format!("{}%", percent.round() as i64));
            }
            None => {
                self.cpu_fraction.set(0.0);
                self.cpu_value_label.set_label("--");
            }
        }
        self.cpu_bar.queue_draw();

        self.mem_label.set_label(&i18n::t("widgets.system_info.mem_label"));
        match read_memory() {
            Some(memory) if memory.total_kib > 0 => {
                let used_kib = memory.used_kib();
                self.mem_fraction.set(used_kib as f64 / memory.total_kib as f64);
                self.mem_value_label.set_label(&format!(
                    "{} / {}",
                    format_gib(used_kib * 1024),
                    format_gib(memory.total_kib * 1024)
                ));
            }
            _ => {
                self.mem_fraction.set(0.0);
                self.mem_value_label.set_label("--");
            }
        }
        self.mem_bar.queue_draw();

        let disk_path = self.disk_path.borrow().clone();
        self.disk_label.set_label(&format!("{} {disk_path}", i18n::t("widgets.system_info.disk_label")));
        match read_disk_usage(&disk_path) {
            Some(disk) if disk.total_bytes > 0 => {
                self.disk_fraction.set(disk.used_bytes as f64 / disk.total_bytes as f64);
                self.disk_value_label
                    .set_label(&format!("{} / {}", format_gib(disk.used_bytes), format_gib(disk.total_bytes)));
            }
            _ => {
                self.disk_fraction.set(0.0);
                self.disk_value_label.set_label("--");
            }
        }
        self.disk_bar.queue_draw();

        let uptime_text = match read_uptime_seconds() {
            Some(seconds) => format_uptime(seconds),
            None => "--".to_string(),
        };
        self.uptime_label.set_label(&i18n::t_args("widgets.system_info.uptime_label", &[("value", &uptime_text)]));
        self.kernel_label.set_label(&format!("Kernel {}", read_kernel_release()));
        self.shell_arch_label.set_label(&format!("Shell {} · {}", read_shell_name(), std::env::consts::ARCH));
        self.cpu_model_label.set_label(&read_cpu_model());
    }

    fn to_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "disk_path": self.disk_path.borrow().clone(),
            "content_scale": self.content_scale.get(),
            "cpu_color": rgba_to_hex(&self.cpu_color.borrow()),
            "mem_color": rgba_to_hex(&self.mem_color.borrow()),
            "disk_color": rgba_to_hex(&self.disk_color.borrow()),
        })
    }

    /// Only touches fields actually present, so a partial/older saved dict
    /// (e.g. from before the color pickers/scale slider existed) still
    /// applies cleanly - mirrors `CpuTempState::apply_dict`.
    fn apply_dict(&self, data: &serde_json::Value) {
        if let Some(path) = data.get("disk_path").and_then(|v| v.as_str()) {
            self.set_disk_path(path.to_string());
        }
        if let Some(scale) = data.get("content_scale").and_then(|v| v.as_f64()) {
            self.set_content_scale(scale);
        }
        if let Some(hex) = data.get("cpu_color").and_then(|v| v.as_str()) {
            self.set_cpu_color(hex_to_rgba(hex));
        }
        if let Some(hex) = data.get("mem_color").and_then(|v| v.as_str()) {
            self.set_mem_color(hex_to_rgba(hex));
        }
        if let Some(hex) = data.get("disk_color").and_then(|v| v.as_str()) {
            self.set_disk_color(hex_to_rgba(hex));
        }
    }
}

/// Builds the widget's whole on-card display: header row (hostname + OS
/// badge), three gauge rows (CPU/memory/disk), then a divider and four
/// footer lines (uptime, kernel, shell + architecture, CPU model) - mirrors
/// the mockup agreed with the user before this widget was built.
fn build_content() -> (Rc<SystemSqState>, gtk::Widget) {
    ensure_css_installed();

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let css_class = format!("xeneon-sysinfo-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class(&css_class);
    // Same margins as `network_sq.rs`'s SQ card, for a consistent inset
    // across every SQ-footprint widget in this app.
    root.set_margin_start(14);
    root.set_margin_end(14);
    root.set_margin_top(12);
    root.set_margin_bottom(10);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let hostname_label = gtk::Label::new(None);
    hostname_label.add_css_class("xeneon-sysinfo-hostname");
    hostname_label.set_halign(gtk::Align::Start);
    header.append(&hostname_label);

    let header_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    header_spacer.set_hexpand(true);
    header.append(&header_spacer);

    let os_badge = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    os_badge.add_css_class("xeneon-sysinfo-os-badge");
    let os_badge_label = gtk::Label::new(None);
    os_badge_label.add_css_class("xeneon-sysinfo-os-badge-label");
    os_badge.append(&os_badge_label);
    header.append(&os_badge);
    root.append(&header);

    let (cpu_row, cpu_label, cpu_value_label, cpu_bar) = build_metric_row();
    cpu_label.set_label(&i18n::t("widgets.system_info.cpu_label"));
    root.append(&cpu_row);

    let (mem_row, mem_label, mem_value_label, mem_bar) = build_metric_row();
    root.append(&mem_row);

    let (disk_row, disk_label, disk_value_label, disk_bar) = build_metric_row();
    root.append(&disk_row);

    let footer = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let uptime_label = gtk::Label::new(None);
    uptime_label.add_css_class("xeneon-sysinfo-footer");
    uptime_label.set_halign(gtk::Align::Start);
    footer.append(&uptime_label);
    let kernel_label = gtk::Label::new(None);
    kernel_label.add_css_class("xeneon-sysinfo-footer");
    kernel_label.set_halign(gtk::Align::Start);
    footer.append(&kernel_label);
    let shell_arch_label = gtk::Label::new(None);
    shell_arch_label.add_css_class("xeneon-sysinfo-footer");
    shell_arch_label.set_halign(gtk::Align::Start);
    footer.append(&shell_arch_label);
    let cpu_model_label = gtk::Label::new(None);
    cpu_model_label.add_css_class("xeneon-sysinfo-footer");
    cpu_model_label.set_halign(gtk::Align::Start);
    // A long model name has nowhere else to go on a card this narrow -
    // ellipsizing beats letting it overflow the card or wrap and push the
    // rest of the footer down.
    cpu_model_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    footer.append(&cpu_model_label);
    root.append(&footer);

    let state = Rc::new(SystemSqState {
        css_class,
        hostname_label,
        os_badge_label,
        cpu_value_label,
        cpu_bar: cpu_bar.clone(),
        mem_label,
        mem_value_label,
        mem_bar: mem_bar.clone(),
        disk_label,
        disk_value_label,
        disk_bar: disk_bar.clone(),
        uptime_label,
        kernel_label,
        shell_arch_label,
        cpu_model_label,
        disk_path: RefCell::new(DEFAULT_DISK_PATH.to_string()),
        previous_cpu_times: RefCell::new(None),
        cpu_fraction: Cell::new(0.0),
        mem_fraction: Cell::new(0.0),
        disk_fraction: Cell::new(0.0),
        content_scale: Cell::new(DEFAULT_CONTENT_SCALE),
        cpu_color: RefCell::new(hex_to_rgba(DEFAULT_CPU_COLOR_HEX)),
        mem_color: RefCell::new(hex_to_rgba(DEFAULT_MEM_COLOR_HEX)),
        disk_color: RefCell::new(hex_to_rgba(DEFAULT_DISK_COLOR_HEX)),
    });
    state.apply_content_scale();

    cpu_bar.set_draw_func({
        let state = state.clone();
        move |_area, cr, width, height| {
            let color = *state.cpu_color.borrow();
            draw_bar(cr, width as f64, height as f64, state.cpu_fraction.get(), rgba_components(&color))
        }
    });
    mem_bar.set_draw_func({
        let state = state.clone();
        move |_area, cr, width, height| {
            let color = *state.mem_color.borrow();
            draw_bar(cr, width as f64, height as f64, state.mem_fraction.get(), rgba_components(&color))
        }
    });
    disk_bar.set_draw_func({
        let state = state.clone();
        move |_area, cr, width, height| {
            let color = *state.disk_color.borrow();
            draw_bar(cr, width as f64, height as f64, state.disk_fraction.get(), rgba_components(&color))
        }
    });

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

/// Builds the settings panel: the "mount point" text entry (the widget's
/// one functional setting), then the content-scale slider and the three
/// gauge color pickers. Returns the panel plus a `resync` closure that
/// re-reads every control's displayed value from `state` - needed after
/// `state.reset()` changes the model directly (see `spawn`/`restore`
/// below), same reasoning and ordering-around-`RefCell`-reentrancy as
/// `network_sq.rs::build_settings`'s own `resync`.
fn build_settings(state: Rc<SystemSqState>) -> (gtk::Widget, Box<dyn Fn()>) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(220, -1);

    let disk_path_label = gtk::Label::new(Some(&i18n::t("widgets.system_info.settings.disk_path")));
    disk_path_label.set_halign(gtk::Align::Start);
    root.append(&disk_path_label);

    let disk_path_entry = gtk::Entry::new();
    disk_path_entry.set_text(&state.disk_path.borrow());
    root.append(&disk_path_entry);

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let scale_label = gtk::Label::new(Some(&i18n::t("widgets.system_info.settings.content_scale")));
    scale_label.set_halign(gtk::Align::Start);
    root.append(&scale_label);
    let scale_slider =
        gtk::Scale::with_range(gtk::Orientation::Horizontal, MIN_CONTENT_SCALE * 100.0, MAX_CONTENT_SCALE * 100.0, 1.0);
    scale_slider.set_value(state.content_scale.get() * 100.0);
    scale_slider.set_draw_value(true);
    scale_slider.set_value_pos(gtk::PositionType::Right);
    root.append(&scale_slider);

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let cpu_color_label = gtk::Label::new(Some(&i18n::t("widgets.system_info.settings.cpu_color")));
    cpu_color_label.set_hexpand(true);
    cpu_color_label.set_halign(gtk::Align::Start);
    let cpu_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    cpu_color_button.set_rgba(&state.cpu_color.borrow());
    root.append(&make_row(&[cpu_color_label.upcast_ref(), cpu_color_button.upcast_ref()]));

    let mem_color_label = gtk::Label::new(Some(&i18n::t("widgets.system_info.settings.mem_color")));
    mem_color_label.set_hexpand(true);
    mem_color_label.set_halign(gtk::Align::Start);
    let mem_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    mem_color_button.set_rgba(&state.mem_color.borrow());
    root.append(&make_row(&[mem_color_label.upcast_ref(), mem_color_button.upcast_ref()]));

    let disk_color_label = gtk::Label::new(Some(&i18n::t("widgets.system_info.settings.disk_color")));
    disk_color_label.set_hexpand(true);
    disk_color_label.set_halign(gtk::Align::Start);
    let disk_color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    disk_color_button.set_rgba(&state.disk_color.borrow());
    root.append(&make_row(&[disk_color_label.upcast_ref(), disk_color_button.upcast_ref()]));

    // --- signal wiring: controls push one-way into `state` ---
    disk_path_entry.connect_changed({
        let state = state.clone();
        move |entry| state.set_disk_path(entry.text().to_string())
    });
    scale_slider.connect_value_changed({
        let state = state.clone();
        move |s| state.set_content_scale(s.value() / 100.0)
    });
    cpu_color_button.connect_rgba_notify({
        let state = state.clone();
        move |b| state.set_cpu_color(b.rgba())
    });
    mem_color_button.connect_rgba_notify({
        let state = state.clone();
        move |b| state.set_mem_color(b.rgba())
    });
    disk_color_button.connect_rgba_notify({
        let state = state.clone();
        move |b| state.set_disk_color(b.rgba())
    });

    // --- retranslation ---
    i18n::on_change({
        let disk_path_label = disk_path_label.clone();
        let scale_label = scale_label.clone();
        let cpu_color_label = cpu_color_label.clone();
        let mem_color_label = mem_color_label.clone();
        let disk_color_label = disk_color_label.clone();
        move || {
            disk_path_label.set_label(&i18n::t("widgets.system_info.settings.disk_path"));
            scale_label.set_label(&i18n::t("widgets.system_info.settings.content_scale"));
            cpu_color_label.set_label(&i18n::t("widgets.system_info.settings.cpu_color"));
            mem_color_label.set_label(&i18n::t("widgets.system_info.settings.mem_color"));
            disk_color_label.set_label(&i18n::t("widgets.system_info.settings.disk_color"));
        }
    });

    let resync: Box<dyn Fn()> = Box::new({
        let state = state.clone();
        move || {
            let content_scale = state.content_scale.get();
            let cpu_color = *state.cpu_color.borrow();
            let mem_color = *state.mem_color.borrow();
            let disk_color = *state.disk_color.borrow();

            scale_slider.set_value(content_scale * 100.0);
            cpu_color_button.set_rgba(&cpu_color);
            mem_color_button.set_rgba(&mem_color);
            disk_color_button.set_rgba(&disk_color);
        }
    });

    (root.upcast(), resync)
}

/// Lays out `widgets` in a single horizontal row - same tiny helper
/// `network_sq.rs`/`temp_gauge.rs` each keep their own copy of, for a
/// label-plus-control settings row.
fn make_row(widgets: &[&gtk::Widget]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for w in widgets {
        row.append(*w);
    }
    row
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
        on_change_ready: None,
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
        on_change_ready: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_gib_uses_one_decimal_below_ten() {
        assert_eq!(format_gib(1_200_000_000), "1.1 Go");
    }

    #[test]
    fn format_gib_drops_decimal_at_ten_and_above() {
        assert_eq!(format_gib(32u64 * 1024 * 1024 * 1024), "32 Go");
    }

    // `format_uptime` reads its unit abbreviations through `i18n::t`, which
    // returns the raw key (not "j"/"h"/"min") until a catalog has been
    // loaded - these tests load the French one explicitly rather than
    // relying on whatever `main()` would otherwise set up first.
    #[test]
    fn format_uptime_shows_days_and_hours_when_at_least_a_day() {
        crate::i18n_runtime::init("fr");
        // 3 days, 4 hours, 30 minutes.
        let seconds = 3.0 * 86_400.0 + 4.0 * 3600.0 + 30.0 * 60.0;
        assert_eq!(format_uptime(seconds), "3 j 4 h");
    }

    #[test]
    fn format_uptime_shows_hours_and_minutes_under_a_day() {
        crate::i18n_runtime::init("fr");
        let seconds = 4.0 * 3600.0 + 12.0 * 60.0;
        assert_eq!(format_uptime(seconds), "4 h 12 min");
    }

    #[test]
    fn format_uptime_shows_only_minutes_under_an_hour() {
        crate::i18n_runtime::init("fr");
        assert_eq!(format_uptime(12.0 * 60.0), "12 min");
    }
}
