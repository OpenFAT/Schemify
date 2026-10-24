//! Plane geometry of the editor: the eight grid symmetries plus translation ([`Xform`]),
//! applied to orientations, arc angles and to the coordinate fields of any design record
//! ([`transform_record`]). One record transform serves move, copy, rotate, flip and paste
//! for every record kind, so a new kind is one match arm here.
//!
//! Coordinates are `i32` milli-units, y down (D33, format spec §3).

use schemify_doc::RecordBuf;
use schemify_format::{Orient, RecordKind, format_coord, parse_coord};

/// A point in milli-units.
pub type Pt = (i32, i32);

/// The 8 signed permutation matrices `[a, b, c, d]` (`x' = a·x + b·y`, `y' = c·x + d·y`) of
/// `Orient` in discriminant order: `R^k · M^m` with `M: (x, y) → (−x, y)` applied first and
/// `R: (x, y) → (−y, x)` a clockwise quarter turn on screen (format spec §3).
const ORIENT_MATRIX: [[i8; 4]; 8] = [
    [1, 0, 0, 1],   // R0
    [0, -1, 1, 0],  // R90
    [-1, 0, 0, -1], // R180
    [0, 1, -1, 0],  // R270
    [-1, 0, 0, 1],  // MR0
    [0, -1, -1, 0], // MR90
    [1, 0, 0, -1],  // MR180
    [0, 1, 1, 0],   // MR270
];

const ORIENTS: [Orient; 8] = [
    Orient::R0,
    Orient::R90,
    Orient::R180,
    Orient::R270,
    Orient::MR0,
    Orient::MR90,
    Orient::MR180,
    Orient::MR270,
];

fn mat_mul(a: [i8; 4], b: [i8; 4]) -> [i8; 4] {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
    ]
}

/// Returns the orientation whose matrix is `m`.
///
/// # Panics
///
/// Panics if `m` is not one of the eight grid symmetries (a caller bug: products of grid
/// symmetries always are).
fn orient_of(m: [i8; 4]) -> Orient {
    let i = ORIENT_MATRIX
        .iter()
        .position(|&x| x == m)
        .expect("grid symmetries are closed under products");
    assert_eq!(ORIENTS[i] as usize, i);
    ORIENTS[i]
}

/// Maps a symbol-local point through an instance placement (format spec §3).
pub fn place(o: Orient, at: Pt, p: Pt) -> Pt {
    let m = ORIENT_MATRIX[o as usize];
    let (x, y) = (i64::from(p.0), i64::from(p.1));
    let q = (
        i64::from(m[0]) * x + i64::from(m[1]) * y + i64::from(at.0),
        i64::from(m[2]) * x + i64::from(m[3]) * y + i64::from(at.1),
    );
    (sat(q.0), sat(q.1))
}

/// The orientation of something placed with `inner` inside a symbol placed with `outer`
/// (a symbol text in an instance): `place(compose(outer, inner), at, p)` equals
/// `place(outer, at, place(inner, (0, 0), p))`.
pub fn compose(outer: Orient, inner: Orient) -> Orient {
    orient_of(mat_mul(
        ORIENT_MATRIX[outer as usize],
        ORIENT_MATRIX[inner as usize],
    ))
}

fn sat(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// `p' = A·p + t` with `A` one of the eight grid symmetries: what a move, rotate, flip or
/// paste does to every coordinate it touches. Composition stays in the group, so a gesture
/// that rotates while moving is still one `Xform` and one transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Xform {
    m: [i8; 4],
    t: (i64, i64),
}

impl Xform {
    /// The identity.
    pub const IDENTITY: Xform = Xform {
        m: [1, 0, 0, 1],
        t: (0, 0),
    };

    /// A translation by `(dx, dy)`.
    pub fn translate(dx: i32, dy: i32) -> Xform {
        Xform {
            m: [1, 0, 0, 1],
            t: (i64::from(dx), i64::from(dy)),
        }
    }

    /// A clockwise quarter turn (on screen) about `pivot`.
    pub fn rotate(pivot: Pt) -> Xform {
        Xform::about(ORIENT_MATRIX[Orient::R90 as usize], pivot)
    }

    /// A horizontal flip (`x → 2·pivot.x − x`).
    pub fn flip_x(pivot: Pt) -> Xform {
        Xform::about(ORIENT_MATRIX[Orient::MR0 as usize], pivot)
    }

    /// A vertical flip (`y → 2·pivot.y − y`); xschem's "vertical flip" is rotate 180 + flip.
    pub fn flip_y(pivot: Pt) -> Xform {
        Xform::about(ORIENT_MATRIX[Orient::MR180 as usize], pivot)
    }

