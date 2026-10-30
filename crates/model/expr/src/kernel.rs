//! Scalar kernels (spec §4, §5): operator or builtin and its scalar operands →
//! one scalar or an error. Shared by the scalar evaluator, constant folding
//! and the calculator's element-wise path, so all three agree by construction.
//! Transcendentals go through `dmath`/`libm` (D32).
//!
//! Contents:
//! - `pub(crate) fn unary(op: OpKind, a: Scalar) -> Result<Scalar, Error>`
//! - `pub(crate) fn binary(op: OpKind, a: Scalar, b: Scalar, strings: &mut String) -> Result<Scalar, Error>`
//! - `pub(crate) fn call(builtin: Builtin, args: &[Scalar], strings: &mut String) -> Result<Scalar, Error>`
//! - `pub(crate) fn finite(x: f64, what: &str) -> Result<Scalar, Error>`,
//!   `pub(crate) fn complex_finite(z: Complex, what: &str) -> Result<Scalar, Error>`
//! - `pub(crate) fn type_name(v: Scalar) -> &'static str`, `pub(crate) fn op_str(op: OpKind) -> &'static str`,
//!   `pub(crate) fn not_bool(what: &str, v: Scalar) -> Error`

use core::cmp::Ordering;

use crate::builtin::Builtin;
use crate::dmath::{self, Complex};
use crate::eval::{self, Scalar};
use crate::program::OpKind;
use crate::spectre;
use crate::text;
use crate::{Code, Error};

/// `-x` and `!b`.
pub(crate) fn unary(op: OpKind, a: Scalar) -> Result<Scalar, Error> {
    debug_assert!(matches!(op, OpKind::Neg | OpKind::Not));
    let r = match (op, a) {
        (OpKind::Neg, Scalar::Int(x)) => Ok(x
            .checked_neg()
            .map_or(Scalar::Num(-(x as f64)), Scalar::Int)),
        (OpKind::Neg, Scalar::Num(x)) => Ok(Scalar::Num(-x)),
        (OpKind::Neg, Scalar::Cnum(re, im)) => Ok(Scalar::Cnum(-re, -im)),
        (OpKind::Not, Scalar::Bool(b)) => Ok(Scalar::Bool(!b)),
        _ => Err(Error::bare(
            Code::X003,
            format!("`{}` cannot take {}", op_str(op), type_name(a)),
        )),
    };
    // `!` maps bool to bool; `-` never makes a bool.
    if let Ok(v) = r {
        debug_assert_eq!(matches!(v, Scalar::Bool(_)), op == OpKind::Not);
    }
    r
}

/// Binary operators (spec §4): int stays int for `+ - * %` unless it
/// overflows; `/` and `^` are num; cnum absorbs int/num; `str + str`
/// concatenates; comparisons of int/num compare as num.
pub(crate) fn binary(
    op: OpKind,
    a: Scalar,
    b: Scalar,
    strings: &mut String,
) -> Result<Scalar, Error> {
    debug_assert!(!matches!(
        op,
        OpKind::Neg | OpKind::Not | OpKind::Const | OpKind::Call
    ));
    let eq_op = matches!(op, OpKind::Eq | OpKind::Ne);
    let r = match (a, b) {
        (Scalar::Int(x), Scalar::Int(y)) => int_binary(op, x, y),
        (Scalar::Int(_) | Scalar::Num(_), Scalar::Int(_) | Scalar::Num(_)) => {
            num_binary(op, real(a), real(b))
        }
        (Scalar::Cnum(ar, ai), Scalar::Cnum(br, bi)) if eq_op => {
            Ok(Scalar::Bool(((ar, ai) == (br, bi)) == (op == OpKind::Eq)))
        }
        (Scalar::Cnum(..), Scalar::Int(_) | Scalar::Num(_) | Scalar::Cnum(..))
        | (Scalar::Int(_) | Scalar::Num(_), Scalar::Cnum(..)) => {
            cnum_binary(op, complex(a), complex(b)).ok_or_else(|| type_err(op_str(op), a, b))?
        }
        (Scalar::Str(..), Scalar::Str(..)) => str_binary(op, a, b, strings),
        (Scalar::Bool(x), Scalar::Bool(y)) if eq_op => {
            Ok(Scalar::Bool((x == y) == (op == OpKind::Eq)))
        }
        _ => Err(type_err(op_str(op), a, b)),
    };
    // A comparison answers bool; arithmetic never does.
    if let Ok(v) = r {
        let comparison = matches!(
            op,
            OpKind::Eq | OpKind::Ne | OpKind::Lt | OpKind::Le | OpKind::Gt | OpKind::Ge
        );
        debug_assert_eq!(matches!(v, Scalar::Bool(_)), comparison, "`{}`", op_str(op));
    }
    r
}

