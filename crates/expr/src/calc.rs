//! Calculator mode (spec §1, §5): the same bytecode over columns.
//!
//! A column lives on an axis: the plot's sweep axis (x values cut into sweep
//! steps), the per-step axis of a reduction (one point per step), or the
//! frequency axis of an `fft`. Element-wise operations need equal axes,
//! except that a per-step column broadcasts over the steps of its parent
//! (`y - avg(y)` removes each step's mean); anything else is `X011`. Scalars
//! broadcast everywhere. Step-aware functions never cross step boundaries.
//!
//! Element-wise operators and builtins reuse the scalar kernels per element,
//! so both modes agree by construction; `+ - * /` on real columns take a
//! straight-loop fast path.

use core::ops::Range;

use rustfft::FftPlanner;
use rustfft::num_complex::Complex64;

use crate::compile::{Builtin, Mode, OpKind, Program};
use crate::eval::{
    Scratch, V, binary, call, call_plugin, fin, load_const, load_value, no_plugins, not_bool,
    op_str, to_value, type_name, unary,
};
use crate::{Code, Error, Span, Value};

/// One result set as seen by a calculator expression. Borrowed: the caller
/// (`wave`/`run`) owns the columns.
#[derive(Clone, Copy, Debug)]
pub struct Plot<'a> {
    /// Sweep axis values (time, frequency, swept source), one per point.
    pub x: &'a [f64],
    /// Sweep steps as contiguous ranges partitioning `0..x.len()`; empty
    /// means one step over all points.
    pub steps: &'a [Range<u32>],
    /// One column per [`Program::signals`] entry, in that order.
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
    /// A constant or a value computed only from scalars.
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

#[derive(Debug, Default)]
pub(crate) struct CalcScratch {
    stack: Vec<CV>,
    /// One per open `?:`/`&&`/`||`: the column condition when both arms run.
    frames: Vec<Option<Col>>,
    /// `axes[0]` is the plot axis.
    axes: Vec<Axis>,
}

#[derive(Clone, Debug)]
struct Col {
    kind: ColKind,
    re: Vec<f64>,
    im: Vec<f64>,
    axis: u32,
}

