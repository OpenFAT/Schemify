//! Display decimation: a trace step of up to millions of samples becomes a polyline of at most
//! two vertices per pixel column (its min and max there), so drawing costs O(width), not
//! O(samples).
//!
//! The hot loop is [`minmax`] over each column's run of samples. It is written in the
//! `simd-loops` shape: eight independent lane accumulators folded over `chunks_exact` (LLVM
//! lowers them to packed `minpd`/`maxpd`, two lanes per SSE2 register, four with AVX2; checked in
//! the assembly for both), then a lane fold, then [`minmax_scalar`] as the tail.
//! `minmax_scalar` is kept verbatim as the oracle. Stable Rust without `unsafe`, so no
//! intrinsics: the shape is what makes the vectorization dependable.
//!
//! The comparisons are `select(v < acc, v, acc)`, which is exactly what `minpd` computes, so the
//! vector and scalar versions agree on every input: NaN samples are skipped (a NaN never wins a
//! comparison), and an empty or all-NaN run gives `(+inf, -inf)`. The one difference is which
//! zero wins between `0.0` and `-0.0`; they compare equal.
//!
//! Contents:
//! - `pub fn decimate(xs: &[f64], ys: &[f64], sx: &Scale, sy: &Scale, out: &mut Vec<[f32; 2]>)`
//! - `pub(crate) fn minmax(y: &[f64]) -> (f64, f64)`
//! - `pub(crate) fn minmax_scalar(y: &[f64]) -> (f64, f64)`
//! - `pub(crate) fn extent(xs, ys, steps, x_lo, x_hi) -> (f64, f64)`
//! - `pub(crate) fn pixel_columns(sx: &Scale) -> usize`
//! - `pub(crate) fn gallop(xs: &[f64], from: usize, end: usize, edge: f64) -> usize`

use core::ops::Range;

use crate::axis::Scale;

/// Lanes of the vector accumulators; 8 × f64 fills four SSE2 or two AVX registers per
/// accumulator, enough independent chains to hide `minpd` latency.
const LANES: usize = 8;

/// Appends the polyline of one sweep step to `out` as `[x, y]` pixels: every sample when the
/// visible part has at most two per pixel column, otherwise each column's min and max (entered
/// from the side nearer its first sample) plus the nearest sample beyond each edge so the line
/// reaches the frame. Non-finite samples are skipped. A step whose x is not ascending (an X-Y
/// plot, a descending sweep) is drawn sample by sample.
///
/// # Panics
///
/// Panics if the columns differ in length or `sx` does not increase left to right.
///
/// # Time complexity
///
/// O(visible samples) for the min/max scan plus O(columns · log(samples per column)) to find
/// the column boundaries; O(samples) for non-ascending steps.
pub fn decimate(xs: &[f64], ys: &[f64], sx: &Scale, sy: &Scale, out: &mut Vec<[f32; 2]>) {
    decimate_with(minmax, xs, ys, sx, sy, out);
}

/// Minimum and maximum of `y`, skipping NaN; `(+inf, -inf)` when no sample compares. Vectorized
/// (packed `minpd`/`maxpd` on every x86-64 level, by the shape of the loop, without `unsafe`);
/// agrees with [`minmax_scalar`] on every input up to the sign of a zero result.
///
/// # Time complexity
///
/// O(n): 9.6 µs per 64k samples in L2 (7× the scalar fold), 0.16 ms per 1M samples.
pub(crate) fn minmax(y: &[f64]) -> (f64, f64) {
    let chunks = y.chunks_exact(LANES);
    let tail = chunks.remainder();
    // The accumulators travel through `fold` by value: that keeps them in registers. The same
    // loop written with `for` over mutable arrays vectorizes for SSE2 but spills and runs scalar
    // under `-C target-cpu=native` (AVX2), measured 7x slower.
    let (lo, hi) = chunks.fold(
        ([f64::INFINITY; LANES], [f64::NEG_INFINITY; LANES]),
        |(mut lo, mut hi), c| {
            for k in 0..LANES {
                let v = c[k];
                lo[k] = if v < lo[k] { v } else { lo[k] };
                hi[k] = if v > hi[k] { v } else { hi[k] };
            }
            (lo, hi)
        },
    );
    let (mut a, mut b) = minmax_scalar(tail);
    for k in 0..LANES {
        a = if lo[k] < a { lo[k] } else { a };
        b = if hi[k] > b { hi[k] } else { b };
    }
    debug_assert!(!a.is_nan() && !b.is_nan(), "results are never NaN");
    (a, b)
}

