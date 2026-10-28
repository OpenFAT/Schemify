//! Scalar kernels (spec §4, §5): operator or builtin and its scalar operands →
//! one scalar or an error. Shared by the scalar evaluator, constant folding
//! and the calculator's element-wise path, so all three agree by construction.
//! Transcendentals go through `dmath`/`libm` (D32).
//!
//! Contents:
//! - `pub(crate) fn unary(op: OpKind, a: V) -> Result<V, Error>`
//! - `pub(crate) fn binary(op: OpKind, a: V, b: V, strs: &mut String) -> Result<V, Error>`
//! - `pub(crate) fn call(b: Builtin, args: &[V], strs: &mut String) -> Result<V, Error>`
//! - `pub(crate) fn fin(x: f64, what: &str) -> Result<V, Error>`,
//!   `pub(crate) fn cfin(z: Complex, what: &str) -> Result<V, Error>`
//! - `pub(crate) fn type_name(v: V) -> &'static str`, `pub(crate) fn op_str(op: OpKind) -> &'static str`,
//!   `pub(crate) fn not_bool(what: &str, v: V) -> Error`

use core::cmp::Ordering;

use crate::builtin::Builtin;
use crate::dmath::{self, Complex};
use crate::eval::{V, check_str_len, str_at};
use crate::program::OpKind;
use crate::text::{parse_num, write_eng, write_int, write_num};
use crate::{Code, Error};

/// `-x` and `!b`.
pub(crate) fn unary(op: OpKind, a: V) -> Result<V, Error> {
    debug_assert!(matches!(op, OpKind::Neg | OpKind::Not));
    match (op, a) {
        (OpKind::Neg, V::Int(x)) => Ok(x.checked_neg().map_or(V::Num(-(x as f64)), V::Int)),
        (OpKind::Neg, V::Num(x)) => Ok(V::Num(-x)),
        (OpKind::Neg, V::Cnum(re, im)) => Ok(V::Cnum(-re, -im)),
        (OpKind::Not, V::Bool(b)) => Ok(V::Bool(!b)),
        _ => Err(Error::bare(
            Code::X003,
            format!("`{}` cannot take {}", op_str(op), type_name(a)),
        )),
    }
}

/// Binary operators (spec §4): int stays int for `+ - * %` unless it
/// overflows; `/` and `^` are num; cnum absorbs int/num; `str + str`
/// concatenates; comparisons of int/num compare as num.
pub(crate) fn binary(op: OpKind, a: V, b: V, strs: &mut String) -> Result<V, Error> {
    debug_assert!(!matches!(
        op,
        OpKind::Neg | OpKind::Not | OpKind::Const | OpKind::Call
    ));
    let eq_op = matches!(op, OpKind::Eq | OpKind::Ne);
    match (a, b) {
        (V::Int(x), V::Int(y)) => int_binary(op, x, y),
        (V::Int(_) | V::Num(_), V::Int(_) | V::Num(_)) => num_binary(op, real(a), real(b)),
        (V::Cnum(ar, ai), V::Cnum(br, bi)) if eq_op => {
            Ok(V::Bool(((ar, ai) == (br, bi)) == (op == OpKind::Eq)))
        }
        (V::Cnum(..), V::Int(_) | V::Num(_) | V::Cnum(..))
        | (V::Int(_) | V::Num(_), V::Cnum(..)) => {
            cnum_binary(op, complex(a), complex(b)).ok_or_else(|| type_err(op_str(op), a, b))?
        }
        (V::Str(..), V::Str(..)) => str_binary(op, a, b, strs),
        (V::Bool(x), V::Bool(y)) if eq_op => Ok(V::Bool((x == y) == (op == OpKind::Eq))),
        _ => Err(type_err(op_str(op), a, b)),
    }
}

