//! Symbols as instances stamp them: one cell's primitives in symbol-local coordinates, and the
//! per-cell cache the scene reads. In: a symbol document. Out: [`SymPrims`].
//!
//! Contents:
//! - `pub struct SymPrims`
//! - `pub(super) struct SymText`
//! - `pub struct Symbols`, `Symbols::clear`, `Symbols::refresh`, `Symbols::get`
//! - `pub(super) fn attr`
//! - `pub(super) fn layer_attr`
//! - `pub(super) fn style_attr`
//! - `pub(super) fn size_of`
//! - `pub(super) fn sym_prims`
//! - `pub(super) fn rotated_extent`
//! - `pub(super) fn sym_text_at`
//! - `pub(super) fn text_size`
//! - `pub(super) fn text_start`

use schemify_canvas::LineStyle;
use schemify_doc::{DocHandle, Document, Span, Strings as DocStrings, Sym};
use schemify_expr::Template;
use schemify_format::{Orient, RecordKind};

use crate::geometry::Pt;

use super::{PIN_HALF, TEXT_PER_SIZE, UNIT};

/// One cell's symbol in symbol-local coordinates: what every instance of it stamps.
#[derive(Clone, Debug, Default)]
pub struct SymPrims {
    pub(super) lines: Vec<([i32; 4], u8, LineStyle)>,
    pub(super) rects: Vec<([i32; 4], u8, LineStyle)>,
    pub(super) arcs: Vec<([i32; 5], u8, LineStyle)>,
    pub(super) polys: Vec<(Vec<Pt>, bool, u8, LineStyle)>,
    pub(super) texts: Vec<SymText>,
    pub(super) pins: Vec<Pt>,
    pub(super) bbox: [i32; 4],
}

#[derive(Clone, Debug)]
pub(super) struct SymText {
    pub(super) at: Pt,
    pub(super) size: i32,
    pub(super) layer: u8,
    hcenter: bool,
    /// The record's orientation: the text turns like a `text` record's (format spec §4).
    pub(super) orient: Orient,
    /// Parsed template; `None` for plain text or a template that does not parse (drawn raw).
    pub(super) template: Option<Template>,
    pub(super) raw: String,
}

/// Symbol primitives per indexed cell row, rebuilt when the cell's document changes.
#[derive(Debug, Default)]
pub struct Symbols {
    /// Per row: the open document drawn (`None`: the indexed symbol) and its edit epoch.
    pub(super) rows: Vec<Option<(Option<DocHandle>, u32, SymPrims)>>,
    /// Advances whenever an entry changes; part of the scene's built key.
    pub epoch: u32,
}

impl Symbols {
    /// Drops every entry (the library rows shifted: a re-index, a project reload); the
    /// epoch keeps advancing, so no scene built from the old entries matches it again.
    ///
    /// # Panics
    ///
    /// Never: the assert checks that the epoch advanced.
    pub fn clear(&mut self) {
        let before = self.epoch;
        self.rows.clear();
        self.epoch = self.epoch.wrapping_add(1);
        assert_ne!(self.epoch, before);
    }

    // ponytail: libraries are indexed once per session; when re-indexing lands (file
    // watching), clear this cache with it, since `index_library` invalidates cell rows.

    /// Makes the entry of cell `row` current for `doc`: open document `h` at edit epoch
    /// `epoch`, or the indexed symbol (`h == None`, epoch 0). Rebuilds the primitives and
    /// advances [`Symbols::epoch`] only when that source changed.
    ///
    /// # Panics
    ///
    /// If `h` is `None` and `epoch` is not 0.
    pub fn refresh(&mut self, row: usize, h: Option<DocHandle>, epoch: u32, doc: &Document) {
        assert!(h.is_some() || epoch == 0, "the indexed symbol has epoch 0");
        if self.rows.len() <= row {
            self.rows.resize_with(row + 1, || None);
        }
        let fresh = matches!(&self.rows[row], Some((d, e, _)) if *d == h && *e == epoch);
        if !fresh {
            self.rows[row] = Some((h, epoch, sym_prims(doc)));
            self.epoch = self.epoch.wrapping_add(1);
        }
        assert!(
            self.rows[row]
                .as_ref()
                .is_some_and(|e| e.0 == h && e.1 == epoch),
            "the entry is current"
        );
    }