/// The scalar fold [`minmax`] vectorizes: the oracle in tests and the vector loop's tail.
/// Kept out of line so benchmarks measure it as written.
#[inline(never)]
pub(crate) fn minmax_scalar(y: &[f64]) -> (f64, f64) {
    let (mut a, mut b) = (f64::INFINITY, f64::NEG_INFINITY);
    for &v in y {
        if v < a {
            a = v;
        }
        if v > b {
            b = v;
        }
    }
    (a, b)
}

/// Minimum and maximum of `ys` over the samples whose x lies in `[x_lo, x_hi]`, step by step
/// (`steps` partition the columns). Steps whose x is not ascending are taken whole. Used for
/// autoscaling; `(+inf, -inf)` when nothing is in range.
///
/// # Panics
///
/// Panics if the columns differ in length or a step lies outside them.
pub(crate) fn extent(
    xs: &[f64],
    ys: &[f64],
    steps: &[Range<u32>],
    x_lo: f64,
    x_hi: f64,
) -> (f64, f64) {
    assert_eq!(xs.len(), ys.len(), "x and y columns differ in length");
    let (mut a, mut b) = (f64::INFINITY, f64::NEG_INFINITY);
    for s in steps {
        let r = s.start as usize..s.end as usize;
        let (x, y) = (&xs[r.clone()], &ys[r]);
        let r = if x.first() <= x.last() {
            x.partition_point(|&v| v < x_lo)..x.partition_point(|&v| v <= x_hi)
        } else {
            0..x.len()
        };
        let (lo, hi) = minmax(&y[r.start..r.end.max(r.start)]);
        a = a.min(lo);
        b = b.max(hi);
    }
    (a, b)
}

/// Whole pixel columns `sx` spans, at least one.
pub(crate) fn pixel_columns(sx: &Scale) -> usize {
    assert!(sx.p1 > sx.p0, "x pixels must increase left to right");
    ((sx.p1 - sx.p0).floor() as usize).max(1)
}

/// First index in `from..=end` whose x is `>= edge` (`xs` ascending): exponential search from
/// `from`, then binary search in the bracket, so a run of k samples costs O(log k) probes that
/// stay near each other in memory.
pub(crate) fn gallop(xs: &[f64], from: usize, end: usize, edge: f64) -> usize {
    debug_assert!(from <= end && end <= xs.len());
    let mut lo = from;
    let mut step = 1;
    // Invariant: xs[from..lo] < edge. Bounded: `step` doubles every pass.
    while lo + step <= end && xs[lo + step - 1] < edge {
        lo += step;
        step *= 2;
    }
    let hi = (lo + step).min(end);
    let k = lo + xs[lo..hi].partition_point(|&x| x < edge);
    debug_assert!(k == end || xs[k] >= edge);
    k
}

// ---- private ----

