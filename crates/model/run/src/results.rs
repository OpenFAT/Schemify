//! The `results` table (run spec §4): raw-file bytes per finished job in, per job the bench
//! var values it ran with, one value per output (a calculator expression over the job's
//! raw file), one verdict per spec and error rows out. Keyed by (design hash, job): the
//! whole table carries the run's design hash, rows are job indices.
//!
//! Data: rows ≤ `JOBS_MAX`; outputs, specs and vars ≤ 2^12 each. Values are `f64` with
//! NaN meaning "no value" (the expression language never produces NaN, so the niche is
//! free). Columns are dense and indexed by job, so completion order never shows: `jobs=1`
//! and `jobs=N` write identical bytes. Lifetime: one run; persisted by `results_file`.
//!
//! Contents:
//! - `pub const JOB_LEVEL: u16`
//! - `pub enum Pass`
//! - `pub struct Results`: `new`, `design`, `stale`, `outputs`, `specs`, `vars`, `value`,
//!   `pass`, `var_value`, `errors`, `fail`, `record`; crate-only `from_columns`,
//!   `spec_outputs`, `rows`, `ensure`, `set_value`, `set_var`, `set_pass`, `error`,
//!   `set_vars`, `set_ops`

use crate::jobs::{Jobs, State};
use crate::plan::Plan;
use schemify_doc::Digest;
use schemify_elab::{Dialect, Elab};
use schemify_expr::{CalcResult, Code, Error, Program, Scratch, SignalKind, Value, op_parts};
use schemify_netlist::NameMap;
use schemify_wave::RawPlot;
use schemify_wave::calc;

/// `error` rows that belong to the whole job, not one output.
pub const JOB_LEVEL: u16 = u16::MAX;

/// A spec verdict for one job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Pass {
    /// Not evaluated: the job has not finished, or the output or a bound has no value.
    Unknown,
    /// Inside `[min, max]`.
    Pass,
    /// Outside.
    Fail,
}

/// Results of one run. Written only by this crate: `prepare` (var values, `op(…)`
/// vectors), [`Results::record`], [`Results::fail`] and the results-file reader.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Results {
    design: Digest,
    outputs: Vec<String>,
    /// Output index per spec.
    specs: Vec<u16>,
    vars: Vec<String>,
    /// Jobs the dense columns cover.
    rows: u32,
    /// `rows × outputs`.
    value: Vec<f64>,
    /// `rows × specs`.
    pass: Vec<Pass>,
    /// `rows × vars`.
    var_value: Vec<f64>,
    error_job: Vec<u32>,
    error_output: Vec<u16>,
    error_message: Vec<String>,
    /// The `op(…)` signals of every output (`<inst>[<param>]`), sorted, distinct.
    ops: Vec<String>,
    /// `rows × ops`: the simulator's vector for each op (`@m.x1.mm2[gm]`), or why the job
    /// has none. Not persisted: only [`Results::record`] of the same run reads it.
    op_vector: Vec<Result<String, OpMissing>>,
}

/// Why a job has no simulator vector for an `op(…)` signal: the error its outputs get.
#[derive(Clone, Debug, PartialEq)]
struct OpMissing {
    code: Code,
    why: String,
}

impl Results {
    /// Creates an empty table for `plan`'s outputs, specs and vars, tagged with the run's
    /// `design` hash (from [`crate::design`]).
    ///
    /// # Panics
    ///
    /// Panics if a spec of `plan` names no output, which [`Plan::read`] never builds.
    #[must_use]
    pub fn new(plan: &Plan, design: Digest) -> Results {
        let mut r = Results::from_columns(
            design,
            plan.outputs.iter().map(|o| o.name.clone()).collect(),
            plan.specs.iter().map(|s| s.output).collect(),
            plan.vars.iter().map(|v| v.name.clone()).collect(),
        );
        let signals = plan.outputs.iter().flat_map(|o| o.prog.signals());
        r.ops = signals
            .filter(|(k, _)| *k == SignalKind::Op)
            .map(|(_, n)| n.to_owned())
            .collect();
        r.ops.sort();
        r.ops.dedup();
        assert_eq!(r.rows, 0, "no job is covered yet");
        assert!(r.ops.windows(2).all(|w| w[0] < w[1]), "sorted and distinct");
        r
    }