/// Scalar builtins (spec §5). Calculator-only builtins reached with scalar
/// arguments are `X003` (they need a column).
///
/// # Panics
/// If `args` is empty or `builtin` compiles to a dedicated op (`if v i x op`).
pub(crate) fn call(
    builtin: Builtin,
    args: &[Scalar],
    strings: &mut String,
) -> Result<Scalar, Error> {
    assert!(!args.is_empty(), "every scalar builtin takes arguments");
    let name = builtin.name();
    let num = |v: Scalar| match v {
        Scalar::Int(x) => Ok(x as f64),
        Scalar::Num(x) => Ok(x),
        _ => Err(Error::bare(
            Code::X003,
            format!("`{name}` needs a number, got {}", type_name(v)),
        )),
    };
    let bad = |v: Scalar| Error::bare(Code::X003, format!("`{name}` cannot take {}", type_name(v)));
    let a0 = args[0];
    let r = match builtin {
        Builtin::Abs => match a0 {
            Scalar::Int(x) => Ok(x
                .checked_abs()
                .map_or(Scalar::Num((x as f64).abs()), Scalar::Int)),
            Scalar::Cnum(re, im) => finite(libm::hypot(re, im), name),
            v => Ok(Scalar::Num(num(v)?.abs())),
        },
        _ if builtin.elementary() => match a0 {
            Scalar::Cnum(re, im) => {
                complex_finite(dmath::complex(builtin, Complex::new(re, im)), name)
            }
            v => finite(dmath::real(builtin, num(v)?), name),
        },
        Builtin::Atan2 => finite(libm::atan2(num(a0)?, num(args[1])?), name),
        Builtin::Floor | Builtin::Ceil | Builtin::Round => match a0 {
            Scalar::Int(x) => Ok(Scalar::Int(x)),
            v => {
                let x = num(v)?;
                Ok(Scalar::Num(match builtin {
                    Builtin::Floor => x.floor(),
                    Builtin::Ceil => x.ceil(),
                    _ => x.round(),
                }))
            }
        },
        Builtin::Int => match a0 {
            Scalar::Int(x) => Ok(Scalar::Int(x)),
            v => {
                let t = num(v)?.trunc();
                // i64 range is [-2^63, 2^63); both bounds are exact in f64.
                if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&t) {
                    Ok(Scalar::Int(t as i64))
                } else {
                    Err(Error::bare(
                        Code::X010,
                        "`int` result outside the int range",
                    ))
                }
            }
        },
        Builtin::Min | Builtin::Max => {
            let max = builtin == Builtin::Max;
            if args.iter().all(|v| matches!(v, Scalar::Int(_))) {
                let ints = args
                    .iter()
                    .map(|v| if let Scalar::Int(x) = v { *x } else { 0 });
                Ok(Scalar::Int(
                    if max { ints.max() } else { ints.min() }.expect("at least two args"),
                ))
            } else {
                let mut m = num(a0)?;
                for v in &args[1..] {
                    let x = num(*v)?;
                    m = if max { m.max(x) } else { m.min(x) };
                }
                Ok(Scalar::Num(m))
            }
        }
        Builtin::Steps => steps(args, num, bad),
        Builtin::Clamp => match (a0, args[1], args[2]) {
            (Scalar::Int(x), Scalar::Int(lo), Scalar::Int(hi)) => {
                Ok(Scalar::Int(x.max(lo).min(hi)))
            }
            (x, lo, hi) => Ok(Scalar::Num(num(x)?.max(num(lo)?).min(num(hi)?))),
        },
        Builtin::Sign => match a0 {
            Scalar::Int(x) => Ok(Scalar::Int(x.signum())),
            v => {
                let x = num(v)?;
                Ok(Scalar::Num(if x > 0.0 {
                    1.0
                } else if x < 0.0 {
                    -1.0
                } else {
                    0.0
                }))
            }
        },
        Builtin::Pow => binary(OpKind::Pow, a0, args[1], strings),
        Builtin::Snap => {
            let (x, g) = (num(a0)?, num(args[1])?);
            finite((x / g).round() * g, name)
        }
        Builtin::Eng => {
            let x = num(a0)?;
            let digits = match args.get(1) {
                None => 3,
                Some(Scalar::Int(d)) if (1..=17).contains(d) => *d as u32,
                Some(_) => {
                    return Err(Error::bare(
                        Code::X003,
                        "`eng` digits must be an int in 1..=17",
                    ));
                }
            };
            write_str(strings, |w| text::write_eng(w, x, digits))
        }
        Builtin::Str => match a0 {
            Scalar::Str(..) => Ok(a0),
            Scalar::Int(x) => write_str(strings, |w| text::write_int(w, x)),
            Scalar::Num(x) => write_str(strings, |w| text::write_num(w, x)),
            Scalar::Bool(v) => write_str(strings, |w| w.push_str(if v { "true" } else { "false" })),
            Scalar::Cnum(..) => Err(Error::bare(Code::X003, "`str` of cnum: use re/im/mag/ph")),
        },
        Builtin::Num => match a0 {
            Scalar::Str(s, len) => Ok(Scalar::Num(text::parse_num(eval::str_at(strings, s, len))?)),
            v => Ok(Scalar::Num(num(v)?)),
        },
        Builtin::Len => match a0 {
            // Unicode scalar values.
            Scalar::Str(s, len) => Ok(Scalar::Int(
                eval::str_at(strings, s, len).chars().count() as i64
            )),
            v => Err(bad(v)),
        },
        Builtin::Re | Builtin::Im | Builtin::Mag | Builtin::Ph | Builtin::Db | Builtin::Conj => {
            let z = match a0 {
                Scalar::Cnum(re, im) => Complex::new(re, im),
                v => Complex::new(num(v)?, 0.0),
            };
            match builtin {
                Builtin::Re => Ok(Scalar::Num(z.re)),
                Builtin::Im => Ok(Scalar::Num(z.im)),
                Builtin::Mag => finite(dmath::norm(z), name),
                // Zero has no phase; define it as 0 so the sign of a zero
                // (`-0.0`) cannot flip the answer to 180.
                Builtin::Ph if z.re == 0.0 && z.im == 0.0 => Ok(Scalar::Num(0.0)),
                Builtin::Ph => Ok(Scalar::Num(libm::atan2(z.im, z.re).to_degrees())),
                Builtin::Db => finite(20.0 * libm::log10(dmath::norm(z)), name),
                _ if matches!(a0, Scalar::Cnum(..)) => Ok(Scalar::Cnum(z.re, -z.im)),
                _ => Ok(a0),
            }
        }
        Builtin::Cplx => Ok(Scalar::Cnum(num(a0)?, num(args[1])?)),
        Builtin::SpectreSource => match a0 {
            Scalar::Str(s, len) => {
                let spec = eval::str_at(strings, s, len).to_owned();
                let mut out = String::with_capacity(spec.len() + 16);
                spectre::source(&spec, &mut out)?;
                write_str(strings, |w| w.push_str(&out))
            }
            v => Err(bad(v)),
        },
        Builtin::If | Builtin::V | Builtin::I | Builtin::X | Builtin::Op => {
            unreachable!("`{name}` compiles to dedicated ops")
        }
        _ => {
            debug_assert!(builtin.calc_only());
            Err(Error::bare(
                Code::X003,
                format!("`{name}` needs a column argument"),
            ))
        }
    };
    debug_assert!(
        !matches!(r, Ok(Scalar::Num(x)) if !x.is_finite()),
        "`{name}` leaked a non-finite value"
    );
    r
}

