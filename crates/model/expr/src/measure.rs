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
use crate::calc::{self, Axis, AxisKind, CalcValue, ColumnKind, ColumnValue};
use crate::column;
use crate::dmath::Complex;
use crate::eval::Scalar;
use crate::fft;
use crate::kernel;
use crate::waveform::{self, Margin};
use crate::{Code, Error};

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
                waveform::deriv_step
            } else {
                waveform::integ_step
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
            per_step(axes, &y, name, |x, v| Ok(waveform::reduce(builtin, x, v)))?
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
            per_step(axes, &y, name, |x, v| {
                Ok(waveform::cross(x, v, level, edge, nth))
            })?
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
                    waveform::cross(x, &y2.re[r.clone()], l2, 0, 1)
                        - waveform::cross(x, &y.re[r], l1, 0, 1)
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
            per_step(axes, &y, name, |x, v| Ok(waveform::settle(x, v, tol)))?
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
                    waveform::margin(which, &y.re[r], im)
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
        CalcValue::Column(c) if c.kind == ColumnKind::Real && per_step_ax == Some(c.axis) => {
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
            Ok(waveform::interp(x, v, x_of(k - 1)))
        })
    };
    let re = part(axes, y.re)?;
    if y.kind != ColumnKind::Complex {
        return Ok(re);
    }
    let im = part(axes, y.im)?;
    Ok(ColumnValue {
        kind: ColumnKind::Complex,
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
    if c.kind == ColumnKind::Real {
        return Ok(());
    }
    Err(Error::bare(
        Code::X003,
        format!("`{name}` needs a real column"),
    ))
}

/// Functions that take complex columns still refuse bool ones.
fn no_bool(c: &ColumnValue, name: &str) -> Result<(), Error> {
    if c.kind == ColumnKind::Bool {
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
        kind: ColumnKind::Real,
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
    debug_assert_eq!(y.kind, ColumnKind::Real);
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
        Some(i) => {
            // The same source steps give the same bins.
            debug_assert_eq!(axes[i as usize].x.len(), re.len(), "fft axis bins differ");
            i
        }
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
        kind: ColumnKind::Complex,
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
    debug_assert!(m >= xs.len(), "resampling never drops points");
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
