//! `sim`: the run spine inside the editor (D15–D18, D23, run spec). One [`Sim`] holds the
//! run in flight or the last one finished or reloaded, and everything derived from it that
//! the editor shows: the results table, the docs-page plot slots, the OP back-annotation and
//! the optimizer's best values.
//!
//! Non-blocking (D39): a run is started by a command (gate + design hash, no job prepared),
//! then every frame [`Sim::poll`] prepares at most [`SUBMIT_PER_FRAME`] jobs, polls the
//! runner (`run::Runner::poll`, no waits) and steps the optimizer. While anything runs
//! [`Sim::wake_ms`] asks the window for a tick every [`POLL_MS`]; when nothing runs it asks
//! for none, so the editor sleeps.
//!
//! Plugin `run` requests (plugin-abi §7) join the same queue: a request for the bench of the
//! current run adds a job to it; another bench waits until the current run is idle.
//!
//! Data design:
//! 1. In: a bench document, a selection (every corner and sweep, the first Monte Carlo
//!    record), plugin run requests, the runner's finished jobs. Out: jobs/results (owner
//!    `run`), the results file (`.schemify/runs/<design>/results` + `bench`), panel records,
//!    plot columns, `op` annotation marks, a staged edit.
//! 2. How many: jobs ≤ 2^20; the panel shows ≤ [`TABLE_ROWS`] job rows; plot slots ≤
//!    [`PLOTS_MAX`]; queued plugin requests ≤ [`QUEUE_MAX`]; history ≤ [`HISTORY_MAX`].
//! 3. Widths: job indices `u32` (run's), output/spec columns `u16`.
//! 4. Access: per frame while running: the runner's children and the finished list; on
//!    completion or job selection: one raw file parse.
//! 5. Lifetime: until the next run on any bench (one run at a time, like xschem).
//! 6. Parallel: the simulators run as child processes (`workers` at once); the editor side is
//!    serial.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use schemify_doc::{DocHandle, Project};
use schemify_elab::{AnnotationRequest, Funcs, Mark, Severity};
use schemify_netlist::NameMap;
use schemify_plugin::{PluginId, RunRequest};
use schemify_run::{
    JOB_LEVEL, NO_SAMPLE, OptRun, Pass, Plan, Results, RunnerConfig, Select, SimCommand, State,
    read_results, yield_summary,
};
use schemify_wave::{RawPlot, Request as WaveRequest, Waves, format_si, group_datasets, parse_raw};

use crate::checks::SOURCE_OP;
use crate::docs::quote as q;
use crate::platform;
use crate::session::Session;
use crate::spines::{self, BENCH, Run};

/// Tick period while a run is in flight.
pub const POLL_MS: u32 = 25;
/// Jobs prepared (elaborated + netlisted) per frame: bounds the frame cost of a large run.
pub const SUBMIT_PER_FRAME: u32 = 8;
/// Job rows the results table shows.
pub const TABLE_ROWS: usize = 64;
/// Plot slots with data at once.
pub const PLOTS_MAX: usize = 64;
/// Plugin run requests waiting for another bench's run to finish.
pub const QUEUE_MAX: usize = 256;
/// Earlier runs of a bench listed in the results panel.
pub const HISTORY_MAX: usize = 8;
/// Device OP quantities back-annotated on instances (ngspice `@dev[param]` vectors).
pub const OP_PARAMS: [&str; 8] = ["id", "gm", "gds", "vds", "vgs", "vth", "vdsat", "i"];

/// What the run does.
#[derive(Debug)]
enum Kind {
    /// Simulate the bench's plan.
    Simulate,
    /// Run an `optimize` record (D18): one candidate job at a time.
    Optimize(Box<OptRun>, String),
    /// Results loaded from disk (no children).
    Loaded,
}

