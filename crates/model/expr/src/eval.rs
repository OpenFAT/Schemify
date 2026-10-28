//! Scalar evaluation (spec §4): a [`Program`] and one [`Value`] per identifier
//! → one [`Value`]. The stack machine over the bytecode; the operator and
//! builtin kernels it calls live in `kernel`, which the calculator reuses per
//! element.
//!
//! Contents:
//! - `pub struct Scratch`, `pub trait PluginValue`, `pub(crate) enum V`
//! - `Program::eval(&self, inputs, scratch) -> Result<Value, Error>`,
//!   `Program::eval_with(&self, inputs, plugins, scratch) -> Result<Value, Error>`
//! - `pub(crate) fn no_plugins(prog: &Program) -> impl FnMut(..)`
//! - `pub(crate) fn run(prog, start, end, inputs, plugin, s) -> Result<V, Error>`
//! - `pub(crate) fn call_plugin(plugin, slot, args, strs) -> Result<R, Error>`
//! - `pub(crate) fn load_const(prog, k, strs) -> Result<V, Error>`,
//!   `pub(crate) fn load_value(v, strs) -> Result<V, Error>`,
//!   `pub(crate) fn to_value(v, strs) -> Value`, `pub(crate) fn str_at(strs, start, len) -> &str`,
//!   `pub(crate) fn check_str_len(strs, len) -> Result<(), Error>`

use crate::builtin::Builtin;
use crate::calc::CalcScratch;
use crate::dmath::Complex;
use crate::kernel::{self, cfin, fin, not_bool, op_str};
use crate::program::{Mode, OpKind, Program};
use crate::{Code, Error, MAX_PLUGIN_ARGS, MAX_STR_BYTES, Value};

/// Reusable evaluation memory: value stacks and the string buffer.
///
/// Keep one per thread and pass it to every evaluation; after the first few
/// evaluations nothing allocates in scalar mode (strings excepted). Results
/// that borrow from it ([`Value::Str`]) are invalidated by the next
/// evaluation.
#[derive(Debug, Default)]
pub struct Scratch {
    pub(crate) stack: Vec<V>,
    /// Every string one evaluation creates, at most 64 MiB.
    pub(crate) strs: String,
    pub(crate) calc: CalcScratch,
}

/// What a plugin callback returns ([`Program::eval_with`], D20): a value the evaluator copies
/// in as soon as the callback returns, so it may own its text (a plugin's string answer).
/// [`Value`]`<'static>` is one.
pub trait PluginValue {
    /// The value, borrowed for the copy.
    fn value(&self) -> Value<'_>;
}

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

/// Total bytes of strings one evaluation may create before `X002`.
const MAX_SCRATCH_STR_BYTES: usize = 64 << 20;

// The value stack is the scalar loop's only memory traffic (data design 3).
const _: () = assert!(size_of::<V>() == 24);
const _: () = assert!(MAX_SCRATCH_STR_BYTES <= u32::MAX as usize);

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
    /// evaluating). Results must be finite; a result is copied in at once, so
    /// it may own its text ([`PluginValue`]).
    ///
    /// # Errors
    /// As [`Program::eval`], plus whatever `plugins` returns.
    ///
    /// # Panics
    /// As [`Program::eval`].
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
        Err(Error::bare(
            Code::X006,
            format!("plugin function `{}` is not loaded", prog.plugin_name(slot)),
        ))
    }
}

/// Runs `prog.ops[start..end]` to one value. Clears the scratch stack and strings.
///
/// # Panics
/// If the range is empty or outside the program, or the code leaves other
/// than one value (a compiler bug).
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
    assert!(start < end, "empty code range {start}..{end}");
    assert!(end as usize <= prog.ops.len(), "code range past the program");
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
                return Err(at(Error::bare(
                    Code::X006,
                    "signals need calculator evaluation",
                )));
            }
            OpKind::Neg | OpKind::Not => {
                let a = pop(&mut s.stack);
                s.stack.push(kernel::unary(op.kind, a).map_err(at)?);
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
                let v = kernel::binary(op.kind, a, b, &mut s.strs).map_err(at)?;
                s.stack.push(v);
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
                let b = Builtin::from_id(op.sub);
                let v = kernel::call(b, &s.stack[base..], &mut s.strs).map_err(at)?;
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

/// Calls plugin slot `slot` with `args` as [`Value`]s.
///
/// # Panics
/// If `args` holds more than 16 values (the compiler's `X008` bound).
pub(crate) fn call_plugin<P, R>(
    plugin: &mut P,
    slot: u32,
    args: &[V],
    strs: &str,
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

/// Constant `k`, its string (if any) copied into `strs`.
pub(crate) fn load_const(prog: &Program, k: u32, strs: &mut String) -> Result<V, Error> {
    match prog.consts[k as usize] {
        V::Str(s, len) => push_str(strs, str_at(&prog.pool, s, len)),
        v => Ok(v),
    }
}

/// Converts a caller value (input or plugin result) into the evaluator's
/// representation, rejecting NaN/inf (`X010`).
pub(crate) fn load_value(v: &Value<'_>, strs: &mut String) -> Result<V, Error> {
    match *v {
        Value::Int(x) => Ok(V::Int(x)),
        Value::Num(x) => fin(x, "input"),
        Value::Bool(b) => Ok(V::Bool(b)),
        Value::Cnum { re, im } => cfin(Complex::new(re, im), "input"),
        Value::Str(s) => push_str(strs, s),
    }
}

/// The caller's view of `v`, its string borrowed from `strs`.
pub(crate) fn to_value(v: V, strs: &str) -> Value<'_> {
    match v {
        V::Int(x) => Value::Int(x),
        V::Num(x) => Value::Num(x),
        V::Bool(b) => Value::Bool(b),
        V::Cnum(re, im) => Value::Cnum { re, im },
        V::Str(s, len) => Value::Str(str_at(strs, s, len)),
    }
}

/// The string at byte range `(start, len)` of a pool or scratch buffer (a `V::Str`).
pub(crate) fn str_at(strs: &str, start: u32, len: u32) -> &str {
    let (start, len) = (start as usize, len as usize);
    debug_assert!(start + len <= strs.len());
    &strs[start..start + len]
}

/// `X002` when a new string of `len` bytes is over 1 MiB or would grow `strs`
/// past the per-evaluation budget.
pub(crate) fn check_str_len(strs: &str, len: usize) -> Result<(), Error> {
    if len > MAX_STR_BYTES {
        return Err(Error::bare(Code::X002, "string longer than 1 MiB"));
    }
    if strs.len() + len > MAX_SCRATCH_STR_BYTES {
        return Err(Error::bare(
            Code::X002,
            "expression creates more than 64 MiB of strings",
        ));
    }
    Ok(())
}

// ---- private ----

fn pop(stack: &mut Vec<V>) -> V {
    stack.pop().expect("compiled code keeps the stack balanced")
}

fn push_str(strs: &mut String, s: &str) -> Result<V, Error> {
    check_str_len(strs, s.len())?;
    let start = strs.len();
    strs.push_str(s);
    Ok(V::Str(start as u32, s.len() as u32))
}
