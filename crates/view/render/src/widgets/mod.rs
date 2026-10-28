//! UI rows: the `ui` crate's tables and one frame's layout in, painter calls out, row by row
//! through [`Painter::ui_row`] so a painter that keeps rows repaints only the rows whose
//! [`RowKey`] (their paint inputs) changed. Each row paints its background, border, text,
//! indicators and widget; text, prose, math and icons are drawn by `text`. Reads the UI
//! tables; writes only painter calls.
//!
//! Contents:
//! - `pub struct UiScratch`, `pub struct RowKey`, `pub struct UiRow`
//! - `pub fn row_key(tables, layout, strings, text, state, r) -> RowKey`
//! - `pub fn draw_ui<P: Painter>(p, tables, layout, strings, text, state, layer, scratch)`
//! - `pub use text::{draw_math, draw_prose, draw_text}`
//! - `pub(crate) fn rect(r: schemify_ui::Rect) -> Rect`

mod text;

use kurbo::{Affine, BezPath, Rect};
use vello_cpu::Glyph;

use schemify_ui::{
    Kind, Layer, NO_NODE, NO_TEXT, Prop, RowPaint, Shown, Strings, StyleProp, Sym, TextSystem,
    UiLayout, UiState, UiTables,
};

use crate::painter::{self, Painter};

pub use text::{draw_math, draw_prose, draw_text};

/// Reused buffers for UI painting.
#[derive(Debug, Default)]
pub struct UiScratch {
    glyphs: Vec<Glyph>,
    coords: Vec<i16>,
    path: BezPath,
    images: Vec<peniko::ImageData>,
}

/// Everything a layout row's paint reads, by value: two equal keys paint the same
/// calls. A painter that keeps rows ([`Painter::ui_row`]) repaints a row only when its key
/// changed. The `ui` crate keeps no per-node change signal (its input routing reports only
/// "relayout" and "redraw" for the whole frame), so this is the paint inputs compared field
/// by field: no hash, no collision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowKey {
    /// The node's id and how the row shows it: the row's identity from frame to frame.
    pub id: Sym,
    /// How the row shows its node.
    pub shown: Shown,
    /// The node's widget kind.
    pub kind: Kind,
    /// The row's rect and its scroll panes' clip (logical px, x0 y0 x1 y1).
    pub rect: [f32; 4],
    /// The clip.
    pub clip: [f32; 4],
    /// Its panes' total scroll offset ([`UiLayout::scrolled`]).
    pub scroll: f32,
    /// Label, accelerator, icon, math and prose ids, which name the same layout while
    /// [`TextSystem::epoch`] stays `epoch`.
    pub text: [u32; 5],
    /// The text cache's epoch.
    pub epoch: u32,
    /// The check-indicator inset.
    pub indent: f32,
    /// [`row_paint`]'s colours (hover, active, disabled already applied).
    pub paint: [u32; 2],
    /// Border, border colour, pad, pad-y, fg, gap, accelerator fg, radius.
    pub style: [u32; 8],
    /// Shows a check indicator; is checked.
    pub check: [bool; 2],
    /// A text area's scroll inside itself.
    pub area: f32,
    /// The focused field's editor (laid out): its text, caret and size hashed; `[0; 2]` when
    /// none draws.
    pub editor: [u64; 2],
    /// Set on a kept row that must be painted again (it drew a stand-in, or a mask the atlas
    /// could not place): never equal to a fresh key.
    pub stale: bool,
    /// The GPU painter's pane slot for the row (0: none).
    pub pane: u8,
}

/// One layout row as a painter receives it: its key, whether any of it is in view, the scroll
/// node it scrolls with ([`Sym::NONE`] outside any) and that pane's clip (logical px).
#[derive(Clone, Copy, Debug)]
pub struct UiRow {
    /// The paint inputs.
    pub key: RowKey,
    /// Some of the row is inside its clip.
    pub visible: bool,
    /// The innermost scroll node over it.
    pub pane: Sym,
    /// The clip it is drawn under.
    pub clip: Rect,
}

