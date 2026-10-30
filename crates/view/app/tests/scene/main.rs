//! Oracle for the incremental scene (`scene::update`, D39 edits): after every edit of a seeded
//! random sequence through `doc::apply_edits` (inserts, deletes, moves, attribute changes,
//! shapes, rotated texts, undo, redo), the incrementally updated scene draws exactly what a
//! full `scene::build`
//! draws — per object the same primitives, geometry and connection points, and the same
//! junction dots — compared as sets (the incremental path appends, so row order differs).

mod walk;

use schemify_canvas::LAYER_DEAD;
use schemify_doc::Project;
use schemify_editor::scene::{self, ObjKey, Scene, Symbols};
use schemify_editor::session::View;

const RES: &str = "schemify 1 shprim\n\npin A inout 0 -30\npin B inout 0 30\n\nparam R 1k\n\nimpl spice spice\n  format spice \"R{name} {A} {B} {R}\"\n\nline 0 -30 0 -15\nline 0 15 0 30\nrect -5 -15 5 15\n  width 2\narc 0 0 8 0 90\ncircle 10 0 2\n  dash 2\npoly -5 -15 5 -15 0 -20 -5 -15\n  closed true\n\nstext 8 -10 R0 {name}\n  size 0.2\nstext 8 2 R0 R={R}\n  size 0.15\n";

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) % n
    }
    /// A grid coordinate in xschem units (coarse, so ends and T-junctions coincide often).
    fn at(&mut self) -> i64 {
        (self.below(13) as i64 - 6) * 20
    }
}

fn id(n: u64) -> String {
    const A: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let v = n.wrapping_mul(0x9e37_79b1) & ((1 << 30) - 1);
    (0..6)
        .rev()
        .map(|k| A[((v >> (5 * k)) & 31) as usize] as char)
        .collect()
}

/// Everything drawn, as sorted sets: per object (key, geometry, seg, primitives, connection
/// points) and the junction dots.
fn canonical(sc: &Scene) -> (Vec<String>, Vec<(i32, i32)>) {
    let p = &sc.prims;
    let mut rows: Vec<String> = Vec::new();
    let key = |o: u32| -> Option<ObjKey> { (o != u32::MAX).then(|| sc.objs.key[o as usize]) };
    // Dead rows (an update's removals, kept in place) draw nothing: not part of the picture.
    let live = |layer: &[u8], i: usize| layer[i] != LAYER_DEAD;
    let live_lines = (0..p.lines.x0.len()).filter(|&i| live(&p.lines.layer, i));
    for i in live_lines {
        let l = &p.lines;
        let k = key(sc.owner.lines[i]);
        rows.push(format!(
            "{k:?} line {} {} {} {} {} {:?}",
            l.x0[i], l.y0[i], l.x1[i], l.y1[i], l.layer[i], l.style[i]
        ));
    }
    let live_rects = (0..p.rects.x0.len()).filter(|&i| live(&p.rects.layer, i));
    for i in live_rects {
        let r = &p.rects;
        let k = key(sc.owner.rects[i]);
        rows.push(format!(
            "{k:?} rect {} {} {} {} {} {:?}",
            r.x0[i], r.y0[i], r.x1[i], r.y1[i], r.layer[i], r.style[i]
        ));
    }
    let mut dots = Vec::new();
    let live_arcs = (0..p.arcs.cx.len()).filter(|&i| live(&p.arcs.layer, i));
    for i in live_arcs {
        let a = &p.arcs;
        match key(sc.owner.arcs[i]) {
            None => dots.push((a.cx[i], a.cy[i])),
            k => rows.push(format!(
                "{k:?} arc {} {} {} {} {} {} {:?}",
                a.cx[i], a.cy[i], a.r[i], a.start[i], a.sweep[i], a.layer[i], a.style[i]
            )),
        }
    }
    for (name, polys, owners) in [
        ("polyline", &p.polylines, &sc.owner.polylines),
        ("polygon", &p.polygons, &sc.owner.polygons),
    ] {
        let mut start = 0;
        for (k, &end) in polys.end.iter().enumerate() {
            let pts: Vec<(i32, i32)> = (start..end as usize)
                .map(|j| (polys.x[j], polys.y[j]))
                .collect();
            start = end as usize;
            if !live(&polys.layer, k) {
                continue;
            }
            rows.push(format!(
                "{:?} {name} {pts:?} {} {:?}",
                key(owners[k]),
                polys.layer[k],
                polys.style[k]
            ));
        }
    }
    let t = &p.texts;
    let mut start = 0;
    for i in 0..t.x.len() {
        let end = t.end[i] as usize;
        if !live(&t.layer, i) {
            start = end;
            continue;
        }
        rows.push(format!(
            "{:?} text {} {} {} {} {} {:?}",
            key(sc.owner.texts[i]),
            t.x[i],
            t.y[i],
            t.size[i],
            t.layer[i],
            t.rot[i],
            &t.text[start..end]
        ));
        start = end;
    }
    for o in 0..sc.objs.len() {
        rows.push(format!(
            "{:?} obj {:?} {}",
            sc.objs.key[o], sc.objs.geom[o], sc.objs.seg[o]
        ));
    }
    for (pt, o) in sc.conn_points() {
        rows.push(format!("{:?} conn {pt:?}", key(o)));
    }
    // Negative space: a dead row and a dropped point belong to no object.
    let dead_owned = sc
        .owner
        .lines
        .iter()
        .zip(&p.lines.layer)
        .chain(sc.owner.texts.iter().zip(&p.texts.layer))
        .any(|(&o, &l)| l == LAYER_DEAD && o != u32::MAX);
    assert!(!dead_owned, "dead rows are unowned");
    rows.sort();
    dots.sort();
    // The index finds every object.
    for o in 0..sc.objs.len() {
        assert_eq!(
            sc.find(sc.objs.key[o]),
            Some(o as u32),
            "index of {:?}",
            sc.objs.key[o]
        );
    }
    (rows, dots)
}

