//! Scalar evaluation (spec §4): a [`Program`] and one [`Value`] per identifier
//! → one [`Value`]. The stack machine over the bytecode; the operator and
//! builtin kernels it calls live in `kernel`, which the calculator reuses per
//! element.
//!
//! Contents:
//! - `pub struct Scratch`, `pub trait PluginValue`, `pub(crate) enum Scalar`
//! - `Program::eval(&self, inputs, scratch) -> Result<Value, Error>`,
//!   `Program::eval_with(&self, inputs, plugins, scratch) -> Result<Value, Error>`
//! - `pub(crate) fn no_plugins(program: &Program) -> impl FnMut(..)`
//! - `pub(crate) fn run(program, code, inputs, plugin, scratch) -> Result<Scalar, Error>`
//! - `pub(crate) fn call_plugin(plugin, slot, args, strings) -> Result<R, Error>`
//! - `pub(crate) fn load_const(program, k, strings) -> Result<Scalar, Error>`,
//!   `pub(crate) fn load_value(v, strings) -> Result<Scalar, Error>`,
//!   `pub(crate) fn to_value(v, strings) -> Value`, `pub(crate) fn str_at(strings, start, len) -> &str`,
//!   `pub(crate) fn check_str_len(strings, len) -> Result<(), Error>`

use crate::builtin::Builtin;
use crate::calc::CalcScratch;
use crate::dmath::Complex;
use crate::kernel;
use crate::program::{CodeRange, Mode, OpKind, Program};
use crate::{Code, Error, MAX_PLUGIN_ARGS, MAX_STR_BYTES, Value};

/// Reusable evaluation memory: value stacks and the string buffer.
///
/// Keep one per thread and pass it to every evaluation; after the first few
/// evaluations nothing allocates in scalar mode (strings excepted). Results
/// that borrow from it ([`Value::Str`]) are invalidated by the next
/// evaluation.
#[derive(Debug, Default)]
pub struct Scratch {
    pub(crate) stack: Vec<Scalar>,
    /// Every string one evaluation creates, at most 64 MiB.
    pub(crate) strings: String,
    pub(crate) calc: CalcScratch,
}

/// What a plugin callback returns ([`Program::eval_with`], D20): a value the evaluator copies
/// in as soon as the callback returns, so it may own its text (a plugin's string answer).
/// [`Value`]`<'static>` is one.
pub trait PluginValue {
    /// The value, borrowed for the copy.
    fn value(&self) -> Value<'_>;
}

/// Internal scalar. `Str` is a byte range in [`Scratch::strings`] (or, inside
/// [`Program::consts`], in the program's pool).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Scalar {
    Int(i64),
    Num(f64),
    Bool(bool),
    Cnum(f64, f64),
    Str(u32, u32),
}

/// Total bytes of strings one evaluation may create before `X002`.
const MAX_SCRATCH_STR_BYTES: usize = 64 << 20;

// The value stack is the scalar loop's only memory traffic (data design 3).
const _: () = assert!(size_of::<Scalar>() == 24);
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
    pub fn eval<'scratch>(
        &self,
        inputs: &[Value<'_>],
        scratch: &'scratch mut Scratch,
    ) -> Result<Value<'scratch>, Error> {
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
    pub fn eval_with<'scratch, P, R>(
        &self,
        inputs: &[Value<'_>],
        plugins: &mut P,
        scratch: &'scratch mut Scratch,
    ) -> Result<Value<'scratch>, Error>
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
        let v = run(self, self.roots[0], inputs, plugins, scratch)?;
        Ok(to_value(v, &scratch.strings))
    }
}

impl PluginValue for Value<'static> {
    fn value(&self) -> Value<'_> {
        *self
    }
}

