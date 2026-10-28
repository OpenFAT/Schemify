//! Element-wise calculator work (spec §1): calculator operands and a scalar
//! kernel → one result column on their common axis. Operators and builtins
//! reuse the scalar kernels per element, so both modes agree by construction;
//! `+ - * /` on real columns take a straight-loop fast path with the same
//! arithmetic, so results are bit-identical.
//!
//! Contents:
//! - `pub(crate) fn elementwise(args, axes, strs, f) -> Result<Col, Error>`
//! - `pub(crate) fn col_binary(op, a, b, axes, strs) -> Result<Col, Error>`
//! - `pub(crate) fn scalars(args: &[CV]) -> Option<Vec<V>>`, `pub(crate) fn cv_type(v: &CV) -> V`
//! - `pub(crate) fn check_finite(v: &[f64], name: &str) -> Result<(), Error>`,
//!   `pub(crate) fn first_nonfinite(v: &[f64]) -> Option<usize>`

use crate::calc::{Axis, AxisKind, CV, Col, ColKind, x011};
use crate::eval::V;
use crate::kernel::{binary, fin, op_str};
use crate::program::OpKind;
use crate::{Code, Error};

/// Builds a column one element at a time, widening real to complex.
struct ColBuilder {
    kind: Option<ColKind>,
    re: Vec<f64>,
    im: Vec<f64>,
}

