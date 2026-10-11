//! Edit gestures beyond the M2 state machine (xschem parity,
//! docs/research/xschem-feature-inventory.md §2.4, mouse gestures):
//!
//! - **partial wire selection**: a Ctrl+drag box (a plain drag with Options → enable stretch)
//!   selects the objects inside it and, of each wire crossing its edge, only the end inside;
//!   the next move moves just those ends, so the wires stretch;
//! - **vertex editing**: a drag that starts on an end of the one selected wire or line, a
//!   corner of a selected rectangle or a vertex of a selected polygon moves just that point
//!   (symbol graphics and schematic `shape`s alike), one undo.
//!
//! Tables: the partial selection is the level's `ends` (`session::Level::ends`, written only
//! here); a vertex drag is `Tool::Vertex`. Both run as steps of `Editor::gesture`.

use schemify_doc::{DocHandle, EditRequest, Op, RecordBuf, TableId};
use schemify_format::{RecordKind, format_coord, parse_coord};
use schemify_render::canvas::CanvasPrims;

use super::{Editor, HIT_PX, Tool};
use crate::edit;
use crate::geometry::{Pt, Xform};
use crate::scene::{LAYER_SEL, ObjKey};

/// Half side of the mark on a selected wire end, milli-units (xschem's pin box).
const END_MARK: i32 = 2_500;

/// First coordinate field and the form of a record vertex editing understands: a wire's or
/// line's two ends (`line`), a rectangle (`rect`), a polygon (`poly`).
fn form_of(rec: &RecordBuf) -> Option<(&str, usize)> {
    match rec.kind {
        RecordKind::Wire => Some(("line", 1)),
        RecordKind::Line => Some(("line", 0)),
        RecordKind::Rect => Some(("rect", 0)),
        RecordKind::Poly => Some(("poly", 0)),
        RecordKind::Shape => match rec.fields.get(1).map(String::as_str) {
            Some(f @ ("line" | "rect" | "poly")) => Some((f, 2)),
            _ => None,
        },
        _ => None,
    }
}

/// The vertices vertex editing moves, in field order: a wire's or line's two ends, a
/// rectangle's four corners (`(x1, y1)`, `(x2, y1)`, `(x2, y2)`, `(x1, y2)`), a polygon's
/// points. Empty for other kinds (arcs and circles have none) and for unparsable fields.
fn vertices(rec: &RecordBuf) -> Vec<Pt> {
    let Some((form, first)) = form_of(rec) else {
        return Vec::new();
    };
    let nums: Option<Vec<i32>> = rec
        .fields
        .get(first..)
        .unwrap_or(&[])
        .iter()
        .map(|t| parse_coord(t).ok())
        .collect();
    let Some(n) = nums.filter(|n| n.len() >= 4 && n.len().is_multiple_of(2)) else {
        return Vec::new();
    };
    match form {
        "rect" => vec![(n[0], n[1]), (n[2], n[1]), (n[2], n[3]), (n[0], n[3])],
        "line" => vec![(n[0], n[1]), (n[2], n[3])],
        _ => n.chunks_exact(2).map(|c| (c[0], c[1])).collect(),
    }
}

/// `rec` with vertex `i` (see [`vertices`]) moved to `p`: a rectangle corner moves its two
/// edges; the shared first and last point of a closed polygon move together. `None` when `i`
/// is not a vertex or the result has no extent (a wire or line of zero length, a rectangle
/// without area, two equal neighbouring polygon points).
fn with_vertex(rec: &RecordBuf, i: usize, p: Pt) -> Option<RecordBuf> {
    let (form, first) = form_of(rec)?;
    let mut v = vertices(rec);
    if i >= v.len() {
        return None;
    }
    let mut out = rec.clone();
    let mut set = |k: usize, val: i32| {
        let slot = &mut out.fields[first + k];
        slot.clear();
        format_coord(val, slot);
    };
    if form == "rect" {
        // Corner i owns x1 (corners 0, 3) or x2 (1, 2), and y1 (0, 1) or y2 (2, 3).
        set(if i == 0 || i == 3 { 0 } else { 2 }, p.0);
        set(if i <= 1 { 1 } else { 3 }, p.1);
        let q = vertices(&out);
        return (q[0].0 != q[2].0 && q[0].1 != q[2].1).then_some(out);
    }
    let last = v.len() - 1;
    let closed_loop = form == "poly" && last >= 3 && v[0] == v[last];
    for k in [
        i,
        if closed_loop && i == 0 { last } else { i },
        if closed_loop && i == last { 0 } else { i },
    ] {
        set(2 * k, p.0);
        set(2 * k + 1, p.1);
        v[k] = p;
    }
    (!v.windows(2).any(|w| w[0] == w[1])).then_some(out)
}

