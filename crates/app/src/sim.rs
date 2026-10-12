//! `sim`: the run spine inside the editor (D15–D18, D23, run spec §7a). One [`Sim`] holds
//! the runs of up to [`RUNS_MAX`] benches (in flight, finished or reloaded; several run at
//! once, each bench queueing its own) and everything derived from the one it shows: the
//! results table, the docs-page plot slots, the OP back-annotation and the optimizer's best
//! values.
//!
//! Non-blocking (D39): a run is started by a command (gate + design hash, no job prepared),
//! then every frame [`Sim::poll`] prepares a few jobs per run (at most [`SUBMIT_PER_FRAME`] in
//! all), polls each runner (`run::Runner::poll`, no waits) and steps the optimizers. While
//! anything runs [`Sim::wake_ms`] asks the window for a tick every [`POLL_MS`]; when nothing
//! runs it asks for none, so the editor sleeps.
//!
//! Requests wait in one queue, served in order a bench at a time: a Simulate or Optimize of a
//! bench whose run is in flight starts when that run is idle; a plugin `run` request
//! (plugin-abi §7) joins the run of its bench, or starts one.
//!
//! Data design:
//! 1. In: bench documents, selections (every corner and sweep, the first Monte Carlo
//!    record), plugin run requests, the runners' finished jobs. Out: jobs/results (owner
//!    `run`), results files (`.schemify/runs/<design>/results` + `bench`), table records,
//!    plot columns, `op` annotation marks, staged edits.
//! 2. How many: ≤ [`RUNS_MAX`] runs; jobs ≤ 2^20 per run; the table lays out ≤
//!    [`TABLE_WINDOW`] job rows at a time; plot slots ≤ [`PLOTS_MAX`]; queued requests ≤
//!    [`QUEUE_MAX`]; history ≤ [`HISTORY_MAX`].
//! 3. Widths: job indices `u32` (run's), output/spec columns `u16`.
//! 4. Access: per frame while running: each runner's children and finished list; on
//!    completion, a sort/filter change or a job selection: one pass over the shown run's
//!    jobs, one raw file parse.
//! 5. Lifetime: a run stays until its bench runs again or it is the oldest idle one when
//!    an eighth bench needs room (its results stay on disk).
//! 6. Parallel: simulators run as child processes (`workers` per run); the editor side is
//!    serial.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use schemify_doc::{DocHandle, Project};
use schemify_elab::{AnnotationRequest, Dialect, Funcs, Mark, Severity};
use schemify_netlist::NameMap;
use schemify_plugin::{PluginId, RunRequest};
use schemify_run::{
    JOB_LEVEL, Jobs, NO_SAMPLE, OptRun, Pass, Plan, Results, RunnerConfig, Select, State, Tools,
    read_results, yield_summary,
};
use schemify_wave::{RawPlot, Request as WaveRequest, Waves, format_si, group_datasets, parse_raw};

use crate::checks::SOURCE_OP;
use crate::docs::quote as q;
use crate::platform;
use crate::session::Session;
use crate::spines::{self, BENCH, Run};
use crate::waves::spice_device;

/// Tick period while a run is in flight.
pub const POLL_MS: u32 = 25;
/// Jobs prepared (elaborated + netlisted) per frame over all runs: bounds the frame cost.
pub const SUBMIT_PER_FRAME: u32 = 8;
/// Job rows the results table lays out at once, around its scroll position; the rows above
/// and below are one spacer each, so a run of any size scrolls.
pub const TABLE_WINDOW: u32 = 64;
/// Height of one results row in px: style `results-row` (18) plus the list's gap (1).
pub const ROW_PX: f32 = 19.0;
/// Plot slots with data at once.
pub const PLOTS_MAX: usize = 64;
/// Jobs one plot slot overlays (sweep points, corners, samples: one trace each).
pub const OVERLAY_JOBS: usize = 16;
/// Run requests waiting for their bench.
pub const QUEUE_MAX: usize = 256;
/// Earlier runs of a bench listed in the results panel.
pub const HISTORY_MAX: usize = 8;
/// Benches with a run in memory at once.
pub const RUNS_MAX: usize = 8;
/// MOSFETs whose OP quantities an editor run saves (`.save @m…[gm]`); beyond, none.
pub const PROBE_DEVICES_MAX: usize = 4096;
/// Device OP quantities back-annotated on instances (ngspice `@dev[param]` vectors).
pub const OP_PARAMS: [&str; 9] = ["id", "gm", "gds", "vds", "vgs", "vth", "vdsat", "i", "ic"];
/// The quantities saved for each MOSFET (every ngspice MOS model has them).
const MOS_SAVES: [&str; 6] = ["id", "gm", "gds", "vgs", "vds", "vdsat"];