/// A plugin callback that reports every call as a missing plugin (`X006`).
pub(crate) fn no_plugins(
    program: &Program,
) -> impl FnMut(u32, &[Value<'_>]) -> Result<Value<'static>, Error> + '_ {
    move |slot, _| {
        Err(Error::bare(
            Code::X006,
            format!(
                "plugin function `{}` is not loaded",
                program.plugin_name(slot)
            ),
        ))
    }
}

/// Runs `program.ops[code]` to one value. Clears the scratch stack and strings.
///
/// # Panics
/// If the range is empty or outside the program, or the code leaves other
/// than one value (a compiler bug).
pub(crate) fn run<P, R>(
    program: &Program,
    code: CodeRange,
    inputs: &[Value<'_>],
    plugin: &mut P,
    s: &mut Scratch,
) -> Result<Scalar, Error>
where
    P: FnMut(u32, &[Value<'_>]) -> Result<R, Error>,
    R: PluginValue,
{
    let CodeRange { start, end } = code;
    assert!(start < end, "empty code range {start}..{end}");
    assert!(
        end as usize <= program.ops.len(),
        "code range past the program"
    );
    debug_assert_eq!(program.ops.len(), program.spans.len());
    s.stack.clear();
    s.strings.clear();
    let mut pc = start;
    while pc < end {
        let op = program.ops[pc as usize];
        let at = |e: Error| e.at(program.spans[pc as usize]);
        match op.kind {
            OpKind::Const => {
                let v = load_const(program, op.arg, &mut s.strings).map_err(at)?;
                s.stack.push(v);
            }
            OpKind::Input => {
                let v = match inputs[op.arg as usize] {
                    // Hot path: a finite num needs no conversion.
                    Value::Num(x) if x.is_finite() => Scalar::Num(x),
                    ref v => load_value(v, &mut s.strings).map_err(at)?,
                };
                s.stack.push(v);
            }
            OpKind::Add | OpKind::Sub | OpKind::Mul | OpKind::Div
                if matches!(
                    s.stack[s.stack.len() - 2..],
                    [Scalar::Num(_), Scalar::Num(_)]
                ) =>
            {
                // Hot path: num ⊕ num in place on the stack top.
                let Some(Scalar::Num(y)) = s.stack.pop() else {
                    unreachable!("guarded above")
                };
                let top = s.stack.last_mut().expect("guarded above");
                let Scalar::Num(x) = *top else {
                    unreachable!("guarded above")
                };
                let r = match op.kind {
                    OpKind::Add => x + y,
                    OpKind::Sub => x - y,
                    OpKind::Mul => x * y,
                    _ => x / y,
                };
                if !r.is_finite() {
                    return Err(at(
                        kernel::finite(r, kernel::op_str(op.kind)).expect_err("non-finite")
                    ));
                }
                *top = Scalar::Num(r);
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
                let v = kernel::binary(op.kind, a, b, &mut s.strings).map_err(at)?;
                s.stack.push(v);
            }
            OpKind::Cond => match pop(&mut s.stack) {
                Scalar::Bool(true) => {}
                Scalar::Bool(false) => {
                    pc = op.arg;
                    continue;
                }
                v => return Err(at(kernel::not_bool("condition", v))),
            },
            OpKind::Else => {
                debug_assert!(op.arg > pc, "jumps go forward");
                debug_assert!(op.arg < end, "an else jump lands on its EndCond");
                pc = op.arg;
                continue;
            }
            OpKind::EndCond => {}
            OpKind::And | OpKind::Or => {
                let short = op.kind == OpKind::Or;
                match pop(&mut s.stack) {
                    Scalar::Bool(b) if b == short => {
                        s.stack.push(Scalar::Bool(b));
                        pc = op.arg;
                        continue;
                    }
                    Scalar::Bool(_) => {}
                    v => return Err(at(kernel::not_bool(kernel::op_str(op.kind), v))),
                }
            }
            OpKind::EndAnd | OpKind::EndOr => {
                let top = *s.stack.last().expect("operand on stack");
                if !matches!(top, Scalar::Bool(_)) {
                    return Err(at(kernel::not_bool(kernel::op_str(op.kind), top)));
                }
            }
            OpKind::Call => {
                debug_assert!(
                    op.arg as usize <= s.stack.len(),
                    "call arguments on the stack"
                );
                let base = s.stack.len() - op.arg as usize;
                let b = Builtin::from_id(op.sub);
                let v = kernel::call(b, &s.stack[base..], &mut s.strings).map_err(at)?;
                s.stack.truncate(base);
                s.stack.push(v);
            }
            OpKind::Plugin => {
                debug_assert!(
                    usize::from(op.sub) <= s.stack.len(),
                    "plugin arguments on the stack"
                );
                let base = s.stack.len() - op.sub as usize;
                let r = call_plugin(plugin, op.arg, &s.stack[base..], &s.strings);
                let v = r
                    .and_then(|r| load_value(&r.value(), &mut s.strings))
                    .map_err(at)?;
                s.stack.truncate(base);
                s.stack.push(v);
            }
        }
        pc += 1;
    }
    assert_eq!(pc, end, "jumps land inside the root");
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
    args: &[Scalar],
    strings: &str,
) -> Result<R, Error>
where
    P: FnMut(u32, &[Value<'_>]) -> Result<R, Error>,
    R: PluginValue,
{
    assert!(
        args.len() <= MAX_PLUGIN_ARGS,
        "plugin arity checked at compile time"
    );
    let mut values = [Value::Bool(false); MAX_PLUGIN_ARGS];
    for (k, v) in args.iter().enumerate() {
        values[k] = to_value(*v, strings);
    }
    plugin(slot, &values[..args.len()])
}

/// Constant `k`, its string (if any) copied into `strings`.
pub(crate) fn load_const(program: &Program, k: u32, strings: &mut String) -> Result<Scalar, Error> {
    debug_assert!(
        (k as usize) < program.consts.len(),
        "constant {k} past the table"
    );
    match program.consts[k as usize] {
        Scalar::Str(s, len) => push_str(strings, str_at(&program.pool, s, len)),
        v => Ok(v),
    }
}

/// Converts a caller value (input or plugin result) into the evaluator's
/// representation, rejecting NaN/inf (`X010`).
pub(crate) fn load_value(v: &Value<'_>, strings: &mut String) -> Result<Scalar, Error> {
    match *v {
        Value::Int(x) => Ok(Scalar::Int(x)),
        Value::Num(x) => kernel::finite(x, "input"),
        Value::Bool(b) => Ok(Scalar::Bool(b)),
        Value::Cnum { re, im } => kernel::complex_finite(Complex::new(re, im), "input"),
        Value::Str(s) => push_str(strings, s),
    }
}

/// The caller's view of `v`, its string borrowed from `strings`.
pub(crate) fn to_value(v: Scalar, strings: &str) -> Value<'_> {
    match v {
        Scalar::Int(x) => Value::Int(x),
        Scalar::Num(x) => Value::Num(x),
        Scalar::Bool(b) => Value::Bool(b),
        Scalar::Cnum(re, im) => Value::Cnum { re, im },
        Scalar::Str(s, len) => Value::Str(str_at(strings, s, len)),
    }
}

/// The string at byte range `(start, len)` of a pool or scratch buffer (a `Scalar::Str`).
pub(crate) fn str_at(strings: &str, start: u32, len: u32) -> &str {
    let (start, len) = (start as usize, len as usize);
    debug_assert!(start + len <= strings.len(), "string range past its buffer");
    debug_assert!(len <= MAX_STR_BYTES, "strings are capped at 1 MiB");
    &strings[start..start + len]
}

/// `X002` when a new string of `len` bytes is over 1 MiB or would grow `strings`
/// past the per-evaluation budget.
pub(crate) fn check_str_len(strings: &str, len: usize) -> Result<(), Error> {
    if len > MAX_STR_BYTES {
        return Err(Error::bare(Code::X002, "string longer than 1 MiB"));
    }
    if strings.len() + len > MAX_SCRATCH_STR_BYTES {
        return Err(Error::bare(
            Code::X002,
            "expression creates more than 64 MiB of strings",
        ));
    }
    // The u32 string ranges of `Scalar::Str` stay in range.
    debug_assert!(strings.len() + len <= u32::MAX as usize);
    Ok(())
}

// ---- private ----

fn pop(stack: &mut Vec<Scalar>) -> Scalar {
    stack.pop().expect("compiled code keeps the stack balanced")
}

fn push_str(strings: &mut String, s: &str) -> Result<Scalar, Error> {
    check_str_len(strings, s.len())?;
    let start = strings.len();
    strings.push_str(s);
    debug_assert_eq!(strings.len(), start + s.len());
    Ok(Scalar::Str(start as u32, s.len() as u32))
}
