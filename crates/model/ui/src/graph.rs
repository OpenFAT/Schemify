//! Axis ticks for the waveform viewer's plots (D23; drawn by `render`): an axis range in, tick
//! positions and the digits that tell neighbouring ticks apart out.
//!
//! Contents:
//! - `pub fn ticks(lo: f64, hi: f64, max_ticks: u8, out: &mut Vec<f64>)`
//! - `pub fn log_ticks(lo: f64, hi: f64, max_ticks: u8, out: &mut Vec<f64>)`
//! - `pub fn tick_digits(ts: &[f64], log: bool) -> usize`

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
#[must_use]
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