/// What a run does.
#[derive(Debug)]
enum Kind {
    /// Simulate the bench's plan.
    Simulate,
    /// Run an `optimize` record (D18): one candidate's jobs at a time.
    Optimize(Box<OptRun>, String),
    /// Results loaded from disk (no children).
    Loaded,
}

/// A docs-page plot slot's data: the expression and its traces, one per job overlaid.
#[derive(Debug)]
pub struct Plot {
    /// Calculator expression of the fence.
    pub expr: String,
    /// A one-pane viewer holding the traces (autoscaled); `Err` explains an empty slot.
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
    /// The shown run changed (a job finished or was submitted): table, plots and OP follow.
    pub changed: bool,
    /// The shown run became idle this poll (results saved).
    pub done: bool,
    /// `(plugin, job)` per finished job a plugin asked for: deliver `RunDone`.
    pub run_done: Vec<(PluginId, u32)>,
    /// Optimizers that finished this poll: their best values.
    pub best: Vec<Best>,
}

/// A results table column; its header button sorts by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Col {
    /// Job index.
    Job,
    /// The job's key line (config, corner, sample, assignments).
    Key,
    /// Job state.
    State,
    /// Output `k`'s value (no value last).
    Output(u16),
    /// Spec `k`'s verdict (fail, unknown, pass).
    Spec(u16),
}

/// How the results table shows the shown run's jobs. `rows` is derived: rebuilt by
/// [`Sim::refresh_table`] when a job finished, the run or the view changed.
#[derive(Debug)]
pub struct TableView {
    /// Sort column.
    pub sort: Col,
    /// Descending.
    pub desc: bool,
    /// Filter words: a row shows when its text (job, key line, state, verdicts) contains
    /// every word, ASCII case-insensitively.
    pub filter: String,
    /// The shown jobs in table order.
    pub rows: Vec<u32>,
    dirty: bool,
}

/// One bench's run.
struct BenchRun {
    bench: DocHandle,
    name: String,
    run: Run,
    kind: Kind,
    plugin_jobs: Vec<(u32, PluginId)>,
    /// The job list the run was gated and hashed with (its stale check rehashes it).
    base: Jobs,
    saved: bool,
}

impl BenchRun {
    fn idle(&self) -> bool {
        let opt_done = match &self.kind {
            Kind::Optimize(o, _) => o.done(),
            _ => true,
        };
        self.run.idle() && opt_done
    }
}

/// A run request waiting for its bench (run spec §7a).
#[derive(Debug)]
enum Request {
    /// Simulate (`optimize` `None`) or optimize (`Some`, empty: the first record) `bench`
    /// once its run in flight is idle, adding `probe` before each netlist's `.end`.
    User {
        bench: DocHandle,
        optimize: Option<String>,
        probe: String,
    },
    /// A plugin's `run` request.
    Plugin(RunRequest),
}

/// Lines of each output stream the run log keeps (the tail).
pub const LOG_LINES: usize = 12;
/// Bytes read from the end of a log file to find its tail.
const LOG_TAIL_BYTES: u64 = 8 << 10;

/// The run log of one job: what ran and the tail of what it printed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobLog {
    /// The job (`u32::MAX` before any refresh).
    pub job: u32,
    /// The simulator command line (`command.log`); empty before the job started.
    pub command: String,
    /// Last [`LOG_LINES`] lines of stdout.
    pub out: Vec<String>,
    /// Last [`LOG_LINES`] lines of stderr.
    pub err: Vec<String>,
}

