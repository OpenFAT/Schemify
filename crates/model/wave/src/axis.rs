//! Axes: a visible range in data units ([`Axis`], held by the viewer tables) and that range
//! mapped onto pixels ([`Scale`], built per frame by the painter). Both work in transformed
//! units `t` (`t = v`, or `log10 v` on a log axis), so zoom, pan and box fractions mean the same
//! on linear and log axes (spec waves §1).
//!
//! Contents:
//! - `pub enum AxisScale`: `Linear`, `Log`
//! - `pub struct Axis`: `scale`, `valid`, `at`, `pub(crate) t`
//! - `pub struct Scale`: `new`, `px`, `value`
//! - `pub(crate) const UNIT: Axis`
//! - `pub(crate) fn fit_range(lo, hi, scale, pad) -> Option<Axis>`
//! - `pub(crate) fn zoom(a: Axis, at: f64, factor: f64) -> Axis`
//! - `pub(crate) fn window(a: Axis, f0: f64, f1: f64) -> Axis`
//! - `pub(crate) fn span(x: &[f64], scale: AxisScale) -> MinMax`

use crate::decimate::{self, MinMax};

/// How an axis maps a value `v` to its transformed units `t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AxisScale {
    /// `t = v`.
    Linear,
    /// `t = log10 v` (decades); only positive values have a place on the axis.
    Log,
}

/// One axis' visible range in data units; on a log axis both ends are positive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    /// Low end (left, bottom).
    pub lo: f64,
    /// High end.
    pub hi: f64,
    /// Linear or logarithmic.
    pub scale: AxisScale,
}

/// One axis mapped onto pixels: a value `v` goes to `t = v` (or `log10 v` on a log axis), then
/// linearly from `[t0, t1]` onto `[p0, p1]`. `p1 < p0` flips the axis (screen y grows down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale {
    /// Transformed value at `p0`.
    pub t0: f64,
    /// Transformed value at `p1`.
    pub t1: f64,
    /// The value transform before the linear map.
    pub scale: AxisScale,
    /// Pixel of `t0`.
    pub p0: f32,
    /// Pixel of `t1`.
    pub p1: f32,
}

/// The axis of an empty pane: `0 .. 1`, linear.
pub(crate) const UNIT: Axis = Axis {
    lo: 0.0,
    hi: 1.0,
    scale: AxisScale::Linear,
};

/// Pixels beyond either end of an axis that [`Scale::px`] still resolves, in axis lengths;
/// anything further is clamped there, so paths stay finite and short for the rasterizer.
const FAR: f64 = 64.0;

/// An axis covering `[lo, hi]` padded by `pad` spans on both sides (in transformed units). A
/// single value widens to ±1 (or one decade each way on a log axis); `pad == 0` keeps the exact
/// data ends. `None` when the range is empty, not finite, or not positive on a log axis.
pub(crate) fn fit_range(lo: f64, hi: f64, scale: AxisScale, pad: f64) -> Option<Axis> {
    assert!(pad.is_finite(), "padding {pad} is a finite fraction");
    assert!(pad >= 0.0, "padding {pad} widens, never narrows");
    if !(lo.is_finite() && hi.is_finite()) || lo > hi || (scale == AxisScale::Log && lo <= 0.0) {
        return None;
    }
    let (t0, t1) = (to_t(lo, scale), to_t(hi, scale));
    let wide = resolvable(t0, t1);
    if wide && pad == 0.0 {
        // Exact data ends: a full zoom shows `0 .. 20u`, not a log10 round trip of them.
        let a = Axis { lo, hi, scale };
        return a.valid().then_some(a);
    }
    let w = t1 - t0;
    let d = if !wide {
        match scale {
            AxisScale::Linear => t0.abs().max(1.0) * 0.5,
            AxisScale::Log => 1.0,
        }
    } else {
        pad * w
    };
    let fitted = Axis::from_t(t0 - d, t1 + d, scale);
    if let (AxisScale::Linear, Some(a)) = (scale, fitted) {
        // A linear fit is exact arithmetic on the ends (the log one rounds through `10^t`).
        assert!(a.lo <= lo, "the fit {a:?} cuts off the low end {lo}");
        assert!(a.hi >= hi, "the fit {a:?} cuts off the high end {hi}");
    }
    fitted
}

/// `a` zoomed by `factor` about fraction `at`; `a` unchanged when the result is not a valid
/// axis (zooming past f64 resolution, a non-finite factor).
pub(crate) fn zoom(a: Axis, at: f64, factor: f64) -> Axis {
    assert!(a.valid(), "the viewer holds only valid axes, got {a:?}");
    if !(factor.is_finite() && factor > 0.0 && at.is_finite()) {
        return a;
    }
    let (t0, t1) = a.ts();
    let c = t0 + at * (t1 - t0);
    let zoomed = Axis::from_t(c - (c - t0) * factor, c + (t1 - c) * factor, a.scale).unwrap_or(a);
    assert_eq!(zoomed.scale, a.scale, "zooming keeps the scale");
    zoomed
}