    /// Returns the design hash the results were computed for.
    #[must_use]
    pub fn design(&self) -> &Digest {
        &self.design
    }

    /// Returns whether these results belong to another design than `current` (any reached
    /// cell, the bench, the lockfile, settings, environment or job list changed).
    #[must_use]
    pub fn stale(&self, current: &Digest) -> bool {
        &self.design != current
    }

    /// Returns the output names (column order of [`Results::value`]).
    #[must_use]
    pub fn outputs(&self) -> &[String] {
        &self.outputs
    }

    /// Returns the number of specs.
    #[must_use]
    pub fn specs(&self) -> usize {
        self.specs.len()
    }

    /// Returns the var names (column order of [`Results::var_value`]).
    #[must_use]
    pub fn vars(&self) -> &[String] {
        &self.vars
    }

    /// Returns output `o` of job `j`, if it has one.
    ///
    /// # Panics
    ///
    /// Panics if `o` is not an output column.
    #[must_use]
    pub fn value(&self, j: u32, o: u16) -> Option<f64> {
        let x = *self.cell(&self.value, self.outputs.len(), j, o as usize)?;
        (!x.is_nan()).then_some(x)
    }

    /// Returns spec `s`'s verdict for job `j`.
    ///
    /// # Panics
    ///
    /// Panics if `s` is not a spec.
    #[must_use]
    pub fn pass(&self, j: u32, s: u16) -> Pass {
        self.cell(&self.pass, self.specs.len(), j, s as usize)
            .copied()
            .unwrap_or(Pass::Unknown)
    }

    /// Returns the numeric value of var `v` job `j` was elaborated with.
    ///
    /// # Panics
    ///
    /// Panics if `v` is not a var column.
    #[must_use]
    pub fn var_value(&self, j: u32, v: u16) -> Option<f64> {
        let x = *self.cell(&self.var_value, self.vars.len(), j, v as usize)?;
        (!x.is_nan()).then_some(x)
    }