/// The last [`LOG_LINES`] lines of `path` (at most [`LOG_TAIL_BYTES`] read); empty when
/// the file is missing.
fn tail(path: &Path) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let from = len.saturating_sub(LOG_TAIL_BYTES);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(from)).is_err()
        || f.take(LOG_TAIL_BYTES).read_to_end(&mut buf).is_err()
    {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    // A cut first line (read from mid-file) is dropped.
    let skip = usize::from(from > 0 && !lines.is_empty());
    let keep = &lines[skip..];
    let out: Vec<String> = keep[keep.len().saturating_sub(LOG_LINES)..]
        .iter()
        .map(|l| (*l).to_owned())
        .collect();
    debug_assert!(out.len() <= LOG_LINES);
    out
}

/// The editor's runs.
pub struct Sim {
    tools: Tools,
    /// The dialect new runs netlist for, which picks the simulator: [`Dialect::Ngspice`] or
    /// [`Dialect::Espice`] (the editor sets it from its netlist dialect before Simulate).
    pub dialect: Dialect,
    runs: Vec<BenchRun>,
    shown: Option<DocHandle>,
    queue: Vec<Request>,
    /// The bench whose run last finished a plugin's job: the `Results` snapshot's rows.
    snapshot: Option<DocHandle>,
    /// The job whose raw file the waves, plots and OP annotation show.
    pub selected: u32,
    /// The run log of the selected job (command line and output tails), refreshed by
    /// [`Sim::poll`], [`Sim::select`] and [`Sim::start`], never in the frame path.
    pub log: JobLog,
    stale: bool,
    stale_gen: Option<u32>,
    /// Earlier run directories of the shown bench, newest first.
    pub history: Vec<PathBuf>,
    raw: Option<(u32, Result<Vec<RawPlot>, String>)>,
    /// Plot slots of the shown page.
    pub plots: Vec<Plot>,
    /// Status line of the shown run (progress, completion, failure).
    pub status: String,
    /// The results table's sort, filter and rows.
    pub table: TableView,
}

impl std::fmt::Debug for Sim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sim")
            .field("runs", &self.runs.len())
            .field("shown", &self.shown)
            .field("selected", &self.selected)
            .finish()
    }
}

impl Sim {
    /// An idle run state whose runners start `tools` (the user config's `sim` rows; tests
    /// pass a fake simulator).
    pub fn new(tools: Tools) -> Sim {
        Sim {
            tools,
            dialect: Dialect::Ngspice,
            runs: Vec::new(),
            shown: None,
            queue: Vec::new(),
            snapshot: None,
            selected: 0,
            log: JobLog::default(),
            stale: false,
            stale_gen: None,
            history: Vec::new(),
            raw: None,
            plots: Vec::new(),
            status: String::new(),
            table: TableView {
                sort: Col::Job,
                desc: false,
                filter: String::new(),
                rows: Vec::new(),
                dirty: true,
            },
        }
    }

    /// Whether a run is in flight or requests wait.
    pub fn busy(&self) -> bool {
        self.runs.iter().any(|r| !r.idle()) || !self.queue.is_empty()
    }

    /// Runs in flight.
    pub fn running(&self) -> usize {
        self.runs.iter().filter(|r| !r.idle()).count()
    }

    /// Requests waiting for their bench.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Milliseconds until the next poll: [`POLL_MS`] while busy, `None` when idle (D39).
    pub fn wake_ms(&self) -> Option<u32> {
        self.busy().then_some(POLL_MS)
    }

    /// The bench whose results are shown.
    pub fn bench(&self) -> Option<DocHandle> {
        self.shown
    }

    /// Whether the shown results belong to another design than the documents now open.
    pub fn stale(&self) -> bool {
        self.stale
    }

    fn index_of(&self, bench: DocHandle) -> Option<usize> {
        self.runs.iter().position(|r| r.bench == bench)
    }

    fn shown_run(&self) -> Option<&BenchRun> {
        self.shown
            .and_then(|b| self.index_of(b))
            .map(|i| &self.runs[i])
    }

