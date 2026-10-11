//! `ui_layout`: one frame's rectangles for every visible node, computed with taffy (flexbox).
//!
//! Rows are in paint order: the window tree in pre-order (layer [`Layer::Main`]), then each open
//! menu popup and the open dialog (layer [`Layer::Overlay`]). Renderers paint the main layer,
//! then the canvas, then the overlay, so popups always sit above the schematic.

use taffy::prelude::*;

use crate::input::UiState;
use crate::strings::{Strings, Sym};
use crate::tables::{Kind, NO_NODE, NO_STYLE, NodeIx, Prop, StyleProp, UiTables};
use crate::text::{NO_TEXT, TextId, TextSystem};

/// Clip of rows outside every `scroll` node: the whole plane.
pub const NO_CLIP: Rect = Rect {
    x: -1.0e9,
    y: -1.0e9,
    w: 2.0e9,
    h: 2.0e9,
};

/// Widest a `prose` node is laid out (the width is a `u16` cache key).
const PROSE_W_MAX: f32 = 8192.0;
/// Tallest text area, in font sizes (plus its padding): it grows with its lines up to this,
/// then scrolls inside itself ([`area_scroll_max`]).
pub const AREA_EM_MAX: f32 = 24.0;

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
    /// A menu's open popup panel (styled by selector `menu`; the node's own class styles its
    /// title).
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
    pub(crate) indent: Vec<f32>,
    pub(crate) clip: Vec<Rect>,
    pub(crate) scroller: Vec<NodeIx>,
    pub(crate) scroll_max: Vec<(NodeIx, f32)>,
    pub(crate) overlay_start: u32,
    pub(crate) title_style: u16,
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

    /// Left inset reserved for a check indicator in menu rows.
    pub fn indent(&self) -> &[f32] {
        &self.indent
    }

    /// Visible part of each row's area: the intersection of its `scroll` ancestors'
    /// rectangles, [`NO_CLIP`] outside any. Painters clip to it; hit testing ignores the rest.
    pub fn clip(&self) -> &[Rect] {
        &self.clip
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

    /// Style row of the `menu-title` selector ([`NO_STYLE`] if the theme has none).
    pub fn title_style(&self) -> u16 {
        self.title_style
    }

    /// First row showing `node` normally, as a title or as a cascade row (not as a popup).
    pub fn row_of(&self, node: NodeIx) -> Option<usize> {
        (0..self.len()).find(|&r| self.node[r] == node && self.shown[r] != Shown::Popup)
    }

    /// Topmost row under `(x, y)`. While a dialog is open only its rows can be hit (modal).
    pub fn hit(&self, x: f32, y: f32, modal: bool) -> Option<usize> {
        let lo = if modal {
            self.overlay_start as usize
        } else {
            0
        };
        (lo..self.len())
            .rev()
            .find(|&r| self.rect[r].contains(x, y) && self.clip[r].contains(x, y))
    }
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
    let pad_y = row_style(
        tables,
        layout.title_style,
        n,
        Shown::Normal,
        StyleProp::PadY,
    ) as f32;
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

/// Resolves a style prop for a row, following the [`Shown`] fallback chains.
pub fn row_style(
    tables: &UiTables,
    title_style: u16,
    node: NodeIx,
    shown: Shown,
    prop: StyleProp,
) -> u32 {
    let n = &tables.nodes;
    let i = node as usize;
    match shown {
        Shown::Normal => tables.styles.get(n.class[i], n.kind[i], prop),
        Shown::Title => tables.styles.get(
            if n.class[i] != NO_STYLE {
                n.class[i]
            } else {
                title_style
            },
            Kind::Item,
            prop,
        ),
        Shown::Cascade => tables.styles.get(n.class[i], Kind::Item, prop),
        Shown::Popup => tables.styles.get(NO_STYLE, Kind::Menu, prop),
    }
}

struct Pending {
    tnode: NodeId,
    node: NodeIx,
    shown: Shown,
    parent: u32,
    label: TextId,
    accel: TextId,
    math: u32,
    prose: u32,
    indent: f32,
}

/// Measure context of a `prose` leaf: its source and font size.
#[derive(Clone, Copy, Debug)]
struct ProseCtx {
    src: Sym,
    px: u16,
}

struct Builder<'a> {
    tables: &'a UiTables,
    strings: &'a Strings,
    text: &'a mut TextSystem,
    state: &'a UiState,
    title_style: u16,
    tree: TaffyTree<ProseCtx>,
    rows: Vec<Pending>,
}