    /// Iterates error rows `(job, output or JOB_LEVEL, message)` in recording order.
    pub fn errors(&self) -> impl Iterator<Item = (u32, u16, &str)> + '_ {
        debug_assert_eq!(
            self.error_job.len(),
            self.error_output.len(),
            "columns in step"
        );
        debug_assert_eq!(
            self.error_job.len(),
            self.error_message.len(),
            "columns in step"
        );
        let rows = self
            .error_job
            .iter()
            .zip(&self.error_output)
            .zip(&self.error_message);
        rows.map(|((&j, &o), m)| (j, o, m.as_str()))
    }

    /// Marks job `j` failed with a job-level message (gate, netlist, compile, simulator,
    /// raw file, or a runner plugin's own failure).
    ///
    /// # Panics
    ///
    /// Panics if `j` is not a job of `jobs` or already finished in another state.
    pub fn fail(&mut self, jobs: &mut Jobs, j: u32, msg: impl Into<String>) {
        assert!(j < jobs.len(), "job {j} of {}", jobs.len());
        self.ensure(j);
        self.error(j, JOB_LEVEL, msg.into());
        jobs.set_state(j, State::Failed);
        debug_assert!(
            self.errors().any(|(job, _, _)| job == j),
            "the job has its reason"
        );
    }

    /// Evaluates `plan`'s outputs and specs for job `j` over its raw file `raw` (ngspice
    /// or Xyce, binary or ASCII) and marks the job [`State::Done`]; a raw file that does
    /// not parse or holds no data fails the job instead. Called by the native runner and
    /// by the app for runner plugins' results.
    ///
    /// Each output is evaluated over the last dataset that holds every signal it names
    /// (run spec §4) and must reduce to one number; identifiers resolve to the job's
    /// bench var values. Failures are error rows; the job is still done.
    ///
    /// Cost: one raw parse plus one calculator evaluation per output.
    ///
    /// # Panics
    ///
    /// Panics if `j` is not a job of `jobs`, already finished in another state, or the
    /// table was made for another plan.
    pub fn record(&mut self, plan: &Plan, jobs: &mut Jobs, j: u32, raw: &[u8]) {
        assert!(j < jobs.len(), "job {j} of {}", jobs.len());
        assert_eq!(
            plan.outputs.len(),
            self.outputs.len(),
            "results of another plan"
        );
        assert_eq!(
            plan.specs.len(),
            self.specs.len(),
            "results of another plan"
        );
        let plots = match schemify_wave::parse_raw(raw) {
            Ok(p) if !p.is_empty() => schemify_wave::group_datasets(p),
            Ok(_) => return self.fail(jobs, j, "raw file holds no data"),
            Err(e) => return self.fail(jobs, j, format!("raw file: {e}")),
        };
        self.ensure(j);
        // Owned names: `lookup` outlives the writes below.
        let vars: Vec<(String, f64)> = (0..self.vars.len() as u16)
            .filter_map(|v| Some((self.vars[v as usize].clone(), self.var_value(j, v)?)))
            .collect();
        let lookup = |n: &str| {
            vars.iter()
                .find(|(k, _)| k == n)
                .map(|(_, x)| Value::Num(*x))
        };
        let mut scratch = Scratch::default();
        let mut values: Vec<Result<f64, String>> = Vec::with_capacity(plan.outputs.len());
        for out in &plan.outputs {
            let x = self
                .op_values(j, &out.prog, &plots)
                .and_then(|ops| eval_output(&out.prog, &plots, &ops, lookup, &mut scratch));
            values.push(x);
        }
        for (o, x) in values.into_iter().enumerate() {
            match x {
                Ok(x) => self.set_value(j, o, x),
                Err(e) => self.error(j, o as u16, e),
            }
        }
        for (s, spec) in plan.specs.iter().enumerate() {
            let verdict = match self.value(j, spec.output) {
                None => Pass::Unknown,
                Some(x) => {
                    let bound = |p: Option<&Program>| p.map(|p| eval_scalar(p, lookup)).transpose();
                    match (bound(spec.min.as_ref()), bound(spec.max.as_ref())) {
                        (Ok(lo), Ok(hi)) => {
                            let ok = lo.is_none_or(|lo| x >= lo) && hi.is_none_or(|hi| x <= hi);
                            if ok { Pass::Pass } else { Pass::Fail }
                        }
                        (Err(e), _) | (_, Err(e)) => {
                            self.error(j, spec.output, format!("spec bound: {e}"));
                            Pass::Unknown
                        }
                    }
                }
            };
            self.set_pass(j, s, verdict);
        }
        jobs.set_state(j, State::Done);
    }

    /// Creates an empty table with these columns and no `op(…)` signals.
    ///
    /// # Panics
    ///
    /// Panics if a spec names no output column.
    pub(crate) fn from_columns(
        design: Digest,
        outputs: Vec<String>,
        specs: Vec<u16>,
        vars: Vec<String>,
    ) -> Results {
        assert!(
            specs.iter().all(|&o| (o as usize) < outputs.len()),
            "a spec names an output"
        );
        let r = Results {
            design,
            outputs,
            specs,
            vars,
            ..Results::default()
        };
        assert_eq!(r.rows, 0);
        r
    }

    /// Returns the output index of every spec.
    pub(crate) fn spec_outputs(&self) -> &[u16] {
        &self.specs
    }

    /// Returns the number of jobs the dense columns cover.
    pub(crate) fn rows(&self) -> u32 {
        self.rows
    }

    /// Grows the dense columns to cover job `j`.
    pub(crate) fn ensure(&mut self, j: u32) {
        if j < self.rows {
            return;
        }
        assert!(j < crate::JOBS_MAX, "job {j} past JOBS_MAX");
        self.rows = j + 1;
        let n = self.rows as usize;
        self.value.resize(n * self.outputs.len(), f64::NAN);
        self.pass.resize(n * self.specs.len(), Pass::Unknown);
        self.var_value.resize(n * self.vars.len(), f64::NAN);
        let none = Err(OpMissing {
            code: Code::X006,
            why: "the job was not netlisted".to_owned(),
        });
        self.op_vector.resize(n * self.ops.len(), none);
        assert_eq!(self.value.len(), n * self.outputs.len());
    }

    /// Sets output `o` of covered job `j`.
    pub(crate) fn set_value(&mut self, j: u32, o: usize, x: f64) {
        assert!(!x.is_nan(), "NaN is the no-value marker");
        let k = self.index(self.outputs.len(), j, o);
        debug_assert_eq!(
            self.value.len(),
            self.rows as usize * self.outputs.len(),
            "covered rows"
        );
        self.value[k] = x;
    }

    /// Sets var `v` of covered job `j`.
    pub(crate) fn set_var(&mut self, j: u32, v: usize, x: f64) {
        assert!(!x.is_nan(), "NaN is the no-value marker");
        let k = self.index(self.vars.len(), j, v);
        debug_assert_eq!(
            self.var_value.len(),
            self.rows as usize * self.vars.len(),
            "covered rows"
        );
        self.var_value[k] = x;
    }

    /// Sets spec `s`'s verdict for covered job `j`.
    pub(crate) fn set_pass(&mut self, j: u32, s: usize, p: Pass) {
        let k = self.index(self.specs.len(), j, s);
        assert_eq!(
            self.pass.len(),
            self.rows as usize * self.specs.len(),
            "covered rows"
        );
        self.pass[k] = p;
    }

    /// Appends an error row of job `j` for output `output` or [`JOB_LEVEL`].
    pub(crate) fn error(&mut self, j: u32, output: u16, msg: String) {
        assert!(output == JOB_LEVEL || (output as usize) < self.outputs.len());
        assert!(j < self.rows, "error of job {j} outside the table");
        self.error_job.push(j);
        self.error_output.push(output);
        self.error_message.push(msg);
        assert_eq!(
            self.error_job.len(),
            self.error_message.len(),
            "columns in step"
        );
    }

    /// Records the bench var values job `j` was elaborated with (numbers only).
    pub(crate) fn set_vars(&mut self, j: u32, e: &Elab) {
        self.ensure(j);
        let b = &e.bench;
        assert_eq!(b.var_name.len(), b.var_text.len());
        for k in 0..b.var_name.len() {
            let name = e.names.get(b.var_name[k]);
            if let Some(v) = self.vars.iter().position(|x| x == name)
                && let Ok(x) = schemify_expr::parse_num(e.names.get(b.var_text[k]))
            {
                self.set_var(j, v, x);
            }
        }
    }

    /// Resolves job `j`'s `op(…)` signals against its netlist's name map (run spec §4) and
    /// returns the lines that make the simulator keep them, for the end of the netlist:
    /// `.save all` then one `.save` of every resolved device vector (ngspice), or nothing
    /// (no `op(…)`, or a dialect without device operating-point vectors, whose `op(…)`
    /// outputs then fail with the reason).
    pub(crate) fn set_ops(&mut self, j: u32, map: &NameMap, dialect: Dialect) -> String {
        self.ensure(j);
        let n = self.ops.len();
        assert_eq!(self.op_vector.len(), self.rows as usize * n, "covered rows");
        let mut saves = String::new();
        for k in 0..n {
            let op = op_parts(&self.ops[k]);
            let v = match dialect {
                Dialect::Ngspice => map
                    .spice_device(op.instance)
                    .map(|dev| format!("@{dev}[{}]", op.parameter.to_ascii_lowercase()))
                    .map_err(|why| OpMissing {
                        code: Code::X004,
                        why: format!("op(): {why}"),
                    }),
                d => Err(OpMissing {
                    code: Code::X006,
                    why: format!(
                        "op() needs device operating-point data, which {} does not provide",
                        d.as_str()
                    ),
                }),
            };
            if let Ok(vec) = &v {
                saves.push_str(if saves.is_empty() {
                    ".save all\n.save"
                } else {
                    ""
                });
                saves.push(' ');
                saves.push_str(vec);
            }
            self.op_vector[j as usize * n + k] = v;
        }
        if !saves.is_empty() {
            saves.push('\n');
            assert!(saves.starts_with(".save all\n"));
            assert!(saves.ends_with('\n'), "whole lines before `.end`");
        }
        saves
    }
}