    /// Plan, jobs and results of the shown run, if any.
    pub fn tables(&self) -> Option<(&Plan, &Jobs, &Results)> {
        self.shown_run()
            .map(|r| (&r.run.plan, &r.run.jobs, &r.run.results))
    }

    /// Plan, jobs and results of `bench`'s run, if it has one.
    pub fn tables_of(&self, bench: DocHandle) -> Option<(&Plan, &Jobs, &Results)> {
        let r = &self.runs[self.index_of(bench)?];
        Some((&r.run.plan, &r.run.jobs, &r.run.results))
    }

    /// The run directory of the shown results.
    pub fn dir(&self) -> Option<&Path> {
        self.shown_run()
            .map(|r| r.run.runner.config().dir.as_path())
    }

    /// Starts a run of `top` (a `.shbench`): Simulate (every corner and sweep, the first
    /// Monte Carlo record) or, with `optimize`, the `optimize` record of that name (the first
    /// when empty). `probe` (from [`probe_lines`], or empty) goes before every job netlist's
    /// `.end`. While a run of `top` is in flight the request waits for it (a queue per
    /// bench); runs of other benches go on. Returns the status line.
    ///
    /// # Errors
    ///
    /// A cell cannot be opened, the plan or selection is refused, the gate blocks (the
    /// message names the blocking violations), the queue is full, or [`RUNS_MAX`] runs are
    /// in flight.
    pub fn start<F: Funcs>(
        &mut self,
        session: &mut Session,
        top: DocHandle,
        optimize: Option<&str>,
        probe: &str,
        env: &[(&str, &str)],
        funcs: &mut F,
    ) -> Result<String, String> {
        let waits = self.index_of(top).is_some_and(|i| !self.runs[i].idle())
            || self
                .queue
                .iter()
                .any(|r| matches!(r, Request::User { bench, .. } if *bench == top));
        if waits {
            if self.queue.len() >= QUEUE_MAX {
                return Err("run queue full".into());
            }
            self.queue.push(Request::User {
                bench: top,
                optimize: optimize.map(str::to_owned),
                probe: probe.to_owned(),
            });
            return Ok(format!(
                "{}: queued after the run in flight",
                session_name(session, top)
            ));
        }
        self.start_now(session, top, optimize, probe, env, funcs)
    }

    fn start_now<F: Funcs>(
        &mut self,
        session: &mut Session,
        top: DocHandle,
        optimize: Option<&str>,
        probe: &str,
        env: &[(&str, &str)],
        funcs: &mut F,
    ) -> Result<String, String> {
        self.make_room(top)?;
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
        let simulate = opt_name.is_none();
        let corners: Vec<&str> = plan
            .corners
            .iter()
            .filter(|_| simulate)
            .map(|c| c.name.as_str())
            .collect();
        let sweeps: Vec<&str> = plan
            .sweeps
            .iter()
            .filter(|_| simulate)
            .map(|s| s.name.as_str())
            .collect();
        let mc = plan
            .montecarlos
            .first()
            .filter(|_| simulate)
            .map(|m| m.name.as_str());
        let select = Select {
            corners: &corners,
            sweeps: &sweeps,
            montecarlo: mc,
            ..Select::default()
        };
        // The optimizer pushes its own candidates: no plan selection.
        let select = simulate.then_some(&select);
        let cfg = self.config(session);
        let mut run = spines::start_run(session, top, select, env, self.dialect, funcs, cfg)?;
        run.extra = probe.to_owned();
        let kind = match &opt_name {
            Some(n) => {
                let o = OptRun::new(&run.plan, n).map_err(|e| format!("optimize: {e:?}"))?;
                Kind::Optimize(Box::new(o), n.clone())
            }
            None => Kind::Simulate,
        };
        let jobs = run.jobs.len();
        let name = session_name(session, top);
        self.install(BenchRun {
            bench: top,
            name,
            base: run.jobs.clone(),
            run,
            kind,
            plugin_jobs: Vec::new(),
            saved: false,
        });
        self.show(top);
        Ok(match opt_name {
            Some(n) => format!("optimize {n}: started"),
            None => format!("simulate: {jobs} job(s) queued"),
        })
    }