const NO_ROW: u32 = u32::MAX;
const ARROW_W: f32 = 14.0;

impl Builder<'_> {
    fn style(&self, node: NodeIx, shown: Shown, p: StyleProp) -> f32 {
        row_style(self.tables, self.title_style, node, shown, p) as f32
    }

    fn text(&mut self, sym: Sym, px: f32) -> (TextId, f32, f32) {
        if sym.is_none() {
            return (NO_TEXT, 0.0, 0.0);
        }
        let id = self.text.shape(self.strings, sym, px.max(1.0) as u16);
        let l = self.text.layout(id);
        (id, l.width(), l.height())
    }

    /// Adds `node` (and, for containers, its subtree) and returns its taffy node.
    fn build(&mut self, node: NodeIx, shown: Shown, parent: u32, indent: f32) -> NodeId {
        let i = node as usize;
        let kind = self.tables.nodes.kind[i];
        let row = self.rows.len() as u32;
        self.rows.push(Pending {
            tnode: NodeId::from(0u64),
            node,
            shown,
            parent,
            label: NO_TEXT,
            accel: NO_TEXT,
            math: u32::MAX,
            prose: u32::MAX,
            indent,
        });
        let st = |b: &Self, p| b.style(node, shown, p);
        let (pad, pad_y, gap) = (
            st(self, StyleProp::Pad),
            st(self, StyleProp::PadY),
            st(self, StyleProp::Gap),
        );
        let (fixed_w, fixed_h, min_w) = (
            st(self, StyleProp::Width),
            st(self, StyleProp::Height),
            st(self, StyleProp::MinWidth),
        );
        let row_dir = st(self, StyleProp::Dir) != 0.0;
        let mut style = Style {
            flex_grow: st(self, StyleProp::Grow),
            flex_shrink: 0.0,
            ..Style::default()
        };
        let container = shown == Shown::Popup
            || (shown == Shown::Normal
                && matches!(
                    kind,
                    Kind::Window
                        | Kind::Menubar
                        | Kind::Toolbar
                        | Kind::Tabs
                        | Kind::Panel
                        | Kind::Split
                        | Kind::Scroll
                        | Kind::List
                        | Kind::Tree
                        | Kind::Dialog
                        | Kind::Status
                ));
        let tnode = if container {
            style.flex_direction = if row_dir {
                FlexDirection::Row
            } else {
                FlexDirection::Column
            };
            if row_dir {
                let stretch = st(self, StyleProp::Stretch) != 0.0;
                style.align_items = Some(if stretch {
                    AlignItems::STRETCH
                } else {
                    AlignItems::CENTER
                });
            }
            style.gap = Size {
                width: length(gap),
                height: length(gap),
            };
            style.padding = taffy::Rect {
                left: length(pad),
                right: length(pad),
                top: length(pad_y),
                bottom: length(pad_y),
            };
            style.size = Size {
                width: if fixed_w > 0.0 {
                    length(fixed_w)
                } else {
                    auto()
                },
                height: if fixed_h > 0.0 {
                    length(fixed_h)
                } else {
                    auto()
                },
            };
            style.min_size.width = length(min_w);
            if kind == Kind::Scroll && shown == Shown::Normal {
                // A viewport: its own size comes from its parent, never from its content.
                style.overflow = taffy::Point {
                    x: taffy::Overflow::Hidden,
                    y: taffy::Overflow::Hidden,
                };
                style.align_items = Some(AlignItems::STRETCH);
                // Zero basis: `grow` hands it the parent's free space, never its content's.
                style.flex_basis = length(0.0);
            }
            let child_indent = if shown == Shown::Popup {
                self.check_indent(node)
            } else {
                0.0
            };
            let kids: Vec<NodeIx> = self.tables.nodes.children(node).collect();
            let mut tkids = Vec::with_capacity(kids.len());
            for c in kids {
                let ck = self.tables.nodes.kind[c as usize];
                let cshown = match (ck, shown) {
                    (Kind::Menu, Shown::Popup) => Shown::Cascade,
                    (Kind::Menu, _) => Shown::Title,
                    _ => Shown::Normal,
                };
                if ck == Kind::Dialog {
                    continue; // dialogs are roots; one nested by mistake is not drawn inline
                }
                tkids.push(self.build(c, cshown, row, child_indent));
            }
            self.tree
                .new_with_children(style, &tkids)
                .expect("taffy node")
        } else {
            let font = st(self, StyleProp::Font).max(8.0);
            if kind == Kind::Prose {
                // Width comes from the parent (stretch); the height is measured at that width.
                style.padding = taffy::Rect {
                    left: length(pad),
                    right: length(pad),
                    top: length(pad_y),
                    bottom: length(pad_y),
                };
                style.min_size.width = length(min_w);
                let ctx = ProseCtx {
                    src: self.tables.nodes.prop(Prop::Value)[i],
                    px: font as u16,
                };
                let tnode = self
                    .tree
                    .new_leaf_with_context(style, ctx)
                    .expect("taffy leaf");
                self.rows[row as usize].tnode = tnode;
                return tnode;
            }
            // A text field shows its value in the label slot.
            let label_prop = if matches!(kind, Kind::TextInput | Kind::TextArea) {
                Prop::Value
            } else {
                Prop::Label
            };
            let label_sym = self.tables.nodes.prop(label_prop)[i];
            let (label, lw, lh) = self.text(label_sym, font);
            let (mut w, mut h) = (lw + 2.0 * pad, lh + 2.0 * pad_y);
            match (kind, shown) {
                (_, Shown::Cascade) => w += indent + gap + ARROW_W,
                (Kind::Item, _) => {
                    let (accel, aw, _) = self.text(self.tables.nodes.prop(Prop::Accel)[i], font);
                    self.rows[row as usize].accel = accel;
                    w += indent + if aw > 0.0 { gap + aw } else { 0.0 };
                }
                (Kind::Separator, _) => (w, h) = (0.0, 0.0),
                (Kind::Checkbox, _) => w += lh + gap,
                (Kind::Dropdown, _) => w += gap + ARROW_W,
                (Kind::TextInput, _) => h = h.max(font * 1.4 + 2.0 * pad_y),
                (Kind::TextArea, _) => {
                    // While focused the editor may hold more lines than the value.
                    let editing = self.state.focus == self.tables.nodes.id[i];
                    let eh = self
                        .state
                        .editor
                        .as_ref()
                        .filter(|_| editing)
                        .and_then(|e| e.try_layout())
                        .map_or(0.0, |l| l.height());
                    h = (lh.max(eh) + font * 1.4 + 2.0 * pad_y)
                        .max(font * 4.0)
                        .min(font * AREA_EM_MAX + 2.0 * pad_y);
                    // Width comes from the parent (stretch), never from the longest line.
                    w = 2.0 * pad + font;
                }
                (Kind::Math, _) => {
                    let src = self.tables.nodes.prop(Prop::Value)[i];
                    if let Ok(m) = self.text.math(self.strings, src, font as u16) {
                        let list = self.text.math_list(m);
                        (w, h) = (
                            list.width + 2.0 * pad,
                            list.height + list.depth + 2.0 * pad_y,
                        );
                        self.rows[row as usize].math = m;
                    }
                }
                _ => {}
            }
            self.rows[row as usize].label = label;
            if fixed_w > 0.0 {
                w = fixed_w;
            }
            if fixed_h > 0.0 {
                h = fixed_h;
            }
            style.min_size = Size {
                width: length(w.max(min_w)),
                height: length(h),
            };
            // A canvas fills its parent on both axes (it may sit in a row beside the docs
            // page), so only a styled height pins it.
            if kind != Kind::Canvas || fixed_h > 0.0 {
                style.size.height = length(h);
            }
            self.tree.new_leaf(style).expect("taffy leaf")
        };
        self.rows[row as usize].tnode = tnode;
        tnode
    }

    /// Indicator inset for a popup: reserved for every row when any row is checkable, so labels
    /// stay aligned (Tk does the same).
    fn check_indent(&self, menu: NodeIx) -> f32 {
        let n = &self.tables.nodes;
        let any = n
            .children(menu)
            .any(|c| !n.prop(Prop::Checked)[c as usize].is_none());
        if any { 18.0 } else { 0.0 }
    }

    /// Lays out the root built at `start` and writes absolute rects for its rows into `out`:
    /// children of a `scroll` node shift up by its clamped offset and are clipped to it.
    fn place(
        &mut self,
        start: usize,
        root: NodeId,
        avail: Size<AvailableSpace>,
        at: (f32, f32),
        out: &mut Frame,
    ) {
        let (tree, text, strings) = (&mut self.tree, &mut *self.text, self.strings);
        tree.compute_layout_with_measure(root, avail, |inputs, _, ctx, style| {
            let ctx = ctx.copied();
            taffy::compute_leaf_layout(
                inputs,
                style,
                |_, _| 0.0,
                |known, space| {
                    let Some(c) = ctx.filter(|c| !c.src.is_none() && c.px > 0) else {
                        return Size::ZERO;
                    };
                    let w = known.width.unwrap_or(match space.width {
                        AvailableSpace::Definite(w) => w,
                        AvailableSpace::MaxContent => PROSE_W_MAX,
                        AvailableSpace::MinContent => 160.0,
                    });
                    let w = w.clamp(1.0, PROSE_W_MAX);
                    let id = text.prose(strings, c.src, c.px, w as u16);
                    let list = text.prose_list(id);
                    Size {
                        width: known.width.unwrap_or(list.width.ceil()),
                        height: list.height,
                    }
                },
            )
        })
        .expect("taffy layout");
        // Scroll extents first: a child's position depends on its scroll parent's offset.
        let rel: Vec<taffy::Layout> = (start..self.rows.len())
            .map(|r| *self.tree.layout(self.rows[r].tnode).expect("laid-out node"))
            .collect();
        let is_scroll = |b: &Self, r: usize| {
            b.rows[r].shown == Shown::Normal
                && b.tables.nodes.kind[b.rows[r].node as usize] == Kind::Scroll
        };
        let mut bottom: Vec<(usize, f32)> = Vec::new();
        for r in start..self.rows.len() {
            let p = self.rows[r].parent;
            if p != NO_ROW && (p as usize) >= start && is_scroll(self, p as usize) {
                let l = &rel[r - start];
                let end = l.location.y + l.size.height;
                match bottom.iter_mut().find(|(s, _)| *s == p as usize) {
                    Some(b) => b.1 = b.1.max(end),
                    None => bottom.push((p as usize, end)),
                }
            }
        }
        for r in start..self.rows.len() {
            let l = &rel[r - start];
            let p = self.rows[r].parent;
            let inside = p != NO_ROW && (p as usize) >= start;
            let (ox, oy, clip, scroller) = if inside {
                let p = p as usize;
                let pr = out.rect[p];
                if is_scroll(self, p) {
                    let node = self.rows[p].node;
                    let content = bottom.iter().find(|(s, _)| *s == p).map_or(0.0, |b| b.1);
                    let pad = self.style(node, Shown::Normal, StyleProp::PadY);
                    let max = (content + pad - pr.h).max(0.0);
                    let off = self
                        .state
                        .scroll_of(self.tables.nodes.id[node as usize])
                        .clamp(0.0, max);
                    if !out.scroll_max.iter().any(|(n, _)| *n == node) {
                        out.scroll_max.push((node, max));
                    }
                    (pr.x, pr.y - off, out.clip[p].intersect(&pr), node)
                } else {
                    (pr.x, pr.y, out.clip[p], out.scroller[p])
                }
            } else {
                (at.0, at.1, NO_CLIP, NO_NODE)
            };
            out.rect.push(Rect {
                x: ox + l.location.x,
                y: oy + l.location.y,
                w: l.size.width,
                h: l.size.height,
            });
            out.clip.push(clip);
            out.scroller.push(scroller);
        }
        // Prose rows: the layout at the final content width (cached from measuring).
        for r in start..self.rows.len() {
            let node = self.rows[r].node;
            let i = node as usize;
            if self.tables.nodes.kind[i] != Kind::Prose {
                continue;
            }
            let src = self.tables.nodes.prop(Prop::Value)[i];
            let pad = self.style(node, Shown::Normal, StyleProp::Pad);
            let font = self.style(node, Shown::Normal, StyleProp::Font).max(8.0);
            let w = (out.rect[r].w - 2.0 * pad).clamp(1.0, PROSE_W_MAX);
            if !src.is_none() {
                self.rows[r].prose = self.text.prose(self.strings, src, font as u16, w as u16);
            }
        }
        debug_assert_eq!(out.rect.len(), self.rows.len());
        debug_assert_eq!(out.clip.len(), out.rect.len());
    }
}