    fn about(m: [i8; 4], c: Pt) -> Xform {
        let (cx, cy) = (i64::from(c.0), i64::from(c.1));
        // p' = M (p − c) + c = M p + (c − M c)
        let mc = (
            i64::from(m[0]) * cx + i64::from(m[1]) * cy,
            i64::from(m[2]) * cx + i64::from(m[3]) * cy,
        );
        Xform {
            m,
            t: (cx - mc.0, cy - mc.1),
        }
    }

    /// Returns `other ∘ self` (apply `self` first).
    pub fn then(self, other: Xform) -> Xform {
        let o = other.m;
        let t = (
            i64::from(o[0]) * self.t.0 + i64::from(o[1]) * self.t.1 + other.t.0,
            i64::from(o[2]) * self.t.0 + i64::from(o[3]) * self.t.1 + other.t.1,
        );
        Xform {
            m: mat_mul(o, self.m),
            t,
        }
    }

    /// Returns whether this is a pure translation.
    pub fn is_translation(self) -> bool {
        self.m == [1, 0, 0, 1]
    }

    /// Maps a point (saturating at the i32 range; the doc layer rejects out-of-range records).
    pub fn point(self, p: Pt) -> Pt {
        let (x, y) = (i64::from(p.0), i64::from(p.1));
        (
            sat(i64::from(self.m[0]) * x + i64::from(self.m[1]) * y + self.t.0),
            sat(i64::from(self.m[2]) * x + i64::from(self.m[3]) * y + self.t.1),
        )
    }

    /// Composes an object orientation with this transform's linear part.
    pub fn orient(self, o: Orient) -> Orient {
        orient_of(mat_mul(self.m, ORIENT_MATRIX[o as usize]))
    }

    /// Maps an arc `(start, sweep)` in degrees (clockwise on screen, like the renderer):
    /// a mirror reverses direction, so the result keeps the same sweep and a new start.
    pub fn arc(self, start: f64, sweep: f64) -> (f64, f64) {
        let o = orient_of(self.m);
        let (mut a, s) = (start, sweep);
        if o.mirrored() {
            a = 180.0 - a - s;
        }
        a += 90.0 * f64::from(o.quarter_turns());
        (a.rem_euclid(360.0), s)
    }
}

fn coord(s: &str) -> Option<i32> {
    parse_coord(s).ok()
}

fn set_coord(slot: &mut String, v: i32) {
    slot.clear();
    format_coord(v, slot);
}

/// Maps the coordinate pair at fields `i, i + 1`.
fn map_pair(fields: &mut [String], i: usize, x: Xform) -> Option<()> {
    let p = (coord(&fields[i])?, coord(&fields[i + 1])?);
    let q = x.point(p);
    set_coord(&mut fields[i], q.0);
    set_coord(&mut fields[i + 1], q.1);
    Some(())
}

fn map_orient(slot: &mut String, x: Xform) -> Option<()> {
    let o = Orient::parse(slot)?;
    *slot = x.orient(o).as_str().to_owned();
    Some(())
}

/// Shortest decimal text of an angle in `[0, 360)` with at most 3 fractional digits.
fn degrees(v: f64) -> String {
    let milli = ((v * 1000.0).round() as i64).rem_euclid(360_000);
    let mut s = String::new();
    format_coord(milli as i32, &mut s);
    s
}

