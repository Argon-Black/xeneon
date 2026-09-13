//! One carousel page: a `gtk::Fixed` driven by hand rather than a Relm4
//! factory. Relm4's usual `FactoryVecDeque` pattern assumes a linear
//! container (append/remove in order) - it doesn't fit a container placed
//! by absolute (x, y) coordinates, where a "move" is a first-class
//! operation on an existing child, not a reorder. So this is deliberately
//! plain gtk4-rs: a `Vec` of placed widgets behind an `Rc<RefCell<_>>`,
//! with drag/delete wired up directly via closures. Mirrors `WidgetGrid`
//! in grid.py.

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Once;
use xeneon_core::grid::{self, Rect, Size};

struct PlacedWidget {
    id: String,
    rect: Rect,
}

pub struct WidgetGrid {
    fixed: gtk::Fixed,
    page_w: i32,
    page_h: i32,
    placed: Rc<RefCell<Vec<PlacedWidget>>>,
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
    pub fn new(page_w: i32, page_h: i32) -> Self {
        ensure_preview_css_installed();
        let fixed = gtk::Fixed::new();
        fixed.set_size_request(page_w, page_h);
        // Keeps widgets off the physical screen edge, matching
        // window.py's PAGE_MARGIN - deliberately GAP on all four sides so
        // the page edge reads as just another gap in the grid, per
        // CLAUDE.md. page_w/page_h are the content area *inside* this
        // margin (see xeneon_core::grid::PAGE_W/PAGE_H), not the full
        // screen, so the margin is added on top rather than eating into
        // them.
        fixed.set_margin_start(grid::GAP);
        fixed.set_margin_end(grid::GAP);
        fixed.set_margin_top(grid::GAP);
        fixed.set_margin_bottom(grid::GAP);
        Self { fixed, page_w, page_h, placed: Rc::new(RefCell::new(Vec::new())) }
    }

    /// The underlying widget to embed in a container (e.g. an
    /// `adw::Carousel` page).
    pub fn widget(&self) -> &gtk::Fixed {
        &self.fixed
    }

    /// Places `content` (wrapped in the generic chrome) at the first free
    /// spot for `size`, titled `title`. Returns `None` if the page is
    /// already full - mirrors `WidgetGrid.find_free_position` returning
    /// `None` in the Python original (the caller would then try another
    /// page; multi-page overflow isn't wired up yet in this phase).
    pub fn add_widget(&self, title: &str, size: Size, content: impl IsA<gtk::Widget>) -> Option<String> {
        let occupied: Vec<Rect> = self.placed.borrow().iter().map(|p| p.rect).collect();
        let (x, y) = grid::find_free_position(&occupied, size, self.page_w, self.page_h)?;
        let id = uuid::Uuid::new_v4().to_string();
        self.insert_at(id.clone(), title, Rect::new(x, y, size.w, size.h), content);
        Some(id)
    }

    fn insert_at(&self, id: String, title: &str, rect: Rect, content: impl IsA<gtk::Widget>) {
        let handles = crate::dashboard_widget::build(title, &content, rect.w, rect.h);
        let root_widget: gtk::Widget = handles.root.clone().upcast();

        self.fixed.put(&root_widget, rect.x as f64, rect.y as f64);
        self.placed.borrow_mut().push(PlacedWidget { id: id.clone(), rect });

        {
            let placed = self.placed.clone();
            let fixed = self.fixed.clone();
            let id = id.clone();
            let root_widget = root_widget.clone();
            handles.delete_button.connect_clicked(move |_| {
                fixed.remove(&root_widget);
                placed.borrow_mut().retain(|p| p.id != id);
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
                fixed.put(&ghost, current.x as f64, current.y as f64);
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
                    fixed.move_(&state.ghost, candidate.x as f64, candidate.y as f64);
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
            let id = id.clone();
            let root_widget = root_widget.clone();
            let preview = preview.clone();
            drag.connect_drag_end(move |_, _, _| {
                let Some(state) = preview.borrow_mut().take() else { return };
                fixed.remove(&state.ghost);
                if state.valid {
                    fixed.move_(&root_widget, state.candidate.x as f64, state.candidate.y as f64);
                    if let Some(p) = placed.borrow_mut().iter_mut().find(|p| p.id == id) {
                        p.rect = state.candidate;
                    }
                }
                // Invalid drop: the real widget never moved, so simply
                // discarding the preview leaves it exactly where it was.
            });
        }
        handles.move_button.add_controller(drag);
    }
}
