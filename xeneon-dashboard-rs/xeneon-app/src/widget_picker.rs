// SPDX-License-Identifier: GPL-3.0-or-later
//! Full-screen widget picker: a `gtk::Revealer` overlay child of the app's
//! root `gtk::Overlay` (see main.rs) that slides down from the top to cover
//! the whole window, ported from `WidgetPicker` in widget_picker.py.
//! Replaces the earlier `adw::Dialog` stand-in (see git history) that only
//! proved the add-a-widget flow end to end.
//!
//! Ported in three steps, tracked in the memory system so a future session
//! can resume cleanly:
//! - **Step 1**: the overlay mechanics only - reveal/hide animation, input
//!   handling, Escape-to-close - with a plain list of catalog entries.
//!   Landed first, deliberately, to validate the trickiest part (a
//!   full-screen Gtk.Revealer/gtk::Overlay combo) on the real Xeneon
//!   hardware before adding the heavier size-grouped live-preview content.
//! - **Step 2**: the plain list is replaced with `gtk::FlowBox` sections
//!   grouped by size family (compact/medium/large - see `size_family`/
//!   `grouped_catalog` below, mirroring `_size_family`/`_grouped_catalog`
//!   in widget_picker.py), each tile showing the real, live widget content
//!   via `(descriptor.spawn)().content` instead of just a title row.
//!   Rebuilt fresh every time the picker opens and torn down on close (see
//!   `open`/`close`) rather than built once and kept alive - several
//!   entries (Clock, Audio, CpuTemp, TempGauge) own a live GLib timer or a
//!   D-Bus watch, and there's no reason to keep those ticking/polling
//!   while nobody can see them.
//! - **Step 3 (this one)**: accent-color styling (`ensure_css_installed`/
//!   `PICKER_CSS` below) - a tile's border highlights in the app's current
//!   accent on hover, and the close button's hover fill uses it too.
//!   References the `@accent_color` named color `theme.rs` defines
//!   display-wide (see that module's own doc comment for why a plain
//!   reference here needs no reload of its own even when the accent
//!   changes later).
//!
//! **The `can_target` gotcha** (the actual bug that cost the most time
//! porting this from Python, see feedback_rust_gtk_dev_loop_gotchas in the
//! memory system): a `Gtk.Revealer` overlay child with `halign`/`valign`
//! set to `Fill` is allocated the *whole* `gtk::Overlay` area by GTK
//! regardless of `reveal_child` - hidden or not. Left targetable, an
//! invisible-but-full-size Revealer like this one sits over the entire
//! window and silently swallows every click and swipe. Fixed the same way
//! `PageIndicator` (page_indicator.rs) already handles it: toggle
//! `can_target` together with `reveal_child`, never leave it targetable
//! while closed.

use adw::prelude::*;

use crate::i18n_runtime as i18n;
use crate::widgets::registry::{WidgetDescriptor, CATALOG};
use xeneon_core::grid::{Size, GAP, PAGE_W, SIZE_L, SIZE_M, SIZE_S, SIZE_SQ, SIZE_SSX, SIZE_SX};

/// The body's own left/right margin (see `new()`) - shared with
/// `rebuild_body()`'s row-wrapping math so the two stay in sync rather
/// than duplicating the same number.
const BODY_MARGIN: i32 = 24;

/// Static (never reloaded) - only ever references `@accent_color` by name,
/// which `theme.rs`'s own provider keeps redefining display-wide on every
/// `apply_accent()` call. GTK resolves named colors at style-computation
/// time, not at parse time, so this provider never needs to know when the
/// accent changes - unlike theme.rs's own provider, which owns the actual
/// value and does need to reload.
const PICKER_CSS: &str = "
.xeneon-widget-picker-header {
  padding: 18px 24px 14px 24px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}
.xeneon-widget-picker-close:hover {
  background-color: alpha(@accent_color, 0.25);
}
.xeneon-widget-picker-family {
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.06em;
  opacity: 0.55;
}
.xeneon-widget-picker-tile {
  border-radius: 12px;
  border: 1px solid rgba(255, 255, 255, 0.08);
}
.xeneon-widget-picker-tile:hover {
  border-color: @accent_color;
}
.xeneon-widget-picker-tile-name {
  font-size: 11px;
  font-weight: 600;
  color: #ffffff;
  background-color: rgba(0, 0, 0, 0.5);
  padding: 3px 8px;
  border-radius: 999px;
}
";

static INSTALL_CSS: std::sync::Once = std::sync::Once::new();

/// `pub(crate)` so `help_overlay.rs` can install the same stylesheet and
/// reuse the `.xeneon-widget-picker-header`/`-close` classes verbatim for
/// its own full-screen overlay header, instead of duplicating an
/// almost-identical block of CSS for a second overlay that looks the same.
pub(crate) fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(PICKER_CSS);
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

