//! Calculator builtins (spec §5): a builtin and its arguments, at least one a
//! column → a column. Element-wise builtins go to `column`; the step-aware
//! ones (`deriv integ`, the reductions `avg rms ymax ymin pp at cross delay
//! settle`, the margins and `fft`) work one sweep step at a time and never
//! cross a step boundary. A reduction's result lives on the per-step axis of
//! its source; an `fft`'s on one frequency axis per source axis (D23).
//!
//! Contents:
//! - `pub(crate) fn call(builtin: Builtin, args: Vec<CalcValue>, axes: &mut Vec<Axis>,
//!   strings: &mut String) -> Result<CalcValue, Error>`

use core::ops::Range;

use crate::builtin::Builtin;
use crate::calc::{self, Axis, AxisKind, CalcValue, ColKind, ColumnValue};
use crate::column;
use crate::dmath::{self, Complex};
use crate::eval::Scalar;
use crate::fft;
use crate::kernel;
use crate::{Code, Error};

/// Which crossing [`margin`] measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Margin {
    /// Gain margin (dB) at the first -180° phase crossing.
    Gain,
    /// Phase margin (°) at the first unity-gain crossing.
    Phase,
}

/// Calls `builtin`: the scalar kernel when every argument is a scalar, per
/// element for a scalar builtin, else the column function.
///
/// # Errors
/// `X003` for a scalar where a column is needed, a complex or bool column
/// where a real one is, or an argument out of range; `X011` for unrelated
/// axes or an `fft` step without two increasing points; `X010` for a
/// non-finite result or a measurement with no answer.
pub(crate) fn call(
    builtin: Builtin,
    mut args: Vec<CalcValue>,
    axes: &mut Vec<Axis>,
    strings: &mut String,
) -> Result<CalcValue, Error> {
    assert!(!axes.is_empty(), "the plot axis is set up first");
    let name = builtin.name();
    if let Some(vs) = column::scalars(&args) {
        return Ok(CalcValue::Scalar(kernel::call(builtin, &vs, strings)?));
    }
    if !builtin.calc_only() {
        let col = column::elementwise(&args, axes, strings, |v, strings| {
            kernel::call(builtin, v, strings)
        })?;
        return Ok(CalcValue::Column(col));
    }
    assert!(!args.is_empty(), "calc builtins take a column first");
    let rest = args.split_off(1);
    let Some(first) = args.pop() else {
        unreachable!("one argument left after the split")
    };
    let y = want_column(first, name)?;
    let axes_before = axes.len();
    let col = match builtin {
        Builtin::Deriv | Builtin::Integ => {
            no_bool(&y, name)?;
            let f = if builtin == Builtin::Deriv {
                deriv_step
            } else {
                integ_step
            };
            let ax = &axes[y.axis as usize];
            let mut re = vec![0.0; y.re.len()];
            let mut im = vec![0.0; y.im.len()];
            for r in ranges(ax) {
                f(&ax.x[r.clone()], &y.re[r.clone()], &mut re[r.clone()]);
                if !im.is_empty() {
                    f(&ax.x[r.clone()], &y.im[r.clone()], &mut im[r]);
                }
            }
            column::check_finite(&re, name)?;
            column::check_finite(&im, name)?;
            ColumnValue { re, im, ..y }
        }
        Builtin::Avg | Builtin::Rms | Builtin::Ymax | Builtin::Ymin | Builtin::Pp => {
            want_real(&y, name)?;
            per_step(axes, &y, name, |x, v| Ok(reduce(builtin, x, v)))?
        }
        Builtin::At => at(axes, y, &rest[0], name)?,
        Builtin::Cross => {
            want_real(&y, name)?;
            let level = want_num(&rest[0], name)?;
            let edge = rest
                .get(1)
                .map_or(Ok(0), |a| want_int(a, name, |e| (-1..=1).contains(&e)))?;
            let nth = rest
                .get(2)
                .map_or(Ok(1), |a| want_int(a, name, |n| n >= 1))?;
            per_step(axes, &y, name, |x, v| Ok(cross(x, v, level, edge, nth)))?
        }
        Builtin::Delay => {
            want_real(&y, name)?;
            let l1 = want_num(&rest[0], name)?;
            let l2 = want_num(&rest[2], name)?;
            let Ok([_, y2, _]) = <[CalcValue; 3]>::try_from(rest) else {
                unreachable!("`delay` arity checked at compile time")
            };
            let y2 = want_column(y2, name)?;
            want_real(&y2, name)?;
            if y2.axis != y.axis {
                return Err(calc::x011(
                    "`delay` signals are on different sweep axes".to_string(),
                ));
            }
            let ax = &axes[y.axis as usize];
            let re: Vec<f64> = ranges(ax)
                .map(|r| {
                    let x = &ax.x[r.clone()];
                    cross(x, &y2.re[r.clone()], l2, 0, 1) - cross(x, &y.re[r], l1, 0, 1)
                })
                .collect();
            column::check_finite(&re, name)?;
            real_col(re, per_step_axis(axes, y.axis))
        }
        Builtin::Settle => {
            want_real(&y, name)?;
            let tol = want_num(&rest[0], name)?;
            if tol < 0.0 {
                return Err(Error::bare(Code::X003, "`settle` tolerance must be >= 0"));
            }
            per_step(axes, &y, name, |x, v| Ok(settle(x, v, tol)))?
        }
        Builtin::Fft => {
            want_real(&y, name)?;
            fft(axes, &y)?
        }
        Builtin::GainMargin | Builtin::PhaseMargin => {
            no_bool(&y, name)?;
            let ax = &axes[y.axis as usize];
            let out: Vec<f64> = ranges(ax)
                .map(|r| {
                    let im = if y.im.is_empty() {
                        &[][..]
                    } else {
                        &y.im[r.clone()]
                    };
                    let which = if builtin == Builtin::GainMargin {
                        Margin::Gain
                    } else {
                        Margin::Phase
                    };
                    margin(which, &y.re[r], im)
                })
                .collect();
            column::check_finite(&out, name)?;
            real_col(out, per_step_axis(axes, y.axis))
        }
        _ => unreachable!("`{name}` is not a column function"),
    };
    // Calculator functions only ever append derived axes.
    debug_assert!(axes.len() >= axes_before, "axes are never removed");
    debug_assert!(
        (col.axis as usize) < axes.len(),
        "result axis {} exists",
        col.axis
    );
    Ok(CalcValue::Column(col))
}

