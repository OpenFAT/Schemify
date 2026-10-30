//! Vector paths ([`MathPaths`]: math delimiters and radicals, button icons) painted as egui
//! shapes: curves flattened to polylines, fills ear-clipped into a mesh (egui only fills convex
//! polygons itself), strokes as polylines.
//!
//! Contents: crate-only `paint_paths(painter, fill, stroke, origin, scale, width, color)`

use egui::epaint::{Mesh, PathStroke};
use egui::{Color32, Painter, Pos2, Shape, pos2};

use crate::math::{MathPaths, Verb};

/// Segments a curve is flattened into.
const CURVE_STEPS: usize = 8;

/// Points one subpath may hold after flattening (bounds the ear clip, which is O(n²)).
const POINTS_MAX: usize = 4096;

/// Paints `fill` filled and `stroke` stroked `width` units wide, in `color`, each point mapped
/// to `origin + p * scale`.
// ponytail: each subpath is filled on its own, so a path with holes (a ring) fills its hole;
// even-odd fills when a glyph-like icon needs one.
pub(crate) fn paint_paths(
    painter: &Painter,
    fill: &MathPaths,
    stroke: &MathPaths,
    origin: Pos2,
    scale: f32,
    width: f32,
    color: Color32,
) {
    assert!(scale.is_finite() && scale > 0.0, "a positive scale");
    let map = |p: [f32; 2]| pos2(origin.x + p[0] * scale, origin.y + p[1] * scale);
    for sub in subpaths(fill, map) {
        if sub.len() >= 3 {
            painter.add(Shape::mesh(fill_mesh(&sub, color)));
        }
    }
    for sub in subpaths(stroke, map) {
        if sub.len() >= 2 {
            painter.add(Shape::line(sub, PathStroke::new(width * scale, color)));
        }
    }
}

/// Flattens every path into its subpaths' polylines.
fn subpaths(paths: &MathPaths, map: impl Fn([f32; 2]) -> Pos2) -> Vec<Vec<Pos2>> {
    let mut out: Vec<Vec<Pos2>> = Vec::new();
    let mut cur: Vec<Pos2> = Vec::new();
    let mut pt = paths.pt.iter().copied();
    let mut last = Pos2::ZERO;
    for &verb in &paths.verb {
        match verb {
            Verb::Move => {
                if cur.len() > 1 {
                    out.push(std::mem::take(&mut cur));
                }
                cur.clear();
                last = map(pt.next().unwrap_or_default());
                cur.push(last);
            }
            Verb::Line => {
                last = map(pt.next().unwrap_or_default());
                cur.push(last);
            }
            Verb::Quad => {
                let (c, e) = (map(pt.next().unwrap_or_default()), map(pt.next().unwrap_or_default()));
                for k in 1..=CURVE_STEPS {
                    let t = k as f32 / CURVE_STEPS as f32;
                    let u = 1.0 - t;
                    cur.push((last.to_vec2() * u * u + c.to_vec2() * 2.0 * u * t + e.to_vec2() * t * t).to_pos2());
                }
                last = e;
            }
            Verb::Cubic => {
                let c1 = map(pt.next().unwrap_or_default());
                let c2 = map(pt.next().unwrap_or_default());
                let e = map(pt.next().unwrap_or_default());
                for k in 1..=CURVE_STEPS {
                    let t = k as f32 / CURVE_STEPS as f32;
                    let u = 1.0 - t;
                    let v = last.to_vec2() * u * u * u
                        + c1.to_vec2() * 3.0 * u * u * t
                        + c2.to_vec2() * 3.0 * u * t * t
                        + e.to_vec2() * t * t * t;
                    cur.push(v.to_pos2());
                }
                last = e;
            }
            Verb::Close => {
                if let Some(&first) = cur.first() {
                    cur.push(first);
                    last = first;
                }
            }
        }
        if cur.len() > POINTS_MAX {
            cur.truncate(POINTS_MAX);
        }
    }
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

/// Ear-clips polygon `pts` (either winding, closing point optional) into a mesh.
fn fill_mesh(pts: &[Pos2], color: Color32) -> Mesh {
    let mut pts = pts.to_vec();
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    let mut mesh = Mesh::default();
    for &p in &pts {
        mesh.colored_vertex(p, color);
    }
    let area: f32 = (0..pts.len())
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    let ccw = area > 0.0;
    let mut idx: Vec<u32> = (0..pts.len() as u32).collect();
    // Each pass removes one ear or gives up on a degenerate rest: bounded by n² steps.
    let mut guard = idx.len() * idx.len() + 8;
    let mut i = 0;
    while idx.len() > 3 && guard > 0 {
        guard -= 1;
        let n = idx.len();
        let (a, b, c) = (idx[(i + n - 1) % n], idx[i % n], idx[(i + 1) % n]);
        let (pa, pb, pc) = (pts[a as usize], pts[b as usize], pts[c as usize]);
        let cross = (pb - pa).x * (pc - pa).y - (pb - pa).y * (pc - pa).x;
        let convex = if ccw { cross > 0.0 } else { cross < 0.0 };
        let empty = convex
            && idx.iter().all(|&j| {
                j == a || j == b || j == c || !inside(pts[j as usize], pa, pb, pc)
            });
        if empty {
            mesh.add_triangle(a, b, c);
            idx.remove(i % n);
        } else {
            i += 1;
        }
    }
    if idx.len() == 3 {
        mesh.add_triangle(idx[0], idx[1], idx[2]);
    }
    debug_assert!(mesh.indices.len() % 3 == 0, "whole triangles");
    mesh
}

fn inside(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
    let s = |p1: Pos2, p2: Pos2, p3: Pos2| (p1.x - p3.x) * (p2.y - p3.y) - (p2.x - p3.x) * (p1.y - p3.y);
    let (d1, d2, d3) = (s(p, a, b), s(p, b, c), s(p, c, a));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concave_l_shape_fills_with_four_triangles() {
        let l = [
            pos2(0.0, 0.0),
            pos2(2.0, 0.0),
            pos2(2.0, 1.0),
            pos2(1.0, 1.0),
            pos2(1.0, 2.0),
            pos2(0.0, 2.0),
        ];
        let m = fill_mesh(&l, Color32::WHITE);
        assert_eq!(m.indices.len(), 4 * 3, "n - 2 triangles");
        // Area of the triangles equals the L's area (3), so no triangle covers the notch.
        let area: f32 = m
            .indices
            .chunks(3)
            .map(|t| {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.vertices[i as usize].pos);
                ((b - a).x * (c - a).y - (b - a).y * (c - a).x).abs() / 2.0
            })
            .sum();
        assert!((area - 3.0).abs() < 1e-4, "area {area}");
    }
}