    /// Frees a slot for a run of `top` (its own old run, else the oldest idle run not shown).
    fn make_room(&mut self, top: DocHandle) -> Result<(), String> {
        if self.index_of(top).is_some() || self.runs.len() < RUNS_MAX {
            return Ok(());
        }
        let shown = self.shown;
        let old = self
            .runs
            .iter()
            .position(|r| r.idle() && Some(r.bench) != shown)
            .ok_or_else(|| format!("{RUNS_MAX} runs are in flight"))?;
        self.runs.remove(old);
        assert!(self.runs.len() < RUNS_MAX);
        Ok(())
    }

    /// Puts `r` in place of its bench's old run (or at the back: the newest).
    fn install(&mut self, r: BenchRun) {
        if let Some(i) = self.index_of(r.bench) {
            self.runs.remove(i);
        }
        if self.shown == Some(r.bench) {
            self.reset_view();
        }
        self.runs.push(r);
        assert!(self.runs.len() <= RUNS_MAX);
    }

    /// Shows `bench`'s results (the table, plots, OP annotation and stale check follow).
    fn show(&mut self, bench: DocHandle) {
        if self.shown != Some(bench) {
            self.shown = Some(bench);
            self.reset_view();
        }
    }

    fn reset_view(&mut self) {
        self.selected = 0;
        self.log = JobLog::default();
        self.stale = false;
        self.stale_gen = None;
        self.raw = None;
        self.plots.clear();
        // Rows index the shown run's jobs: none until [`Sim::refresh_table`] rebuilds them.
        self.table.rows.clear();
        self.table.dirty = true;
    }

    /// The runner configuration for a run of `session`'s project: the run directory under
    /// its root, every library's directory, the user config's programs.
    fn config(
        &self,
        session: &Session,
    ) -> impl FnOnce(&schemify_doc::Digest) -> RunnerConfig + use<> {
        let root = session.root.clone();
        let lib_dirs = session.lib_dirs();
        let tools = self.tools.clone();
        move |d| {
            let mut c = RunnerConfig::new(&root, d);
            c.lib_dirs = lib_dirs;
            c.tools = tools;
            c
        }
    }

    /// Cancels every run in flight (children are killed, unprepared jobs cancelled) and
    /// drops the queued requests. Returns whether anything was running.
    pub fn cancel(&mut self) -> bool {
        let was = self.busy();
        for r in &mut self.runs {
            r.run.cancel();
            if let Kind::Optimize(..) = r.kind {
                r.kind = Kind::Simulate; // the stopped search offers nothing to stage
            }
        }
        self.queue.clear();
        self.table.dirty = true;
        assert!(!self.busy());
        was
    }

    /// Enqueues plugin run requests (bounded by [`QUEUE_MAX`]; the rest are refused with a
    /// status note).
    pub fn enqueue(&mut self, reqs: Vec<RunRequest>) {
        for r in reqs {
            if self.queue.len() >= QUEUE_MAX {
                self.status = "run queue full: plugin request dropped".into();
                break;
            }
            self.queue.push(Request::Plugin(r));
        }
        assert!(self.queue.len() <= QUEUE_MAX);
    }

