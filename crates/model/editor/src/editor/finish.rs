//! Gesture ends as edit requests: wires, moves, stretches, placements, shapes, cuts.
//!
//! Contents:
//! - `pub(super) fn dist`
//! - `pub(super) fn arc_3`
//! - `Editor::manhattan_points`
//! - `Editor::finish_wire`
//! - `Editor::finish_move`
//! - `Editor::finish_place`
//! - `Editor::shape_click`
//! - `Editor::finish_shape`
//! - `Editor::auto_trim`
//! - `Editor::cut_wire`

use schemify_doc::{DocHandle, EditRequest, Op, RecordBuf, TableId};
use schemify_format::{FileKind, Orient, RecordKind};

use crate::commands::Cmd;
use crate::edit::{FreshId, Ids};
use crate::geometry::{Pt, Xform};
use crate::scene::ObjKey;
use crate::session::View;
use crate::{edit, wiring};

use super::Editor;
use super::tool::{Form, Placement, Tool};

pub(super) fn dist(a: Pt, b: Pt) -> i32 {
    let (dx, dy) = (
        f64::from(b.0) - f64::from(a.0),
        f64::from(b.1) - f64::from(a.1),
    );
    (dx.hypot(dy).round() as i64).clamp(0, i64::from(i32::MAX)) as i32
}

/// The arc from `a` to `b` through `c` (xschem's 3-point arc): center, radius (milli-units)
/// and start/sweep in milli-degrees, clockwise on screen, sweep in `(0, 360000)`. `None` for
/// collinear or repeated points.
pub(super) fn arc_3(a: Pt, b: Pt, c: Pt) -> Option<(i32, i32, i32, i32, i32)> {
    let (ax, ay) = (f64::from(a.0), f64::from(a.1));
    let (bx, by) = (f64::from(b.0), f64::from(b.1));
    let (cx, cy) = (f64::from(c.0), f64::from(c.1));
    let d = 2.0 * (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by));
    if d.abs() < 1e-6 {
        return None;
    }
    let a2 = ax * ax + ay * ay;
    let b2 = bx * bx + by * by;
    let c2 = cx * cx + cy * cy;
    let ux = (a2 * (by - cy) + b2 * (cy - ay) + c2 * (ay - by)) / d;
    let uy = (a2 * (cx - bx) + b2 * (ax - cx) + c2 * (bx - ax)) / d;
    let r = (ax - ux).hypot(ay - uy);
    let ang = |x: f64, y: f64| ((y - uy).atan2(x - ux).to_degrees() * 1000.0).round();
    let (ta, tb, tc) = (ang(ax, ay), ang(bx, by), ang(cx, cy));
    let cw = |from: f64, to: f64| (to - from).rem_euclid(360_000.0);
    let (start, sweep) = if cw(ta, tc) <= cw(ta, tb) {
        (ta, cw(ta, tb)) // clockwise from a passes c before b
    } else {
        (tb, cw(tb, ta)) // the arc from b clockwise to a holds c
    };
    let fits = |v: f64| v.abs() < f64::from(i32::MAX);
    if !(fits(ux) && fits(uy) && fits(r)) || sweep <= 0.0 {
        return None;
    }
    Some((
        ux.round() as i32,
        uy.round() as i32,
        r.round() as i32,
        start.rem_euclid(360_000.0) as i32,
        sweep as i32,
    ))
}