/// Every grid preset, smallest footprint first, paired with the i18n key
/// naming it - the single source of truth for both the section headers
/// and grouping below. Ordered by screen-space (area), not just height:
/// SSX/SX/S share S's height but differ in width, so listing them by
/// area rather than declaring an arbitrary tie order keeps "smallest
/// first" actually true.
const SIZE_ORDER: [(Size, &str); 6] = [
    (SIZE_SSX, "widgets.add_menu.size_ssx"),
    (SIZE_SX, "widgets.add_menu.size_sx"),
    (SIZE_S, "widgets.add_menu.size_s"),
    (SIZE_SQ, "widgets.add_menu.size_sq"),
    (SIZE_M, "widgets.add_menu.size_m"),
    (SIZE_L, "widgets.add_menu.size_l"),
];

fn size_label_key(size: Size) -> &'static str {
    SIZE_ORDER.iter().find(|(candidate, _)| *candidate == size).map(|(_, key)| *key).unwrap_or_else(|| {
        // Shouldn't happen - every CATALOG entry uses one of the six
        // presets above - but degrades to *some* label rather than
        // panicking if a future preset is ever added here without a
        // matching entry in SIZE_ORDER.
        "widgets.add_menu.size_m"
    })
}

/// Every CATALOG entry except the dummy placeholders (dev/test-only
/// footprint fillers, never meant to be a real user-facing choice here),
/// grouped by *exact* preset rather than the old three broad
/// small/medium/large families - two different presets sharing a family
/// (e.g. SQ and M, both "medium"-height) previously ended up side by
/// side in the same row with no visual cue telling them apart beyond a
/// text label. One section per preset makes the actual footprint the
/// grouping itself, not just a caption on top of it. A preset with
/// nothing in it is omitted rather than shown as an empty section.
/// Catalog order is kept within a section (already same-size, so
/// there's nothing meaningful left to sort by).
///
/// `existing_kinds` is every kind currently placed on any real page
/// (`WidgetGrid::kinds`, collected in main.rs right before `open()`) -
/// used to also skip a `WidgetDescriptor::singleton` kind that's already
/// placed somewhere, the same way `dummy_*` is skipped unconditionally.
fn grouped_catalog(existing_kinds: &[String]) -> Vec<(&'static str, Vec<&'static WidgetDescriptor>)> {
    let mut buckets: Vec<Vec<&'static WidgetDescriptor>> = vec![Vec::new(); SIZE_ORDER.len()];
    for descriptor in CATALOG {
        if descriptor.kind.starts_with("dummy_") {
            continue;
        }
        if descriptor.singleton && existing_kinds.iter().any(|kind| kind == descriptor.kind) {
            continue;
        }
        if let Some(index) = SIZE_ORDER.iter().position(|(size, _)| *size == descriptor.size) {
            buckets[index].push(descriptor);
        }
    }
    SIZE_ORDER
        .into_iter()
        .map(|(_, key)| key)
        .zip(buckets)
        .filter(|(_, entries)| !entries.is_empty())
        .collect()
}