/// Scalar builtins (spec §5). Calculator-only builtins reached with scalar
/// arguments are `X003` (they need a column).
///
/// # Panics
/// If `args` is empty or `b` compiles to a dedicated op (`if v i x op`).
pub(crate) fn call(b: Builtin, args: &[V], strs: &mut String) -> Result<V, Error> {
    assert!(!args.is_empty(), "every scalar builtin takes arguments");
    let name = b.name();
    let num = |v: V| match v {
        V::Int(x) => Ok(x as f64),
        V::Num(x) => Ok(x),
        _ => Err(Error::bare(
            Code::X003,
            format!("`{name}` needs a number, got {}", type_name(v)),
        )),
    };
    let bad = |v: V| {
        Error::bare(
            Code::X003,
            format!("`{name}` cannot take {}", type_name(v)),
        )
    };
    let a0 = args[0];
    let r = match b {
        Builtin::Abs => match a0 {
            V::Int(x) => Ok(x.checked_abs().map_or(V::Num((x as f64).abs()), V::Int)),
            V::Cnum(re, im) => fin(libm::hypot(re, im), name),
            v => Ok(V::Num(num(v)?.abs())),
        },
        _ if b.elementary() => match a0 {
            V::Cnum(re, im) => cfin(dmath::complex(b, Complex::new(re, im)), name),
            v => fin(dmath::real(b, num(v)?), name),
        },
        Builtin::Atan2 => fin(libm::atan2(num(a0)?, num(args[1])?), name),
        Builtin::Floor | Builtin::Ceil | Builtin::Round => match a0 {
            V::Int(x) => Ok(V::Int(x)),
            v => {
                let x = num(v)?;
                Ok(V::Num(match b {
                    Builtin::Floor => x.floor(),
                    Builtin::Ceil => x.ceil(),
                    _ => x.round(),
                }))
            }
        },
        Builtin::Int => match a0 {
            V::Int(x) => Ok(V::Int(x)),
            v => {
                let t = num(v)?.trunc();
                // i64 range is [-2^63, 2^63); both bounds are exact in f64.
                if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&t) {
                    Ok(V::Int(t as i64))
                } else {
                    Err(Error::bare(Code::X010, "`int` result outside the int range"))
                }
            }
        },
        Builtin::Min | Builtin::Max => {
            let max = b == Builtin::Max;
            if args.iter().all(|v| matches!(v, V::Int(_))) {
                let ints = args.iter().map(|v| if let V::Int(x) = v { *x } else { 0 });
                Ok(V::Int(
                    if max { ints.max() } else { ints.min() }.expect("at least two args"),
                ))
            } else {
                let mut m = num(a0)?;
                for v in &args[1..] {
                    let x = num(*v)?;
                    m = if max { m.max(x) } else { m.min(x) };
                }
                Ok(V::Num(m))
            }
        }
        Builtin::Steps => steps(args, num, bad),
        Builtin::Clamp => match (a0, args[1], args[2]) {
            (V::Int(x), V::Int(lo), V::Int(hi)) => Ok(V::Int(x.max(lo).min(hi))),
            (x, lo, hi) => Ok(V::Num(num(x)?.max(num(lo)?).min(num(hi)?))),
        },
        Builtin::Sign => match a0 {
            V::Int(x) => Ok(V::Int(x.signum())),
            v => {
                let x = num(v)?;
                Ok(V::Num(if x > 0.0 {
                    1.0
                } else if x < 0.0 {
                    -1.0
                } else {
                    0.0
                }))
            }
        },
        Builtin::Pow => binary(OpKind::Pow, a0, args[1], strs),
        Builtin::Snap => {
            let (x, g) = (num(a0)?, num(args[1])?);
            fin((x / g).round() * g, name)
        }
        Builtin::Eng => {
            let x = num(a0)?;
            let digits = match args.get(1) {
                None => 3,
                Some(V::Int(d)) if (1..=17).contains(d) => *d as u32,
                Some(_) => {
                    return Err(Error::bare(
                        Code::X003,
                        "`eng` digits must be an int in 1..=17",
                    ));
                }
            };
            write_str(strs, |w| write_eng(w, x, digits))
        }
        Builtin::Str => match a0 {
            V::Str(..) => Ok(a0),
            V::Int(x) => write_str(strs, |w| write_int(w, x)),
            V::Num(x) => write_str(strs, |w| write_num(w, x)),
            V::Bool(v) => write_str(strs, |w| w.push_str(if v { "true" } else { "false" })),
            V::Cnum(..) => Err(Error::bare(Code::X003, "`str` of cnum: use re/im/mag/ph")),
        },
        Builtin::Num => match a0 {
            V::Str(s, len) => Ok(V::Num(parse_num(str_at(strs, s, len))?)),
            v => Ok(V::Num(num(v)?)),
        },
        Builtin::Len => match a0 {
            // Unicode scalar values.
            V::Str(s, len) => Ok(V::Int(str_at(strs, s, len).chars().count() as i64)),
            v => Err(bad(v)),
        },
        Builtin::Re | Builtin::Im | Builtin::Mag | Builtin::Ph | Builtin::Db | Builtin::Conj => {
            let z = match a0 {
                V::Cnum(re, im) => Complex::new(re, im),
                v => Complex::new(num(v)?, 0.0),
            };
            match b {
                Builtin::Re => Ok(V::Num(z.re)),
                Builtin::Im => Ok(V::Num(z.im)),
                Builtin::Mag => fin(dmath::norm(z), name),
                // Zero has no phase; define it as 0 so the sign of a zero
                // (`-0.0`) cannot flip the answer to 180.
                Builtin::Ph if z.re == 0.0 && z.im == 0.0 => Ok(V::Num(0.0)),
                Builtin::Ph => Ok(V::Num(libm::atan2(z.im, z.re).to_degrees())),
                Builtin::Db => fin(20.0 * libm::log10(dmath::norm(z)), name),
                _ if matches!(a0, V::Cnum(..)) => Ok(V::Cnum(z.re, -z.im)),
                _ => Ok(a0),
            }
        }
        Builtin::Cplx => Ok(V::Cnum(num(a0)?, num(args[1])?)),
        Builtin::SpectreSource => match a0 {
            V::Str(s, len) => {
                let spec = str_at(strs, s, len).to_owned();
                let mut out = String::with_capacity(spec.len() + 16);
                crate::spectre::source(&spec, &mut out)?;
                write_str(strs, |w| w.push_str(&out))
            }
            v => Err(bad(v)),
        },
        Builtin::If | Builtin::V | Builtin::I | Builtin::X | Builtin::Op => {
            unreachable!("`{name}` compiles to dedicated ops")
        }
        _ => {
            debug_assert!(b.calc_only());
            Err(Error::bare(
                Code::X003,
                format!("`{name}` needs a column argument"),
            ))
        }
    };
    debug_assert!(
        !matches!(r, Ok(V::Num(x)) if !x.is_finite()),
        "`{name}` leaked a non-finite value"
    );
    r
}