/// A docs-page plot slot's data: the expression and its trace over the selected job.
#[derive(Debug)]
pub struct Plot {
    /// Calculator expression of the fence.
    pub expr: String,
    /// A one-pane viewer holding the trace (autoscaled); `Err` explains an empty slot.
    pub waves: Result<Waves, String>,
}

/// The optimizer's result, for the editor to stage (D18: a staged edit the user reviews).
#[derive(Clone, Debug, PartialEq)]
pub struct Best {
    /// Bench document.
    pub bench: DocHandle,
    /// Transaction label.
    pub label: String,
    /// `(var, canonical number text)` per varied var.
    pub values: Vec<(String, String)>,
}

/// What one [`Sim::poll`] produced for the rest of the frame.
#[derive(Debug, Default)]
pub struct Polled {
    /// Some job finished or was submitted (the table, plots and OP change).
    pub changed: bool,
    /// The run became idle this poll (results saved).
    pub done: bool,
    /// `(plugin, job)` per finished job a plugin asked for: deliver `RunDone`.
    pub run_done: Vec<(PluginId, u32)>,
    /// The optimizer finished: its best values.
    pub best: Option<Best>,
}

/// The editor's run state.
pub struct Sim {
    sim: Option<SimCommand>,
    run: Option<Run>,
    kind: Kind,
    bench: Option<DocHandle>,
    bench_name: String,
    plugin_jobs: Vec<(u32, PluginId)>,
    queue: Vec<RunRequest>,
    /// The job list the run was gated and hashed with (its stale check rehashes it).
    base: schemify_run::Jobs,
    /// The job whose raw file the waves, plots and OP annotation show.
    pub selected: u32,
    /// Results belong to another design than the documents now open (an edit since).
    pub stale: bool,
    stale_gen: Option<u32>,
    /// Earlier run directories of the bench, newest first.
    pub history: Vec<PathBuf>,
    raw: Option<(u32, Result<Vec<RawPlot>, String>)>,
    /// Plot slots of the shown page.
    pub plots: Vec<Plot>,
    /// Status line of the run (progress, completion, failure).
    pub status: String,
    saved: bool,
}

impl std::fmt::Debug for Sim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sim")
            .field("run", &self.run)
            .field("selected", &self.selected)
            .finish()
    }
}

impl Sim {
    /// An idle run state. `sim` overrides the simulator command line (tests use a fake
    /// one; `None` = `ngspice -b -r {raw} {netlist}`, D15).
    pub fn new(sim: Option<SimCommand>) -> Sim {
        Sim {
            sim,
            run: None,
            kind: Kind::Loaded,
            bench: None,
            bench_name: String::new(),
            plugin_jobs: Vec::new(),
            queue: Vec::new(),
            base: schemify_run::Jobs::new(),
            selected: 0,
            stale: false,
            stale_gen: None,
            history: Vec::new(),
            raw: None,
            plots: Vec::new(),
            status: String::new(),
            saved: true,
        }
    }

    /// Whether a run is in flight or plugin requests wait.
    pub fn busy(&self) -> bool {
        self.run.as_ref().is_some_and(|r| !self.run_idle(r)) || !self.queue.is_empty()
    }

    fn run_idle(&self, r: &Run) -> bool {
        let opt_done = match &self.kind {
            Kind::Optimize(o, _) => o.done(),
            _ => true,
        };
        r.idle() && opt_done
    }

    /// Milliseconds until the next poll: [`POLL_MS`] while busy, `None` when idle (D39).
    pub fn wake_ms(&self) -> Option<u32> {
        self.busy().then_some(POLL_MS)
    }

    /// The bench of the current results.
    pub fn bench(&self) -> Option<DocHandle> {
        self.bench
    }

    /// Plan, jobs and results of the current run, if any.
    pub fn tables(&self) -> Option<(&Plan, &schemify_run::Jobs, &Results)> {
        self.run.as_ref().map(|r| (&r.plan, &r.jobs, &r.results))
    }