/// One tile: the real widget content, live and at its true pixel size,
/// inside a plain card - no delete/configure/move chrome, since nothing
/// has been placed on a page yet. `content.set_can_target(false)` so the
/// tile's own click gesture always gets the click instead of something
/// inside the live preview (e.g. the audio widget's transport buttons)
/// swallowing it first.
fn build_tile(descriptor: &'static WidgetDescriptor, on_activate: impl Fn(&'static str) + 'static) -> gtk::Widget {
    // Everything but `content` (settings panel, to_dict, on_reset) is
    // dropped here - a preview tile has nowhere to show settings and
    // isn't persisted, so there's nothing to keep it for. Plain Rust
    // ownership makes this safe with no parenting/floating-ref concerns
    // (unlike DashboardWidget in the Python port, this content was never
    // wrapped in anything that expects to own it).
    //
    // `descriptor.preview`, when set, replaces this live `spawn()` outright
    // - see `WidgetDescriptor::preview`'s own doc comment for why (a heavy
    // or stateful `spawn`, like the YouTube widget's WebKit view, isn't
    // something the picker should pay for just to show a tile).
    let content = match descriptor.preview {
        Some(preview) => preview(),
        None => (descriptor.spawn)().content,
    };
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.set_can_target(false);

    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("card");
    card.append(&content);

    let tile = gtk::Overlay::new();
    tile.add_css_class("xeneon-widget-picker-tile");
    tile.set_size_request(descriptor.size.w, descriptor.size.h);
    // GTK computes a widget's *effective* hexpand/vexpand from its
    // descendants when not set explicitly on the widget itself - content
    // above requests both (needed so it fills a real DashboardWidget's
    // Gtk.Fixed-allocated rect on an actual page), and that request
    // otherwise propagates all the way up through card/tile to this
    // tile's GtkFlowBoxChild, stretching it to share whatever space is
    // left in its row instead of staying at the size_request set above -
    // every tile in a row ends up the same rendered width regardless of
    // its actual preset, defeating the whole point of this preview.
    // Setting it explicitly here stops that propagation at this widget.
    tile.set_hexpand(false);
    tile.set_vexpand(false);
    // Clips the card's content to the tile's own rounded corners - CSS
    // `overflow: hidden` isn't a real GTK CSS property, this widget-level
    // property is the actual mechanism (same fix needed on the Python
    // side tonight).
    tile.set_overflow(gtk::Overflow::Hidden);
    tile.set_child(Some(&card));

    // Includes the size name (e.g. "Musique · Grand") - two entries for
    // the same plugin (audio_l/audio_m/audio_sq) otherwise show the exact
    // same text and are only told apart by comparing tile dimensions,
    // which isn't obvious at a glance without something to compare
    // against side by side.
    let name_label = gtk::Label::new(Some(&format!(
        "{} · {}",
        i18n::t(descriptor.title_key),
        i18n::t(size_label_key(descriptor.size))
    )));
    name_label.add_css_class("xeneon-widget-picker-tile-name");
    name_label.set_halign(gtk::Align::Start);
    name_label.set_valign(gtk::Align::End);
    name_label.set_margin_start(6);
    name_label.set_margin_bottom(6);
    tile.add_overlay(&name_label);

    let click = gtk::GestureClick::new();
    click.connect_released(move |_, _, _, _| on_activate(descriptor.kind));
    tile.add_controller(click);

    tile.upcast()
}

pub struct WidgetPicker {
    revealer: gtk::Revealer,
    title_label: gtk::Label,
    body: gtk::Box,
    on_pick: std::rc::Rc<dyn Fn(&'static str)>,
}

impl WidgetPicker {
    pub fn widget(&self) -> &gtk::Revealer {
        &self.revealer
    }

    pub fn new(on_pick: impl Fn(&'static str) + 'static) -> std::rc::Rc<Self> {
        ensure_css_installed();

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        revealer.set_transition_duration(550);
        revealer.set_halign(gtk::Align::Fill);
        revealer.set_valign(gtk::Align::Fill);
        revealer.set_hexpand(true);
        revealer.set_vexpand(true);
        revealer.set_reveal_child(false);
        // See the can_target gotcha in the module doc comment above.
        revealer.set_can_target(false);
        revealer.add_css_class("xeneon-widget-picker");

        let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        surface.add_css_class("xeneon-widget-picker-surface");
        // Opaque, same mechanism (not just the same color) as every other
        // page including Settings: "view" is the flat-background class
        // main.rs applies to the root gtk::Overlay for the same reason
        // (see its own comment) - reusing it here means this page always
        // matches Settings/the dashboard's own background exactly, in
        // light or dark, without hardcoding a color that could drift out
        // of sync.
        surface.add_css_class("view");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.add_css_class("xeneon-widget-picker-header");
        let title_label = gtk::Label::new(Some(&i18n::t("widgets.add_menu.title")));
        title_label.add_css_class("title-2");
        title_label.set_halign(gtk::Align::Start);
        title_label.set_hexpand(true);
        header.append(&title_label);
        // Same constructor as every other close/delete button in the app
        // (dashboard_widget.rs, appearance_popover.rs) rather than
        // `Button::new()` + `set_icon_name()` separately - matches a
        // known-working pattern exactly rather than a second path to the
        // same result.
        let close_button = gtk::Button::from_icon_name("window-close-symbolic");
        close_button.add_css_class("flat");
        close_button.add_css_class("circular");
        close_button.add_css_class("xeneon-widget-picker-close");
        close_button.set_tooltip_text(Some(&i18n::t("widgets.appearance.close_tooltip")));
        header.append(&close_button);
        surface.append(&header);

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 20);
        body.add_css_class("xeneon-widget-picker-body");
        body.set_margin_top(12);
        body.set_margin_bottom(20);
        body.set_margin_start(BODY_MARGIN);
        body.set_margin_end(BODY_MARGIN);
        scroller.set_child(Some(&body));
        surface.append(&scroller);

        revealer.set_child(Some(&surface));

        let close = {
            let revealer = revealer.clone();
            move || {
                revealer.set_reveal_child(false);
                revealer.set_can_target(false);
            }
        };

        close_button.connect_clicked({
            let close = close.clone();
            move |_| close()
        });

        // Escape-to-close is handled at the *window* level instead of a
        // Gtk.EventControllerKey on this revealer (there used to be one
        // here - see git history) - an EventController's default Bubble
        // propagation phase only sees a key event after GTK has already
        // routed it to whatever widget currently holds keyboard focus,
        // and `open()`'s `revealer.grab_focus()` call has nothing
        // focusable to actually land on inside this tree (every tile's
        // own content is `can_target(false)`, and a plain Gtk.Revealer
        // isn't focusable itself) - so focus silently stays wherever it
        // already was, outside this revealer's own subtree entirely, and
        // Escape here never fired in practice. main.rs's own
        // window-level controller is Capture phase (sees every key press
        // before GTK dispatches it anywhere), so that's where Escape is
        // wired instead - see its AppMsg::CloseWidgetPicker handling.

        let picker =
            std::rc::Rc::new(Self { revealer, title_label, body, on_pick: std::rc::Rc::new(on_pick) });

        i18n::on_change({
            let picker = picker.clone();
            move || picker.retranslate()
        });

        picker
    }

