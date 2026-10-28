//! Calculator mode (spec §1, §5): a [`Mode::Calc`] program and one result
//! set ([`Plot`]) → a scalar or a column. The same bytecode as scalar mode,
//! run over columns.
//!
//! A column lives on an axis: the plot's sweep axis (x values cut into sweep
//! steps), the per-step axis of a reduction (one point per step), or the
//! frequency axis of an `fft`. Element-wise operations need equal axes,
//! except that a per-step column broadcasts over the steps of its parent
//! (`y - avg(y)` removes each step's mean); anything else is `X011`. Scalars
//! broadcast everywhere. Element-wise work is `column`'s, step-aware
//! functions are `measure`'s.
//!
//! Contents:
//! - `pub struct Plot`, `pub struct Column`, `pub enum CalcResult`, `pub enum ColKind`,
//!   `pub struct ColOut`, `pub struct AxisOut`
//! - `pub(crate) struct CalcScratch`, `pub(crate) struct ColumnValue`, `pub(crate) enum CalcValue`,
//!   `pub(crate) enum AxisKind`, `pub(crate) struct Axis`
//! - `Program::eval_calc(&self, inputs, plot, plugins, scratch) -> Result<CalcResult, Error>`,
//!   `Program::eval_calc_plain(&self, inputs, plot, scratch) -> Result<CalcResult, Error>`
//! - `pub(crate) fn x011(msg: String) -> Error`

use core::ops::Range;

use crate::builtin::Builtin;
use crate::column;
use crate::eval::{self, PluginValue, Scalar, Scratch};
use crate::kernel;
use crate::measure;
use crate::program::{CodeRange, Mode, OpKind, Program, SignalKind};
use crate::{Code, Error, Value};

/// One result set as seen by a calculator expression. Borrowed: the caller
/// (`wave`/`run`) owns the columns.
#[derive(Clone, Copy, Debug)]
pub struct Plot<'results> {
    /// Sweep axis values (time, frequency, swept source), one per point.
    pub x: &'results [f64],
    /// Sweep steps as contiguous ranges partitioning `0..x.len()`; empty
    /// means one step over all points.
    pub steps: &'results [Range<u32>],
    /// One column per [`Program::signals`] entry, in that order; an `op`
    /// signal is one finite real number.
    pub signals: &'results [Column<'results>],
}

/// A borrowed signal column.
#[derive(Clone, Copy, Debug)]
pub struct Column<'results> {
    /// Real parts, one per point.
    pub re: &'results [f64],
    /// Imaginary parts; empty for a real signal.
    pub im: &'results [f64],
}

/// Result of a calculator evaluation.
#[derive(Clone, Debug, PartialEq)]
pub enum CalcResult<'scratch> {
    /// A constant or a value computed only from scalars; a string borrows
    /// the [`Scratch`] until its next evaluation.
    Scalar(Value<'scratch>),
    /// A column (owned; keep it as a trace).
    Column(ColOut),
}

/// What the elements of a column are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColKind {
    /// `col`.
    Real,
    /// `ccol`.
    Complex,
    /// Result of a comparison; elements are 0.0 or 1.0.
    Bool,
}

/// An evaluated column.
#[derive(Clone, Debug, PartialEq)]
pub struct ColOut {
    /// Element kind.
    pub kind: ColKind,
    /// Real parts (0/1 for [`ColKind::Bool`]).
    pub re: Vec<f64>,
    /// Imaginary parts; empty unless [`ColKind::Complex`].
    pub im: Vec<f64>,
    /// `None` when the column is on the plot's own axis; otherwise the axis
    /// it is on: per-step results (x = step index, one step) or `fft` bins.
    pub axis: Option<AxisOut>,
}

/// A derived axis.
#[derive(Clone, Debug, PartialEq)]
pub struct AxisOut {
    /// Axis values.
    pub x: Vec<f64>,
    /// Steps partitioning the axis.
    pub steps: Vec<Range<u32>>,
}

/// The calculator's part of [`Scratch`]: reused stacks and the axis table.
#[derive(Debug, Default)]
pub(crate) struct CalcScratch {
    stack: Vec<CalcValue>,
    /// One per open `?:`/`&&`/`||`: the column condition when both arms run.
    frames: Vec<Option<ColumnValue>>,
    /// `axes[0]` is the plot axis; derived axes are appended on first use.
    axes: Vec<Axis>,
}

/// An intermediate column; `im` is empty unless the kind is complex.
#[derive(Clone, Debug)]
pub(crate) struct ColumnValue {
    pub kind: ColKind,
    pub re: Vec<f64>,
    pub im: Vec<f64>,
    /// Index into the axis table.
    pub axis: u32,
}

/// A calculator stack value: a scalar or a column.
#[derive(Clone, Debug)]
pub(crate) enum CalcValue {
    Scalar(Scalar),
    Column(ColumnValue),
}