/// The [`RowKey`] of layout row `r`.
///
/// # Panics
///
/// Panics if `r` is not a row of `layout`.
pub fn row_key(
    tables: &UiTables,
    layout: &UiLayout,
    strings: &Strings,
    text: &TextSystem,
    state: &UiState,
    r: usize,
) -> RowKey {
    assert!(r < layout.len(), "a layout row");
    let n = &tables.nodes;
    let node = layout.node()[r];
    let i = node as usize;
    let (shown, kind, id) = (layout.shown()[r], n.kind()[i], n.id()[i]);
    let style = |prop| schemify_ui::row_style(tables, layout.title_style(), node, shown, prop);
    let RowPaint { bg, fg } = schemify_ui::row_paint(tables, layout, strings, state, r);
    let (q, c) = (layout.rect()[r], layout.clip()[r]);
    let editing = matches!(kind, Kind::TextInput | Kind::TextArea) && state.focus() == id;
    let editor = state
        .editor()
        .filter(|_| editing)
        .and_then(|e| e.try_layout().map(|l| (e, l)))
        .map_or([0; 2], |(e, l)| {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            e.raw_text().hash(&mut h);
            let text = h.finish();
            let mut h = std::collections::hash_map::DefaultHasher::new();
            [l.width(), l.height()].map(f32::to_bits).hash(&mut h);
            if let Some(b) = e.cursor_geometry(1.0) {
                [b.x0, b.y0, b.x1, b.y1].map(f64::to_bits).hash(&mut h);
            }
            [text, h.finish() | 1]
        });
    let area = if kind == Kind::TextArea {
        let max = schemify_ui::area_scroll_max(tables, layout, text, state, r);
        state.scroll_of(id).min(max)
    } else {
        0.0
    };
    let key = RowKey {
        id,
        shown,
        kind,
        rect: [q.x, q.y, q.x + q.w, q.y + q.h],
        clip: [c.x, c.y, c.x + c.w, c.y + c.h],
        scroll: layout.scrolled()[r],
        text: [
            layout.label()[r],
            layout.accel()[r],
            layout.icon()[r],
            layout.math()[r],
            layout.prose()[r],
        ],
        epoch: text.epoch(),
        indent: layout.indent()[r],
        paint: [bg, fg],
        style: [
            StyleProp::Border,
            StyleProp::BorderColor,
            StyleProp::Pad,
            StyleProp::PadY,
            StyleProp::Fg,
            StyleProp::Gap,
            StyleProp::AccelFg,
            StyleProp::Radius,
        ]
        .map(style),
        check: [
            !n.prop(Prop::Checked)[i].is_none(),
            n.is_checked(strings, node),
        ],
        area,
        editor,
        stale: false,
        pane: 0,
    };
    debug_assert!(!key.stale, "a fresh key");
    debug_assert_eq!(key.pane, 0, "a fresh key");
    key
}