    pub fn open(self: &std::rc::Rc<Self>, existing_kinds: &[String]) {
        self.rebuild_body(existing_kinds);
        self.revealer.set_can_target(true);
        self.revealer.set_reveal_child(true);
        self.revealer.grab_focus();
    }

    pub fn close(&self) {
        self.revealer.set_reveal_child(false);
        self.revealer.set_can_target(false);
        self.clear_body();
    }

    fn clear_body(&self) {
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
    }

    fn rebuild_body(self: &std::rc::Rc<Self>, existing_kinds: &[String]) {
        self.clear_body();
        // Manually wrapped plain `gtk::Box` rows, not `gtk::FlowBox`:
        // FlowBox's own row-height negotiation - shared across every
        // child in a line even with homogeneous(false), which only turns
        // off shared *column width* - produced a bizarre square
        // allocation for two SIZE_M tiles sharing a row (830x830
        // measured, instead of 832x336) neither of their own intrinsic
        // sizes explains, hence the earlier switch away from it (see git
        // history). But every entry in one `entries` group shares the
        // exact same preset width (grouped_catalog groups by exact size),
        // so a *single* un-wrapped row is only safe as long as that many
        // same-size tiles still fit across the screen - a section with
        // enough entries (e.g. every "Moyen"-preset widget) can exceed
        // that easily. The picker's own ScrolledWindow has its horizontal
        // policy set to Never (see `new()`), which in GTK means "never
        // show a horizontal scrollbar" - *not* "clip this axis": with
        // nothing to negotiate against, an overflowing row's natural
        // width propagates straight up through the whole widget tree and
        // forces the *entire application window* wider than the physical
        // Xeneon panel, pushing everything past its right edge off
        // screen entirely (this is exactly what pushed the close button
        // out of view - see the "close button" debugging trail in git
        // history for how that was tracked down). Wrapping to a new row
        // by hand, computed from each preset's own known width, avoids
        // both problems at once: no FlowBox row-height coupling, and no
        // row ever wider than the page itself.
        let available_width = PAGE_W - 2 * BODY_MARGIN;
        for (size_key, entries) in grouped_catalog(existing_kinds) {
            let section = gtk::Box::new(gtk::Orientation::Vertical, 8);

            let label = gtk::Label::new(Some(&i18n::t(size_key)));
            label.add_css_class("xeneon-widget-picker-family");
            label.set_halign(gtk::Align::Start);
            section.append(&label);

            let mut row = gtk::Box::new(gtk::Orientation::Horizontal, GAP);
            row.set_halign(gtk::Align::Start);
            let mut row_width = 0;
            let mut row_has_tile = false;
            for descriptor in entries {
                let tile_width = descriptor.size.w;
                let width_with_this_tile =
                    if row_has_tile { row_width + GAP + tile_width } else { tile_width };
                if row_has_tile && width_with_this_tile > available_width {
                    section.append(&row);
                    row = gtk::Box::new(gtk::Orientation::Horizontal, GAP);
                    row.set_halign(gtk::Align::Start);
                    row_width = 0;
                    row_has_tile = false;
                }

                let picker = self.clone();
                let tile = build_tile(descriptor, move |kind| {
                    (picker.on_pick)(kind);
                    picker.close();
                });
                row.append(&tile);
                row_width = if row_has_tile { row_width + GAP + tile_width } else { tile_width };
                row_has_tile = true;
            }
            if row_has_tile {
                section.append(&row);
            }

            self.body.append(&section);
        }
    }

    fn retranslate(&self) {
        self.title_label.set_label(&i18n::t("widgets.add_menu.title"));
        // The body is rebuilt fresh every open() (see its own doc comment
        // above), so a language change while the picker happens to be
        // closed is picked up automatically next time - nothing stale to
        // retranslate here while it's shut.
    }
}