/// What an axis is: the plot's, one point per step of `parent`, or `fft` bins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AxisKind {
    Plot,
    PerStep,
    Fft,
}

/// One axis: x values cut into steps; `parent` is the axis it derives from.
#[derive(Debug)]
pub(crate) struct Axis {
    pub x: Vec<f64>,
    pub steps: Vec<Range<u32>>,
    pub parent: u32,
    pub kind: AxisKind,
}

impl Program {
    /// Evaluates a [`Mode::Calc`] program over one result set. Plugin calls
    /// fail with `X006` unless `plugins` supplies them (see
    /// [`Program::eval_with`]); with column arguments a plugin is called once
    /// per element.
    ///
    /// Cost: O(points) per operation, allocating each intermediate column.
    ///
    /// # Errors
    /// `X011` when a signal's length differs from `plot.x` or the steps do not
    /// partition it, or when columns on different axes meet; `X003` for a
    /// scalar where a column is needed (or a complex/bool column where a real
    /// one is); `X010` for any non-finite element and for measurements with
    /// no answer (no crossing, `at` outside the axis, no unity-gain point).
    ///
    /// # Panics
    /// If the program is not [`Mode::Calc`], `inputs.len() != idents().len()`
    /// or `plot.signals.len() != signals().len()`.
    pub fn eval_calc<'scratch, P, R>(
        &self,
        inputs: &[Value<'_>],
        plot: &Plot<'_>,
        plugins: &mut P,
        scratch: &'scratch mut Scratch,
    ) -> Result<CalcResult<'scratch>, Error>
    where
        P: FnMut(u32, &[Value<'_>]) -> Result<R, Error>,
        R: PluginValue,
    {
        assert_eq!(
            self.mode,
            Mode::Calc,
            "eval_calc of a scalar-mode program; use eval"
        );
        assert_eq!(self.roots.len(), 1, "eval_calc of a template program");
        assert_eq!(inputs.len(), self.idents.len(), "one input per identifier");
        assert_eq!(
            plot.signals.len(),
            self.signals.len(),
            "one column per signal"
        );
        let steps = check_plot(self, plot)?;
        let cs = &mut scratch.calc;
        cs.axes.clear();
        cs.axes.push(Axis {
            x: plot.x.to_vec(),
            steps,
            parent: 0,
            kind: AxisKind::Plot,
        });
        scratch.strings.clear();
        let v = run(
            self,
            self.roots[0],
            inputs,
            plot,
            plugins,
            &mut scratch.strings,
            cs,
        )?;
        debug_assert_eq!(cs.axes[0].kind, AxisKind::Plot, "axis 0 stays the plot's");
        Ok(match v {
            CalcValue::Scalar(v) => CalcResult::Scalar(eval::to_value(v, &scratch.strings)),
            CalcValue::Column(c) => {
                let axis = (c.axis != 0).then(|| {
                    let a = &cs.axes[c.axis as usize];
                    AxisOut {
                        x: a.x.clone(),
                        steps: a.steps.clone(),
                    }
                });
                debug_assert_eq!(
                    c.re.len(),
                    cs.axes[c.axis as usize].x.len(),
                    "a result column covers its axis"
                );
                CalcResult::Column(ColOut {
                    kind: c.kind,
                    re: c.re,
                    im: c.im,
                    axis,
                })
            }
        })
    }

    /// [`Program::eval_calc`] without plugins.
    ///
    /// # Errors
    /// As [`Program::eval_calc`].
    ///
    /// # Panics
    /// As [`Program::eval_calc`].
    pub fn eval_calc_plain<'scratch>(
        &self,
        inputs: &[Value<'_>],
        plot: &Plot<'_>,
        scratch: &'scratch mut Scratch,
    ) -> Result<CalcResult<'scratch>, Error> {
        self.eval_calc(inputs, plot, &mut eval::no_plugins(self), scratch)
    }
}

/// `X011` with `msg`, not yet spanned.
pub(crate) fn x011(msg: String) -> Error {
    debug_assert!(!msg.is_empty(), "an X011 names the mismatch");
    Error::bare(Code::X011, msg)
}

// ---- private ----

/// Validates column lengths and the step partition; returns the steps.
fn check_plot(program: &Program, plot: &Plot<'_>) -> Result<Vec<Range<u32>>, Error> {
    let n = plot.x.len();
    assert!(n <= u32::MAX as usize, "plot longer than u32 points");
    debug_assert_eq!(
        plot.signals.len(),
        program.signals.len(),
        "checked by the caller"
    );
    let bound = plot.signals.iter().zip(program.signals());
    for (k, (sig, (kind, name))) in bound.enumerate() {
        if kind == SignalKind::Op {
            if sig.re.len() != 1 || !sig.im.is_empty() || !sig.re[0].is_finite() {
                return Err(x011(format!(
                    "signal {k} `op({name})` must be one finite real number"
                )));
            }
            continue;
        }
        let im_ok = sig.im.is_empty() || sig.im.len() == n;
        if sig.re.len() != n || !im_ok {
            let f = if kind == SignalKind::V { "v" } else { "i" };
            return Err(x011(format!(
                "signal {k} `{f}({name})` has {} points, the axis has {n}",
                sig.re.len()
            )));
        }
    }
    if plot.steps.is_empty() {
        return Ok(vec![Range {
            start: 0,
            end: n as u32,
        }]);
    }
    let mut at = 0;
    for s in plot.steps {
        if s.start != at || s.end < s.start {
            return Err(x011(format!(
                "sweep steps do not partition the axis at point {at}"
            )));
        }
        at = s.end;
    }
    if at as usize != n {
        return Err(x011(format!("sweep steps cover {at} of {n} points")));
    }
    debug_assert!(!plot.steps.is_empty(), "the empty case returned above");
    Ok(plot.steps.to_vec())
}

// ponytail: every column operation allocates its result (and signals are
// copied in); fine for one trace per result set. Reuse operand buffers if
// calculator evaluation ever shows up in a profile.
fn run<P, R>(
    program: &Program,
    code: CodeRange,
    inputs: &[Value<'_>],
    plot: &Plot<'_>,
    plugin: &mut P,
    strings: &mut String,
    cs: &mut CalcScratch,
) -> Result<CalcValue, Error>
where
    P: FnMut(u32, &[Value<'_>]) -> Result<R, Error>,
    R: PluginValue,
{
    let CodeRange { start, end } = code;
    assert!(start < end, "empty code range");
    assert!(
        end as usize <= program.ops.len(),
        "code range past the program"
    );
    assert!(!cs.axes.is_empty(), "the plot axis is set up first");
    cs.stack.clear();
    cs.frames.clear();
    let mut pc = start;
    while pc < end {
        let op = program.ops[pc as usize];
        let at = |e: Error| e.at(program.spans[pc as usize]);
        let stack = &mut cs.stack;
        match op.kind {
            OpKind::Const => {
                let v = eval::load_const(program, op.arg, strings).map_err(at)?;
                stack.push(CalcValue::Scalar(v));
            }
            OpKind::Input => {
                let v = eval::load_value(&inputs[op.arg as usize], strings).map_err(at)?;
                stack.push(CalcValue::Scalar(v));
            }
            OpKind::Signal if program.signals[op.arg as usize].kind == SignalKind::Op => {
                // One number from the operating point, never a column (check_plot).
                stack.push(CalcValue::Scalar(Scalar::Num(
                    plot.signals[op.arg as usize].re[0],
                )));
            }
            OpKind::Signal => {
                let sig = plot.signals[op.arg as usize];
                let kind = if sig.im.is_empty() {
                    ColKind::Real
                } else {
                    ColKind::Complex
                };
                stack.push(CalcValue::Column(ColumnValue {
                    kind,
                    re: sig.re.to_vec(),
                    im: sig.im.to_vec(),
                    axis: 0,
                }));
            }
            OpKind::Axis => stack.push(CalcValue::Column(ColumnValue {
                kind: ColKind::Real,
                re: cs.axes[0].x.clone(),
                im: Vec::new(),
                axis: 0,
            })),
            OpKind::Neg | OpKind::Not => {
                let a = pop(stack);
                let r = match a {
                    CalcValue::Scalar(v) => {
                        CalcValue::Scalar(kernel::unary(op.kind, v).map_err(at)?)
                    }
                    CalcValue::Column(c) if op.kind == OpKind::Neg && c.kind != ColKind::Bool => {
                        CalcValue::Column(ColumnValue {
                            re: c.re.iter().map(|x| -x).collect(),
                            im: c.im.iter().map(|x| -x).collect(),
                            ..c
                        })
                    }
                    a => CalcValue::Column(
                        column::elementwise(&[a], &cs.axes, strings, |v, _| {
                            kernel::unary(op.kind, v[0])
                        })
                        .map_err(at)?,
                    ),
                };
                cs.stack.push(r);
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
                let b = pop(stack);
                let a = pop(stack);
                let r = match (a, b) {
                    (CalcValue::Scalar(x), CalcValue::Scalar(y)) => {
                        CalcValue::Scalar(kernel::binary(op.kind, x, y, strings).map_err(at)?)
                    }
                    (a, b) => CalcValue::Column(
                        column::column_binary(op.kind, a, b, &cs.axes, strings).map_err(at)?,
                    ),
                };
                cs.stack.push(r);
            }
            OpKind::Cond => match pop(stack) {
                CalcValue::Scalar(Scalar::Bool(t)) => {
                    cs.frames.push(None);
                    if !t {
                        pc = op.arg;
                        continue;
                    }
                }
                CalcValue::Column(c) if c.kind == ColKind::Bool => cs.frames.push(Some(c)),
                v => {
                    return Err(at(kernel::not_bool(
                        "condition",
                        column::representative_scalar(&v),
                    )));
                }
            },
            OpKind::Else => {
                // A column condition runs both arms; a scalar one jumps.
                if !matches!(cs.frames.last(), Some(Some(_))) {
                    pc = op.arg;
                    continue;
                }
            }
            OpKind::EndCond => {
                if let Some(cond) = cs.frames.pop().expect("EndCond matches a Cond") {
                    let e = pop(stack);
                    let t = pop(stack);
                    let operands = [CalcValue::Column(cond), t, e];
                    let r = column::elementwise(&operands, &cs.axes, strings, |v, _| {
                        Ok(if v[0] == Scalar::Bool(true) {
                            v[1]
                        } else {
                            v[2]
                        })
                    });
                    cs.stack.push(CalcValue::Column(r.map_err(at)?));
                }
            }
            OpKind::And | OpKind::Or => {
                let short = op.kind == OpKind::Or;
                match pop(stack) {
                    CalcValue::Scalar(Scalar::Bool(b)) if b == short => {
                        stack.push(CalcValue::Scalar(Scalar::Bool(b)));
                        pc = op.arg;
                        continue;
                    }
                    CalcValue::Scalar(Scalar::Bool(_)) => cs.frames.push(None),
                    CalcValue::Column(c) if c.kind == ColKind::Bool => cs.frames.push(Some(c)),
                    v => {
                        let what = kernel::op_str(op.kind);
                        return Err(at(kernel::not_bool(
                            what,
                            column::representative_scalar(&v),
                        )));
                    }
                }
            }
            OpKind::EndAnd | OpKind::EndOr => {
                let is_and = op.kind == OpKind::EndAnd;
                match cs.frames.pop().expect("EndAnd/EndOr matches an And/Or") {
                    None => match stack.last().expect("operand on stack") {
                        CalcValue::Scalar(Scalar::Bool(_)) => {}
                        CalcValue::Column(c) if c.kind == ColKind::Bool => {}
                        v => {
                            let what = kernel::op_str(op.kind);
                            return Err(at(kernel::not_bool(
                                what,
                                column::representative_scalar(v),
                            )));
                        }
                    },
                    Some(lhs) => {
                        let rhs = pop(stack);
                        let operands = [CalcValue::Column(lhs), rhs];
                        let r = column::elementwise(&operands, &cs.axes, strings, |v, _| {
                            match (v[0], v[1]) {
                                (Scalar::Bool(a), Scalar::Bool(b)) => {
                                    Ok(Scalar::Bool(if is_and { a && b } else { a || b }))
                                }
                                (_, b) => Err(kernel::not_bool(kernel::op_str(op.kind), b)),
                            }
                        });
                        cs.stack.push(CalcValue::Column(r.map_err(at)?));
                    }
                }
            }
            OpKind::Call => {
                debug_assert!(
                    op.arg as usize <= stack.len(),
                    "call arguments on the stack"
                );
                let base = stack.len() - op.arg as usize;
                let args: Vec<CalcValue> = stack.drain(base..).collect();
                let builtin = Builtin::from_id(op.sub);
                let r = measure::call(builtin, args, &mut cs.axes, strings).map_err(at)?;
                cs.stack.push(r);
            }
            OpKind::Plugin => {
                debug_assert!(
                    usize::from(op.sub) <= stack.len(),
                    "plugin arguments on the stack"
                );
                let base = stack.len() - op.sub as usize;
                let args: Vec<CalcValue> = stack.drain(base..).collect();
                let slot = op.arg;
                let r = if let Some(vs) = column::scalars(&args) {
                    let r = eval::call_plugin(plugin, slot, &vs, strings);
                    let v = r.and_then(|r| eval::load_value(&r.value(), strings));
                    CalcValue::Scalar(v.map_err(at)?)
                } else {
                    let r = column::elementwise(&args, &cs.axes, strings, |v, strings| {
                        let r = eval::call_plugin(plugin, slot, v, strings)?;
                        eval::load_value(&r.value(), strings)
                    });
                    CalcValue::Column(r.map_err(at)?)
                };
                cs.stack.push(r);
            }
        }
        pc += 1;
    }
    assert_eq!(pc, end, "jumps land inside the root");
    assert_eq!(cs.stack.len(), 1, "program must leave exactly one value");
    assert!(cs.frames.is_empty(), "condition frames unbalanced");
    let Some(result) = cs.stack.pop() else {
        unreachable!("one value checked above")
    };
    Ok(result)
}

fn pop(stack: &mut Vec<CalcValue>) -> CalcValue {
    stack.pop().expect("compiled code keeps the stack balanced")
}
