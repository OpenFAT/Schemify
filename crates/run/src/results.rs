//! The `results` table (run spec §4): per job, the bench var values it ran with, one
//! value per output (a calculator expression over the job's raw file) and one verdict
//! per spec; plus an error table. Keyed by (design hash, job): the whole table carries
//! the run's design hash, rows are job indices.
//!
//! Six questions: in, raw-file bytes per finished job; out, dense rows. Rows ≤
//! `JOBS_MAX`; outputs, specs and vars ≤ 2^12 each. Values are `f64` with NaN meaning
//! "no value" (the expression language never produces NaN, so the niche is free).
//! Dense, indexed by job, so completion order never shows: `jobs=1` and `jobs=N` write
//! identical bytes. Lifetime: one run; persisted as one line-record file
//! ([`write_results`], [`read_results`]).

use crate::jobs::{Jobs, NO_SAMPLE, State};
use crate::plan::Plan;
use schemify_doc::Digest;
use schemify_elab::Elab;
use schemify_expr::{CalcResult, Program, Scratch, Value, parse_num, write_num};
use schemify_wave::{RawPlot, group_datasets, parse_raw};
use std::fmt::Write;

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

/// Results of one run. Written only by this crate: [`crate::prepare`] (var values),
/// [`Results::record`] and [`Results::fail`].
#[derive(Clone, Debug, PartialEq)]
pub struct Results {
    design: Digest,
    outputs: Vec<String>,
    /// Output index per spec.
    specs: Vec<u16>,
    vars: Vec<String>,
    rows: u32,
    value: Vec<f64>,
    pass: Vec<Pass>,
    var_value: Vec<f64>,
    err_job: Vec<u32>,
    err_output: Vec<u16>,
    err_msg: Vec<String>,
}

impl Results {
    /// Creates an empty table for `plan`'s outputs, specs and vars, tagged with the run's
    /// `design` hash (from [`crate::design`]).
    pub fn new(plan: &Plan, design: Digest) -> Results {
        let r = Results {
            design,
            outputs: plan.outputs.iter().map(|o| o.name.clone()).collect(),
            specs: plan.specs.iter().map(|s| s.output).collect(),
            vars: plan.vars.iter().map(|v| v.name.clone()).collect(),
            rows: 0,
            value: Vec::new(),
            pass: Vec::new(),
            var_value: Vec::new(),
            err_job: Vec::new(),
            err_output: Vec::new(),
            err_msg: Vec::new(),
        };
        assert!(r.specs.iter().all(|&o| (o as usize) < r.outputs.len()));
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

    /// Returns the output names (column order).
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
    pub fn value(&self, j: u32, o: u16) -> Option<f64> {
        let x = *self.cell(&self.value, self.outputs.len(), j, o as usize)?;
        (!x.is_nan()).then_some(x)
    }

    /// Returns spec `s`'s verdict for job `j`.
    pub fn pass(&self, j: u32, s: u16) -> Pass {
        self.cell(&self.pass, self.specs.len(), j, s as usize)
            .copied()
            .unwrap_or(Pass::Unknown)
    }

    /// Returns the numeric value of var `v` job `j` was elaborated with.
    pub fn var_value(&self, j: u32, v: u16) -> Option<f64> {
        let x = *self.cell(&self.var_value, self.vars.len(), j, v as usize)?;
        (!x.is_nan()).then_some(x)
    }

    /// Iterates error rows `(job, output or JOB_LEVEL, message)` in recording order.
    pub fn errors(&self) -> impl Iterator<Item = (u32, u16, &str)> + '_ {
        (0..self.err_job.len()).map(|e| {
            (
                self.err_job[e],
                self.err_output[e],
                self.err_msg[e].as_str(),
            )
        })
    }