// ---- private ----

impl Results {
    fn cell<'column, T>(
        &self,
        col: &'column [T],
        width: usize,
        j: u32,
        k: usize,
    ) -> Option<&'column T> {
        assert!(k < width, "column {k} of {width}");
        debug_assert_eq!(col.len(), self.rows as usize * width, "covered rows");
        (j < self.rows).then(|| &col[j as usize * width + k])
    }

    fn index(&self, width: usize, j: u32, k: usize) -> usize {
        assert!(k < width, "column {k} of {width}");
        assert!(j < self.rows, "job {j} outside the table");
        j as usize * width + k
    }

    /// One value per `op(…)` signal of `prog`, in signal order, from job `j`'s operating
    /// point; the first that fails is the output's error.
    fn op_values(&self, j: u32, prog: &Program, plots: &[RawPlot]) -> Result<Vec<f64>, String> {
        let n = self.ops.len();
        assert!(j < self.rows, "job {j} outside the table");
        let op_plot = calc::op_dataset(plots);
        let mut ops = Vec::new();
        for (k, (kind, name)) in prog.signals().enumerate() {
            if kind != SignalKind::Op {
                continue;
            }
            let at = prog.signal_span(k);
            let slot = self.ops.binary_search_by(|x| x.as_str().cmp(name));
            let slot = slot.expect("Results::new lists every op signal");
            let x = match (&self.op_vector[j as usize * n + slot], op_plot) {
                (Err(missing), _) => Err(Error::new(missing.code, at, missing.why.clone())),
                (Ok(_), None) => Err(Error::new(
                    Code::X010,
                    at,
                    "op() reads the operating point: the job has none (add `.op` to the bench)",
                )),
                (Ok(vec), Some(p)) => calc::op_value(p, vec).ok_or_else(|| {
                    let why = format!("no vector `{vec}` in the operating point (op({name}))");
                    Error::new(Code::X004, at, why)
                }),
            };
            ops.push(x.map_err(|e| e.to_string())?);
        }
        assert!(ops.len() <= prog.signals().count());
        Ok(ops)
    }
}

