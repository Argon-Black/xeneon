//! Unifies what the Python original keeps as two separately-maintained
//! tables - `CATALOG` (picker) and the if/elif chain in
//! `build_from_state` (restore) in widget_picker.py - into one: each kind
//! provides both a `spawn` (fresh instance for the picker) and a
//! `restore` (rebuild from a saved `content` JSON blob) function, from a
//! single static list. Adding a new plugin means adding one entry here,
//! not two places that can drift apart.

use gtk::prelude::*;
use xeneon_core::grid::{Size, SIZE_L, SIZE_M, SIZE_S, SIZE_SQ, SIZE_SSX, SIZE_SX};

/// What a spawned/restored widget hands back to `WidgetGrid`: its content
/// widget, an optional settings panel (shown in the configure popover),
/// and a way to serialize its current state for persistence. Plugins with
/// no settings/content of their own (the dummy widgets) just return `None`
/// and a `to_dict` that always produces `null`.
pub struct WidgetInstance {
    pub content: gtk::Widget,
    pub settings: Option<gtk::Widget>,
    pub to_dict: Box<dyn Fn() -> serde_json::Value>,
    /// Runs when the appearance popover's reset button is clicked, in
    /// addition to (never instead of) resetting the generic appearance -
    /// a plugin with its own settings (Clock) resets *those* back to
    /// defaults here and resyncs its settings controls to match, since
    /// this reset button is the only reset affordance a widget gets.
    /// `None` for plugins with no settings of their own (the dummy
    /// widgets) - matches `on_reset` being optional in grid.py's
    /// `DashboardWidget`.
    pub on_reset: Option<Box<dyn Fn()>>,
}

pub struct WidgetDescriptor {
    pub kind: &'static str,
    pub title_key: &'static str,
    pub size: Size,
    pub spawn: fn() -> WidgetInstance,
    pub restore: fn(&serde_json::Value) -> WidgetInstance,
}

pub static CATALOG: &[WidgetDescriptor] = &[
    WidgetDescriptor {
        kind: "clock",
        title_key: "widgets.clock.title",
        size: SIZE_M,
        spawn: crate::widgets::clock::spawn,
        restore: crate::widgets::clock::restore,
    },
    // Step 1 of the audio port: SIZE_L only. A "audio_sq" entry sharing
    // the same widgets/audio.rs code is added once the SQ variant exists
    // - see that module's own doc comment for the step breakdown.
    WidgetDescriptor {
        kind: "audio_l",
        title_key: "widgets.audio.title",
        size: SIZE_L,
        spawn: crate::widgets::audio::spawn,
        restore: crate::widgets::audio::restore,
    },
    WidgetDescriptor {
        kind: "cpu_temp",
        title_key: "widgets.cpu_temp.title",
        size: SIZE_SSX,
        spawn: crate::widgets::cpu_temp::spawn,
        restore: crate::widgets::cpu_temp::restore,
    },
    WidgetDescriptor {
        kind: "dummy_s",
        title_key: "widgets.dummy.title_s",
        size: SIZE_S,
        spawn: crate::widgets::dummy::spawn_s,
        restore: crate::widgets::dummy::restore_s,
    },
    WidgetDescriptor {
        kind: "dummy_m",
        title_key: "widgets.dummy.title_m",
        size: SIZE_M,
        spawn: crate::widgets::dummy::spawn_m,
        restore: crate::widgets::dummy::restore_m,
    },
    WidgetDescriptor {
        kind: "dummy_l",
        title_key: "widgets.dummy.title_l",
        size: SIZE_L,
        spawn: crate::widgets::dummy::spawn_l,
        restore: crate::widgets::dummy::restore_l,
    },
    WidgetDescriptor {
        kind: "dummy_sq",
        title_key: "widgets.dummy.title_sq",
        size: SIZE_SQ,
        spawn: crate::widgets::dummy::spawn_sq,
        restore: crate::widgets::dummy::restore_sq,
    },
    WidgetDescriptor {
        kind: "dummy_sx",
        title_key: "widgets.dummy.title_sx",
        size: SIZE_SX,
        spawn: crate::widgets::dummy::spawn_sx,
        restore: crate::widgets::dummy::restore_sx,
    },
    WidgetDescriptor {
        kind: "dummy_ssx",
        title_key: "widgets.dummy.title_ssx",
        size: SIZE_SSX,
        spawn: crate::widgets::dummy::spawn_ssx,
        restore: crate::widgets::dummy::restore_ssx,
    },
];

pub fn find(kind: &str) -> Option<&'static WidgetDescriptor> {
    CATALOG.iter().find(|d| d.kind == kind)
}

/// Wraps a plain content widget with no settings/persisted state of its
/// own into a `WidgetInstance` - the common case for the dummy widgets.
pub fn instance_without_settings(content: impl IsA<gtk::Widget>) -> WidgetInstance {
    WidgetInstance { content: content.upcast(), settings: None, to_dict: Box::new(|| serde_json::Value::Null), on_reset: None }
}