/// Applies `x` to every coordinate, orientation and arc angle of `rec` (any record kind;
/// kinds without geometry are unchanged). Returns `None`, leaving `rec` partly mapped, when
/// a coordinate field does not parse; records read from a `doc::Document` always parse.
pub fn transform_record(rec: &mut RecordBuf, x: Xform) -> Option<()> {
    let f = &mut rec.fields;
    match rec.kind {
        RecordKind::Inst => {
            let at = rec.attrs.iter_mut().find(|a| a.key == "at")?;
            if at.values.len() != 3 {
                return None;
            }
            map_pair(&mut at.values, 0, x)?;
            map_orient(&mut at.values[2], x)?;
        }
        RecordKind::Wire => {
            map_pair(f, 1, x)?;
            map_pair(f, 3, x)?;
        }
        RecordKind::Label | RecordKind::Port | RecordKind::Text => {
            map_pair(f, 1, x)?;
            map_orient(&mut f[3], x)?;
        }
        RecordKind::Noconn => map_pair(f, 1, x)?,
        RecordKind::Pin => map_pair(f, 2, x)?,
        RecordKind::Line | RecordKind::Rect => {
            map_pair(f, 0, x)?;
            map_pair(f, 2, x)?;
        }
        RecordKind::Circle => map_pair(f, 0, x)?,
        RecordKind::Arc => {
            map_pair(f, 0, x)?;
            // The sweep is invariant (a mirror moves the start instead), so its text stays.
            let (a, s) = (f[3].parse::<f64>().ok()?, f[4].parse::<f64>().ok()?);
            f[3] = degrees(x.arc(a, s).0);
        }
        RecordKind::Poly => {
            for i in (0..f.len()).step_by(2) {
                if i + 1 < f.len() {
                    map_pair(f, i, x)?;
                }
            }
        }
        RecordKind::Stext => {
            map_pair(f, 0, x)?;
            map_orient(&mut f[2], x)?;
        }
        // `shape <id> <form> <values…>`: the rank-2 forms shifted by two fields.
        RecordKind::Shape => match f.get(1).map(String::as_str) {
            Some("arc") if f.len() == 7 => {
                map_pair(f, 2, x)?;
                let (a, s) = (coord(&f[5])?, coord(&f[6])?);
                let (a, _) = x.arc(f64::from(a) / 1000.0, f64::from(s) / 1000.0);
                f[5] = degrees(a);
            }
            Some("circle") if f.len() == 5 => map_pair(f, 2, x)?,
            Some("line" | "rect" | "poly") => {
                for i in (2..f.len()).step_by(2) {
                    if i + 1 < f.len() {
                        map_pair(f, i, x)?;
                    }
                }
            }
            _ => return None,
        },
        _ => {}
    }
    Some(())
}

/// Returns the anchor of a record: the point an in-place rotate or flip pivots on
/// (instance origin, label anchor, wire first endpoint, first graphic point).
pub fn anchor(rec: &RecordBuf) -> Option<Pt> {
    let f = &rec.fields;
    let pair = |i: usize| Some((coord(f.get(i)?)?, coord(f.get(i + 1)?)?));
    match rec.kind {
        RecordKind::Inst => {
            let at = rec.find_attr("at")?;
            Some((coord(at.values.first()?)?, coord(at.values.get(1)?)?))
        }
        RecordKind::Wire | RecordKind::Label | RecordKind::Port | RecordKind::Text => pair(1),
        RecordKind::Noconn => pair(1),
        RecordKind::Pin => pair(2),
        RecordKind::Line
        | RecordKind::Rect
        | RecordKind::Circle
        | RecordKind::Arc
        | RecordKind::Poly
        | RecordKind::Stext => pair(0),
        RecordKind::Shape => pair(2),
        _ => None,
    }
}

