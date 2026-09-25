// SPDX-License-Identifier: GPL-3.0-or-later
//! Unifies what the Python original keeps as two separately-maintained
//! tables - `CATALOG` (picker) and the if/elif chain in
//! `build_from_state` (restore) in widget_picker.py - into one: each kind
//! provides both a `spawn` (fresh instance for the picker) and a
//! `restore` (rebuild from a saved `content` JSON blob) function, from a
//! single static list. Adding a new plugin means adding one entry here,
//! not two places that can drift apart.

use gtk::prelude::*;
use std::rc::Rc;
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
    /// Called once, right after `WidgetGrid` places the widget, with a
    /// ready-to-use "save me now" closure - for a plugin whose own state
    /// can change *outside* the two save points every widget already gets
    /// for free (the appearance popover's "closed" signal, a whole-widget
    /// drag ending). The shortcuts grid is the first such plugin: adding,
    /// moving or deleting an icon happens straight on the canvas, with no
    /// popover involved at all, so it needs to trigger a save itself the
    /// moment that happens - mirrors `ShortcutsContent.set_change_notifier()`/
    /// `_notify()` in shortcuts.py, where the same widget-picker glue wires
    /// the plugin's own on-change callback to the app's save-this-widget
    /// function. `None` for every plugin whose state only ever changes
    /// through its settings popover (Clock, Weather...), which is already
    /// covered by the popover-closed save.
    pub on_change_ready: Option<Box<dyn FnOnce(Rc<dyn Fn()>)>>,
}

pub struct WidgetDescriptor {
    pub kind: &'static str,
    /// Shown as this kind's row label in the widget picker.
    pub title_key: &'static str,
    /// Shown as the on-card header inside `DashboardWidget`'s chrome - the
    /// same as `title_key` for almost every plugin, but empty for
    /// shortcuts, which puts its own always-there-on-hover "+" button in
    /// that same top-left corner instead (see widgets/shortcuts.rs) -
    /// mirrors `_spawn_shortcuts` passing `""` straight to `DashboardWidget`
    /// in widget_picker.py while `CATALOG` there still names it "Raccourcis"
    /// for the picker. Kept as a separate field (not a special case keyed
    /// off `kind` elsewhere) so this table stays the single source of truth
    /// per kind, per this module's whole reason for existing.
    pub card_title_key: &'static str,
    pub size: Size,
    pub spawn: fn() -> WidgetInstance,
    pub restore: fn(&serde_json::Value) -> WidgetInstance,
    /// Overrides what the widget picker shows for this kind's preview
    /// tile, instead of the default (`(spawn)().content`, live and
    /// discarded once the picker closes - see `build_tile` in
    /// widget_picker.rs). `None` for every plugin cheap enough to spawn
    /// just for a throwaway preview (a D-Bus watch, a local file read...).
    /// `Some` for a plugin whose `spawn` is itself heavy/stateful enough
    /// that a second live instance just for the picker tile is wasteful
    /// or actively harmful - the YouTube widget is the first: `spawn`
    /// starts a whole WebKit web process loading youtube.com, and the
    /// picker already builds one tile per open/close, on top of whatever
    /// instance is already placed on a page.
    pub preview: Option<fn() -> gtk::Widget>,
    /// Only one placed instance of this kind allowed across all real
    /// pages at once - the picker (`grouped_catalog` in
    /// widget_picker.rs) skips offering it again while one already
    /// exists. `false` for every plugin cheap enough to have several
    /// (a D-Bus watch, a local file read...); `true` for the YouTube
    /// widget, whose `spawn` starts a whole WebKit web process - the
    /// stability/resource cost the user asked to cap at one.
    pub singleton: bool,
}

