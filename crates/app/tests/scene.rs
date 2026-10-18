//! Oracle for the incremental scene (`scene::update`, D39 edits): after every edit of a seeded
//! random sequence through `doc::apply_edits` (inserts, deletes, moves, attribute changes,
//! shapes, rotated texts, undo, redo), the incrementally updated scene draws exactly what a
//! full `scene::build`
//! draws — per object the same primitives, geometry and connection points, and the same
//! junction dots — compared as sets (the incremental path appends, so row order differs).

use schemify_app::scene::{self, ObjKey, Scene, Symbols};
use schemify_app::session::View;
use schemify_doc::{
    DocHandle, Edit, EditRequest, Mode, Project, Transaction, apply_edits, ops_from_text,
};
use schemify_format::FileKind;

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
    for i in 0..p.lines.x0.len() {
        let l = &p.lines;
        let k = key(sc.owner.lines[i]);
        rows.push(format!(
            "{k:?} line {} {} {} {} {} {:?}",
            l.x0[i], l.y0[i], l.x1[i], l.y1[i], l.layer[i], l.style[i]
        ));
    }
    for i in 0..p.rects.x0.len() {
        let r = &p.rects;
        let k = key(sc.owner.rects[i]);
        rows.push(format!(
            "{k:?} rect {} {} {} {} {} {:?}",
            r.x0[i], r.y0[i], r.x1[i], r.y1[i], r.layer[i], r.style[i]
        ));
    }
    let mut dots = Vec::new();
    for i in 0..p.arcs.cx.len() {
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
            rows.push(format!(
                "{:?} {name} {pts:?} {} {:?}",
                key(owners[k]),
                polys.layer[k],
                polys.style[k]
            ));
            start = end as usize;
        }
    }
    let t = &p.texts;
    let mut start = 0;
    for i in 0..t.x.len() {
        let end = t.end[i] as usize;
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
    for &(pt, o) in &sc.conn {
        rows.push(format!("{:?} conn {pt:?}", key(o)));
    }
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

/// One random edit as op text, from the document's current objects.
fn random_ops(rng: &mut Rng, p: &Project, h: DocHandle, next: &mut u64) -> String {
    let doc = p.doc(h).expect("open");
    let s = doc.strings();
    let pick = |rng: &mut Rng, n: usize| (n > 0).then(|| rng.below(n as u64) as usize);
    let mut fresh = || {
        *next += 1;
        id(*next)
    };
    match rng.below(18) {
        0..=2 => {
            // An axis-aligned wire, often ending on another wire (T-junctions), sometimes
            // diagonal.
            let (x, y) = (rng.at(), rng.at());
            let len = (rng.below(4) as i64 + 1) * 20;
            let (x2, y2) = match rng.below(5) {
                0 | 1 => (x + len, y),
                2 | 3 => (x, y + len),
                _ => (x + len, y + len),
            };
            format!("insert wire {} {x} {y} {x2} {y2}\n", fresh())
        }
        3 => format!(
            "insert inst {} ./res\n  at {} {} {}\n  name R{}\n",
            fresh(),
            rng.at(),
            rng.at(),
            ["R0", "R90", "R180", "MR270"][rng.below(4) as usize],
            *next
        ),
        4 => format!(
            "insert label {} {} {} R0 n{}\n",
            fresh(),
            rng.at(),
            rng.at(),
            rng.below(5)
        ),
        5 => format!("insert noconn {} {} {}\n", fresh(), rng.at(), rng.at()),
        6 => format!(
            "insert text {} {} {} {} t{}\n",
            fresh(),
            rng.at(),
            rng.at(),
            ["R0", "R90", "R180", "R270", "MR90"][rng.below(5) as usize],
            rng.below(9)
        ),
        14 | 15 => {
            // A schematic graphic of every form.
            let (x, y) = (rng.at(), rng.at());
            let d = (rng.below(3) as i64 + 1) * 20;
            let (form, values) = match rng.below(5) {
                0 => ("line", format!("{x} {y} {} {}", x + d, y)),
                1 => ("rect", format!("{x} {y} {} {}", x + d, y + d)),
                2 => ("circle", format!("{x} {y} {d}")),
                3 => ("arc", format!("{x} {y} {d} 0 90")),
                _ => ("poly", format!("{x} {y} {} {y} {x} {}", x + d, y + d)),
            };
            let layer =
                ["", "  layer 7\n", "  width 2\n", "  dash 3\n  width 3\n"][rng.below(4) as usize];
            format!("insert shape {} {form} {values}\n{layer}", fresh())
        }
        16 => {
            // Delete or move a shape (its coordinates, or its layer).
            let t = doc.shapes();
            match pick(rng, t.id.len()) {
                Some(i) if rng.below(3) == 0 => format!("delete shape {}\n", t.id[i].text(s)),
                Some(i) if rng.below(2) == 0 => {
                    let attr = ["layer", "width", "dash"][rng.below(3) as usize];
                    format!(
                        "set_attr shape {} {attr} {}\n",
                        t.id[i].text(s),
                        rng.below(8) + 1
                    )
                }
                Some(i) => format!(
                    "set_field shape {} {} {}\n",
                    t.id[i].text(s),
                    rng.below(2) + 2,
                    rng.at()
                ),
                None => String::new(),
            }
        }
        17 => {
            // Turn a text.
            let t = doc.texts();
            match pick(rng, t.id.len()) {
                Some(i) => format!(
                    "set_field text {} 3 {}\n",
                    t.id[i].text(s),
                    ["R0", "R90", "R180", "R270"][rng.below(4) as usize]
                ),
                None => String::new(),
            }
        }
        7 | 8 => {
            // Delete something.
            let tables = [
                ("wire", doc.wires().id.clone()),
                ("inst", doc.instances().id.clone()),
                ("label", doc.labels().id.clone()),
            ];
            let (word, ids) = &tables[rng.below(3) as usize];
            match pick(rng, ids.len()) {
                Some(i) => format!("delete {word} {}\n", ids[i].text(s)),
                None => String::new(),
            }
        }
        9 => {
            // Move a wire end.
            let w = doc.wires();
            match pick(rng, w.id.len()) {
                Some(i) => format!(
                    "set_field wire {} {} {}\n",
                    w.id[i].text(s),
                    rng.below(4) + 1,
                    rng.at()
                ),
                None => String::new(),
            }
        }
        10 => {
            // Move or rotate an instance.
            let t = doc.instances();
            match pick(rng, t.id.len()) {
                Some(i) => format!(
                    "set_attr inst {} at {} {} R90\n",
                    t.id[i].text(s),
                    rng.at(),
                    rng.at()
                ),
                None => String::new(),
            }
        }
        11 => {
            // Rename a label (its text is drawn) or override a param shown on the symbol.
            let t = doc.instances();
            match pick(rng, t.id.len()) {
                Some(i) => format!(
                    "set_attr inst {} R {}k\n",
                    t.id[i].text(s),
                    rng.below(9) + 1
                ),
                None => String::new(),
            }
        }
        12 => "undo".to_owned(),
        _ => "redo".to_owned(),
    }
}

fn walk(seed: u64, steps: usize) {
    let mut top = String::from("schemify 1 shcomp\n\nimpl sch schematic\n\n");
    let mut rng = Rng(seed);
    let mut next = 0u64;
    for _ in 0..40 {
        next += 1;
        let (x, y) = (rng.at(), rng.at());
        top.push_str(&format!("wire {} {x} {y} {} {y}\n", id(next), x + 40));
    }
    let mut p = Project::new(None, None, seed).unwrap();
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
    let mut inc = Scene::default();
    scene::build(&mut inc, &p, h, View::Schematic, &symbols, 0, None);
    let (mut updated, mut epoch) = (0, 0u32);
    for step in 0..steps {
        let text = random_ops(&mut rng, &p, h, &mut next);
        let edit = match text.as_str() {
            "" => continue,
            "undo" => Edit::Undo,
            "redo" => Edit::Redo,
            t => Edit::Tx(Transaction {
                label: "edit".into(),
                mode: Mode::Commit,
                ops: ops_from_text(t, FileKind::Shcomp).unwrap_or_else(|e| panic!("{t}: {e:?}")),
            }),
        };
        let _ = apply_edits(
            &mut p,
            &[EditRequest {
                doc: h,
                edit,
                unix_ms: 0,
            }],
        );
        epoch += 1;
        updated += usize::from(scene::update(&mut inc, &p, h, &symbols, epoch, None));
        let mut full = Scene::default();
        scene::build(&mut full, &p, h, View::Schematic, &symbols, epoch, None);
        let (a, b) = (canonical(&inc), canonical(&full));
        if a != b {
            let only_inc: Vec<_> = a.0.iter().filter(|r| !b.0.contains(r)).collect();
            let only_full: Vec<_> = b.0.iter().filter(|r| !a.0.contains(r)).collect();
            panic!(
                "step {step} ({text:?}): incremental differs\n only incremental: {only_inc:#?}\n only full: {only_full:#?}\n dots {:?} vs {:?}",
                a.1, b.1
            );
        }
    }
    assert!(
        updated * 2 > steps,
        "most edits were incremental ({updated} of {steps})"
    );
    let doc = p.doc(h).expect("open");
    assert!(!doc.shapes().id.is_empty(), "the walk drew shapes");
    use schemify_render::canvas::LineStyle;
    let p = &inc.prims;
    let styles = p
        .lines
        .style
        .iter()
        .chain(&p.rects.style)
        .chain(&p.arcs.style);
    assert!(
        styles.clone().any(|s| s.weight() > 1) && styles.clone().any(|s| s.dash() > 0),
        "the walk drew weighted and dashed strokes"
    );
    assert!(p.lines.style.contains(&LineStyle::PLAIN), "and plain ones");
    assert!(
        inc.prims.texts.rot.iter().any(|&r| r != 0),
        "and turned texts"
    );
}

/// A symbol graphic's `width` and `dash` reach every placed instance's primitive row; the
/// graphics without them stay plain (format spec §4, stroke attributes).
#[test]
fn symbol_stroke_attributes_reach_the_scene() {
    use schemify_render::canvas::LineStyle;
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

#[test]
fn incremental_scene_equals_full_build() {
    for seed in 1..=6 {
        walk(seed, 300);
    }
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
