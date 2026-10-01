// SPDX-License-Identifier: GPL-3.0-or-later
pub mod agenda;
pub mod audio;
pub mod card_header;
pub mod clock;
pub mod cpu_temp;
pub mod dummy;
pub mod hue;
pub mod icon_cache;
pub mod interface_picker;
pub mod network;
pub mod network_card;
pub mod network_m;
pub mod network_s;
pub mod network_sq;
pub mod network_sx;
pub mod network_text_card;
pub mod registry;
pub mod shortcuts;
pub mod system_info;
pub mod system_sq;
pub mod temp_gauge;
pub mod weather;
pub mod youtube;

use gtk::prelude::*;

/// A plain horizontal row of widgets, spacing 8 - the smallest possible
/// settings-row container. Audit finding 2026-09-29: hand-rolled
/// identically 8 times (clock.rs, cpu_temp.rs, temp_gauge.rs,
/// system_sq.rs, network.rs, network_sq.rs, network_m.rs, weather.rs) -
/// kept here rather than in any one of them, since none of them "owns"
/// the concept more than another.
pub fn make_row(widgets: &[&gtk::Widget]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for w in widgets {
        row.append(*w);
    }
    row
}

/// Whether `entries[index]` holds a manually-set value (`Some`) rather
/// than falling back to auto-detection. Audit finding 2026-09-29:
/// hand-rolled identically - only the element type differed
/// (`(String, String)` for cpu_temp.rs/temp_gauge.rs's sensor picker,
/// bare `String` for the network family's interface picker) - across 7
/// widget modules; generic over that element type here.
pub fn is_manual<T>(entries: &[Option<T>], index: usize) -> bool {
    entries.get(index).map(|entry| entry.is_some()).unwrap_or(false)
}