// ---- private ----

/// `at(y, x0)`: x0 is a number, or one value per sweep step of `y` (a
/// reduction such as `cross(...)` or `ymax(...)`): step k is read at its own x.
fn at(
    axes: &mut Vec<Axis>,
    y: ColumnValue,
    x0: &CalcValue,
    name: &str,
) -> Result<ColumnValue, Error> {
    no_bool(&y, name)?;
    let per_step_ax = find_axis(axes, AxisKind::PerStep, y.axis);
    let xs: Vec<f64> = match x0 {
        CalcValue::Column(c) if c.kind == ColKind::Real && per_step_ax == Some(c.axis) => {
            c.re.clone()
        }
        CalcValue::Column(_) => {
            return Err(Error::bare(
                Code::X003,
                format!(
                    "`{name}` needs a number or one value per sweep step (e.g. cross(...)) \
                     as its x, got a column"
                ),
            ));
        }
        v => vec![want_num(v, name)?],
    };
    let steps = axes[y.axis as usize].steps.len();
    assert!(
        xs.len() == 1 || xs.len() == steps,
        "{} x values for {steps} sweep steps",
        xs.len()
    );
    let x_of = |k: usize| if xs.len() == 1 { xs[0] } else { xs[k] };
    // Each part (real, then imaginary) is read step by step at its own x.
    let part = |axes: &mut Vec<Axis>, re: Vec<f64>| {
        let col = real_col(re, y.axis);
        let mut k = 0;
        per_step(axes, &col, name, |x, v| {
            k += 1;
            Ok(interp(x, v, x_of(k - 1)))
        })
    };
    let re = part(axes, y.re)?;
    if y.kind != ColKind::Complex {
        return Ok(re);
    }
    let im = part(axes, y.im)?;
    Ok(ColumnValue {
        kind: ColKind::Complex,
        im: im.re,
        ..re
    })
}

fn want_column(v: CalcValue, name: &str) -> Result<ColumnValue, Error> {
    match v {
        CalcValue::Column(c) => Ok(c),
        CalcValue::Scalar(v) => Err(Error::bare(
            Code::X003,
            format!("`{name}` needs a column, got {}", kernel::type_name(v)),
        )),
    }
}

fn want_real(c: &ColumnValue, name: &str) -> Result<(), Error> {
    if c.kind == ColKind::Real {
        return Ok(());
    }
    Err(Error::bare(
        Code::X003,
        format!("`{name}` needs a real column"),
    ))
}

