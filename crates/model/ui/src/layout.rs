//! `ui_layout`: the composed tables, the interaction state and a viewport in, one frame's
//! rectangles for every visible node out ([`UiLayout`], computed with taffy in [`crate::flex`]).
//!
//! Rows are in paint order: the window tree in pre-order (layer [`Layer::Main`]), then the
//! canvas's floats (children of the `canvas` node, centred along its top edge), the open dialog
//! and each open popup, menus and dropdowns (layer [`Layer::Overlay`]). Renderers paint the main
//! layer, then the canvas, then the overlay, so floats and popups sit above the schematic and a
//! popup above the dialog that holds its dropdown. A modal dialog leaves floats unhittable.
//!
//! Contents:
//! - `pub const NO_CLIP: Rect`, `pub const AREA_EM_MAX: f32`, `pub const FLOAT_INSET: f32`,
//!   `pub const SPLIT_GAP: f32`, `pub const SPLIT_MIN: f32`; crate-only `PROSE_W_MAX`
//! - `pub struct Rect`: `contains`, `intersect`
//! - `pub enum Shown`, `pub enum Layer`
//! - `pub struct UiLayout`: `len`, `is_empty`, `node`, `shown`, `rect`, `label`, `accel`, `math`,
//!   `prose`, `icon`, `indent`, `clip`, `scrolled`, `scroll_max`, `scroller`, `rows`, `floats`,
//!   `title_style`, `row_of`, `hit`; crate-only `style`
//! - `pub fn layout(tables, strings, text, state, viewport) -> UiLayout`
//! - `pub fn area_scroll_max(tables, layout, text, state, r) -> f32`

use taffy::prelude::*;

use crate::flex::{Builder, Frame, NO_ROW};
use crate::kinds::{Kind, StyleProp};
use crate::paint::row_style;
use crate::state::UiState;
use crate::strings::Strings;
use crate::tables::{NO_NODE, NO_STYLE, NodeIx, UiTables};
use crate::text::{NO_TEXT, TextId, TextSystem};

/// Clip of rows outside every `scroll` node: the whole plane.
pub const NO_CLIP: Rect = Rect {
    x: -1.0e9,
    y: -1.0e9,
    w: 2.0e9,
    h: 2.0e9,
};

/// Tallest text area, in font sizes (plus its padding): it grows with its lines up to this,
/// then scrolls inside itself ([`area_scroll_max`]).
pub const AREA_EM_MAX: f32 = 24.0;

/// Gap between the canvas's top edge and a float (a child of the canvas node), in pixels.
pub const FLOAT_INSET: f32 = 8.0;

/// Narrowest divider of a `split` node (its gap), so the pointer can grab it.
pub const SPLIT_GAP: f32 = 4.0;

/// Smallest pane a dragged `split` divider leaves on either side.
pub const SPLIT_MIN: f32 = 24.0;

/// Widest a `prose` node is laid out (the width is a `u16` cache key).
pub(crate) const PROSE_W_MAX: f32 = 8192.0;

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

/// How a node is shown in a layout row. A menu appears as a title or cascade row *and*, when
/// open, as a popup panel; the other kinds always show as themselves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shown {
    /// The node's own kind.
    Normal,
    /// A menu's title in a menubar (styled by selector `menu-title`, then `item`).
    Title,
    /// A menu as a row inside another menu's popup (styled like `item`, with an arrow).
    Cascade,
    /// A menu's or dropdown's open popup panel (styled by selector `menu`; the node's own class
    /// styles its title).
    Popup,
}

/// Paint layer of a row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    /// The window tree (bars, canvas frame, status).
    Main,
    /// Menu popups and the dialog, painted after the canvas.
    Overlay,
}