/// Rounds a world coordinate to the nearest multiple of `snap` (ties away from zero).
pub fn snap(v: f64, snap: i32) -> i32 {
    assert!(snap > 0, "snap must be positive");
    let s = f64::from(snap);
    let r = (v / s).round() * s;
    r.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orient_matrices_match_the_spec_transform() {
        // Spec §3: mirror first, then rotate clockwise.
        let p = (3, 7);
        for (i, &o) in ORIENTS.iter().enumerate() {
            let (mut x, mut y) = p;
            if o.mirrored() {
                x = -x;
            }
            for _ in 0..o.quarter_turns() {
                (x, y) = (-y, x);
            }
            assert_eq!(place(o, (0, 0), p), (x, y), "{}", o.as_str());
            assert_eq!(orient_of(ORIENT_MATRIX[i]), o);
        }
    }

    #[test]
    fn composed_orientations_place_like_nested_placements() {
        let at = (7, -3);
        for &o in &ORIENTS {
            for &i in &ORIENTS {
                for p in [(1, 2), (-4, 9), (0, -6)] {
                    assert_eq!(
                        place(compose(o, i), at, p),
                        place(o, at, place(i, (0, 0), p)),
                        "{} ∘ {}",
                        o.as_str(),
                        i.as_str()
                    );
                }
            }
            assert_eq!(compose(o, Orient::R0), o);
            assert_eq!(compose(Orient::R0, o), o);
        }
        assert_eq!(compose(Orient::R90, Orient::R90), Orient::R180);
        assert_eq!(compose(Orient::R270, Orient::R180).quarter_turns(), 1);
    }

    #[test]
    fn four_rotations_and_two_flips_are_identity() {
        let c = (10_000, -20_000);
        let r = Xform::rotate(c);
        let r4 = r.then(r).then(r).then(r);
        assert_eq!(r4, Xform::IDENTITY);
        let f = Xform::flip_x(c);
        assert_eq!(f.then(f), Xform::IDENTITY);
        assert_eq!(r.point(c), c, "pivot is fixed");
        assert_eq!(f.point((c.0 + 5, c.1)), (c.0 - 5, c.1));
        for &o in &ORIENTS {
            assert_eq!(r4.orient(o), o);
            assert_ne!(r.orient(o), o, "a quarter turn changes every orientation");
        }
    }

    #[test]
    fn orientation_composes_like_points() {
        // Placing a local point with the composed orientation equals transforming the placed
        // point: x(place(o, at, p)) == place(x.orient(o), x(at), p).
        let x = Xform::rotate((5, 5)).then(Xform::flip_x((-3, 2)));
        for &o in &ORIENTS {
            for p in [(1, 2), (-4, 9), (0, -6)] {
                let at = (11, -13);
                assert_eq!(x.point(place(o, at, p)), place(x.orient(o), x.point(at), p));
            }
        }
    }

    #[test]
    fn arcs_follow_their_endpoints() {
        let ends = |a: f64, s: f64| {
            let p = |t: f64| {
                let r = t.to_radians();
                (
                    (r.cos() * 1000.0).round() as i32,
                    (r.sin() * 1000.0).round() as i32,
                )
            };
            let (mut e1, mut e2) = (p(a), p(a + s));
            if e2 < e1 {
                std::mem::swap(&mut e1, &mut e2);
            }
            (e1, e2)
        };
        for x in [
            Xform::rotate((0, 0)),
            Xform::flip_x((0, 0)),
            Xform::flip_y((0, 0)),
        ] {
            let (a, s) = (30.0, 100.0);
            let (b, t) = x.arc(a, s);
            let (e1, e2) = ends(a, s);
            let (mut m1, mut m2) = (x.point(e1), x.point(e2));
            if m2 < m1 {
                std::mem::swap(&mut m1, &mut m2);
            }
            assert_eq!(ends(b, t), (m1, m2));
            assert_eq!(t, s);
        }
    }

    #[test]
    fn records_transform_every_geometry_kind() {
        let mut wire = RecordBuf::new(RecordKind::Wire)
            .field("ab")
            .field("0")
            .field("0")
            .field("10")
            .field("0");
        transform_record(&mut wire, Xform::rotate((0, 0))).unwrap();
        assert_eq!(wire.fields[1..], ["0", "0", "0", "10"]);
        let mut inst = RecordBuf::new(RecordKind::Inst)
            .field("cd")
            .field("lib:r")
            .attr("at", &["10", "0", "R0"])
            .attr("name", &["R1"]);
        transform_record(&mut inst, Xform::flip_x((0, 0))).unwrap();
        assert_eq!(inst.find_attr("at").unwrap().values, ["-10", "0", "MR0"]);
        let mut bad = RecordBuf::new(RecordKind::Noconn)
            .field("ef")
            .field("x")
            .field("0");
        assert!(transform_record(&mut bad, Xform::translate(1, 1)).is_none());
        let mut prose = RecordBuf::new(RecordKind::Prose).field("gh");
        assert!(transform_record(&mut prose, Xform::translate(1, 1)).is_some());
        assert_eq!(prose.fields, ["gh"]);
        // Schematic shapes: coordinates after the form; an arc's start turns, its sweep stays.
        let shape = |form: &str, v: &[&str]| {
            let mut r = RecordBuf::new(RecordKind::Shape).field("jk").field(form);
            for s in v {
                r = r.field(s);
            }
            r
        };
        let mut line = shape("line", &["0", "0", "10", "0"]);
        transform_record(&mut line, Xform::rotate((0, 0))).unwrap();
        assert_eq!(line.fields[2..], ["0", "0", "0", "10"]);
        let mut arc = shape("arc", &["10", "0", "5", "0", "90"]);
        transform_record(&mut arc, Xform::rotate((0, 0))).unwrap();
        assert_eq!(arc.fields[2..], ["0", "10", "5", "90", "90"]);
        assert_eq!(anchor(&arc), Some((0, 10_000)));
        let mut odd = shape("oval", &["0", "0"]);
        assert!(transform_record(&mut odd, Xform::translate(1, 1)).is_none());
    }

    #[test]
    fn snapping_rounds_to_grid() {
        assert_eq!(snap(14_999.0, 10_000), 10_000);
        assert_eq!(snap(15_000.0, 10_000), 20_000);
        assert_eq!(snap(-15_000.0, 10_000), -20_000);
        assert_eq!(snap(1e30, 10_000), i32::MAX);
    }
}
