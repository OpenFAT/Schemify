//! Element-wise calculator work (spec §1): calculator operands and a scalar
//! kernel → one result column on their common axis. Operators and builtins
//! reuse the scalar kernels per element, so both modes agree by construction;
//! `+ - * /` on real columns take a straight-loop fast path with the same
//! arithmetic, so results are bit-identical.
//!
//! Contents:
//! - `pub(crate) fn elementwise(args, axes, strings, f) -> Result<ColumnValue, Error>`
//! - `pub(crate) fn column_binary(op, a, b, axes, strings) -> Result<ColumnValue, Error>`
//! - `pub(crate) fn scalars(args: &[CalcValue]) -> Option<Vec<Scalar>>`, `pub(crate) fn representative_scalar(v: &CalcValue) -> Scalar`
//! - `pub(crate) fn check_finite(v: &[f64], name: &str) -> Result<(), Error>`,
//!   `pub(crate) fn first_nonfinite(v: &[f64]) -> Option<usize>`

use crate::calc::{self, Axis, AxisKind, CalcValue, ColumnKind, ColumnValue};
use crate::eval::Scalar;
use crate::kernel;
use crate::program::OpKind;
use crate::{Code, Error};

/// Builds a column one element at a time, widening real to complex.
struct ColumnBuilder {
    kind: Option<ColumnKind>,
    re: Vec<f64>,
    im: Vec<f64>,
}

/// A real operand of the arithmetic fast path.
#[derive(Clone, Copy)]
enum RealOperand<'column> {
    Scalar(f64),
    Column { values: &'column [f64], axis: u32 },
}

/// Lanes of the vectorizable non-finite scan.
const LANES: usize = 8;

/// Applies a scalar kernel per element; per-step columns repeat over their
/// parent's steps, scalars over everything.
///
/// # Errors
/// `X011` when column arguments are on unrelated axes; `X003` when results
/// mix bool and numbers or are strings; whatever `f` returns, its message
/// suffixed with the point.
///
/// # Panics
/// If no argument is a column.
pub(crate) fn elementwise(
    args: &[CalcValue],
    axes: &[Axis],
    strings: &mut String,
    mut f: impl FnMut(&[Scalar], &mut String) -> Result<Scalar, Error>,
) -> Result<ColumnValue, Error> {
    let axis = common_axis(args, axes)?;
    let ax = &axes[axis as usize];
    let n = ax.x.len();
    debug_assert_eq!(
        ax.steps.last().map_or(0, |step| step.end as usize),
        n,
        "an axis's steps cover its points"
    );
    let mut vs: Vec<Scalar> = vec![Scalar::Bool(false); args.len()];
    let mut out = ColumnBuilder {
        kind: None,
        re: Vec::with_capacity(n),
        im: Vec::new(),
    };
    for (si, s) in ax.steps.iter().enumerate() {
        for i in s.start as usize..s.end as usize {
            for (k, a) in args.iter().enumerate() {
                vs[k] = match a {
                    CalcValue::Scalar(v) => *v,
                    CalcValue::Column(c) => element(c, if c.axis == axis { i } else { si }),
                };
            }
            let v = f(&vs, strings).map_err(|e| Error {
                message: format!("{} (at point {i})", e.message),
                ..e
            })?;
            out.push(v)?;
        }
    }
    assert_eq!(out.re.len(), n, "elementwise output covers the axis");
    Ok(out.finish(axis))
}

/// Column binary operator; real `+ - * /` skip the per-element kernel.
///
/// # Errors
/// As [`elementwise`] with the operator's kernel; `X010` naming the first
/// non-finite point.
pub(crate) fn column_binary(
    op: OpKind,
    a: CalcValue,
    b: CalcValue,
    axes: &[Axis],
    strings: &mut String,
) -> Result<ColumnValue, Error> {
    debug_assert!(
        matches!(a, CalcValue::Column(_)) || matches!(b, CalcValue::Column(_)),
        "two scalars take the scalar kernel"
    );
    if matches!(op, OpKind::Add | OpKind::Sub | OpKind::Mul | OpKind::Div) {
        let fast = match (real(&a), real(&b)) {
            (
                Some(x @ RealOperand::Column { axis: ax, .. }),
                Some(y @ RealOperand::Column { axis: ay, .. }),
            ) if ax == ay => Some((x, y, ax)),
            (Some(x @ RealOperand::Column { axis, .. }), Some(y @ RealOperand::Scalar(_))) => {
                Some((x, y, axis))
            }
            (Some(x @ RealOperand::Scalar(_)), Some(y @ RealOperand::Column { axis, .. })) => {
                Some((x, y, axis))
            }
            _ => None,
        };
        if let Some((x, y, axis)) = fast {
            let re = arith(op, x, y);
            debug_assert_eq!(
                re.len(),
                axes[axis as usize].x.len(),
                "result covers the axis"
            );
            if let Some(i) = first_nonfinite(&re) {
                kernel::finite(re[i], kernel::op_str(op)).map_err(|e| Error {
                    message: format!("{} (at point {i})", e.message),
                    ..e
                })?;
            }
            return Ok(ColumnValue {
                kind: ColumnKind::Real,
                re,
                im: Vec::new(),
                axis,
            });
        }
    }
    elementwise(&[a, b], axes, strings, |v, strings| {
        kernel::binary(op, v[0], v[1], strings)
    })
}

