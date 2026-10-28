//! Naive reference: a boxed expression tree, a tree-walking evaluator written
//! from the spec (§4, §5) without looking at the crate's evaluator, a
//! minimal-parentheses printer (spec §3 precedence). Its inputs come from
//! `generate`. Test-only; slow and allocation-heavy on purpose.
//!
//! Contents:
//! - `pub enum Op`, `pub struct NumLit`, `pub enum E`, `pub enum RV`,
//!   `RV::as_value(&self) -> Value`, `pub type R`, `pub const VARS`
//! - `pub fn print(e: &E, var: &dyn Fn(usize) -> String) -> String`
//! - `pub fn lit_value(l: &NumLit) -> f64`
//! - `pub fn eval(e: &E, env: &[RV]) -> R`
//! - `pub fn canonical(x: f64) -> String`

use schemify_expr::dmath::Complex;
use schemify_expr::{Code, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
}

/// A number literal as written: mantissa text, decimal exponent, suffix.
#[derive(Clone, Debug)]
pub struct NumLit {
    pub mant: String,
    pub exp: Option<i32>,
    pub suffix: &'static str,
}

#[derive(Clone, Debug)]
pub enum E {
    Int(i64),
    Num(NumLit),
    Str(String),
    Bool(bool),
    Pi,
    Var(usize),
    Neg(Box<E>),
    Not(Box<E>),
    Bin(Op, Box<E>, Box<E>),
    Tern(Box<E>, Box<E>, Box<E>),
    Call(&'static str, Vec<E>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum RV {
    Int(i64),
    Num(f64),
    Bool(bool),
    Str(String),
    C(f64, f64),
}

pub const VARS: [&str; 7] = ["a", "b", "c", "d", "s", "t", "z"];

pub type R = Result<RV, Code>;

impl RV {
    pub fn as_value(&self) -> Value<'_> {
        match self {
            RV::Int(x) => Value::Int(*x),
            RV::Num(x) => Value::Num(*x),
            RV::Bool(b) => Value::Bool(*b),
            RV::Str(s) => Value::Str(s),
            RV::C(re, im) => Value::Cnum { re: *re, im: *im },
        }
    }
}

/// Prints with the fewest parentheses the grammar needs; `var` names variables.
pub fn print(e: &E, var: &dyn Fn(usize) -> String) -> String {
    let mut s = String::new();
    put(e, 0, var, &mut s);
    s
}

/// Correctly rounded literal value (spec §2): suffix folded into the exponent.
pub fn lit_value(l: &NumLit) -> f64 {
    let e = l.exp.unwrap_or(0);
    match suffix_pow(l.suffix) {
        Some(p) => format!("{}e{}", l.mant, e + p)
            .parse::<f64>()
            .expect("mantissa and exponent form a float"),
        None => {
            assert!(l.suffix.eq_ignore_ascii_case("mil"));
            format!("{}e{}", l.mant, e)
                .parse::<f64>()
                .expect("mantissa and exponent form a float")
                * 25.4e-6
        }
    }
}

pub fn eval(e: &E, env: &[RV]) -> R {
    match e {
        E::Int(x) => Ok(RV::Int(*x)),
        E::Num(l) => Ok(RV::Num(lit_value(l))),
        E::Str(s) => Ok(RV::Str(s.clone())),
        E::Bool(b) => Ok(RV::Bool(*b)),
        E::Pi => Ok(RV::Num(std::f64::consts::PI)),
        E::Var(i) => Ok(env[*i].clone()),
        E::Neg(x) => match eval(x, env)? {
            RV::Int(v) => Ok(v.checked_neg().map_or(RV::Num(-(v as f64)), RV::Int)),
            RV::Num(v) => Ok(RV::Num(-v)),
            RV::C(a, b) => Ok(RV::C(-a, -b)),
            _ => Err(Code::X003),
        },
        E::Not(x) => match eval(x, env)? {
            RV::Bool(b) => Ok(RV::Bool(!b)),
            _ => Err(Code::X003),
        },
        E::Bin(op @ (Op::And | Op::Or), a, b) => {
            let short = *op == Op::Or;
            match eval(a, env)? {
                RV::Bool(x) if x == short => Ok(RV::Bool(x)),
                RV::Bool(_) => match eval(b, env)? {
                    RV::Bool(y) => Ok(RV::Bool(y)),
                    _ => Err(Code::X003),
                },
                _ => Err(Code::X003),
            }
        }
        E::Bin(op, a, b) => {
            let x = eval(a, env)?;
            let y = eval(b, env)?;
            binop(*op, &x, &y)
        }
        E::Tern(c, t, f) => tern(c, t, f, env),
        E::Call("if", args) => tern(&args[0], &args[1], &args[2], env),
        E::Call(f, args) => {
            let mut vals = Vec::new();
            for a in args {
                vals.push(eval(a, env)?);
            }
            builtin(f, &vals)
        }
    }
}

