//! Scalar evaluation: the stack machine over [`Program`] bytecode and the
//! scalar kernels (operators and builtins) that calculator mode reuses per
//! element. Semantics: spec §4 (promotion, short-circuit, X003/X010).

use core::cmp::Ordering;
use core::fmt::Write;

use crate::compile::{Builtin, Mode, OpKind, Program};
use crate::dmath::{self, Complex};
use crate::text::{Bytes, fmt_eng, fmt_num};
use crate::{Code, Error, MAX_PLUGIN_ARGS, MAX_STR_BYTES, Span, Value};

/// Total bytes of strings one evaluation may create before `X002`.
const MAX_SCRATCH_STR_BYTES: usize = 64 << 20;

/// Internal scalar. `Str` is a byte range in [`Scratch::strs`] (or, inside
/// [`Program::consts`], in the program's pool).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum V {
    Int(i64),
    Num(f64),
    Bool(bool),
    Cnum(f64, f64),
    Str(u32, u32),
}

/// Reusable evaluation memory: value stacks and the string buffer.
///
/// Keep one per thread and pass it to every evaluation; after the first few
/// evaluations nothing allocates in scalar mode (strings excepted). Results
/// that borrow from it ([`Value::Str`]) are invalidated by the next
/// evaluation.
#[derive(Debug, Default)]
pub struct Scratch {
    pub(crate) stack: Vec<V>,
    pub(crate) strs: Vec<u8>,
    pub(crate) calc: crate::calc::CalcScratch,
}

impl Program {
    /// Evaluates a [`Mode::Scalar`] program. Plugin calls fail with `X006`;
    /// use [`Program::eval_with`] to supply them.
    ///
    /// `inputs` holds one value per identifier, in [`Program::idents`] order
    /// (see [`Program::bind`]).
    ///
    /// # Errors
    /// Runtime `X003` (type), `X010` (NaN/inf), `X001` (`num()` of bad text),
    /// `X002` (string too long), `X006` (plugin call), spanned at the operator
    /// or call that failed.
    ///
    /// # Panics
    /// If the program is not [`Mode::Scalar`], is a template, or
    /// `inputs.len() != idents().len()`.
    pub fn eval<'s>(
        &self,
        inputs: &[Value<'_>],
        scratch: &'s mut Scratch,
    ) -> Result<Value<'s>, Error> {
        self.eval_with(inputs, &mut no_plugins(self), scratch)
    }

    /// [`Program::eval`] with plugin functions. `plugins(slot, args)` is called
    /// for every `ns.f(args)`, with `slot` indexing [`Program::plugin_fns`].
    /// It must be pure; a missing plugin should return `X006` (callers holding
    /// a D20 cached value substitute it for the whole expression instead of
    /// evaluating). Results must be finite; a result is copied in at once, so it may own its
    /// text ([`PluginValue`]).
    ///
    /// # Errors
    /// As [`Program::eval`], plus whatever `plugins` returns.
    pub fn eval_with<'s, P, R>(
        &self,
        inputs: &[Value<'_>],
        plugins: &mut P,
        scratch: &'s mut Scratch,
    ) -> Result<Value<'s>, Error>
    where
        P: FnMut(u32, &[Value<'_>]) -> Result<R, Error>,
        R: PluginValue,
    {
        assert_eq!(
            self.mode,
            Mode::Scalar,
            "eval of a calculator-mode program; use eval_calc"
        );
        assert_eq!(
            self.roots.len(),
            1,
            "eval of a template program; use Template::render"
        );
        assert_eq!(inputs.len(), self.idents.len(), "one input per identifier");
        let (start, end) = self.roots[0];
        let v = run(self, start, end, inputs, plugins, scratch)?;
        Ok(to_value(v, &scratch.strs))
    }
}

/// What a plugin callback returns ([`Program::eval_with`], D20): a value the evaluator copies
/// in as soon as the callback returns, so it may own its text (a plugin's string answer).
/// [`Value`]`<'static>` is one.
pub trait PluginValue {
    /// The value, borrowed for the copy.
    fn value(&self) -> Value<'_>;
}

impl PluginValue for Value<'static> {
    fn value(&self) -> Value<'_> {
        *self
    }
}