/// A symbol graphic's `width` and `dash` reach every placed instance's primitive row; the
/// graphics without them stay plain (format spec §4, stroke attributes).
#[test]
fn symbol_stroke_attributes_reach_the_scene() {
    use schemify_canvas::LineStyle;
    let top =
        "schemify 1 shcomp\n\nimpl sch schematic\n\ninst aaaaaa ./res\n  at 0 0 R90\n  name R1\n";
    let mut p = Project::new(None, None, 1).unwrap();
    let files: [(&str, &[u8]); 2] = [
        ("res.shprim", RES.as_bytes()),
        ("top.shcomp", top.as_bytes()),
    ];
    p.index_library("", "", &files).unwrap();
    let res = p
        .open("", "res.shprim", RES.as_bytes(), None)
        .unwrap()
        .handle;
    let h = p
        .open("", "top.shcomp", top.as_bytes(), None)
        .unwrap()
        .handle;
    let mut symbols = Symbols::default();
    let row = p.resolve(h, "./res").expect("resolves");
    symbols.refresh(row, Some(res), 0, p.doc(res).unwrap());
    let mut sc = Scene::default();
    scene::build(&mut sc, &p, h, View::Schematic, &symbols, 0, None);
    let pr = &sc.prims;
    // The body rect (weight 2) and the four pin squares (plain).
    let mut rects: Vec<LineStyle> = pr.rects.style.clone();
    rects.sort();
    assert_eq!(
        rects,
        [LineStyle::PLAIN, LineStyle::PLAIN, LineStyle::new(2, 0)]
    );
    assert!(pr.lines.style.iter().all(|&s| s == LineStyle::PLAIN));
    let arcs: Vec<LineStyle> = pr.arcs.style.clone();
    assert!(arcs.contains(&LineStyle::new(1, 2)) && arcs.contains(&LineStyle::PLAIN));
    assert!(pr.polygons.style.iter().all(|&s| s == LineStyle::PLAIN));
}