/// A real operand of the arithmetic fast path.
#[derive(Clone, Copy)]
enum Real<'a> {
    S(f64),
    C(&'a [f64], u32),
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
    args: &[CV],
    axes: &[Axis],
    strs: &mut String,
    mut f: impl FnMut(&[V], &mut String) -> Result<V, Error>,
) -> Result<Col, Error> {
    let axis = common_axis(args, axes)?;
    let ax = &axes[axis as usize];
    let n = ax.x.len();
    let mut vs: Vec<V> = vec![V::Bool(false); args.len()];
    let mut out = ColBuilder {
        kind: None,
        re: Vec::with_capacity(n),
        im: Vec::new(),
    };
    for (si, s) in ax.steps.iter().enumerate() {
        for i in s.start as usize..s.end as usize {
            for (k, a) in args.iter().enumerate() {
                vs[k] = match a {
                    CV::S(v) => *v,
                    CV::C(c) => elem(c, if c.axis == axis { i } else { si }),
                };
            }
            let v = f(&vs, strs).map_err(|e| Error {
                msg: format!("{} (at point {i})", e.msg),
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
pub(crate) fn col_binary(
    op: OpKind,
    a: CV,
    b: CV,
    axes: &[Axis],
    strs: &mut String,
) -> Result<Col, Error> {
    debug_assert!(matches!(a, CV::C(_)) || matches!(b, CV::C(_)));
    if matches!(op, OpKind::Add | OpKind::Sub | OpKind::Mul | OpKind::Div) {
        let fast = match (real(&a), real(&b)) {
            (Some(x @ Real::C(_, ax)), Some(y @ Real::C(_, ay))) if ax == ay => Some((x, y, ax)),
            (Some(x @ Real::C(_, ax)), Some(y @ Real::S(_))) => Some((x, y, ax)),
            (Some(x @ Real::S(_)), Some(y @ Real::C(_, ay))) => Some((x, y, ay)),
            _ => None,
        };
        if let Some((x, y, axis)) = fast {
            let re = arith(op, x, y);
            if let Some(i) = first_nonfinite(&re) {
                fin(re[i], op_str(op)).map_err(|e| Error {
                    msg: format!("{} (at point {i})", e.msg),
                    ..e
                })?;
            }
            return Ok(Col {
                kind: ColKind::Real,
                re,
                im: Vec::new(),
                axis,
            });
        }
    }
    elementwise(&[a, b], axes, strs, |v, strs| binary(op, v[0], v[1], strs))
}

/// The arguments as scalars, or `None` when any is a column.
pub(crate) fn scalars(args: &[CV]) -> Option<Vec<V>> {
    args.iter()
        .map(|a| match a {
            CV::S(v) => Some(*v),
            CV::C(_) => None,
        })
        .collect()
}

/// A representative scalar of `v`, for type-error messages.
pub(crate) fn cv_type(v: &CV) -> V {
    match v {
        CV::S(v) => *v,
        CV::C(c) => elem(c, 0),
    }
}

/// `X010` naming `name` and the first non-finite point of `v`.
pub(crate) fn check_finite(v: &[f64], name: &str) -> Result<(), Error> {
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
    debug_assert_eq!(
        r,
        first_nonfinite_scalar(v),
        "vector scan agrees with the oracle"
    );
    r
}

// ---- private ----

impl ColBuilder {
    fn push(&mut self, v: V) -> Result<(), Error> {
        let (kind, re, im) = match v {
            V::Int(x) => (ColKind::Real, x as f64, 0.0),
            V::Num(x) => (ColKind::Real, x, 0.0),
            V::Cnum(re, im) => (ColKind::Complex, re, im),
            V::Bool(b) => (ColKind::Bool, if b { 1.0 } else { 0.0 }, 0.0),
            V::Str(..) => return Err(Error::bare(Code::X003, "string columns do not exist")),
        };
        match (self.kind, kind) {
            (None, k) => self.kind = Some(k),
            (Some(a), b) if a == b => {}
            (Some(ColKind::Real), ColKind::Complex) => {
                self.kind = Some(ColKind::Complex);
                self.im.resize(self.re.len(), 0.0);
            }
            (Some(ColKind::Complex), ColKind::Real) => {}
            _ => {
                return Err(Error::bare(
                    Code::X003,
                    "column mixes bool and numeric elements",
                ));
            }
        }
        self.re.push(re);
        if self.kind == Some(ColKind::Complex) {
            self.im.push(im);
        }
        Ok(())
    }

    fn finish(self, axis: u32) -> Col {
        let kind = self.kind.unwrap_or(ColKind::Real);
        debug_assert!(kind != ColKind::Complex || self.im.len() == self.re.len());
        Col {
            kind,
            re: self.re,
            im: self.im,
            axis,
        }
    }
}

/// The axis every column argument lives on (or broadcasts to).
fn common_axis(args: &[CV], axes: &[Axis]) -> Result<u32, Error> {
    let child_of =
        |c: u32, p: u32| axes[c as usize].kind == AxisKind::PerStep && axes[c as usize].parent == p;
    let mut target = None;
    for a in args {
        let CV::C(c) = a else { continue };
        target = Some(match target {
            None => c.axis,
            Some(t) if t == c.axis || child_of(c.axis, t) => t,
            Some(t) if child_of(t, c.axis) => c.axis,
            Some(_) => return Err(x011("columns are on different sweep axes".to_string())),
        });
    }
    let target = target.expect("elementwise needs a column argument");
    for a in args {
        if let CV::C(c) = a
            && c.axis != target
            && !child_of(c.axis, target)
        {
            return Err(x011("columns are on different sweep axes".to_string()));
        }
    }
    Ok(target)
}

fn elem(c: &Col, i: usize) -> V {
    match c.kind {
        ColKind::Real => V::Num(c.re.get(i).copied().unwrap_or(0.0)),
        ColKind::Complex => V::Cnum(
            c.re.get(i).copied().unwrap_or(0.0),
            c.im.get(i).copied().unwrap_or(0.0),
        ),
        ColKind::Bool => V::Bool(c.re.get(i).is_some_and(|x| *x != 0.0)),
    }
}

fn real(v: &CV) -> Option<Real<'_>> {
    match v {
        CV::S(V::Int(x)) => Some(Real::S(*x as f64)),
        CV::S(V::Num(x)) => Some(Real::S(*x)),
        CV::C(c) if c.kind == ColKind::Real => Some(Real::C(&c.re, c.axis)),
        _ => None,
    }
}

/// `x op y` over the column operand(s). One monomorphic loop per operator (a
/// `fn` pointer per element would block vectorization).
fn arith(op: OpKind, x: Real<'_>, y: Real<'_>) -> Vec<f64> {
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

fn lanes(x: Real<'_>, y: Real<'_>, f: impl Fn(f64, f64) -> f64) -> Vec<f64> {
    match (x, y) {
        (Real::C(x, _), Real::C(y, _)) => {
            assert_eq!(x.len(), y.len(), "same axis, same length");
            x.iter().zip(y).map(|(a, b)| f(*a, *b)).collect()
        }
        (Real::C(x, _), Real::S(y)) => x.iter().map(|a| f(*a, y)).collect(),
        (Real::S(x), Real::C(y, _)) => y.iter().map(|b| f(x, *b)).collect(),
        (Real::S(_), Real::S(_)) => unreachable!("scalar operands take the scalar kernel"),
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
