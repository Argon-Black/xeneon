//! Grid size presets and the pure positioning math (snap-to-grid, free-space
//! search) that both the drag/drop UI and the "add a new widget" flow rely
//! on. Ported from `xeneon_dashboard/grid.py` in the Python app - see that
//! file and the "Widgets (plugins)" section of CLAUDE.md for the original
//! rationale. None of this module touches GTK: it's plain arithmetic on
//! rectangles, which is what makes it unit-testable without a display.

/// Gap between two widgets, and also the page's own edge margin (see
/// `PAGE_MARGIN` in window.py) - deliberately the same value everywhere so
/// the whole layout reads with one consistent rhythm.
pub const GAP: i32 = 16;

/// A fixed widget footprint in logical pixels. All the `SIZE_*` presets
/// below are `Size` values; a widget is always exactly one of them, never a
/// freely-resized rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub w: i32,
    pub h: i32,
}

impl Size {
    pub const fn new(w: i32, h: i32) -> Self {
        Self { w, h }
    }
}

// All of this assumes the Xeneon Edge's real panel resolution, 2560x720,
// at 100% display scale (logical px == physical px - see CLAUDE.md). If
// that scale ever changes, every constant below needs recomputing from
// `Gdk.Monitor.get_geometry()` (or Rust's `gdk4::Monitor::geometry()`), not
// just S/M/L but SQ/SX/SSX too since they're all derived from these.

/// Small: one sixth of L stacked (6*S + 5*GAP == L, up to a 2px rounding
/// error - see the comment on `SIZE_S.h` below).
pub const SIZE_S: Size = Size::new(832, 101);
/// Medium: half of L (2*M + GAP == L).
pub const SIZE_M: Size = Size::new(832, 336);
/// Large: fills a full column's height inside the page margins
/// (720 - 2*GAP == 688).
pub const SIZE_L: Size = Size::new(832, 688);
/// "Carre" - M cut in half vertically, with a GAP between the two halves
/// like everywhere else in the grid (2*SQ.w + GAP == M.w). Same height as M.
pub const SIZE_SQ: Size = Size::new(408, 336);
/// Same halving as SQ, applied to S instead of M (2*SX.w + GAP == S.w).
/// Same height as S.
pub const SIZE_SX: Size = Size::new(408, 101);
/// Same halving again, applied to SX (2*SSX.w + GAP == SX.w). Same height
/// as SX (and so S).
pub const SIZE_SSX: Size = Size::new(196, 101);

/// Fallback page width, used only before GTK has allocated the page's real
/// size. Three full-width columns tile the 2560px screen exactly:
/// `3*832 + 2*GAP (between columns) + 2*GAP (page margins, == GAP) == 2560`.
pub const PAGE_W: i32 = 3 * SIZE_S.w + 2 * GAP;
/// Fallback page height - a page is exactly one SIZE_L tall.
pub const PAGE_H: i32 = SIZE_L.h;

// Note on SIZE_S.h: `6*S + 5*GAP` should equal `SIZE_L.h` (688) for S to
// tile a column exactly, which gives S = 101.166..., not an integer. The
// Python original accepts 101 as-is (a 2px shortfall over 6 stacked S) as a
// known, deliberate rounding error rather than something to "fix" - doing
// so would mean revisiting every other derived constant too. Keep 101 here
// for the same reason: this is a shared invariant with the Python app's
// already-saved widget layouts, not a bug to correct independently.

/// An axis-aligned rectangle in page-local logical pixels: a widget's
/// current footprint (or a hypothetical one being tested for placement).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// Standard AABB overlap test - true if the two rectangles share any
    /// area at all (touching edges don't count as overlapping).
    pub fn overlaps(&self, other: &Rect) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }
}

/// Every valid left-edge x for a widget of the given `width` on a page of
/// `page_width`, tiling flush from 0 at `width + GAP` steps and stopping
/// once the next one would overflow the page. Always returns at least
/// `[0]`, even if the widget doesn't actually fit (the caller clamps
/// separately). Used both to snap a drag to a column and, with a
/// different step, to scan for free space.
pub fn column_positions(page_width: i32, width: i32) -> Vec<i32> {
    let step = width + GAP;
    let mut positions = vec![0];
    let mut x = step;
    while x + width <= page_width {
        positions.push(x);
        x += step;
    }
    positions
}