/// Spec §7, re-derived for the reference.
pub fn canonical(x: f64) -> String {
    if x == x.trunc() && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else if x.abs() >= 1e-4 && x.abs() < 1e15 {
        format!("{x}")
    } else {
        format!("{x:e}")
    }
}

// ---- private ----

fn prec(e: &E) -> u8 {
    match e {
        E::Tern(..) => 0,
        E::Bin(Op::Or, ..) => 1,
        E::Bin(Op::And, ..) => 2,
        E::Bin(Op::Eq | Op::Ne | Op::Lt | Op::Le | Op::Gt | Op::Ge, ..) => 3,
        E::Bin(Op::Add | Op::Sub, ..) => 4,
        E::Bin(Op::Mul | Op::Div | Op::Rem, ..) => 5,
        E::Neg(_) | E::Not(_) => 6,
        E::Bin(Op::Pow, ..) => 7,
        _ => 8,
    }
}

fn op_text(op: Op) -> &'static str {
    match op {
        Op::Or => "||",
        Op::And => "&&",
        Op::Eq => "==",
        Op::Ne => "!=",
        Op::Lt => "<",
        Op::Le => "<=",
        Op::Gt => ">",
        Op::Ge => ">=",
        Op::Add => "+",
        Op::Sub => "-",
        Op::Mul => "*",
        Op::Div => "/",
        Op::Rem => "%",
        Op::Pow => "^",
    }
}

fn put(e: &E, min: u8, var: &dyn Fn(usize) -> String, s: &mut String) {
    let paren = prec(e) < min;
    if paren {
        s.push('(');
    }
    match e {
        // A negative literal does not exist in the grammar; print it as `(-n)`.
        E::Int(x) if *x < 0 => s.push_str(&format!("({x})")),
        E::Int(x) => s.push_str(&x.to_string()),
        E::Num(l) => {
            s.push_str(&l.mant);
            if let Some(x) = l.exp {
                s.push_str(&format!("e{x}"));
            }
            s.push_str(l.suffix);
        }
        E::Str(t) => {
            s.push('"');
            for ch in t.chars() {
                match ch {
                    '"' => s.push_str("\\\""),
                    '\\' => s.push_str("\\\\"),
                    '\n' => s.push_str("\\n"),
                    '\t' => s.push_str("\\t"),
                    c if (c as u32) < 0x20 => s.push_str(&format!("\\u{{{:x}}}", c as u32)),
                    c => s.push(c),
                }
            }
            s.push('"');
        }
        E::Bool(b) => s.push_str(if *b { "true" } else { "false" }),
        E::Pi => s.push_str("pi"),
        E::Var(i) => s.push_str(&var(*i)),
        E::Neg(x) | E::Not(x) => {
            s.push(if matches!(e, E::Neg(_)) { '-' } else { '!' });
            put(x, 6, var, s);
        }
        E::Bin(op, a, b) => {
            let (l, r) = match op {
                Op::Pow => (8, 6),
                Op::Eq | Op::Ne | Op::Lt | Op::Le | Op::Gt | Op::Ge => (4, 4),
                _ => (prec(e), prec(e) + 1),
            };
            put(a, l, var, s);
            s.push(' ');
            s.push_str(op_text(*op));
            s.push(' ');
            put(b, r, var, s);
        }
        E::Tern(c, t, f) => {
            put(c, 1, var, s);
            s.push_str(" ? ");
            put(t, 0, var, s);
            s.push_str(" : ");
            put(f, 0, var, s);
        }
        E::Call(f, args) => {
            s.push_str(f);
            s.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                put(a, 0, var, s);
            }
            s.push(')');
        }
    }
    if paren {
        s.push(')');
    }
}

fn fin(x: f64) -> R {
    if x.is_finite() {
        Ok(RV::Num(x))
    } else {
        Err(Code::X010)
    }
}

fn cfin(z: Complex) -> R {
    if z.re.is_finite() && z.im.is_finite() {
        Ok(RV::C(z.re, z.im))
    } else {
        Err(Code::X010)
    }
}