/// A plugin callback that reports every call as a missing plugin (`X006`).
pub(crate) fn no_plugins(
    prog: &Program,
) -> impl FnMut(u32, &[Value<'_>]) -> Result<Value<'static>, Error> + '_ {
    move |slot, _| {
        Err(Error::new(
            Code::X006,
            Span::default(),
            format!("plugin function `{}` is not loaded", prog.plugin_name(slot)),
        ))
    }
}

/// Runs `prog.ops[start..end]` to one value. Clears the scratch stack and strings.
pub(crate) fn run<P, R>(
    prog: &Program,
    start: u32,
    end: u32,
    inputs: &[Value<'_>],
    plugin: &mut P,
    s: &mut Scratch,
) -> Result<V, Error>
where
    P: FnMut(u32, &[Value<'_>]) -> Result<R, Error>,
    R: PluginValue,
{
    assert!(
        start < end && end as usize <= prog.ops.len(),
        "code range {start}..{end} invalid"
    );
    s.stack.clear();
    s.strs.clear();
    let mut pc = start;
    while pc < end {
        let op = prog.ops[pc as usize];
        let at = |e: Error| e.at(prog.spans[pc as usize]);
        match op.kind {
            OpKind::Const => {
                let v = load_const(prog, op.arg, &mut s.strs).map_err(at)?;
                s.stack.push(v);
            }
            OpKind::Input => {
                let v = match inputs[op.arg as usize] {
                    // Hot path: a finite num needs no conversion.
                    Value::Num(x) if x.is_finite() => V::Num(x),
                    ref v => load_value(v, &mut s.strs).map_err(at)?,
                };
                s.stack.push(v);
            }
            OpKind::Add | OpKind::Sub | OpKind::Mul | OpKind::Div
                if matches!(s.stack[s.stack.len() - 2..], [V::Num(_), V::Num(_)]) =>
            {
                // Hot path: num ⊕ num in place on the stack top.
                let Some(V::Num(y)) = s.stack.pop() else {
                    unreachable!("guarded above")
                };
                let top = s.stack.last_mut().expect("guarded above");
                let V::Num(x) = *top else {
                    unreachable!("guarded above")
                };
                let r = match op.kind {
                    OpKind::Add => x + y,
                    OpKind::Sub => x - y,
                    OpKind::Mul => x * y,
                    _ => x / y,
                };
                if !r.is_finite() {
                    return Err(at(fin(r, op_str(op.kind)).expect_err("non-finite")));
                }
                *top = V::Num(r);
            }
            OpKind::Signal | OpKind::Axis => {
                return Err(at(Error::new(
                    Code::X006,
                    Span::default(),
                    "signals need calculator evaluation",
                )));
            }
            OpKind::Neg | OpKind::Not => {
                let a = pop(&mut s.stack);
                s.stack.push(unary(op.kind, a).map_err(at)?);
            }
            OpKind::Add
            | OpKind::Sub
            | OpKind::Mul
            | OpKind::Div
            | OpKind::Rem
            | OpKind::Pow
            | OpKind::Eq
            | OpKind::Ne
            | OpKind::Lt
            | OpKind::Le
            | OpKind::Gt
            | OpKind::Ge => {
                let b = pop(&mut s.stack);
                let a = pop(&mut s.stack);
                s.stack
                    .push(binary(op.kind, a, b, &mut s.strs).map_err(at)?);
            }
            OpKind::Cond => match pop(&mut s.stack) {
                V::Bool(true) => {}
                V::Bool(false) => {
                    pc = op.arg;
                    continue;
                }
                v => return Err(at(not_bool("condition", v))),
            },
            OpKind::Else => {
                pc = op.arg;
                continue;
            }
            OpKind::EndCond => {}
            OpKind::And | OpKind::Or => {
                let short = op.kind == OpKind::Or;
                match pop(&mut s.stack) {
                    V::Bool(b) if b == short => {
                        s.stack.push(V::Bool(b));
                        pc = op.arg;
                        continue;
                    }
                    V::Bool(_) => {}
                    v => return Err(at(not_bool(op_str(op.kind), v))),
                }
            }
            OpKind::EndAnd | OpKind::EndOr => {
                let top = *s.stack.last().expect("operand on stack");
                if !matches!(top, V::Bool(_)) {
                    return Err(at(not_bool(op_str(op.kind), top)));
                }
            }
            OpKind::Call => {
                let base = s.stack.len() - op.arg as usize;
                let v =
                    call(Builtin::from_id(op.sub), &s.stack[base..], &mut s.strs).map_err(at)?;
                s.stack.truncate(base);
                s.stack.push(v);
            }
            OpKind::Plugin => {
                let base = s.stack.len() - op.sub as usize;
                let r = call_plugin(plugin, op.arg, &s.stack[base..], &s.strs);
                let v = r
                    .and_then(|r| load_value(&r.value(), &mut s.strs))
                    .map_err(at)?;
                s.stack.truncate(base);
                s.stack.push(v);
            }
        }
        pc += 1;
    }
    assert_eq!(s.stack.len(), 1, "program must leave exactly one value");
    Ok(s.stack[0])
}

fn pop(stack: &mut Vec<V>) -> V {
    stack.pop().expect("compiled code keeps the stack balanced")
}

pub(crate) fn call_plugin<P, R>(
    plugin: &mut P,
    slot: u32,
    args: &[V],
    strs: &[u8],
) -> Result<R, Error>
where
    P: FnMut(u32, &[Value<'_>]) -> Result<R, Error>,
    R: PluginValue,
{
    assert!(
        args.len() <= MAX_PLUGIN_ARGS,
        "plugin arity checked at compile time"
    );
    let mut vals = [Value::Bool(false); MAX_PLUGIN_ARGS];
    for (slot_v, v) in vals.iter_mut().zip(args) {
        *slot_v = to_value(*v, strs);
    }
    plugin(slot, &vals[..args.len()])
}

pub(crate) fn load_const(prog: &Program, k: u32, strs: &mut Vec<u8>) -> Result<V, Error> {
    match prog.consts[k as usize] {
        V::Str(s, len) => push_str(strs, &prog.pool.as_bytes()[s as usize..(s + len) as usize]),
        v => Ok(v),
    }
}

/// Converts a caller value (input or plugin result) into the evaluator's
/// representation, rejecting NaN/inf (`X010`).
pub(crate) fn load_value(v: &Value<'_>, strs: &mut Vec<u8>) -> Result<V, Error> {
    match *v {
        Value::Int(x) => Ok(V::Int(x)),
        Value::Num(x) => fin(x, "input"),
        Value::Bool(b) => Ok(V::Bool(b)),
        Value::Cnum { re, im } => cfin(Complex::new(re, im), "input"),
        Value::Str(s) => push_str(strs, s.as_bytes()),
    }
}

pub(crate) fn to_value(v: V, strs: &[u8]) -> Value<'_> {
    match v {
        V::Int(x) => Value::Int(x),
        V::Num(x) => Value::Num(x),
        V::Bool(b) => Value::Bool(b),
        V::Cnum(re, im) => Value::Cnum { re, im },
        V::Str(s, len) => Value::Str(
            core::str::from_utf8(&strs[s as usize..(s + len) as usize])
                .expect("scratch strings are UTF-8"),
        ),
    }
}

fn push_str(strs: &mut Vec<u8>, bytes: &[u8]) -> Result<V, Error> {
    check_str_len(strs, bytes.len())?;
    let start = strs.len();
    strs.extend_from_slice(bytes);
    Ok(V::Str(start as u32, bytes.len() as u32))
}

fn check_str_len(strs: &[u8], len: usize) -> Result<(), Error> {
    if len > MAX_STR_BYTES {
        return Err(Error::new(
            Code::X002,
            Span::default(),
            "string longer than 1 MiB",
        ));
    }
    if strs.len() + len > MAX_SCRATCH_STR_BYTES {
        return Err(Error::new(
            Code::X002,
            Span::default(),
            "expression creates more than 64 MiB of strings",
        ));
    }
    Ok(())
}

pub(crate) fn type_name(v: V) -> &'static str {
    match v {
        V::Int(_) => "int",
        V::Num(_) => "num",
        V::Bool(_) => "bool",
        V::Cnum(..) => "cnum",
        V::Str(..) => "str",
    }
}

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

pub(crate) fn not_bool(what: &str, v: V) -> Error {
    Error::new(
        Code::X003,
        Span::default(),
        format!("`{what}` needs bool, got {}", type_name(v)),
    )
}

fn type_err(what: &str, a: V, b: V) -> Error {
    Error::new(
        Code::X003,
        Span::default(),
        format!("`{what}` cannot take {} and {}", type_name(a), type_name(b)),
    )
}

/// `Num(x)` if finite, else `X010` naming what produced it.
pub(crate) fn fin(x: f64, what: &str) -> Result<V, Error> {
    if x.is_finite() {
        return Ok(V::Num(x));
    }
    let kind = if x.is_nan() { "NaN" } else { "infinity" };
    Err(Error::new(
        Code::X010,
        Span::default(),
        format!("`{what}` produced {kind}"),
    ))
}

fn cfin(z: Complex, what: &str) -> Result<V, Error> {
    if z.re.is_finite() && z.im.is_finite() {
        return Ok(V::Cnum(z.re, z.im));
    }
    Err(Error::new(
        Code::X010,
        Span::default(),
        format!("`{what}` produced a non-finite complex value"),
    ))
}

pub(crate) fn unary(op: OpKind, a: V) -> Result<V, Error> {
    match (op, a) {
        (OpKind::Neg, V::Int(x)) => Ok(x.checked_neg().map_or(V::Num(-(x as f64)), V::Int)),
        (OpKind::Neg, V::Num(x)) => Ok(V::Num(-x)),
        (OpKind::Neg, V::Cnum(re, im)) => Ok(V::Cnum(-re, -im)),
        (OpKind::Not, V::Bool(b)) => Ok(V::Bool(!b)),
        _ => Err(Error::new(
            Code::X003,
            Span::default(),
            format!("`{}` cannot take {}", op_str(op), type_name(a)),
        )),
    }
}

/// Binary operators (spec §4): int stays int for `+ - * %` unless it
/// overflows; `/` and `^` are num; cnum absorbs int/num; `str + str`
/// concatenates; comparisons of int/num compare as num.
pub(crate) fn binary(op: OpKind, a: V, b: V, strs: &mut Vec<u8>) -> Result<V, Error> {
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
            cnum_binary(op, complex(a), complex(b)).map_err(|_| type_err(op_str(op), a, b))?
        }
        (V::Str(..), V::Str(..)) => str_binary(op, a, b, strs),
        (V::Bool(x), V::Bool(y)) if eq_op => Ok(V::Bool((x == y) == (op == OpKind::Eq))),
        _ => Err(type_err(op_str(op), a, b)),
    }
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
        OpKind::Pow => dmath::pow(x, y),
        _ => {
            let ord = x.partial_cmp(&y).expect("values are never NaN");
            return Ok(V::Bool(cmp(op, ord)));
        }
    };
    fin(r, op_str(op))
}

/// Complex arithmetic; `Err(())` for operators complex numbers lack.
fn cnum_binary(op: OpKind, a: Complex, b: Complex) -> Result<Result<V, Error>, ()> {
    let r = match op {
        OpKind::Add => a + b,
        OpKind::Sub => a - b,
        OpKind::Mul => a * b,
        OpKind::Div => a / b,
        OpKind::Pow => dmath::cpow(a, b),
        _ => return Err(()),
    };
    Ok(cfin(r, op_str(op)))
}

fn str_binary(op: OpKind, a: V, b: V, strs: &mut Vec<u8>) -> Result<V, Error> {
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

/// Scalar builtins (spec §5). Calculator-only builtins reached with scalar
/// arguments are `X003` (they need a column).
pub(crate) fn call(b: Builtin, args: &[V], strs: &mut Vec<u8>) -> Result<V, Error> {
    assert!(!args.is_empty(), "every scalar builtin takes arguments");
    let name = b.name();
    let num = |v: V| match v {
        V::Int(x) => Ok(x as f64),
        V::Num(x) => Ok(x),
        _ => Err(Error::new(
            Code::X003,
            Span::default(),
            format!("`{name}` needs a number, got {}", type_name(v)),
        )),
    };
    let bad = |v: V| {
        Error::new(
            Code::X003,
            Span::default(),
            format!("`{name}` cannot take {}", type_name(v)),
        )
    };
    let a0 = args[0];
    let r = match b {
        Builtin::Abs => match a0 {
            V::Int(x) => Ok(x.checked_abs().map_or(V::Num((x as f64).abs()), V::Int)),
            V::Cnum(re, im) => fin(dmath::hypot(re, im), name),
            v => Ok(V::Num(num(v)?.abs())),
        },
        Builtin::Sqrt
        | Builtin::Exp
        | Builtin::Ln
        | Builtin::Log10
        | Builtin::Sin
        | Builtin::Cos
        | Builtin::Tan
        | Builtin::Asin
        | Builtin::Acos
        | Builtin::Atan
        | Builtin::Sinh
        | Builtin::Cosh
        | Builtin::Tanh => match a0 {
            V::Cnum(re, im) => cfin(dmath::complex(b, Complex::new(re, im)), name),
            v => fin(dmath::real(b, num(v)?), name),
        },
        Builtin::Atan2 => fin(dmath::atan2(num(a0)?, num(args[1])?), name),
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
                    Err(Error::new(
                        Code::X010,
                        Span::default(),
                        "`int` result outside the int range",
                    ))
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
                    return Err(Error::new(
                        Code::X003,
                        Span::default(),
                        "`eng` digits must be an int in 1..=17",
                    ));
                }
            };
            write_str(strs, |w| fmt_eng(w, x, digits))
        }
        Builtin::Str => match a0 {
            V::Str(..) => Ok(a0),
            V::Int(x) => write_str(strs, |w| write!(w, "{x}")),
            V::Num(x) => write_str(strs, |w| fmt_num(w, x)),
            V::Bool(v) => write_str(strs, |w| w.write_str(if v { "true" } else { "false" })),
            V::Cnum(..) => Err(Error::new(
                Code::X003,
                Span::default(),
                "`str` of cnum: use re/im/mag/ph",
            )),
        },
        Builtin::Num => match a0 {
            V::Str(s, len) => {
                let text = core::str::from_utf8(&strs[s as usize..(s + len) as usize])
                    .expect("scratch strings are UTF-8");
                Ok(V::Num(crate::text::parse_num(text)?))
            }
            v => Ok(V::Num(num(v)?)),
        },
        Builtin::Len => match a0 {
            // Unicode scalar values: count bytes that do not continue a sequence.
            V::Str(s, len) => Ok(V::Int(
                strs[s as usize..(s + len) as usize]
                    .iter()
                    .filter(|&&c| (c as i8) >= -64)
                    .count() as i64,
            )),
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
                Builtin::Ph => Ok(V::Num(dmath::atan2(z.im, z.re).to_degrees())),
                Builtin::Db => fin(20.0 * dmath::log10(dmath::norm(z)), name),
                _ if matches!(a0, V::Cnum(..)) => Ok(V::Cnum(z.re, -z.im)),
                _ => Ok(a0),
            }
        }
        Builtin::Cplx => Ok(V::Cnum(num(a0)?, num(args[1])?)),
        Builtin::SpectreSource => match a0 {
            V::Str(s, len) => {
                let spec = core::str::from_utf8(&strs[s as usize..(s + len) as usize])
                    .expect("scratch strings are UTF-8")
                    .to_owned();
                let mut out = String::with_capacity(spec.len() + 16);
                crate::spectre::source(&spec, &mut out)?;
                write_str(strs, |w| w.write_str(&out))
            }
            v => Err(bad(v)),
        },
        Builtin::If | Builtin::V | Builtin::I | Builtin::X | Builtin::Op => {
            unreachable!("`{name}` compiles to dedicated ops")
        }
        _ => {
            debug_assert!(b.calc_only());
            Err(Error::new(
                Code::X003,
                Span::default(),
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

fn write_str(
    strs: &mut Vec<u8>,
    f: impl FnOnce(&mut Bytes<'_>) -> core::fmt::Result,
) -> Result<V, Error> {
    let start = strs.len();
    f(&mut Bytes(strs)).expect("writing to a Vec cannot fail");
    let len = strs.len() - start;
    check_str_len(&strs[..start], len)?;
    Ok(V::Str(start as u32, len as u32))
}