/// The candidate closest to `value` by absolute difference. Panics on an
/// empty slice - every call site here always seeds `candidates` with at
/// least `[0]`, so this should never actually be reached empty.
fn nearest(value: i32, candidates: &[i32]) -> i32 {
    *candidates
        .iter()
        .min_by_key(|&&c| (c - value).abs())
        .expect("candidates must be non-empty")
}

/// Snaps a dragged widget's raw (unsnapped) position onto the grid: the
/// nearest column for x, and the nearest "shelf" for y. Mirrors
/// `WidgetGrid._snap_to_layout` in grid.py.
///
/// `others` is every other widget currently on the same page (the one being
/// dragged must already be excluded by the caller - this function has no
/// notion of "self").
///
/// The y candidates ("shelves") are the union of two sets, which is what
/// lets a lone short widget in an empty column still snap to arbitrary
/// positions down the page rather than only "just below the one widget
/// already there":
/// - self-tiling shelves: `{0, step, 2*step, ...}` with `step = h + GAP`,
///   as if the column were filled with copies of this widget;
/// - neighbour-relative shelves: for every other widget whose x-range
///   overlaps this widget's snapped x, one extra shelf at
///   `other.y + other.h + GAP`.
pub fn snap_to_layout(
    others: &[Rect],
    size: Size,
    raw_x: i32,
    raw_y: i32,
    page_w: i32,
    page_h: i32,
) -> (i32, i32) {
    let max_x = (page_w - size.w).max(0);
    let max_y = (page_h - size.h).max(0);
    let clamped_x = raw_x.clamp(0, max_x);
    let clamped_y = raw_y.clamp(0, max_y);

    let x = nearest(clamped_x, &column_positions(page_w, size.w));

    let step = size.h + GAP;
    let mut shelves = vec![0];
    let mut y = step;
    while y <= max_y {
        shelves.push(y);
        y += step;
    }
    for other in others {
        let overlaps_x = x < other.x + other.w && other.x < x + size.w;
        if overlaps_x {
            let shelf = other.y + other.h + GAP;
            if shelf <= max_y {
                shelves.push(shelf);
            }
        }
    }

    let y = nearest(clamped_y, &shelves);
    (x, y)
}

/// Scans for the first free (x, y) spot that fits `size` on a page already
/// occupied by `occupied`, top-to-bottom then left-to-right - mirrors
/// `WidgetGrid.find_free_position`. Returns `None` if the page is full
/// (the caller then tries the next page, or creates a new one).
///
/// Narrow widgets (SQ width or less) scan at half-column pitch
/// (`SIZE_SQ.w + GAP`) so two of them can land side by side on the same
/// shelf; anything wider scans at full-column pitch (`SIZE_S.w + GAP`).
pub fn find_free_position(occupied: &[Rect], size: Size, page_w: i32, page_h: i32) -> Option<(i32, i32)> {
    let step_x = if size.w <= SIZE_SQ.w {
        SIZE_SQ.w + GAP
    } else {
        SIZE_S.w + GAP
    };

    let mut candidate_ys: Vec<i32> = std::iter::once(0)
        .chain(occupied.iter().map(|r| r.y + r.h + GAP))
        .collect();
    candidate_ys.sort_unstable();
    candidate_ys.dedup();

    for y in candidate_ys {
        if y + size.h > page_h {
            continue;
        }
        let mut x = 0;
        while x + size.w <= page_w {
            let candidate = Rect::new(x, y, size.w, size.h);
            if !occupied.iter().any(|r| r.overlaps(&candidate)) {
                return Some((x, y));
            }
            x += step_x;
        }
    }
    None
}

