//! What the shown run's selected job derives: raw datasets, the run log, docs-page plots, OP
//! back-annotation marks, the plugin `Results` rows, and the results table's sorted, filtered
//! rows.
//!
//! Contents:
//! - `pub(super) fn tail`
//! - `Sim::datasets`
//! - `Sim::select`
//! - `Sim::refresh_log`
//! - `Sim::refresh_plots`
//! - `Sim::op_marks`
//! - `Sim::result_rows`
//! - `Sim::sort_by`
//! - `Sim::set_filter`
//! - `Sim::refresh_table`
//! - `pub(super) fn row_text`
//! - `pub(super) fn plot`
//! - `pub(super) fn op_mark`
//! - `pub(super) fn inst_path`

use std::fmt::Write as _;
use std::path::Path;

use schemify_elab::{AnnotationRequest, Mark, Severity};
use schemify_netlist::NameMap;
use schemify_run::{Jobs, Pass, Results, State};
use schemify_wave::calc::op_dataset;
use schemify_wave::{RawPlot, Request as WaveRequest, Waves, format_si, group_datasets, parse_raw};

use crate::checks::SOURCE_OP;
use crate::platform;

use super::{
    Col, JobLog, Kind, LOG_LINES, LOG_TAIL_BYTES, OP_PARAMS, OVERLAY_JOBS, PLOTS_MAX, Plot, Sim,
};

/// The last [`LOG_LINES`] lines of `path` (at most [`LOG_TAIL_BYTES`] read); empty when
/// the file is missing.
pub(super) fn tail(path: &Path) -> Vec<String> {
    let Ok((buf, cut)) = platform::read_tail(path, LOG_TAIL_BYTES) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    // A cut first line (read from mid-file) is dropped.
    let skip = usize::from(cut && !lines.is_empty());
    let keep = &lines[skip..];
    let out: Vec<String> = keep[keep.len().saturating_sub(LOG_LINES)..]
        .iter()
        .map(|l| (*l).to_owned())
        .collect();
    debug_assert!(out.len() <= LOG_LINES);
    out
}

impl Sim {
    /// The datasets of the selected job's raw file, parsed once per selection; only a
    /// finished job's file is read (a running simulator may be writing it).
    pub(super) fn datasets(&mut self) -> Option<&[RawPlot]> {
        let j = self.selected;
        let done = self
            .tables()
            .is_some_and(|t| j < t.1.len() && t.1.state(j) == State::Done);
        if !done {
            return None;
        }
        let fresh = matches!(&self.raw, Some((k, _)) if *k == j);
        if !fresh {
            let parsed = match self.job_raw(j) {
                Some(p) => platform::read(&p)
                    .and_then(|b| parse_raw(&b).map_err(|e| e.to_string()))
                    .map(group_datasets),
                None => Err(format!("job {j} has no raw file")),
            };
            self.raw = Some((j, parsed));
        }
        match &self.raw {
            Some((_, Ok(d))) => Some(d),
            _ => None,
        }
    }

    /// Selects job `j` for the waves, plots and OP annotation.
    pub fn select(&mut self, j: u32) {
        if self.tables().is_some_and(|t| j < t.1.len()) {
            self.selected = j;
            self.plots.clear();
            self.refresh_log();
        }
    }

    /// Re-reads the selected job's run log ([`JobLog`]): bounded tails of three small files.
    pub(super) fn refresh_log(&mut self) {
        let Some(dir) = self.dir().map(|d| d.join(self.selected.to_string())) else {
            self.log = JobLog::default();
            return;
        };
        self.log = JobLog {
            job: self.selected,
            command: tail(&dir.join(schemify_run::COMMAND)).join(" "),
            out: tail(&dir.join(schemify_run::STDOUT)),
            err: tail(&dir.join(schemify_run::STDERR)),
        };
    }