fn suffix_pow(s: &str) -> Option<i32> {
    Some(match s.to_ascii_lowercase().as_str() {
        "" => 0,
        "f" => -15,
        "p" => -12,
        "n" => -9,
        "u" | "\u{b5}" | "\u{3bc}" => -6,
        "m" => -3,
        "k" => 3,
        "meg" => 6,
        "g" => 9,
        "t" => 12,
        _ => return None,
    })
}

fn num(v: &RV) -> Result<f64, Code> {
    match v {
        RV::Int(x) => Ok(*x as f64),
        RV::Num(x) => Ok(*x),
        _ => Err(Code::X003),
    }
}

fn cplx(v: &RV) -> Option<Complex> {
    match v {
        RV::Int(x) => Some(Complex::new(*x as f64, 0.0)),
        RV::Num(x) => Some(Complex::new(*x, 0.0)),
        RV::C(a, b) => Some(Complex::new(*a, *b)),
        _ => None,
    }
}

fn tern(c: &E, t: &E, f: &E, env: &[RV]) -> R {
    match eval(c, env)? {
        RV::Bool(true) => eval(t, env),
        RV::Bool(false) => eval(f, env),
        _ => Err(Code::X003),
    }
}

fn binop(op: Op, x: &RV, y: &RV) -> R {
    use std::cmp::Ordering;
    let cmp = |o: Ordering| -> R {
        Ok(RV::Bool(match op {
            Op::Eq => o == Ordering::Equal,
            Op::Ne => o != Ordering::Equal,
            Op::Lt => o == Ordering::Less,
            Op::Le => o != Ordering::Greater,
            Op::Gt => o == Ordering::Greater,
            Op::Ge => o != Ordering::Less,
            _ => unreachable!(),
        }))
    };
    let is_cmp = matches!(op, Op::Eq | Op::Ne | Op::Lt | Op::Le | Op::Gt | Op::Ge);
    match (x, y) {
        (RV::Int(a), RV::Int(b)) => {
            if is_cmp {
                return cmp(a.cmp(b));
            }
            let r = match op {
                Op::Add => a.checked_add(*b),
                Op::Sub => a.checked_sub(*b),
                Op::Mul => a.checked_mul(*b),
                Op::Rem => a.checked_rem(*b),
                _ => None,
            };
            match r {
                Some(v) => Ok(RV::Int(v)),
                None => binop(op, &RV::Num(*a as f64), &RV::Num(*b as f64)),
            }
        }
        (RV::Int(_) | RV::Num(_), RV::Int(_) | RV::Num(_)) => {
            let (a, b) = (num(x)?, num(y)?);
            if is_cmp {
                return cmp(a.partial_cmp(&b).expect("finite operands are ordered"));
            }
            fin(match op {
                Op::Add => a + b,
                Op::Sub => a - b,
                Op::Mul => a * b,
                Op::Div => a / b,
                Op::Rem => a % b,
                Op::Pow => libm::pow(a, b),
                _ => unreachable!(),
            })
        }
        (RV::C(..), RV::C(..)) if matches!(op, Op::Eq | Op::Ne) => {
            Ok(RV::Bool((x == y) == (op == Op::Eq)))
        }
        (RV::C(..), _) | (_, RV::C(..)) => {
            let (Some(a), Some(b)) = (cplx(x), cplx(y)) else {
                return Err(Code::X003);
            };
            cfin(match op {
                Op::Add => a + b,
                Op::Sub => a - b,
                Op::Mul => a * b,
                Op::Div => a / b,
                Op::Pow => schemify_expr::dmath::cpow(a, b),
                _ => return Err(Code::X003),
            })
        }
        (RV::Str(a), RV::Str(b)) => match op {
            Op::Add if a.len() + b.len() > (1 << 20) => Err(Code::X002),
            Op::Add => Ok(RV::Str(format!("{a}{b}"))),
            _ if is_cmp => cmp(a.as_bytes().cmp(b.as_bytes())),
            _ => Err(Code::X003),
        },
        (RV::Bool(a), RV::Bool(b)) if matches!(op, Op::Eq | Op::Ne) => {
            Ok(RV::Bool((a == b) == (op == Op::Eq)))
        }
        _ => Err(Code::X003),
    }
}

