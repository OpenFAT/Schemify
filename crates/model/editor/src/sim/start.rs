//! Starting runs: the gate and design hash of a Simulate or Optimize, room among the
//! [`RUNS_MAX`](super::RUNS_MAX) runs, the request queue served a bench at a time, and the
//! probe lines a run adds before each netlist's `.end`.
//!
//! Contents:
//! - `Sim::start`
//! - `Sim::make_room`
//! - `Sim::install`
//! - `Sim::show`
//! - `Sim::reset_view`
//! - `Sim::config`
//! - `Sim::cancel`
//! - `Sim::enqueue`
//! - `Sim::serve_queue`
//! - `pub fn probe_lines`

use std::fmt::Write as _;

use schemify_doc::{Digest, DocHandle};
use schemify_elab::Funcs;
use schemify_netlist::NameMap;
use schemify_plugin::RunRequest;
use schemify_run::{NO_SAMPLE, OptRun, Plan, RunnerConfig, Select};

use crate::session::Session;
use crate::{spines, waves};

use super::{
    BenchRun, JobLog, Kind, PROBE_DEVICES_MAX, QUEUE_MAX, RUNS_MAX, Request, Sim, session_name,
};

/// The quantities saved for each MOSFET (every ngspice MOS model has them).
const MOS_SAVES: [&str; 6] = ["id", "gm", "gds", "vgs", "vds", "vdsat"];

impl Sim {
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
    pub(super) fn make_room(&mut self, top: DocHandle) -> Result<(), String> {
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
    pub(super) fn install(&mut self, r: BenchRun) {
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
    pub(super) fn show(&mut self, bench: DocHandle) {
        if self.shown != Some(bench) {
            self.shown = Some(bench);
            self.reset_view();
        }
    }

    pub(super) fn reset_view(&mut self) {
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
    pub(super) fn config(&self, session: &Session) -> impl FnOnce(&Digest) -> RunnerConfig + use<> {
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
    ///
    /// # Panics
    ///
    /// Never: the assert checks that the run is idle afterwards.
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
    ///
    /// # Panics
    ///
    /// Never: requests past [`QUEUE_MAX`] are refused, not queued.
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
    ///
    /// # Panics
    ///
    /// Never: the queue only shrinks here.
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
}

/// The lines an editor run adds before each job netlist's `.end` while OP annotation is on
/// (run spec §7a, ngspice): `.save all` keeps every node voltage and branch current,
/// `.options savecurrents` adds every device's terminal currents (`@r1[i]`, `@m.x1.mm2[id]`:
/// cross-probe and OP marks), and one `.save` per MOSFET the netlist names directly saves
/// its OP quantities (`@m1[gm]`, …), for at most [`PROBE_DEVICES_MAX`] leaves of `map`.
///
/// # Panics
///
/// Never: the assert checks that the text ends with a line break.
pub fn probe_lines(map: &NameMap) -> String {
    let mut t = String::from(".save all\n.options savecurrents\n");
    let leaves = map.leaves(PROBE_DEVICES_MAX);
    let devices = leaves.iter().map(|e| waves::spice_device(e));
    for dev in devices {
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