impl Editor {
    /// The active level's partially selected wire ends (empty without a level).
    pub(super) fn partial_ends(&self) -> &[(ObjKey, Pt)] {
        self.session.level().map_or(&[], |l| &l.ends)
    }

    /// Replaces the active level's partial wire selection with `ends` (rows of other tables
    /// dropped, sorted, distinct).
    pub(super) fn set_partial(&mut self, mut ends: Vec<(ObjKey, Pt)>) {
        ends.retain(|e| e.0.table == TableId::Wires);
        ends.sort_unstable();
        ends.dedup();
        if let Some(l) = self.session.level_mut() {
            l.ends = ends;
        }
        debug_assert!(self.partial_ends().windows(2).all(|w| w[0] < w[1]));
    }

    /// The partial selection after a box from `from` to `to` (world): with `partial`
    /// (Ctrl+drag) the inside end of every wire crossing the box edge joins it (`keep`:
    /// added to the rows there, else replacing them); `unselect` (Alt+drag) drops the rows
    /// inside the box; a plain box that keeps nothing clears it.
    pub(super) fn box_partial(
        &mut self,
        from: (f64, f64),
        to: (f64, f64),
        partial: bool,
        keep: bool,
        unselect: bool,
    ) {
        let (x0, y0, x1, y1) = (
            from.0.min(to.0),
            from.1.min(to.1),
            from.0.max(to.0),
            from.1.max(to.1),
        );
        let inside = |p: Pt| {
            let (x, y) = (f64::from(p.0), f64::from(p.1));
            (x0..=x1).contains(&x) && (y0..=y1).contains(&y)
        };
        let mut ends: Vec<(ObjKey, Pt)> = if keep || unselect {
            self.partial_ends().to_vec()
        } else {
            Vec::new()
        };
        if unselect {
            ends.retain(|e| !inside(e.1));
        } else if partial {
            let sel = self.selection();
            let objs = &self.scene.objs;
            for i in 0..objs.len() {
                let k = objs.key[i];
                if k.table != TableId::Wires || sel.binary_search(&k).is_ok() {
                    continue;
                }
                let g = objs.geom[i];
                let (a, b) = ((g[0], g[1]), (g[2], g[3]));
                match (inside(a), inside(b)) {
                    (true, false) => ends.push((k, a)),
                    (false, true) => ends.push((k, b)),
                    _ => {} // fully inside: selected whole; outside: not selected
                }
            }
        }
        self.set_partial(ends);
    }

    /// A left press with nothing in progress on a vertex of the one selected wire, line,
    /// rectangle or polygon (within the hit tolerance) starts dragging it (`Tool::Vertex`).
    /// Returns whether it did.
    pub(super) fn vertex_press(&mut self, s: (f32, f32)) -> bool {
        let &[key] = self.selection() else {
            return false;
        };
        let Some((h, _)) = self.active() else {
            return false;
        };
        let Some(rec) = self
            .session
            .project
            .doc(h)
            .and_then(|d| edit::loc_of(d, key).map(|l| d.record(l)))
        else {
            return false;
        };
        let pts = vertices(&rec);
        let tol = HIT_PX / self.camera().scale;
        let raw = self.world_at(s);
        let near =
            |p: &Pt| (f64::from(p.0) - raw.0).abs() <= tol && (f64::from(p.1) - raw.1).abs() <= tol;
        let Some(index) = pts.iter().position(near) else {
            return false;
        };
        let at = pts[index];
        self.tool = Tool::Vertex {
            key,
            index: index as u32,
            from: at,
            to: at,
        };
        assert!(index < pts.len());
        true
    }

    /// Ends a vertex drag: one `put` of the record with vertex `index` at `to` ("move
    /// vertex", one undo); nothing when it did not move, a status line when the shape would
    /// lose its extent.
    pub(super) fn finish_vertex(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        key: ObjKey,
        index: u32,
        to: Pt,
    ) {
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let Some((_, target, rec)) = edit::selected_records(doc, &[key]).into_iter().next() else {
            return;
        };
        let i = index as usize;
        if vertices(&rec).get(i) == Some(&to) {
            return; // dropped where it was: a click
        }
        let Some(moved) = with_vertex(&rec, i, to) else {
            self.session.status = "that vertex move leaves the shape without extent".into();
            return;
        };
        let wire = rec.kind == RecordKind::Wire;
        self.commit(reqs, h, "move vertex", vec![Op::Put { target, rec: moved }]);
        if wire && self.opts.auto_trim {
            self.trim_after = Some(h);
        }
    }

