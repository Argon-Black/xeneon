//! One carousel page: a `gtk::Fixed` driven by hand rather than a Relm4
//! factory. Relm4's usual `FactoryVecDeque` pattern assumes a linear
//! container (append/remove in order) - it doesn't fit a container placed
//! by absolute (x, y) coordinates, where a "move" is a first-class
//! operation on an existing child, not a reorder. So this is deliberately
//! plain gtk4-rs: a `Vec` of placed widgets behind an `Rc<RefCell<_>>`,
//! with drag/delete wired up directly via closures. Mirrors `WidgetGrid`
//! in grid.py, simplified for this phase (no move-preview outline yet -
//! dragging moves the real widget live and snaps on every step).

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
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

impl WidgetGrid {
    pub fn new(page_w: i32, page_h: i32) -> Self {
        let fixed = gtk::Fixed::new();
        fixed.set_size_request(page_w, page_h);
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
        {
            let start_rect = start_rect.clone();
            let placed = self.placed.clone();
            let id = id.clone();
            drag.connect_drag_begin(move |gesture, _, _| {
                // Claim the sequence immediately (a dedicated move button,
                // not an ambiguous whole-card drag) so the ancestor
                // Adw.Carousel's swipe recognizer never steals a
                // horizontal drag - same fix as the Python original.
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if let Some(p) = placed.borrow().iter().find(|p| p.id == id) {
                    start_rect.set(p.rect);
                }
            });
        }
        {
            let placed = self.placed.clone();
            let fixed = self.fixed.clone();
            let page_w = self.page_w;
            let page_h = self.page_h;
            let id = id.clone();
            let root_widget = root_widget.clone();
            drag.connect_drag_update(move |_, offset_x, offset_y| {
                let start = start_rect.get();
                let raw_x = start.x + offset_x.round() as i32;
                let raw_y = start.y + offset_y.round() as i32;
                let others: Vec<Rect> = placed.borrow().iter().filter(|p| p.id != id).map(|p| p.rect).collect();
                let size = Size::new(start.w, start.h);
                let (snapped_x, snapped_y) = grid::snap_to_layout(&others, size, raw_x, raw_y, page_w, page_h);
                let candidate = Rect::new(snapped_x, snapped_y, size.w, size.h);
                if grid::is_free(&others, candidate) {
                    fixed.move_(&root_widget, snapped_x as f64, snapped_y as f64);
                    if let Some(p) = placed.borrow_mut().iter_mut().find(|p| p.id == id) {
                        p.rect = candidate;
                    }
                }
            });
        }
        handles.move_button.add_controller(drag);
    }
}
