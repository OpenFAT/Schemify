//! The taffy (flexbox) tree of [`crate::layout`]: one subtree of the UI tables in, one row
//! per shown node and its taffy node out, each leaf measured (labels, icons, accelerators,
//! math, prose). [`crate::place`] lays the tree out. A row is one node as shown ([`Shown`]):
//! a menu is a title or cascade row and, when open, a popup root of its own.
//!
//! Contents:
//! - `pub(crate) const NO_ROW: u32`
//! - `pub(crate) struct Pending`, `pub(crate) struct ProseContext`
//! - `pub(crate) struct Builder`: `build`, `style`

use taffy::{
    AlignItems, Dimension, FlexDirection, LengthPercentage, LengthPercentageAuto, NodeId, Overflow,
    Point, Size, Style, TaffyTree,
};

use crate::kinds::{Kind, Prop, StyleProp};
use crate::layout::{AREA_EM_MAX, SPLIT_GAP, Shown};
use crate::paint;
use crate::state::UiState;
use crate::strings::{Strings, Sym};
use crate::tables::{NodeIx, UiTables};
use crate::text::{NO_TEXT, TextId, TextSystem};

/// Parent of a root row.
pub(crate) const NO_ROW: u32 = u32::MAX;

/// One row while its frame is built; the columns of [`crate::UiLayout`] are read from it.
pub(crate) struct Pending {
    pub(crate) taffy_node: NodeId,
    pub(crate) node: NodeIx,
    pub(crate) shown: Shown,
    /// Row of the parent ([`NO_ROW`] for a root).
    pub(crate) parent: u32,
    pub(crate) label: TextId,
    pub(crate) accel: TextId,
    pub(crate) math: u32,
    pub(crate) prose: u32,
    pub(crate) icon: u32,
    pub(crate) indent: f32,
}

/// The taffy tree of one frame and the rows built into it.
pub(crate) struct Builder<'frame> {
    pub(crate) tables: &'frame UiTables,
    pub(crate) strings: &'frame Strings,
    pub(crate) text: &'frame mut TextSystem,
    pub(crate) state: &'frame UiState,
    pub(crate) title_style: u16,
    pub(crate) tree: TaffyTree<ProseContext>,
    pub(crate) rows: Vec<Pending>,
}

/// Measure context of a `prose` leaf: its source and font size.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProseContext {
    pub(crate) src: Sym,
    pub(crate) px: u16,
}

/// The style lengths every row reads, resolved once.
struct Metrics {
    pad: f32,
    pad_y: f32,
    gap: f32,
    fixed_w: f32,
    fixed_h: f32,
    min_w: f32,
    direction: FlexDirection,
}

/// A string shaped for a leaf: its layout ([`NO_TEXT`] for none) and its size.
struct Shaped {
    id: TextId,
    w: f32,
    h: f32,
}

/// Width of the arrow of a cascade row or a dropdown.
const ARROW_W: f32 = 14.0;

impl Builder<'_> {
    /// Adds `node` (and, for containers, its subtree) as row(s) under row `parent` and returns
    /// its taffy node.
    pub(crate) fn build(&mut self, node: NodeIx, shown: Shown, parent: u32, indent: f32) -> NodeId {
        let row = self.rows.len() as u32;
        assert!(parent == NO_ROW || parent < row, "a parent row comes first");
        self.rows.push(Pending {
            taffy_node: NodeId::from(0u64),
            node,
            shown,
            parent,
            label: NO_TEXT,
            accel: NO_TEXT,
            math: u32::MAX,
            prose: u32::MAX,
            icon: u32::MAX,
            indent,
        });
        let st = |p| self.style(node, shown, p);
        let m = Metrics {
            pad: st(StyleProp::Pad),
            pad_y: st(StyleProp::PadY),
            gap: st(StyleProp::Gap),
            fixed_w: st(StyleProp::Width),
            fixed_h: st(StyleProp::Height),
            min_w: st(StyleProp::MinWidth),
            direction: if st(StyleProp::Dir) != 0.0 {
                FlexDirection::Row
            } else {
                FlexDirection::Column
            },
        };
        let style = Style {
            flex_grow: st(StyleProp::Grow),
            flex_shrink: 0.0,
            ..Style::default()
        };
        let kind = self.tables.nodes.kind[node as usize];
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
        let taffy_node = if container {
            self.container(row, &m, style)
        } else {
            self.leaf(row, &m, style)
        };
        self.rows[row as usize].taffy_node = taffy_node;
        debug_assert!(
            self.rows.len() > row as usize,
            "the row and its subtree are built"
        );
        taffy_node
    }

    /// [`paint::row_style`] of a row as a float.
    pub(crate) fn style(&self, node: NodeIx, shown: Shown, p: StyleProp) -> f32 {
        debug_assert!(
            (node as usize) < self.tables.nodes.len(),
            "node {node} out of range"
        );
        paint::row_style(self.tables, self.title_style, node, shown, p) as f32
    }
}

// ---- private ----