    /// Rebuilds the plot slots for `exprs` (the shown page's `plot` fences): each a one-pane
    /// viewer with one trace per finished job of the run (sweep points, corners, Monte Carlo
    /// samples; the selected job's first, at most [`OVERLAY_JOBS`]; an optimizer run shows
    /// the selected job only), each on the last dataset of its job that evaluates the
    /// expression, autoscaled. Kept when the expressions and the selection did not change.
    ///
    /// Cost: reads and parses the overlaid jobs' raw files (the selected one is cached).
    pub fn refresh_plots(&mut self, exprs: &[String]) {
        let same = self.plots.len() == exprs.len()
            && self.plots.iter().zip(exprs).all(|(p, e)| p.expr == *e);
        if same {
            return;
        }
        let mut jobs: Vec<Vec<RawPlot>> = Vec::new();
        jobs.extend(self.datasets().map(<[RawPlot]>::to_vec));
        let overlay = !jobs.is_empty()
            && !self
                .shown_run()
                .is_some_and(|r| matches!(r.kind, Kind::Optimize(..)));
        let others: Vec<u32> = match self.tables() {
            Some((_, t, _)) if overlay => (0..t.len())
                .filter(|&j| j != self.selected && t.state(j) == State::Done)
                .take(OVERLAY_JOBS - 1)
                .collect(),
            _ => Vec::new(),
        };
        for j in others {
            let parsed = self.job_raw(j).and_then(|p| platform::read(&p).ok());
            if let Some(d) = parsed.and_then(|b| parse_raw(&b).ok()) {
                jobs.push(group_datasets(d));
            }
        }
        assert!(jobs.len() <= OVERLAY_JOBS);
        self.plots = exprs
            .iter()
            .take(PLOTS_MAX)
            .map(|e| Plot {
                expr: e.clone(),
                waves: if jobs.is_empty() {
                    Err("no results yet: run the bench to fill this graph".into())
                } else {
                    plot(&jobs, e)
                },
            })
            .collect();
        assert!(self.plots.len() <= PLOTS_MAX);
    }

    /// The `op` source's annotation request from the selected job's operating point: the
    /// dataset whose plot name says "operating point" (ngspice), else a one-point DC
    /// dataset (Xyce writes `.op` that way). Node voltages mark nets (`v(<net>)`, or a bare
    /// node name, Xyce's `X1:N` spelled `x1.n` → the net's emitted name, lowercase); device
    /// quantities mark instances through `map` ([`OP_PARAMS`] of `@<dev>[<param>]`, a
    /// branch current `<dev>#branch`, Xyce lead currents `i(<dev>)`, `id(<dev>)`, …). Empty
    /// when the job has no operating point.
    pub fn op_marks(&mut self, map: &NameMap) -> AnnotationRequest {
        let mut marks = Vec::new();
        if let Some(op) = self.datasets().and_then(op_dataset) {
            // An OP dataset has one point and no real scale: column 0 is a vector too.
            for (k, v) in op.vars().iter().enumerate() {
                let Some(&x) = op.col(k).first() else {
                    continue;
                };
                if let Some(m) = op_mark(map, &v.name, x) {
                    marks.push(m);
                }
            }
        }
        AnnotationRequest {
            source: SOURCE_OP.to_owned(),
            marks,
        }
    }

    /// Rows plugins read as the `Results` snapshot: one per finished job and output of the
    /// run that last finished a plugin's job (else the shown run); `pass` is the verdict of
    /// the output's spec (1 pass, 2 fail, 0 unknown or no spec; a spec names one output).
    pub fn result_rows(&self) -> crate::plugins::ResultRows {
        let run = self
            .snapshot
            .and_then(|b| self.index_of(b))
            .map(|i| &self.runs[i])
            .or_else(|| self.shown_run());
        let Some(br) = run else {
            return Vec::new();
        };
        let (plan, jobs, res) = (&br.run.plan, &br.run.jobs, &br.run.results);
        let spec_of: Vec<Option<u16>> = (0..res.outputs().len())
            .map(|o| {
                let s = plan.specs.iter().position(|s| s.output as usize == o)?;
                Some(s as u16)
            })
            .collect();
        let mut rows = Vec::new();
        for j in 0..jobs.len() {
            if !jobs.state(j).finished() {
                continue;
            }
            for (o, name) in res.outputs().iter().enumerate() {
                let value = res.value(j, o as u16).map_or(String::new(), |v| {
                    let mut t = String::new();
                    schemify_expr::write_num(&mut t, v);
                    t
                });
                let pass = match spec_of[o].map(|s| res.pass(j, s)) {
                    Some(Pass::Pass) => 1,
                    Some(Pass::Fail) => 2,
                    _ => 0,
                };
                rows.push((j, name.clone(), value, pass));
            }
        }
        rows
    }