/// One frame's layout. Valid until the tables, the [`UiState`], the viewport or the
/// [`TextSystem`] cache change.
#[derive(Clone, Debug, Default)]
pub struct UiLayout {
    pub(crate) node: Vec<NodeIx>,
    pub(crate) shown: Vec<Shown>,
    pub(crate) rect: Vec<Rect>,
    pub(crate) label: Vec<TextId>,
    pub(crate) accel: Vec<TextId>,
    pub(crate) math: Vec<u32>,
    pub(crate) prose: Vec<u32>,
    pub(crate) icon: Vec<u32>,
    pub(crate) indent: Vec<f32>,
    pub(crate) clip: Vec<Rect>,
    pub(crate) scroller: Vec<NodeIx>,
    pub(crate) scrolled: Vec<f32>,
    pub(crate) scroll_max: Vec<(NodeIx, f32)>,
    pub(crate) overlay_start: u32,
    /// First row a modal dialog leaves hittable: the dialog's own (floats sit below it).
    pub(crate) modal_start: u32,
    pub(crate) title_style: u16,
}

impl Rect {
    /// True when `(px, py)` is inside (left/top inclusive, right/bottom exclusive).
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }

    /// The overlap of two rectangles (zero-sized when they do not meet).
    pub fn intersect(&self, o: &Rect) -> Rect {
        let (x0, y0) = (self.x.max(o.x), self.y.max(o.y));
        let (x1, y1) = (
            (self.x + self.w).min(o.x + o.w),
            (self.y + self.h).min(o.y + o.h),
        );
        Rect {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0.0),
            h: (y1 - y0).max(0.0),
        }
    }
}

impl UiLayout {
    /// Number of rows.
    pub fn len(&self) -> usize {
        self.node.len()
    }

    /// True when nothing is visible.
    pub fn is_empty(&self) -> bool {
        self.node.is_empty()
    }

    /// Node of each row.
    pub fn node(&self) -> &[NodeIx] {
        &self.node
    }

    /// How each row shows its node.
    pub fn shown(&self) -> &[Shown] {
        &self.shown
    }

    /// Rectangle of each row.
    pub fn rect(&self) -> &[Rect] {
        &self.rect
    }

    /// Label layout of each row ([`NO_TEXT`] when none).
    pub fn label(&self) -> &[TextId] {
        &self.label
    }

    /// Accelerator layout of each row ([`NO_TEXT`] when none).
    pub fn accel(&self) -> &[TextId] {
        &self.accel
    }

    /// Math layout id of each row (`u32::MAX` when none).
    pub fn math(&self) -> &[u32] {
        &self.math
    }

    /// Prose layout id ([`TextSystem::prose_list`]) of each row (`u32::MAX` when none).
    pub fn prose(&self) -> &[u32] {
        &self.prose
    }

    /// Icon id ([`TextSystem::icon_paths`]) of each row (`u32::MAX` when none): a button
    /// with a parsable `icon` shows it instead of its label.
    pub fn icon(&self) -> &[u32] {
        &self.icon
    }

    /// Left inset reserved for a check indicator in menu rows.
    pub fn indent(&self) -> &[f32] {
        &self.indent
    }

    /// Visible part of each row's area: the intersection of its `scroll` ancestors'
    /// rectangles, [`NO_CLIP`] outside any. Painters clip to it; hit testing ignores the rest.
    pub fn clip(&self) -> &[Rect] {
        &self.clip
    }

    /// How far each row is scrolled up: the sum of its `scroll` ancestors' offsets (logical px,
    /// 0 outside any). The row's place in its panes' content is its rect moved down by it, so a
    /// painter can keep a scrolled row and move it by its pane's offset alone.
    pub fn scrolled(&self) -> &[f32] {
        &self.scrolled
    }

    /// Largest scroll offset of a laid-out `scroll` node (content height minus view height).
    pub fn scroll_max(&self, node: NodeIx) -> Option<f32> {
        self.scroll_max
            .iter()
            .find(|(n, _)| *n == node)
            .map(|(_, m)| *m)
    }

    /// Innermost `scroll` node enclosing row `r` ([`NO_NODE`] when none).
    pub fn scroller(&self, r: usize) -> NodeIx {
        self.scroller[r]
    }