pub static CATALOG: &[WidgetDescriptor] = &[
    WidgetDescriptor {
        kind: "clock",
        title_key: "widgets.clock.title",
        card_title_key: "widgets.clock.title",
        size: SIZE_M,
        spawn: crate::widgets::clock::spawn,
        restore: crate::widgets::clock::restore,
        preview: None,
        singleton: false,
    },
    // Two entries sharing the same widgets/audio.rs code (a scale factor
    // derived from `size` handles the visual difference - see that
    // module's own doc comment), same pattern as the dummy_* entries.
    WidgetDescriptor {
        kind: "audio_l",
        title_key: "widgets.audio.title",
        card_title_key: "widgets.audio.title",
        size: SIZE_L,
        spawn: crate::widgets::audio::spawn_l,
        restore: crate::widgets::audio::restore_l,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "audio_m",
        title_key: "widgets.audio.title",
        card_title_key: "widgets.audio.title",
        size: SIZE_M,
        spawn: crate::widgets::audio::spawn_m,
        restore: crate::widgets::audio::restore_m,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "audio_sq",
        title_key: "widgets.audio.title",
        card_title_key: "widgets.audio.title",
        size: SIZE_SQ,
        spawn: crate::widgets::audio::spawn_sq,
        restore: crate::widgets::audio::restore_sq,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "agenda",
        title_key: "widgets.agenda.title",
        card_title_key: "widgets.agenda.title",
        size: SIZE_M,
        spawn: crate::widgets::agenda::spawn,
        restore: crate::widgets::agenda::restore,
        preview: Some(crate::widgets::agenda::preview),
        singleton: false,
    },
    WidgetDescriptor {
        kind: "weather",
        title_key: "widgets.weather.title",
        card_title_key: "widgets.weather.title",
        size: SIZE_M,
        spawn: crate::widgets::weather::spawn,
        restore: crate::widgets::weather::restore,
        preview: Some(crate::widgets::weather::preview),
        singleton: false,
    },
    WidgetDescriptor {
        kind: "cpu_temp",
        title_key: "widgets.cpu_temp.title",
        card_title_key: "widgets.cpu_temp.title",
        size: SIZE_SSX,
        spawn: crate::widgets::cpu_temp::spawn,
        restore: crate::widgets::cpu_temp::restore,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "network_ssx",
        title_key: "widgets.network.title",
        card_title_key: "widgets.network.title",
        size: SIZE_SSX,
        spawn: crate::widgets::network::spawn,
        restore: crate::widgets::network::restore,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "network_sx",
        title_key: "widgets.network.title",
        card_title_key: "widgets.network.title",
        size: SIZE_SX,
        spawn: crate::widgets::network_sx::spawn,
        restore: crate::widgets::network_sx::restore,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "network_s",
        title_key: "widgets.network.title",
        card_title_key: "widgets.network.title",
        size: SIZE_S,
        spawn: crate::widgets::network_s::spawn,
        restore: crate::widgets::network_s::restore,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "network_sq",
        title_key: "widgets.network.title",
        card_title_key: "widgets.network.title",
        size: SIZE_SQ,
        spawn: crate::widgets::network_sq::spawn,
        restore: crate::widgets::network_sq::restore,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "network_m",
        title_key: "widgets.network.title",
        card_title_key: "widgets.network.title",
        size: SIZE_M,
        spawn: crate::widgets::network_m::spawn,
        restore: crate::widgets::network_m::restore,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "temp_gauge",
        title_key: "widgets.temp_gauge.title",
        card_title_key: "widgets.temp_gauge.title",
        size: SIZE_SQ,
        spawn: crate::widgets::temp_gauge::spawn,
        restore: crate::widgets::temp_gauge::restore,
        preview: None,
        singleton: false,
    },
    // Empty card_title_key: the shortcuts grid puts its own hover-revealed
    // "+" button in that same top-left corner instead of a title label -
    // see WidgetDescriptor::card_title_key's own doc comment.
    WidgetDescriptor {
        kind: "shortcuts",
        title_key: "widgets.shortcuts.title",
        card_title_key: "",
        size: SIZE_L,
        spawn: crate::widgets::shortcuts::spawn,
        restore: crate::widgets::shortcuts::restore,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "youtube",
        title_key: "widgets.youtube.title",
        card_title_key: "widgets.youtube.title",
        size: SIZE_L,
        spawn: crate::widgets::youtube::spawn,
        restore: crate::widgets::youtube::restore,
        preview: Some(crate::widgets::youtube::preview),
        singleton: true,
    },
    WidgetDescriptor {
        kind: "dummy_s",
        title_key: "widgets.dummy.title_s",
        card_title_key: "widgets.dummy.title_s",
        size: SIZE_S,
        spawn: crate::widgets::dummy::spawn_s,
        restore: crate::widgets::dummy::restore_s,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "dummy_m",
        title_key: "widgets.dummy.title_m",
        card_title_key: "widgets.dummy.title_m",
        size: SIZE_M,
        spawn: crate::widgets::dummy::spawn_m,
        restore: crate::widgets::dummy::restore_m,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "dummy_l",
        title_key: "widgets.dummy.title_l",
        card_title_key: "widgets.dummy.title_l",
        size: SIZE_L,
        spawn: crate::widgets::dummy::spawn_l,
        restore: crate::widgets::dummy::restore_l,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "dummy_sq",
        title_key: "widgets.dummy.title_sq",
        card_title_key: "widgets.dummy.title_sq",
        size: SIZE_SQ,
        spawn: crate::widgets::dummy::spawn_sq,
        restore: crate::widgets::dummy::restore_sq,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "dummy_sx",
        title_key: "widgets.dummy.title_sx",
        card_title_key: "widgets.dummy.title_sx",
        size: SIZE_SX,
        spawn: crate::widgets::dummy::spawn_sx,
        restore: crate::widgets::dummy::restore_sx,
        preview: None,
        singleton: false,
    },
    WidgetDescriptor {
        kind: "dummy_ssx",
        title_key: "widgets.dummy.title_ssx",
        card_title_key: "widgets.dummy.title_ssx",
        size: SIZE_SSX,
        spawn: crate::widgets::dummy::spawn_ssx,
        restore: crate::widgets::dummy::restore_ssx,
        preview: None,
        singleton: false,
    },
];

pub fn find(kind: &str) -> Option<&'static WidgetDescriptor> {
    CATALOG.iter().find(|d| d.kind == kind)
}

/// Wraps a plain content widget with no settings/persisted state of its
/// own into a `WidgetInstance` - the common case for the dummy widgets.
pub fn instance_without_settings(content: impl IsA<gtk::Widget>) -> WidgetInstance {
    WidgetInstance {
        content: content.upcast(),
        settings: None,
        to_dict: Box::new(|| serde_json::Value::Null),
        on_reset: None,
        on_change_ready: None,
    }
}
