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
//!
//! Contents:
//! - `pub const POLL_MS`
//! - `pub const SUBMIT_PER_FRAME`
//! - `pub const TABLE_WINDOW`
//! - `pub const ROW_PX`
//! - `pub const PLOTS_MAX`
//! - `pub const OVERLAY_JOBS`
//! - `pub const QUEUE_MAX`
//! - `pub const HISTORY_MAX`
//! - `pub const RUNS_MAX`
//! - `pub const PROBE_DEVICES_MAX`
//! - `pub const OP_PARAMS`
//! - `pub struct Plot`
//! - `pub struct Best`
//! - `pub struct Polled`
//! - `pub enum Col`
//! - `pub struct TableView`
//! - `pub const LOG_LINES`
//! - `pub struct JobLog`
//! - `pub struct Sim`, `Sim::new`, `Sim::busy`, `Sim::running`, `Sim::queued`, `Sim::wake_ms`,
//!   `Sim::bench`, `Sim::stale`, `Sim::tables`, `Sim::tables_of`, `Sim::dir`

mod poll;
mod results;
mod start;
mod table;

use std::fmt;
use std::path::{Path, PathBuf};

use schemify_doc::DocHandle;
use schemify_elab::Dialect;
use schemify_plugin::{PluginId, RunRequest};
use schemify_run::{Jobs, OptRun, Plan, Results, Tools};
use schemify_wave::{RawPlot, Waves};

use crate::session::Session;
use crate::spines::{self, Run};

pub use start::probe_lines;

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

/// Lines of each output stream the run log keeps (the tail).
pub const LOG_LINES: usize = 12;

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

/// Bytes read from the end of a log file to find its tail.
const LOG_TAIL_BYTES: u64 = 8 << 10;

impl fmt::Debug for Sim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
    #[must_use]
    pub fn new(tools: Tools) -> Sim {
        Sim {
            tools,
            dialect: Dialect::Espice,
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
    #[must_use]
    pub fn busy(&self) -> bool {
        self.runs.iter().any(|r| !r.idle()) || !self.queue.is_empty()
    }

    /// Runs in flight.
    ///
    /// # Panics
    ///
    /// Never: runs are capped at [`RUNS_MAX`].
    #[must_use]
    pub fn running(&self) -> usize {
        assert!(self.runs.len() <= RUNS_MAX);
        let n = self.runs.iter().filter(|r| !r.idle()).count();
        assert!(n <= self.runs.len());
        n
    }

    /// Requests waiting for their bench.
    ///
    /// # Panics
    ///
    /// Never: requests past [`QUEUE_MAX`] are refused.
    #[must_use]
    pub fn queued(&self) -> usize {
        assert!(self.queue.len() <= QUEUE_MAX);
        self.queue.len()
    }

    /// Milliseconds until the next poll: [`POLL_MS`] while busy, `None` when idle (D39).
    #[must_use]
    pub fn wake_ms(&self) -> Option<u32> {
        self.busy().then_some(POLL_MS)
    }

    /// The bench whose results are shown.
    #[must_use]
    pub fn bench(&self) -> Option<DocHandle> {
        self.shown
    }

    /// Whether the shown results belong to another design than the documents now open.
    #[must_use]
    pub fn stale(&self) -> bool {
        self.stale
    }

    fn index_of(&self, bench: DocHandle) -> Option<usize> {
        assert!(self.runs.len() <= RUNS_MAX);
        let i = self.runs.iter().position(|r| r.bench == bench);
        debug_assert!(
            i.is_none_or(|i| !self.runs[i + 1..].iter().any(|r| r.bench == bench)),
            "one run per bench"
        );
        i
    }

    fn shown_run(&self) -> Option<&BenchRun> {
        let r = self
            .shown
            .and_then(|b| self.index_of(b))
            .map(|i| &self.runs[i]);
        assert!(r.is_none_or(|r| Some(r.bench) == self.shown));
        r
    }

    /// Plan, jobs and results of the shown run, if any.
    #[must_use]
    pub fn tables(&self) -> Option<(&Plan, &Jobs, &Results)> {
        self.shown_run()
            .map(|r| (&r.run.plan, &r.run.jobs, &r.run.results))
    }

    /// Plan, jobs and results of `bench`'s run, if it has one.
    ///
    /// # Panics
    ///
    /// Never: the run found is `bench`'s.
    #[must_use]
    pub fn tables_of(&self, bench: DocHandle) -> Option<(&Plan, &Jobs, &Results)> {
        let r = &self.runs[self.index_of(bench)?];
        assert_eq!(r.bench, bench);
        Some((&r.run.plan, &r.run.jobs, &r.run.results))
    }

    /// The run directory of the shown results.
    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        self.shown_run()
            .map(|r| r.run.runner.config().dir.as_path())
    }
}

// ---- private ----

impl BenchRun {
    fn idle(&self) -> bool {
        let opt_done = match &self.kind {
            Kind::Optimize(o, _) => o.done(),
            _ => true,
        };
        self.run.idle() && opt_done
    }
}

/// The display name a run directory's `bench` sidecar records (`lib:dir/stem`, the project
/// library without the `lib:`).
fn session_name(session: &Session, h: DocHandle) -> String {
    session
        .path(h)
        .and_then(|path| spines::bench_name(&session.project, |lib| session.lib_dir(lib), path))
        .unwrap_or_else(|| session.name(h))
}