    /// Rows of one paint layer.
    pub fn rows(&self, layer: Layer) -> std::ops::Range<usize> {
        match layer {
            Layer::Main => 0..self.overlay_start as usize,
            Layer::Overlay => self.overlay_start as usize..self.len(),
        }
    }

    /// Rows of the canvas's floats: the start of [`Layer::Overlay`], before the dialog and
    /// popups (empty when the canvas has no children).
    pub fn floats(&self) -> std::ops::Range<usize> {
        debug_assert!(self.overlay_start <= self.modal_start);
        self.overlay_start as usize..self.modal_start as usize
    }

    /// Style row of the `menu-title` selector ([`NO_STYLE`] if the theme has none).
    pub fn title_style(&self) -> u16 {
        self.title_style
    }

    /// [`row_style`] of `node` shown normally, as a float (what input and painting measure with).
    pub(crate) fn style(&self, tables: &UiTables, node: NodeIx, prop: StyleProp) -> f32 {
        row_style(tables, self.title_style, node, Shown::Normal, prop) as f32
    }

    /// First row showing `node` normally, as a title or as a cascade row (not as a popup).
    pub fn row_of(&self, node: NodeIx) -> Option<usize> {
        (0..self.len()).find(|&r| self.node[r] == node && self.shown[r] != Shown::Popup)
    }

    /// Topmost row under `(x, y)`. While a dialog is open only its rows can be hit (modal).
    pub fn hit(&self, x: f32, y: f32, modal: bool) -> Option<usize> {
        let lo = if modal { self.modal_start as usize } else { 0 };
        (lo..self.len())
            .rev()
            .find(|&r| self.rect[r].contains(x, y) && self.clip[r].contains(x, y))
    }
}

