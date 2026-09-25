// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared data-reading layer for the upcoming "system info" widget (CPU
//! load, memory, disk, hostname, uptime, kernel, OS, shell, CPU model) -
//! this module only *reads* those values; no widget lives here yet (see the
//! module doc comment on `network.rs` for the same split between a data
//! module and its size-variant widget files, e.g. `network_sq.rs`). A
//! `system_sq.rs` built on top of these functions is the planned next step.
//!
//! Every value here comes from a plain Linux kernel interface (`/proc`,
//! `/sys`) or a single `statvfs(2)` syscall - no crate needed for any of it
//! beyond `libc` (already resolved in this workspace's `Cargo.lock`
//! transitively through the GTK bindings, so declaring it directly in
//! `xeneon-app/Cargo.toml` adds no new code to the dependency tree), which
//! matches this project's minimal-dependencies preference the same way
//! `cpu_temp.rs` reads straight from `/sys/class/hwmon` and `network.rs`
//! reads straight from `/proc/net/dev`.
//!
//! CPU load needs two samples: `/proc/stat`'s per-field jiffie counters are
//! cumulative since boot, not a rate, so (like `network.rs`'s throughput
//! counters) turning them into a percentage requires the caller to keep the
//! previous `CpuTimes` and call `usage_percent_since` on the new one -
//! there's no single-shot "current CPU usage" file to read. Disk usage is
//! the one value that isn't a plain file read: there is no `/proc` or
//! `/sys` file exposing free/used space for an arbitrary mount point, so
//! `read_disk_usage` calls `statvfs(2)` directly instead - the same syscall
//! `df`/`coreutils` use internally, just without spawning a process and
//! parsing its text output.
//!
//! Every reader returns `None`/an empty string on failure (missing file,
//! unparsable content, syscall error) rather than panicking, mirroring
//! `cpu_temp.rs::all_sensors`'s defensive shape - a field the future widget
//! can't read should fall back to a placeholder ("--"), not take the whole
//! card down.

use std::ffi::CString;
use std::mem::MaybeUninit;
use std::path::Path;

/// Where the kernel exposes per-CPU (and aggregate) jiffie counters -
/// `read_cpu_times` only ever looks at the first line (`cpu  ...`, the
/// aggregate across all cores), not the per-core lines that follow it.
const STAT_PATH: &str = "/proc/stat";
/// Where the kernel exposes memory totals/availability in kB.
const MEMINFO_PATH: &str = "/proc/meminfo";
/// Where the kernel exposes seconds-since-boot as the file's first field
/// (the second field, idle time summed across cores, isn't used here).
const UPTIME_PATH: &str = "/proc/uptime";
/// Same hostname the `hostname(1)` command and `gethostname(2)` report -
/// reading the file directly avoids a libc call for a value this simple.
const HOSTNAME_PATH: &str = "/proc/sys/kernel/hostname";
/// The kernel's own release string (e.g. `6.12.4-200.fc44.x86_64`), same as
/// `uname -r` - reading the file directly avoids a libc `uname(2)` call for
/// a value this simple.
const OSRELEASE_PATH: &str = "/proc/sys/kernel/osrelease";
/// Standard freedesktop.org location for distro identification - present on
/// every systemd-based distro (Fedora, Ubuntu, Arch...) this app targets.
const OS_RELEASE_PATH: &str = "/etc/os-release";
/// Where the kernel exposes per-core identification - only the first
/// `model name` line is used (see `read_cpu_model`), on the assumption that
/// every core in a single machine shares the same model.
const CPUINFO_PATH: &str = "/proc/cpuinfo";