/// True if `candidate` doesn't overlap any rectangle in `others` - used to
/// validate a drag's landing spot before committing to it. Same "caller
/// excludes self" convention as `snap_to_layout`.
pub fn is_free(others: &[Rect], candidate: Rect) -> bool {
    !others.iter().any(|r| r.overlaps(&candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    // These derivations are load-bearing invariants documented in
    // CLAUDE.md - if any of these break, the grid no longer tiles the
    // 2560x720 screen exactly and every saved layout desyncs.
    #[test]
    fn size_presets_tile_the_screen_exactly() {
        assert_eq!(3 * SIZE_S.w + 4 * GAP, 2560); // 3 columns + 2 inter-gaps + 2 page margins
        assert_eq!(SIZE_L.h, 720 - 2 * GAP);
        assert_eq!(2 * SIZE_M.h + GAP, SIZE_L.h);
        assert_eq!(2 * SIZE_SQ.w + GAP, SIZE_M.w);
        assert_eq!(SIZE_SQ.h, SIZE_M.h);
        assert_eq!(2 * SIZE_SX.w + GAP, SIZE_S.w);
        assert_eq!(SIZE_SX.h, SIZE_S.h);
        assert_eq!(2 * SIZE_SSX.w + GAP, SIZE_SX.w);
        assert_eq!(SIZE_SSX.h, SIZE_SX.h);
        // 6*S + 5*GAP should equal SIZE_L.h (688) but doesn't quite, by
        // design - see the comment above SIZE_S. Pin the accepted 2px
        // shortfall explicitly so a "fix" doesn't slip in unnoticed.
        assert_eq!(6 * SIZE_S.h + 5 * GAP, SIZE_L.h - 2);
    }

    #[test]
    fn column_positions_tiles_three_full_width_columns() {
        assert_eq!(column_positions(PAGE_W, SIZE_S.w), vec![0, 832 + GAP, 2 * (832 + GAP)]);
    }

    #[test]
    fn column_positions_always_has_at_least_the_origin() {
        // A widget wider than the page still gets a single anchor at 0.
        assert_eq!(column_positions(100, 9999), vec![0]);
    }

    #[test]
    fn snap_to_layout_clamps_and_snaps_to_nearest_column() {
        let (x, y) = snap_to_layout(&[], SIZE_S, 900, 0, PAGE_W, PAGE_H);
        assert_eq!(x, 832 + GAP); // nearer the second column than the first
        assert_eq!(y, 0);
    }

    #[test]
    fn snap_to_layout_offers_a_shelf_below_an_overlapping_neighbour() {
        // A widget already sits at (0, 0, 832, 101). Dropping another S
        // roughly underneath it should offer a shelf right below it, not
        // only the self-tiling shelves.
        let neighbour = Rect::new(0, 0, SIZE_S.w, SIZE_S.h);
        let (x, y) = snap_to_layout(&[neighbour], SIZE_S, 0, SIZE_S.h + 5, PAGE_W, PAGE_H);
        assert_eq!(x, 0);
        assert_eq!(y, SIZE_S.h + GAP);
    }

    #[test]
    fn find_free_position_returns_first_gap_top_to_bottom_left_to_right() {
        let occupied = [Rect::new(0, 0, SIZE_M.w, SIZE_M.h)];
        let pos = find_free_position(&occupied, SIZE_M, PAGE_W, PAGE_H);
        assert_eq!(pos, Some((SIZE_M.w + GAP, 0)));
    }

    #[test]
    fn find_free_position_none_when_page_is_full() {
        // Fill every full-width column with an L widget - no room left.
        let occupied: Vec<Rect> = column_positions(PAGE_W, SIZE_L.w)
            .into_iter()
            .map(|x| Rect::new(x, 0, SIZE_L.w, SIZE_L.h))
            .collect();
        assert_eq!(find_free_position(&occupied, SIZE_L, PAGE_W, PAGE_H), None);
    }

    #[test]
    fn is_free_detects_overlap() {
        let others = [Rect::new(0, 0, SIZE_S.w, SIZE_S.h)];
        assert!(!is_free(&others, Rect::new(10, 10, SIZE_S.w, SIZE_S.h)));
        assert!(is_free(&others, Rect::new(SIZE_S.w + GAP, 0, SIZE_S.w, SIZE_S.h)));
    }
}