/// Paints the rows of one layer. Call with [`Layer::Main`], draw the canvas into the canvas
/// row's rect, then call with [`Layer::Overlay`] so popups cover the schematic. Each row goes
/// through [`Painter::ui_row`] (UI source 0 for the main layer, 1 for the overlay), so a
/// painter that keeps rows repaints only the rows whose [`RowKey`] changed.
///
/// # Panics
///
/// Panics when `layout`'s columns are out of step.
#[allow(
    clippy::too_many_arguments,
    reason = "one frame's UI inputs: tables, layout, strings, text, state, layer and scratch"
)]
pub fn draw_ui<P: Painter>(
    p: &mut P,
    tables: &UiTables,
    layout: &UiLayout,
    strings: &Strings,
    text: &mut TextSystem,
    state: &UiState,
    layer: Layer,
    scratch: &mut UiScratch,
) {
    assert_eq!(layout.rect().len(), layout.len(), "layout columns in step");
    assert_eq!(layout.clip().len(), layout.len(), "layout columns in step");
    // Rows inside a scroll pane are clipped to it; consecutive rows share one clip layer.
    let mut clipped: Option<schemify_ui::Rect> = None;
    let rows = layout.rows(layer);
    let src = match layer {
        Layer::Main => 0,
        Layer::Overlay => 1,
    };
    let mut r = rows.start;
    while r < rows.end {
        let clip = layout.clip()[r];
        if clipped != Some(clip).filter(|c| *c != schemify_ui::NO_CLIP) {
            if clipped.take().is_some() {
                p.pop_clip();
            }
            if clip != schemify_ui::NO_CLIP {
                p.push_clip(rect(clip));
                clipped = Some(clip);
            }
        }
        let vis = layout.rect()[r].intersect(&clip);
        let scroller = layout.scroller(r);
        let row = UiRow {
            key: row_key(tables, layout, strings, text, state, r),
            visible: vis.w > 0.0 && vis.h > 0.0, // else scrolled out of view
            pane: match scroller {
                NO_NODE => Sym::NONE,
                s => tables.nodes.id()[s as usize],
            },
            clip: rect(clip),
        };
        p.ui_row(src, &row, |p| {
            draw_row(p, tables, layout, strings, text, state, scratch, r);
        });
        r += 1;
    }
    if clipped.is_some() {
        p.pop_clip();
    }
    p.ui_end(src);
}

/// A `ui_layout` rect as a kurbo rect.
pub(crate) fn rect(r: schemify_ui::Rect) -> Rect {
    Rect::new(
        f64::from(r.x),
        f64::from(r.y),
        f64::from(r.x + r.w),
        f64::from(r.y + r.h),
    )
}

// ---- private ----