    /// Overlay of these gestures: a mark on every partially selected wire end, the partial
    /// wires stretched by a move in progress, and the shape of a vertex drag.
    pub(super) fn gesture_overlay(&self, o: &mut CanvasPrims) {
        let ends = self.partial_ends();
        let geom = |k: ObjKey| self.scene.find(k).map(|i| self.scene.objs.geom[i as usize]);
        let moving = match &self.tool {
            Tool::Move { from, to, lin, .. } if lin.is_translation() => {
                Some(lin.then(Xform::translate(to.0 - from.0, to.1 - from.1)))
            }
            _ => None,
        };
        for &(k, p) in ends {
            let Some(g) = geom(k) else { continue };
            let (a, b) = ((g[0], g[1]), (g[2], g[3]));
            let fixed = if p == a {
                b
            } else if p == b {
                a
            } else {
                continue; // stale row: selects nothing
            };
            let q = moving.map_or(p, |x| x.point(p));
            if q != p {
                o.line(fixed.0, fixed.1, q.0, q.1, LAYER_SEL);
            }
            o.rect(
                q.0 - END_MARK,
                q.1 - END_MARK,
                q.0 + END_MARK,
                q.1 + END_MARK,
                LAYER_SEL,
            );
        }
        if let Tool::Vertex { key, index, to, .. } = &self.tool {
            let rec = self.active().and_then(|(h, _)| {
                let d = self.session.project.doc(h)?;
                let l = edit::loc_of(d, *key)?;
                with_vertex(&d.record(l), *index as usize, *to)
            });
            if let Some(rec) = rec {
                let v = vertices(&rec);
                let closed = form_of(&rec).is_some_and(|f| f.0 == "rect")
                    || rec
                        .find_attr("closed")
                        .is_some_and(|a| a.values.first().is_some_and(|x| x == "true"));
                o.poly(&v, closed, LAYER_SEL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: RecordKind, fields: &[&str]) -> RecordBuf {
        fields.iter().fold(RecordBuf::new(kind), |r, f| r.field(f))
    }

    #[test]
    fn vertices_of_each_editable_kind() {
        let wire = rec(RecordKind::Wire, &["w1", "0", "0", "10", "0"]);
        assert_eq!(vertices(&wire), [(0, 0), (10_000, 0)]);
        let r = rec(RecordKind::Rect, &["0", "0", "10", "20"]);
        assert_eq!(
            vertices(&r),
            [(0, 0), (10_000, 0), (10_000, 20_000), (0, 20_000)]
        );
        let shape = rec(
            RecordKind::Shape,
            &["s1", "poly", "0", "0", "5", "5", "0", "5"],
        );
        assert_eq!(vertices(&shape), [(0, 0), (5_000, 5_000), (0, 5_000)]);
        // No vertices: arcs, circles, labels.
        let circle = rec(RecordKind::Shape, &["s2", "circle", "0", "0", "5"]);
        assert!(vertices(&circle).is_empty());
        assert!(vertices(&rec(RecordKind::Circle, &["0", "0", "5"])).is_empty());
        let label = rec(RecordKind::Label, &["l1", "0", "0", "R0", "a"]);
        assert!(vertices(&label).is_empty());
    }

    #[test]
    fn moving_a_vertex_changes_only_its_coordinates() {
        let wire = rec(RecordKind::Wire, &["w1", "0", "0", "10", "0"]);
        let w = with_vertex(&wire, 1, (10_000, 30_000)).unwrap();
        assert_eq!(w.fields, ["w1", "0", "0", "10", "30"]);
        assert!(with_vertex(&wire, 1, (0, 0)).is_none(), "zero length");
        assert!(with_vertex(&wire, 2, (5, 5)).is_none(), "no third vertex");
        // A rectangle corner moves its two edges; the opposite corner stays.
        let r = rec(RecordKind::Rect, &["0", "0", "10", "20"]);
        let m = with_vertex(&r, 2, (30_000, 40_000)).unwrap();
        assert_eq!(m.fields, ["0", "0", "30", "40"]);
        let m = with_vertex(&r, 3, (-5_000, 25_000)).unwrap();
        assert_eq!(m.fields, ["-5", "0", "10", "25"]);
        assert!(with_vertex(&r, 1, (0, 5_000)).is_none(), "no width");
        // A closed polygon's shared first and last point moves as one.
        let poly = rec(
            RecordKind::Poly,
            &["0", "0", "10", "0", "10", "10", "0", "0"],
        );
        let m = with_vertex(&poly, 0, (-5_000, 0)).unwrap();
        assert_eq!(m.fields, ["-5", "0", "10", "0", "10", "10", "-5", "0"]);
        let repeated = with_vertex(&poly, 1, (10_000, 10_000));
        assert!(repeated.is_none(), "two equal neighbouring points");
    }
}
