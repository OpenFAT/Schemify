//! `ui_layout`: where one frame's [`crate::show`] put every shown node, recorded as egui laid
//! it out, so the host hit-tests, windows lists and fills the canvas and graph slots from the
//! same rectangles the user sees.
//!
//! Contents:
//! - `pub const NO_CLIP: Rect`, `pub const AREA_EM_MAX: f32`, `pub const FLOAT_INSET: f32`,
//!   `pub const SPLIT_GAP: f32`, `pub const SPLIT_MIN: f32`
//! - `pub struct Rect`: `contains`, `intersect`; crate-only `of`
//! - `pub struct Slot`
//! - `pub struct UiLayout`: `len`, `is_empty`, `node`, `rect`, `clip`, `slot`, `row_of`,
//!   `scroll_max`; crate-only `clear`, `push`, `set_scroll_max`

use crate::tables::NodeIx;

/// Clip of rows outside every `scroll` node: the whole plane.
pub const NO_CLIP: Rect = Rect {
    x: -1.0e9,
    y: -1.0e9,
    w: 2.0e9,
    h: 2.0e9,
};

/// Tallest text area, in font sizes: it grows with its lines up to this, then scrolls.
pub const AREA_EM_MAX: f32 = 24.0;

/// Gap between the canvas's top edge and a float (a child of the canvas node), in pixels.
pub const FLOAT_INSET: f32 = 8.0;

/// Narrowest divider of a `split` node (its gap), so the pointer can grab it.
pub const SPLIT_GAP: f32 = 4.0;

/// Smallest pane a dragged `split` divider leaves on either side.
pub const SPLIT_MIN: f32 = 24.0;

/// Axis-aligned rectangle in logical pixels.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

/// A reserved place in egui's shape list: the host paints a `canvas` or `graph` node into it
/// after the view ran, so the picture sits under the floats and popups painted after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    /// The layer the node was painted on.
    pub layer: egui::LayerId,
    /// The reserved shape (a no-op until the host sets it).
    pub shape: egui::layers::ShapeIdx,
}

/// One frame's shown nodes, in paint order. Valid until the next [`crate::show`].
#[derive(Clone, Debug, Default)]
pub struct UiLayout {
    node: Vec<NodeIx>,
    rect: Vec<Rect>,
    clip: Vec<Rect>,
    slot: Vec<Option<Slot>>,
    /// Each shown `scroll` node with its largest offset.
    scroll: Vec<(NodeIx, f32)>,
}

impl Rect {
    /// True when `(px, py)` is inside (left/top inclusive, right/bottom exclusive).
    #[must_use]
    pub fn contains(&self, px: f32, py: f32) -> bool {
        debug_assert!(self.w >= 0.0, "a rect's width is not negative");
        debug_assert!(self.h >= 0.0, "a rect's height is not negative");
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }

    /// The overlap of two rectangles (zero-sized when they do not meet).
    #[must_use]
    pub fn intersect(&self, o: &Rect) -> Rect {
        let (x0, y0) = (self.x.max(o.x), self.y.max(o.y));
        let x1 = (self.x + self.w).min(o.x + o.w);
        let y1 = (self.y + self.h).min(o.y + o.h);
        let out = Rect {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0.0),
            h: (y1 - y0).max(0.0),
        };
        debug_assert!(out.w <= self.w.max(o.w) && out.h <= self.h.max(o.h));
        out
    }

    pub(crate) fn of(r: egui::Rect) -> Rect {
        Rect {
            x: r.min.x,
            y: r.min.y,
            w: r.width().max(0.0),
            h: r.height().max(0.0),
        }
    }
}

impl UiLayout {
    /// Shown rows.
    #[must_use]
    pub fn len(&self) -> usize {
        debug_assert_eq!(self.rect.len(), self.node.len(), "columns in step");
        self.node.len()
    }

    /// True when nothing is shown (no window node, or a zero viewport).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        debug_assert_eq!(self.clip.len(), self.node.len(), "columns in step");
        self.node.is_empty()
    }

    /// The node of each row.
    #[must_use]
    pub fn node(&self) -> &[NodeIx] {
        &self.node
    }

    /// Each row's rectangle in logical px.
    #[must_use]
    pub fn rect(&self) -> &[Rect] {
        &self.rect
    }

    /// Each row's clip (its scroll ancestors' viewport; [`NO_CLIP`] outside any).
    #[must_use]
    pub fn clip(&self) -> &[Rect] {
        &self.clip
    }

    /// Each row's paint slot (`canvas` and `graph` rows only).
    #[must_use]
    pub fn slot(&self) -> &[Option<Slot>] {
        &self.slot
    }

    /// The first row showing node `n`.
    #[must_use]
    pub fn row_of(&self, n: NodeIx) -> Option<usize> {
        let r = self.node.iter().position(|&x| x == n);
        debug_assert!(r.is_none_or(|r| r < self.len()));
        r
    }

    /// Largest scroll offset of `scroll` node `n`, when it is shown.
    #[must_use]
    pub fn scroll_max(&self, n: NodeIx) -> Option<f32> {
        let v = self.scroll.iter().find(|s| s.0 == n).map(|s| s.1);
        debug_assert!(v.is_none_or(|v| v >= 0.0));
        v
    }

    pub(crate) fn clear(&mut self) {
        self.node.clear();
        self.rect.clear();
        self.clip.clear();
        self.slot.clear();
        self.scroll.clear();
        debug_assert!(self.is_empty());
    }

    pub(crate) fn push(&mut self, n: NodeIx, rect: Rect, clip: Rect, slot: Option<Slot>) {
        self.node.push(n);
        self.rect.push(rect);
        self.clip.push(clip);
        self.slot.push(slot);
        debug_assert_eq!(self.slot.len(), self.node.len(), "columns in step");
    }

    pub(crate) fn set_scroll_max(&mut self, n: NodeIx, max: f32) {
        debug_assert!(max >= 0.0, "a scroll range is not negative");
        self.scroll.push((n, max));
    }
}