/// A symbol's own view pairs each polygon primitive with its own graphic row: polys of the
/// minimum two points and longer, between other graphics, keep their keys and geometry. The
/// pairing is one to one because no document holds a poly under two points: the reader
/// refuses one (`E029`).
#[test]
fn each_symbol_poly_keeps_its_own_row() {
    use schemify_doc::TableId;
    let head = "schemify 1 shprim\n\npin A inout 0 -30\n\nimpl spice spice\n  format spice \"R{name} {A}\"\n\n";
    let sym = format!("{head}poly 50 50 60 50\nline 0 0 5 0\npoly 10 10 20 10 20 20\n");
    let mut p = Project::new(None, None, 1).unwrap();
    p.index_library("", "", &[("s.shprim", sym.as_bytes())])
        .unwrap();
    let h = p.open("", "s.shprim", sym.as_bytes(), None).unwrap().handle;
    let mut sc = Scene::default();
    scene::build(&mut sc, &p, h, View::Symbol, &Symbols::default(), 0, None);
    let object = |r: u32| {
        let key = ObjKey {
            table: TableId::Graphics,
            key: r,
        };
        sc.objs
            .key
            .iter()
            .position(|k| *k == key)
            .expect("one object per graphic row")
    };
    let (first, second) = (object(0), object(2));
    assert_eq!(sc.objs.geom[first], [50_000, 50_000, 60_000, 50_000]);
    assert_eq!(sc.objs.geom[second], [10_000, 10_000, 20_000, 20_000]);
    let polys = &sc.prims.polylines;
    assert_eq!(polys.end.len(), 2, "one polyline per poly row");
    let starts = [0, polys.end[0] as usize];
    for (k, (obj, at)) in [(first, (50_000, 50_000)), (second, (10_000, 10_000))]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            sc.owner.polylines[k], obj as u32,
            "polyline {k} drawn for its row"
        );
        assert_eq!((polys.x[starts[k]], polys.y[starts[k]]), at);
    }
    // Negative space: a one-point poly never reaches a document.
    let one = format!("{head}poly 50 50\n");
    let refused = p.open("", "t.shprim", one.as_bytes(), None).unwrap_err();
    assert!(format!("{refused:?}").contains("E029"), "{refused:?}");
}

/// The selection set (`scene::Selected`): a reselect clears every bit the last one set, a
/// scene that shrinks or grows keeps no stale bit, an index outside the scene (`u32::MAX`, the
/// owner of an unowned primitive) is never selected, and a small selection of a large scene
/// touches only its own bits.
#[test]
fn selection_set_follows_reselects_and_scene_size() {
    let mut s = scene::Selected::default();
    assert!(s.is_empty() && !s.contains(0) && !s.contains(u32::MAX));
    s.set(200, [5, 64, 5, 199]);
    assert_eq!((s.len(), s.universe()), (3, 200), "duplicates collapse");
    for o in 0..200 {
        assert_eq!(s.contains(o), [5, 64, 199].contains(&o), "object {o}");
    }
    assert!(
        !s.contains(200) && !s.contains(u32::MAX),
        "outside the scene"
    );
    s.set(70, [65]);
    assert!(
        !s.contains(5) && !s.contains(64) && !s.contains(199),
        "old bits cleared"
    );
    assert!(s.contains(65) && s.len() == 1);
    s.set(1 << 22, [3, (1 << 22) - 1]);
    assert!(!s.contains(65) && s.contains(3) && s.contains((1 << 22) - 1));
    s.set(1 << 22, std::iter::empty());
    assert!(s.is_empty() && !s.contains(3) && !s.contains((1 << 22) - 1));
}

/// Selecting an index outside the scene is a programmer error.
#[test]
#[should_panic(expected = "selected object out of the scene")]
fn selection_set_rejects_an_object_outside_the_scene() {
    scene::Selected::default().set(10, [10]);
}