impl Editor {
    pub(super) fn manhattan_points(&self, from: Pt, to: Pt) -> Vec<Pt> {
        let mode = if self.opts.orthogonal {
            if (to.0 - from.0).abs() >= (to.1 - from.1).abs() {
                1
            } else {
                2
            }
        } else {
            self.opts.manhattan
        };
        match mode {
            1 => vec![from, (to.0, from.1), to],
            2 => vec![from, (from.0, to.1), to],
            _ => vec![from, to],
        }
    }
    pub(super) fn finish_wire(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        from: Pt,
        to: Pt,
    ) {
        let pts = self.manhattan_points(from, to);
        let ops = {
            let Some(doc) = self.session.project.doc(h) else {
                return;
            };
            let mut ids = Ids::new(&mut self.rng);
            edit::wires(doc, &pts, &mut ids).ops
        };
        self.opts.constrain = 0;
        self.commit(reqs, h, "draw wire", ops);
        if self.opts.auto_trim {
            self.trim_after = Some(h);
        }
        if self.opts.persistent && from != to {
            // Options → persistent wire: the next segment starts where this one ended.
            self.tool = Tool::Wire {
                from: to,
                to,
                drag: false,
            };
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish_move(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        from: Pt,
        to: Pt,
        lin: Xform,
        copy: bool,
        stretch: u8,
    ) {
        let x = lin.then(Xform::translate(to.0 - from.0, to.1 - from.1));
        self.opts.constrain = 0;
        if x == Xform::IDENTITY && !copy {
            return;
        }
        if !copy && (stretch > 0 || !self.partial_ends().is_empty()) {
            self.finish_stretch(reqs, h, x, stretch);
            if self.opts.auto_trim {
                self.trim_after = Some(h);
            }
            return;
        }
        if copy {
            let edit::Inserted { ops, keys } = {
                let Some(doc) = self.session.project.doc(h) else {
                    return;
                };
                let sel = self.session.level().map_or(&[][..], |l| &l.sel);
                let mut ids = Ids::new(&mut self.rng);
                edit::duplicate(doc, sel, x, &mut ids)
            };
            self.commit(reqs, h, "copy", ops);
            if !keys.is_empty() {
                self.set_selection(keys);
            }
        } else {
            let ops = self.with_doc(h, |doc, sel| edit::transform(doc, sel, x));
            self.commit(reqs, h, "move", ops);
        }
    }
    pub(super) fn finish_place(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        view: View,
        what: Placement,
        at: Pt,
        lin: Xform,
    ) {
        let mut err = None;
        let (label, ops, keys) = {
            let p = &self.session.project;
            let Some(doc) = p.doc(h) else { return };
            let schematic = view == View::Schematic && doc.kind() != FileKind::Shprim;
            let mut ids = Ids::new(&mut self.rng);
            let mut one = |kind: RecordKind, table: TableId, o: Orient, text: &str| {
                let FreshId { text: id, key: raw } = ids.draw(doc);
                let rec = edit::placed(kind, at, o, text, &id);
                (
                    vec![Op::Insert { rec, at: None }],
                    vec![ObjKey { table, key: raw }],
                )
            };
            match what {
                Placement::Inst(cellref) if schematic => {
                    let prefix = p
                        .resolve(h, &cellref)
                        .ok()
                        .and_then(|r| p.interface(r))
                        .and_then(|i| i.header_value("prefix").map(str::to_owned))
                        .unwrap_or_else(|| "X".to_owned());
                    let name = edit::unique_name(doc, &prefix, &mut Default::default());
                    let FreshId { text: id, key: raw } = ids.draw(doc);
                    let rec = edit::instance(&cellref, at, lin.orient(Orient::R0), &name, &id);
                    let key = ObjKey {
                        table: TableId::Instances,
                        key: raw,
                    };
                    (
                        "place instance",
                        vec![Op::Insert { rec, at: None }],
                        vec![key],
                    )
                }
                Placement::Label(t) if schematic => {
                    let (o, k) = one(
                        RecordKind::Label,
                        TableId::Labels,
                        lin.orient(Orient::R0),
                        &t,
                    );
                    ("place label", o, k)
                }
                Placement::Port(t, out) if schematic => {
                    let base = if out { Orient::R180 } else { Orient::R0 };
                    let (o, k) = one(RecordKind::Port, TableId::Ports, lin.orient(base), &t);
                    ("place port", o, k)
                }
                Placement::Text(t) if schematic => {
                    let (o, k) = one(RecordKind::Text, TableId::Texts, lin.orient(Orient::R0), &t);
                    ("place text", o, k)
                }
                Placement::SymPin(name) if view == View::Symbol => {
                    let rec = edit::pin(&name, at);
                    ("place pin", vec![Op::Insert { rec, at: None }], Vec::new())
                }
                Placement::Paste(recs) => {
                    let x = lin.then(Xform::translate(at.0, at.1));
                    let copies = edit::insert_copies(doc, recs, x, &mut ids);
                    ("paste", copies.ops, copies.keys)
                }
                _ => {
                    err = Some("that object does not belong in this view");
                    ("", Vec::new(), Vec::new())
                }
            }
        };
        if let Some(e) = err {
            self.session.say(e);
            return;
        }
        self.commit(reqs, h, label, ops);
        if !keys.is_empty() {
            self.set_selection(keys);
        }
    }
    /// A click while a shape tool is active: adds the point, closes a polygon on its first
    /// vertex, finishes an open polygon on its last vertex, and commits once the shape has
    /// its points. Persistent mode chains lines.
    pub(super) fn shape_click(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        view: View,
        form: Form,
        mut pts: Vec<Pt>,
        p: Pt,
    ) {
        assert!(!pts.is_empty(), "a shape tool starts with its first point");
        if form == Form::Poly {
            if pts.len() >= 3 && p == pts[0] {
                self.finish_shape(reqs, h, view, form, &pts, true);
                return;
            }
            if pts.len() >= 2 && Some(&p) == pts.last() {
                self.finish_shape(reqs, h, view, form, &pts, false);
                return;
            }
        }
        if Some(&p) != pts.last() {
            pts.push(p);
        }
        if form.points() == Some(pts.len()) {
            self.finish_shape(reqs, h, view, form, &pts, false);
            if form == Form::Line && self.opts.persistent {
                self.tool = Tool::Shape {
                    form,
                    pts: vec![p],
                    to: p,
                };
            }
        } else {
            self.tool = Tool::Shape { form, pts, to: p };
        }
        self.opts.constrain = 0;
    }
    /// Commits a drawn shape: symbol graphics in the symbol view, a `shape` record in a
    /// schematic.
    pub(super) fn finish_shape(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        view: View,
        form: Form,
        pts: &[Pt],
        closed: bool,
    ) {
        self.tool = Tool::Idle;
        let schematic = view == View::Schematic;
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        if schematic && doc.kind() == FileKind::Shprim {
            self.session
                .say("a primitive has no schematic: draw in its symbol");
            return;
        }
        let mut ids = Ids::new(&mut self.rng);
        let (id, raw) = if schematic {
            let FreshId { text: t, key: r } = ids.draw(doc);
            (Some(t), Some(r))
        } else {
            (None, None)
        };
        let Some(rec) = shape_record(form, pts, closed, self.opts.layer, id.as_deref()) else {
            self.session.say("that shape has no extent");
            return;
        };
        let label = match form {
            Form::Line => "draw line",
            Form::Rect => "draw rect",
            Form::Poly => "draw polygon",
            Form::Arc => "draw arc",
            Form::Circle => "draw circle",
        };
        self.commit(reqs, h, label, vec![Op::Insert { rec, at: None }]);
        if let Some(raw) = raw {
            self.set_selection(vec![ObjKey {
                table: TableId::Shapes,
                key: raw,
            }]);
        }
    }
    /// Ends a move: a plain move, or a stretch (`stretch` 1) / kissing (2) move whose
    /// attached wires follow (translations only; a move rotated in flight is plain). The
    /// selected ends of partially selected wires (Ctrl+drag box) move with any translation.
    fn finish_stretch(&mut self, reqs: &mut Vec<EditRequest>, h: DocHandle, x: Xform, stretch: u8) {
        let sel = self.selection().to_vec();
        let ends: Vec<(ObjKey, Pt)> = self
            .partial_ends()
            .iter()
            .copied()
            .filter(|e| sel.binary_search(&e.0).is_err())
            .collect();
        if !x.is_translation() || (stretch == 0 && ends.is_empty()) {
            let ops = self.with_doc(h, |doc, sel| edit::transform(doc, sel, x));
            self.commit(reqs, h, "move", ops);
            return;
        }
        let manhattan = self.opts.manhattan;
        let out = {
            let mut ids = Ids::new(&mut self.rng);
            wiring::stretch(
                &self.session.project,
                h,
                wiring::Moved {
                    sel: &sel,
                    ends: &ends,
                    follow: stretch > 0,
                    kiss: stretch == 2,
                },
                x,
                manhattan,
                &mut ids,
            )
        };
        let label = match stretch {
            2 => "kissing move",
            1 => "stretch move",
            _ => "move",
        };
        // The selection, whole and partial, stays what was moved (xschem's default: partially
        // selected wires stay selected after a stretch move); partial ends follow their move.
        self.commit(reqs, h, label, out.ops);
        let moved: Vec<(ObjKey, Pt)> = ends.iter().map(|&(k, p)| (k, x.point(p))).collect();
        self.set_partial(moved);
        self.session
            .say(format!("{label}: {} wire(s) followed", out.wires.len()));
    }
    /// Spine step after `doc::apply_edits`: when a wire edit asked for it (Options → auto
    /// join/trim), joins and trims that document's wires as its own transaction (trim reads
    /// the applied tables, so it cannot share the wire edit's). Returns whether it applied.
    pub(super) fn auto_trim(&mut self, reqs: &mut Vec<EditRequest>) -> bool {
        let Some(h) = self.trim_after.take() else {
            return false;
        };
        let ops = {
            let mut ids = Ids::new(&mut self.rng);
            wiring::join_trim(&self.session.project, h, &mut ids)
        };
        if ops.is_empty() {
            return false;
        }
        self.commit(reqs, h, "join/trim wires", ops);
        self.apply(reqs)
    }
    /// Tools → Break wires at mouse position: cuts the wire under the pointer there (snapped
    /// with the align variant, else the pointer projected onto an axis-aligned wire).
    pub(super) fn cut_wire(&mut self, reqs: &mut Vec<EditRequest>, h: DocHandle, cmd: Cmd) {
        let Some(o) = self.hit_at(self.pointer) else {
            self.session.say("no wire under the pointer");
            return;
        };
        let key = self.scene.objs.key[o as usize];
        if key.table != TableId::Wires {
            self.session.say("no wire under the pointer");
            return;
        }
        let g = self.scene.objs.geom[o as usize];
        let p = if cmd == Cmd::BreakAtMouseAlign {
            self.pointer_world_snapped()
        } else {
            let (x, y) = self.pointer_world();
            (x.round() as i32, y.round() as i32)
        };
        // Project onto an axis-aligned wire so the cut lies on it.
        let p = if g[1] == g[3] {
            (p.0, g[1])
        } else if g[0] == g[2] {
            (g[0], p.1)
        } else {
            p
        };
        let ops = {
            let Some(doc) = self.session.project.doc(h) else {
                return;
            };
            let mut ids = Ids::new(&mut self.rng);
            wiring::break_at(doc, &[p], &mut ids).0
        };
        if ops.is_empty() {
            self.session.say("the cut point is not inside the wire");
        }
        self.commit(reqs, h, "cut wire", ops);
    }
}

// ---- private ----

/// One shape's points as a record: rank-2 symbol graphics (`schematic == false`) or a rank-3
/// `shape` with id `id`. `None` when the points do not make the shape (a degenerate rect, a
/// polygon under two points, an arc through collinear points).
fn shape_record(
    form: Form,
    pts: &[Pt],
    closed: bool,
    layer: u8,
    id: Option<&str>,
) -> Option<RecordBuf> {
    let c = crate::geometry::coord;
    let (word, values): (&str, Vec<i32>) = match form {
        Form::Line | Form::Rect => {
            let (a, b) = (*pts.first()?, *pts.get(1)?);
            if a == b || (form == Form::Rect && (a.0 == b.0 || a.1 == b.1)) {
                return None;
            }
            let w = if form == Form::Line { "line" } else { "rect" };
            (w, vec![a.0, a.1, b.0, b.1])
        }
        Form::Circle => {
            let (m, p) = (*pts.first()?, *pts.get(1)?);
            let r = dist(m, p);
            if r == 0 {
                return None;
            }
            ("circle", vec![m.0, m.1, r])
        }
        Form::Arc => {
            let (cx, cy, r, start, sweep) = arc_3(*pts.first()?, *pts.get(1)?, *pts.get(2)?)?;
            ("arc", vec![cx, cy, r, start, sweep])
        }
        Form::Poly => {
            if pts.len() < 2 || (closed && pts.len() < 3) {
                return None;
            }
            ("poly", pts.iter().flat_map(|p| [p.0, p.1]).collect())
        }
    };
    let mut rec = match id {
        Some(id) => RecordBuf::new(RecordKind::Shape).field(id).field(word),
        None => RecordBuf::new(match word {
            "line" => RecordKind::Line,
            "rect" => RecordKind::Rect,
            "circle" => RecordKind::Circle,
            "arc" => RecordKind::Arc,
            _ => RecordKind::Poly,
        }),
    };
    // Rank-2 arc angles are file numbers, everything else (and every rank-3 value) is a
    // coordinate: both print as the shortest decimal of the milli value.
    for v in &values {
        rec = rec.field(&c(*v));
    }
    if layer != 4 {
        rec = rec.attr("layer", &[&layer.to_string()]);
    }
    if form == Form::Poly && closed {
        rec = rec.attr("closed", &["true"]);
    }
    Some(rec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_point_arcs_pass_through_their_points() {
        // A quarter circle of radius 10 around the origin, clockwise on screen from +x to +y.
        let (cx, cy, r, start, sweep) = arc_3((10_000, 0), (0, 10_000), (6_000, 8_000)).unwrap();
        assert_eq!((cx, cy, r), (0, 0, 10_000));
        assert_eq!((start, sweep), (0, 90_000));
        // The same points with the middle one on the other side: the long way round.
        let (_, _, _, s2, w2) = arc_3((10_000, 0), (0, 10_000), (-10_000, 0)).unwrap();
        assert_eq!((s2, w2), (90_000, 270_000));
        assert!(arc_3((0, 0), (1, 1), (2, 2)).is_none(), "collinear");
    }

    #[test]
    fn shapes_need_extent() {
        assert!(shape_record(Form::Rect, &[(0, 0), (0, 10)], false, 4, Some("abcdef")).is_none());
        let r = shape_record(
            Form::Poly,
            &[(0, 0), (10, 0), (10, 10)],
            true,
            7,
            Some("abcdef"),
        )
        .unwrap();
        assert_eq!(
            r.fields,
            ["abcdef", "poly", "0", "0", "0.01", "0", "0.01", "0.01"]
        );
        assert_eq!(r.find_attr("layer").unwrap().values, ["7"]);
        assert!(r.find_attr("closed").is_some());
        let sym = shape_record(Form::Circle, &[(0, 0), (3_000, 4_000)], false, 4, None).unwrap();
        assert_eq!(sym.kind, RecordKind::Circle);
        assert_eq!(sym.fields, ["0", "0", "5"]);
    }
}