/// Rectangles, clips and scroll extents accumulated across [`Builder::place`] calls.
#[derive(Default)]
struct Frame {
    rect: Vec<Rect>,
    clip: Vec<Rect>,
    scroller: Vec<NodeIx>,
    scroll_max: Vec<(NodeIx, f32)>,
}

/// Lays out the window tree into a `viewport` (logical px), then every open menu popup and the
/// open dialog. Shapes and caches every visible string in `text`.
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
    let window =
        (0..nodes.len()).find(|&i| nodes.parent[i] == NO_NODE && nodes.kind[i] == Kind::Window);
    if let Some(w) = window {
        let root = b.build(w as NodeIx, Shown::Normal, NO_ROW, 0.0);
        let avail = Size {
            width: AvailableSpace::Definite(viewport.0),
            height: AvailableSpace::Definite(viewport.1),
        };
        // Pin the root to the viewport so `grow` children (the canvas) fill it.
        let mut s = b.tree.style(root).expect("root style").clone();
        s.size = Size {
            width: length(viewport.0),
            height: length(viewport.1),
        };
        b.tree.set_style(root, s).expect("root style");
        b.place(0, root, avail, (0.0, 0.0), &mut f);
    }
    let overlay_start = b.rows.len() as u32;
    let max_content = Size {
        width: AvailableSpace::MaxContent,
        height: AvailableSpace::MaxContent,
    };
    // Popups: each anchored to the row that opened it (bounded by the open chain length).
    for &menu in &state.open {
        let Some(m) = nodes.find(menu) else { break };
        let anchor = (0..b.rows.len())
            .rev()
            .find(|&r| b.rows[r].node == m && b.rows[r].shown != Shown::Popup);
        let Some(a) = anchor else { break };
        let ar = f.rect[a];
        let below = b.rows[a].shown == Shown::Title;
        let start = b.rows.len();
        let root = b.build(m, Shown::Popup, NO_ROW, 0.0);
        b.place(start, root, max_content, (0.0, 0.0), &mut f);
        let (pw, ph) = (f.rect[start].w, f.rect[start].h);
        let mut x = if below { ar.x } else { ar.x + ar.w };
        let mut y = if below { ar.y + ar.h } else { ar.y };
        x = x.min(viewport.0 - pw).max(0.0);
        y = y.min(viewport.1 - ph).max(0.0);
        f.shift(start, x, y);
    }
    if let Some(d) = nodes
        .find(state.dialog)
        .filter(|&d| nodes.kind[d as usize] == Kind::Dialog)
    {
        let start = b.rows.len();
        let root = b.build(d, Shown::Normal, NO_ROW, 0.0);
        b.place(start, root, max_content, (0.0, 0.0), &mut f);
        let (w, h) = (f.rect[start].w, f.rect[start].h);
        let (dx, dy) = match state.dialog_at {
            // At a point (a context menu): kept inside the viewport.
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
    let out = UiLayout {
        node: b.rows.iter().map(|r| r.node).collect(),
        shown: b.rows.iter().map(|r| r.shown).collect(),
        rect: f.rect,
        label: b.rows.iter().map(|r| r.label).collect(),
        accel: b.rows.iter().map(|r| r.accel).collect(),
        math: b.rows.iter().map(|r| r.math).collect(),
        prose: b.rows.iter().map(|r| r.prose).collect(),
        indent: b.rows.iter().map(|r| r.indent).collect(),
        clip: f.clip,
        scroller: f.scroller,
        scroll_max: f.scroll_max,
        overlay_start,
        title_style,
    };
    debug_assert_eq!(out.rect.len(), out.node.len());
    debug_assert_eq!(out.clip.len(), out.node.len());
    out
}

impl Frame {
    /// Moves rows `start..` (a popup or dialog placed at the origin) by `(dx, dy)`.
    fn shift(&mut self, start: usize, dx: f32, dy: f32) {
        for r in start..self.rect.len() {
            self.rect[r].x += dx;
            self.rect[r].y += dy;
            if self.clip[r] != NO_CLIP {
                self.clip[r].x += dx;
                self.clip[r].y += dy;
            }
        }
    }
}
