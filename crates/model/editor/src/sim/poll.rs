//! The per-frame poll of every run (jobs prepared, runners polled, optimizers stepped),
//! progress and staleness, and loading a run's results back from disk.

use super::{
    BenchRun, Best, HISTORY_MAX, Kind, Polled, RUNS_MAX, SUBMIT_PER_FRAME, Sim, session_name,
};
use crate::platform;
use crate::session::Session;
use crate::spines::{BENCH, Run};
use schemify_doc::{DocHandle, Project};
use schemify_elab::Funcs;
use schemify_run::{Plan, RunnerConfig, State, read_results};
use schemify_wave::format_si;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

impl Sim {
    /// One frame of every run: prepares and submits up to [`SUBMIT_PER_FRAME`] jobs in all,
    /// polls the runners, steps the optimizers, saves a run's results when it goes idle.
    pub fn poll<F: Funcs>(
        &mut self,
        project: &Project,
        env: &[(&str, &str)],
        funcs: &mut F,
    ) -> Polled {
        let mut out = Polled::default();
        let active = self.running().max(1) as u32;
        let budget = (SUBMIT_PER_FRAME / active).max(1);
        let shown = self.shown;
        let mut notes: Vec<String> = Vec::new();
        for r in &mut self.runs {
            let is_shown = Some(r.bench) == shown;
            if project.doc(r.bench).is_none() {
                if !r.idle() {
                    r.run.cancel();
                    notes.push(format!(
                        "{}: simulation stopped, the bench was closed",
                        r.name
                    ));
                }
                continue;
            }
            if r.idle() && r.saved {
                continue;
            }
            if let Kind::Optimize(o, _) = &mut r.kind
                && let Err(e) = o.step(&mut r.run.jobs, &r.run.results)
            {
                notes.push(format!("optimize: {e:?}"));
            }
            let before = r.run.next;
            r.run.submit_next(project, r.bench, env, funcs, budget);
            r.run.poll();
            let changed = r.run.next != before || !r.run.finished.is_empty();
            for &j in &r.run.finished {
                if let Some(&(_, p)) = r.plugin_jobs.iter().find(|x| x.0 == j) {
                    out.run_done.push((p, j));
                    self.snapshot = Some(r.bench);
                }
            }
            r.run.finished.clear();
            // The optimizer tells a finished candidate and asks the next in the same frame.
            if let Kind::Optimize(o, _) = &mut r.kind
                && let Ok(Some(c)) = o.step(&mut r.run.jobs, &r.run.results)
            {
                r.run
                    .submit_next(project, r.bench, env, funcs, c.end - c.start);
            }
            let done = r.idle() && !r.saved;
            if done {
                r.saved = true;
                match r.run.save(&r.name) {
                    Ok(dir) => notes.push(format!("{}: saved {}", r.name, dir.display())),
                    Err(e) => notes.push(format!("{}: results: {e}", r.name)),
                }
                if let Kind::Optimize(o, name) = &r.kind
                    && let Some(best) = o.opt.best()
                    && best.score < f64::MAX
                {
                    let values = o
                        .opt
                        .params()
                        .iter()
                        .zip(best.params)
                        .map(|(p, &v)| {
                            let mut t = String::new();
                            schemify_expr::write_num(&mut t, v);
                            (p.name.clone(), t)
                        })
                        .collect();
                    out.best.push(Best {
                        bench: r.bench,
                        label: format!("optimize {name}: best of {} evaluations", o.opt.n_evals()),
                        values,
                    });
                }
            }
            if is_shown {
                out.changed |= changed || done;
                out.done |= done;
            }
        }
        if out.changed {
            self.table.dirty = true;
        }
        if self.shown_run().is_some() {
            self.status = self.progress();
            if out.changed || self.running() > 0 {
                self.refresh_log();
            }
        }
        for n in notes {
            let _ = write!(self.status, "; {n}");
        }
        assert!(self.runs.len() <= RUNS_MAX);
        out
    }

