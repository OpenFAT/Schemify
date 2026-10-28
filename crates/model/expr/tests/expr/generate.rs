//! Proptest generators for the differential oracle: random `E` trees of
//! every type the reference evaluates (numeric, bool, str, mixed), number
//! literals with SI suffixes, and the variable environment for `reference::VARS`.
//!
//! Contents:
//! - `pub fn num_lit() -> impl Strategy<Value = NumLit>`
//! - `pub fn expr(nvars: usize) -> impl Strategy<Value = E>`
//! - `pub fn num_expr(nvars: usize, depth: u32) -> BoxedStrategy<E>`
//! - `pub fn bool_expr(nvars: usize, depth: u32) -> BoxedStrategy<E>`
//! - `pub fn str_expr(nvars: usize, depth: u32) -> BoxedStrategy<E>`
//! - `pub fn mixed(nvars: usize) -> BoxedStrategy<E>`
//! - `pub fn env() -> impl Strategy<Value = Vec<RV>>`

use proptest::prelude::*;

use crate::reference::{E, NumLit, Op, RV};

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

const ARITH: [Op; 6] = [Op::Add, Op::Sub, Op::Mul, Op::Div, Op::Rem, Op::Pow];

const CMP: [Op; 6] = [Op::Eq, Op::Ne, Op::Lt, Op::Le, Op::Gt, Op::Ge];

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

// ---- private ----

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