/// The arguments as scalars, or `None` when any is a column.
pub(crate) fn scalars(args: &[CalcValue]) -> Option<Vec<Scalar>> {
    let scalars: Option<Vec<Scalar>> = args
        .iter()
        .map(|a| match a {
            CalcValue::Scalar(v) => Some(*v),
            CalcValue::Column(_) => None,
        })
        .collect();
    debug_assert!(scalars.as_ref().is_none_or(|vs| vs.len() == args.len()));
    scalars
}

/// A representative scalar of `v`, for type-error messages.
pub(crate) fn representative_scalar(v: &CalcValue) -> Scalar {
    match v {
        CalcValue::Scalar(v) => *v,
        CalcValue::Column(c) => element(c, 0),
    }
}

/// `X010` naming `name` and the first non-finite point of `v`.
pub(crate) fn check_finite(v: &[f64], name: &str) -> Result<(), Error> {
    debug_assert!(!name.is_empty(), "the error names its builtin");
    match first_nonfinite(v) {
        None => Ok(()),
        Some(i) => Err(Error::bare(
            Code::X010,
            format!("`{name}` has no finite value at point {i}"),
        )),
    }
}

/// Index of the first non-finite value. The common case (all finite) runs a branch-free 8-lane
/// OR over exponent bits, which vectorizes; only a hit falls back to [`first_nonfinite_scalar`].
pub(crate) fn first_nonfinite(v: &[f64]) -> Option<usize> {
    const EXP: u64 = 0x7ff0_0000_0000_0000;
    let chunks = v.chunks_exact(LANES);
    let tail = v.len() - chunks.remainder().len();
    let bad = chunks.fold([false; LANES], |mut acc, c| {
        for k in 0..LANES {
            acc[k] |= c[k].to_bits() & EXP == EXP;
        }
        acc
    });
    let r = if bad.iter().any(|&b| b) {
        first_nonfinite_scalar(&v[..tail])
    } else {
        first_nonfinite_scalar(&v[tail..]).map(|i| i + tail)
    };
    debug_assert!(r.is_none_or(|i| !v[i].is_finite()), "the hit is non-finite");
    debug_assert_eq!(
        r,
        first_nonfinite_scalar(v),
        "vector scan agrees with the oracle"
    );
    r
}

// ---- private ----

impl ColumnBuilder {
    fn push(&mut self, v: Scalar) -> Result<(), Error> {
        debug_assert!(
            self.im.is_empty() || self.im.len() == self.re.len(),
            "imaginary parts in step with real ones"
        );
        let (kind, re, im) = match v {
            Scalar::Int(x) => (ColumnKind::Real, x as f64, 0.0),
            Scalar::Num(x) => (ColumnKind::Real, x, 0.0),
            Scalar::Cnum(re, im) => (ColumnKind::Complex, re, im),
            Scalar::Bool(b) => (ColumnKind::Bool, if b { 1.0 } else { 0.0 }, 0.0),
            Scalar::Str(..) => return Err(Error::bare(Code::X003, "string columns do not exist")),
        };
        match (self.kind, kind) {
            (None, k) => self.kind = Some(k),
            (Some(a), b) if a == b => {}
            (Some(ColumnKind::Real), ColumnKind::Complex) => {
                self.kind = Some(ColumnKind::Complex);
                self.im.resize(self.re.len(), 0.0);
            }
            (Some(ColumnKind::Complex), ColumnKind::Real) => {}
            _ => {
                return Err(Error::bare(
                    Code::X003,
                    "column mixes bool and numeric elements",
                ));
            }
        }
        self.re.push(re);
        if self.kind == Some(ColumnKind::Complex) {
            self.im.push(im);
            debug_assert_eq!(self.im.len(), self.re.len());
        }
        Ok(())
    }

    fn finish(self, axis: u32) -> ColumnValue {
        let kind = self.kind.unwrap_or(ColumnKind::Real);
        if kind == ColumnKind::Complex {
            debug_assert_eq!(self.im.len(), self.re.len(), "complex parts in step");
        } else {
            debug_assert!(
                self.im.is_empty(),
                "only complex columns have imaginary parts"
            );
        }
        ColumnValue {
            kind,
            re: self.re,
            im: self.im,
            axis,
        }
    }
}

