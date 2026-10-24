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
        // NaN-ignoring; the kernel and its oracle test live in `wave`.
        let (lo, hi) = schemify_wave::minmax(&ys[i0..i0 + n]);
        let empty = lo > hi;
        min.push(if empty { f32::NAN } else { lo as f32 });
        max.push(if empty { f32::NAN } else { hi as f32 });
        i0 += n;
    }
    debug_assert_eq!(min.len(), usize::from(columns));
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

/// Writes tick positions for a log axis over `[lo, hi]` into `out`: every decade (every k-th one
/// when there are more than `max_ticks`), plus 2× and 5× when at most three decades show; below
/// two decades it falls back to linear [`ticks`], which read fine on a short log axis.
///
/// # Panics
///
/// Panics unless `0 < lo < hi` (both finite) and `max_ticks >= 2`.
pub fn log_ticks(lo: f64, hi: f64, max_ticks: u8, out: &mut Vec<f64>) {
    assert!(
        lo > 0.0 && hi.is_finite() && lo < hi,
        "empty log tick range"
    );
    assert!(max_ticks >= 2, "need at least two ticks");
    let (d0, d1) = (lo.log10().ceil() as i32, hi.log10().floor() as i32);
    if d1 - d0 + 1 < 2 {
        ticks(lo, hi, max_ticks, out);
        return;
    }
    out.clear();
    let decades = (d1 - d0 + 1) as u32;
    let every = decades.div_ceil(u32::from(max_ticks)).max(1) as i32;
    let minor = decades <= 3;
    // Bounded: at most `decades` (≤ 600 for f64) iterations, 3 pushes each.
    for d in (d0 - 1)..=d1 {
        let base = 10f64.powi(d);
        if d >= d0 && (d - d0) % every == 0 {
            out.push(base);
        }
        if minor {
            out.extend(
                [2.0 * base, 5.0 * base]
                    .into_iter()
                    .filter(|v| *v >= lo && *v <= hi),
            );
        }
    }
    out.sort_by(f64::total_cmp);
    debug_assert!(
        out.iter()
            .all(|v| *v >= lo * (1.0 - 1e-12) && *v <= hi * (1.0 + 1e-12))
    );
}

/// Significant digits that tell neighbouring ticks `ts` apart: at least 3, more when the view
/// is narrow next to its distance from zero (a 50 ps window at 4.2 ms needs 9), at most 15. A
/// log axis, fewer than two ticks, or ticks not ascending get 3.
pub fn tick_digits(ts: &[f64], log: bool) -> usize {
    let (Some(&a), Some(&b)) = (ts.first(), ts.get(1)) else {
        return 3;
    };
    let big = ts.iter().fold(0.0f64, |m, t| m.max(t.abs()));
    if log || b <= a || big == 0.0 {
        return 3;
    }
    let d = ((big / (b - a)).log10().ceil() as i64 + 1).clamp(3, 15) as usize;
    debug_assert!((3..=15).contains(&d));
    d
}

/// The `graph` widget's tick label: `v` rounded to 6 decimal places in Rust's shortest
/// round-trip form (`0.1 + 0.2` reads `0.3`, `1.0` reads `1`), so float noise from the tick
/// step never shows.
pub fn tick_label(v: f64) -> String {
    format!("{}", (v * 1e6).round() / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_digits_tell_ticks_apart() {
        // (ticks, log, digits)
        let cases: [(&[f64], bool, usize); 6] = [
            (&[0.0, 5e-6, 10e-6], false, 3),
            (&[-100.0, -80.0, -60.0], false, 3),
            (&[1e3, 1e4], true, 3),
            (&[1.0], false, 3),
            (&[], false, 3),
            (&[2.0, 1.0], false, 3), // descending
        ];
        for (ts, log, d) in cases {
            assert_eq!(tick_digits(ts, log), d, "{ts:?}");
        }
        let narrow = [4.2e-3, 4.2e-3 + 1e-11, 4.2e-3 + 2e-11];
        let d = tick_digits(&narrow, false);
        let s: Vec<String> = narrow
            .iter()
            .map(|&t| schemify_wave::format_si(t, d))
            .collect();
        assert!(s[0] != s[1] && s[1] != s[2], "{s:?}");
        assert_eq!(tick_digits(&[1e15, 1e15 + 1.0], false), 15, "capped");
    }

    #[test]
    fn tick_labels_hide_float_noise() {
        let cases = [
            (0.1 + 0.2, "0.3"),
            (1.0, "1"),
            (-2.5, "-2.5"),
            (1e-7, "0"),
            (1.234_567_89, "1.234568"),
            (0.0, "0"),
        ];
        for (v, s) in cases {
            assert_eq!(tick_label(v), s, "{v}");
        }
    }

    fn lcg(seed: &mut u64) -> f64 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*seed >> 11) as f64) / ((1u64 << 53) as f64) * 2.0 - 1.0
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
    fn log_ticks_mark_decades() {
        let mut t = Vec::new();
        log_ticks(1e3, 1e8, 8, &mut t);
        assert_eq!(t, vec![1e3, 1e4, 1e5, 1e6, 1e7, 1e8]);
        log_ticks(1.0, 100.0, 8, &mut t); // three decades: 2× and 5× too
        assert_eq!(t, vec![1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0]);
        log_ticks(1e-15, 1e15, 6, &mut t);
        assert_eq!(t.len(), 6);
        log_ticks(2.0, 9.0, 5, &mut t); // inside one decade: linear
        assert_eq!(t, vec![2.0, 4.0, 6.0, 8.0]);
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