    /// Serves the queue in order, a bench at a time: a plugin request becomes a job of its
    /// bench's run (a fresh run when the bench has none or its results are stale), a
    /// Simulate/Optimize starts once its bench's run is idle. A request behind a waiting
    /// one of the same bench waits too.
    pub fn serve_queue<F: Funcs>(
        &mut self,
        session: &mut Session,
        env: &[(&str, &str)],
        funcs: &mut F,
    ) {
        let mut blocked: Vec<DocHandle> = Vec::new();
        let mut i = 0;
        while i < self.queue.len() {
            let bench = match &self.queue[i] {
                Request::User { bench, .. } => Ok(*bench),
                Request::Plugin(req) => {
                    match crate::session::find_cell(&session.project, &req.bench) {
                        None => Err(format!("plugin run: no bench {}", req.bench)),
                        Some(row) => session.open_cell(row),
                    }
                }
            };
            let top = match bench {
                Ok(h) => h,
                Err(e) => {
                    self.queue.remove(i);
                    self.status = e;
                    continue;
                }
            };
            let busy = self.index_of(top).is_some_and(|k| !self.runs[k].idle());
            let user = matches!(self.queue[i], Request::User { .. });
            // A user run waits for its bench's run; a plugin job joins it unless the results
            // are stale (then it waits for a fresh run).
            let joinable = self.index_of(top).is_some() && !(self.shown == Some(top) && self.stale);
            let waits = if user { busy } else { busy && !joinable };
            if waits || blocked.contains(&top) {
                blocked.push(top);
                i += 1;
                continue;
            }
            match self.queue.remove(i) {
                Request::User {
                    bench,
                    optimize,
                    probe,
                } => {
                    let started =
                        self.start_now(session, bench, optimize.as_deref(), &probe, env, funcs);
                    self.status = started.unwrap_or_else(|e| e);
                }
                Request::Plugin(req) => self.serve_plugin(session, top, &req, env, funcs),
            }
        }
        assert!(self.queue.len() <= QUEUE_MAX);
    }

    /// One plugin request for bench `top`: a job of its run, or of a fresh run of it.
    fn serve_plugin<F: Funcs>(
        &mut self,
        session: &mut Session,
        top: DocHandle,
        req: &RunRequest,
        env: &[(&str, &str)],
        funcs: &mut F,
    ) {
        let stale = self.shown == Some(top) && self.stale;
        if self.index_of(top).is_none() || stale {
            let fresh = self.make_room(top).and_then(|()| {
                let cfg = self.config(session);
                spines::start_run(session, top, None, env, self.dialect, funcs, cfg)
            });
            match fresh {
                Ok(run) => {
                    let name = session_name(session, top);
                    self.install(BenchRun {
                        bench: top,
                        name,
                        base: run.jobs.clone(),
                        run,
                        kind: Kind::Simulate,
                        plugin_jobs: Vec::new(),
                        saved: false,
                    });
                    if self.shown.is_none() {
                        self.show(top);
                    }
                }
                Err(e) => {
                    self.status = format!("plugin run: {e}");
                    return;
                }
            }
        }
        let k = self.index_of(top).expect("a run of this bench");
        let r = &mut self.runs[k];
        let opt = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        let (config, corner) = (opt(&req.config), opt(&req.corner));
        let vary: Vec<(&str, &str)> = req
            .vary
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        match r
            .run
            .jobs
            .push(config.as_deref(), corner.as_deref(), NO_SAMPLE, &vary)
        {
            Ok(j) => {
                r.plugin_jobs.push((j, req.plugin));
                r.saved = false;
                self.table.dirty |= self.shown == Some(top);
            }
            Err(e) => self.status = format!("plugin run: {e:?}"),
        }
    }

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
            self.refresh_log();
        }
    }

    /// Re-reads the selected job's run log ([`JobLog`]): bounded tails of three small files.
    pub fn refresh_log(&mut self) {
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

    /// Rebuilds [`TableView::rows`] when stale: the shown run's jobs whose text matches the
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

    /// Appends the results panel to the state contribution `t` (nodes under `bottom`): a bar
    /// with the run summary, a stale marker, the filter field and buttons; the table header
    /// (buttons that sort); the Monte Carlo yield and the optimizer's best point as rows; the
    /// job rows of [`TableView::rows`] near the list's scroll offset `scroll_px` (at most
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
            let ys = yield_summary(plan, jobs, res);
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
                    format!("μ {}", format_si(mean, 4))
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
            let mut key = format!("{name}: score {} at", format_si(b.score, 4));
            for (p, v) in o.opt.params().iter().zip(b.params) {
                let _ = write!(key, " {}={}", p.name, format_si(*v, 4));
            }
            cell(t, &format!("{rid}.key"), rid, &key, "results-yield");
            self.job_cells(t, rid, jobs, res, j);
        }
        // Job rows near the scroll offset; spacers keep the list's full height.
        let shown = self.table.rows.len() as u32;
        let data_px = (scroll_px - ROW_PX * (1 + extra_rows) as f32).max(0.0);
        let first = ((data_px / ROW_PX) as u32).min(shown);
        let start = (first.saturating_sub(TABLE_WINDOW / 8) / 16) * 16;
        let end = (start + TABLE_WINDOW).min(shown);
        // The UI caps a length at 2^14 px: a spacer is panels of GAP_ROWS rows each, then
        // the rest.
        const GAP_ROWS: u32 = 862;
        let spacer = |t: &mut String, id: &str, rows: u32| {
            let full = rows / GAP_ROWS;
            if full > 0 {
                let px = GAP_ROWS * ROW_PX as u32 - 1;
                let _ = writeln!(t, "style results-gap-full\n  height {px}");
            }
            for k in 0..full {
                let _ = writeln!(
                    t,
                    "node results.{id}.{k} panel\n  parent results.list\n  style results-gap-full"
                );
            }
            let rest = rows % GAP_ROWS;
            if rest > 0 {
                let px = rest * ROW_PX as u32 - 1;
                let _ = writeln!(
                    t,
                    "style results-{id}\n  height {px}\nnode results.{id}.{full} panel\n  parent results.list\n  style results-{id}"
                );
            }
        };
        spacer(t, "gap-top", start);
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
        spacer(t, "gap-bottom", shown - end);
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
            let v = res.value(j, o).map_or("-".to_owned(), |x| format_si(x, 4));
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