/// `Num(x)` if finite, else `X010` naming what produced it.
pub(crate) fn finite(x: f64, what: &str) -> Result<Scalar, Error> {
    debug_assert!(!what.is_empty(), "the error names its source");
    if x.is_finite() {
        return Ok(Scalar::Num(x));
    }
    let kind = if x.is_nan() { "NaN" } else { "infinity" };
    Err(Error::bare(Code::X010, format!("`{what}` produced {kind}")))
}

/// `Cnum(z)` if both parts are finite, else `X010` naming what produced it.
pub(crate) fn complex_finite(z: Complex, what: &str) -> Result<Scalar, Error> {
    debug_assert!(!what.is_empty(), "the error names its source");
    if z.re.is_finite() && z.im.is_finite() {
        return Ok(Scalar::Cnum(z.re, z.im));
    }
    Err(Error::bare(
        Code::X010,
        format!("`{what}` produced a non-finite complex value"),
    ))
}

/// The spec §1 name of `v`'s type, for messages.
pub(crate) fn type_name(v: Scalar) -> &'static str {
    match v {
        Scalar::Int(_) => "int",
        Scalar::Num(_) => "num",
        Scalar::Bool(_) => "bool",
        Scalar::Cnum(..) => "cnum",
        Scalar::Str(..) => "str",
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
pub(crate) fn not_bool(what: &str, v: Scalar) -> Error {
    debug_assert!(!matches!(v, Scalar::Bool(_)));
    Error::bare(
        Code::X003,
        format!("`{what}` needs bool, got {}", type_name(v)),
    )
}

// ---- private ----

fn type_err(what: &str, a: Scalar, b: Scalar) -> Error {
    debug_assert!(!what.is_empty(), "the error names its operator");
    Error::bare(
        Code::X003,
        format!("`{what}` cannot take {} and {}", type_name(a), type_name(b)),
    )
}

fn real(v: Scalar) -> f64 {
    match v {
        Scalar::Int(x) => x as f64,
        Scalar::Num(x) => x,
        _ => unreachable!("real() of {}", type_name(v)),
    }
}

fn complex(v: Scalar) -> Complex {
    match v {
        Scalar::Cnum(re, im) => Complex::new(re, im),
        v => Complex::new(real(v), 0.0),
    }
}

fn int_binary(op: OpKind, x: i64, y: i64) -> Result<Scalar, Error> {
    debug_assert!(
        !matches!(op, OpKind::And | OpKind::Or),
        "lazy operators compile to jumps"
    );
    let r = match op {
        OpKind::Add => x.checked_add(y),
        OpKind::Sub => x.checked_sub(y),
        OpKind::Mul => x.checked_mul(y),
        OpKind::Rem => x.checked_rem(y),
        OpKind::Div | OpKind::Pow => None,
        _ => return Ok(Scalar::Bool(cmp(op, x.cmp(&y)))),
    };
    // Only `+ - * %` can stay int; `/` and `^` always promote.
    if r.is_some() {
        debug_assert!(
            matches!(op, OpKind::Add | OpKind::Sub | OpKind::Mul | OpKind::Rem),
            "`{}` kept an int",
            op_str(op)
        );
    }
    // Overflow (or `/`, `^`, `% 0`) promotes to num.
    r.map_or_else(
        || num_binary(op, x as f64, y as f64),
        |v| Ok(Scalar::Int(v)),
    )
}

fn num_binary(op: OpKind, x: f64, y: f64) -> Result<Scalar, Error> {
    // Values are never NaN, so every comparison below is total.
    debug_assert!(!x.is_nan(), "left operand is NaN");
    debug_assert!(!y.is_nan(), "right operand is NaN");
    let r = match op {
        OpKind::Add => x + y,
        OpKind::Sub => x - y,
        OpKind::Mul => x * y,
        OpKind::Div => x / y,
        OpKind::Rem => x % y,
        OpKind::Pow => libm::pow(x, y),
        _ => {
            let ord = x.partial_cmp(&y).expect("values are never NaN");
            return Ok(Scalar::Bool(cmp(op, ord)));
        }
    };
    finite(r, op_str(op))
}

/// Complex arithmetic; `None` for operators complex numbers lack.
fn cnum_binary(op: OpKind, a: Complex, b: Complex) -> Option<Result<Scalar, Error>> {
    debug_assert!(!a.re.is_nan(), "operands are finite");
    debug_assert!(!b.re.is_nan(), "operands are finite");
    let r = match op {
        OpKind::Add => a + b,
        OpKind::Sub => a - b,
        OpKind::Mul => a * b,
        OpKind::Div => a / b,
        OpKind::Pow => dmath::cpow(a, b),
        _ => return None,
    };
    Some(complex_finite(r, op_str(op)))
}

fn str_binary(op: OpKind, a: Scalar, b: Scalar, strings: &mut String) -> Result<Scalar, Error> {
    let (Scalar::Str(sa, la), Scalar::Str(sb, lb)) = (a, b) else {
        unreachable!("str_binary of non-strings")
    };
    let (ra, rb) = (
        sa as usize..(sa + la) as usize,
        sb as usize..(sb + lb) as usize,
    );
    debug_assert!(ra.end <= strings.len(), "left operand past the buffer");
    debug_assert!(rb.end <= strings.len(), "right operand past the buffer");
    if op == OpKind::Add {
        eval::check_str_len(strings, (la + lb) as usize)?;
        let start = strings.len();
        strings.extend_from_within(ra);
        strings.extend_from_within(rb);
        debug_assert_eq!(strings.len(), start + (la + lb) as usize);
        return Ok(Scalar::Str(start as u32, la + lb));
    }
    if matches!(
        op,
        OpKind::Sub | OpKind::Mul | OpKind::Div | OpKind::Rem | OpKind::Pow
    ) {
        return Err(type_err(op_str(op), a, b));
    }
    Ok(Scalar::Bool(cmp(op, strings[ra].cmp(&strings[rb]))))
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
    args: &[Scalar],
    num: impl Fn(Scalar) -> Result<f64, Error>,
    bad: impl Fn(Scalar) -> Error,
) -> Result<Scalar, Error> {
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
    debug_assert!(
        k + 1 < bounds.len(),
        "step {k} lies between two breakpoints"
    );
    debug_assert!(2 + 2 * k < args.len() - 1);
    match args[2 + 2 * k] {
        y @ (Scalar::Int(_) | Scalar::Num(_)) => Ok(y),
        v => Err(bad(v)),
    }
}

/// Appends what `f` writes to `strings` as a new string.
fn write_str(strings: &mut String, f: impl FnOnce(&mut String)) -> Result<Scalar, Error> {
    let start = strings.len();
    f(strings);
    assert!(strings.len() >= start, "writers only append");
    let len = strings.len() - start;
    eval::check_str_len(&strings[..start], len)?;
    Ok(Scalar::Str(start as u32, len as u32))
}