/// [`decimate`] with its min/max kernel as a parameter, so tests and the benchmark run the
/// scalar oracle through the same column walk.
fn decimate_with(
    kernel: fn(&[f64]) -> (f64, f64),
    xs: &[f64],
    ys: &[f64],
    sx: &Scale,
    sy: &Scale,
    out: &mut Vec<[f32; 2]>,
) {
    assert_eq!(xs.len(), ys.len(), "x and y columns differ in length");
    let cols = pixel_columns(sx);
    let n = xs.len();
    let before = out.len();
    let push = |x: f64, y: f64, out: &mut Vec<[f32; 2]>| {
        if x.is_finite() && y.is_finite() {
            out.push([sx.px(x), sy.px(y)]);
        }
    };
    if n == 0 {
        return;
    }
    if xs[0] > xs[n - 1] || !xs[0].is_finite() {
        // ponytail: non-ascending steps are drawn point by point, O(samples) per frame.
        // Decimate them by sorting a copy once if X-Y plots of millions of points matter.
        for (&x, &y) in xs.iter().zip(ys) {
            push(x, y, out);
        }
        return;
    }
    let (x_lo, x_hi) = (sx.value(sx.p0), sx.value(sx.p1));
    let first = xs.partition_point(|&x| x < x_lo);
    let end = xs.partition_point(|&x| x <= x_hi).max(first);
    let (a, b) = (first.saturating_sub(1), (end + 1).min(n));
    if b - a <= 2 * cols {
        for i in a..b {
            push(xs[i], ys[i], out);
        }
        return;
    }
    if a < first {
        push(xs[a], ys[a], out);
    }
    let mut i = first;
    for c in 0..cols {
        let j = if c + 1 == cols {
            end
        } else {
            gallop(xs, i, end, sx.value(sx.p0 + (c + 1) as f32))
        };
        if j > i {
            let (lo, hi) = kernel(&ys[i..j]);
            if lo <= hi {
                let x = sx.p0 + c as f32 + 0.5;
                let (pl, ph) = (sy.px(lo), sy.px(hi));
                let f = ys[i];
                let (p, q) = if (f - lo).abs() <= (hi - f).abs() {
                    (pl, ph)
                } else {
                    (ph, pl)
                };
                out.push([x, p]);
                out.push([x, q]);
            }
        }
        i = j;
    }
    if end < b {
        push(xs[b - 1], ys[b - 1], out);
    }
    debug_assert!(out.len() - before <= 2 * cols + 2);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digital::{bin_counts, logic_runs};

    /// Deterministic xorshift in [-1, 1).
    fn rng(seed: &mut u64) -> f64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }

    #[test]
    fn minmax_matches_scalar_at_every_tail_length() {
        let mut seed = 0x9e37_79b9_7f4a_7c15;
        for len in 0..=(3 * LANES + 5) {
            for nan_at in [None, Some(0), Some(len / 2), Some(len.saturating_sub(1))] {
                let mut y: Vec<f64> = (0..len).map(|_| rng(&mut seed) * 1e3).collect();
                if let Some(k) = nan_at.filter(|&k| k < len) {
                    y[k] = f64::NAN;
                }
                let (v, s) = (minmax(&y), minmax_scalar(&y));
                assert!(v.0 == s.0 && v.1 == s.1, "len {len}: {v:?} vs {s:?}");
            }
        }
        assert_eq!(minmax(&[]), (f64::INFINITY, f64::NEG_INFINITY));
        assert_eq!(minmax(&[f64::NAN; 11]), (f64::INFINITY, f64::NEG_INFINITY));
        assert_eq!(
            minmax(&[1.0, f64::NEG_INFINITY, 2.0]),
            (f64::NEG_INFINITY, 2.0)
        );
    }

    #[test]
    fn minmax_matches_scalar_on_long_random_runs() {
        let mut seed = 42;
        for len in [1000, 4097, 100_003] {
            let y: Vec<f64> = (0..len).map(|_| rng(&mut seed)).collect();
            assert_eq!(minmax(&y), minmax_scalar(&y));
        }
    }

    #[test]
    fn gallop_finds_the_boundary() {
        let xs: Vec<f64> = (0..100).map(f64::from).collect();
        for from in [0, 3, 50, 99, 100] {
            for edge in [-1.0, 0.0, 0.5, 7.0, 64.0, 99.5, 1e9] {
                let want = from + xs[from..].partition_point(|&x| x < edge);
                assert_eq!(
                    gallop(&xs, from, 100, edge),
                    want,
                    "from {from} edge {edge}"
                );
            }
        }
    }

    /// Naive oracle: bucket every sample by its pixel column, then min/max per bucket.
    fn naive_columns(xs: &[f64], ys: &[f64], sx: &Scale, cols: usize) -> Vec<(f64, f64)> {
        let mut b = vec![(f64::INFINITY, f64::NEG_INFINITY); cols];
        let (x_lo, x_hi) = (sx.value(sx.p0), sx.value(sx.p1));
        for (&x, &y) in xs.iter().zip(ys) {
            if x < x_lo || x > x_hi {
                continue;
            }
            let mut c = 0;
            while c + 1 < cols && x >= sx.value(sx.p0 + (c + 1) as f32) {
                c += 1;
            }
            b[c] = (b[c].0.min(y), b[c].1.max(y));
        }
        b
    }

    #[test]
    fn dense_decimation_equals_naive_bucketing() {
        let mut seed = 7;
        let n = 50_000;
        let xs: Vec<f64> = (0..n).map(|i| f64::from(i) * 1e-6).collect();
        let ys: Vec<f64> = (0..n).map(|_| rng(&mut seed)).collect();
        for log in [false, true] {
            let (lo, hi) = (0.0123 + f64::from(u8::from(log)) * 1e-4, 0.0377);
            let sx = Scale::new(lo, hi, log, 10.0, 310.0);
            let sy = Scale::new(-1.0, 1.0, false, 200.0, 0.0);
            let mut pts = Vec::new();
            decimate(&xs, &ys, &sx, &sy, &mut pts);
            let want = naive_columns(&xs, &ys, &sx, 300);
            // Interior vertices come in pairs per column; strip the two edge anchors.
            let inner = &pts[1..pts.len() - 1];
            assert_eq!(inner.len(), 600);
            for (c, pair) in inner.chunks_exact(2).enumerate() {
                let (a, b) = (pair[0][1].min(pair[1][1]), pair[0][1].max(pair[1][1]));
                assert_eq!(
                    (a, b),
                    (sy.px(want[c].1), sy.px(want[c].0)),
                    "column {c} log {log}"
                );
                assert_eq!(pair[0][0], 10.0 + c as f32 + 0.5);
            }
            // Same result with the scalar kernel.
            let mut scalar = Vec::new();
            decimate_with(minmax_scalar, &xs, &ys, &sx, &sy, &mut scalar);
            assert_eq!(pts, scalar);
        }
    }

    #[test]
    fn sparse_view_draws_every_sample_plus_edge_neighbours() {
        let xs = [0.0, 1.0, 2.0, 3.0, 4.0];
        let ys = [0.0, 1.0, 0.0, 1.0, 0.0];
        let sx = Scale::new(1.5, 2.5, false, 0.0, 100.0);
        let sy = Scale::new(0.0, 1.0, false, 100.0, 0.0);
        let mut pts = Vec::new();
        decimate(&xs, &ys, &sx, &sy, &mut pts);
        assert_eq!(pts, vec![[-50.0, 0.0], [50.0, 100.0], [150.0, 0.0]]);
        pts.clear();
        decimate(&[], &[], &sx, &sy, &mut pts);
        decimate(&[2.0, 1.0], &[f64::NAN, 1.0], &sx, &sy, &mut pts);
        assert_eq!(pts, vec![[-50.0, 0.0]]); // descending: point by point, NaN skipped
    }

    #[test]
    fn extent_respects_window_and_steps() {
        let xs = [0.0, 1.0, 2.0, 0.0, 1.0, 2.0];
        let ys = [5.0, -1.0, 7.0, 3.0, 9.0, -4.0];
        assert_eq!(extent(&xs, &ys, &[0..3, 3..6], 0.5, 1.5), (-1.0, 9.0));
        let (all, first) = (0..6u32, 0..3u32);
        let all = std::slice::from_ref(&all);
        assert_eq!(extent(&xs, &ys, all, 0.0, 2.0), (-4.0, 9.0)); // not ascending: whole
        let first = std::slice::from_ref(&first);
        assert_eq!(
            extent(&xs, &ys, first, 5.0, 6.0),
            (f64::INFINITY, f64::NEG_INFINITY)
        );
    }

    /// `cargo test --release -p schemify-wave -- --ignored --nocapture bench_decimation`
    #[test]
    #[ignore = "release-mode benchmark; run explicitly"]
    fn bench_decimation() {
        use std::hint::black_box;
        use std::time::Instant;
        let mut seed = 1;
        let n = 1_000_000;
        let xs: Vec<f64> = (0..n).map(|i| f64::from(i) * 1e-9).collect();
        let ys: Vec<f64> = (0..n).map(|_| rng(&mut seed)).collect();
        let time = |f: &dyn Fn() -> usize| {
            let mut best = f64::INFINITY;
            for _ in 0..50 {
                let t = Instant::now();
                black_box(f());
                best = best.min(t.elapsed().as_secs_f64() * 1e3);
            }
            best
        };
        let mm_v = time(&|| black_box(minmax(black_box(&ys))).0 as usize);
        let mm_s = time(&|| black_box(minmax_scalar(black_box(&ys))).0 as usize);
        // 64k samples (512 KiB) stay in L2: the compute-bound ratio.
        let l2 = &ys[..65_536];
        let l2_v = time(&|| black_box(minmax(black_box(l2))).0 as usize);
        let l2_s = time(&|| black_box(minmax_scalar(black_box(l2))).0 as usize);
        println!(
            "64k f64 (L2): minmax vector {:.1} µs, scalar {:.1} µs ({:.1}x)",
            l2_v * 1e3,
            l2_s * 1e3,
            l2_s / l2_v
        );
        let sx = Scale::new(0.0, xs[n as usize - 1], false, 0.0, 1500.0);
        let sy = Scale::new(-1.0, 1.0, false, 400.0, 0.0);
        let dec = |k: fn(&[f64]) -> (f64, f64)| {
            let mut out = Vec::with_capacity(4000);
            decimate_with(k, black_box(&xs), black_box(&ys), &sx, &sy, &mut out);
            out.len()
        };
        let d_v = time(&|| dec(minmax));
        let d_s = time(&|| dec(minmax_scalar));
        println!(
            "1M f64: minmax vector {mm_v:.3} ms, scalar {mm_s:.3} ms ({:.1}x); \
             decimate to 1500 columns: vector {d_v:.3} ms, scalar {d_s:.3} ms ({:.1}x)",
            mm_s / mm_v,
            d_s / d_v
        );
        // Digital lanes and histogram bins over the same million samples.
        let lr = |thr: Option<f64>| {
            let mut out = Vec::with_capacity(1500);
            logic_runs(black_box(&xs), black_box(&ys), thr, &sx, &mut out);
            out.len()
        };
        let lr_t = time(&|| lr(Some(0.0)));
        let bus: Vec<f64> = ys.iter().map(|v| (v * 4.0 + 4.0).floor()).collect();
        let lr_b = time(&|| {
            let mut out = Vec::with_capacity(1500);
            logic_runs(black_box(&xs), black_box(&bus), None, &sx, &mut out);
            out.len()
        });
        let hist = |log: bool| {
            let mut bins = [0u32; 64];
            bin_counts(
                black_box(&ys),
                if log { -3.0 } else { -1.0 },
                0.0,
                log,
                &mut bins,
            );
            bins[0] as usize
        };
        let (h_lin, h_log) = (time(&|| hist(false)), time(&|| hist(true)));
        println!(
            "1M f64: logic_runs threshold {lr_t:.3} ms, bus {lr_b:.3} ms; \
             bin_counts linear {h_lin:.3} ms, log {h_log:.3} ms"
        );
    }
}