/// `a` narrowed (or moved) to the fractions `[f0, f1]` of its span; unchanged when invalid or
/// narrower than 1e-9 of the span.
pub(crate) fn window(a: Axis, f0: f64, f1: f64) -> Axis {
    assert!(a.valid(), "the viewer holds only valid axes, got {a:?}");
    if !(f0.is_finite() && f1.is_finite()) || f1 - f0 < 1e-9 {
        return a;
    }
    let (t0, t1) = a.ts();
    let w = t1 - t0;
    let windowed = Axis::from_t(t0 + f0 * w, t0 + f1 * w, a.scale).unwrap_or(a);
    assert_eq!(
        windowed.scale, a.scale,
        "panning and box zoom keep the scale"
    );
    windowed
}

/// Min and max of `x` skipping NaN, with the min taken over positive values on a log axis;
/// `min` is `+inf` (and `max` `-inf` off a log axis) when nothing qualifies.
pub(crate) fn span(x: &[f64], scale: AxisScale) -> MinMax {
    let all = decimate::minmax(x);
    if scale == AxisScale::Linear || all.min > 0.0 {
        return all;
    }
    // ponytail: a second scalar pass, only on log axes whose data reaches zero.
    let positive = x.iter().copied().filter(|&v| v > 0.0);
    let min = positive.fold(f64::INFINITY, f64::min);
    assert!(
        min > 0.0,
        "a log span's low end is positive (+inf when none is)"
    );
    MinMax { min, max: all.max }
}

impl Axis {
    /// The axis mapped onto pixels `p0` (at `lo`) to `p1` (at `hi`).
    ///
    /// # Panics
    ///
    /// As [`Scale::new`]: panics if `p0 == p1` or either is not finite, or if the axis is not
    /// ordered and finite (every axis [`Waves`](crate::Waves) hands out is).
    #[must_use]
    pub fn scale(&self, p0: f32, p1: f32) -> Scale {
        Scale::new(self.lo, self.hi, self.scale, p0, p1)
    }

    /// Finite, ordered, positive on a log axis, and not narrower than f64 can resolve. Every
    /// axis the viewer holds satisfies this; requests that would break it are refused.
    #[must_use]
    pub fn valid(&self) -> bool {
        let (t0, t1) = self.ts();
        self.lo.is_finite()
            && self.hi.is_finite()
            && (self.scale == AxisScale::Linear || self.lo > 0.0)
            && t0 < t1
            && resolvable(t0, t1)
    }

    /// The data value at fraction `f` of the span (0 = `lo`, 1 = `hi`), in transformed units.
    #[must_use]
    pub fn at(&self, f: f64) -> f64 {
        let (t0, t1) = self.ts();
        from_t(t0 + f * (t1 - t0), self.scale)
    }

    /// Value `v` in this axis' transformed units (`log10 v` on a log axis).
    pub(crate) fn t(&self, v: f64) -> f64 {
        to_t(v, self.scale)
    }
}

impl Scale {
    /// Maps the data range `[lo, hi]` onto pixels `[p0, p1]`.
    ///
    /// # Panics
    ///
    /// Panics unless `lo < hi` (both finite, and positive on a log scale) and `p0 != p1`
    /// (both finite).
    #[must_use]
    pub fn new(lo: f64, hi: f64, scale: AxisScale, p0: f32, p1: f32) -> Scale {
        assert!(lo.is_finite(), "axis low end {lo} is not finite");
        assert!(hi.is_finite(), "axis high end {hi} is not finite");
        assert!(lo < hi, "empty axis range {lo} .. {hi}");
        assert!(
            scale == AxisScale::Linear || lo > 0.0,
            "log axis needs a positive range, got {lo} .. {hi}"
        );
        assert!(p0.is_finite(), "pixel {p0} is not finite");
        assert!(p1.is_finite(), "pixel {p1} is not finite");
        assert!(p0 != p1, "empty pixel range at {p0}");
        let s = Scale {
            t0: to_t(lo, scale),
            t1: to_t(hi, scale),
            scale,
            p0,
            p1,
        };
        assert!(s.t0 < s.t1, "axis range collapses after the log transform");
        s
    }

    /// Pixel of value `v`, clamped to 64 axis lengths beyond the ends. On a log axis a value
    /// `<= 0` (or NaN) maps to the far low end; on a linear axis NaN stays NaN.
    #[inline]
    #[must_use]
    pub fn px(&self, v: f64) -> f32 {
        debug_assert!(self.t0 < self.t1, "a scale built by Scale::new is ordered");
        let t = match self.scale {
            AxisScale::Linear => v,
            AxisScale::Log if v > 0.0 => v.log10(),
            AxisScale::Log => return self.p0 - (self.p1 - self.p0) * FAR as f32,
        };
        let f = ((t - self.t0) / (self.t1 - self.t0)).clamp(-FAR, 1.0 + FAR);
        (f64::from(self.p0) + f * f64::from(self.p1 - self.p0)) as f32
    }

