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
//! no new widget-drawing technique introduced here. Unlike `network_sq.rs`
//! and `temp_gauge.rs`, this first version has no appearance customization
//! beyond the generic popover (no content-scale slider, no color pickers) -
//! the three gauge colors are fixed constants matching this app's existing
//! defaults (`CPU_COLOR`/`MEM_COLOR` reuse `network_sq.rs`'s down/up rate
//! blue and coral so the palette reads as one family across cards). The
//! only functional setting is which mount point the disk gauge reads,
//! since `/` isn't always the partition a user cares about (a separate
//! `/home`, a data drive...).
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
use std::sync::Once;

use crate::i18n_runtime as i18n;
use crate::widgets::registry::WidgetInstance;
use crate::widgets::system_info::{
    read_cpu_model, read_cpu_times, read_disk_usage, read_hostname, read_kernel_release, read_memory,
    read_os_pretty_name, read_shell_name, read_uptime_seconds, CpuTimes,
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
/// (`DEFAULT_DOWN_COLOR_HEX`) so the two cards read as one palette.
const CPU_COLOR: (f64, f64, f64) = (0x5d as f64 / 255.0, 0xa9 as f64 / 255.0, 0xe8 as f64 / 255.0);
/// Same coral as `network_sq.rs`'s default up-rate/trace color
/// (`DEFAULT_UP_COLOR_HEX`).
const MEM_COLOR: (f64, f64, f64) = (0xe8 as f64 / 255.0, 0x87 as f64 / 255.0, 0x5d as f64 / 255.0);
/// A third color not already used by a neighbouring widget, picked to read
/// clearly against the same dark card background.
const DISK_COLOR: (f64, f64, f64) = (0x8f as f64 / 255.0, 0xd3 as f64 / 255.0, 0xc7 as f64 / 255.0);

const HOSTNAME_FONT_PX: i32 = 20;
const OS_BADGE_FONT_PX: i32 = 11;
const GAUGE_LABEL_FONT_PX: i32 = 14;
const GAUGE_VALUE_FONT_PX: i32 = 15;
const FOOTER_FONT_PX: i32 = 12;
/// Height of each gauge's `DrawingArea`, in pixels - also doubles as the
/// bar's stroke thickness (see `draw_bar`), matching the mockup's 10px bars.
const GAUGE_BAR_HEIGHT_PX: i32 = 10;

static INSTALL_CSS: Once = Once::new();

fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!(
            ".xeneon-sysinfo-hostname {{ font-size: {HOSTNAME_FONT_PX}px; font-weight: 500; color: #ffffff; }}\n\
             .xeneon-sysinfo-os-badge {{ background-color: rgba(255, 255, 255, 0.08); \
             border-radius: 10px; padding: 3px 10px; }}\n\
             .xeneon-sysinfo-os-badge-label {{ font-size: {OS_BADGE_FONT_PX}px; color: rgba(255, 255, 255, 0.7); }}\n\
             .xeneon-sysinfo-gauge-label {{ font-size: {GAUGE_LABEL_FONT_PX}px; color: rgba(255, 255, 255, 0.78); }}\n\
             .xeneon-sysinfo-gauge-value {{ font-size: {GAUGE_VALUE_FONT_PX}px; font-weight: 500; color: #ffffff; }}\n\
             .xeneon-sysinfo-footer {{ font-size: {FOOTER_FONT_PX}px; color: rgba(255, 255, 255, 0.55); }}\n\
             .xeneon-sysinfo-divider {{ background-color: rgba(255, 255, 255, 0.08); }}"
        ));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

/// A thin full-width rule, styled by `.xeneon-sysinfo-divider` above -
/// simpler and more predictable across GTK themes than relying on
/// `gtk::Separator`'s own theme-dependent look for what the mockup drew as
/// a plain 1px line.
fn divider() -> gtk::Box {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    line.set_size_request(-1, 1);
    line.add_css_class("xeneon-sysinfo-divider");
    line
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
}

impl SystemSqState {
    fn set_disk_path(&self, path: String) {
        let trimmed = path.trim();
        let effective = if trimmed.is_empty() { DEFAULT_DISK_PATH.to_string() } else { trimmed.to_string() };
        *self.disk_path.borrow_mut() = effective;
        self.refresh();
    }

    /// Re-reads every value and redraws all three gauges - called on every
    /// timer tick (see `build_content`) and immediately after the disk path
    /// setting changes, same "just re-poll everything, it's cheap enough"
    /// approach `cpu_temp.rs::refresh` documents for its own hwmon reads.
    fn refresh(&self) {
        self.hostname_label.set_label(&read_hostname());
        self.os_badge_label.set_label(&read_os_pretty_name());

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
        serde_json::json!({ "disk_path": self.disk_path.borrow().clone() })
    }

    fn apply_dict(&self, data: &serde_json::Value) {
        if let Some(path) = data.get("disk_path").and_then(|v| v.as_str()) {
            self.set_disk_path(path.to_string());
        }
    }
}

/// Builds the widget's whole on-card display: header row (hostname + OS
/// badge), three gauge rows (CPU/memory/disk), then a divider and four
/// footer lines (uptime, kernel, shell + architecture, CPU model) - mirrors
/// the mockup agreed with the user before this widget was built.
fn build_content() -> (Rc<SystemSqState>, gtk::Widget) {
    ensure_css_installed();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
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

    root.append(&divider());

    let (cpu_row, cpu_label, cpu_value_label, cpu_bar) = build_metric_row();
    cpu_label.set_label(&i18n::t("widgets.system_info.cpu_label"));
    root.append(&cpu_row);

    let (mem_row, mem_label, mem_value_label, mem_bar) = build_metric_row();
    root.append(&mem_row);

    let (disk_row, disk_label, disk_value_label, disk_bar) = build_metric_row();
    root.append(&disk_row);

    root.append(&divider());

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
    });

    cpu_bar.set_draw_func({
        let state = state.clone();
        move |_area, cr, width, height| draw_bar(cr, width as f64, height as f64, state.cpu_fraction.get(), CPU_COLOR)
    });
    mem_bar.set_draw_func({
        let state = state.clone();
        move |_area, cr, width, height| draw_bar(cr, width as f64, height as f64, state.mem_fraction.get(), MEM_COLOR)
    });
    disk_bar.set_draw_func({
        let state = state.clone();
        move |_area, cr, width, height| {
            draw_bar(cr, width as f64, height as f64, state.disk_fraction.get(), DISK_COLOR)
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

/// Builds the settings panel: a single "mount point" text entry for the
/// disk gauge - the only functional setting this widget has (see the
/// module doc comment for why there's no appearance customization yet).
fn build_settings(state: Rc<SystemSqState>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(220, -1);

    let disk_path_label = gtk::Label::new(Some(&i18n::t("widgets.system_info.settings.disk_path")));
    disk_path_label.set_halign(gtk::Align::Start);
    root.append(&disk_path_label);

    let disk_path_entry = gtk::Entry::new();
    disk_path_entry.set_text(&state.disk_path.borrow());
    root.append(&disk_path_entry);

    disk_path_entry.connect_changed({
        let state = state.clone();
        move |entry| state.set_disk_path(entry.text().to_string())
    });

    i18n::on_change({
        let disk_path_label = disk_path_label.clone();
        move || disk_path_label.set_label(&i18n::t("widgets.system_info.settings.disk_path"))
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
