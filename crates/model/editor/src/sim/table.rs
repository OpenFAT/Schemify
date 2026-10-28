//! The results table as UI records: header buttons, the windowed job rows and their cells.
//!
//! Contents:
//! - `Sim::contribute`

use std::fmt::Write as _;

use schemify_run::{JOB_LEVEL, Jobs, NO_SAMPLE, Pass, Results, State};
use schemify_ui::records;

use crate::docs::quote as q;

use super::{Col, HISTORY_MAX, Kind, ROW_PX, Sim, TABLE_WINDOW};

impl Sim {
    /// Appends the results panel to the state contribution `t` (nodes under `bottom`): a bar
    /// with the run summary, a stale marker, the filter field and buttons; the table header
    /// (buttons that sort); the Monte Carlo yield and the optimizer's best point as rows; the
    /// job rows of [`TableView::rows`](super::TableView::rows) near the list's scroll offset `scroll_px` (at most
    /// [`TABLE_WINDOW`], spacers standing in for the rest; each row's button picks the job);
    /// error rows; the bench's earlier runs.
    pub fn contribute(&self, t: &mut String, scroll_px: f32) {
        let _ = writeln!(
            t,
            "node results.pane panel\n  parent bottom\n  style bottom-pane"
        );
        let _ = writeln!(
            t,
            "node results.bar panel\n  parent results.pane\n  style bottom-bar"
        );
        let title = match self.shown_run() {
            None => "Results: nothing simulated yet (s on a .shbench)".to_owned(),
            Some(br) => {
                let mut s = format!("Results: {}  ", br.name);
                s.push_str(&self.progress());
                if self.stale {
                    s.push_str("  [stale: the design changed since this run]");
                }
                s
            }
        };
        let _ = writeln!(
            t,
            "node results.title label\n  parent results.bar\n  label {}\n  style bottom-title",
            q(&title)
        );
        let _ = writeln!(
            t,
            "node results.filter text_input\n  parent results.bar\n  value {}\n  command results.filter\n  tooltip {}",
            q(&self.table.filter),
            q("Filter rows (Enter): every word must appear in the job, key, state or verdicts")
        );
        for (id, label, cmd) in [
            ("results.waves", "Waves", "results.waves"),
            ("results.op", "OP", "waves.op_annotate"),
            ("results.cancel", "Cancel", "sim.cancel"),
            ("results.close", "x", "view.results"),
        ] {
            let _ = writeln!(
                t,
                "node {id} button\n  parent results.bar\n  label {label}\n  command {cmd}"
            );
        }
        let _ = writeln!(
            t,
            "node results.list scroll\n  parent results.pane\n  style bottom-list"
        );
        let Some(br) = self.shown_run() else { return };
        let (plan, jobs, res) = (&br.run.plan, &br.run.jobs, &br.run.results);
        let row = |t: &mut String, id: &str| {
            let _ = writeln!(
                t,
                "node {id} panel\n  parent results.list\n  style results-row"
            );
        };
        let cell = |t: &mut String, id: &str, parent: &str, text: &str, style: &str| {
            let _ = writeln!(
                t,
                "node {id} label\n  parent {parent}\n  label {}\n  style {style}",
                q(text)
            );
        };
        // Header: one sort button per column, the sorted one marked.
        row(t, "results.head");
        let mark = |c: Col| match (self.table.sort == c, self.table.desc) {
            (true, false) => " ^",
            (true, true) => " v",
            (false, _) => "",
        };
        let head = |t: &mut String, id: &str, text: &str, style: &str, c: Col| {
            let _ = writeln!(
                t,
                "node results.head.{id} button\n  parent results.head\n  label {}\n  command results.sort\n  style {style}",
                q(&format!("{text}{}", mark(c)))
            );
        };
        head(t, "job", "job", "results-key", Col::Job);
        head(
            t,
            "key",
            "config corner sample vars",
            "results-wide",
            Col::Key,
        );
        head(t, "state", "state", "results-cell", Col::State);
        for (o, name) in res.outputs().iter().enumerate() {
            head(
                t,
                &format!("o{o}"),
                name,
                "results-cell",
                Col::Output(o as u16),
            );
        }
        for s in 0..res.specs() {
            let name = &res.outputs()[plan.specs[s].output as usize];
            head(
                t,
                &format!("s{s}"),
                &format!("spec {name}"),
                "results-cell",
                Col::Spec(s as u16),
            );
        }
        let mut extra_rows = 0u32;
        // Monte Carlo yield: a row whose spec cells hold each spec's yield (95% Wilson CI)
        // and whose output cells hold the sample mean.
        let mc = (0..jobs.len()).any(|j| jobs.sample(j) != NO_SAMPLE);
        if mc && !plan.specs.is_empty() {
            extra_rows += 1;
            let rid = "results.yield";
            row(t, rid);
            cell(t, &format!("{rid}.job"), rid, "MC", "results-key");
            let ys = schemify_run::yield_summary(plan, jobs, res);
            let total = ys.first().map_or(0, |y| y.total);
            cell(
                t,
                &format!("{rid}.key"),
                rid,
                &format!("yield over {total} sample(s), 95% Wilson CI"),
                "results-wide",
            );
            cell(t, &format!("{rid}.state"), rid, "-", "results-dim");
            for o in 0..res.outputs().len() as u16 {
                let vals: Vec<f64> = (0..jobs.len())
                    .filter(|&j| jobs.sample(j) != NO_SAMPLE)
                    .filter_map(|j| res.value(j, o))
                    .collect();
                let text = if vals.is_empty() {
                    "-".to_owned()
                } else {
                    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
                    format!("μ {}", schemify_wave::format_si(mean, 4))
                };
                cell(t, &format!("{rid}.o{o}"), rid, &text, "results-cell");
            }
            for y in &ys {
                let pct = if y.total == 0 {
                    0.0
                } else {
                    100.0 * f64::from(y.pass) / f64::from(y.total)
                };
                let text = format!("{pct:.0}% ({:.0}–{:.0})", 100.0 * y.lo, 100.0 * y.hi);
                cell(
                    t,
                    &format!("{rid}.s{}", y.spec),
                    rid,
                    &text,
                    "results-yield",
                );
            }
        }
        // The optimizer's best point: its job's row, the varied values in the key cell.
        if let Kind::Optimize(o, name) = &br.kind
            && let (Some(b), Some(j)) = (o.opt.best(), o.best_job())
        {
            extra_rows += 1;
            let rid = "results.best";
            row(t, rid);
            let _ = writeln!(
                t,
                "node results.best.{j} button\n  parent {rid}\n  label best\n  command results.pick\n  style results-pick"
            );
            let mut key = format!("{name}: score {} at", schemify_wave::format_si(b.score, 4));
            for (p, v) in o.opt.params().iter().zip(b.params) {
                let _ = write!(key, " {}={}", p.name, schemify_wave::format_si(*v, 4));
            }
            cell(t, &format!("{rid}.key"), rid, &key, "results-yield");
            self.job_cells(t, rid, jobs, res, j);
        }
        // Job rows near the scroll offset; spacers keep the list's full height.
        let shown = self.table.rows.len() as u32;
        let data_px = scroll_px - ROW_PX * (1 + extra_rows) as f32;
        let (start, end) = records::window(data_px, ROW_PX, shown, TABLE_WINDOW);
        // The list's gap (1) follows every row, so a row's pitch is ROW_PX.
        let gap = |t: &mut String, id: &str, rows: u32| {
            records::spacer(t, "results.list", id, rows * ROW_PX as u32, 1);
        };
        gap(t, "results.gap-top", start);
        let mut key = String::new();
        for &j in &self.table.rows[start as usize..end as usize] {
            let rid = format!("results.r.{j}");
            row(t, &rid);
            let style = if j == self.selected {
                "results-pick-sel"
            } else {
                "results-pick"
            };
            let _ = writeln!(
                t,
                "node results.row.{j} button\n  parent {rid}\n  label {j}\n  command results.pick\n  style {style}"
            );
            key.clear();
            jobs.write_key(j, &mut key);
            cell(t, &format!("{rid}.key"), &rid, &key, "results-wide");
            self.job_cells(t, &rid, jobs, res, j);
        }
        gap(t, "results.gap-bottom", shown - end);
        if shown < jobs.len() {
            let more = format!("{} of {} job(s) match the filter", shown, jobs.len());
            cell(t, "results.filtered", "results.list", &more, "results-dim");
        }
        // The run log of the selected job: what ran and the tail of what it printed.
        let log = &self.log;
        if !log.command.is_empty() || !log.out.is_empty() || !log.err.is_empty() {
            let head = format!("job {} log", log.job);
            cell(t, "results.log.title", "results.list", &head, "results-key");
            if !log.command.is_empty() {
                let cmd = format!("$ {}", log.command);
                cell(t, "results.log.cmd", "results.list", &cmd, "results-log");
            }
            for (i, l) in log.out.iter().enumerate() {
                cell(
                    t,
                    &format!("results.log.out.{i}"),
                    "results.list",
                    l,
                    "results-log",
                );
            }
            for (i, l) in log.err.iter().enumerate() {
                cell(
                    t,
                    &format!("results.log.err.{i}"),
                    "results.list",
                    l,
                    "results-fail",
                );
            }
        }
        let first_errors = res.errors().take(8);
        for (e, (j, o, msg)) in first_errors.enumerate() {
            let what = if o == JOB_LEVEL {
                format!("job {j}: {msg}")
            } else {
                format!("job {j} {}: {msg}", res.outputs()[o as usize])
            };
            cell(
                t,
                &format!("results.err.{e}"),
                "results.list",
                &what,
                "results-fail",
            );
        }
        let shown = &self.history[..self.history.len().min(HISTORY_MAX)];
        for (i, d) in shown.iter().enumerate() {
            let label = format!(
                "run {}",
                d.file_name()
                    .map_or(String::new(), |n| n.to_string_lossy().into_owned())
            );
            let _ = writeln!(
                t,
                "node results.hist.{i} button\n  parent results.list\n  label {}\n  command results.history",
                q(&label)
            );
        }
    }

    /// The state, output and spec cells of job `j` in row `rid`.
    fn job_cells(&self, t: &mut String, rid: &str, jobs: &Jobs, res: &Results, j: u32) {
        let cell = |t: &mut String, id: String, text: &str, style: &str| {
            let _ = writeln!(
                t,
                "node {id} label\n  parent {rid}\n  label {}\n  style {style}",
                q(text)
            );
        };
        let st = jobs.state(j);
        let st_style = match st {
            State::Done => "results-cell",
            State::Failed => "results-fail",
            _ => "results-dim",
        };
        cell(t, format!("{rid}.state"), st.word(), st_style);
        for o in 0..res.outputs().len() as u16 {
            let v = res
                .value(j, o)
                .map_or("-".to_owned(), |x| schemify_wave::format_si(x, 4));
            cell(t, format!("{rid}.o{o}"), &v, "results-cell");
        }
        for s in 0..res.specs() as u16 {
            let (text, style) = match res.pass(j, s) {
                Pass::Pass => ("pass", "results-pass"),
                Pass::Fail => ("fail", "results-fail"),
                Pass::Unknown => ("-", "results-dim"),
            };
            cell(t, format!("{rid}.s{s}"), text, style);
        }
    }
}