fn builtin(f: &str, v: &[RV]) -> R {
    let n0 = || num(&v[0]);
    match f {
        "abs" => match &v[0] {
            RV::Int(x) => Ok(x.checked_abs().map_or(RV::Num((*x as f64).abs()), RV::Int)),
            RV::C(a, b) => fin(libm::hypot(*a, *b)),
            o => Ok(RV::Num(num(o)?.abs())),
        },
        "sqrt" | "exp" | "ln" | "log10" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan"
        | "sinh" | "cosh" | "tanh" => {
            if let RV::C(a, b) = v[0] {
                let z = Complex::new(a, b);
                return cfin(schemify_expr::dmath::complex_named(f, z).expect("elementary"));
            }
            let x = n0()?;
            fin(schemify_expr::dmath::real_named(f, x).expect("elementary"))
        }
        "atan2" => fin(libm::atan2(n0()?, num(&v[1])?)),
        "floor" | "ceil" | "round" => match &v[0] {
            RV::Int(x) => Ok(RV::Int(*x)),
            o => {
                let x = num(o)?;
                Ok(RV::Num(match f {
                    "floor" => x.floor(),
                    "ceil" => x.ceil(),
                    _ => x.round(),
                }))
            }
        },
        "int" => match &v[0] {
            RV::Int(x) => Ok(RV::Int(*x)),
            o => {
                let t = num(o)?.trunc();
                if t >= -(2f64.powi(63)) && t < 2f64.powi(63) {
                    Ok(RV::Int(t as i64))
                } else {
                    Err(Code::X010)
                }
            }
        },
        "min" | "max" => {
            if v.iter().all(|x| matches!(x, RV::Int(_))) {
                let it = v.iter().map(|x| if let RV::Int(i) = x { *i } else { 0 });
                return Ok(RV::Int(
                    if f == "min" { it.min() } else { it.max() }
                        .expect("min/max take at least one argument"),
                ));
            }
            let mut m = n0()?;
            for x in &v[1..] {
                let x = num(x)?;
                m = if f == "min" { m.min(x) } else { m.max(x) };
            }
            Ok(RV::Num(m))
        }
        "clamp" => match (&v[0], &v[1], &v[2]) {
            (RV::Int(x), RV::Int(l), RV::Int(h)) => Ok(RV::Int((*x).max(*l).min(*h))),
            (x, l, h) => Ok(RV::Num(num(x)?.max(num(l)?).min(num(h)?))),
        },
        "sign" => match &v[0] {
            RV::Int(x) => Ok(RV::Int(x.signum())),
            o => {
                let x = num(o)?;
                Ok(RV::Num(if x > 0.0 {
                    1.0
                } else if x < 0.0 {
                    -1.0
                } else {
                    0.0
                }))
            }
        },
        "pow" => binop(Op::Pow, &v[0], &v[1]),
        "snap" => {
            let (x, g) = (n0()?, num(&v[1])?);
            fin((x / g).round() * g)
        }
        "str" => match &v[0] {
            RV::Str(s) => Ok(RV::Str(s.clone())),
            RV::Int(x) => Ok(RV::Str(x.to_string())),
            RV::Num(x) => Ok(RV::Str(canonical(*x))),
            RV::Bool(b) => Ok(RV::Str(b.to_string())),
            RV::C(..) => Err(Code::X003),
        },
        "num" => match &v[0] {
            RV::Str(s) => schemify_expr::parse_num(s).map(RV::Num).map_err(|e| e.code),
            o => Ok(RV::Num(num(o)?)),
        },
        "len" => match &v[0] {
            RV::Str(s) => Ok(RV::Int(s.chars().count() as i64)),
            _ => Err(Code::X003),
        },
        "re" | "im" | "mag" | "ph" | "db" | "conj" | "cplx" if f == "cplx" => {
            Ok(RV::C(n0()?, num(&v[1])?))
        }
        "re" | "im" | "mag" | "ph" | "db" | "conj" => {
            let z = match &v[0] {
                RV::C(a, b) => Complex::new(*a, *b),
                o => Complex::new(num(o)?, 0.0),
            };
            match f {
                "re" => Ok(RV::Num(z.re)),
                "im" => Ok(RV::Num(z.im)),
                "mag" => fin(schemify_expr::dmath::norm(z)),
                "ph" if z.re == 0.0 && z.im == 0.0 => Ok(RV::Num(0.0)),
                "ph" => Ok(RV::Num(libm::atan2(z.im, z.re).to_degrees())),
                "db" => fin(20.0 * libm::log10(schemify_expr::dmath::norm(z))),
                _ => match &v[0] {
                    RV::C(a, b) => Ok(RV::C(*a, -*b)),
                    o => Ok(o.clone()),
                },
            }
        }
        _ => panic!("reference has no builtin `{f}`"),
    }
}
