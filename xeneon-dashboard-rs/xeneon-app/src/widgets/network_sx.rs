// SPDX-License-Identifier: GPL-3.0-or-later
//! Network throughput widget (SX footprint): "wlan0  ↓12.4M  ↑1.2M" - both
//! directions on one line, unlike `network.rs`'s SSX card which is only
//! wide enough for one.
//!
//! The actual implementation lives in `network_text_card.rs`, shared with
//! `network_s.rs` (dedup audit finding 2026-09-29) - this file is just
//! the `SX` variant's knobs: a smaller font, a tighter line-width cap,
//! and no `/s` suffix on the rates (S has room to spare for it; this
//! card, roughly half S's width, doesn't).
use crate::widgets::network_text_card::{self, NetworkTextCardVariant};
use crate::widgets::registry::WidgetInstance;

pub struct Sx;

impl NetworkTextCardVariant for Sx {
    const ID: &'static str = "sx";
    const FONT_PX: i32 = 20;
    const MAX_LINE_WIDTH_CHARS: i32 = 34;
    const SHOW_RATE_UNIT_SUFFIX: bool = false;
}

pub fn spawn() -> WidgetInstance {
    network_text_card::spawn::<Sx>()
}

pub fn restore(data: &serde_json::Value) -> WidgetInstance {
    network_text_card::restore::<Sx>(data)
}
