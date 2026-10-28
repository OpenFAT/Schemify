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
//! - `pub(crate) struct CalcScratch`, `pub(crate) struct Col`, `pub(crate) enum CV`,
//!   `pub(crate) enum AxisKind`, `pub(crate) struct Axis`
//! - `Program::eval_calc(&self, inputs, plot, plugins, scratch) -> Result<CalcResult, Error>`,
//!   `Program::eval_calc_plain(&self, inputs, plot, scratch) -> Result<CalcResult, Error>`
//! - `pub(crate) fn x011(msg: String) -> Error`

use core::ops::Range;

use crate::builtin::Builtin;
use crate::column::{col_binary, cv_type, elementwise, scalars};
use crate::eval::{
    PluginValue, Scratch, V, call_plugin, load_const, load_value, no_plugins, to_value,
};
use crate::kernel::{binary, not_bool, op_str, unary};
use crate::measure::call_cv;
use crate::program::{Mode, OpKind, Program, SignalKind};
use crate::{Code, Error, Value};

/// One result set as seen by a calculator expression. Borrowed: the caller
/// (`wave`/`run`) owns the columns.
#[derive(Clone, Copy, Debug)]
pub struct Plot<'a> {
    /// Sweep axis values (time, frequency, swept source), one per point.
    pub x: &'a [f64],
    /// Sweep steps as contiguous ranges partitioning `0..x.len()`; empty
    /// means one step over all points.
    pub steps: &'a [Range<u32>],
    /// One column per [`Program::signals`] entry, in that order; an `op`
    /// signal is one finite real number.
    pub signals: &'a [Column<'a>],
}

/// A borrowed signal column.
#[derive(Clone, Copy, Debug)]
pub struct Column<'a> {
    /// Real parts, one per point.
    pub re: &'a [f64],
    /// Imaginary parts; empty for a real signal.
    pub im: &'a [f64],
}

/// Result of a calculator evaluation.
#[derive(Clone, Debug, PartialEq)]
pub enum CalcResult<'s> {
    /// A constant or a value computed only from scalars; a string borrows
    /// the [`Scratch`] until its next evaluation.
    Scalar(Value<'s>),
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
    stack: Vec<CV>,
    /// One per open `?:`/`&&`/`||`: the column condition when both arms run.
    frames: Vec<Option<Col>>,
    /// `axes[0]` is the plot axis; derived axes are appended on first use.
    axes: Vec<Axis>,
}

/// An intermediate column; `im` is empty unless the kind is complex.
#[derive(Clone, Debug)]
pub(crate) struct Col {
    pub kind: ColKind,
    pub re: Vec<f64>,
    pub im: Vec<f64>,
    /// Index into the axis table.
    pub axis: u32,
}

