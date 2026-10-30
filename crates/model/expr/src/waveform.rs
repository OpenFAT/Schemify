//! One sweep step of a waveform (spec §5): x and y slices of one step → a
//! derivative, an integral, or one measured number. Pure functions; `measure`
//! cuts columns into steps and calls these, so a step boundary is never crossed.
//! "No answer" is NaN, which `measure` reports as `X010`.
//!
//! Contents:
//! - `pub(crate) enum Margin`
//! - `pub(crate) fn deriv_step(x, y, out)`, `pub(crate) fn integ_step(x, y, out)`
//! - `pub(crate) fn reduce(builtin, x, y) -> f64`, `pub(crate) fn interp(x, y, x0) -> f64`,
//!   `pub(crate) fn cross(x, y, level, edge, nth) -> f64`, `pub(crate) fn settle(x, y, tol) -> f64`,
//!   `pub(crate) fn margin(which, re, im) -> f64`

use crate::builtin::Builtin;
use crate::dmath::{self, Complex};

/// Which crossing [`margin`] measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Margin {
    /// Gain margin (dB) at the first -180° phase crossing.
    Gain,
    /// Phase margin (°) at the first unity-gain crossing.
    Phase,
}

/// dy/dx: central differences inside, one-sided at the step ends.
pub(crate) fn deriv_step(x: &[f64], y: &[f64], out: &mut [f64]) {
    assert_eq!(x.len(), y.len());
    assert_eq!(y.len(), out.len());
    let n = y.len();
    if n < 2 {
        out.fill(0.0);
        return;
    }
    out[0] = (y[1] - y[0]) / (x[1] - x[0]);
    out[n - 1] = (y[n - 1] - y[n - 2]) / (x[n - 1] - x[n - 2]);
    for i in 1..n - 1 {
        out[i] = (y[i + 1] - y[i - 1]) / (x[i + 1] - x[i - 1]);
    }
}

/// Cumulative trapezoid integral from the step start.
pub(crate) fn integ_step(x: &[f64], y: &[f64], out: &mut [f64]) {
    assert_eq!(x.len(), y.len());
    assert_eq!(y.len(), out.len());
    let mut acc = 0.0;
    for i in 0..y.len() {
        if i > 0 {
            acc += (x[i] - x[i - 1]) * (y[i] + y[i - 1]) * 0.5;
        }
        out[i] = acc;
    }
    // The integral starts at zero at the step start.
    debug_assert!(out.first().is_none_or(|&first| first == 0.0));
}

/// `avg rms ymax ymin pp` of one step; NaN for an empty step.
pub(crate) fn reduce(builtin: Builtin, x: &[f64], y: &[f64]) -> f64 {
    debug_assert_eq!(x.len(), y.len());
    debug_assert!(matches!(
        builtin,
        Builtin::Avg | Builtin::Rms | Builtin::Ymax | Builtin::Ymin | Builtin::Pp
    ));
    if y.is_empty() {
        return f64::NAN;
    }
    let span = x[x.len() - 1] - x[0];
    let max = || y.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let min = || y.iter().copied().fold(f64::INFINITY, f64::min);
    match builtin {
        Builtin::Avg if y.len() == 1 => y[0],
        Builtin::Avg => trapz(x, y, |v| v) / span,
        Builtin::Rms if y.len() == 1 => y[0].abs(),
        Builtin::Rms => (trapz(x, y, |v| v * v) / span).sqrt(),
        Builtin::Ymax => max(),
        Builtin::Ymin => min(),
        _ => max() - min(),
    }
}

/// Linear interpolation at `x0` on the first segment containing it; NaN outside.
pub(crate) fn interp(x: &[f64], y: &[f64], x0: f64) -> f64 {
    debug_assert_eq!(x.len(), y.len());
    debug_assert!(x0.is_finite(), "x values are finite");
    if x.len() == 1 && x[0] == x0 {
        return y[0];
    }
    for k in 0..x.len().saturating_sub(1) {
        let (a, b) = (x[k], x[k + 1]);
        if (a <= x0 && x0 <= b) || (b <= x0 && x0 <= a) {
            return if a == b {
                y[k]
            } else {
                y[k] + (y[k + 1] - y[k]) * (x0 - a) / (b - a)
            };
        }
    }
    f64::NAN
}