    /// Value at pixel `p` (the inverse of [`Scale::px`] inside the clamp).
    #[inline]
    #[must_use]
    pub fn value(&self, p: f32) -> f64 {
        debug_assert!(
            self.p0 != self.p1,
            "a scale built by Scale::new has pixel extent"
        );
        let f = f64::from(p - self.p0) / f64::from(self.p1 - self.p0);
        from_t(self.t0 + f * (self.t1 - self.t0), self.scale)
    }
}

// ---- private ----

#[inline]
fn to_t(v: f64, scale: AxisScale) -> f64 {
    match scale {
        AxisScale::Linear => v,
        AxisScale::Log => v.log10(),
    }
}

#[inline]
fn from_t(t: f64, scale: AxisScale) -> f64 {
    match scale {
        AxisScale::Linear => t,
        AxisScale::Log => 10f64.powf(t),
    }
}

/// True when `[t0, t1]` is wider than f64 resolves at its magnitude (1e-12 relative).
fn resolvable(t0: f64, t1: f64) -> bool {
    t1 - t0 > 1e-12 * t0.abs().max(t1.abs()).max(1e-300)
}

impl Axis {
    /// The ends in transformed units.
    fn ts(&self) -> (f64, f64) {
        (self.t(self.lo), self.t(self.hi))
    }

    /// The axis whose transformed ends are `t0`, `t1`, when that is [`Axis::valid`].
    fn from_t(t0: f64, t1: f64, scale: AxisScale) -> Option<Axis> {
        let a = Axis {
            lo: from_t(t0, scale),
            hi: from_t(t1, scale),
            scale,
        };
        a.valid().then_some(a)
    }
}

#[cfg(test)]
mod tests {
    use super::AxisScale::{Linear, Log};
    use super::*;

    #[test]
    fn scale_clamps_and_inverts() {
        let s = Scale::new(1.0, 1000.0, Log, 0.0, 300.0);
        assert_eq!(s.px(10.0), 100.0);
        assert!((s.value(200.0) - 100.0).abs() < 1e-9);
        assert_eq!(s.px(0.0), -300.0 * FAR as f32);
        assert_eq!(s.px(-1.0), -300.0 * FAR as f32);
        assert_eq!(s.px(1e300), 300.0 * (1.0 + FAR as f32));
        assert_eq!(s.px(f64::NAN), -300.0 * FAR as f32);
        let y = Scale::new(-1.0, 1.0, Linear, 100.0, 0.0);
        assert_eq!((y.px(-1.0), y.px(1.0)), (100.0, 0.0));
        assert!(y.px(f64::NAN).is_nan());
    }

    #[test]
    fn fit_range_pads_widens_and_refuses() {
        let a = |lo, hi, scale| Axis { lo, hi, scale };
        assert_eq!(
            fit_range(0.0, 20e-6, Linear, 0.0),
            Some(a(0.0, 20e-6, Linear))
        );
        assert_eq!(
            fit_range(0.0, 1.0, Linear, 0.05),
            Some(a(-0.05, 1.05, Linear))
        );
        assert_eq!(fit_range(4.0, 4.0, Linear, 0.0), Some(a(2.0, 6.0, Linear)));
        assert_eq!(fit_range(0.0, 0.0, Linear, 0.0), Some(a(-0.5, 0.5, Linear)));
        let d = fit_range(10.0, 10.0, Log, 0.0).unwrap();
        assert!(
            (d.lo - 1.0).abs() < 1e-12 && (d.hi - 100.0).abs() < 1e-12,
            "{d:?}"
        );
        assert_eq!(fit_range(0.0, 1.0, Log, 0.0), None, "log needs lo > 0");
        assert_eq!(fit_range(2.0, 1.0, Linear, 0.0), None);
        assert_eq!(
            fit_range(f64::INFINITY, f64::NEG_INFINITY, Linear, 0.0),
            None
        );
    }

    #[test]
    fn invalid_zooms_and_windows_keep_the_axis() {
        let a = Axis {
            lo: 1.0,
            hi: 2.0,
            scale: Linear,
        };
        assert_eq!(zoom(a, 0.5, f64::NAN), a);
        assert_eq!(zoom(a, 0.5, 0.0), a);
        assert_eq!(zoom(a, 0.5, 1e-20), a, "past f64 resolution");
        assert_eq!(window(a, 0.5, 0.5), a);
        assert_eq!(window(a, 0.0, f64::INFINITY), a);
        assert_eq!(
            zoom(a, 0.0, 2.0),
            Axis {
                lo: 1.0,
                hi: 3.0,
                scale: Linear
            }
        );
        assert_eq!(
            window(a, 1.0, 2.0),
            Axis {
                lo: 2.0,
                hi: 3.0,
                scale: Linear
            }
        );
    }

    #[test]
    fn log_span_skips_non_positive_values() {
        let span_of = |x: &[f64], scale| {
            let s = span(x, scale);
            (s.min, s.max)
        };
        assert_eq!(span_of(&[-1.0, 0.0, 3.0, 2.0], Log), (2.0, 3.0));
        assert_eq!(span_of(&[-1.0, 0.0, 3.0, 2.0], Linear), (-1.0, 3.0));
        assert_eq!(span_of(&[-1.0], Log), (f64::INFINITY, -1.0));
    }
}