    /// The run directory of the current results.
    pub fn dir(&self) -> Option<&Path> {
        self.run.as_ref().map(|r| r.runner.config().dir.as_path())
    }

    /// Starts a run of `top` (a `.shbench`): Simulate (every corner and sweep, the first
    /// Monte Carlo record) or, with `optimize`, the `optimize` record of that name (the first
    /// when empty). Refused while another run is in flight. Returns the status line.
    ///
    /// # Errors
    ///
    /// Busy, a cell cannot be opened, the plan or selection is refused, or the gate blocks
    /// (the message names the blocking violations).
    pub fn start<F: Funcs>(
        &mut self,
        session: &mut Session,
        top: DocHandle,
        optimize: Option<&str>,
        env: &[(&str, &str)],
        funcs: &mut F,
    ) -> Result<String, String> {
        if self.run.as_ref().is_some_and(|r| !self.run_idle(r)) {
            return Err("a run is in progress (Simulation → Cancel simulation)".into());
        }
        let doc = session.project.doc(top).ok_or("the bench is closed")?;
        let plan = Plan::read(doc).map_err(|e| format!("bench: {e:?}"))?;
        let opt_name = match optimize {
            None => None,
            Some(n) if !n.is_empty() => Some(n.to_owned()),
            Some(_) => Some(
                plan.optimizes
                    .first()
                    .map(|o| o.name.clone())
                    .ok_or("the bench has no optimize record")?,
            ),
        };
        let corners: Vec<&str> = if opt_name.is_some() {
            Vec::new()
        } else {
            plan.corners.iter().map(|c| c.name.as_str()).collect()
        };
        let sweeps: Vec<&str> = if opt_name.is_some() {
            Vec::new()
        } else {
            plan.sweeps.iter().map(|s| s.name.as_str()).collect()
        };
        let mc = if opt_name.is_some() {
            None
        } else {
            plan.montecarlos.first().map(|m| m.name.as_str())
        };
        let select = Select {
            corners: &corners,
            sweeps: &sweeps,
            montecarlo: mc,
            ..Select::default()
        };
        // The optimizer pushes its own candidates: no plan selection.
        let select = opt_name.is_none().then_some(&select);
        let run = spines::start_run(session, top, select, env, funcs, self.config(session))?;
        let kind = match &opt_name {
            Some(n) => {
                let o = OptRun::new(&run.plan, n).map_err(|e| format!("optimize: {e:?}"))?;
                Kind::Optimize(Box::new(o), n.clone())
            }
            None => Kind::Simulate,
        };
        let jobs = run.jobs.len();
        self.reset(top, session_name(session, top), run, kind);
        Ok(match opt_name {
            Some(n) => format!("optimize {n}: started"),
            None => format!("simulate: {jobs} job(s) queued"),
        })
    }

    /// The runner configuration for a run of `session`'s project: the run directory under
    /// its root, every library's directory, the simulator override.
    fn config(
        &self,
        session: &Session,
    ) -> impl FnOnce(&schemify_doc::Digest) -> RunnerConfig + use<> {
        let root = session.root.clone();
        let lib_dirs = session.lib_dirs();
        let sim = self.sim.clone();
        move |d| {
            let mut c = RunnerConfig::new(&root, d);
            c.lib_dirs = lib_dirs;
            if let Some(s) = sim {
                c.tools.sim = s;
            }
            c
        }
    }

    fn reset(&mut self, top: DocHandle, name: String, run: Run, kind: Kind) {
        self.base = run.jobs.clone();
        self.run = Some(run);
        self.kind = kind;
        self.bench = Some(top);
        self.bench_name = name;
        self.plugin_jobs.clear();
        self.selected = 0;
        self.stale = false;
        self.stale_gen = None;
        self.raw = None;
        self.plots.clear();
        self.saved = false;
    }

