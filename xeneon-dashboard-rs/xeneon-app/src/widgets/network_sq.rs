// SPDX-License-Identifier: GPL-3.0-or-later
//! Network throughput widget (SQ footprint): a small network icon and the
//! interface name in the top-left corner, the down/up rates on their own
//! line below that, and a scrolling in/out history graph filling the rest
//! of the card - matching the mockup shown to the user before this widget
//! was built.
//!
//! The actual implementation lives in `network_card.rs`, shared with
//! `network_m.rs` (dedup audit finding 2026-09-29, step 4 of the
//! network-widget-family cleanup) - this file is just the `Sq` variant's
//! knobs. SQ's header icon+name are the widget's fixed-size *label*, not
//! its content, so there's no size slider (`SUPPORTS_CONTENT_SCALE =
//! false` - see `network_card::NetworkCardVariant`'s own doc comment),
//! and its history/rate-line sizing is tuned for SQ's half-column width.
use crate::widgets::network_card::{self, NetworkCardVariant};
use crate::widgets::registry::WidgetInstance;

pub struct Sq;

impl NetworkCardVariant for Sq {
    const ID: &'static str = "sq";
    const MAX_VALUES_WIDTH_CHARS: i32 = 24;
    const MAX_HISTORY_SAMPLES: usize = 60;
    const SUPPORTS_CONTENT_SCALE: bool = false;
    const DEFAULT_CONTENT_SCALE: f64 = 1.0;
    const MIN_CONTENT_SCALE: f64 = 1.0;
    const MAX_CONTENT_SCALE: f64 = 1.0;
}

pub fn spawn() -> WidgetInstance {
    network_card::spawn::<Sq>()
}

pub fn restore(data: &serde_json::Value) -> WidgetInstance {
    network_card::restore::<Sq>(data)
}