/// Lays out the window tree into a `viewport` (logical px), then the canvas's floats, the open
/// dialog and every open popup. Shapes and caches every visible string in `text`.
///
/// # Panics
///
/// Panics if `viewport` is not finite and positive.
pub fn layout(
    tables: &UiTables,
    strings: &Strings,
    text: &mut TextSystem,
    state: &UiState,
    viewport: (f32, f32),
) -> UiLayout {
    assert!(
        viewport.0 > 0.0 && viewport.1 > 0.0 && viewport.0.is_finite() && viewport.1.is_finite(),
        "bad viewport"
    );
    text.begin_frame();
    let title_style = strings
        .find("menu-title")
        .map_or(NO_STYLE, |s| tables.styles.row(s));
    let mut b = Builder {
        tables,
        strings,
        text,
        state,
        title_style,
        tree: TaffyTree::new(),
        rows: Vec::new(),
    };
    let mut f = Frame::default();
    let nodes = &tables.nodes;
    // The window, pinned to the viewport so `grow` children (the canvas) fill it.
    let window =
        (0..nodes.len()).find(|&i| nodes.parent[i] == NO_NODE && nodes.kind[i] == Kind::Window);
    if let Some(w) = window {
        let root = b.build(w as NodeIx, Shown::Normal, NO_ROW, 0.0);
        let avail = Size {
            width: AvailableSpace::Definite(viewport.0),
            height: AvailableSpace::Definite(viewport.1),
        };
        let mut s = b.tree.style(root).expect("root style").clone();
        s.size = Size {
            width: length(viewport.0),
            height: length(viewport.1),
        };
        b.tree.set_style(root, s).expect("root style");
        b.place(0, root, avail, (0.0, 0.0), &mut f);
    }
    let overlay_start = b.rows.len() as u32;
    // Floats: each child of the canvas, laid out at its content size and centred along the
    // canvas's top edge, so it paints above the schematic but under any dialog or popup.
    // ponytail: every float takes the same spot; add an anchor style prop for a second one.
    let canvas_row =
        (0..overlay_start as usize).find(|&r| nodes.kind[b.rows[r].node as usize] == Kind::Canvas);
    if let Some(cr) = canvas_row {
        let c = f.rect[cr];
        let kids: Vec<NodeIx> = nodes.children(b.rows[cr].node).collect();
        for k in kids {
            let (start, w, _) = b.float(k, Shown::Normal, &mut f);
            f.shift(start, c.x + ((c.w - w) / 2.0).max(0.0), c.y + FLOAT_INSET);
        }
    }
    let modal_start = b.rows.len() as u32;
    debug_assert!(modal_start >= overlay_start);
    // The dialog first: a dropdown inside it anchors its popup to a laid-out row, and the
    // popup paints (and hits) above it.
    if let Some(d) = nodes
        .find(state.dialog)
        .filter(|&d| nodes.kind[d as usize] == Kind::Dialog)
    {
        let (start, w, h) = b.float(d, Shown::Normal, &mut f);
        // At a point (a context menu) or where the user dragged it: kept inside the viewport.
        let (dx, dy) = match state.dialog_at.or(state.dialog_pos) {
            Some((x, y)) => (
                x.min(viewport.0 - w).max(0.0),
                y.min(viewport.1 - h).max(0.0),
            ),
            None => (
                ((viewport.0 - w) / 2.0).max(0.0),
                ((viewport.1 - h) / 3.0).max(0.0),
            ),
        };
        f.shift(start, dx, dy);
    }
    // Popups: each anchored to the row that opened it (bounded by the open chain length).
    for &menu in &state.open {
        let Some(m) = nodes.find(menu) else { break };
        let anchor = (0..b.rows.len())
            .rev()
            .find(|&r| b.rows[r].node == m && b.rows[r].shown != Shown::Popup);
        let Some(a) = anchor else { break };
        let ar = f.rect[a];
        // Menubar titles and dropdowns drop down; cascades open to the right.
        let below = b.rows[a].shown == Shown::Title
            || (b.rows[a].shown == Shown::Normal && nodes.kind[m as usize] == Kind::Dropdown);
        let (start, pw, ph) = b.float(m, Shown::Popup, &mut f);
        let mut x = if below { ar.x } else { ar.x + ar.w };
        let mut y = if below { ar.y + ar.h } else { ar.y };
        x = x.min(viewport.0 - pw).max(0.0);
        y = y.min(viewport.1 - ph).max(0.0);
        f.shift(start, x, y);
    }
    let out = UiLayout {
        node: b.rows.iter().map(|r| r.node).collect(),
        shown: b.rows.iter().map(|r| r.shown).collect(),
        rect: f.rect,
        label: b.rows.iter().map(|r| r.label).collect(),
        accel: b.rows.iter().map(|r| r.accel).collect(),
        math: b.rows.iter().map(|r| r.math).collect(),
        prose: b.rows.iter().map(|r| r.prose).collect(),
        icon: b.rows.iter().map(|r| r.icon).collect(),
        indent: b.rows.iter().map(|r| r.indent).collect(),
        clip: f.clip,
        scroller: f.scroller,
        scrolled: f.scrolled,
        scroll_max: f.scroll_max,
        overlay_start,
        modal_start,
        title_style,
    };
    debug_assert_eq!(out.rect.len(), out.node.len());
    debug_assert_eq!(out.clip.len(), out.node.len());
    debug_assert_eq!(out.scrolled.len(), out.node.len());
    out
}

/// How far text area row `r` scrolls inside itself: the height of its text (the editor's
/// while it has focus) minus the height inside its vertical padding; 0 when it fits.
pub fn area_scroll_max(
    tables: &UiTables,
    layout: &UiLayout,
    text: &TextSystem,
    state: &UiState,
    r: usize,
) -> f32 {
    let n = layout.node[r];
    debug_assert_eq!(tables.nodes.kind[n as usize], Kind::TextArea);
    let pad_y = layout.style(tables, n, StyleProp::PadY);
    let content = if state.focus == tables.nodes.id[n as usize] {
        let ed = state.editor.as_ref().and_then(|e| e.try_layout());
        ed.map_or(0.0, |l| l.height())
    } else if layout.label[r] != NO_TEXT {
        text.layout(layout.label[r]).height()
    } else {
        0.0
    };
    (content - (layout.rect[r].h - 2.0 * pad_y)).max(0.0)
}