impl Builder<'_> {
    /// Shapes `sym` at `px` (nothing for an absent string).
    fn text(&mut self, sym: Sym, px: f32) -> Shaped {
        debug_assert!(px.is_finite(), "a font size is finite");
        if sym.is_none() {
            return Shaped {
                id: NO_TEXT,
                w: 0.0,
                h: 0.0,
            };
        }
        let id = self.text.shape(self.strings, sym, px.max(1.0) as u16);
        debug_assert_ne!(id, NO_TEXT);
        let l = self.text.layout(id);
        let shaped = Shaped {
            id,
            w: l.width(),
            h: l.height(),
        };
        debug_assert!(shaped.w >= 0.0, "a shaped width is not negative");
        shaped
    }

    /// A flex container row and its children's subtrees.
    fn container(&mut self, row: u32, m: &Metrics, mut style: Style) -> NodeId {
        assert!((row as usize) < self.rows.len(), "row {row} is built");
        let Pending { node, shown, .. } = self.rows[row as usize];
        let kind = self.tables.nodes.kind[node as usize];
        if kind == Kind::Dialog {
            debug_assert_eq!(self.rows[row as usize].parent, NO_ROW, "a dialog is a root");
        }
        style.flex_direction = m.direction;
        if m.direction == FlexDirection::Row {
            let stretch = self.style(node, shown, StyleProp::Stretch) != 0.0;
            style.align_items = Some(if stretch {
                AlignItems::STRETCH
            } else {
                AlignItems::CENTER
            });
        }
        let split = kind == Kind::Split && shown == Shown::Normal;
        let gap = if split { m.gap.max(SPLIT_GAP) } else { m.gap };
        style.gap = Size {
            width: LengthPercentage::length(gap),
            height: LengthPercentage::length(gap),
        };
        style.padding = padding(m.pad, m.pad_y);
        style.size = Size {
            width: if m.fixed_w > 0.0 {
                Dimension::length(m.fixed_w)
            } else {
                Dimension::auto()
            },
            height: if m.fixed_h > 0.0 {
                Dimension::length(m.fixed_h)
            } else {
                Dimension::auto()
            },
        };
        style.min_size.width = LengthPercentageAuto::length(m.min_w);
        if kind == Kind::Scroll && shown == Shown::Normal {
            // A viewport: its own size comes from its parent, never from its content.
            style.overflow = Point {
                x: Overflow::Hidden,
                y: Overflow::Hidden,
            };
            style.align_items = Some(AlignItems::STRETCH);
            // Zero basis: `grow` hands it the parent's free space, never its content's.
            style.flex_basis = Dimension::length(0.0);
        }
        let child_indent = if shown == Shown::Popup {
            self.check_indent(node)
        } else {
            0.0
        };
        let kids: Vec<NodeIx> = self.tables.nodes.children(node).collect();
        let mut tkids: Vec<NodeId> = Vec::with_capacity(kids.len());
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
        if split
            && tkids.len() >= 2
            && let Some(px) = self.state.split_of(self.tables.nodes.id[node as usize])
        {
            self.size_panes(tkids[0], tkids[1], px, m.direction);
        }
        self.tree
            .new_with_children(style, &tkids)
            .expect("taffy accepts a fresh node's children")
    }

    /// A leaf row: its content (label, icon, accelerator, math) measured into a minimum size;
    /// a `prose` leaf is measured by taffy at its final width instead.
    fn leaf(&mut self, row: u32, m: &Metrics, mut style: Style) -> NodeId {
        let Pending {
            node,
            shown,
            indent,
            ..
        } = self.rows[row as usize];
        let i = node as usize;
        let kind = self.tables.nodes.kind[i];
        debug_assert!(indent >= 0.0, "an indicator inset is not negative");
        let font = self.style(node, shown, StyleProp::Font).max(8.0);
        if kind == Kind::Prose {
            // Width comes from the parent (stretch); the height is measured at that width.
            style.padding = padding(m.pad, m.pad_y);
            style.min_size.width = LengthPercentageAuto::length(m.min_w);
            let context = ProseContext {
                src: self.tables.nodes.prop(Prop::Value)[i],
                px: font as u16,
            };
            return self
                .tree
                .new_leaf_with_context(style, context)
                .expect("taffy accepts a leaf");
        }
        // A text field shows its value in the label slot.
        let label_prop = if matches!(kind, Kind::TextInput | Kind::TextArea) {
            Prop::Value
        } else {
            Prop::Label
        };
        let label_sym = self.tables.nodes.prop(label_prop)[i];
        let icon_src = self.tables.nodes.prop(Prop::Icon)[i];
        let icon = if kind == Kind::Button && !icon_src.is_none() {
            self.text.icon(self.strings, icon_src)
        } else {
            None
        };
        let label = match icon {
            // An icon replaces the label: a square of 1.4 em.
            Some(id) => {
                self.rows[row as usize].icon = id;
                let side = (font * 1.4).round();
                Shaped {
                    id: NO_TEXT,
                    w: side,
                    h: side,
                }
            }
            None => self.text(label_sym, font),
        };
        let (mut w, mut h) = (label.w + 2.0 * m.pad, label.h + 2.0 * m.pad_y);
        match (kind, shown) {
            (_, Shown::Cascade) => w += indent + m.gap + ARROW_W,
            (Kind::Item, _) => {
                let accel = self.text(self.tables.nodes.prop(Prop::Accel)[i], font);
                self.rows[row as usize].accel = accel.id;
                w += indent + if accel.w > 0.0 { m.gap + accel.w } else { 0.0 };
            }
            (Kind::Separator, _) => (w, h) = (0.0, 0.0),
            (Kind::Checkbox, _) => w += label.h + m.gap,
            (Kind::Dropdown, _) => w += m.gap + ARROW_W,
            (Kind::TextInput, _) => h = h.max(font * 1.4 + 2.0 * m.pad_y),
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
                h = (label.h.max(eh) + font * 1.4 + 2.0 * m.pad_y)
                    .max(font * 4.0)
                    .min(font * AREA_EM_MAX + 2.0 * m.pad_y);
                // Width comes from the parent (stretch), never from the longest line.
                w = 2.0 * m.pad + font;
            }
            (Kind::Math, _) => {
                let src = self.tables.nodes.prop(Prop::Value)[i];
                if let Ok(id) = self.text.math(self.strings, src, font as u16) {
                    let list = self.text.math_list(id);
                    (w, h) = (
                        list.width + 2.0 * m.pad,
                        list.height + list.depth + 2.0 * m.pad_y,
                    );
                    self.rows[row as usize].math = id;
                }
            }
            _ => {}
        }
        self.rows[row as usize].label = label.id;
        if m.fixed_w > 0.0 {
            w = m.fixed_w;
        }
        if m.fixed_h > 0.0 {
            h = m.fixed_h;
        }
        debug_assert!(w >= 0.0, "a leaf's width is not negative");
        debug_assert!(h >= 0.0, "a leaf's height is not negative");
        style.min_size = Size {
            width: LengthPercentageAuto::length(w.max(m.min_w)),
            height: LengthPercentageAuto::length(h),
        };
        // A canvas fills its parent on both axes (it may sit in a row beside the docs
        // page), so only a styled height pins it.
        if kind != Kind::Canvas || m.fixed_h > 0.0 {
            style.size.height = Dimension::length(h);
        }
        self.tree.new_leaf(style).expect("taffy accepts a leaf")
    }

    /// Pins the second pane of a dragged `split` to `px` along its axis and lets the first take
    /// the rest (overriding both panes' own sizes and grow factors).
    fn size_panes(&mut self, first: NodeId, second: NodeId, px: f32, direction: FlexDirection) {
        assert!(px.is_finite(), "split size must be finite");
        assert!(px >= 0.0, "split size must not be negative");
        debug_assert_ne!(first, second, "two panes");
        let mut a = self
            .tree
            .style(first)
            .expect("a built pane has a style")
            .clone();
        a.flex_grow = a.flex_grow.max(1.0);
        a.flex_basis = Dimension::length(0.0);
        let mut b = self
            .tree
            .style(second)
            .expect("a built pane has a style")
            .clone();
        b.flex_grow = 0.0;
        if direction == FlexDirection::Row {
            a.min_size.width = LengthPercentageAuto::length(0.0);
            a.size.width = Dimension::auto();
            b.size.width = Dimension::length(px);
            b.min_size.width = LengthPercentageAuto::length(px);
        } else {
            a.min_size.height = LengthPercentageAuto::length(0.0);
            a.size.height = Dimension::auto();
            b.size.height = Dimension::length(px);
            b.min_size.height = LengthPercentageAuto::length(px);
        }
        self.tree
            .set_style(first, a)
            .expect("a built pane takes a style");
        self.tree
            .set_style(second, b)
            .expect("a built pane takes a style");
    }

    /// Indicator inset for a popup: reserved for every row when any row is checkable, so labels
    /// stay aligned (Tk does the same).
    fn check_indent(&self, menu: NodeIx) -> f32 {
        let n = &self.tables.nodes;
        debug_assert!(
            matches!(n.kind[menu as usize], Kind::Menu | Kind::Dropdown),
            "only a popup reserves an indicator inset"
        );
        let any = n
            .children(menu)
            .any(|c| !n.prop(Prop::Checked)[c as usize].is_none());
        if any { 18.0 } else { 0.0 }
    }
}

/// Taffy padding of `pad` left and right, `pad_y` above and below.
fn padding(pad: f32, pad_y: f32) -> taffy::Rect<LengthPercentage> {
    debug_assert!(pad >= 0.0, "style lengths are unsigned");
    debug_assert!(pad_y >= 0.0, "style lengths are unsigned");
    taffy::Rect {
        left: LengthPercentage::length(pad),
        right: LengthPercentage::length(pad),
        top: LengthPercentage::length(pad_y),
        bottom: LengthPercentage::length(pad_y),
    }
}