    /// The status line of the shown run: progress while busy, a summary when done, and how
    /// many other runs are in flight.
    pub fn progress(&self) -> String {
        let Some(br) = self.shown_run() else {
            return String::new();
        };
        let r = &br.run;
        let st = r.jobs.states();
        let n = |s: State| st.iter().filter(|&&x| x == s).count();
        let (done, failed, cancelled) = (n(State::Done), n(State::Failed), n(State::Cancelled));
        let running = n(State::Running);
        let mut t = match &br.kind {
            Kind::Optimize(o, name) => {
                let best = o
                    .opt
                    .best()
                    .map_or("-".to_owned(), |b| format_si(b.score, 4));
                format!(
                    "optimize {name}: {} evaluation(s), best score {best}",
                    o.opt.n_evals()
                )
            }
            _ => format!(
                "simulate: {done}/{} done, {failed} failed, {running} running",
                st.len()
            ),
        };
        if cancelled > 0 {
            let _ = write!(t, ", {cancelled} cancelled");
        }
        let others = self
            .runs
            .iter()
            .filter(|o| o.bench != br.bench && !o.idle())
            .count();
        if others > 0 {
            let _ = write!(t, " (+{others} other run(s))");
        }
        if let Some((_, line)) = r.runner.progress().next()
            && !line.is_empty()
        {
            let _ = write!(t, " — {line}");
        }
        t
    }

    /// Marks the shown results stale when the design changed (run spec §4): recomputes the
    /// design hash of the bench over the run's job list once per edit generation `gen`,
    /// opening the cells it reaches (the canvas draws instances from the library index, so
    /// they may not be open yet). Cost: one elaboration per config of the jobs (plus one
    /// per round of newly opened cells), only after an edit and only while results are shown.
    // ponytail: every edit anywhere re-hashes once; skip edits to documents the bench does
    // not reach if benches get large.
    pub fn check_stale<F: Funcs>(
        &mut self,
        session: &mut Session,
        env: &[(&str, &str)],
        funcs: &mut F,
        generation: u32,
    ) {
        let Some(br) = self.shown_run() else {
            return;
        };
        if self.stale_gen == Some(generation)
            || !br.idle()
            || session.project.doc(br.bench).is_none()
        {
            return;
        }
        // Optimizer candidates and plugin jobs are pushed after the gate: hash the job list
        // the run was started (or loaded) with. Each round opens at least one new cell.
        let mut stale = true;
        for _ in 0..=session.project.cells().lib.len() {
            let (bench, base, dialect) = (br.bench, &br.base, br.run.dialect);
            match schemify_run::design(&session.project, bench, base, env, dialect, funcs) {
                Ok(d) => stale = d != br.run.digest,
                Err(schemify_run::Blocked::NeedsOpen(rows))
                    if rows.iter().all(|&r| session.open_cell(r as usize).is_ok()) =>
                {
                    continue;
                }
                Err(_) => {}
            }
            break;
        }
        self.stale = stale;
        self.stale_gen = Some(generation);
    }

    /// Shows the results of bench `top`: its run in memory, else the newest saved one
    /// (`.schemify/runs/*/bench` naming it). Lists the bench's run directories in
    /// [`Sim::history`]. Returns whether the shown results changed.
    pub fn load_latest(&mut self, session: &Session, top: DocHandle) -> bool {
        let name = session_name(session, top);
        self.history = history(&session.root, &name);
        if self.index_of(top).is_some() {
            let changed = self.shown != Some(top);
            self.show(top);
            return changed;
        }
        let Some(dir) = self.history.first().cloned() else {
            return false;
        };
        self.load_dir(session, top, &dir).is_ok()
    }