    pub(super) fn get(&self, row: usize) -> Option<&SymPrims> {
        self.rows.get(row)?.as_ref().map(|e| &e.2)
    }
}

pub(super) fn attr<'d>(doc: &'d Document, span: Span, key: &str) -> Option<&'d str> {
    let v = doc.pool().find_attr(doc.strings(), span, key)?;
    Some(v.first().map_or("", |s| doc.strings().get(*s)))
}

pub(super) fn layer_attr(doc: &Document, span: Span, default: u8) -> u8 {
    assert!(default < 22, "a drawing layer");
    attr(doc, span, "layer")
        .and_then(|v| v.parse::<u8>().ok())
        .filter(|&l| l < 22)
        .unwrap_or(default)
}

/// The `width`, `dash` and `fill` attributes of a graphic (format spec §4, stroke attributes):
/// a value that is not a number reads as absent; the rest rounds and clamps in
/// [`LineStyle::new`]. `fill true` fills a closed shape; absent or `false` leaves it to the
/// layer.
pub(super) fn style_attr(doc: &Document, span: Span) -> LineStyle {
    let n = |key: &str, absent: u8| {
        attr(doc, span, key)
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .map_or(absent, |v| v.round().clamp(0.0, 255.0) as u8)
    };
    let st = LineStyle::new(n("width", 1), n("dash", 0));
    if attr(doc, span, "fill") == Some("true") {
        st.filled()
    } else {
        st
    }
}

pub(super) fn size_of(text: Option<&str>, default: f64) -> i32 {
    assert!(default > 0.0, "a positive default size");
    let s = text.and_then(|t| t.parse::<f64>().ok()).unwrap_or(default);
    let size = (s.clamp(0.01, 100.0) * TEXT_PER_SIZE) as i32;
    assert!(size > 0, "a drawable text size");
    size
}

/// Builds the symbol primitives of a cell document (its pins and rank-2 graphics).
pub(super) fn sym_prims(doc: &Document) -> SymPrims {
    let g = doc.graphics();
    let s = doc.strings();
    let coords = &doc.pool().coords;
    let mut p = SymPrims::default();
    let grow = |b: &mut [i32; 4], x: i32, y: i32| {
        b[0] = b[0].min(x);
        b[1] = b[1].min(y);
        b[2] = b[2].max(x);
        b[3] = b[3].max(y);
    };
    let mut bbox = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    for i in 0..g.shape.len() {
        let c = &coords[g.coords[i].range()];
        debug_assert!(c.len() >= 2, "every graphic has a point");
        let layer = layer_attr(doc, g.attrs[i], 4);
        let style = style_attr(doc, g.attrs[i]);
        match g.shape[i] {
            RecordKind::Line => {
                p.lines.push(([c[0], c[1], c[2], c[3]], layer, style));
                grow(&mut bbox, c[0], c[1]);
                grow(&mut bbox, c[2], c[3]);
            }
            RecordKind::Rect => {
                p.rects.push(([c[0], c[1], c[2], c[3]], layer, style));
                grow(&mut bbox, c[0], c[1]);
                grow(&mut bbox, c[2], c[3]);
            }
            RecordKind::Arc | RecordKind::Circle => {
                let (start, sweep) = if g.shape[i] == RecordKind::Arc {
                    (
                        degrees_milli(s, g.angles[i][0]),
                        degrees_milli(s, g.angles[i][1]),
                    )
                } else {
                    (0, 360_000)
                };
                p.arcs
                    .push(([c[0], c[1], c[2].max(0), start, sweep], layer, style));
                grow(&mut bbox, c[0] - c[2], c[1] - c[2]);
                grow(&mut bbox, c[0] + c[2], c[1] + c[2]);
            }
            RecordKind::Poly => {
                let pts: Vec<Pt> = c.chunks_exact(2).map(|q| (q[0], q[1])).collect();
                for q in &pts {
                    grow(&mut bbox, q.0, q.1);
                }
                let closed = attr(doc, g.attrs[i], "closed") == Some("true");
                if pts.len() >= 2 {
                    p.polys.push((pts, closed, layer, style));
                }
            }
            RecordKind::Stext => {
                let raw = s.get(g.text[i]).to_owned();
                let template = raw
                    .contains('{')
                    .then(|| Template::parse(&raw).ok())
                    .flatten();
                p.texts.push(SymText {
                    at: (c[0], c[1]),
                    size: size_of(attr(doc, g.attrs[i], "size"), 0.3),
                    layer: layer_attr(doc, g.attrs[i], 3),
                    hcenter: attr(doc, g.attrs[i], "hcenter").is_some(),
                    orient: g.orient[i],
                    template,
                    raw,
                });
                grow(&mut bbox, c[0], c[1]);
            }
            k => unreachable!("{k:?} is not a graphic"),
        }
    }
    let pins = doc.pins();
    for i in 0..pins.x.len() {
        p.pins.push((pins.x[i], pins.y[i]));
        grow(&mut bbox, pins.x[i] - PIN_HALF, pins.y[i] - PIN_HALF);
        grow(&mut bbox, pins.x[i] + PIN_HALF, pins.y[i] + PIN_HALF);
    }
    if bbox[0] > bbox[2] {
        bbox = [-5 * UNIT, -5 * UNIT, 5 * UNIT, 5 * UNIT];
    }
    assert!(bbox[0] <= bbox[2] && bbox[1] <= bbox[3], "an ordered box");
    assert_eq!(p.pins.len(), pins.x.len(), "one pin per pin row");
    p.bbox = bbox;
    p
}