    /// Sorts the table by `col`; the same column again reverses the order. A column the
    /// shown results lack is ignored.
    pub fn sort_by(&mut self, col: Col) {
        let valid = self.tables().is_some_and(|(_, _, res)| match col {
            Col::Output(o) => (o as usize) < res.outputs().len(),
            Col::Spec(s) => (s as usize) < res.specs(),
            _ => true,
        });
        if !valid {
            return;
        }
        if self.table.sort == col {
            self.table.desc = !self.table.desc;
        } else {
            self.table.sort = col;
            self.table.desc = false;
        }
        self.table.dirty = true;
    }

    /// Filters the table to the rows whose text holds every word of `filter`.
    pub fn set_filter(&mut self, filter: &str) {
        let f = filter.trim();
        if f != self.table.filter {
            self.table.filter = f.to_owned();
            self.table.dirty = true;
        }
    }

    /// Rebuilds [`TableView::rows`](super::TableView::rows) when stale: the shown run's jobs whose text matches the
    /// filter, in sort order (ties by job index, so the order is total and stable).
    ///
    /// Cost: O(jobs · (outputs + specs)) text building plus an O(rows log rows) sort, only
    /// after a change.
    pub fn refresh_table(&mut self) {
        if !self.table.dirty {
            return;
        }
        self.table.dirty = false;
        let mut rows = Vec::new();
        if let Some(br) = self.shown_run() {
            let (jobs, res) = (&br.run.jobs, &br.run.results);
            let words: Vec<String> = self
                .table
                .filter
                .split_whitespace()
                .map(str::to_ascii_lowercase)
                .collect();
            let mut text = String::new();
            for j in 0..jobs.len() {
                if !words.is_empty() {
                    text.clear();
                    row_text(jobs, res, j, &mut text);
                    if !words.iter().all(|w| text.contains(w.as_str())) {
                        continue;
                    }
                }
                rows.push(j);
            }
            let rank = |p: Pass| match p {
                Pass::Fail => 0,
                Pass::Unknown => 1,
                Pass::Pass => 2,
            };
            let mut key = String::new();
            let mut keys: Vec<String> = Vec::new();
            if self.table.sort == Col::Key {
                keys = rows
                    .iter()
                    .map(|&j| {
                        key.clear();
                        jobs.write_key(j, &mut key);
                        key.clone()
                    })
                    .collect();
            }
            let mut order: Vec<usize> = (0..rows.len()).collect();
            let sort = self.table.sort;
            order.sort_by(|&a, &b| {
                let (ja, jb) = (rows[a], rows[b]);
                let primary = match sort {
                    Col::Job => ja.cmp(&jb),
                    Col::Key => keys[a].cmp(&keys[b]),
                    Col::State => (jobs.state(ja) as u8).cmp(&(jobs.state(jb) as u8)),
                    Col::Output(o) => match (res.value(ja, o), res.value(jb, o)) {
                        (Some(x), Some(y)) => x.total_cmp(&y),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => std::cmp::Ordering::Equal,
                    },
                    Col::Spec(s) => rank(res.pass(ja, s)).cmp(&rank(res.pass(jb, s))),
                };
                primary.then(ja.cmp(&jb))
            });
            let mut sorted: Vec<u32> = order.iter().map(|&k| rows[k]).collect();
            if self.table.desc {
                sorted.reverse();
            }
            rows = sorted;
        }
        assert!(rows.windows(2).all(|w| w[0] != w[1]));
        self.table.rows = rows;
    }
}

/// The text the table filter matches for job `j`: index, key line, state and verdicts,
/// lowercase.
pub(super) fn row_text(jobs: &Jobs, res: &Results, j: u32, out: &mut String) {
    let _ = write!(out, "{j} ");
    jobs.write_key(j, out);
    out.push(' ');
    out.push_str(jobs.state(j).word());
    for s in 0..res.specs() as u16 {
        match res.pass(j, s) {
            Pass::Pass => out.push_str(" pass"),
            Pass::Fail => out.push_str(" fail"),
            Pass::Unknown => {}
        }
    }
    out.make_ascii_lowercase();
}