/// Reads one file and trims it, or `None` if it can't be read - same
/// defensive shape as `cpu_temp.rs`'s own private `read_stripped` (not
/// shared across modules; each data module in this codebase owns its own
/// copy rather than factoring out a one-line helper).
fn read_stripped(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// One sample of `/proc/stat`'s aggregate CPU line: total jiffies spent
/// idle (`idle` + `iowait` - both count as "had nothing to do") versus
/// jiffies spent overall, both cumulative since boot. On its own this says
/// nothing about *current* load - see `usage_percent_since`.
pub struct CpuTimes {
    idle: u64,
    total: u64,
}

impl CpuTimes {
    /// CPU usage (0.0-100.0) over the interval between `previous` and
    /// `self` - the standard `top`/`htop` technique of comparing two
    /// `/proc/stat` snapshots rather than trusting any single one, since a
    /// lone sample only ever holds cumulative-since-boot counters. Returns
    /// `0.0` for a zero-length or clock-went-backwards interval (`total`
    /// not having advanced) instead of dividing by zero.
    pub fn usage_percent_since(&self, previous: &CpuTimes) -> f64 {
        let total_delta = self.total.saturating_sub(previous.total);
        if total_delta == 0 {
            return 0.0;
        }
        let idle_delta = self.idle.saturating_sub(previous.idle);
        let busy_fraction = 1.0 - (idle_delta as f64 / total_delta as f64);
        busy_fraction.clamp(0.0, 1.0) * 100.0
    }
}

/// Reads the current aggregate CPU sample from `/proc/stat`'s first line:
/// `cpu  user nice system idle iowait irq softirq steal guest guest_nice`
/// (field count varies slightly by kernel version, so this only assumes the
/// first four - user/nice/system/idle - are always present, matching what
/// every `/proc/stat` parser out there relies on). `None` if the file is
/// missing or its first line doesn't look like the expected format.
pub fn read_cpu_times() -> Option<CpuTimes> {
    let content = std::fs::read_to_string(STAT_PATH).ok()?;
    let first_line = content.lines().next()?;
    let mut fields = first_line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let values: Vec<u64> = fields.filter_map(|f| f.parse().ok()).collect();
    if values.len() < 4 {
        return None;
    }
    // iowait (index 4) isn't present on every kernel config, hence the
    // `.get(4)` fallback to 0 rather than indexing directly.
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    let total = values.iter().sum();
    Some(CpuTimes { idle, total })
}

/// `/proc/meminfo`'s two totals this widget cares about, both in kB (the
/// unit `/proc/meminfo` itself uses, despite the misleading `kB` suffix on
/// each line actually meaning kiB).
pub struct MemoryInfo {
    pub total_kib: u64,
    pub available_kib: u64,
}

impl MemoryInfo {
    /// `MemAvailable` (not the older, less accurate `MemFree`) is what
    /// modern tools like `free(1)` use for "how much could a new process
    /// actually get" - it already accounts for reclaimable caches/buffers,
    /// so `total - available` is the usual "used" figure users expect,
    /// rather than the much larger `total - free`.
    pub fn used_kib(&self) -> u64 {
        self.total_kib.saturating_sub(self.available_kib)
    }
}

/// Reads `MemTotal`/`MemAvailable` from `/proc/meminfo`. `None` if the file
/// is missing or either field can't be found/parsed - `MemAvailable` has
/// been present since Linux 3.14 (2014), so its absence would mean a kernel
/// old enough that this widget isn't expected to support it anyway.
pub fn read_memory() -> Option<MemoryInfo> {
    let content = std::fs::read_to_string(MEMINFO_PATH).ok()?;
    let mut total_kib = None;
    let mut available_kib = None;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kib = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available_kib = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        }
        if total_kib.is_some() && available_kib.is_some() {
            break;
        }
    }
    Some(MemoryInfo { total_kib: total_kib?, available_kib: available_kib? })
}

/// Seconds since boot - `/proc/uptime`'s first field (the second, summed
/// idle time across all cores, isn't used here). `None` if the file can't
/// be read or its first field doesn't parse as a number.
pub fn read_uptime_seconds() -> Option<f64> {
    let content = std::fs::read_to_string(UPTIME_PATH).ok()?;
    content.split_whitespace().next()?.parse().ok()
}

/// Bytes total/used on the filesystem holding `path`, straight from
/// `statvfs(2)` - the same syscall `df`/coreutils use, just called directly
/// instead of spawning `df` and parsing its text output. `used` mirrors
/// `df`'s own "Used" column: `(f_blocks - f_bfree) * f_frsize`, i.e. blocks
/// actually occupied, as opposed to `f_bavail` (blocks a non-root process
/// could still allocate, which is usually smaller because of the
/// filesystem's root-reserved margin).
pub struct DiskUsage {
    pub total_bytes: u64,
    pub used_bytes: u64,
}

