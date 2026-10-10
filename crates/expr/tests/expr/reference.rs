//! Naive reference: a boxed expression tree, a tree-walking evaluator written
//! from the spec (§4, §5) without looking at the crate's evaluator, a
//! minimal-parentheses printer (spec §3 precedence), and proptest generators.
//! Test-only; slow and allocation-heavy on purpose.

use proptest::prelude::*;
use rustfft::num_complex::Complex64;
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

pub const VARS: [&str; 7] = ["a", "b", "c", "d", "s", "t", "z"];

// ── printer ───────────────────────────────────────────────────────────────

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

/// Prints with the fewest parentheses the grammar needs; `var` names variables.
pub fn print(e: &E, var: &dyn Fn(usize) -> String) -> String {
    let mut s = String::new();
    put(e, 0, var, &mut s);
    s
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

// ── evaluator ─────────────────────────────────────────────────────────────

pub type R = Result<RV, Code>;

fn fin(x: f64) -> R {
    if x.is_finite() {
        Ok(RV::Num(x))
    } else {
        Err(Code::X010)
    }
}

fn cfin(z: Complex64) -> R {
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

/// Correctly rounded literal value (spec §2): suffix folded into the exponent.
pub fn lit_value(l: &NumLit) -> f64 {
    let e = l.exp.unwrap_or(0);
    match suffix_pow(l.suffix) {
        Some(p) => format!("{}e{}", l.mant, e + p).parse().unwrap(),
        None => {
            assert!(l.suffix.eq_ignore_ascii_case("mil"));
            format!("{}e{}", l.mant, e).parse::<f64>().unwrap() * 25.4e-6
        }
    }
}

fn num(v: &RV) -> Result<f64, Code> {
    match v {
        RV::Int(x) => Ok(*x as f64),
        RV::Num(x) => Ok(*x),
        _ => Err(Code::X003),
    }
}

fn cplx(v: &RV) -> Option<Complex64> {
    match v {
        RV::Int(x) => Some(Complex64::new(*x as f64, 0.0)),
        RV::Num(x) => Some(Complex64::new(*x, 0.0)),
        RV::C(a, b) => Some(Complex64::new(*a, *b)),
        _ => None,
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
                return cmp(a.partial_cmp(&b).unwrap());
            }
            fin(match op {
                Op::Add => a + b,
                Op::Sub => a - b,
                Op::Mul => a * b,
                Op::Div => a / b,
                Op::Rem => a % b,
                Op::Pow => schemify_expr::dmath::pow(a, b),
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

fn builtin(f: &str, v: &[RV]) -> R {
    let n0 = || num(&v[0]);
    match f {
        "abs" => match &v[0] {
            RV::Int(x) => Ok(x.checked_abs().map_or(RV::Num((*x as f64).abs()), RV::Int)),
            RV::C(a, b) => fin(schemify_expr::dmath::hypot(*a, *b)),
            o => Ok(RV::Num(num(o)?.abs())),
        },
        "sqrt" | "exp" | "ln" | "log10" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan"
        | "sinh" | "cosh" | "tanh" => {
            if let RV::C(a, b) = v[0] {
                let z = Complex64::new(a, b);
                return cfin(schemify_expr::dmath::complex_named(f, z).expect("elementary"));
            }
            let x = n0()?;
            fin(schemify_expr::dmath::real_named(f, x).expect("elementary"))
        }
        "atan2" => fin(schemify_expr::dmath::atan2(n0()?, num(&v[1])?)),
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
                    if f == "min" { it.min() } else { it.max() }.unwrap(),
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
                RV::C(a, b) => Complex64::new(*a, *b),
                o => Complex64::new(num(o)?, 0.0),
            };
            match f {
                "re" => Ok(RV::Num(z.re)),
                "im" => Ok(RV::Num(z.im)),
                "mag" => fin(schemify_expr::dmath::norm(z)),
                "ph" if z.re == 0.0 && z.im == 0.0 => Ok(RV::Num(0.0)),
                "ph" => Ok(RV::Num(
                    schemify_expr::dmath::atan2(z.im, z.re).to_degrees(),
                )),
                "db" => fin(20.0 * schemify_expr::dmath::log10(schemify_expr::dmath::norm(z))),
                _ => match &v[0] {
                    RV::C(a, b) => Ok(RV::C(*a, -*b)),
                    o => Ok(o.clone()),
                },
            }
        }
        _ => panic!("reference has no builtin `{f}`"),
    }
}

// ── generators ────────────────────────────────────────────────────────────

/// Scalar builtins the generator calls, with arity.
const FUNCS: [(&str, usize); 34] = [
    ("abs", 1),
    ("sqrt", 1),
    ("exp", 1),
    ("ln", 1),
    ("log10", 1),
    ("sin", 1),
    ("cos", 1),
    ("tan", 1),
    ("asin", 1),
    ("acos", 1),
    ("atan", 1),
    ("atan2", 2),
    ("sinh", 1),
    ("cosh", 1),
    ("tanh", 1),
    ("floor", 1),
    ("ceil", 1),
    ("round", 1),
    ("int", 1),
    ("min", 2),
    ("max", 3),
    ("clamp", 3),
    ("sign", 1),
    ("pow", 2),
    ("if", 3),
    ("snap", 2),
    ("str", 1),
    ("num", 1),
    ("len", 1),
    ("re", 1),
    ("mag", 1),
    ("ph", 1),
    ("db", 1),
    ("cplx", 2),
];

const SUFFIXES: [&str; 14] = [
    "", "", "", "f", "p", "n", "u", "m", "k", "Meg", "G", "t", "mil", "MIL",
];

pub fn num_lit() -> impl Strategy<Value = NumLit> {
    (
        0u32..100_000,
        prop::option::of(0u32..4),
        prop::option::of(-20i32..20),
        prop::sample::select(&SUFFIXES[..]),
    )
        .prop_map(|(m, frac, exp, suffix)| {
            let mant = match frac {
                Some(0) => format!(".{m}"),
                Some(k) => {
                    let s = format!("{m:0>width$}", width = k as usize + 1);
                    let split = s.len() - k as usize;
                    format!("{}.{}", &s[..split], &s[split..])
                }
                None => m.to_string(),
            };
            // A bare digit string with no exponent/suffix lexes as an int; force a num.
            let mant = if !mant.contains('.') && exp.is_none() && suffix.is_empty() {
                format!("{mant}.5")
            } else {
                mant
            };
            NumLit { mant, exp, suffix }
        })
}

fn leaf(nvars: usize) -> impl Strategy<Value = E> {
    prop_oneof![
        4 => (0i64..1000).prop_map(E::Int),
        1 => prop::sample::select(vec![i64::MAX, i64::MIN + 1, 1 << 40, -(1 << 40), 0]).prop_map(E::Int),
        4 => num_lit().prop_map(E::Num),
        6 => (0..nvars).prop_map(E::Var),
        1 => "[a-z0-9\"\\\\ {}é]{0,6}".prop_map(E::Str),
        1 => prop::sample::select(vec!["1k", "2.5", "-3", "x", "1e400", "", "4meg"]).prop_map(|s| E::Str(s.to_string())),
        1 => any::<bool>().prop_map(E::Bool),
        1 => Just(E::Pi),
    ]
}

const BIN_OPS: [Op; 14] = [
    Op::Or,
    Op::And,
    Op::Eq,
    Op::Ne,
    Op::Lt,
    Op::Le,
    Op::Gt,
    Op::Ge,
    Op::Add,
    Op::Sub,
    Op::Mul,
    Op::Div,
    Op::Rem,
    Op::Pow,
];

/// Random expressions over `nvars` variables, depth ≤ 6.
pub fn expr(nvars: usize) -> impl Strategy<Value = E> {
    leaf(nvars).prop_recursive(6, 96, 3, |inner| {
        prop_oneof![
            2 => inner.clone().prop_map(|x| E::Neg(Box::new(x))),
            1 => inner.clone().prop_map(|x| E::Not(Box::new(x))),
            8 => (prop::sample::select(&BIN_OPS[..]), inner.clone(), inner.clone()).prop_map(|(op, a, b)| E::Bin(op, Box::new(a), Box::new(b))),
            2 => (inner.clone(), inner.clone(), inner.clone()).prop_map(|(c, t, f)| E::Tern(Box::new(c), Box::new(t), Box::new(f))),
            4 => (prop::sample::select(&FUNCS[..]), prop::collection::vec(inner, 3)).prop_map(|((f, n), mut args)| {
                args.truncate(n);
                E::Call(f, args)
            }),
        ]
    })
}

const ARITH: [Op; 6] = [Op::Add, Op::Sub, Op::Mul, Op::Div, Op::Rem, Op::Pow];
const CMP: [Op; 6] = [Op::Eq, Op::Ne, Op::Lt, Op::Le, Op::Gt, Op::Ge];

fn num_leaf(nvars: usize) -> BoxedStrategy<E> {
    let numeric_vars = nvars.min(4);
    prop_oneof![
        4 => (0i64..1000).prop_map(E::Int),
        1 => prop::sample::select(vec![i64::MAX, i64::MIN + 1, 1 << 40, -(1 << 40), 0]).prop_map(E::Int),
        4 => num_lit().prop_map(E::Num),
        5 => (0..numeric_vars).prop_map(E::Var),
        1 => Just(if nvars > 6 { E::Var(6) } else { E::Pi }),
        1 => Just(E::Pi),
    ]
    .boxed()
}

fn bin(op: Op, a: E, b: E) -> E {
    E::Bin(op, Box::new(a), Box::new(b))
}

/// Mostly well-typed numeric expressions, so most cases reach a value.
pub fn num_expr(nvars: usize, depth: u32) -> BoxedStrategy<E> {
    if depth == 0 {
        return num_leaf(nvars);
    }
    let (n, b, s) = (
        num_expr(nvars, depth - 1),
        bool_expr(nvars, depth - 1),
        str_expr(nvars, depth - 1),
    );
    prop_oneof![
        3 => num_leaf(nvars),
        6 => (prop::sample::select(&ARITH[..]), n.clone(), n.clone()).prop_map(|(op, x, y)| bin(op, x, y)),
        1 => n.clone().prop_map(|x| E::Neg(Box::new(x))),
        1 => (b.clone(), n.clone(), n.clone()).prop_map(|(c, t, f)| E::Tern(Box::new(c), Box::new(t), Box::new(f))),
        1 => (b, n.clone(), n.clone()).prop_map(|(c, t, f)| E::Call("if", vec![c, t, f])),
        4 => (prop::sample::select(&FUNCS[..]), prop::collection::vec(n, 3)).prop_filter_map("numeric builtin", |((f, k), mut args)| {
            if matches!(f, "str" | "len" | "num" | "if") {
                return None;
            }
            args.truncate(k);
            Some(E::Call(f, args))
        }),
        1 => s.clone().prop_map(|x| E::Call("len", vec![x])),
        1 => s.prop_map(|x| E::Call("num", vec![x])),
    ]
    .boxed()
}

pub fn bool_expr(nvars: usize, depth: u32) -> BoxedStrategy<E> {
    let leaf = prop_oneof![
        any::<bool>().prop_map(E::Bool),
        Just(if nvars > 5 { E::Var(5) } else { E::Bool(true) })
    ];
    if depth == 0 {
        return leaf.boxed();
    }
    let (n, b) = (num_expr(nvars, depth - 1), bool_expr(nvars, depth - 1));
    prop_oneof![
        1 => leaf,
        4 => (prop::sample::select(&CMP[..]), n.clone(), n).prop_map(|(op, x, y)| bin(op, x, y)),
        2 => (prop::sample::select(vec![Op::And, Op::Or]), b.clone(), b.clone()).prop_map(|(op, x, y)| bin(op, x, y)),
        1 => b.prop_map(|x| E::Not(Box::new(x))),
    ]
    .boxed()
}

pub fn str_expr(nvars: usize, depth: u32) -> BoxedStrategy<E> {
    let leaf = prop_oneof![
        2 => prop::sample::select(vec!["1k", "2.5", "-3", "x", "1e400", "", "4meg", "7mil"]).prop_map(|s| E::Str(s.to_string())),
        1 => "[a-z0-9\"\\\\ {}é]{0,6}".prop_map(E::Str),
        1 => Just(if nvars > 4 { E::Var(4) } else { E::Str("s".into()) }),
    ];
    if depth == 0 {
        return leaf.boxed();
    }
    let (n, s) = (num_expr(nvars, depth - 1), str_expr(nvars, depth - 1));
    prop_oneof![
        2 => leaf,
        2 => n.prop_map(|x| E::Call("str", vec![x])),
        1 => (s.clone(), s).prop_map(|(x, y)| bin(Op::Add, x, y)),
    ]
    .boxed()
}

/// The differential's mix: typed numeric/bool/string expressions plus the
/// untyped generator for negative space.
pub fn mixed(nvars: usize) -> BoxedStrategy<E> {
    prop_oneof![
        4 => num_expr(nvars, 5),
        1 => bool_expr(nvars, 4),
        1 => str_expr(nvars, 3),
        2 => expr(nvars),
    ]
    .boxed()
}

/// Values for VARS: a, b, c, d numeric (int or num), s str, t bool, z cnum.
pub fn env() -> impl Strategy<Value = Vec<RV>> {
    let numeric = prop_oneof![
        (-50i64..50).prop_map(RV::Int),
        (-1e3f64..1e3).prop_map(RV::Num),
        prop::sample::select(vec![0.0, -0.0, 1e-300, 1e300, 0.5]).prop_map(RV::Num),
    ];
    (
        prop::collection::vec(numeric, 4),
        "[a-z0-9.]{0,5}",
        any::<bool>(),
        (-10f64..10.0, -10f64..10.0),
    )
        .prop_map(|(mut v, s, t, (re, im))| {
            v.push(RV::Str(s));
            v.push(RV::Bool(t));
            v.push(RV::C(re, im));
            v
        })
}