/// The axis every column argument lives on (or broadcasts to).
fn common_axis(args: &[CalcValue], axes: &[Axis]) -> Result<u32, Error> {
    let child_of =
        |c: u32, p: u32| axes[c as usize].kind == AxisKind::PerStep && axes[c as usize].parent == p;
    let mut target = None;
    for a in args {
        let CalcValue::Column(c) = a else { continue };
        target = Some(match target {
            None => c.axis,
            Some(t) if t == c.axis || child_of(c.axis, t) => t,
            Some(t) if child_of(t, c.axis) => c.axis,
            Some(_) => {
                return Err(calc::x011(
                    "columns are on different sweep axes".to_string(),
                ));
            }
        });
    }
    let Some(target) = target else {
        panic!("elementwise needs a column argument");
    };
    debug_assert!(
        (target as usize) < axes.len(),
        "axis {target} past the table"
    );
    for a in args {
        if let CalcValue::Column(c) = a
            && c.axis != target
            && !child_of(c.axis, target)
        {
            return Err(calc::x011(
                "columns are on different sweep axes".to_string(),
            ));
        }
    }
    Ok(target)
}

fn element(c: &ColumnValue, i: usize) -> Scalar {
    if c.kind == ColumnKind::Complex {
        debug_assert_eq!(c.im.len(), c.re.len(), "complex parts in step");
    } else {
        debug_assert!(c.im.is_empty(), "only complex columns have imaginary parts");
    }
    match c.kind {
        ColumnKind::Real => Scalar::Num(c.re.get(i).copied().unwrap_or(0.0)),
        ColumnKind::Complex => Scalar::Cnum(
            c.re.get(i).copied().unwrap_or(0.0),
            c.im.get(i).copied().unwrap_or(0.0),
        ),
        ColumnKind::Bool => Scalar::Bool(c.re.get(i).is_some_and(|x| *x != 0.0)),
    }
}

fn real(v: &CalcValue) -> Option<RealOperand<'_>> {
    match v {
        CalcValue::Scalar(Scalar::Int(x)) => Some(RealOperand::Scalar(*x as f64)),
        CalcValue::Scalar(Scalar::Num(x)) => Some(RealOperand::Scalar(*x)),
        CalcValue::Column(c) if c.kind == ColumnKind::Real => Some(RealOperand::Column {
            values: &c.re,
            axis: c.axis,
        }),
        _ => None,
    }
}

/// `x op y` over the column operand(s). One monomorphic loop per operator (a
/// `fn` pointer per element would block vectorization).
fn arith(op: OpKind, x: RealOperand<'_>, y: RealOperand<'_>) -> Vec<f64> {
    match op {
        OpKind::Add => lanes(x, y, |a, b| a + b),
        OpKind::Sub => lanes(x, y, |a, b| a - b),
        OpKind::Mul => lanes(x, y, |a, b| a * b),
        _ => {
            debug_assert_eq!(op, OpKind::Div);
            lanes(x, y, |a, b| a / b)
        }
    }
}

fn lanes(x: RealOperand<'_>, y: RealOperand<'_>, f: impl Fn(f64, f64) -> f64) -> Vec<f64> {
    match (x, y) {
        (RealOperand::Column { values: x, .. }, RealOperand::Column { values: y, .. }) => {
            assert_eq!(x.len(), y.len(), "same axis, same length");
            x.iter().zip(y).map(|(a, b)| f(*a, *b)).collect()
        }
        (RealOperand::Column { values: x, .. }, RealOperand::Scalar(y)) => {
            x.iter().map(|a| f(*a, y)).collect()
        }
        (RealOperand::Scalar(x), RealOperand::Column { values: y, .. }) => {
            y.iter().map(|b| f(x, *b)).collect()
        }
        (RealOperand::Scalar(_), RealOperand::Scalar(_)) => {
            unreachable!("scalar operands take the scalar kernel")
        }
    }
}

/// The scalar oracle for [`first_nonfinite`], and its tail.
fn first_nonfinite_scalar(v: &[f64]) -> Option<usize> {
    v.iter().position(|x| !x.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vector scan agrees with the scalar oracle for every length around the lane width and
    /// every position and kind of non-finite value, including none.
    #[test]
    fn first_nonfinite_matches_oracle() {
        for n in 0..3 * LANES + 3 {
            let clean: Vec<f64> = (0..n).map(|i| i as f64 - 4.0).collect();
            assert_eq!(first_nonfinite(&clean), None);
            for at in 0..n {
                for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                    let mut v = clean.clone();
                    v[at] = bad;
                    if at + 1 < n {
                        v[n - 1] = f64::NAN;
                    }
                    assert_eq!(first_nonfinite(&v), Some(at), "n={n} at={at}");
                    assert_eq!(first_nonfinite(&v), first_nonfinite_scalar(&v));
                }
            }
        }
        assert_eq!(first_nonfinite(&[f64::MAX, f64::MIN_POSITIVE, -0.0]), None);
    }
}