/// Functions that take complex columns still refuse bool ones.
fn no_bool(c: &ColumnValue, name: &str) -> Result<(), Error> {
    if c.kind == ColKind::Bool {
        want_real(c, name)?;
    }
    Ok(())
}

fn want_num(v: &CalcValue, name: &str) -> Result<f64, Error> {
    match v {
        CalcValue::Scalar(Scalar::Int(x)) => Ok(*x as f64),
        CalcValue::Scalar(Scalar::Num(x)) => Ok(*x),
        CalcValue::Scalar(v) => Err(Error::bare(
            Code::X003,
            format!("`{name}` needs a number, got {}", kernel::type_name(*v)),
        )),
        CalcValue::Column(_) => Err(Error::bare(
            Code::X003,
            format!("`{name}` needs a scalar here, got a column"),
        )),
    }
}

fn want_int(v: &CalcValue, name: &str, ok: impl Fn(i64) -> bool) -> Result<i64, Error> {
    match v {
        CalcValue::Scalar(Scalar::Int(x)) if ok(*x) => Ok(*x),
        _ => Err(Error::bare(
            Code::X003,
            format!("`{name}`: argument out of range or not an int"),
        )),
    }
}

/// The steps of `ax` as index ranges.
fn ranges(ax: &Axis) -> impl Iterator<Item = Range<usize>> + '_ {
    debug_assert!(
        ax.steps
            .last()
            .is_none_or(|step| step.end as usize == ax.x.len()),
        "an axis's steps cover its points"
    );
    ax.steps.iter().map(|s| s.start as usize..s.end as usize)
}

fn real_col(re: Vec<f64>, axis: u32) -> ColumnValue {
    ColumnValue {
        kind: ColKind::Real,
        re,
        im: Vec::new(),
        axis,
    }
}

/// The derived axis of `kind` over `parent`, if one exists yet.
fn find_axis(axes: &[Axis], kind: AxisKind, parent: u32) -> Option<u32> {
    debug_assert!(
        kind != AxisKind::Plot,
        "the plot axis is axis 0, never derived"
    );
    debug_assert!(
        (parent as usize) < axes.len(),
        "parent axis {parent} exists"
    );
    axes.iter()
        .position(|a| a.kind == kind && a.parent == parent)
        .map(|i| i as u32)
}

/// The per-step axis of `parent`, created on first use.
fn per_step_axis(axes: &mut Vec<Axis>, parent: u32) -> u32 {
    if let Some(i) = find_axis(axes, AxisKind::PerStep, parent) {
        return i;
    }
    let n = axes[parent as usize].steps.len();
    let x: Vec<f64> = (0..n).map(|i| i as f64).collect();
    axes.push(Axis {
        x,
        steps: vec![Range {
            start: 0,
            end: n as u32,
        }],
        parent,
        kind: AxisKind::PerStep,
    });
    let index = axes.len() as u32 - 1;
    debug_assert_eq!(find_axis(axes, AxisKind::PerStep, parent), Some(index));
    index
}

/// One value per step of `y`'s axis; NaN (no answer) is `X010`.
fn per_step(
    axes: &mut Vec<Axis>,
    y: &ColumnValue,
    name: &str,
    mut f: impl FnMut(&[f64], &[f64]) -> Result<f64, Error>,
) -> Result<ColumnValue, Error> {
    debug_assert_eq!(y.kind, ColKind::Real);
    let ax = &axes[y.axis as usize];
    assert_eq!(y.re.len(), ax.x.len(), "a column covers its axis");
    let mut re = Vec::with_capacity(ax.steps.len());
    for r in ranges(ax) {
        re.push(f(&ax.x[r.clone()], &y.re[r])?);
    }
    debug_assert_eq!(re.len(), ax.steps.len(), "one value per step");
    if let Some(i) = column::first_nonfinite(&re) {
        return Err(Error::bare(
            Code::X010,
            format!("`{name}` has no answer in sweep step {i}"),
        ));
    }
    Ok(real_col(re, per_step_axis(axes, y.axis)))
}