/// A calculator stack value: a scalar or a column.
#[derive(Clone, Debug)]
pub(crate) enum CV {
    S(V),
    C(Col),
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
    pub fn eval_calc<'s, P, R>(
        &self,
        inputs: &[Value<'_>],
        plot: &Plot<'_>,
        plugins: &mut P,
        scratch: &'s mut Scratch,
    ) -> Result<CalcResult<'s>, Error>
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
        scratch.strs.clear();
        let (start, end) = self.roots[0];
        let v = run(
            self,
            (start, end),
            inputs,
            plot,
            plugins,
            &mut scratch.strs,
            cs,
        )?;
        Ok(match v {
            CV::S(v) => CalcResult::Scalar(to_value(v, &scratch.strs)),
            CV::C(c) => {
                let axis = (c.axis != 0).then(|| {
                    let a = &cs.axes[c.axis as usize];
                    AxisOut {
                        x: a.x.clone(),
                        steps: a.steps.clone(),
                    }
                });
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
    pub fn eval_calc_plain<'s>(
        &self,
        inputs: &[Value<'_>],
        plot: &Plot<'_>,
        scratch: &'s mut Scratch,
    ) -> Result<CalcResult<'s>, Error> {
        self.eval_calc(inputs, plot, &mut no_plugins(self), scratch)
    }
}

/// `X011` with `msg`, not yet spanned.
pub(crate) fn x011(msg: String) -> Error {
    Error::bare(Code::X011, msg)
}

// ---- private ----

/// Validates column lengths and the step partition; returns the steps.
fn check_plot(prog: &Program, plot: &Plot<'_>) -> Result<Vec<Range<u32>>, Error> {
    let n = plot.x.len();
    assert!(n <= u32::MAX as usize, "plot longer than u32 points");
    for (k, (sig, (kind, name))) in plot.signals.iter().zip(prog.signals()).enumerate() {
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
    Ok(plot.steps.to_vec())
}

// ponytail: every column operation allocates its result (and signals are
// copied in); fine for one trace per result set. Reuse operand buffers if
// calculator evaluation ever shows up in a profile.
fn run<P, R>(
    prog: &Program,
    (start, end): (u32, u32),
    inputs: &[Value<'_>],
    plot: &Plot<'_>,
    plugin: &mut P,
    strs: &mut String,
    cs: &mut CalcScratch,
) -> Result<CV, Error>
where
    P: FnMut(u32, &[Value<'_>]) -> Result<R, Error>,
    R: PluginValue,
{
    assert!(start < end, "empty code range");
    assert!(
        end as usize <= prog.ops.len(),
        "code range past the program"
    );
    cs.stack.clear();
    cs.frames.clear();
    let mut pc = start;
    while pc < end {
        let op = prog.ops[pc as usize];
        let at = |e: Error| e.at(prog.spans[pc as usize]);
        let stack = &mut cs.stack;
        match op.kind {
            OpKind::Const => stack.push(CV::S(load_const(prog, op.arg, strs).map_err(at)?)),
            OpKind::Input => stack.push(CV::S(
                load_value(&inputs[op.arg as usize], strs).map_err(at)?,
            )),
            OpKind::Signal if prog.signals[op.arg as usize].0 == SignalKind::Op => {
                // One number from the operating point, never a column (check_plot).
                stack.push(CV::S(V::Num(plot.signals[op.arg as usize].re[0])));
            }
            OpKind::Signal => {
                let sig = plot.signals[op.arg as usize];
                let kind = if sig.im.is_empty() {
                    ColKind::Real
                } else {
                    ColKind::Complex
                };
                stack.push(CV::C(Col {
                    kind,
                    re: sig.re.to_vec(),
                    im: sig.im.to_vec(),
                    axis: 0,
                }));
            }
            OpKind::Axis => stack.push(CV::C(Col {
                kind: ColKind::Real,
                re: cs.axes[0].x.clone(),
                im: Vec::new(),
                axis: 0,
            })),
            OpKind::Neg | OpKind::Not => {
                let a = pop(stack);
                let r = match a {
                    CV::S(v) => CV::S(unary(op.kind, v).map_err(at)?),
                    CV::C(c) if op.kind == OpKind::Neg && c.kind != ColKind::Bool => CV::C(Col {
                        re: c.re.iter().map(|x| -x).collect(),
                        im: c.im.iter().map(|x| -x).collect(),
                        ..c
                    }),
                    a => CV::C(
                        elementwise(&[a], &cs.axes, strs, |v, _| unary(op.kind, v[0]))
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
                    (CV::S(x), CV::S(y)) => CV::S(binary(op.kind, x, y, strs).map_err(at)?),
                    (a, b) => CV::C(col_binary(op.kind, a, b, &cs.axes, strs).map_err(at)?),
                };
                cs.stack.push(r);
            }
            OpKind::Cond => match pop(stack) {
                CV::S(V::Bool(t)) => {
                    cs.frames.push(None);
                    if !t {
                        pc = op.arg;
                        continue;
                    }
                }
                CV::C(c) if c.kind == ColKind::Bool => cs.frames.push(Some(c)),
                v => return Err(at(not_bool("condition", cv_type(&v)))),
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
                    let r = elementwise(&[CV::C(cond), t, e], &cs.axes, strs, |v, _| {
                        Ok(if v[0] == V::Bool(true) { v[1] } else { v[2] })
                    });
                    cs.stack.push(CV::C(r.map_err(at)?));
                }
            }
            OpKind::And | OpKind::Or => {
                let short = op.kind == OpKind::Or;
                match pop(stack) {
                    CV::S(V::Bool(b)) if b == short => {
                        stack.push(CV::S(V::Bool(b)));
                        pc = op.arg;
                        continue;
                    }
                    CV::S(V::Bool(_)) => cs.frames.push(None),
                    CV::C(c) if c.kind == ColKind::Bool => cs.frames.push(Some(c)),
                    v => return Err(at(not_bool(op_str(op.kind), cv_type(&v)))),
                }
            }
            OpKind::EndAnd | OpKind::EndOr => {
                let is_and = op.kind == OpKind::EndAnd;
                match cs.frames.pop().expect("EndAnd/EndOr matches an And/Or") {
                    None => match stack.last().expect("operand on stack") {
                        CV::S(V::Bool(_)) => {}
                        CV::C(c) if c.kind == ColKind::Bool => {}
                        v => return Err(at(not_bool(op_str(op.kind), cv_type(v)))),
                    },
                    Some(lhs) => {
                        let rhs = pop(stack);
                        let r = elementwise(&[CV::C(lhs), rhs], &cs.axes, strs, |v, _| {
                            match (v[0], v[1]) {
                                (V::Bool(a), V::Bool(b)) => {
                                    Ok(V::Bool(if is_and { a && b } else { a || b }))
                                }
                                (_, b) => Err(not_bool(op_str(op.kind), b)),
                            }
                        });
                        cs.stack.push(CV::C(r.map_err(at)?));
                    }
                }
            }
            OpKind::Call => {
                let base = stack.len() - op.arg as usize;
                let args: Vec<CV> = stack.drain(base..).collect();
                let r = call_cv(Builtin::from_id(op.sub), args, &mut cs.axes, strs).map_err(at)?;
                cs.stack.push(r);
            }
            OpKind::Plugin => {
                let base = stack.len() - op.sub as usize;
                let args: Vec<CV> = stack.drain(base..).collect();
                let slot = op.arg;
                let r = if let Some(vs) = scalars(&args) {
                    let r = call_plugin(plugin, slot, &vs, strs);
                    CV::S(r.and_then(|r| load_value(&r.value(), strs)).map_err(at)?)
                } else {
                    let r = elementwise(&args, &cs.axes, strs, |v, strs| {
                        let r = call_plugin(plugin, slot, v, strs)?;
                        load_value(&r.value(), strs)
                    });
                    CV::C(r.map_err(at)?)
                };
                cs.stack.push(r);
            }
        }
        pc += 1;
    }
    assert_eq!(cs.stack.len(), 1, "program must leave exactly one value");
    assert!(cs.frames.is_empty(), "condition frames unbalanced");
    Ok(cs.stack.pop().expect("one value"))
}

fn pop(stack: &mut Vec<CV>) -> CV {
    stack.pop().expect("compiled code keeps the stack balanced")
}