/// `None` if `path` contains a NUL byte (can't become a `CString`) or the
/// syscall itself fails (path doesn't exist, no permission...) - mirrors
/// every other reader in this module returning `None` on failure rather
/// than panicking.
pub fn read_disk_usage(path: &str) -> Option<DiskUsage> {
    let c_path = CString::new(path).ok()?;
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c_path` is a valid NUL-terminated buffer for the duration of
    // this call, and `stat.as_mut_ptr()` points at enough space for a
    // `libc::statvfs` (the type `MaybeUninit` was declared with). `statvfs`
    // only ever reads `c_path` and writes to `stat` - on a non-zero return
    // it may have written nothing at all, which is exactly why
    // `assume_init` below only runs after checking that return value.
    let result = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    // SAFETY: `result == 0` means the kernel fully populated `stat`.
    let stat = unsafe { stat.assume_init() };
    let block_size = stat.f_frsize as u64;
    let total_bytes = stat.f_blocks as u64 * block_size;
    let free_bytes = stat.f_bfree as u64 * block_size;
    Some(DiskUsage { total_bytes, used_bytes: total_bytes.saturating_sub(free_bytes) })
}

/// The machine's hostname, or `""` if `/proc/sys/kernel/hostname` can't be
/// read (should not normally happen on Linux).
pub fn read_hostname() -> String {
    read_stripped(HOSTNAME_PATH).unwrap_or_default()
}

/// The kernel release string (e.g. `6.12.4-200.fc44.x86_64`), same value
/// `uname -r` reports.
pub fn read_kernel_release() -> String {
    read_stripped(OSRELEASE_PATH).unwrap_or_default()
}

/// Strips the surrounding quotes `/etc/os-release` wraps its values in
/// (`NAME="Fedora Linux"`) - a plain `str::replace('"', "")` rather than a
/// full shell-quoting parser, since every distro's `/etc/os-release` in
/// practice only ever uses plain double quotes here, never escapes.
fn unquote(value: &str) -> String {
    value.trim().trim_matches('"').to_string()
}

/// The distro's human-facing name, preferring `PRETTY_NAME` (e.g. "Fedora
/// Linux 44 (Workstation Edition)") and falling back to the plainer `NAME`
/// field if that's missing, then to `""` if the file itself can't be read -
/// every systemd-based distro ships at least one of the two.
pub fn read_os_pretty_name() -> String {
    let Ok(content) = std::fs::read_to_string(OS_RELEASE_PATH) else {
        return String::new();
    };
    let mut pretty_name = None;
    let mut plain_name = None;
    for line in content.lines() {
        if let Some(value) = line.strip_prefix("PRETTY_NAME=") {
            pretty_name = Some(unquote(value));
        } else if let Some(value) = line.strip_prefix("NAME=") {
            plain_name = Some(unquote(value));
        }
    }
    pretty_name.or(plain_name).unwrap_or_default()
}

/// The user's login shell's bare filename (e.g. `zsh`, not `/usr/bin/zsh`),
/// read from the `SHELL` environment variable - the same variable
/// `chsh`/every login manager sets, and simpler than looking the user up in
/// `/etc/passwd`. `""` if `SHELL` isn't set at all (rare, but possible in a
/// minimal/non-interactive environment).
pub fn read_shell_name() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|path| Path::new(&path).file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// This machine's CPU model name (e.g. "AMD Ryzen 7 5800X 8-Core
/// Processor"), from the first `model name` line in `/proc/cpuinfo` - every
/// core repeats the same line, so only the first is read. `""` if the file
/// can't be read or has no such field at all, which happens on some ARM
/// boards that expose a different field name instead - not worth chasing
/// every architecture's naming quirk for a "nice to have" display line.
pub fn read_cpu_model() -> String {
    let Ok(content) = std::fs::read_to_string(CPUINFO_PATH) else {
        return String::new();
    };
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("model name") {
            if let Some(value) = rest.split_once(':') {
                return value.1.trim().to_string();
            }
        }
    }
    String::new()
}

/// The architecture this binary was compiled for (e.g. `x86_64`,
/// `aarch64`) - `std::env::consts::ARCH`, a compile-time constant, is used
/// directly at the call site rather than wrapped in a function here, since
/// on the overwhelming majority of machines (no foreign-architecture
/// emulation) it matches the running kernel's own architecture without
/// needing a `uname(2)` call at all.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_usage_percent_since_handles_zero_interval() {
        let sample = CpuTimes { idle: 100, total: 200 };
        assert_eq!(sample.usage_percent_since(&sample), 0.0);
    }

    #[test]
    fn cpu_usage_percent_since_computes_busy_fraction() {
        let previous = CpuTimes { idle: 100, total: 1000 };
        let current = CpuTimes { idle: 150, total: 1500 };
        // 500 total jiffies elapsed, 50 of them idle -> 90% busy.
        assert!((current.usage_percent_since(&previous) - 90.0).abs() < f64::EPSILON);
    }

    #[test]
    fn unquote_strips_surrounding_quotes() {
        assert_eq!(unquote("\"Fedora Linux\""), "Fedora Linux");
        assert_eq!(unquote("plain"), "plain");
    }
}
