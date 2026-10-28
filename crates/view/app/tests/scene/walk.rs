//! The seeded random walk: random edits through `doc::apply_edits`, each step's incremental
//! scene and selection rows against a full build and a scan.

use schemify_canvas::{CanvasPrims, PrimKind};
use schemify_doc::{
    DocHandle, Edit, EditRequest, Mode, Project, TableId, Transaction, apply_edits, ops_from_text,
};
use schemify_editor::scene::{self, ObjKey, Scene, Symbols};
use schemify_editor::session::View;
use schemify_elab::{CellState, no_funcs};
use schemify_format::FileKind;

use super::{RES, Rng, canonical, id};

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
            // Override a param shown on the symbol (a constant, or an expression of the
            // cell's vars), drop the override, or change a var many overrides read: the
            // texts of every instance whose value changed are drawn again.
            let t = doc.instances();
            let n = rng.below(9) + 1;
            match pick(rng, t.id.len()) {
                Some(_) if rng.below(3) == 0 => {
                    let var = ["rv", "rw"][rng.below(2) as usize];
                    let value = ["", "rv*"][rng.below(2) as usize];
                    let value = if var == "rv" {
                        format!("{n}")
                    } else {
                        format!("{value}{n}")
                    };
                    format!("set_field var {var} 1 {value}\n")
                }
                Some(i) if rng.below(4) == 0 => format!("remove_attr inst {} R\n", t.id[i].text(s)),
                Some(i) => {
                    let value = [
                        format!("{n}k"),
                        format!("rv*{n}"),
                        format!("rw+{n}"),
                        "rv".into(),
                    ][rng.below(4) as usize]
                        .clone();
                    format!("set_attr inst {} R {value}\n", t.id[i].text(s))
                }
                None => String::new(),
            }
        }
        12 => "undo".to_owned(),
        _ => "redo".to_owned(),
    }
}

/// What `scene::push_moved` drew before the index (every primitive scanned for a selected
/// owner): the oracle of the indexed version.
fn moved_by_scan(sc: &Scene, sel: &scene::Selected) -> CanvasPrims {
    let on = |o: u32| sel.contains(o);
    let (p, l) = (&sc.prims, scene::LAYER_SEL);
    let mut out = CanvasPrims::default();
    for i in 0..p.lines.x0.len() {
        if on(sc.owner.lines[i]) {
            let q = &p.lines;
            out.line(q.x0[i], q.y0[i], q.x1[i], q.y1[i], l);
        }
    }
    for i in 0..p.rects.x0.len() {
        if on(sc.owner.rects[i]) {
            let q = &p.rects;
            out.rect(q.x0[i], q.y0[i], q.x1[i], q.y1[i], l);
        }
    }
    for i in 0..p.arcs.cx.len() {
        if on(sc.owner.arcs[i]) {
            let a = &p.arcs;
            out.arc(a.cx[i], a.cy[i], a.r[i], a.start[i], a.sweep[i], l);
        }
    }
    for (polys, owners, closed) in [
        (&p.polylines, &sc.owner.polylines, false),
        (&p.polygons, &sc.owner.polygons, true),
    ] {
        let mut start = 0usize;
        for (k, &end) in polys.end.iter().enumerate() {
            let end = end as usize;
            if on(owners[k]) {
                let pts: Vec<(i32, i32)> = (start..end).map(|j| (polys.x[j], polys.y[j])).collect();
                out.poly(&pts, closed, l);
            }
            start = end;
        }
    }
    let t = &p.texts;
    let mut start = 0usize;
    for i in 0..t.x.len() {
        let end = t.end[i] as usize;
        if on(sc.owner.texts[i]) {
            out.text(t.x[i], t.y[i], t.size[i], l, &t.text[start..end]);
        }
        start = end;
    }
    out
}

/// Oracle for the selection's rows (`scene::selected_prims`) and what reads them: the marks and
/// the move preview equal a scan of every primitive for a selected owner.
fn selection_rows_match_scans(sc: &Scene, rng: &mut Rng) {
    use schemify_editor::geometry::Xform;
    let n = sc.objs.len();
    let mut sel = scene::Selected::default();
    let picks: Vec<u32> = (0..n as u32).filter(|_| rng.below(4) == 0).collect();
    sel.set(n, picks);
    let mut rows = Vec::new();
    scene::selected_prims(sc, &sel, &mut rows);
    let mut marks = schemify_canvas::Marks::default();
    scene::marks(sc, &rows, 7, &mut marks);
    let tables = [
        (PrimKind::Line, &sc.owner.lines),
        (PrimKind::Rect, &sc.owner.rects),
        (PrimKind::Arc, &sc.owner.arcs),
        (PrimKind::Polyline, &sc.owner.polylines),
        (PrimKind::Polygon, &sc.owner.polygons),
    ];
    let mut want = schemify_canvas::Marks::default();
    for (kind, owners) in tables {
        for (i, &o) in owners.iter().enumerate() {
            if sel.contains(o) {
                want.push(kind, i as u32, 7);
            }
        }
    }
    assert!(marks == want, "selection marks");
    let mut got = CanvasPrims::default();
    scene::push_moved(sc, &rows, Xform::IDENTITY, &mut got);
    assert!(got == moved_by_scan(sc, &sel), "move preview");
    assert!(!sel.is_empty() || got.is_empty());
}

