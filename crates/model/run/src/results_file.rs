//! The results file (run spec §4): the `jobs` and `results` tables in, one line-record
//! text out ([`write_results`]), and back ([`read_results`]). Line records in the format
//! spec §2 lexical layer (values quoted and split by `schemify_abi::text`), not a format
//! spec file kind. Canonical: the same tables always give the same bytes, whatever order
//! the jobs finished in.
//!
//! Contents:
//! - `pub fn write_results(jobs: &Jobs, r: &Results) -> String`
//! - `pub fn read_results(text: &str) -> Result<(Jobs, Results), String>`

use crate::jobs::{Jobs, NO_SAMPLE, State};
use crate::results::{JOB_LEVEL, Pass, Results};
use schemify_abi::text;
use schemify_doc::Digest;

/// One job record being read: complete when the next record starts.
struct JobRecord {
    state: State,
    config: Option<String>,
    corner: Option<String>,
    sample: u32,
    seed: u32,
    sets: Vec<(String, String)>,
    /// `var`, `value`, `pass` and `error` lines, applied once the job has a row.
    fields: Vec<FieldLine>,
}

/// One attribute line kept for later: its 0-based line number and its values.
struct FieldLine {
    line: usize,
    values: Vec<String>,
}

/// Writes the run's jobs and results as one line-record file (run spec §4): header,
/// column names, then one `job` record per job in index order.
///
/// # Panics
///
/// Panics if `r` covers jobs `jobs` lacks.
#[must_use]
pub fn write_results(jobs: &Jobs, r: &Results) -> String {
    use std::fmt::Write;

    assert!(r.rows() <= jobs.len(), "results for jobs the table lacks");
    let mut out = String::from("schemify 1 results\n  design ");
    out.push_str(&schemify_doc::hex(r.design()));
    out.push('\n');

    // Column names: one section per kind, each after a blank line when present.
    let specs: Vec<&str> = r
        .spec_outputs()
        .iter()
        .map(|&o| r.outputs()[o as usize].as_str())
        .collect();
    let sections: [(&str, Vec<&str>); 3] = [
        ("output", r.outputs().iter().map(String::as_str).collect()),
        ("spec", specs),
        ("var", r.vars().iter().map(String::as_str).collect()),
    ];
    for (word, names) in &sections {
        if !names.is_empty() {
            out.push('\n');
        }
        for n in names {
            out.push_str(word);
            out.push(' ');
            text::push_value(&mut out, n);
            out.push('\n');
        }
    }

    // Jobs, each with its error rows sorted by output (stable: recording order within).
    let mut errs: Vec<(u32, u16, &str)> = r.errors().collect();
    errs.sort_by_key(|&(job, output, _)| (job, output));
    let mut e = 0;
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
        if let Some(seed) = jobs.seed(j) {
            attr(&mut out, "seed", &[&seed.to_string()]);
        }
        for (v, x) in jobs.assignments(j) {
            attr(&mut out, "set", &[v, x]);
        }
        for (v, n) in r.vars().iter().enumerate() {
            if let Some(x) = r.var_value(j, v as u16) {
                num.clear();
                schemify_expr::write_num(&mut num, x);
                attr(&mut out, "var", &[n, &num]);
            }
        }
        for (o, n) in r.outputs().iter().enumerate() {
            if let Some(x) = r.value(j, o as u16) {
                num.clear();
                schemify_expr::write_num(&mut num, x);
                attr(&mut out, "value", &[n, &num]);
            }
        }
        for (s, &o) in r.spec_outputs().iter().enumerate() {
            let verdict = match r.pass(j, s as u16) {
                Pass::Pass => "pass",
                Pass::Fail => "fail",
                Pass::Unknown => continue,
            };
            attr(&mut out, "pass", &[&r.outputs()[o as usize], verdict]);
        }
        while let Some(&(job, output, message)) = errs.get(e)
            && job == j
        {
            let what = match output {
                JOB_LEVEL => "-",
                o => r.outputs()[o as usize].as_str(),
            };
            attr(&mut out, "error", &[what, message]);
            e += 1;
        }
    }
    assert_eq!(e, errs.len(), "every error row belongs to a job");
    assert!(out.ends_with('\n'), "whole lines");
    out
}