/// The far corner of a `(w, h)` text box from its anchor after `rot` clockwise quarter turns.
pub(super) fn rotated_extent((w, h): (i32, i32), rot: u8) -> (i32, i32) {
    match rot % 4 {
        0 => (w, h),
        1 => (-h, w),
        2 => (-w, -h),
        _ => (h, -w),
    }
}

/// Where a symbol text turned `rot` quarter turns with its anchor at `q` starts: the anchor;
/// for `hcenter` half its width back along its direction; under a mirror (`mirrored`, the
/// composed orientation's) its whole width back, so the text ends at the anchor and a label
/// column right of the body stays clear of the mirrored body (format spec §4, `stext`).
pub(super) fn sym_text_at(q: Pt, t: &SymText, text: &str, rot: u8, mirrored: bool) -> Pt {
    if !t.hcenter && !mirrored {
        return q;
    }
    let (w, _) = text_size(t.size, text);
    // ponytail: right alignment uses the 0.6 em/char estimate, so a mirrored column is
    // ragged by the estimate's error; shape in the scene if that shows.
    let back = if t.hcenter { w / 2 } else { w };
    let (dx, dy) = rotated_extent((-back, 0), rot);
    assert!(back >= 0);
    (q.0 + dx, q.1 + dy)
}

/// Approximate text box: 0.6 em per character (the renderer shapes the real glyphs).
pub(super) fn text_size(size: i32, text: &str) -> (i32, i32) {
    debug_assert!(size >= 0, "a non-negative text size");
    let n = text.chars().count().max(1) as f64;
    ((f64::from(size) * 0.6 * n) as i32, size)
}

/// Where text `s` turned `rot` quarter turns about `at` is drawn from, and its drawn turns
/// (`< 2`: never upside down, format spec §3); `None` when it draws nothing.
pub(super) fn text_start(at: Pt, size: i32, s: &str, rot: u8) -> Option<(Pt, u8)> {
    if s.is_empty() || size <= 0 {
        return None;
    }
    let rot = rot % 4;
    // The upright turn's box from the far corner is the same box (rotated_extent(r + 2) is
    // -rotated_extent(r)).
    let (dx, dy) = rotated_extent(text_size(size, s), rot);
    let start = if rot >= 2 {
        ((at.0 + dx, at.1 + dy), rot - 2)
    } else {
        (at, rot)
    };
    debug_assert!(start.1 < 2);
    Some(start)
}

// ---- private ----

fn degrees_milli(s: &DocStrings, sym: Sym) -> i32 {
    (s.get(sym).parse::<f64>().unwrap_or(0.0) * 1000.0) as i32
}