/// The oracle walk: `steps` random edits of a schematic of 40 random wires plus `base`
/// wires on a grid beside them (the large-scale case), each followed by the comparison.
/// Returns how many revalued instances had their texts rewritten in place.
fn walk(seed: u64, steps: usize, base: usize) -> usize {
    let mut top = String::from("schemify 1 shcomp\n\nimpl sch schematic\n\n");
    let mut rng = Rng(seed);
    let mut next = 0u64;
    for _ in 0..40 {
        next += 1;
        let (x, y) = (rng.at(), rng.at());
        top.push_str(&format!("wire {} {x} {y} {} {y}\n", id(next), x + 40));
    }
    let side = (base as f64).sqrt().ceil() as usize;
    for k in 0..base {
        next += 1;
        let (x, y) = (400 + (k % side) as i64 * 30, (k / side) as i64 * 30);
        let y2 = y + 20 * (k % 3 == 0) as i64;
        top.push_str(&format!("wire {} {x} {y} {} {y2}\n", id(next), x + 20));
    }
    top.push_str("\nvar rv 2\nvar rw rv*3\n");
    let mut p = Project::new(None, None, seed).expect("a project with no library opens");
    let files: [(&str, &[u8]); 2] = [
        ("res.shprim", RES.as_bytes()),
        ("top.shcomp", top.as_bytes()),
    ];
    p.index_library("", "", &files)
        .expect("the library indexes");
    let res = p
        .open("", "res.shprim", RES.as_bytes(), None)
        .expect("the fixture cell opens")
        .handle;
    let h = p
        .open("", "top.shcomp", top.as_bytes(), None)
        .expect("the fixture cell opens")
        .handle;
    let mut symbols = Symbols::default();
    let row = p.resolve(h, "./res").expect("resolves");
    symbols.refresh(
        row,
        Some(res),
        0,
        p.doc(res).expect("the open cell's document is loaded"),
    );
    // The elab state rides along (D19: symbol texts show evaluated values), as the editor
    // drives it: a rebuilt graph builds the scene again, a value edit re-stamps the
    // instances elab names (`revalued`).
    let mut st = CellState::build(&p, h, &mut no_funcs).expect("elab state");
    let mut inc = Scene::default();
    scene::build(&mut inc, &p, h, View::Schematic, &symbols, 0, Some(&st));
    let (mut updated, mut epoch, mut revalued) = (0, 0u32, 0);
    // Revalued instances whose texts were rewritten in place (not re-stamped).
    let mut in_place = 0;
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
        let outcome = apply_edits(
            &mut p,
            &[EditRequest {
                doc: h,
                edit,
                unix_ms: 0,
            }],
        );
        let ops = p
            .applied(h, &outcome[0])
            .map_or_else(Vec::new, <[_]>::to_vec);
        epoch += 1;
        let done = st.recompute(&p, h, &ops, &mut no_funcs);
        let ids = &p
            .doc(h)
            .expect("the open cell's document is loaded")
            .instances()
            .id;
        let keys: Vec<ObjKey> = st
            .revalued
            .iter()
            .map(|&r| ObjKey {
                table: TableId::Instances,
                key: ids[r as usize].0,
            })
            .collect();
        revalued += keys.len();
        if done.graph {
            scene::build(&mut inc, &p, h, View::Schematic, &symbols, epoch, Some(&st));
        } else {
            let before: Vec<Option<u32>> = keys.iter().map(|&k| inc.find(k)).collect();
            let done = scene::update(&mut inc, &p, h, &symbols, epoch, Some(&st), &keys);
            updated += usize::from(done);
            // A re-stamped object is pushed last; one rewritten in place keeps its index.
            let last = inc.objs.len() as u32 - 1;
            in_place += keys
                .iter()
                .zip(&before)
                .filter(|&(&k, &o)| done && o.is_some_and(|o| o != last) && inc.find(k) == o)
                .count();
        }
        let fresh = CellState::build(&p, h, &mut no_funcs).expect("elab state");
        let mut full = Scene::default();
        scene::build(
            &mut full,
            &p,
            h,
            View::Schematic,
            &symbols,
            epoch,
            Some(&fresh),
        );
        if step % 25 == 0 {
            selection_rows_match_scans(&inc, &mut rng);
        }
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
    assert!(
        revalued > 0,
        "value edits re-stamped instances (seed {seed}, updated {updated})"
    );
    if base > 0 {
        return in_place; // a short walk at scale: the checks below are the small walks'
    }
    let doc = p.doc(h).expect("open");
    assert!(!doc.shapes().id.is_empty(), "the walk drew shapes");
    use schemify_canvas::LineStyle;
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
    in_place
}

#[test]
fn incremental_scene_equals_full_build() {
    let in_place: usize = (1..=6).map(|seed| walk(seed, 300, 0)).sum();
    assert!(in_place > 0, "a value edit rewrote texts in place");
}

/// The same oracle at the D39 scale: random edits of a 100k-wire schematic.
#[test]
#[ignore = "100k objects, a full build per step: run in release"]
fn incremental_scene_equals_full_build_at_100k() {
    walk(7, 12, 100_000);
}
