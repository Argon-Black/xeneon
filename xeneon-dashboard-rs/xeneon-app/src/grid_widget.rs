//! One carousel page: a `gtk::Fixed` driven by hand rather than a Relm4
//! factory. Relm4's usual `FactoryVecDeque` pattern assumes a linear
//! container (append/remove in order) - it doesn't fit a container placed
//! by absolute (x, y) coordinates, where a "move" is a first-class
//! operation on an existing child, not a reorder. So this is deliberately
//! plain gtk4-rs: a `Vec` of placed widgets behind an `Rc<RefCell<_>>`,
//! with drag/delete wired up directly via closures. Mirrors `WidgetGrid`
//! in grid.py, including its persistence: a widget is saved to
//! `widgets/<id>.json` the moment it's added and again after every valid
//! drag, and the page's own `pages/<id>.json` is (re)saved when renamed.

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Once;
use xeneon_core::appearance::WidgetAppearance;
use xeneon_core::grid::{self, Rect, Size};
use xeneon_core::page_state::{self, PageState};
use xeneon_core::widget_state::{self, WidgetState};

use crate::i18n_runtime as i18n;
use crate::widgets::registry::WidgetInstance;

struct PlacedWidget {
    id: String,
    kind: String,
    rect: Rect,
    css_class: String,
    /// Shared with the live popover, if open - see
    /// `DashboardWidgetHandles::appearance`'s own doc comment.
    appearance: Rc<RefCell<WidgetAppearance>>,
    to_dict: Rc<dyn Fn() -> serde_json::Value>,
}

pub struct WidgetGrid {
    fixed: gtk::Fixed,
    page_w: i32,
    page_h: i32,
    // Shared (not a plain usize) so the save closures set up in
    // `insert_at` - which must be 'static and so can't borrow `self` -
    // can still read the *current* value at save time rather than the
    // value that was current when the widget was first added. Needed
    // once a page's index can shift after the fact (an earlier page
    // deleted, see `set_page_index`): a closure that had baked in a
    // stale index would keep re-saving widgets under the wrong page
    // forever, silently corrupting persistence.
    page_index: Rc<Cell<usize>>,
    page_id: String,
    custom_name: RefCell<Option<String>>,
    widgets_dir: PathBuf,
    pages_dir: PathBuf,
    /// False for the dev-mode test page: its dummy widgets are
    /// regenerated fresh in code on every launch (see `dev_mode_enabled`
    /// in main.rs), so they must never write to the same
    /// widgets/pages directories a real page's content lives in - that
    /// would leak throwaway test content into the persisted layout.
    persist: bool,
    placed: Rc<RefCell<Vec<PlacedWidget>>>,
    /// Fired when a widget delete leaves this page with zero widgets -
    /// lets the caller (`main.rs`) decide whether to remove the page
    /// itself (it always keeps the first page, deleted or not - see
    /// `AppMsg::PageEmptied`). Not fired by anything other than an actual
    /// user delete: starting empty (a freshly created page) or emptying
    /// via page teardown doesn't go through this path.
    on_emptied: Rc<RefCell<Option<Box<dyn Fn()>>>>,
}

/// One stable CSS class per page, scoped by `page_id` rather than shared
/// across every page - needed once a later phase lets a page override the
/// app-wide default image with its own (see `set_background_image`'s own
/// doc comment), at which point pages can no longer all share one rule.
fn background_css_class(page_id: &str) -> String {
    format!("xeneon-page-background-{page_id}")
}

static INSTALL_PREVIEW_CSS: Once = Once::new();

