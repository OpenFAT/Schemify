//! Polygon fills: a simple polygon's points in, its triangles (point-index triples) out, by
//! ear clipping. The fill rows of [`super::InstanceRows`] are these triangles.
//!
//! Contents:
//! - `pub(super) fn ear_clip(xs: &[i32], ys: &[i32], out: &mut Vec<[u32; 3]>)`

/// Triangulates the simple polygon (`xs`, `ys`), at least 3 points, by ear clipping into
/// `out` (cleared first) as point-index triples. A polygon with no ear left (degenerate or
/// self-intersecting) is fanned off its next vertex, so the output is always `n − 2`
/// triangles. O(n²) per ear.
///
/// # Panics
///
/// Panics on fewer than 3 points or columns of different lengths.
pub(super) fn ear_clip(xs: &[i32], ys: &[i32], out: &mut Vec<[u32; 3]>) {
    let n = xs.len();
    assert!(n >= 3, "a polygon of 3 points or more");
    assert_eq!(ys.len(), n, "point columns in step");
    out.clear();
    let mut idx: Vec<u32> = (0..n as u32).collect();
    let area2: i64 = (0..n)
        .map(|i| {
            let j = (i + 1) % n;
            i64::from(xs[i]) * i64::from(ys[j]) - i64::from(xs[j]) * i64::from(ys[i])
        })
        .sum();
    let sign = area2.signum();
    let inside = |k: usize, a: usize, b: usize, c: usize| {
        let (u, v, w) = (
            cross(xs, ys, a, b, k) * sign,
            cross(xs, ys, b, c, k) * sign,
            cross(xs, ys, c, a, k) * sign,
        );
        u >= 0 && v >= 0 && w >= 0
    };
    let mut steps = 0;
    while idx.len() > 3 {
        steps += 1;
        assert!(steps <= n, "one vertex clipped per step");
        let m = idx.len();
        let ear = (0..m).find(|&i| {
            let (a, b, c) = (
                idx[(i + m - 1) % m] as usize,
                idx[i] as usize,
                idx[(i + 1) % m] as usize,
            );
            cross(xs, ys, a, b, c) * sign > 0
                && !idx.iter().any(|&k| {
                    let k = k as usize;
                    k != a && k != b && k != c && inside(k, a, b, c)
                })
        });
        let i = ear.unwrap_or(1);
        out.push([idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]]);
        idx.remove(i);
    }
    out.push([idx[0], idx[1], idx[2]]);
    assert_eq!(out.len(), n - 2, "a simple polygon has n − 2 triangles");
}

// ---- private ----

/// Twice the signed area of triangle `a b c`.
fn cross(xs: &[i32], ys: &[i32], a: usize, b: usize, c: usize) -> i64 {
    let (ax, ay) = (i64::from(xs[a]), i64::from(ys[a]));
    (i64::from(xs[b]) - ax) * (i64::from(ys[c]) - ay)
        - (i64::from(ys[b]) - ay) * (i64::from(xs[c]) - ax)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: the triangles of a concave polygon cover its area exactly (the shoelace area),
    /// whichever way it winds.
    #[test]
    fn ear_clipping_covers_the_polygon() {
        let shapes: [&[(i32, i32)]; 3] = [
            &[(0, 0), (10, 0), (10, 10), (5, 4), (0, 10)],
            &[(0, 0), (6, 0), (6, 2), (2, 2), (2, 6), (0, 6)],
            &[(0, 0), (4, 0), (4, 4), (0, 4)],
        ];
        for pts in shapes {
            for reversed in [false, true] {
                let mut p: Vec<(i32, i32)> = pts.to_vec();
                if reversed {
                    p.reverse();
                }
                let xs: Vec<i32> = p.iter().map(|q| q.0).collect();
                let ys: Vec<i32> = p.iter().map(|q| q.1).collect();
                let mut out = Vec::new();
                ear_clip(&xs, &ys, &mut out);
                let area2: i64 = out
                    .iter()
                    .map(|&[a, b, c]| cross(&xs, &ys, a as usize, b as usize, c as usize).abs())
                    .sum();
                let n = xs.len();
                let want: i64 = (0..n)
                    .map(|i| {
                        let j = (i + 1) % n;
                        i64::from(xs[i]) * i64::from(ys[j]) - i64::from(xs[j]) * i64::from(ys[i])
                    })
                    .sum::<i64>()
                    .abs();
                assert_eq!(area2, want, "{pts:?} reversed {reversed}");
                assert_eq!(out.len(), n - 2);
            }
        }
    }
}