/// x of the `nth` crossing of `level` (`edge` 1 rising, -1 falling, 0 either),
/// linearly interpolated; NaN if there is none. A rising crossing goes from
/// below to at-or-above, a falling one from above to at-or-below.
pub(crate) fn cross(x: &[f64], y: &[f64], level: f64, edge: i64, nth: i64) -> f64 {
    debug_assert!(nth >= 1);
    debug_assert!((-1..=1).contains(&edge));
    debug_assert_eq!(x.len(), y.len());
    let mut seen = 0;
    for k in 0..y.len().saturating_sub(1) {
        let (s0, s1) = (y[k] - level, y[k + 1] - level);
        let rising = s0 < 0.0 && s1 >= 0.0;
        let falling = s0 > 0.0 && s1 <= 0.0;
        let hit = match edge {
            1 => rising,
            -1 => falling,
            _ => rising || falling,
        };
        if hit {
            seen += 1;
            if seen == nth {
                return x[k] + (x[k + 1] - x[k]) * (-s0) / (s1 - s0);
            }
        }
    }
    f64::NAN
}

/// First x after which `|y - y_final| <= tol` holds to the end of the step.
pub(crate) fn settle(x: &[f64], y: &[f64], tol: f64) -> f64 {
    debug_assert!(tol >= 0.0);
    debug_assert_eq!(x.len(), y.len());
    let Some(&last) = y.last() else {
        return f64::NAN;
    };
    match y.iter().rposition(|v| (v - last).abs() > tol) {
        None => x[0],
        Some(k) => {
            debug_assert!(k + 1 < y.len(), "the last point is always settled");
            x[k + 1]
        }
    }
}

/// Gain margin (dB) at the first -180° phase crossing, or phase margin (°)
/// at the first unity-gain crossing, of a loop gain whose phase starts near
/// 0°. NaN when the crossing does not exist.
pub(crate) fn margin(which: Margin, re: &[f64], im: &[f64]) -> f64 {
    debug_assert!(im.is_empty() || im.len() == re.len());
    let n = re.len();
    let z = |i: usize| Complex::new(re[i], if im.is_empty() { 0.0 } else { im[i] });
    let db = |i: usize| 20.0 * libm::log10(dmath::norm(z(i)));
    let mut prev_ph = 0.0;
    let mut phase = Vec::with_capacity(n);
    for i in 0..n {
        let mut p = libm::atan2(z(i).im, z(i).re).to_degrees();
        if i > 0 {
            p -= 360.0 * ((p - prev_ph) / 360.0).round();
        }
        phase.push(p);
        prev_ph = p;
    }
    debug_assert_eq!(phase.len(), n);
    for i in 0..n.saturating_sub(1) {
        if which == Margin::Gain {
            let (p0, p1) = (phase[i] + 180.0, phase[i + 1] + 180.0);
            if p0 > 0.0 && p1 <= 0.0 {
                let t = p0 / (p0 - p1);
                return -(db(i) + t * (db(i + 1) - db(i)));
            }
        } else {
            let (m0, m1) = (db(i), db(i + 1));
            if m0 >= 0.0 && m1 < 0.0 {
                let t = m0 / (m0 - m1);
                return 180.0 + phase[i] + t * (phase[i + 1] - phase[i]);
            }
        }
    }
    f64::NAN
}

// ---- private ----

/// Trapezoid integral of `g(y)` over the step.
fn trapz(x: &[f64], y: &[f64], g: impl Fn(f64) -> f64) -> f64 {
    debug_assert_eq!(x.len(), y.len());
    debug_assert!(y.len() >= 2, "single points are handled by the caller");
    (1..y.len())
        .map(|i| (x[i] - x[i - 1]) * (g(y[i]) + g(y[i - 1])) * 0.5)
        .sum()
}