/// A one-pane viewer over the datasets of `jobs` (each a raw file's grouped datasets, the
/// first the selected job's) showing `expr` once per job, on the last dataset of that job
/// that evaluates it. A scalar answer of the first job explains an empty slot instead.
pub(super) fn plot(jobs: &[Vec<RawPlot>], expr: &str) -> Result<Waves, String> {
    let mut data: Vec<RawPlot> = Vec::new();
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::with_capacity(jobs.len());
    for job in jobs {
        // The viewer's cap (its dataset indices are u8).
        if data.len() + job.len() > schemify_wave::MAX_DATASETS {
            break;
        }
        ranges.push(data.len()..data.len() + job.len());
        data.extend(job.iter().cloned());
    }
    let mut last = String::from("no dataset");
    // A refused request changes nothing, so one viewer serves every attempt.
    let mut w = Waves::new(data);
    let mut traces = 0usize;
    for (k, range) in ranges.into_iter().enumerate() {
        for d in range.rev() {
            let req = WaveRequest::AddTrace {
                src: expr.to_owned(),
                dataset: d as u8,
                pane: 0,
            };
            match w.apply(&req) {
                Ok(schemify_wave::Applied::Trace(_)) => {
                    traces += 1;
                    break;
                }
                Ok(schemify_wave::Applied::Value(v)) if k == 0 => {
                    return Err(format!("{expr} = {v}"));
                }
                Ok(schemify_wave::Applied::Value(_)) => break,
                Ok(schemify_wave::Applied::Done) => {}
                Err(e) => last = e.to_string(),
            }
        }
    }
    if traces == 0 {
        return Err(format!("{expr}: {last}"));
    }
    let _ = w.apply(&WaveRequest::Autoscale);
    debug_assert_eq!(w.trace_count(), traces);
    Ok(w)
}

/// One OP mark from raw vector `name` = `x`, ngspice or Xyce spelling (case-insensitive,
/// Xyce's `:` hierarchy separator read as `.`): `v(net)` or a bare node name on the net;
/// `@dev[p]` (also as ngspice writes saved ones, `i(@dev[p])`, `v(@dev[p])`) with `p` in
/// [`OP_PARAMS`], `<dev>#branch` (`i`) and lead currents
/// `i(dev)`, `id(dev)`, `ig(dev)`, `is(dev)`, `ib(dev)`, `ic(dev)`, `ie(dev)` on the device's
/// instance path.
pub(super) fn op_mark(map: &NameMap, name: &str, x: f64) -> Option<Mark> {
    let mut lower = name.to_ascii_lowercase().replace(':', ".");
    // A saved device vector comes wrapped by its kind: `i(@m1[id])`, `v(@m1[vgs])`.
    if let Some(inner) = lower
        .strip_prefix("i(@")
        .or_else(|| lower.strip_prefix("v(@"))
        .and_then(|r| r.strip_suffix(')'))
    {
        lower = format!("@{inner}");
    }
    let dev_mark = |dev: &str, param: &str| {
        Some(Mark {
            target: inst_path(map, dev)?,
            severity: Severity::Info,
            text: format!("{param}={}", format_si(x, 4)),
        })
    };
    if let Some(dev) = lower.strip_prefix('@') {
        let (dev, param) = dev.split_once('[')?;
        let param = param.strip_suffix(']')?;
        if !OP_PARAMS.contains(&param) {
            return None;
        }
        return dev_mark(dev, param);
    }
    if let Some(dev) = lower.strip_suffix("#branch") {
        return dev_mark(dev, "i");
    }
    if let Some((f, rest)) = lower.split_once('(')
        && let Some(arg) = rest.strip_suffix(')')
        && f != "v"
    {
        let lead = matches!(f, "i" | "id" | "ig" | "is" | "ib" | "ic" | "ie");
        return if lead { dev_mark(arg, f) } else { None };
    }
    let net = match lower.strip_prefix("v(").and_then(|r| r.strip_suffix(')')) {
        Some(n) => n,
        None if !lower.contains(['(', '#', '@']) => lower.as_str(),
        None => return None,
    };
    map.net_path(net)?;
    Some(Mark {
        target: net.to_owned(),
        severity: Severity::Info,
        text: format!("{}V", format_si(x, 4)),
    })
}

/// The instance path of an ngspice device name: the type letter and dot (`m.`) are dropped,
/// then the longest dotted prefix the name map knows wins (`x1.mm2` → `/x1/M2`;
/// `xm1.msky130_fd_pr__nfet_01v8` → `/M1`, the model subckt's inner device).
pub(super) fn inst_path(map: &NameMap, dev: &str) -> Option<String> {
    let body = match dev.split_once('.') {
        Some((t, rest)) if t.len() == 1 => rest,
        _ => dev,
    };
    let mut end = body.len();
    loop {
        if let Some(p) = map.inst_path(&body[..end]) {
            return Some(p);
        }
        end = body[..end].rfind('.')?;
    }
}