/// The text the table filter matches for job `j`: index, key line, state and verdicts,
/// lowercase.
fn row_text(jobs: &Jobs, res: &Results, j: u32, out: &mut String) {
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

/// The lines an editor run adds before each job netlist's `.end` while OP annotation is on
/// (run spec §7a, ngspice): `.save all` keeps every node voltage and branch current,
/// `.options savecurrents` adds every device's terminal currents (`@r1[i]`, `@m.x1.mm2[id]`:
/// cross-probe and OP marks), and one `.save` per MOSFET the netlist names directly saves
/// its OP quantities (`@m1[gm]`, …), for at most [`PROBE_DEVICES_MAX`] leaves of `map`.
pub fn probe_lines(map: &NameMap) -> String {
    let mut t = String::from(".save all\n.options savecurrents\n");
    for dev in map
        .leaves(PROBE_DEVICES_MAX)
        .iter()
        .map(|e| spice_device(e))
    {
        if !dev.starts_with('m') {
            continue;
        }
        t.push_str(".save");
        for p in MOS_SAVES {
            let _ = write!(t, " @{dev}[{p}]");
        }
        t.push('\n');
    }
    assert!(t.ends_with('\n'));
    t
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

/// A one-pane viewer over the datasets of `jobs` (each a raw file's grouped datasets, the
/// first the selected job's) showing `expr` once per job, on the last dataset of that job
/// that evaluates it. A scalar answer of the first job explains an empty slot instead.
fn plot(jobs: &[Vec<RawPlot>], expr: &str) -> Result<Waves, String> {
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

/// The operating point of a job's datasets: ngspice's "Operating Point" plot, else a
/// one-point DC plot (Xyce's `.op`).
fn op_dataset(data: &[RawPlot]) -> Option<&RawPlot> {
    let named = |p: &&RawPlot, s: &str| p.name().to_ascii_lowercase().contains(s);
    data.iter()
        .find(|p| named(p, "operating point"))
        .or_else(|| {
            data.iter()
                .find(|p| named(p, "dc") && p.vars().first().is_some_and(|_| p.col(0).len() == 1))
        })
}

/// One OP mark from raw vector `name` = `x`, ngspice or Xyce spelling (case-insensitive,
/// Xyce's `:` hierarchy separator read as `.`): `v(net)` or a bare node name on the net;
/// `@dev[p]` (also as ngspice writes saved ones, `i(@dev[p])`, `v(@dev[p])`) with `p` in
/// [`OP_PARAMS`], `<dev>#branch` (`i`) and lead currents
/// `i(dev)`, `id(dev)`, `ig(dev)`, `is(dev)`, `ib(dev)`, `ic(dev)`, `ie(dev)` on the device's
/// instance path.
fn op_mark(map: &NameMap, name: &str, x: f64) -> Option<Mark> {
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