/// The dragged widget's real card never moves during a drag - only this
/// translucent outline does, at the *snapped* candidate position, green
/// while the landing spot is free and red while it isn't. The real widget
/// only jumps to its new spot once, on release, if the drop was valid.
/// Moving the real widget live on every pointer event (an earlier version
/// of this file did that) reads as a visible flicker/jank since it jumps
/// between snapped positions rather than following the pointer smoothly -
/// this is the same fix `_move_preview` provides in grid.py.
fn ensure_preview_css_installed() {
    INSTALL_PREVIEW_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        css.load_from_string(
            "
            .xeneon-move-preview {
                border-radius: 8px;
            }
            .xeneon-move-preview-valid {
                background-color: rgba(46, 204, 113, 0.35);
                border: 2px solid rgba(46, 204, 113, 0.9);
            }
            .xeneon-move-preview-invalid {
                background-color: rgba(231, 76, 60, 0.35);
                border: 2px solid rgba(231, 76, 60, 0.9);
            }
            ",
        );
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

/// Drag state alive only between `drag-begin` and `drag-end`: the ghost
/// widget itself, the last candidate rect it was moved to, and whether
/// that candidate is currently a valid (unoccupied) landing spot.
struct MovePreview {
    ghost: gtk::Box,
    candidate: Rect,
    valid: bool,
}

impl WidgetGrid {
    /// A brand-new page: fresh `page_id`, no custom name yet.
    pub fn new(page_w: i32, page_h: i32, page_index: usize, widgets_dir: PathBuf, pages_dir: PathBuf) -> Self {
        Self::restore(page_w, page_h, page_index, uuid::Uuid::new_v4().to_string(), None, widgets_dir, pages_dir)
    }

    /// A page reconstructed from a saved `PageState` (or a fresh one, via
    /// [`Self::new`] above) - same underlying constructor either way, just
    /// with an existing identity/name instead of a generated one.
    pub fn restore(
        page_w: i32,
        page_h: i32,
        page_index: usize,
        page_id: String,
        custom_name: Option<String>,
        widgets_dir: PathBuf,
        pages_dir: PathBuf,
    ) -> Self {
        Self::build(page_w, page_h, page_index, page_id, custom_name, widgets_dir, pages_dir, true)
    }

    /// A page whose widgets are never written to disk - see the `persist`
    /// field's own doc comment. Used only for the dev-mode test page.
    pub fn ephemeral(page_w: i32, page_h: i32, page_index: usize) -> Self {
        Self::build(page_w, page_h, page_index, uuid::Uuid::new_v4().to_string(), None, PathBuf::new(), PathBuf::new(), false)
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        page_w: i32,
        page_h: i32,
        page_index: usize,
        page_id: String,
        custom_name: Option<String>,
        widgets_dir: PathBuf,
        pages_dir: PathBuf,
        persist: bool,
    ) -> Self {
        ensure_preview_css_installed();
        let fixed = gtk::Fixed::new();
        // Sized to the *full* carousel page (content area plus the GAP
        // bezel on every side), not `gtk::Widget`'s own margin properties -
        // a margin is space *outside* a widget's own paint box, which would
        // leave the GAP bezel unpainted by this page's own background image
        // (see `set_background_image` below), showing a plain border
        // around it instead of the full-bleed, edge-to-edge look that's
        // meant to have. page_w/page_h stay the *content* coordinate space
        // every x/y in this file is expressed in (matching
        // xeneon_core::grid::PAGE_W/PAGE_H and its find_free_position/
        // snap_to_layout math) - GAP is added right at the point each one
        // is actually put/moved onto `fixed`, not baked in here.
        fixed.set_size_request(page_w + 2 * grid::GAP, page_h + 2 * grid::GAP);
        fixed.add_css_class(&background_css_class(&page_id));
        Self {
            fixed,
            page_w,
            page_h,
            page_index: Rc::new(Cell::new(page_index)),
            page_id,
            custom_name: RefCell::new(custom_name),
            widgets_dir,
            pages_dir,
            persist,
            placed: Rc::new(RefCell::new(Vec::new())),
            on_emptied: Rc::new(RefCell::new(None)),
        }
    }

    /// The underlying widget to embed in a container (e.g. an
    /// `adw::Carousel` page).
    pub fn widget(&self) -> &gtk::Fixed {
        &self.fixed
    }

    /// Renders `path` (or, with `None`, clears it) as this page's full-bleed
    /// background - `cover`-scaled, no border, filling `fixed`'s entire
    /// paint box edge to edge (see `build()`'s own note on why margins were
    /// swapped for a grown `size_request` + GAP-offset placement to make
    /// that possible). Driven today only by the app-wide
    /// `Config.app_background_image_path` setting (applied to every real
    /// page uniformly from `main.rs`); not yet persisted per page - that's
    /// the later per-page-override phase the settings row's own subtitle
    /// mentions.
    pub fn set_background_image(&self, path: Option<&str>) {
        let rule = path.map(|path| {
            let uri = gtk::gio::File::for_path(path).uri();
            format!(
                ".{class} {{ background-image: url('{uri}'); background-size: cover; \
                 background-position: center; background-repeat: no-repeat; }}",
                class = background_css_class(&self.page_id)
            )
        });
        crate::appearance_css::set_raw_rule(&background_css_class(&self.page_id), rule);
    }

    /// Stable identity (the `pages/<id>.json` filename), independent of
    /// `page_index` - used by `AppMsg::PageEmptied` to find this page
    /// again after the message round-trips through the event loop (a
    /// `page_index` alone wouldn't survive another page being deleted
    /// first).
    pub fn page_id(&self) -> &str {
        &self.page_id
    }

    pub fn page_index(&self) -> usize {
        self.page_index.get()
    }

    /// Re-numbers this page (its own persisted file, if named, and every
    /// currently-placed widget's file) - called when an earlier page is
    /// deleted and this one shifts down to fill the gap, so persistence
    /// stays contiguous (`0..page_count`) for the next restart. See the
    /// `page_index` field's own doc comment for why a plain field
    /// wouldn't be enough on its own.
    pub fn set_page_index(&self, new_index: usize) {
        self.page_index.set(new_index);
        self.save_page_state();
        if !self.persist {
            return;
        }
        for p in self.placed.borrow().iter() {
            if let Err(err) = widget_state::update_page_index(&self.widgets_dir, &p.id, new_index) {
                eprintln!("xeneon-dashboard: failed to reindex widget {}: {err}", p.id);
            }
        }
    }

    /// Registers the callback fired when a widget delete leaves this page
    /// empty - see the `on_emptied` field's own doc comment. Overwrites
    /// any previously registered callback (only one caller ever needs
    /// this, `main.rs` at page-creation time).
    pub fn set_on_emptied(&self, cb: impl Fn() + 'static) {
        *self.on_emptied.borrow_mut() = Some(Box::new(cb));
    }

    pub fn custom_name(&self) -> Option<String> {
        self.custom_name.borrow().clone()
    }

    /// The custom name if set, else a templated "Page {n}" (1-based) -
    /// mirrors `WidgetGrid.display_name()` in grid.py.
    pub fn display_name(&self) -> String {
        match self.custom_name.borrow().as_deref() {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => i18n::t_args("settings.pages_group.default_name", &[("n", &(self.page_index() + 1).to_string())]),
        }
    }

    /// Sets (or clears, with `None`/whitespace-only) the page's custom
    /// name and immediately persists it - matches the Python original's
    /// `on_apply`/`on_restore` normalizing an empty entry to `None` rather
    /// than storing an empty string.
    pub fn set_custom_name(&self, name: Option<String>) {
        let cleaned = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        *self.custom_name.borrow_mut() = cleaned;
        self.save_page_state();
    }

    fn save_page_state(&self) {
        if !self.persist {
            return;
        }
        let state = PageState {
            id: self.page_id.clone(),
            page_index: self.page_index(),
            name: self.custom_name(),
            background: serde_json::Value::Null,
        };
        if let Err(err) = page_state::save(&self.pages_dir, &state) {
            eprintln!("xeneon-dashboard: failed to save page {}: {err}", self.page_id);
        }
    }

    /// Cheap check for "would `add_widget` succeed for this size", without
    /// actually constructing anything - lets a caller pick which page to
    /// add to *before* spawning a live widget instance (which may already
    /// own resources like Clock's tick timer that would otherwise need
    /// tearing down again immediately if the spawn turned out unused).
    pub fn has_room_for(&self, size: Size) -> bool {
        let occupied: Vec<Rect> = self.placed.borrow().iter().map(|p| p.rect).collect();
        grid::find_free_position(&occupied, size, self.page_w, self.page_h).is_some()
    }

    /// Places a freshly spawned widget at the first free spot for `size`,
    /// titled `title_key` (an i18n key, or `""` for no header label) and
    /// identified by `kind` (what a saved file's `"kind"` field will read
    /// back as - see `widgets::registry`). Saved to disk immediately.
    /// Returns `None` if the page is already full - mirrors
    /// `WidgetGrid.find_free_position` returning `None` in the Python
    /// original (the caller would then try another page; multi-page
    /// overflow isn't wired up yet in this phase).
    pub fn add_widget(&self, title_key: &str, kind: &str, size: Size, instance: WidgetInstance) -> Option<String> {
        let occupied: Vec<Rect> = self.placed.borrow().iter().map(|p| p.rect).collect();
        let (x, y) = grid::find_free_position(&occupied, size, self.page_w, self.page_h)?;
        let id = uuid::Uuid::new_v4().to_string();
        let rect = Rect::new(x, y, size.w, size.h);
        let to_dict = instance.to_dict.as_ref()();
        // Fresh widget: the global default-appearance setting applies,
        // mirroring window.py's add_widget() - except for a dummy widget
        // (its per-size color lives in its own inline CSS class, not the
        // generic WidgetAppearance system - see widgets/dummy.rs) or a
        // non-persisting page (the dev-mode test page), which both stay
        // untouched/plain, same as every spawn in the Python original.
        let appearance = if self.persist && !kind.starts_with("dummy_") {
            WidgetAppearance::from_config_default(&crate::config_store::get().default_widget_appearance)
        } else {
            WidgetAppearance::default()
        };
        self.insert_at(id.clone(), kind.to_string(), title_key, rect, appearance.clone(), instance);
        if self.persist {
            self.save_widget_state(&id, kind, rect, appearance, to_dict);
        }
        Some(id)
    }

    /// Re-places a widget exactly as `state` describes (its saved id,
    /// position and appearance, not a freshly scanned free spot) - used
    /// when rebuilding a page from disk at startup. Not re-saved
    /// immediately since it already matches what's on disk (the
    /// registry's `restore` already applied `state.content` before
    /// handing back `instance`).
    pub fn restore_widget(&self, state: &WidgetState, title_key: &str, instance: WidgetInstance) {
        let rect = Rect::new(state.x, state.y, state.w, state.h);
        self.insert_at(state.id.clone(), state.kind.clone(), title_key, rect, state.appearance.clone(), instance);
    }

    fn save_widget_state(&self, id: &str, kind: &str, rect: Rect, appearance: WidgetAppearance, content: serde_json::Value) {
        let state = WidgetState {
            id: id.to_string(),
            kind: kind.to_string(),
            page_index: self.page_index(),
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
            appearance,
            content,
        };
        if let Err(err) = widget_state::save(&self.widgets_dir, &state) {
            eprintln!("xeneon-dashboard: failed to save widget {id}: {err}");
        }
    }

    /// Force-applies `appearance` to every real (non-dummy) widget on this
    /// page, re-rendering its CSS live and persisting immediately - used by
    /// the "apply default appearance to all widgets" button in the settings
    /// page's appearance group. Dummy widgets are skipped (their per-size
    /// color lives outside the generic WidgetAppearance system, see
    /// `add_widget()`'s own note). A non-persisting page (the dev-mode test
    /// page) is never actually reached here - it's never part of the page
    /// list the settings page is given - but the `persist` check stays for
    /// the same reason every other save path in this file has one.
    pub fn apply_appearance_to_all(&self, appearance: &WidgetAppearance) {
        for placed in self.placed.borrow().iter() {
            if placed.kind.starts_with("dummy_") {
                continue;
            }
            *placed.appearance.borrow_mut() = appearance.clone();
            crate::appearance_css::apply(&placed.css_class, &placed.appearance.borrow());
            if !self.persist {
                continue;
            }
            let state = WidgetState {
                id: placed.id.clone(),
                kind: placed.kind.clone(),
                page_index: self.page_index(),
                x: placed.rect.x,
                y: placed.rect.y,
                w: placed.rect.w,
                h: placed.rect.h,
                appearance: appearance.clone(),
                content: (placed.to_dict)(),
            };
            if let Err(err) = widget_state::save(&self.widgets_dir, &state) {
                eprintln!("xeneon-dashboard: failed to save widget {}: {err}", placed.id);
            }
        }
    }

    fn insert_at(&self, id: String, kind: String, title_key: &str, rect: Rect, appearance: WidgetAppearance, instance: WidgetInstance) {
        let WidgetInstance { content, settings, to_dict, on_reset } = instance;
        let to_dict: Rc<dyn Fn() -> serde_json::Value> = Rc::from(to_dict);
        let css_class = format!("xeneon-appearance-{id}");
        let handles = crate::dashboard_widget::build(
            title_key,
            &content,
            rect.w,
            rect.h,
            css_class.clone(),
            appearance,
            settings.as_ref(),
            on_reset,
        );
        let root_widget: gtk::Widget = handles.root.clone().upcast();

        self.fixed.put(&root_widget, (rect.x + grid::GAP) as f64, (rect.y + grid::GAP) as f64);
        self.placed.borrow_mut().push(PlacedWidget {
            id: id.clone(),
            kind: kind.clone(),
            rect,
            css_class,
            appearance: handles.appearance.clone(),
            to_dict: to_dict.clone(),
        });

        {
            let popover = &handles.settings_popover;
            let widgets_dir = self.widgets_dir.clone();
            let page_index = self.page_index.clone();
            let persist = self.persist;
            let placed = self.placed.clone();
            let id = id.clone();
            let kind = kind.clone();
            let to_dict = to_dict.clone();
            let appearance = handles.appearance.clone();
            popover.connect_closed(move |_| {
                if !persist {
                    return;
                }
                let Some(rect) = placed.borrow().iter().find(|p| p.id == id).map(|p| p.rect) else { return };
                let state = WidgetState {
                    id: id.clone(),
                    kind: kind.clone(),
                    page_index: page_index.get(),
                    x: rect.x,
                    y: rect.y,
                    w: rect.w,
                    h: rect.h,
                    appearance: appearance.borrow().clone(),
                    content: to_dict(),
                };
                if let Err(err) = widget_state::save(&widgets_dir, &state) {
                    eprintln!("xeneon-dashboard: failed to save widget {id}: {err}");
                }
            });
        }

        {
            let placed = self.placed.clone();
            let fixed = self.fixed.clone();
            let widgets_dir = self.widgets_dir.clone();
            let persist = self.persist;
            let id = id.clone();
            let root_widget = root_widget.clone();
            let on_emptied = self.on_emptied.clone();
            handles.delete_button.connect_clicked(move |_| {
                fixed.remove(&root_widget);
                placed.borrow_mut().retain(|p| p.id != id);
                if persist {
                    if let Err(err) = widget_state::delete(&widgets_dir, &id) {
                        eprintln!("xeneon-dashboard: failed to delete saved widget {id}: {err}");
                    }
                }
                if placed.borrow().is_empty() {
                    if let Some(cb) = on_emptied.borrow().as_ref() {
                        cb();
                    }
                }
            });
        }

        let drag = gtk::GestureDrag::new();
        let start_rect = Rc::new(Cell::new(rect));
        let preview: Rc<RefCell<Option<MovePreview>>> = Rc::new(RefCell::new(None));

        {
            let start_rect = start_rect.clone();
            let placed = self.placed.clone();
            let fixed = self.fixed.clone();
            let preview = preview.clone();
            let id = id.clone();
            drag.connect_drag_begin(move |gesture, _, _| {
                // Claim the sequence immediately (a dedicated move button,
                // not an ambiguous whole-card drag) so the ancestor
                // Adw.Carousel's swipe recognizer never steals a
                // horizontal drag - same fix as the Python original.
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let Some(current) = placed.borrow().iter().find(|p| p.id == id).map(|p| p.rect) else {
                    return;
                };
                start_rect.set(current);

                let ghost = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                ghost.set_size_request(current.w, current.h);
                ghost.add_css_class("xeneon-move-preview");
                ghost.add_css_class("xeneon-move-preview-valid");
                fixed.put(&ghost, (current.x + grid::GAP) as f64, (current.y + grid::GAP) as f64);
                *preview.borrow_mut() = Some(MovePreview { ghost, candidate: current, valid: true });
            });
        }
        {
            let start_rect = start_rect.clone();
            let placed = self.placed.clone();
            let fixed = self.fixed.clone();
            let page_w = self.page_w;
            let page_h = self.page_h;
            let id = id.clone();
            let preview = preview.clone();
            drag.connect_drag_update(move |_, offset_x, offset_y| {
                let start = start_rect.get();
                let raw_x = start.x + offset_x.round() as i32;
                let raw_y = start.y + offset_y.round() as i32;
                let others: Vec<Rect> = placed.borrow().iter().filter(|p| p.id != id).map(|p| p.rect).collect();
                let size = Size::new(start.w, start.h);
                let (snapped_x, snapped_y) = grid::snap_to_layout(&others, size, raw_x, raw_y, page_w, page_h);
                let candidate = Rect::new(snapped_x, snapped_y, size.w, size.h);
                let valid = grid::is_free(&others, candidate);

                let mut preview = preview.borrow_mut();
                let Some(state) = preview.as_mut() else { return };
                if state.candidate != candidate {
                    fixed.move_(&state.ghost, (candidate.x + grid::GAP) as f64, (candidate.y + grid::GAP) as f64);
                    state.candidate = candidate;
                }
                if state.valid != valid {
                    if valid {
                        state.ghost.remove_css_class("xeneon-move-preview-invalid");
                        state.ghost.add_css_class("xeneon-move-preview-valid");
                    } else {
                        state.ghost.remove_css_class("xeneon-move-preview-valid");
                        state.ghost.add_css_class("xeneon-move-preview-invalid");
                    }
                    state.valid = valid;
                }
            });
        }
        {
            let placed = self.placed.clone();
            let fixed = self.fixed.clone();
            let widgets_dir = self.widgets_dir.clone();
            let page_index = self.page_index.clone();
            let persist = self.persist;
            let id = id.clone();
            let kind = kind.clone();
            let root_widget = root_widget.clone();
            let preview = preview.clone();
            let to_dict = to_dict.clone();
            let appearance = handles.appearance.clone();
            drag.connect_drag_end(move |_, _, _| {
                let Some(state) = preview.borrow_mut().take() else { return };
                fixed.remove(&state.ghost);
                if state.valid {
                    fixed.move_(&root_widget, (state.candidate.x + grid::GAP) as f64, (state.candidate.y + grid::GAP) as f64);
                    if let Some(p) = placed.borrow_mut().iter_mut().find(|p| p.id == id) {
                        p.rect = state.candidate;
                    }
                    if !persist {
                        return;
                    }
                    let saved = WidgetState {
                        id: id.clone(),
                        kind: kind.clone(),
                        page_index: page_index.get(),
                        x: state.candidate.x,
                        y: state.candidate.y,
                        w: state.candidate.w,
                        h: state.candidate.h,
                        appearance: appearance.borrow().clone(),
                        content: to_dict(),
                    };
                    if let Err(err) = widget_state::save(&widgets_dir, &saved) {
                        eprintln!("xeneon-dashboard: failed to save widget {id}: {err}");
                    }
                }
                // Invalid drop: the real widget never moved, so simply
                // discarding the preview leaves it exactly where it was.
            });
        }
        handles.move_button.add_controller(drag);
    }
}