/// `Num(x)` if finite, else `X010` naming what produced it.
pub(crate) fn fin(x: f64, what: &str) -> Result<V, Error> {
    if x.is_finite() {
        return Ok(V::Num(x));
    }
    let kind = if x.is_nan() { "NaN" } else { "infinity" };
    Err(Error::bare(
        Code::X010,
        format!("`{what}` produced {kind}"),
    ))
}

/// `Cnum(z)` if both parts are finite, else `X010` naming what produced it.
pub(crate) fn cfin(z: Complex, what: &str) -> Result<V, Error> {
    if z.re.is_finite() && z.im.is_finite() {
        return Ok(V::Cnum(z.re, z.im));
    }
    Err(Error::bare(
        Code::X010,
        format!("`{what}` produced a non-finite complex value"),
    ))
}

/// The spec §1 name of `v`'s type, for messages.
pub(crate) fn type_name(v: V) -> &'static str {
    match v {
        V::Int(_) => "int",
        V::Num(_) => "num",
        V::Bool(_) => "bool",
        V::Cnum(..) => "cnum",
        V::Str(..) => "str",
    }
}

/// The source spelling of an operator, for messages.
pub(crate) fn op_str(op: OpKind) -> &'static str {
    match op {
        OpKind::Neg | OpKind::Sub => "-",
        OpKind::Not => "!",
        OpKind::Add => "+",
        OpKind::Mul => "*",
        OpKind::Div => "/",
        OpKind::Rem => "%",
        OpKind::Pow => "^",
        OpKind::Eq => "==",
        OpKind::Ne => "!=",
        OpKind::Lt => "<",
        OpKind::Le => "<=",
        OpKind::Gt => ">",
        OpKind::Ge => ">=",
        OpKind::And | OpKind::EndAnd => "&&",
        OpKind::Or | OpKind::EndOr => "||",
        OpKind::Cond | OpKind::Else | OpKind::EndCond => "?:",
        OpKind::Const
        | OpKind::Input
        | OpKind::Signal
        | OpKind::Axis
        | OpKind::Call
        | OpKind::Plugin => "operand",
    }
}

/// `X003`: `what` (a condition or logical operator) got a non-bool.
pub(crate) fn not_bool(what: &str, v: V) -> Error {
    debug_assert!(!matches!(v, V::Bool(_)));
    Error::bare(
        Code::X003,
        format!("`{what}` needs bool, got {}", type_name(v)),
    )
}

// ---- private ----

fn type_err(what: &str, a: V, b: V) -> Error {
    Error::bare(
        Code::X003,
        format!("`{what}` cannot take {} and {}", type_name(a), type_name(b)),
    )
}

fn real(v: V) -> f64 {
    match v {
        V::Int(x) => x as f64,
        V::Num(x) => x,
        _ => unreachable!("real() of {}", type_name(v)),
    }
}

fn complex(v: V) -> Complex {
    match v {
        V::Cnum(re, im) => Complex::new(re, im),
        v => Complex::new(real(v), 0.0),
    }
}

fn int_binary(op: OpKind, x: i64, y: i64) -> Result<V, Error> {
    let r = match op {
        OpKind::Add => x.checked_add(y),
        OpKind::Sub => x.checked_sub(y),
        OpKind::Mul => x.checked_mul(y),
        OpKind::Rem => x.checked_rem(y),
        OpKind::Div | OpKind::Pow => None,
        _ => return Ok(V::Bool(cmp(op, x.cmp(&y)))),
    };
    // Overflow (or `/`, `^`, `% 0`) promotes to num.
    r.map_or_else(|| num_binary(op, x as f64, y as f64), |v| Ok(V::Int(v)))
}

