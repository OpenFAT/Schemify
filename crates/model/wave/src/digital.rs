//! The per-mode display reductions besides the analog polyline: digital lanes (spec waves §4,
//! one [`Run`] per stretch of equal pixel columns, busy where a column's samples disagree) and
//! histogram bins (spec waves §5, sample counts per equal bin).
//!
//! Contents:
//! - `pub struct Run`
//! - `pub fn logic_runs(xs: &[f64], ys: &[f64], threshold: Option<f64>, sx: &Scale, out: &mut Vec<Run>)`
//! - `pub fn logic(y: f64, threshold: Option<f64>) -> u32`
//! - `pub fn bin_counts(y: &[f64], t0: f64, t1: f64, log: bool, bins: &mut [u32])`

use crate::axis::Scale;
use crate::decimate;

/// One run of equal pixel columns of a digital lane (spec waves §4): columns `c0..c1` counted
/// from the x scale's `p0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    /// First column.
    pub c0: u32,
    /// One past the last column.
    pub c1: u32,
    /// The logic or bus value; `None` when the samples in a column disagree (busy).
    pub value: Option<u32>,
}

/// Appends the digital runs of one sweep step to `out` (spec waves §4): per pixel column of
/// `sx`, the value held into it from the sample before plus every sample in it, `None` when
/// they disagree; equal neighbours merge, columns before the first or after the last sample are
/// left out. `threshold` thresholds an analog trace; `None` reads a bus's values. Runs already
/// in `out` are kept and never merged with.
///
/// # Panics
///
/// Panics if the columns differ in length or `sx` does not increase left to right.
///
/// # Time complexity
///
/// O(visible samples + columns · log(samples per column)); nothing for a non-ascending step.
pub fn logic_runs(xs: &[f64], ys: &[f64], threshold: Option<f64>, sx: &Scale, out: &mut Vec<Run>) {
    assert_eq!(xs.len(), ys.len(), "x and y columns differ in length");
    let cols = decimate::pixel_columns(sx);
    let n = xs.len();
    if n == 0 || xs[0] > xs[n - 1] || !xs[0].is_finite() {
        // ponytail: a non-ascending step (an X-Y sweep) has no time order to hold values in.
        return;
    }
    let before = out.len();
    let edge = |c: usize| sx.value(sx.p0 + c as f32);
    // `i`: first sample at or after the current column's left edge.
    let mut i = xs.partition_point(|&x| x < edge(0));
    for c in 0..cols {
        let j = decimate::gallop(xs, i, n, edge(c + 1));
        let col = &ys[i..j];
        // The value held into the column needs a sample before it and one at or after it;
        // without one the column's first sample sets the value.
        let first = if i > 0 && i < n {
            ys[i - 1]
        } else if let Some(&y) = col.first() {
            y
        } else {
            i = j;
            continue;
        };
        i = j;
        let v0 = logic(first, threshold);
        // A fold with no early exit: one compare and or per sample, no branch.
        let busy = col
            .iter()
            .fold(false, |b, &y| b | (logic(y, threshold) != v0));
        let value = (!busy).then_some(v0);
        let c = c as u32;
        let mine = out.len() > before;
        match out.last_mut() {
            Some(r) if mine && r.c1 == c && r.value == value => r.c1 = c + 1,
            _ => out.push(Run {
                c0: c,
                c1: c + 1,
                value,
            }),
        }
    }
    let runs = &out[before..];
    debug_assert!(
        runs.windows(2).all(|w| w[0].c1 <= w[1].c0),
        "runs are ordered and disjoint"
    );
    debug_assert!(runs.iter().all(|r| r.c0 < r.c1), "every run spans a column");
    debug_assert!(
        runs.last().is_none_or(|r| r.c1 as usize <= cols),
        "runs stay inside the scale"
    );
}

/// The logic value of sample `y`: `y >= threshold` as 0/1, or the bus value `y` itself when
/// there is no threshold (NaN reads 0).
#[inline]
#[must_use]
pub fn logic(y: f64, threshold: Option<f64>) -> u32 {
    match threshold {
        Some(t) => u32::from(y >= t),
        None => y as u32,
    }
}

/// Adds the samples of `y` to `bins` equal bins over `[t0, t1]` in transformed units (`log10`
/// when `log`, spec waves §5): bin `⌊(t − t0) · bins / (t1 − t0)⌋`, `t1` itself in the last bin.
/// Samples outside the range or not finite after the transform are not counted.
///
/// # Panics
///
/// Panics if `bins` is empty or `t0 < t1` does not hold with both finite.
pub fn bin_counts(y: &[f64], t0: f64, t1: f64, log: bool, bins: &mut [u32]) {
    assert!(!bins.is_empty(), "no bins");
    assert!(t0.is_finite(), "bin range start {t0} is not finite");
    assert!(t1.is_finite(), "bin range end {t1} is not finite");
    assert!(t0 < t1, "empty bin range {t0} .. {t1}");
    let n = bins.len();
    let k = n as f64 / (t1 - t0);
    debug_assert!(
        bins.iter()
            .all(|&c| c as usize <= (u32::MAX as usize).saturating_sub(y.len())),
        "counts never overflow"
    );
    // ponytail: a branch per sample. Branchless (add 0 to a clamped bin) measured 1.7x slower:
    // the clamped misses serialize on one bin's store. Four sub-histograms if a profile asks.
    for &v in y {
        let t = if log { v.log10() } else { v };
        if !(t >= t0 && t <= t1) {
            continue; // outside, or NaN (a log of a value <= 0 is -inf or NaN)
        }
        let b = (((t - t0) * k) as usize).min(n - 1);
        bins[b] += 1;
    }
}
