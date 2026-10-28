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
use schemify_expr::{
    CalcResult, Code, Error, Program, Scratch, SignalKind, Value, op_parts, parse_num,
};
use schemify_netlist::NameMap;
use schemify_wave::calc::{op_dataset, op_value};
use schemify_wave::{RawPlot, group_datasets, parse_raw};

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
    err_job: Vec<u32>,
    err_output: Vec<u16>,
    err_msg: Vec<String>,
    /// The `op(…)` signals of every output (`<inst>[<param>]`), sorted, distinct.
    ops: Vec<String>,
    /// `rows × ops`: the simulator's vector for each op (`@m.x1.mm2[gm]`), or why the job
    /// has none. Not persisted: only [`Results::record`] of the same run reads it.
    op_vector: Vec<Result<String, (Code, String)>>,
}

impl Results {
    /// Creates an empty table for `plan`'s outputs, specs and vars, tagged with the run's
    /// `design` hash (from [`crate::design`]).
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
        assert_eq!(r.outputs.len(), plan.outputs.len());
        r
    }

    /// Returns the design hash the results were computed for.
    pub fn design(&self) -> &Digest {
        &self.design
    }

    /// Returns whether these results belong to another design than `current` (any reached
    /// cell, the bench, the lockfile, settings, environment or job list changed).
    pub fn stale(&self, current: &Digest) -> bool {
        &self.design != current
    }

    /// Returns the output names (column order of [`Results::value`]).
    pub fn outputs(&self) -> &[String] {
        &self.outputs
    }

    /// Returns the number of specs.
    pub fn specs(&self) -> usize {
        self.specs.len()
    }

    /// Returns the var names (column order of [`Results::var_value`]).
    pub fn vars(&self) -> &[String] {
        &self.vars
    }

    /// Returns output `o` of job `j`, if it has one.
    ///
    /// # Panics
    ///
    /// Panics if `o` is not an output column.
    pub fn value(&self, j: u32, o: u16) -> Option<f64> {
        let x = *self.cell(&self.value, self.outputs.len(), j, o as usize)?;
        (!x.is_nan()).then_some(x)
    }

    /// Returns spec `s`'s verdict for job `j`.
    ///
    /// # Panics
    ///
    /// Panics if `s` is not a spec.
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
    pub fn var_value(&self, j: u32, v: u16) -> Option<f64> {
        let x = *self.cell(&self.var_value, self.vars.len(), j, v as usize)?;
        (!x.is_nan()).then_some(x)
    }

    /// Iterates error rows `(job, output or JOB_LEVEL, message)` in recording order.
    pub fn errors(&self) -> impl Iterator<Item = (u32, u16, &str)> + '_ {
        let rows = self.err_job.iter().zip(&self.err_output).zip(&self.err_msg);
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
        let plots = match parse_raw(raw) {
            Ok(p) if !p.is_empty() => group_datasets(p),
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
        let mut values = Vec::with_capacity(plan.outputs.len());
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
                    let bound = |p: &Option<Program>| {
                        p.as_ref().map(|p| eval_scalar(p, lookup)).transpose()
                    };
                    match (bound(&spec.min), bound(&spec.max)) {
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
        let none = Err((Code::X006, "the job was not netlisted".to_owned()));
        self.op_vector.resize(n * self.ops.len(), none);
        assert_eq!(self.value.len(), n * self.outputs.len());
    }

    /// Sets output `o` of covered job `j`.
    pub(crate) fn set_value(&mut self, j: u32, o: usize, x: f64) {
        let k = self.index(self.outputs.len(), j, o);
        self.value[k] = x;
    }

    /// Sets var `v` of covered job `j`.
    pub(crate) fn set_var(&mut self, j: u32, v: usize, x: f64) {
        let k = self.index(self.vars.len(), j, v);
        self.var_value[k] = x;
    }

    /// Sets spec `s`'s verdict for covered job `j`.
    pub(crate) fn set_pass(&mut self, j: u32, s: usize, p: Pass) {
        let k = self.index(self.specs.len(), j, s);
        self.pass[k] = p;
    }

    /// Appends an error row of job `j` for output `output` or [`JOB_LEVEL`].
    pub(crate) fn error(&mut self, j: u32, output: u16, msg: String) {
        assert!(output == JOB_LEVEL || (output as usize) < self.outputs.len());
        assert!(j < self.rows, "error of job {j} outside the table");
        self.err_job.push(j);
        self.err_output.push(output);
        self.err_msg.push(msg);
    }

    /// Records the bench var values job `j` was elaborated with (numbers only).
    pub(crate) fn set_vars(&mut self, j: u32, e: &Elab) {
        self.ensure(j);
        let b = &e.bench;
        assert_eq!(b.var_name.len(), b.var_text.len());
        for k in 0..b.var_name.len() {
            let name = e.names.get(b.var_name[k]);
            if let Some(v) = self.vars.iter().position(|x| x == name)
                && let Ok(x) = parse_num(e.names.get(b.var_text[k]))
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
        let mut saves = String::new();
        for k in 0..n {
            let (inst, param) = op_parts(&self.ops[k]);
            let v = match dialect {
                Dialect::Ngspice => map
                    .spice_device(inst)
                    .map(|dev| format!("@{dev}[{}]", param.to_ascii_lowercase()))
                    .map_err(|why| (Code::X004, format!("op(): {why}"))),
                d => Err((
                    Code::X006,
                    format!(
                        "op() needs device operating-point data, which {} does not provide",
                        d.as_str()
                    ),
                )),
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
        }
        assert!(saves.is_empty() || saves.starts_with(".save all\n"));
        saves
    }
}

// ---- private ----

impl Results {
    fn cell<'t, T>(&self, col: &'t [T], width: usize, j: u32, k: usize) -> Option<&'t T> {
        assert!(k < width, "column {k} of {width}");
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
        let op_plot = op_dataset(plots);
        let mut ops = Vec::new();
        for (k, (kind, name)) in prog.signals().enumerate() {
            if kind != SignalKind::Op {
                continue;
            }
            let at = prog.signal_span(k);
            let slot = self.ops.binary_search_by(|x| x.as_str().cmp(name));
            let slot = slot.expect("Results::new lists every op signal");
            let x = match (&self.op_vector[j as usize * n + slot], op_plot) {
                (Err((code, why)), _) => Err(Error::new(*code, at, why.clone())),
                (Ok(_), None) => Err(Error::new(
                    Code::X010,
                    at,
                    "op() reads the operating point: the job has none (add `.op` to the bench)",
                )),
                (Ok(vec), Some(p)) => op_value(p, vec).ok_or_else(|| {
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
fn eval_output<'v>(
    prog: &Program,
    plots: &[RawPlot],
    ops: &[f64],
    lookup: impl Fn(&str) -> Option<Value<'v>> + Copy,
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
    let r = schemify_wave::calc::eval(prog, plot, Some(ops), lookup, scratch)
        .map_err(|e| e.to_string())?;
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

fn eval_scalar<'v>(
    prog: &Program,
    lookup: impl Fn(&str) -> Option<Value<'v>>,
) -> Result<f64, String> {
    let mut inputs = Vec::new();
    prog.bind(&mut inputs, lookup).map_err(|e| e.to_string())?;
    match prog
        .eval(&inputs, &mut Scratch::default())
        .map_err(|e| e.to_string())?
    {
        Value::Num(x) => Ok(x),
        Value::Int(i) => Ok(i as f64),
        v => Err(format!("is not a number: {v:?}")),
    }
}
