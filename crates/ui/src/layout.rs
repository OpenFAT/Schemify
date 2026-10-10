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
    pub(crate) indent: Vec<f32>,
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

    /// Left inset reserved for a check indicator in menu rows.
    pub fn indent(&self) -> &[f32] {
        &self.indent
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
            .find(|&r| self.rect[r].contains(x, y))
    }
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
    indent: f32,
}

struct Builder<'a> {
    tables: &'a UiTables,
    strings: &'a Strings,
    text: &'a mut TextSystem,
    title_style: u16,
    tree: TaffyTree<()>,
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
                style.align_items = Some(AlignItems::CENTER);
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
            // A text field shows its value in the label slot.
            let label_prop = if kind == Kind::TextInput {
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
            style.size.height = length(h);
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

    /// Lays out the root built at `start` and writes absolute rects for its rows into `out`.
    fn place(
        &mut self,
        start: usize,
        root: NodeId,
        avail: Size<AvailableSpace>,
        at: (f32, f32),
        out: &mut Vec<Rect>,
    ) {
        self.tree.compute_layout(root, avail).expect("taffy layout");
        for r in start..self.rows.len() {
            let l = self.tree.layout(self.rows[r].tnode).expect("laid-out node");
            let p = self.rows[r].parent;
            let (ox, oy) = if p == NO_ROW || (p as usize) < start {
                at
            } else {
                (out[p as usize].x, out[p as usize].y)
            };
            out.push(Rect {
                x: ox + l.location.x,
                y: oy + l.location.y,
                w: l.size.width,
                h: l.size.height,
            });
        }
        debug_assert_eq!(out.len(), self.rows.len());
    }
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
        title_style,
        tree: TaffyTree::new(),
        rows: Vec::new(),
    };
    let mut rects = Vec::new();
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
        b.place(0, root, avail, (0.0, 0.0), &mut rects);
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
        let ar = rects[a];
        let below = b.rows[a].shown == Shown::Title;
        let start = b.rows.len();
        let root = b.build(m, Shown::Popup, NO_ROW, 0.0);
        b.place(start, root, max_content, (0.0, 0.0), &mut rects);
        let (pw, ph) = (rects[start].w, rects[start].h);
        let mut x = if below { ar.x } else { ar.x + ar.w };
        let mut y = if below { ar.y + ar.h } else { ar.y };
        x = x.min(viewport.0 - pw).max(0.0);
        y = y.min(viewport.1 - ph).max(0.0);
        for r in &mut rects[start..] {
            r.x += x;
            r.y += y;
        }
    }
    if let Some(d) = nodes
        .find(state.dialog)
        .filter(|&d| nodes.kind[d as usize] == Kind::Dialog)
    {
        let start = b.rows.len();
        let root = b.build(d, Shown::Normal, NO_ROW, 0.0);
        b.place(start, root, max_content, (0.0, 0.0), &mut rects);
        let (dx, dy) = (
            ((viewport.0 - rects[start].w) / 2.0).max(0.0),
            ((viewport.1 - rects[start].h) / 3.0).max(0.0),
        );
        for r in &mut rects[start..] {
            r.x += dx;
            r.y += dy;
        }
    }
    let out = UiLayout {
        node: b.rows.iter().map(|r| r.node).collect(),
        shown: b.rows.iter().map(|r| r.shown).collect(),
        rect: rects,
        label: b.rows.iter().map(|r| r.label).collect(),
        accel: b.rows.iter().map(|r| r.accel).collect(),
        math: b.rows.iter().map(|r| r.math).collect(),
        indent: b.rows.iter().map(|r| r.indent).collect(),
        overlay_start,
        title_style,
    };
    debug_assert_eq!(out.rect.len(), out.node.len());
    out
}