/// dy/dx: central differences inside, one-sided at the step ends.
fn deriv_step(x: &[f64], y: &[f64], out: &mut [f64]) {
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
fn integ_step(x: &[f64], y: &[f64], out: &mut [f64]) {
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
fn reduce(builtin: Builtin, x: &[f64], y: &[f64]) -> f64 {
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

/// Trapezoid integral of `g(y)` over the step.
fn trapz(x: &[f64], y: &[f64], g: impl Fn(f64) -> f64) -> f64 {
    debug_assert_eq!(x.len(), y.len());
    debug_assert!(y.len() >= 2, "single points are handled by the caller");
    (1..y.len())
        .map(|i| (x[i] - x[i - 1]) * (g(y[i]) + g(y[i - 1])) * 0.5)
        .sum()
}

/// Linear interpolation at `x0` on the first segment containing it; NaN outside.
fn interp(x: &[f64], y: &[f64], x0: f64) -> f64 {
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
fn cross(x: &[f64], y: &[f64], level: f64, edge: i64, nth: i64) -> f64 {
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
fn settle(x: &[f64], y: &[f64], tol: f64) -> f64 {
    debug_assert!(tol >= 0.0);
    debug_assert_eq!(x.len(), y.len());
    let Some(&last) = y.last() else {
        return f64::NAN;
    };
    match y.iter().rposition(|v| (v - last).abs() > tol) {
        None => x[0],
        Some(k) => x[k + 1],
    }
}

/// Gain margin (dB) at the first -180° phase crossing, or phase margin (°)
/// at the first unity-gain crossing, of a loop gain whose phase starts near
/// 0°. NaN when the crossing does not exist.
fn margin(which: Margin, re: &[f64], im: &[f64]) -> f64 {
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

/// Single-sided spectrum per step: resample the step uniformly onto the next
/// power of two of its length (transient steps are not uniform), transform,
/// keep bins `0..m/2` scaled to amplitude. Bin k is at `k / span`.
fn fft(axes: &mut Vec<Axis>, y: &ColumnValue) -> Result<ColumnValue, Error> {
    let src = &axes[y.axis as usize];
    assert_eq!(y.re.len(), src.x.len(), "a column covers its axis");
    let (mut re, mut im, mut fx, mut steps) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for r in ranges(src) {
        let (xs, ys) = (&src.x[r.clone()], &y.re[r]);
        let n = xs.len();
        let span = if n >= 2 { xs[n - 1] - xs[0] } else { 0.0 };
        if n < 2 || span <= 0.0 || span.is_nan() {
            return Err(calc::x011(
                "`fft` needs at least 2 points and an increasing axis per step".to_string(),
            ));
        }
        let m = n.next_power_of_two();
        let mut buf = resample(xs, ys, m);
        fft::forward(&mut buf);
        let start = re.len() as u32;
        let bins = buf.iter().take(m / 2);
        for (k, v) in bins.enumerate() {
            let scale = if k == 0 { 1.0 } else { 2.0 } / m as f64;
            fx.push(k as f64 / span);
            re.push(v.re * scale);
            im.push(v.im * scale);
        }
        steps.push(start..re.len() as u32);
    }
    debug_assert_eq!(re.len(), im.len());
    debug_assert_eq!(re.len(), fx.len(), "one frequency per bin");
    column::check_finite(&re, "fft")?;
    column::check_finite(&im, "fft")?;
    // One fft axis per source axis, so fft(a) and fft(b) of one plot combine.
    let axis = match find_axis(axes, AxisKind::Fft, y.axis) {
        Some(i) => i,
        None => {
            axes.push(Axis {
                x: fx,
                steps,
                parent: y.axis,
                kind: AxisKind::Fft,
            });
            axes.len() as u32 - 1
        }
    };
    Ok(ColumnValue {
        kind: ColKind::Complex,
        re,
        im,
        axis,
    })
}

/// `m` uniform samples over `[x0, x_end)` by linear interpolation; one
/// forward pass, so O(n + m). Assumes `xs` non-decreasing (a sweep axis).
fn resample(xs: &[f64], ys: &[f64], m: usize) -> Vec<Complex> {
    assert!(xs.len() >= 2, "resample needs two points per step");
    assert_eq!(xs.len(), ys.len());
    let (x0, span) = (xs[0], xs[xs.len() - 1] - xs[0]);
    let mut k = 0;
    let out: Vec<Complex> = (0..m)
        .map(|i| {
            let t = x0 + span * i as f64 / m as f64;
            while k + 2 < xs.len() && xs[k + 1] <= t {
                k += 1;
            }
            let (a, b) = (xs[k], xs[k + 1]);
            let y = if b > a {
                ys[k] + (ys[k + 1] - ys[k]) * ((t - a) / (b - a)).clamp(0.0, 1.0)
            } else {
                ys[k]
            };
            Complex::new(y, 0.0)
        })
        .collect();
    debug_assert_eq!(out.len(), m);
    out
}
