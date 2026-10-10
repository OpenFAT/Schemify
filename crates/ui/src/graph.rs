//! The `graph` widget's data model: per-pixel-column min/max decimation of caller-provided
//! columns, and axis ticks. Drawing lives in `render`; the waveform viewer (D23) builds on this.

/// Reduces samples `(xs[i], ys[i])` with `x ∈ [x0, x1]` to one `(min, max)` per pixel column,
/// writing `columns` entries into `min` and `max` (cleared first). Columns with no sample hold
/// NaN, so a gap in the data stays a gap on screen; NaN samples are ignored.
///
/// # Panics
///
/// Panics unless `xs.len() == ys.len()`, `x0 < x1` (both finite) and `columns > 0`. `xs` must be
/// ascending; that is checked only in debug builds (it costs a full scan).
///
/// # Time complexity
///
/// O(n + columns · log n): each column binary-searches its sample run, then folds it.
pub fn decimate(
    xs: &[f64],
    ys: &[f64],
    x0: f64,
    x1: f64,
    columns: u16,
    min: &mut Vec<f32>,
    max: &mut Vec<f32>,
) {
    assert_eq!(xs.len(), ys.len(), "x and y columns differ in length");
    assert!(x0.is_finite() && x1.is_finite() && x0 < x1, "empty x range");
    assert!(columns > 0, "zero-width graph");
    debug_assert!(
        xs.windows(2).all(|w| w[0] <= w[1]),
        "x column not ascending"
    );
    min.clear();
    max.clear();
    let w = (x1 - x0) / f64::from(columns);
    let mut i0 = xs.partition_point(|&x| x < x0);
    for c in 0..columns {
        let last = c + 1 == columns;
        let hi = if last { x1 } else { x0 + w * f64::from(c + 1) };
        let run = &xs[i0..];
        let n = if last {
            run.partition_point(|&x| x <= hi)
        } else {
            run.partition_point(|&x| x < hi)
        };
        let (lo, hi) = minmax(&ys[i0..i0 + n]);
        let empty = lo > hi;
        min.push(if empty { f32::NAN } else { lo as f32 });
        max.push(if empty { f32::NAN } else { hi as f32 });
        i0 += n;
    }
    debug_assert_eq!(min.len(), usize::from(columns));
}

/// Min and max of `y`, ignoring NaN; `(+inf, -inf)` when nothing is finite-comparable.
///
/// Four independent accumulators in the chunk loop let LLVM keep them in vector registers
/// (`minpd`/`maxpd`); the scalar tail is the same fold as [`minmax_scalar`], the test oracle.
fn minmax(y: &[f64]) -> (f64, f64) {
    let mut lo = [f64::INFINITY; 4];
    let mut hi = [f64::NEG_INFINITY; 4];
    let chunks = y.chunks_exact(4);
    let tail = chunks.remainder();
    for c in chunks {
        for l in 0..4 {
            lo[l] = lo[l].min(c[l]);
            hi[l] = hi[l].max(c[l]);
        }
    }
    let (mut a, mut b) = (
        lo.iter().copied().fold(f64::INFINITY, f64::min),
        hi.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    );
    for &v in tail {
        a = a.min(v);
        b = b.max(v);
    }
    (a, b)
}

#[cfg(test)]
fn minmax_scalar(y: &[f64]) -> (f64, f64) {
    y.iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| {
            (a.min(v), b.max(v))
        })
}