    /// Cancels the run in flight (children are killed, unprepared jobs cancelled) and drops
    /// the queued plugin requests. Returns whether anything was running.
    pub fn cancel(&mut self) -> bool {
        let was = self.busy();
        if let Some(r) = self.run.as_mut() {
            r.cancel();
        }
        if let Kind::Optimize(..) = self.kind {
            self.kind = Kind::Simulate; // the stopped search offers nothing to stage
        }
        self.queue.clear();
        assert!(!self.busy());
        was
    }

    /// Enqueues plugin run requests (bounded by [`QUEUE_MAX`]; the rest are refused with a
    /// status note).
    pub fn enqueue(&mut self, reqs: Vec<RunRequest>) {
        for r in reqs {
            if self.queue.len() >= QUEUE_MAX {
                self.status = "plugin run queue full: request dropped".into();
                break;
            }
            self.queue.push(r);
        }
        assert!(self.queue.len() <= QUEUE_MAX);
    }

    /// Serves queued plugin requests: a request for the current run's bench becomes a job of
    /// it; another bench starts a fresh run (no selection) once the current one is idle.
    pub fn serve_queue<F: Funcs>(
        &mut self,
        session: &mut Session,
        env: &[(&str, &str)],
        funcs: &mut F,
    ) {
        while let Some(req) = self.queue.first().cloned() {
            let Some(row) = find_cell(&session.project, &req.bench) else {
                self.queue.remove(0);
                self.status = format!("plugin run: no bench {}", req.bench);
                continue;
            };
            let top = match session.open_cell(row) {
                Ok(h) => h,
                Err(e) => {
                    self.queue.remove(0);
                    self.status = e;
                    continue;
                }
            };
            if self.bench != Some(top) || self.run.is_none() || self.stale {
                if self.busy_run() {
                    return; // the other bench's run finishes first
                }
                let cfg = self.config(session);
                match spines::start_run(session, top, None, env, funcs, cfg) {
                    Ok(run) => {
                        self.reset(top, session_name(session, top), run, Kind::Simulate);
                    }
                    Err(e) => {
                        self.queue.remove(0);
                        self.status = format!("plugin run: {e}");
                        continue;
                    }
                }
            }
            let run = self.run.as_mut().expect("a run of this bench");
            let opt = |s: &str| (!s.is_empty()).then(|| s.to_owned());
            let (config, corner) = (opt(&req.config), opt(&req.corner));
            let vary: Vec<(&str, &str)> = req
                .vary
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            match run
                .jobs
                .push(config.as_deref(), corner.as_deref(), NO_SAMPLE, &vary)
            {
                Ok(j) => {
                    self.plugin_jobs.push((j, req.plugin));
                    self.saved = false;
                }
                Err(e) => self.status = format!("plugin run: {e:?}"),
            }
            self.queue.remove(0);
        }
    }

    fn busy_run(&self) -> bool {
        self.run.as_ref().is_some_and(|r| !self.run_idle(r))
    }

