// SPDX-License-Identifier: GPL-3.0-or-later
//! Network throughput widget (M footprint): the exact same design as
//! `network_sq.rs` - small network icon + interface name in the top-left
//! corner, a VPN pill badge at the top-right, the down/up rates on their
//! own line, and a scrolling in/out history graph filling the rest of the
//! card - just at the full-column width this card gets instead of SQ's
//! half-column (same height as SQ, double the width).
//!
//! The actual implementation lives in `network_card.rs`, shared with
//! `network_sq.rs` (dedup audit finding 2026-09-29, step 4 of the
//! network-widget-family cleanup) - this file is just the `M` variant's
//! knobs: a header-icon size slider (`SUPPORTS_CONTENT_SCALE = true`,
//! unlike SQ), a more generous rate-line width cap, and a longer history
//! buffer, all tuned for M's roughly-double-SQ horizontal room.
use crate::widgets::network_card::{self, NetworkCardVariant};
use crate::widgets::registry::WidgetInstance;

pub struct M;

impl NetworkCardVariant for M {
    const ID: &'static str = "m";
    const MAX_VALUES_WIDTH_CHARS: i32 = 34;
    const MAX_HISTORY_SAMPLES: usize = 90;
    const SUPPORTS_CONTENT_SCALE: bool = true;
    const DEFAULT_CONTENT_SCALE: f64 = 1.25;
    const MIN_CONTENT_SCALE: f64 = 0.5;
    const MAX_CONTENT_SCALE: f64 = 2.0;
}

pub fn spawn() -> WidgetInstance {
    network_card::spawn::<M>()
}

pub fn restore(data: &serde_json::Value) -> WidgetInstance {
    network_card::restore::<M>(data)
}
