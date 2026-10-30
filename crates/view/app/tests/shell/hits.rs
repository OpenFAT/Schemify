//! Hit-testing and box-selection properties over every primitive.

use schemify_editor::scene::{self, Objects, Scene};

use crate::common::{Session, U, Ui};

fn covers(objs: &Objects, i: usize, p: (f64, f64), tol: f64) -> bool {
    let g = objs.geom[i];
    if objs.seg[i] {
        let (ax, ay, bx, by) = (
            f64::from(g[0]),
            f64::from(g[1]),
            f64::from(g[2]),
            f64::from(g[3]),
        );
        let (dx, dy) = (bx - ax, by - ay);
        let l2 = (dx * dx + dy * dy).max(1e-9);
        let t = (((p.0 - ax) * dx + (p.1 - ay) * dy) / l2).clamp(0.0, 1.0);
        let (qx, qy) = (ax + t * dx - p.0, ay + t * dy - p.1);
        (qx * qx + qy * qy).sqrt() <= tol + 1e-6
    } else {
        let b = objs.bbox(i).map(f64::from);
        p.0 >= b[0] - tol && p.0 <= b[2] + tol && p.1 >= b[1] - tol && p.1 <= b[3] + tol
    }
}

/// One point on every primitive, with its owner.
fn prim_points(sc: &Scene) -> Vec<((f64, f64), u32)> {
    let p = &sc.prims;
    let o = &sc.owner;
    let mut out = Vec::new();
    for i in 0..p.lines.x0.len() {
        let mid = (
            (f64::from(p.lines.x0[i]) + f64::from(p.lines.x1[i])) / 2.0,
            (f64::from(p.lines.y0[i]) + f64::from(p.lines.y1[i])) / 2.0,
        );
        out.push((mid, o.lines[i]));
    }
    for i in 0..p.rects.x0.len() {
        out.push((
            (f64::from(p.rects.x0[i]), f64::from(p.rects.y0[i])),
            o.rects[i],
        ));
    }
    for i in 0..p.arcs.cx.len() {
        let a = f64::from(p.arcs.start[i]).to_radians() / 1000.0;
        let r = f64::from(p.arcs.r[i]);
        let c = (f64::from(p.arcs.cx[i]), f64::from(p.arcs.cy[i]));
        out.push(((c.0 + r * a.cos(), c.1 + r * a.sin()), o.arcs[i]));
    }
    for (polys, owners) in [(&p.polylines, &o.polylines), (&p.polygons, &o.polygons)] {
        let mut start = 0;
        for (k, &end) in polys.end.iter().enumerate() {
            out.push((
                (f64::from(polys.x[start]), f64::from(polys.y[start])),
                owners[k],
            ));
            start = end as usize;
        }
    }
    for i in 0..p.texts.x.len() {
        let q = (
            f64::from(p.texts.x[i]) + 500.0,
            f64::from(p.texts.y[i]) + 500.0,
        );
        out.push((q, o.texts[i]));
    }
    out
}

fn check_hits(sc: &Scene) {
    let tol = f64::from(U);
    let mut checked = 0;
    for (p, owner) in prim_points(sc) {
        if owner == u32::MAX {
            continue; // junction dots are cues, not objects
        }
        assert!(
            covers(&sc.objs, owner as usize, p, tol),
            "object {owner} does not cover its own primitive at {p:?}"
        );
        let hit = scene::hit(&sc.objs, p, tol).expect("every drawn primitive is hittable");
        assert!(
            covers(&sc.objs, hit as usize, p, tol),
            "hit returned a non-covering object"
        );
        checked += 1;
    }
    assert!(checked > 10, "{checked} primitives checked");
}

#[test]
fn every_primitive_is_hittable_schematic_and_symbol() {
    let mut s = Session::open("inv", "top.shcomp", "hits", Ui::Default);
    check_hits(s.ed.scene());
    s.click((10 * U, 0));
    s.key("e");
    check_hits(s.ed.scene());
    s.key("Ctrl+E");
    s.click((10 * U, 0));
    s.key("i");
    check_hits(s.ed.scene());
}

#[test]
fn box_select_is_consistent_with_point_hits() {
    let s = Session::open("inv", "top.shcomp", "boxes", Ui::Default);
    let sc = s.ed.scene();
    let objs = &sc.objs;
    let e = sc.extent.map(f64::from);
    // The whole extent selects everything, in both modes; an empty box far away nothing.
    let all = scene::box_select(
        objs,
        [e[0] - 1.0, e[1] - 1.0, e[2] + 1.0, e[3] + 1.0],
        false,
    );
    assert_eq!(all.len(), objs.len());
    assert!(scene::box_select(objs, [9e8, 9e8, 9e8 + 1.0, 9e8 + 1.0], true).is_empty());
    let mut seed = 0x2545_f491_u32;
    let mut rnd = |m: f64| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        f64::from(seed) / f64::from(u32::MAX) * m
    };
    let (w, h) = (e[2] - e[0], e[3] - e[1]);
    for _ in 0..200 {
        let (x0, y0) = (e[0] + rnd(w), e[1] + rnd(h));
        let b = [x0, y0, x0 + rnd(w / 2.0), y0 + rnd(h / 2.0)];
        let inside = scene::box_select(objs, b, false);
        let touch = scene::box_select(objs, b, true);
        assert!(
            inside.iter().all(|i| touch.contains(i)),
            "inside ⊆ touching"
        );
        for &i in &inside {
            let bb = objs.bbox(i as usize).map(f64::from);
            assert!(bb[0] >= b[0] && bb[2] <= b[2] && bb[1] >= b[1] && bb[3] <= b[3]);
        }
        // Union of point hits inside the box ⊆ touching selection.
        for k in 0..64 {
            let p = (
                b[0] + (b[2] - b[0]) * f64::from(k % 8) / 7.0,
                b[1] + (b[3] - b[1]) * f64::from(k / 8) / 7.0,
            );
            if let Some(j) = scene::hit(objs, p, 0.0) {
                assert!(
                    touch.contains(&j),
                    "point hit {j} at {p:?} outside the touch selection"
                );
            }
        }
    }
}