/// Paints row `r` of the layout: background, border, text, indicators and its widget.
#[allow(
    clippy::too_many_arguments,
    reason = "one row's inputs: the UI tables, layout, strings, text, state, row and scratch"
)]
fn draw_row<P: Painter>(
    p: &mut P,
    tables: &UiTables,
    layout: &UiLayout,
    strings: &Strings,
    text: &mut TextSystem,
    state: &UiState,
    scratch: &mut UiScratch,
    r: usize,
) {
    let n = &tables.nodes;
    let node = layout.node()[r];
    let i = node as usize;
    let shown = layout.shown()[r];
    let kind = n.kind()[i];
    let id = n.id()[i];
    let style = |prop| schemify_ui::row_style(tables, layout.title_style(), node, shown, prop);
    let area = rect(layout.rect()[r]);
    let RowPaint { bg, fg } = schemify_ui::row_paint(tables, layout, strings, state, r);
    let fg = painter::rgba(fg);
    if bg & 0xff != 0 && kind != Kind::Canvas {
        p.fill_rect(area, painter::rgba(bg));
    }
    let border = style(StyleProp::Border);
    if border > 0 {
        let w = f64::from(border);
        let color = painter::rgba(style(StyleProp::BorderColor));
        painter::outline(p, &mut scratch.path, area.inset(-w / 2.0), w, color);
    }
    let pad = f64::from(style(StyleProp::Pad));
    let indent = f64::from(layout.indent()[r]);
    if kind == Kind::Separator {
        let y = (area.y0 + area.y1) / 2.0;
        p.fill_rect(
            Rect::new(area.x0 + 2.0, y.floor(), area.x1 - 2.0, y.floor() + 1.0),
            painter::rgba(style(StyleProp::Fg)),
        );
        return;
    }
    let area_kind = kind == Kind::TextArea;
    let editing = matches!(kind, Kind::TextInput | Kind::TextArea) && state.focus() == id;
    let label = layout.label()[r];
    let pad_y = f64::from(style(StyleProp::PadY));
    // A text area taller than its box scrolls inside itself, clipped to the box.
    let scrolled = if area_kind {
        let max = schemify_ui::area_scroll_max(tables, layout, text, state, r);
        p.push_clip(area);
        f64::from(state.scroll_of(id).min(max))
    } else {
        0.0
    };
    if editing && let Some(ed) = state.editor().and_then(|e| e.try_layout()) {
        let y = if area_kind {
            area.y0 + pad_y - scrolled
        } else {
            area.y0 + (area.height() - f64::from(ed.height())) / 2.0
        };
        let (x, w) = (area.x0 + pad, f64::from(ed.width()));
        draw_text(p, ed, x, y, fg, scratch);
        if let Some(c) = state.editor().and_then(|e| e.cursor_geometry(1.0)) {
            p.fill_rect(Rect::new(x + c.x0, y + c.y0, x + c.x0 + 1.0, y + c.y1), fg);
        }
        debug_assert!(w >= 0.0);
    } else if label != NO_TEXT {
        let l = text.layout(label);
        let y = if area_kind {
            area.y0 + pad_y - scrolled
        } else {
            area.y0 + (area.height() - f64::from(l.height())) / 2.0
        };
        let x = match kind {
            Kind::Checkbox => {
                area.x0 + pad + f64::from(l.height()) + f64::from(style(StyleProp::Gap))
            }
            _ => area.x0 + pad + indent,
        };
        draw_text(p, l, x, y, fg, scratch);
    }
    if area_kind {
        p.pop_clip();
    }
    let accel = layout.accel()[r];
    if accel != NO_TEXT {
        let l = text.layout(accel);
        let y = area.y0 + (area.height() - f64::from(l.height())) / 2.0;
        let color = painter::rgba(style(StyleProp::AccelFg));
        draw_text(
            p,
            l,
            area.x1 - pad - f64::from(l.width()),
            y,
            color,
            scratch,
        );
    }
    let icon = layout.icon()[r];
    if icon != u32::MAX {
        text::draw_icon(p, text.icon_paths(icon), area, (pad, pad_y), fg, scratch);
    }
    if shown == Shown::Cascade || (kind == Kind::Dropdown && shown == Shown::Normal) {
        let (cx, cy) = (area.x1 - pad - 4.0, (area.y0 + area.y1) / 2.0);
        let path = &mut scratch.path;
        path.truncate(0);
        if kind == Kind::Dropdown {
            path.move_to((cx - 4.0, cy - 2.0));
            path.line_to((cx + 4.0, cy - 2.0));
            path.line_to((cx, cy + 3.0));
        } else {
            path.move_to((cx - 3.0, cy - 4.0));
            path.line_to((cx + 2.0, cy));
            path.line_to((cx - 3.0, cy + 4.0));
        }
        path.close_path();
        p.fill_path(path, fg);
    }
    let checkable =
        !n.prop(Prop::Checked)[i].is_none() && matches!(kind, Kind::Item | Kind::Checkbox);
    if checkable {
        let size = 9.0;
        let x = area.x0 + pad;
        let y = ((area.y0 + area.y1) / 2.0 - size / 2.0).floor();
        let b = Rect::new(x, y, x + size, y + size);
        let path = &mut scratch.path;
        path.truncate(0);
        if style(StyleProp::Radius) > 0 {
            path.extend(kurbo::Shape::path_elements(
                &kurbo::Circle::new(b.center(), size / 2.0),
                0.1,
            ));
        } else {
            path.extend(kurbo::Shape::path_elements(&b.inset(-0.5), 0.1));
        }
        p.stroke_path(path, 1.0, fg);
        if n.is_checked(strings, node) {
            p.fill_rect(b.inset(-2.0), fg);
        }
    }
    if kind == Kind::Math && layout.math()[r] != u32::MAX {
        let list = text.math_list(layout.math()[r]);
        let at = Affine::translate((area.x0 + pad, area.y0 + f64::from(style(StyleProp::PadY))));
        draw_math(p, list, text.math_fonts(), at, fg, scratch);
    }
    if kind == Kind::Prose && layout.prose()[r] != u32::MAX {
        draw_prose(
            p,
            layout.prose()[r],
            text,
            (area.x0 + pad, area.y0 + pad_y),
            fg,
            scratch,
        );
    }
}