    fn cell<'t, T>(&self, col: &'t [T], width: usize, j: u32, k: usize) -> Option<&'t T> {
        assert!(k < width, "column {k} of {width}");
        (j < self.rows).then(|| &col[j as usize * width + k])
    }

    /// Grows the dense columns to cover job `j`.
    fn ensure(&mut self, j: u32) {
        if j < self.rows {
            return;
        }
        self.rows = j + 1;
        let n = self.rows as usize;
        self.value.resize(n * self.outputs.len(), f64::NAN);
        self.pass.resize(n * self.specs.len(), Pass::Unknown);
        self.var_value.resize(n * self.vars.len(), f64::NAN);
        assert!(self.value.len() == n * self.outputs.len());
    }

    fn error(&mut self, j: u32, output: u16, msg: String) {
        assert!(output == JOB_LEVEL || (output as usize) < self.outputs.len());
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
                self.var_value[j as usize * self.vars.len() + v] = x;
            }
        }
    }

    /// Marks job `j` failed with a job-level message (gate, netlist, compile, simulator,
    /// raw file, or a runner plugin's own failure).
    pub fn fail(&mut self, jobs: &mut Jobs, j: u32, msg: impl Into<String>) {
        assert!(j < jobs.len());
        self.ensure(j);
        self.error(j, JOB_LEVEL, msg.into());
        jobs.set_state(j, State::Failed);
    }

    /// Evaluates `plan`'s outputs and specs for job `j` over its raw file `raw` (ngspice
    /// or Xyce, binary or ASCII) and marks the job [`State::Done`]; a raw file that does
    /// not parse fails the job instead. Called by the native runner and by the app for
    /// runner plugins' results.
    ///
    /// Each output is evaluated over the last dataset that holds every signal it names
    /// (run spec §4) and must reduce to one number; identifiers resolve to the job's
    /// bench var values. Failures are error rows; the job is still done.
    ///
    /// Cost: one raw parse plus one calculator evaluation per output.
    pub fn record(&mut self, plan: &Plan, jobs: &mut Jobs, j: u32, raw: &[u8]) {
        assert!(j < jobs.len());
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
        let vars: Vec<(String, f64)> = (0..self.vars.len() as u16)
            .filter_map(|v| Some((self.vars[v as usize].clone(), self.var_value(j, v)?)))
            .collect();
        let lookup = |n: &str| {
            vars.iter()
                .find(|(k, _)| k == n)
                .map(|(_, x)| Value::Num(*x))
        };
        let mut scratch = Scratch::default();
        let width = self.outputs.len();
        for (o, out) in plan.outputs.iter().enumerate() {
            match eval_output(&out.prog, &plots, lookup, &mut scratch) {
                Ok(x) => self.value[j as usize * width + o] = x,
                Err(e) => self.error(j, o as u16, e),
            }
        }
        for (s, spec) in plan.specs.iter().enumerate() {
            let verdict = match self.value(j, spec.output) {
                None => Pass::Unknown,
                Some(x) => {
                    let lo = spec
                        .min
                        .as_ref()
                        .map(|p| eval_scalar(p, lookup))
                        .transpose();
                    let hi = spec
                        .max
                        .as_ref()
                        .map(|p| eval_scalar(p, lookup))
                        .transpose();
                    match (lo, hi) {
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
            self.pass[j as usize * self.specs.len() + s] = verdict;
        }
        jobs.set_state(j, State::Done);
    }
}

/// Evaluates one output over the last dataset that binds all its signals; the result
/// must be one number (a scalar, or a column of one point such as a reduction of a
/// single-step dataset).
fn eval_output<'v>(
    prog: &Program,
    plots: &[RawPlot],
    lookup: impl Fn(&str) -> Option<Value<'v>> + Copy,
    scratch: &mut Scratch,
) -> Result<f64, String> {
    let has_all = |p: &RawPlot| prog.signals().all(|(k, n)| p.find_signal(k, n).is_some());
    let plot = plots
        .iter()
        .rev()
        .find(|p| has_all(p))
        .unwrap_or(&plots[plots.len() - 1]);
    let r = schemify_wave::calc::eval(prog, plot, lookup, scratch).map_err(|e| e.to_string())?;
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

// ------------------------------------------------------------------------ results file

/// Writes the run's jobs and results as one line-record file (run spec §4): header,
/// column names, then one `job` record per job in index order. Canonical: the same
/// tables always produce the same bytes, whatever order jobs finished in.
pub fn write_results(jobs: &Jobs, r: &Results) -> String {
    assert!(r.rows <= jobs.len(), "results for jobs the table lacks");
    let mut out = String::from("schemify 1 results\n  design ");
    out.push_str(&schemify_doc::hex(&r.design));
    out.push('\n');
    let section = |out: &mut String, word: &str, names: &mut dyn Iterator<Item = &str>| {
        let mut first = true;
        for n in names {
            if first {
                out.push('\n');
                first = false;
            }
            out.push_str(word);
            out.push(' ');
            push_value(out, n);
            out.push('\n');
        }
    };
    section(
        &mut out,
        "output",
        &mut r.outputs.iter().map(String::as_str),
    );
    section(
        &mut out,
        "spec",
        &mut r.specs.iter().map(|&o| r.outputs[o as usize].as_str()),
    );
    section(&mut out, "var", &mut r.vars.iter().map(String::as_str));
    let mut errs: Vec<usize> = (0..r.err_job.len()).collect();
    errs.sort_by_key(|&e| (r.err_job[e], r.err_output[e]));
    let mut e = 0;
    let attr = |out: &mut String, key: &str, vals: &[&str]| {
        out.push_str("  ");
        out.push_str(key);
        for v in vals {
            out.push(' ');
            push_value(out, v);
        }
        out.push('\n');
    };
    let mut num = String::new();
    for j in 0..jobs.len() {
        write!(out, "\njob {j} {}\n", jobs.state(j).word()).expect("String write");
        if let Some(c) = jobs.config(j) {
            attr(&mut out, "config", &[c]);
        }
        if let Some(c) = jobs.corner(j) {
            attr(&mut out, "corner", &[c]);
        }
        if jobs.sample(j) != NO_SAMPLE {
            attr(&mut out, "sample", &[&jobs.sample(j).to_string()]);
        }
        for (v, x) in jobs.assignments(j) {
            attr(&mut out, "set", &[v, x]);
        }
        let cols = [
            ("var", &r.vars, &r.var_value),
            ("value", &r.outputs, &r.value),
        ];
        for (key, names, col) in cols {
            for (k, n) in names.iter().enumerate() {
                if let Some(x) = r.cell(col, names.len(), j, k).filter(|x| !x.is_nan()) {
                    num.clear();
                    write_num(&mut num, *x);
                    attr(&mut out, key, &[n, &num]);
                }
            }
        }
        for (s, &o) in r.specs.iter().enumerate() {
            match r.pass(j, s as u16) {
                Pass::Pass => attr(&mut out, "pass", &[&r.outputs[o as usize], "pass"]),
                Pass::Fail => attr(&mut out, "pass", &[&r.outputs[o as usize], "fail"]),
                Pass::Unknown => {}
            }
        }
        while e < errs.len() && r.err_job[errs[e]] == j {
            let k = errs[e];
            let what = match r.err_output[k] {
                JOB_LEVEL => "-",
                o => r.outputs[o as usize].as_str(),
            };
            attr(&mut out, "error", &[what, &r.err_msg[k]]);
            e += 1;
        }
    }
    assert_eq!(e, errs.len(), "every error row belongs to a job");
    out
}

/// Appends `v` bare when it is a legal bare value (format spec §2), else quoted.
fn push_value(out: &mut String, v: &str) {
    let bare = !v.is_empty()
        && !v.starts_with(['#', '|'])
        && v.chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && c != '"' && c != '\\');
    if bare {
        out.push_str(v);
        return;
    }
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => write!(out, "\\u{{{:x}}}", c as u32).expect("String write"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Splits one line into values (bare or quoted, format spec §2 lexical layer).
fn split_values(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut it = line.chars().peekable();
    loop {
        while it.next_if(|c| *c == ' ' || *c == '\t').is_some() {}
        let Some(&c) = it.peek() else { break };
        let mut v = String::new();
        if c == '"' {
            it.next();
            loop {
                match it.next().ok_or("unclosed quote")? {
                    '"' => break,
                    '\\' => match it.next().ok_or("dangling escape")? {
                        '"' => v.push('"'),
                        '\\' => v.push('\\'),
                        'n' => v.push('\n'),
                        't' => v.push('\t'),
                        'u' => {
                            let hex: String =
                                it.by_ref().skip(1).take_while(|c| *c != '}').collect();
                            let ch = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32);
                            v.push(ch.ok_or("bad \\u escape")?);
                        }
                        e => return Err(format!("bad escape \\{e}")),
                    },
                    c => v.push(c),
                }
            }
        } else {
            while let Some(c) = it.next_if(|c| *c != ' ' && *c != '\t') {
                v.push(c);
            }
        }
        out.push(v);
    }
    Ok(out)
}

/// Reads a file written by [`write_results`] back into its jobs and results. Results
/// carry the design hash they were written with; compare it to the current design
/// ([`Results::stale`]).
///
/// # Errors
///
/// A line number and reason for anything [`write_results`] does not write.
pub fn read_results(text: &str) -> Result<(Jobs, Results), String> {
    let mut lines = text.lines().enumerate();
    let bad = |n: usize, why: &str| format!("line {}: {why}", n + 1);
    match lines.next() {
        Some((_, "schemify 1 results")) => {}
        _ => return Err(bad(0, "not a results file")),
    }
    let mut r = Results {
        design: [0; 32],
        outputs: Vec::new(),
        specs: Vec::new(),
        vars: Vec::new(),
        rows: 0,
        value: Vec::new(),
        pass: Vec::new(),
        var_value: Vec::new(),
        err_job: Vec::new(),
        err_output: Vec::new(),
        err_msg: Vec::new(),
    };
    let mut jobs = Jobs::new();
    // A job's record is complete when the next record starts; its fields gather here.
    struct Pending {
        state: State,
        config: Option<String>,
        corner: Option<String>,
        sample: u32,
        sets: Vec<(String, String)>,
        fields: Vec<(usize, Vec<String>)>,
    }
    let mut cur: Option<Pending> = None;
    let mut header = true;
    let flush = |cur: Option<Pending>, jobs: &mut Jobs, r: &mut Results| -> Result<(), String> {
        let Some(p) = cur else { return Ok(()) };
        let sets: Vec<(&str, &str)> = p
            .sets
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let j = jobs
            .push(p.config.as_deref(), p.corner.as_deref(), p.sample, &sets)
            .map_err(|e| e.to_string())?;
        r.ensure(j);
        for (n, f) in p.fields {
            let find = |names: &[String], x: &str| {
                names
                    .iter()
                    .position(|y| y == x)
                    .ok_or_else(|| bad(n, &format!("unknown name `{x}`")))
            };
            match (f[0].as_str(), f.len()) {
                ("var" | "value", 3) => {
                    let x = parse_num(&f[2]).map_err(|e| bad(n, &e.to_string()))?;
                    if f[0] == "var" {
                        let k = find(&r.vars, &f[1])?;
                        r.var_value[j as usize * r.vars.len() + k] = x;
                    } else {
                        let k = find(&r.outputs, &f[1])?;
                        r.value[j as usize * r.outputs.len() + k] = x;
                    }
                }
                ("pass", 3) => {
                    let o = find(&r.outputs, &f[1])? as u16;
                    let s = r
                        .specs
                        .iter()
                        .position(|&x| x == o)
                        .ok_or_else(|| bad(n, "no such spec"))?;
                    r.pass[j as usize * r.specs.len() + s] = match f[2].as_str() {
                        "pass" => Pass::Pass,
                        "fail" => Pass::Fail,
                        _ => return Err(bad(n, "expected pass|fail")),
                    };
                }
                ("error", 3) => {
                    let o = if f[1] == "-" {
                        JOB_LEVEL
                    } else {
                        find(&r.outputs, &f[1])? as u16
                    };
                    r.error(j, o, f[2].clone());
                }
                _ => return Err(bad(n, "unexpected attribute")),
            }
        }
        jobs.set_state(j, p.state);
        Ok(())
    };
    for (n, line) in lines {
        if line.trim().is_empty() {
            continue;
        }
        let indented = line.starts_with(' ');
        let f = split_values(line).map_err(|e| bad(n, &e))?;
        if !indented {
            header = false;
            match (f[0].as_str(), f.len()) {
                ("output", 2) => r.outputs.push(f[1].clone()),
                ("spec", 2) => {
                    let o = r
                        .outputs
                        .iter()
                        .position(|x| *x == f[1])
                        .ok_or_else(|| bad(n, "spec of no output"))?;
                    r.specs.push(o as u16);
                }
                ("var", 2) => r.vars.push(f[1].clone()),
                ("job", 3) => {
                    flush(cur.take(), &mut jobs, &mut r)?;
                    if f[1].parse::<u32>() != Ok(jobs.len()) {
                        return Err(bad(n, "jobs must be numbered 0, 1, 2, …"));
                    }
                    let state = State::from_word(&f[2]).ok_or_else(|| bad(n, "unknown state"))?;
                    cur = Some(Pending {
                        state,
                        config: None,
                        corner: None,
                        sample: NO_SAMPLE,
                        sets: Vec::new(),
                        fields: Vec::new(),
                    });
                }
                _ => return Err(bad(n, "unexpected record")),
            }
            continue;
        }
        if header {
            if f.len() != 2 || f[0] != "design" || f[1].len() != 64 {
                return Err(bad(n, "expected `design <64 hex digits>`"));
            }
            for (k, b) in r.design.iter_mut().enumerate() {
                *b = u8::from_str_radix(&f[1][2 * k..2 * k + 2], 16)
                    .map_err(|_| bad(n, "bad hex"))?;
            }
            continue;
        }
        let p = cur
            .as_mut()
            .ok_or_else(|| bad(n, "attribute outside a job"))?;
        match (f[0].as_str(), f.len()) {
            ("config", 2) => p.config = Some(f[1].clone()),
            ("corner", 2) => p.corner = Some(f[1].clone()),
            ("sample", 2) => p.sample = f[1].parse().map_err(|_| bad(n, "bad sample"))?,
            ("set", 3) => p.sets.push((f[1].clone(), f[2].clone())),
            _ => p.fields.push((n, f)),
        }
    }
    flush(cur.take(), &mut jobs, &mut r)?;
    assert!(r.rows <= jobs.len());
    Ok((jobs, r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_quote_and_split_back() {
        let vals = [
            "a",
            "(1k)+(50)*(-0.5)",
            "two words",
            "q\"b\\s",
            "#x",
            "|y",
            "tab\there",
            "nl\n",
            "\u{7}",
        ];
        let mut line = String::new();
        for v in vals {
            push_value(&mut line, v);
            line.push(' ');
        }
        let back = split_values(&line).unwrap();
        assert_eq!(back, vals);
        assert!(split_values("\"open").is_err());
        assert!(split_values("\"bad \\q\"").is_err());
    }

    #[test]
    fn read_rejects_garbage() {
        assert!(read_results("").is_err());
        assert!(read_results("schemify 1 shbench\n").is_err());
        assert!(read_results("schemify 1 results\n  design 00\n").is_err());
        let hdr = format!("schemify 1 results\n  design {}\n", "0".repeat(64));
        assert!(
            read_results(&format!("{hdr}\njob 1 done\n")).is_err(),
            "numbering"
        );
        assert!(read_results(&format!("{hdr}\njob 0 weird\n")).is_err());
        assert!(read_results(&format!("{hdr}\njob 0 done\n  value nope 1\n")).is_err());
        let (j, r) = read_results(&format!("{hdr}\njob 0 done\n  set R 1k\n")).unwrap();
        assert_eq!((j.len(), j.assigned(0, "R")), (1, Some("1k")));
        assert!(!r.stale(&[0; 32]) && r.stale(&[1; 32]));
    }
}