/// Reads a file written by [`write_results`] back into its jobs and results. Results
/// carry the design hash they were written with; compare it to the current design
/// ([`Results::stale`]).
///
/// Cost: O(file bytes + jobs × columns).
///
/// # Errors
///
/// A line number and reason for anything [`write_results`] does not write: another
/// header, a malformed value, jobs out of order, an unknown state, name or attribute, or
/// a column record after the first job.
pub fn read_results(text: &str) -> Result<(Jobs, Results), String> {
    let mut lines = text.lines().enumerate();
    match lines.next() {
        Some((_, "schemify 1 results")) => {}
        _ => return Err(bad(0, "not a results file")),
    }
    let mut design: Digest = [0; 32];
    let (mut outputs, mut specs, mut vars) = (Vec::new(), Vec::new(), Vec::new());
    // Built at the first job record, once the columns are known.
    let mut table: Option<(Jobs, Results)> = None;
    let mut cur: Option<JobRecord> = None;
    let mut header = true;
    for (n, line) in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_values(line).ok_or_else(|| bad(n, "malformed value"))?;
        if line.starts_with(' ') && header {
            if f.len() != 2 || f[0] != "design" || f[1].len() != 64 {
                return Err(bad(n, "expected `design <64 hex digits>`"));
            }
            for (k, b) in design.iter_mut().enumerate() {
                let byte = f[1]
                    .get(2 * k..2 * k + 2)
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                *b = byte.ok_or_else(|| bad(n, "bad hex"))?;
            }
            continue;
        }
        if line.starts_with(' ') {
            let p = cur
                .as_mut()
                .ok_or_else(|| bad(n, "attribute outside a job"))?;
            p.attr(n, f)?;
            continue;
        }
        header = false;
        match (f[0].as_str(), f.len(), table.is_none()) {
            ("output", 2, true) => outputs.push(f[1].clone()),
            ("spec", 2, true) => {
                let o = outputs
                    .iter()
                    .position(|x| *x == f[1])
                    .ok_or_else(|| bad(n, "spec of no output"))?;
                specs.push(o as u16);
            }
            ("var", 2, true) => vars.push(f[1].clone()),
            ("job", 3, _) => {
                let (jobs, r) = table.get_or_insert_with(|| {
                    let r =
                        Results::from_columns(design, outputs.clone(), specs.clone(), vars.clone());
                    (Jobs::new(), r)
                });
                if let Some(p) = cur.take() {
                    p.flush(jobs, r)?;
                }
                if f[1].parse::<u32>() != Ok(jobs.len()) {
                    return Err(bad(n, "jobs must be numbered 0, 1, 2, …"));
                }
                let state = State::from_word(&f[2]).ok_or_else(|| bad(n, "unknown state"))?;
                cur = Some(JobRecord::new(state));
            }
            _ => return Err(bad(n, "unexpected record")),
        }
    }
    let (mut jobs, mut r) = table.unwrap_or_else(|| {
        (
            Jobs::new(),
            Results::from_columns(design, outputs, specs, vars),
        )
    });
    if let Some(p) = cur.take() {
        p.flush(&mut jobs, &mut r)?;
    }
    // Provable: every flush covers its job.
    debug_assert_eq!(r.rows(), jobs.len(), "every job read has a row");
    Ok((jobs, r))
}

// ---- private ----

impl JobRecord {
    fn new(state: State) -> JobRecord {
        JobRecord {
            state,
            config: None,
            corner: None,
            sample: NO_SAMPLE,
            seed: 0,
            sets: Vec::new(),
            fields: Vec::new(),
        }
    }

    /// Reads attribute line `n` (its values `f`, never empty).
    fn attr(&mut self, n: usize, f: Vec<String>) -> Result<(), String> {
        assert!(!f.is_empty(), "a line has a first value");
        match (f[0].as_str(), f.len()) {
            ("config", 2) => self.config = Some(f[1].clone()),
            ("corner", 2) => self.corner = Some(f[1].clone()),
            ("sample", 2) => {
                self.sample = f[1].parse::<u32>().map_err(|_| bad(n, "bad sample"))?;
            }
            ("seed", 2) => match f[1].parse::<u32>() {
                Ok(s) if s != 0 => self.seed = s,
                _ => return Err(bad(n, "bad seed")),
            },
            ("set", 3) => self.sets.push((f[1].clone(), f[2].clone())),
            _ => self.fields.push(FieldLine { line: n, values: f }),
        }
        Ok(())
    }

