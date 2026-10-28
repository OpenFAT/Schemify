//! Placement of a [`Builder`]'s taffy tree: the tree in, absolute rectangles, clips and scroll
//! extents of its rows out (a [`Frame`]). Children of a `scroll` node shift up by its clamped
//! offset and are clipped to it; `prose` leaves are measured at the width they get.
//!
//! Contents:
//! - `pub(crate) struct Frame`: `shift`
//! - `pub(crate) struct Placed`
//! - `pub(crate) struct Builder` (placement): `float`, `place`

use taffy::{AvailableSpace, Layout, NodeId, Size};

use crate::flex::{Builder, NO_ROW};
use crate::kinds::{Kind, Prop, StyleProp};
use crate::layout::{NO_CLIP, PROSE_W_MAX, Rect, Shown};
use crate::tables::{NO_NODE, NodeIx};

/// Rectangles, clips and scroll extents accumulated across [`Builder::place`] calls; parallel
/// to [`Builder::rows`].
#[derive(Default)]
pub(crate) struct Frame {
    pub(crate) rect: Vec<Rect>,
    pub(crate) clip: Vec<Rect>,
    pub(crate) scroller: Vec<NodeIx>,
    pub(crate) scrolled: Vec<f32>,
    /// Each laid-out `scroll` node, once, parallel to `scroll_max`.
    pub(crate) scroll_node: Vec<NodeIx>,
    /// Largest offset of each node in `scroll_node`.
    pub(crate) scroll_max: Vec<f32>,
}

/// Where [`Builder::float`] put a root laid out at the origin: its first row and its size.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Placed {
    pub(crate) start: usize,
    pub(crate) w: f32,
    pub(crate) h: f32,
}

impl Frame {
    /// Moves rows `start..` (a popup or dialog placed at the origin) by `(dx, dy)`.
    pub(crate) fn shift(&mut self, start: usize, dx: f32, dy: f32) {
        assert!(start <= self.rect.len(), "rows to shift exist");
        assert_eq!(self.clip.len(), self.rect.len(), "clips in step with rects");
        debug_assert!(dx.is_finite(), "a finite move");
        debug_assert!(dy.is_finite(), "a finite move");
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

impl Builder<'_> {
    /// Builds and places `node` as its own root at its content size, at the origin (the
    /// caller shifts it into place).
    pub(crate) fn float(&mut self, node: NodeIx, shown: Shown, out: &mut Frame) -> Placed {
        let start = self.rows.len();
        assert!(
            (node as usize) < self.tables.nodes.len(),
            "node {node} out of range"
        );
        let root = self.build(node, shown, NO_ROW, 0.0);
        let max_content = Size {
            width: AvailableSpace::MaxContent,
            height: AvailableSpace::MaxContent,
        };
        self.place(start, root, max_content, out);
        let r = out.rect[start];
        debug_assert!(r.w >= 0.0, "taffy sizes are not negative");
        debug_assert!(r.h >= 0.0, "taffy sizes are not negative");
        Placed {
            start,
            w: r.w,
            h: r.h,
        }
    }

    /// Lays out the root built at `start` at the origin and writes absolute rects for its rows
    /// into `out`: children of a `scroll` node shift up by its clamped offset and are clipped
    /// to it.
    pub(crate) fn place(
        &mut self,
        start: usize,
        root: NodeId,
        avail: Size<AvailableSpace>,
        out: &mut Frame,
    ) {
        assert_eq!(out.rect.len(), start, "rows are placed in order");
        assert!(start < self.rows.len(), "the root's row is built");
        debug_assert_eq!(
            self.rows[start].taffy_node, root,
            "the root's row is the first placed"
        );
        debug_assert_eq!(out.scroll_node.len(), out.scroll_max.len());
        self.compute(root, avail);
        // Scroll extents first: a child's position depends on its scroll parent's offset.
        let rel: Vec<Layout> = (start..self.rows.len())
            .map(|r| {
                *self
                    .tree
                    .layout(self.rows[r].taffy_node)
                    .expect("laid-out node")
            })
            .collect();
        let mut bottom: Vec<(usize, f32)> = Vec::new();
        for r in start..self.rows.len() {
            let p = self.rows[r].parent;
            if p != NO_ROW && (p as usize) >= start && self.is_scroll(p as usize) {
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
            let (ox, oy, clip, scroller, scrolled) = if inside {
                let p = p as usize;
                let pr = out.rect[p];
                if self.is_scroll(p) {
                    let node = self.rows[p].node;
                    let content = bottom.iter().find(|(s, _)| *s == p).map_or(0.0, |b| b.1);
                    let pad = self.style(node, Shown::Normal, StyleProp::PadY);
                    let max = (content + pad - pr.h).max(0.0);
                    let off = self
                        .state
                        .scroll_of(self.tables.nodes.id[node as usize])
                        .clamp(0.0, max);
                    if !out.scroll_node.contains(&node) {
                        out.scroll_node.push(node);
                        out.scroll_max.push(max);
                    }
                    let clip = out.clip[p].intersect(&pr);
                    (pr.x, pr.y - off, clip, node, out.scrolled[p] + off)
                } else {
                    (pr.x, pr.y, out.clip[p], out.scroller[p], out.scrolled[p])
                }
            } else {
                (0.0, 0.0, NO_CLIP, NO_NODE, 0.0)
            };
            out.rect.push(Rect {
                x: ox + l.location.x,
                y: oy + l.location.y,
                w: l.size.width,
                h: l.size.height,
            });
            out.clip.push(clip);
            out.scroller.push(scroller);
            out.scrolled.push(scrolled);
            debug_assert!(scrolled >= 0.0, "offsets are clamped at 0");
        }
        self.prose_rows(start, out);
        debug_assert_eq!(out.rect.len(), self.rows.len());
        debug_assert_eq!(out.clip.len(), out.rect.len());
        debug_assert_eq!(out.scroll_node.len(), out.scroll_max.len());
    }
}

// ---- private ----

impl Builder<'_> {
    /// Runs taffy over the tree under `root`, measuring `prose` leaves at the width they get.
    fn compute(&mut self, root: NodeId, avail: Size<AvailableSpace>) {
        if let AvailableSpace::Definite(w) = avail.width {
            debug_assert!(w > 0.0, "a definite width is positive");
        }
        let (tree, text, strings) = (&mut self.tree, &mut *self.text, self.strings);
        tree.compute_layout_with_measure(root, avail, |inputs, _, context, style| {
            let context = context.copied();
            taffy::compute_leaf_layout(
                inputs,
                style,
                |_, _| 0.0,
                |known, space| {
                    let Some(c) = context.filter(|c| !c.src.is_none() && c.px > 0) else {
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
        .expect("taffy lays out a tree it built");
        debug_assert!(self.tree.layout(root).is_ok(), "the root is laid out");
    }

    /// True when row `r` is a `scroll` node shown as itself.
    fn is_scroll(&self, r: usize) -> bool {
        debug_assert!(r < self.rows.len());
        debug_assert!((self.rows[r].node as usize) < self.tables.nodes.len());
        self.rows[r].shown == Shown::Normal
            && self.tables.nodes.kind[self.rows[r].node as usize] == Kind::Scroll
    }

    /// Prose rows from `start`: the layout at the final content width (cached from measuring).
    fn prose_rows(&mut self, start: usize, out: &Frame) {
        assert_eq!(out.rect.len(), self.rows.len(), "every row is placed");
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
        debug_assert_eq!(out.rect.len(), self.rows.len(), "prose adds no rows");
    }
}