fn num_binary(op: OpKind, x: f64, y: f64) -> Result<V, Error> {
    let r = match op {
        OpKind::Add => x + y,
        OpKind::Sub => x - y,
        OpKind::Mul => x * y,
        OpKind::Div => x / y,
        OpKind::Rem => x % y,
        OpKind::Pow => libm::pow(x, y),
        _ => {
            let ord = x.partial_cmp(&y).expect("values are never NaN");
            return Ok(V::Bool(cmp(op, ord)));
        }
    };
    fin(r, op_str(op))
}

/// Complex arithmetic; `None` for operators complex numbers lack.
fn cnum_binary(op: OpKind, a: Complex, b: Complex) -> Option<Result<V, Error>> {
    let r = match op {
        OpKind::Add => a + b,
        OpKind::Sub => a - b,
        OpKind::Mul => a * b,
        OpKind::Div => a / b,
        OpKind::Pow => dmath::cpow(a, b),
        _ => return None,
    };
    Some(cfin(r, op_str(op)))
}

fn str_binary(op: OpKind, a: V, b: V, strs: &mut String) -> Result<V, Error> {
    let (V::Str(sa, la), V::Str(sb, lb)) = (a, b) else {
        unreachable!("str_binary of non-strings")
    };
    let (ra, rb) = (
        sa as usize..(sa + la) as usize,
        sb as usize..(sb + lb) as usize,
    );
    if op == OpKind::Add {
        check_str_len(strs, (la + lb) as usize)?;
        let start = strs.len();
        strs.extend_from_within(ra);
        strs.extend_from_within(rb);
        return Ok(V::Str(start as u32, la + lb));
    }
    if matches!(
        op,
        OpKind::Sub | OpKind::Mul | OpKind::Div | OpKind::Rem | OpKind::Pow
    ) {
        return Err(type_err(op_str(op), a, b));
    }
    Ok(V::Bool(cmp(op, strs[ra].cmp(&strs[rb]))))
}

fn cmp(op: OpKind, ord: Ordering) -> bool {
    match op {
        OpKind::Eq => ord.is_eq(),
        OpKind::Ne => ord.is_ne(),
        OpKind::Lt => ord.is_lt(),
        OpKind::Le => ord.is_le(),
        OpKind::Gt => ord.is_gt(),
        OpKind::Ge => ord.is_ge(),
        _ => unreachable!("not a comparison"),
    }
}

/// A measured step table (PDK geometry rules, D12 as amended 2026-10-15): `steps(x,
/// b0, y0, b1, y1, …, bn)` is yi for bi ≤ x < bi+1 and bn itself, an error anywhere
/// else, so a point outside what was measured is never extrapolated. Breakpoints
/// compare with a relative tolerance of 1e-9 (a length computed from other params).
fn steps(
    args: &[V],
    num: impl Fn(V) -> Result<f64, Error>,
    bad: impl Fn(V) -> Error,
) -> Result<V, Error> {
    debug_assert!(args.len() >= 4, "arity checked at compile time");
    if !args.len().is_multiple_of(2) {
        return Err(Error::bare(
            Code::X008,
            "`steps` takes x, then breakpoint/value pairs, then the last breakpoint",
        ));
    }
    let x = num(args[0])?;
    let bounds: Vec<f64> = args[1..]
        .iter()
        .step_by(2)
        .map(|v| num(*v))
        .collect::<Result<_, _>>()?;
    if bounds.windows(2).any(|w| w[0] >= w[1]) {
        return Err(Error::bare(Code::X003, "`steps` breakpoints must increase"));
    }
    let near = |a: f64, b: f64| (a - b).abs() <= 1e-9 * a.abs().max(b.abs());
    let last = *bounds.last().expect("at least two breakpoints");
    let k = if near(x, last) {
        Some(bounds.len() - 2)
    } else {
        (0..bounds.len() - 1).find(|&k| {
            (x > bounds[k] || near(x, bounds[k])) && x < bounds[k + 1] && !near(x, bounds[k + 1])
        })
    };
    let Some(k) = k else {
        return Err(Error::bare(
            Code::X010,
            format!(
                "`steps`: {x} is outside the measured range {}..{last}",
                bounds[0]
            ),
        ));
    };
    debug_assert!(2 + 2 * k < args.len() - 1);
    match args[2 + 2 * k] {
        y @ (V::Int(_) | V::Num(_)) => Ok(y),
        v => Err(bad(v)),
    }
}

/// Appends what `f` writes to `strs` as a new string.
fn write_str(strs: &mut String, f: impl FnOnce(&mut String)) -> Result<V, Error> {
    let start = strs.len();
    f(strs);
    let len = strs.len() - start;
    check_str_len(&strs[..start], len)?;
    Ok(V::Str(start as u32, len as u32))
}
