// SPDX-License-Identifier: GPL-3.0-or-later
//! Network throughput widget (S footprint): "wlan0  ↓12.4M/s  ↑1.2M/s" -
//! same information as `network_sx.rs`'s SX card, at the full-column width
//! this card gets instead of a half-column.
//!
//! The actual implementation lives in `network_text_card.rs`, shared with
//! `network_sx.rs` (dedup audit finding 2026-09-29) - this file is just
//! the `S` variant's knobs: a larger font, a far more generous line-width
//! cap (this card is roughly twice as wide as SX), and the `/s` suffix on
//! each rate that SX drops to save space.
use crate::widgets::network_text_card::{self, NetworkTextCardVariant};
use crate::widgets::registry::WidgetInstance;

pub struct S;

impl NetworkTextCardVariant for S {
    const ID: &'static str = "s";
    const FONT_PX: i32 = 22;
    const MAX_LINE_WIDTH_CHARS: i32 = 50;
    const SHOW_RATE_UNIT_SUFFIX: bool = true;
}

pub fn spawn() -> WidgetInstance {
    network_text_card::spawn::<S>()
}

pub fn restore(data: &serde_json::Value) -> WidgetInstance {
    network_text_card::restore::<S>(data)
}