    /// Appends the job to `jobs` and its values, verdicts and errors to `r`.
    fn flush(self, jobs: &mut Jobs, r: &mut Results) -> Result<(), String> {
        let sets: Vec<(&str, &str)> = self
            .sets
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let j = jobs
            .push_row(
                self.config.as_deref(),
                self.corner.as_deref(),
                self.sample,
                self.seed,
                &sets,
            )
            .map_err(|e| e.to_string())?;
        r.ensure(j);
        assert_eq!(r.rows(), jobs.len(), "jobs are read in order");
        for FieldLine { line: n, values: f } in self.fields {
            let find = |names: &[String], x: &str| {
                names
                    .iter()
                    .position(|y| y == x)
                    .ok_or_else(|| bad(n, &format!("unknown name `{x}`")))
            };
            match (f[0].as_str(), f.len()) {
                ("var", 3) => {
                    let x = schemify_expr::parse_num(&f[2]).map_err(|e| bad(n, &e.to_string()))?;
                    r.set_var(j, find(r.vars(), &f[1])?, x);
                }
                ("value", 3) => {
                    let x = schemify_expr::parse_num(&f[2]).map_err(|e| bad(n, &e.to_string()))?;
                    r.set_value(j, find(r.outputs(), &f[1])?, x);
                }
                ("pass", 3) => {
                    let o = find(r.outputs(), &f[1])? as u16;
                    let s = r.spec_outputs().iter().position(|&x| x == o);
                    let s = s.ok_or_else(|| bad(n, "no such spec"))?;
                    let p = match f[2].as_str() {
                        "pass" => Pass::Pass,
                        "fail" => Pass::Fail,
                        _ => return Err(bad(n, "expected pass|fail")),
                    };
                    r.set_pass(j, s, p);
                }
                ("error", 3) => {
                    let o = if f[1] == "-" {
                        JOB_LEVEL
                    } else {
                        find(r.outputs(), &f[1])? as u16
                    };
                    r.error(j, o, f[2].clone());
                }
                _ => return Err(bad(n, "unexpected attribute")),
            }
        }
        jobs.set_state(j, self.state);
        assert_eq!(r.rows(), jobs.len());
        Ok(())
    }
}

/// Appends one indented attribute line `  <key> <value>…`.
fn attr(out: &mut String, key: &str, vals: &[&str]) {
    out.push_str("  ");
    out.push_str(key);
    for v in vals {
        out.push(' ');
        text::push_value(out, v);
    }
    out.push('\n');
}

fn bad(n: usize, why: &str) -> String {
    assert!(!why.is_empty(), "an error says what is wrong");
    format!("line {}: {why}", n + 1)
}

/// The values of one line, owned (format spec §2 lexical layer); `None` for a malformed
/// one or a line without values.
fn split_values(line: &str) -> Option<Vec<String>> {
    let v: Vec<String> = text::values(line)
        .map(|v| v.ok().map(|v| v.into_owned()))
        .collect::<Option<_>>()?;
    (!v.is_empty()).then_some(v)
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
            text::push_value(&mut line, v);
            line.push(' ');
        }
        let back = split_values(&line).unwrap();
        assert_eq!(back, vals);
        assert!(split_values("\"open").is_none());
        assert!(split_values("\"bad \\q\"").is_none());
        // Unquoted, a leading `#` or `|` would read as a comment or verbatim line: refused.
        assert!(split_values("#x").is_none());
        assert!(split_values("a |x").is_none());
    }

    /// Names and values starting with `#` or `|` survive a results file.
    #[test]
    fn hash_and_bar_values_round_trip() {
        let hdr = format!("schemify 1 results\n  design {}\n", "0".repeat(64));
        let text = format!(
            "{hdr}\noutput \"#x\"\noutput \"|x\"\n\njob 0 done\n  set R \"#x\"\n  set C \"|x\"\n  value \"#x\" 1\n  value \"|x\" 2\n"
        );
        let (j, r) = read_results(&text).unwrap();
        assert_eq!(j.assigned(0, "R"), Some("#x"));
        assert_eq!(j.assigned(0, "C"), Some("|x"));
        assert_eq!((r.value(0, 0), r.value(0, 1)), (Some(1.0), Some(2.0)));
        assert_eq!(write_results(&j, &r), text);
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
        assert!(
            read_results(&format!("{hdr}\njob 0 done\n\noutput late\n")).is_err(),
            "columns first"
        );
        assert!(read_results(&format!("{hdr}\njob 0 done\n  pass x maybe\n")).is_err());
        let (j, r) = read_results(&format!("{hdr}\njob 0 done\n  set R 1k\n")).unwrap();
        assert_eq!((j.len(), j.assigned(0, "R")), (1, Some("1k")));
        assert!(!r.stale(&[0; 32]));
        assert!(r.stale(&[1; 32]));
    }

    /// Every attribute the writer emits reads back to the same bytes, errors sorted by
    /// output with recording order kept within one output.
    #[test]
    fn every_attribute_round_trips() {
        let hdr = format!("schemify 1 results\n  design {}\n", "ab".repeat(32));
        let text = format!(
            "{hdr}\noutput a\noutput b\n\nspec b\n\nvar R\n\njob 0 failed\n  config c1\n  corner ff\n  sample 2\n  seed 7\n  set R 1k\n  error - \"no raw\"\n\njob 1 done\n  var R 1000\n  value a 1\n  value b 2\n  pass b fail\n  error a x\n  error b y\n  error b z\n"
        );
        let (j, r) = read_results(&text).unwrap();
        assert_eq!(j.seed(0), Some(7));
        assert_eq!(j.state(0), State::Failed);
        assert_eq!(r.pass(1, 0), Pass::Fail);
        assert_eq!(r.var_value(1, 0), Some(1000.0));
        assert_eq!(write_results(&j, &r), text);
    }
}