/// Evaluates one output over the last dataset that binds all its signals; the result
/// must be one number (a scalar, or a column of one point such as a reduction of a
/// single-step dataset).
fn eval_output<'value>(
    prog: &Program,
    plots: &[RawPlot],
    ops: &[f64],
    lookup: impl Fn(&str) -> Option<Value<'value>> + Copy,
    scratch: &mut Scratch,
) -> Result<f64, String> {
    assert!(!plots.is_empty());
    // `op(…)` values come from the operating point, whatever dataset the rest reads.
    let has_all = |p: &RawPlot| {
        prog.signals()
            .all(|(k, n)| k == SignalKind::Op || p.find_signal(k, n).is_some())
    };
    let plot = plots
        .iter()
        .rev()
        .find(|p| has_all(p))
        .unwrap_or(&plots[plots.len() - 1]);
    let r = calc::eval(prog, plot, Some(ops), lookup, scratch).map_err(|e| e.to_string())?;
    let x = match r {
        CalcResult::Scalar(Value::Num(x)) => x,
        CalcResult::Scalar(Value::Int(i)) => i as f64,
        CalcResult::Column(c) if c.re.len() == 1 && c.im.is_empty() => c.re[0],
        CalcResult::Column(c) => {
            return Err(format!(
                "is a column of {} points; reduce it to one number (at, avg, ymax, cross, …)",
                c.re.len()
            ));
        }
        CalcResult::Scalar(v) => return Err(format!("is not a number: {v:?}")),
    };
    assert!(
        x.is_finite(),
        "the calculator never yields non-finite values"
    );
    Ok(x)
}

fn eval_scalar<'value>(
    prog: &Program,
    lookup: impl Fn(&str) -> Option<Value<'value>>,
) -> Result<f64, String> {
    let mut inputs = Vec::new();
    prog.bind(&mut inputs, lookup).map_err(|e| e.to_string())?;
    let x = match prog
        .eval(&inputs, &mut Scratch::default())
        .map_err(|e| e.to_string())?
    {
        Value::Num(x) => x,
        Value::Int(i) => i as f64,
        v => return Err(format!("is not a number: {v:?}")),
    };
    assert!(
        x.is_finite(),
        "the expression language refuses NaN and infinity (X010)"
    );
    Ok(x)
}