/// Writes "nice" tick positions (steps of 1, 2 or 5 × 10^k) covering `[lo, hi]` into `out`,
/// at most `max_ticks + 1` of them.
///
/// # Panics
///
/// Panics unless `lo < hi` (both finite) and `max_ticks >= 2`.
pub fn ticks(lo: f64, hi: f64, max_ticks: u8, out: &mut Vec<f64>) {
    assert!(
        lo.is_finite() && hi.is_finite() && lo < hi,
        "empty tick range"
    );
    assert!(max_ticks >= 2, "need at least two ticks");
    out.clear();
    let raw = (hi - lo) / f64::from(max_ticks);
    let mag = 10f64.powf(raw.log10().floor());
    let n = raw / mag;
    let step = mag
        * if n <= 1.0 {
            1.0
        } else if n <= 2.0 {
            2.0
        } else if n <= 5.0 {
            5.0
        } else {
            10.0
        };
    let first = (lo / step).ceil();
    for k in 0..=u32::from(max_ticks) {
        let v = (first + f64::from(k)) * step;
        if v > hi + step * 1e-9 {
            break;
        }
        out.push(v);
    }
    debug_assert!(out.len() <= usize::from(max_ticks) + 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: &mut u64) -> f64 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*seed >> 11) as f64) / ((1u64 << 53) as f64) * 2.0 - 1.0
    }

    #[test]
    fn minmax_matches_scalar_oracle_at_every_tail_length() {
        let mut seed = 7;
        for len in 0..=16 {
            let mut y: Vec<f64> = (0..len).map(|_| lcg(&mut seed)).collect();
            if len > 5 {
                y[3] = f64::NAN;
            }
            assert_eq!(minmax(&y), minmax_scalar(&y), "len {len}");
        }
    }

    #[test]
    fn decimate_equals_naive_bucketing() {
        let mut seed = 42;
        let n = 10_000;
        let xs: Vec<f64> = (0..n).map(|i| i as f64 * 0.001).collect();
        let ys: Vec<f64> = (0..n).map(|_| lcg(&mut seed)).collect();
        let (x0, x1, cols) = (1.234, 7.5, 300u16);
        let (mut mn, mut mx) = (Vec::new(), Vec::new());
        decimate(&xs, &ys, x0, x1, cols, &mut mn, &mut mx);
        // Naive oracle: assign each sample to its column by the same boundary arithmetic.
        let w = (x1 - x0) / f64::from(cols);
        let mut omn = vec![f64::INFINITY; cols as usize];
        let mut omx = vec![f64::NEG_INFINITY; cols as usize];
        for (&x, &y) in xs.iter().zip(&ys) {
            if x < x0 || x > x1 {
                continue;
            }
            let mut c = 0usize;
            while c + 1 < cols as usize && x >= x0 + w * (c + 1) as f64 {
                c += 1;
            }
            omn[c] = omn[c].min(y);
            omx[c] = omx[c].max(y);
        }
        for c in 0..cols as usize {
            assert_eq!(mn[c], omn[c] as f32, "col {c}");
            assert_eq!(mx[c], omx[c] as f32, "col {c}");
        }
    }

    #[test]
    fn sparse_data_leaves_nan_gaps() {
        let (mut mn, mut mx) = (Vec::new(), Vec::new());
        decimate(&[0.0, 10.0], &[1.0, 2.0], 0.0, 10.0, 10, &mut mn, &mut mx);
        assert_eq!((mn[0], mx[0]), (1.0, 1.0));
        assert_eq!((mn[9], mx[9]), (2.0, 2.0));
        assert!(mn[1..9].iter().all(|v| v.is_nan()));
        decimate(&[], &[], 0.0, 1.0, 3, &mut mn, &mut mx);
        assert!(mn.iter().all(|v| v.is_nan()) && mn.len() == 3);
    }

    #[test]
    fn ticks_are_nice_and_cover_range() {
        let mut t = Vec::new();
        ticks(24.62, 24.93, 8, &mut t);
        assert_eq!(t.len(), 6);
        assert!((t[0] - 24.65).abs() < 1e-9 && (t[1] - 24.7).abs() < 1e-9);
        ticks(-50.0, 60.0, 10, &mut t);
        assert_eq!(t.first(), Some(&-40.0));
        assert_eq!(t.last(), Some(&60.0));
    }
}