    /// Loads the results saved in run directory `dir` for bench `top` and shows them.
    ///
    /// # Errors
    ///
    /// The bench's run is in flight, the bench's plan no longer reads, the results file is
    /// missing or invalid, or [`RUNS_MAX`] runs are in flight.
    pub fn load_dir(
        &mut self,
        session: &Session,
        top: DocHandle,
        dir: &Path,
    ) -> Result<(), String> {
        if self.index_of(top).is_some_and(|i| !self.runs[i].idle()) {
            return Err("a run of this bench is in progress".into());
        }
        self.make_room(top)?;
        let doc = session.project.doc(top).ok_or("the bench is closed")?;
        let plan = Plan::read(doc).map_err(|e| format!("bench: {e:?}"))?;
        let text = platform::read(&dir.join(schemify_run::RESULTS))?;
        let text = String::from_utf8(text).map_err(|e| e.to_string())?;
        let (jobs, results) = read_results(&text)?;
        let digest = *results.design();
        let mut cfg = RunnerConfig::new(&session.root, &digest);
        cfg.dir = dir.to_path_buf();
        cfg.tools = self.tools.clone();
        let run = Run {
            plan,
            next: jobs.len(),
            jobs,
            results,
            runner: schemify_run::Runner::new(cfg),
            digest,
            finished: Vec::new(),
            extra: String::new(),
            // A saved run does not record its simulator: it is current only for the one
            // selected now (the design hash covers the dialect).
            dialect: self.dialect,
        };
        let name = session_name(session, top);
        self.install(BenchRun {
            bench: top,
            name,
            base: run.jobs.clone(),
            run,
            kind: Kind::Loaded,
            plugin_jobs: Vec::new(),
            saved: true,
        });
        self.show(top);
        self.reset_view();
        self.status = format!("results loaded from {}", dir.display());
        Ok(())
    }

    /// The raw file of job `j` of the shown run (`<run dir>/<j>/out.raw`, else the first
    /// `.raw` the job wrote, as the runner reads it).
    pub fn job_raw(&self, j: u32) -> Option<PathBuf> {
        let dir = self.dir()?.join(j.to_string());
        let main = dir.join(schemify_run::RAW);
        if platform::exists(&main) {
            return Some(main);
        }
        let files = platform::list_files(&dir, &[]);
        files
            .into_iter()
            .find(|p| p.extension().is_some_and(|x| x == "raw"))
    }
}

/// Run directories under `<root>/.schemify/runs/` whose `bench` sidecar names `bench`,
/// newest first (by the results file's modification time in seconds, then name).
///
/// Goes through [`platform`], so the web build finds the runs in browser storage too (web
/// spec §6: runs a project brought along, such as CI's, load like local ones).
/// ponytail: lists every file under `runs/` to find the sidecars (bounded by
/// `platform::LIST_FILES_MAX`, so runs past it are not offered); a directory-only listing
/// in `platform` if run trees ever get that large.
pub(super) fn history(root: &Path, bench: &str) -> Vec<PathBuf> {
    let runs = root.join(".schemify").join("runs");
    let mut dirs: Vec<(u64, PathBuf)> = platform::list_files(&runs, &[])
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n == BENCH))
        .filter_map(|p| p.parent().map(Path::to_path_buf))
        .filter(|d| d.parent() == Some(runs.as_path()))
        .filter(|d| {
            platform::read(&d.join(BENCH))
                .is_ok_and(|s| String::from_utf8_lossy(&s).trim() == bench)
                && platform::exists(&d.join(schemify_run::RESULTS))
        })
        .map(|d| {
            let t = platform::mtime_unix_s(&d.join(schemify_run::RESULTS)).unwrap_or(0);
            (t, d)
        })
        .collect();
    dirs.sort_by(|a, b| b.cmp(a));
    dirs.truncate(HISTORY_MAX);
    debug_assert!(dirs.iter().all(|(_, d)| d.starts_with(&runs)));
    dirs.into_iter().map(|(_, d)| d).collect()
}