    /// One frame of the run: prepares and submits up to [`SUBMIT_PER_FRAME`] jobs, polls the
    /// runner, steps the optimizer, saves the results when the run goes idle.
    pub fn poll<F: Funcs>(
        &mut self,
        project: &Project,
        env: &[(&str, &str)],
        funcs: &mut F,
    ) -> Polled {
        let mut out = Polled::default();
        let (Some(run), Some(top)) = (self.run.as_mut(), self.bench) else {
            return out;
        };
        if project.doc(top).is_none() {
            run.cancel();
            self.status = "simulation stopped: the bench was closed".into();
            return out;
        }
        if let Kind::Optimize(o, _) = &mut self.kind {
            match o.step(&mut run.jobs, &run.results) {
                Ok(_) => {}
                Err(e) => self.status = format!("optimize: {e:?}"),
            }
        }
        let before = run.next;
        run.submit_next(project, top, env, funcs, SUBMIT_PER_FRAME);
        run.poll();
        out.changed = run.next != before || !run.finished.is_empty();
        for &j in &run.finished {
            if let Some(&(_, p)) = self.plugin_jobs.iter().find(|x| x.0 == j) {
                out.run_done.push((p, j));
            }
        }
        run.finished.clear();
        // The optimizer tells a finished candidate and asks the next in the same frame.
        if let Kind::Optimize(o, _) = &mut self.kind
            && let Ok(Some(_)) = o.step(&mut run.jobs, &run.results)
        {
            run.submit_next(project, top, env, funcs, 1);
        }
        let idle = self.run.as_ref().is_some_and(|r| self.run_idle(r));
        self.status = self.progress();
        if idle && !self.saved {
            self.saved = true;
            out.done = true;
            out.changed = true;
            let r = self.run.as_ref().expect("checked");
            match r.save(&self.bench_name) {
                Ok(dir) => {
                    let _ = write!(self.status, "; saved {}", dir.display());
                }
                Err(e) => self.status = format!("results: {e}"),
            }
            if let Kind::Optimize(o, name) = &self.kind
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
                out.best = Some(Best {
                    bench: top,
                    label: format!("optimize {name}: best of {} evaluations", o.opt.n_evals()),
                    values,
                });
            }
        }
        out
    }

    /// The status line of the run: progress while busy, a summary when done.
    pub fn progress(&self) -> String {
        let Some(r) = &self.run else {
            return String::new();
        };
        let st = r.jobs.states();
        let n = |s: State| st.iter().filter(|&&x| x == s).count();
        let (done, failed, cancelled) = (n(State::Done), n(State::Failed), n(State::Cancelled));
        let running = n(State::Running);
        let mut t = match &self.kind {
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
        if let Some((_, line)) = r.runner.progress().next()
            && !line.is_empty()
        {
            let _ = write!(t, " — {line}");
        }
        t
    }

    /// Marks the results stale when the design changed (run spec §4): recomputes the design
    /// hash of the bench over the run's job list once per edit generation `gen`. Cost: one
    /// elaboration per config of the jobs, only after an edit and only while results exist.
    // ponytail: every edit anywhere re-hashes once; skip edits to documents the bench does
    // not reach if benches get large.
    pub fn check_stale<F: Funcs>(
        &mut self,
        project: &Project,
        env: &[(&str, &str)],
        funcs: &mut F,
        generation: u32,
    ) {
        if self.stale_gen == Some(generation) || self.busy() {
            return;
        }
        self.stale_gen = Some(generation);
        let (Some(r), Some(top)) = (&self.run, self.bench) else {
            return;
        };
        if project.doc(top).is_none() {
            return;
        }
        // Optimizer candidates and plugin jobs are pushed after the gate: hash the job list
        // the run was started (or loaded) with.
        self.stale = match schemify_run::design(project, top, &self.base, env, funcs) {
            Ok(d) => d != r.digest,
            Err(_) => true,
        };
    }

    /// Loads the newest saved results of bench `top` (`.schemify/runs/*/bench` naming it),
    /// unless this bench's results are already shown. Lists the bench's run directories in
    /// [`Sim::history`]. Returns whether results were loaded.
    pub fn load_latest(&mut self, session: &Session, top: DocHandle) -> bool {
        let name = session_name(session, top);
        self.history = history(&session.root, &name);
        if self.bench == Some(top) && self.run.is_some() {
            return false;
        }
        let Some(dir) = self.history.first().cloned() else {
            return false;
        };
        self.load_dir(session, top, &dir).is_ok()
    }

    /// Loads the results saved in run directory `dir` for bench `top`.
    ///
    /// # Errors
    ///
    /// Busy, the bench's plan no longer reads, or the results file is missing or invalid.
    pub fn load_dir(
        &mut self,
        session: &Session,
        top: DocHandle,
        dir: &Path,
    ) -> Result<(), String> {
        if self.busy() {
            return Err("a run is in progress".into());
        }
        let doc = session.project.doc(top).ok_or("the bench is closed")?;
        let plan = Plan::read(doc).map_err(|e| format!("bench: {e:?}"))?;
        let text = platform::read(&dir.join(schemify_run::RESULTS))?;
        let text = String::from_utf8(text).map_err(|e| e.to_string())?;
        let (jobs, results) = read_results(&text)?;
        let digest = *results.design();
        let mut cfg = RunnerConfig::new(&session.root, &digest);
        cfg.dir = dir.to_path_buf();
        let run = Run {
            plan,
            next: jobs.len(),
            jobs,
            results,
            runner: schemify_run::Runner::new(cfg),
            digest,
            finished: Vec::new(),
        };
        let name = session_name(session, top);
        self.reset(top, name, run, Kind::Loaded);
        self.saved = true;
        self.status = format!("results loaded from {}", dir.display());
        Ok(())
    }

    /// The raw file of job `j` (`<run dir>/<j>/out.raw`, else the first `.raw` the job wrote,
    /// as the runner reads it).
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

    /// The datasets of the selected job's raw file, parsed once per selection; only a
    /// finished job's file is read (a running simulator may be writing it).
    fn datasets(&mut self) -> Option<&[RawPlot]> {
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
        }
    }

    /// Rebuilds the plot slots for `exprs` (the shown page's `plot` fences) over the selected
    /// job: each a one-pane viewer with the trace added on the last dataset that evaluates
    /// it, autoscaled. Kept when the expressions and the selection did not change.
    pub fn refresh_plots(&mut self, exprs: &[String]) {
        let same = self.plots.len() == exprs.len()
            && self.plots.iter().zip(exprs).all(|(p, e)| p.expr == *e);
        if same {
            return;
        }
        let data: Option<Vec<RawPlot>> = self.datasets().map(<[RawPlot]>::to_vec);
        self.plots = exprs
            .iter()
            .take(PLOTS_MAX)
            .map(|e| Plot {
                expr: e.clone(),
                waves: match &data {
                    Some(d) => plot(d, e),
                    None => Err("no results yet: run the bench to fill this graph".into()),
                },
            })
            .collect();
        assert!(self.plots.len() <= PLOTS_MAX);
    }

    /// The `op` source's annotation request from the selected job's operating point (the
    /// dataset whose plot name says "operating point"): node voltages on nets
    /// (`v(<net>)` → the net's emitted name, lowercase) and device quantities
    /// ([`OP_PARAMS`] of `@<dev>[<param>]` → the instance path through `map`). Empty when
    /// the job has no operating point.
    pub fn op_marks(&mut self, map: &NameMap) -> AnnotationRequest {
        let mut marks = Vec::new();
        if let Some(op) = self.datasets().and_then(|d| {
            d.iter()
                .find(|p| p.name().to_ascii_lowercase().contains("operating point"))
        }) {
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

    /// Rows plugins read as the `Results` snapshot: one per finished job and output.
    pub fn result_rows(&self) -> crate::plugins::ResultRows {
        let Some((_, jobs, res)) = self.tables() else {
            return Vec::new();
        };
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
                rows.push((j, name.clone(), value, 0));
            }
        }
        rows
    }

    /// Appends the results panel to the state contribution `t` (nodes under `bottom`): a bar
    /// with the run summary, a stale marker and buttons, the job table (job, key, state, one
    /// column per output and per spec; specs coloured pass/fail; ≤ [`TABLE_ROWS`] rows, each
    /// row's button picks the job), Monte Carlo yield with its 95% Wilson interval, the
    /// optimizer's best point, and the bench's earlier runs.
    pub fn contribute(&self, t: &mut String) {
        let _ = writeln!(
            t,
            "node results.pane panel\n  parent bottom\n  style bottom-pane"
        );
        let _ = writeln!(
            t,
            "node results.bar panel\n  parent results.pane\n  style bottom-bar"
        );
        let title = match &self.run {
            None => "Results: nothing simulated yet (s on a .shbench)".to_owned(),
            Some(_) => {
                let mut s = format!("Results: {}  ", self.bench_name);
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
        let Some(r) = &self.run else { return };
        let (plan, jobs, res) = (&r.plan, &r.jobs, &r.results);
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
        row(t, "results.head");
        cell(t, "results.head.job", "results.head", "job", "results-key");
        cell(
            t,
            "results.head.key",
            "results.head",
            "config corner sample vars",
            "results-wide",
        );
        cell(
            t,
            "results.head.state",
            "results.head",
            "state",
            "results-cell",
        );
        for (o, name) in res.outputs().iter().enumerate() {
            cell(
                t,
                &format!("results.head.o{o}"),
                "results.head",
                name,
                "results-cell",
            );
        }
        for s in 0..res.specs() {
            let name = &res.outputs()[plan.specs[s].output as usize];
            let id = format!("results.head.s{s}");
            cell(
                t,
                &id,
                "results.head",
                &format!("spec {name}"),
                "results-cell",
            );
        }
        let mut key = String::new();
        for j in 0..jobs.len().min(TABLE_ROWS as u32) {
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
            let st = jobs.state(j);
            let st_style = match st {
                State::Done => "results-cell",
                State::Failed => "results-fail",
                _ => "results-dim",
            };
            cell(t, &format!("{rid}.state"), &rid, st.word(), st_style);
            for o in 0..res.outputs().len() as u16 {
                let v = res.value(j, o).map_or("-".to_owned(), |x| format_si(x, 4));
                cell(t, &format!("{rid}.o{o}"), &rid, &v, "results-cell");
            }
            for s in 0..res.specs() as u16 {
                let (text, style) = match res.pass(j, s) {
                    Pass::Pass => ("pass", "results-pass"),
                    Pass::Fail => ("fail", "results-fail"),
                    Pass::Unknown => ("-", "results-dim"),
                };
                cell(t, &format!("{rid}.s{s}"), &rid, text, style);
            }
        }
        if jobs.len() as usize > TABLE_ROWS {
            let more = format!(
                "… {} more job(s) in the results file",
                jobs.len() as usize - TABLE_ROWS
            );
            cell(t, "results.more", "results.list", &more, "results-dim");
        }
        for (e, (j, o, msg)) in res.errors().take(8).enumerate() {
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
        let mc = (0..jobs.len()).any(|j| jobs.sample(j) != NO_SAMPLE);
        if mc && !plan.specs.is_empty() {
            for y in yield_summary(plan, jobs, res) {
                let name = &res.outputs()[plan.specs[y.spec as usize].output as usize];
                let pct = if y.total == 0 {
                    0.0
                } else {
                    100.0 * f64::from(y.pass) / f64::from(y.total)
                };
                let line = format!(
                    "yield {name}: {}/{} = {pct:.1}%  (95% Wilson CI {:.1}% … {:.1}%)",
                    y.pass,
                    y.total,
                    100.0 * y.lo,
                    100.0 * y.hi
                );
                cell(
                    t,
                    &format!("results.yield.{}", y.spec),
                    "results.list",
                    &line,
                    "results-yield",
                );
            }
        }
        if let Kind::Optimize(o, name) = &self.kind
            && let Some(b) = o.opt.best()
        {
            let mut line = format!("optimize {name}: best score {} at", format_si(b.score, 4));
            for (p, v) in o.opt.params().iter().zip(b.params) {
                let _ = write!(line, " {}={}", p.name, format_si(*v, 4));
            }
            cell(t, "results.best", "results.list", &line, "results-yield");
        }
        for (i, d) in self.history.iter().enumerate().take(HISTORY_MAX) {
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
}

/// The display name a run directory's `bench` sidecar records (`lib:dir/stem`, the project
/// library without the `lib:`).
fn session_name(session: &Session, h: DocHandle) -> String {
    let p = &session.project;
    if let Some(row) = (0..p.cells().lib.len()).find(|&r| {
        session.path(h).is_some_and(|path| {
            let lib = p.cells().lib[r];
            session
                .lib_dir(lib)
                .is_some_and(|d| d.join(crate::libs::cell_relpath(p, r)) == path)
        })
    }) {
        let lib = &p.libs().name[usize::from(p.cells().lib[row])];
        let rel = crate::libs::cell_relpath(p, row);
        let stem = rel.rsplit_once('.').map_or(rel.as_str(), |(s, _)| s);
        return if lib.is_empty() {
            stem.to_owned()
        } else {
            format!("{lib}:{stem}")
        };
    }
    session.name(h)
}

/// The indexed cell a plugin's `lib:cell` (or a project cell's `dir/stem`) names.
fn find_cell(p: &Project, name: &str) -> Option<usize> {
    let (lib, rest) = name.split_once(':').unwrap_or(("", name));
    let rest = rest.strip_suffix(".shbench").unwrap_or(rest);
    let cells = p.cells();
    (0..cells.lib.len()).find(|&r| {
        let rel = crate::libs::cell_relpath(p, r);
        let stem = rel.rsplit_once('.').map_or(rel.as_str(), |(s, _)| s);
        p.libs().name[usize::from(cells.lib[r])] == lib && stem == rest
    })
}

/// Run directories under `<root>/.schemify/runs/` whose `bench` sidecar names `bench`,
/// newest first (by the results file's modification time, then name).
fn history(root: &Path, bench: &str) -> Vec<PathBuf> {
    let runs = root.join(".schemify").join("runs");
    let Ok(entries) = std::fs::read_dir(&runs) else {
        return Vec::new();
    };
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|d| {
            std::fs::read_to_string(d.join(BENCH)).is_ok_and(|s| s.trim() == bench)
                && d.join(schemify_run::RESULTS).exists()
        })
        .map(|d| {
            let t = std::fs::metadata(d.join(schemify_run::RESULTS))
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (t, d)
        })
        .collect();
    dirs.sort_by(|a, b| b.cmp(a));
    dirs.into_iter().take(HISTORY_MAX).map(|(_, d)| d).collect()
}

/// A one-pane viewer over `data` showing `expr` on the last dataset that evaluates it.
fn plot(data: &[RawPlot], expr: &str) -> Result<Waves, String> {
    let mut last = String::from("no dataset");
    // A refused request changes nothing, so one viewer serves every attempt.
    let mut w = Waves::new(data.to_vec());
    for d in (0..data.len()).rev() {
        let req = WaveRequest::AddTrace {
            src: expr.to_owned(),
            dataset: d as u8,
            pane: 0,
        };
        match w.apply(&req) {
            Ok(schemify_wave::Applied::Trace(_)) => {
                let _ = w.apply(&WaveRequest::Autoscale);
                return Ok(w);
            }
            Ok(schemify_wave::Applied::Value(v)) => return Err(format!("{expr} = {v}")),
            Ok(schemify_wave::Applied::Done) => {}
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!("{expr}: {last}"))
}

/// One OP mark from raw vector `name` = `x`: `v(net)` (or a bare node name) on the net,
/// `@dev[p]` with `p` in [`OP_PARAMS`] on the device's instance path.
fn op_mark(map: &NameMap, name: &str, x: f64) -> Option<Mark> {
    let lower = name.to_ascii_lowercase();
    if let Some(dev) = lower.strip_prefix('@') {
        let (dev, param) = dev.split_once('[')?;
        let param = param.strip_suffix(']')?;
        if !OP_PARAMS.contains(&param) {
            return None;
        }
        let path = inst_path(map, dev)?;
        return Some(Mark {
            target: path,
            severity: Severity::Info,
            text: format!("{param}={}", format_si(x, 4)),
        });
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
fn inst_path(map: &NameMap, dev: &str) -> Option<String> {
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