#[derive(Clone, Debug)]
enum CV {
    S(V),
    C(Col),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AxisKind {
    Plot,
    PerStep,
    Fft,
}

#[derive(Debug)]
struct Axis {
    x: Vec<f64>,
    steps: Vec<Range<u32>>,
    parent: u32,
    kind: AxisKind,
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
    pub fn eval_calc<'s, P>(
        &self,
        inputs: &[Value<'_>],
        plot: &Plot<'_>,
        plugins: &mut P,
        scratch: &'s mut Scratch,
    ) -> Result<CalcResult<'s>, Error>
    where
        P: FnMut(u32, &[Value<'_>]) -> Result<Value<'static>, Error>,
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
            start,
            end,
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
    pub fn eval_calc_plain<'s>(
        &self,
        inputs: &[Value<'_>],
        plot: &Plot<'_>,
        scratch: &'s mut Scratch,
    ) -> Result<CalcResult<'s>, Error> {
        self.eval_calc(inputs, plot, &mut no_plugins(self), scratch)
    }
}

fn x011(msg: String) -> Error {
    Error::new(Code::X011, Span::default(), msg)
}

/// Validates column lengths and the step partition; returns the steps.
fn check_plot(prog: &Program, plot: &Plot<'_>) -> Result<Vec<Range<u32>>, Error> {
    let n = plot.x.len();
    assert!(n <= u32::MAX as usize, "plot longer than u32 points");
    for (k, (sig, (kind, name))) in plot.signals.iter().zip(prog.signals()).enumerate() {
        let im_ok = sig.im.is_empty() || sig.im.len() == n;
        if sig.re.len() != n || !im_ok {
            let f = if kind == crate::SignalKind::V {
                "v"
            } else {
                "i"
            };
            return Err(x011(format!(
                "signal {k} `{f}({name})` has {} points, the axis has {n}",
                sig.re.len()
            )));
        }
    }
    if plot.steps.is_empty() {
        return Ok(core::iter::once(0..n as u32).collect());
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
#[allow(clippy::too_many_arguments)]
fn run<P>(
    prog: &Program,
    start: u32,
    end: u32,
    inputs: &[Value<'_>],
    plot: &Plot<'_>,
    plugin: &mut P,
    strs: &mut Vec<u8>,
    cs: &mut CalcScratch,
) -> Result<CV, Error>
where
    P: FnMut(u32, &[Value<'_>]) -> Result<Value<'static>, Error>,
{
    assert!(
        start < end && end as usize <= prog.ops.len(),
        "code range invalid"
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
                let r = if args.iter().all(|a| matches!(a, CV::S(_))) {
                    let vs: Vec<V> = args
                        .iter()
                        .map(|a| if let CV::S(v) = a { *v } else { unreachable!() })
                        .collect();
                    let r = call_plugin(plugin, slot, &vs, strs);
                    CV::S(r.and_then(|r| load_value(&r, strs)).map_err(at)?)
                } else {
                    let r = elementwise(&args, &cs.axes, strs, |v, strs| {
                        let r = call_plugin(plugin, slot, v, strs)?;
                        load_value(&r, strs)
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

/// A representative scalar for type-error messages.
fn cv_type(v: &CV) -> V {
    match v {
        CV::S(v) => *v,
        CV::C(c) => elem(c, 0),
    }
}

fn elem(c: &Col, i: usize) -> V {
    match c.kind {
        ColKind::Real => V::Num(c.re.get(i).copied().unwrap_or(0.0)),
        ColKind::Complex => V::Cnum(
            c.re.get(i).copied().unwrap_or(0.0),
            c.im.get(i).copied().unwrap_or(0.0),
        ),
        ColKind::Bool => V::Bool(c.re.get(i).is_some_and(|x| *x != 0.0)),
    }
}

/// The axis every column argument lives on (or broadcasts to).
fn common_axis(args: &[CV], axes: &[Axis]) -> Result<u32, Error> {
    let child_of =
        |c: u32, p: u32| axes[c as usize].kind == AxisKind::PerStep && axes[c as usize].parent == p;
    let mut target = None;
    for a in args {
        let CV::C(c) = a else { continue };
        target = Some(match target {
            None => c.axis,
            Some(t) if t == c.axis || child_of(c.axis, t) => t,
            Some(t) if child_of(t, c.axis) => c.axis,
            Some(_) => return Err(x011("columns are on different sweep axes".to_string())),
        });
    }
    let target = target.expect("elementwise needs a column argument");
    for a in args {
        if let CV::C(c) = a
            && c.axis != target
            && !child_of(c.axis, target)
        {
            return Err(x011("columns are on different sweep axes".to_string()));
        }
    }
    Ok(target)
}

/// Applies a scalar kernel per element; per-step columns repeat over their
/// parent's steps, scalars over everything.
fn elementwise(
    args: &[CV],
    axes: &[Axis],
    strs: &mut Vec<u8>,
    mut f: impl FnMut(&[V], &mut Vec<u8>) -> Result<V, Error>,
) -> Result<Col, Error> {
    let axis = common_axis(args, axes)?;
    let ax = &axes[axis as usize];
    let n = ax.x.len();
    let mut vs: Vec<V> = args.iter().map(|_| V::Bool(false)).collect();
    let mut out = ColBuilder {
        kind: None,
        re: Vec::with_capacity(n),
        im: Vec::new(),
    };
    for (si, s) in ax.steps.iter().enumerate() {
        for i in s.start as usize..s.end as usize {
            for (k, a) in args.iter().enumerate() {
                vs[k] = match a {
                    CV::S(v) => *v,
                    CV::C(c) => elem(c, if c.axis == axis { i } else { si }),
                };
            }
            let v = f(&vs, strs).map_err(|e| Error {
                msg: format!("{} (at point {i})", e.msg),
                ..e
            })?;
            out.push(v)?;
        }
    }
    assert_eq!(out.re.len(), n, "elementwise output covers the axis");
    Ok(out.finish(axis))
}

struct ColBuilder {
    kind: Option<ColKind>,
    re: Vec<f64>,
    im: Vec<f64>,
}

impl ColBuilder {
    fn push(&mut self, v: V) -> Result<(), Error> {
        let (kind, re, im) = match v {
            V::Int(x) => (ColKind::Real, x as f64, 0.0),
            V::Num(x) => (ColKind::Real, x, 0.0),
            V::Cnum(re, im) => (ColKind::Complex, re, im),
            V::Bool(b) => (ColKind::Bool, if b { 1.0 } else { 0.0 }, 0.0),
            V::Str(..) => {
                return Err(Error::new(
                    Code::X003,
                    Span::default(),
                    "string columns do not exist",
                ));
            }
        };
        match (self.kind, kind) {
            (None, k) => self.kind = Some(k),
            (Some(a), b) if a == b => {}
            (Some(ColKind::Real), ColKind::Complex) => {
                self.kind = Some(ColKind::Complex);
                self.im.resize(self.re.len(), 0.0);
            }
            (Some(ColKind::Complex), ColKind::Real) => {}
            _ => {
                return Err(Error::new(
                    Code::X003,
                    Span::default(),
                    "column mixes bool and numeric elements",
                ));
            }
        }
        self.re.push(re);
        if self.kind == Some(ColKind::Complex) {
            self.im.push(im);
        }
        Ok(())
    }

    fn finish(self, axis: u32) -> Col {
        let kind = self.kind.unwrap_or(ColKind::Real);
        debug_assert!(kind != ColKind::Complex || self.im.len() == self.re.len());
        Col {
            kind,
            re: self.re,
            im: self.im,
            axis,
        }
    }
}

/// Column binary operator; real `+ - * /` on one axis skip the per-element kernel.
fn col_binary(op: OpKind, a: CV, b: CV, axes: &[Axis], strs: &mut Vec<u8>) -> Result<Col, Error> {
    enum R<'a> {
        S(f64),
        C(&'a [f64], u32),
    }
    fn real(v: &CV) -> Option<R<'_>> {
        match v {
            CV::S(V::Int(x)) => Some(R::S(*x as f64)),
            CV::S(V::Num(x)) => Some(R::S(*x)),
            CV::C(c) if c.kind == ColKind::Real => Some(R::C(&c.re, c.axis)),
            _ => None,
        }
    }
    if matches!(op, OpKind::Add | OpKind::Sub | OpKind::Mul | OpKind::Div) {
        let fast = match (real(&a), real(&b)) {
            (Some(R::C(x, ax)), Some(R::C(y, ay))) if ax == ay => Some((zip(op, x, y), ax)),
            (Some(R::C(x, ax)), Some(R::S(y))) => Some((map(op, x, |l, f| f(l, y)), ax)),
            (Some(R::S(x)), Some(R::C(y, ay))) => Some((map(op, y, |r, f| f(x, r)), ay)),
            _ => None,
        };
        if let Some((re, axis)) = fast {
            if let Some(i) = re.iter().position(|v| !v.is_finite()) {
                fin(re[i], op_str(op)).map_err(|e| Error {
                    msg: format!("{} (at point {i})", e.msg),
                    ..e
                })?;
            }
            return Ok(Col {
                kind: ColKind::Real,
                re,
                im: Vec::new(),
                axis,
            });
        }
    }
    elementwise(&[a, b], axes, strs, |v, strs| binary(op, v[0], v[1], strs))
}

fn arith(op: OpKind) -> fn(f64, f64) -> f64 {
    match op {
        OpKind::Add => |a, b| a + b,
        OpKind::Sub => |a, b| a - b,
        OpKind::Mul => |a, b| a * b,
        _ => |a, b| a / b,
    }
}

fn zip(op: OpKind, x: &[f64], y: &[f64]) -> Vec<f64> {
    debug_assert_eq!(x.len(), y.len(), "same axis, same length");
    // One monomorphic loop per operator so each vectorizes.
    match op {
        OpKind::Add => x.iter().zip(y).map(|(a, b)| a + b).collect(),
        OpKind::Sub => x.iter().zip(y).map(|(a, b)| a - b).collect(),
        OpKind::Mul => x.iter().zip(y).map(|(a, b)| a * b).collect(),
        _ => x.iter().zip(y).map(|(a, b)| a / b).collect(),
    }
}

fn map(op: OpKind, x: &[f64], g: impl Fn(f64, fn(f64, f64) -> f64) -> f64) -> Vec<f64> {
    let f = arith(op);
    x.iter().map(|&v| g(v, f)).collect()
}

fn want_col(v: CV, name: &str) -> Result<Col, Error> {
    match v {
        CV::C(c) => Ok(c),
        CV::S(v) => Err(Error::new(
            Code::X003,
            Span::default(),
            format!("`{name}` needs a column, got {}", type_name(v)),
        )),
    }
}

fn want_real(c: &Col, name: &str) -> Result<(), Error> {
    if c.kind == ColKind::Real {
        return Ok(());
    }
    Err(Error::new(
        Code::X003,
        Span::default(),
        format!("`{name}` needs a real column"),
    ))
}

fn want_num(v: &CV, name: &str) -> Result<f64, Error> {
    match v {
        CV::S(V::Int(x)) => Ok(*x as f64),
        CV::S(V::Num(x)) => Ok(*x),
        CV::S(v) => Err(Error::new(
            Code::X003,
            Span::default(),
            format!("`{name}` needs a number, got {}", type_name(*v)),
        )),
        CV::C(_) => Err(Error::new(
            Code::X003,
            Span::default(),
            format!("`{name}` needs a scalar here, got a column"),
        )),
    }
}

fn want_int(v: &CV, name: &str, ok: impl Fn(i64) -> bool) -> Result<i64, Error> {
    match v {
        CV::S(V::Int(x)) if ok(*x) => Ok(*x),
        _ => Err(Error::new(
            Code::X003,
            Span::default(),
            format!("`{name}`: argument out of range or not an int"),
        )),
    }
}

fn call_cv(
    b: Builtin,
    mut args: Vec<CV>,
    axes: &mut Vec<Axis>,
    strs: &mut Vec<u8>,
) -> Result<CV, Error> {
    let name = b.name();
    if args.iter().all(|a| matches!(a, CV::S(_))) {
        let vs: Vec<V> = args
            .iter()
            .map(|a| if let CV::S(v) = a { *v } else { unreachable!() })
            .collect();
        return Ok(CV::S(call(b, &vs, strs)?));
    }
    if !b.calc_only() {
        return Ok(CV::C(elementwise(&args, axes, strs, |v, strs| {
            call(b, v, strs)
        })?));
    }
    let mut rest = args.split_off(1);
    let y = want_col(args.pop().expect("calc builtins take a column first"), name)?;
    let col = match b {
        Builtin::Deriv | Builtin::Integ => {
            if y.kind == ColKind::Bool {
                want_real(&y, name)?;
            }
            let ax = &axes[y.axis as usize];
            let f = if b == Builtin::Deriv {
                deriv_step
            } else {
                integ_step
            };
            let mut re = vec![0.0; y.re.len()];
            let mut im = vec![0.0; y.im.len()];
            for s in &ax.steps {
                let r = s.start as usize..s.end as usize;
                f(&ax.x[r.clone()], &y.re[r.clone()], &mut re[r.clone()]);
                if !im.is_empty() {
                    f(&ax.x[r.clone()], &y.im[r.clone()], &mut im[r]);
                }
            }
            check_finite(&re, name)?;
            check_finite(&im, name)?;
            Col { re, im, ..y }
        }
        Builtin::Avg | Builtin::Rms | Builtin::Ymax | Builtin::Ymin | Builtin::Pp => {
            want_real(&y, name)?;
            per_step(axes, &y, name, |x, v| reduce(b, x, v))?
        }
        Builtin::At => {
            if y.kind == ColKind::Bool {
                want_real(&y, name)?;
            }
            let x0 = want_num(&rest[0], name)?;
            let complex = y.kind == ColKind::Complex;
            let (re_col, im_col) = (
                Col {
                    im: Vec::new(),
                    kind: ColKind::Real,
                    ..y.clone()
                },
                Col {
                    re: y.im,
                    im: Vec::new(),
                    kind: ColKind::Real,
                    ..y
                },
            );
            let re = per_step(axes, &re_col, name, |x, v| Ok(interp(x, v, x0)))?;
            if complex {
                let im = per_step(axes, &im_col, name, |x, v| Ok(interp(x, v, x0)))?;
                Col {
                    kind: ColKind::Complex,
                    im: im.re,
                    ..re
                }
            } else {
                re
            }
        }
        Builtin::Cross => {
            want_real(&y, name)?;
            let level = want_num(&rest[0], name)?;
            let edge = rest
                .get(1)
                .map_or(Ok(0), |a| want_int(a, name, |e| (-1..=1).contains(&e)))?;
            let nth = rest
                .get(2)
                .map_or(Ok(1), |a| want_int(a, name, |n| n >= 1))?;
            per_step(axes, &y, name, |x, v| Ok(cross(x, v, level, edge, nth)))?
        }
        Builtin::Delay => {
            want_real(&y, name)?;
            let l1 = want_num(&rest[0], name)?;
            let l2 = want_num(&rest[2], name)?;
            let y2 = want_col(
                core::mem::replace(&mut rest[1], CV::S(V::Bool(false))),
                name,
            )?;
            want_real(&y2, name)?;
            if y2.axis != y.axis {
                return Err(x011(
                    "`delay` signals are on different sweep axes".to_string(),
                ));
            }
            let ax = &axes[y.axis as usize];
            let mut re = Vec::with_capacity(ax.steps.len());
            for s in &ax.steps {
                let r = s.start as usize..s.end as usize;
                let x = &ax.x[r.clone()];
                re.push(cross(x, &y2.re[r.clone()], l2, 0, 1) - cross(x, &y.re[r], l1, 0, 1));
            }
            check_finite(&re, name)?;
            let axis = per_step_axis(axes, y.axis);
            Col {
                kind: ColKind::Real,
                re,
                im: Vec::new(),
                axis,
            }
        }
        Builtin::Settle => {
            want_real(&y, name)?;
            let tol = want_num(&rest[0], name)?;
            if tol < 0.0 {
                return Err(Error::new(
                    Code::X003,
                    Span::default(),
                    "`settle` tolerance must be >= 0",
                ));
            }
            per_step(axes, &y, name, |x, v| Ok(settle(x, v, tol)))?
        }
        Builtin::Fft => {
            want_real(&y, name)?;
            fft(axes, &y)?
        }
        Builtin::GainMargin | Builtin::PhaseMargin => {
            if y.kind == ColKind::Bool {
                want_real(&y, name)?;
            }
            let (ax, re, im) = (&axes[y.axis as usize], &y.re, &y.im);
            let mut out = Vec::with_capacity(ax.steps.len());
            for s in &ax.steps {
                let r = s.start as usize..s.end as usize;
                let im = if im.is_empty() {
                    &[][..]
                } else {
                    &im[r.clone()]
                };
                out.push(margin(b == Builtin::GainMargin, &re[r], im));
            }
            check_finite(&out, name)?;
            let axis = per_step_axis(axes, y.axis);
            Col {
                kind: ColKind::Real,
                re: out,
                im: Vec::new(),
                axis,
            }
        }
        _ => unreachable!("`{name}` is not a column function"),
    };
    Ok(CV::C(col))
}

fn check_finite(v: &[f64], name: &str) -> Result<(), Error> {
    match v.iter().position(|x| !x.is_finite()) {
        None => Ok(()),
        Some(i) => Err(Error::new(
            Code::X010,
            Span::default(),
            format!("`{name}` has no finite value at point {i}"),
        )),
    }
}

/// The per-step axis of `parent`, created on first use.
fn per_step_axis(axes: &mut Vec<Axis>, parent: u32) -> u32 {
    if let Some(i) = axes
        .iter()
        .position(|a| a.kind == AxisKind::PerStep && a.parent == parent)
    {
        return i as u32;
    }
    let n = axes[parent as usize].steps.len();
    axes.push(Axis {
        x: (0..n).map(|i| i as f64).collect(),
        steps: core::iter::once(0..n as u32).collect(),
        parent,
        kind: AxisKind::PerStep,
    });
    axes.len() as u32 - 1
}

/// One value per step of `y`'s axis.
fn per_step(
    axes: &mut Vec<Axis>,
    y: &Col,
    name: &str,
    mut f: impl FnMut(&[f64], &[f64]) -> Result<f64, Error>,
) -> Result<Col, Error> {
    debug_assert_eq!(y.kind, ColKind::Real);
    let ax = &axes[y.axis as usize];
    let mut re = Vec::with_capacity(ax.steps.len());
    for s in &ax.steps {
        let r = s.start as usize..s.end as usize;
        re.push(f(&ax.x[r.clone()], &y.re[r])?);
    }
    if let Some(i) = re.iter().position(|x| !x.is_finite()) {
        return Err(Error::new(
            Code::X010,
            Span::default(),
            format!("`{name}` has no answer in sweep step {i}"),
        ));
    }
    let axis = per_step_axis(axes, y.axis);
    Ok(Col {
        kind: ColKind::Real,
        re,
        im: Vec::new(),
        axis,
    })
}

/// dy/dx: central differences inside, one-sided at the step ends.
fn deriv_step(x: &[f64], y: &[f64], out: &mut [f64]) {
    debug_assert!(x.len() == y.len() && y.len() == out.len());
    let n = y.len();
    if n < 2 {
        out.iter_mut().for_each(|d| *d = 0.0);
        return;
    }
    out[0] = (y[1] - y[0]) / (x[1] - x[0]);
    out[n - 1] = (y[n - 1] - y[n - 2]) / (x[n - 1] - x[n - 2]);
    for i in 1..n - 1 {
        out[i] = (y[i + 1] - y[i - 1]) / (x[i + 1] - x[i - 1]);
    }
}

/// Cumulative trapezoid integral from the step start.
fn integ_step(x: &[f64], y: &[f64], out: &mut [f64]) {
    debug_assert!(x.len() == y.len() && y.len() == out.len());
    let mut acc = 0.0;
    for i in 0..y.len() {
        if i > 0 {
            acc += (x[i] - x[i - 1]) * (y[i] + y[i - 1]) * 0.5;
        }
        out[i] = acc;
    }
}

fn reduce(b: Builtin, x: &[f64], y: &[f64]) -> Result<f64, Error> {
    if y.is_empty() {
        return Ok(f64::NAN);
    }
    let span = x[x.len() - 1] - x[0];
    Ok(match b {
        Builtin::Avg if y.len() == 1 => y[0],
        Builtin::Avg => trapz(x, y, |v| v) / span,
        Builtin::Rms if y.len() == 1 => y[0].abs(),
        Builtin::Rms => (trapz(x, y, |v| v * v) / span).sqrt(),
        Builtin::Ymax => y.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        Builtin::Ymin => y.iter().copied().fold(f64::INFINITY, f64::min),
        _ => {
            y.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                - y.iter().copied().fold(f64::INFINITY, f64::min)
        }
    })
}

/// Trapezoid integral of `g(y)` over the step.
fn trapz(x: &[f64], y: &[f64], g: impl Fn(f64) -> f64) -> f64 {
    debug_assert_eq!(x.len(), y.len());
    (1..y.len())
        .map(|i| (x[i] - x[i - 1]) * (g(y[i]) + g(y[i - 1])) * 0.5)
        .sum()
}

/// Linear interpolation at `x0` on the first segment containing it; NaN outside.
fn interp(x: &[f64], y: &[f64], x0: f64) -> f64 {
    if x.len() == 1 && x[0] == x0 {
        return y[0];
    }
    for k in 0..x.len().saturating_sub(1) {
        let (a, b) = (x[k], x[k + 1]);
        if (a <= x0 && x0 <= b) || (b <= x0 && x0 <= a) {
            return if a == b {
                y[k]
            } else {
                y[k] + (y[k + 1] - y[k]) * (x0 - a) / (b - a)
            };
        }
    }
    f64::NAN
}

/// x of the `nth` crossing of `level` (`edge` 1 rising, -1 falling, 0 either),
/// linearly interpolated; NaN if there is none. A rising crossing goes from
/// below to at-or-above, a falling one from above to at-or-below.
fn cross(x: &[f64], y: &[f64], level: f64, edge: i64, nth: i64) -> f64 {
    debug_assert!(nth >= 1 && (-1..=1).contains(&edge));
    let mut seen = 0;
    for k in 0..y.len().saturating_sub(1) {
        let (s0, s1) = (y[k] - level, y[k + 1] - level);
        let rising = s0 < 0.0 && s1 >= 0.0;
        let falling = s0 > 0.0 && s1 <= 0.0;
        let hit = match edge {
            1 => rising,
            -1 => falling,
            _ => rising || falling,
        };
        if hit {
            seen += 1;
            if seen == nth {
                return x[k] + (x[k + 1] - x[k]) * (-s0) / (s1 - s0);
            }
        }
    }
    f64::NAN
}

/// First x after which `|y - y_final| <= tol` holds to the end of the step.
fn settle(x: &[f64], y: &[f64], tol: f64) -> f64 {
    let Some(&last) = y.last() else {
        return f64::NAN;
    };
    match y.iter().rposition(|v| (v - last).abs() > tol) {
        None => x[0],
        Some(k) => x[k + 1],
    }
}

/// Gain margin (dB) at the first -180° phase crossing, or phase margin (°)
/// at the first unity-gain crossing, of a loop gain whose phase starts near
/// 0°. NaN when the crossing does not exist.
fn margin(gain: bool, re: &[f64], im: &[f64]) -> f64 {
    let n = re.len();
    let z = |i: usize| Complex64::new(re[i], if im.is_empty() { 0.0 } else { im[i] });
    let db = |i: usize| 20.0 * z(i).norm().log10();
    let mut prev_ph = 0.0;
    let mut phase = Vec::with_capacity(n);
    for i in 0..n {
        let mut p = z(i).im.atan2(z(i).re).to_degrees();
        if i > 0 {
            p -= 360.0 * ((p - prev_ph) / 360.0).round();
        }
        phase.push(p);
        prev_ph = p;
    }
    for i in 0..n.saturating_sub(1) {
        if gain {
            let (p0, p1) = (phase[i] + 180.0, phase[i + 1] + 180.0);
            if p0 > 0.0 && p1 <= 0.0 {
                let t = p0 / (p0 - p1);
                return -(db(i) + t * (db(i + 1) - db(i)));
            }
        } else {
            let (m0, m1) = (db(i), db(i + 1));
            if m0 >= 0.0 && m1 < 0.0 {
                let t = m0 / (m0 - m1);
                return 180.0 + phase[i] + t * (phase[i + 1] - phase[i]);
            }
        }
    }
    f64::NAN
}

/// `m` uniform samples over `[x0, x_end)` by linear interpolation; one
/// forward pass, so O(n + m). Assumes `xs` non-decreasing (a sweep axis).
fn resample(xs: &[f64], ys: &[f64], m: usize) -> Vec<Complex64> {
    assert!(
        xs.len() >= 2 && xs.len() == ys.len(),
        "resample needs two points per step"
    );
    let (x0, span) = (xs[0], xs[xs.len() - 1] - xs[0]);
    let mut k = 0;
    let out: Vec<Complex64> = (0..m)
        .map(|i| {
            let t = x0 + span * i as f64 / m as f64;
            while k + 2 < xs.len() && xs[k + 1] <= t {
                k += 1;
            }
            let (a, b) = (xs[k], xs[k + 1]);
            let y = if b > a {
                ys[k] + (ys[k + 1] - ys[k]) * ((t - a) / (b - a)).clamp(0.0, 1.0)
            } else {
                ys[k]
            };
            Complex64::new(y, 0.0)
        })
        .collect();
    debug_assert_eq!(out.len(), m);
    out
}

/// Single-sided spectrum per step: resample the step uniformly onto the next
/// power of two of its length (transient steps are not uniform), transform,
/// keep bins `0..m/2` scaled to amplitude. Bin k is at `k / span`.
fn fft(axes: &mut Vec<Axis>, y: &Col) -> Result<Col, Error> {
    let src = y.axis as usize;
    let mut planner = FftPlanner::new();
    let (mut re, mut im, mut fx, mut steps) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for s in &axes[src].steps {
        let r = s.start as usize..s.end as usize;
        let (xs, ys) = (&axes[src].x[r.clone()], &y.re[r]);
        let n = xs.len();
        let span = if n >= 2 { xs[n - 1] - xs[0] } else { 0.0 };
        if n < 2 || span <= 0.0 || span.is_nan() {
            return Err(x011(
                "`fft` needs at least 2 points and an increasing axis per step".to_string(),
            ));
        }
        let m = n.next_power_of_two();
        let mut buf = resample(xs, ys, m);
        planner.plan_fft_forward(m).process(&mut buf);
        let start = re.len() as u32;
        for (k, v) in buf.iter().take(m / 2).enumerate() {
            let scale = if k == 0 { 1.0 } else { 2.0 } / m as f64;
            fx.push(k as f64 / span);
            re.push(v.re * scale);
            im.push(v.im * scale);
        }
        steps.push(start..re.len() as u32);
    }
    check_finite(&re, "fft")?;
    check_finite(&im, "fft")?;
    // One fft axis per source axis, so fft(a) and fft(b) of one plot combine.
    let axis = match axes
        .iter()
        .position(|a| a.kind == AxisKind::Fft && a.parent == y.axis)
    {
        Some(i) => i as u32,
        None => {
            axes.push(Axis {
                x: fx,
                steps,
                parent: y.axis,
                kind: AxisKind::Fft,
            });
            axes.len() as u32 - 1
        }
    };
    Ok(Col {
        kind: ColKind::Complex,
        re,
        im,
        axis,
    })
}
