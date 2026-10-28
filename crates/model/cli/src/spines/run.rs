//! The run spine up to submission and its native driver (run spec §1–§5, §7a): a bench and a
//! selection → a [`Run`] (plan, jobs, results, runner) → prepared and submitted jobs →
//! polled or awaited results → the saved run directory with its `bench` sidecar. Native
//! only: the web simulates through runner plugins (D16).
//!
//! Data: one [`Run`] per Simulate/Optimize. Jobs ≤ `run::JOBS_MAX` (2^20), outputs, specs
//! and vars ≤ `run::TABLE_MAX` each; `finished` holds the jobs one poll finished (≤ workers
//! + jobs started that poll). Lifetime: until the next run replaces it.
//!
//! Contents:
//! - `pub const BENCH: &str`
//! - `pub struct Run { plan, jobs, results, runner, digest, next, finished, extra, dialect }`
//! - `pub fn start_run<C: Cells, F: Funcs>(&mut C, DocHandle, Option<&Select>, &[(&str, &str)],
//!   Dialect, &mut F, impl FnOnce(&Digest) -> RunnerConfig) -> Result<Run, String>`
//! - `Run::submit_next`, `Run::poll`, `Run::idle`, `Run::cancel`, `Run::save`

use std::path::PathBuf;

use schemify_doc::{Digest, DocHandle, Project};
use schemify_elab::{Dialect, Funcs, Request};
use schemify_run::{Jobs, Plan, Results, Runner, RunnerConfig, Select, State};

use super::{Cells, SIMULATORS, elaborate_all};

/// The sidecar beside a run's `results` naming its bench (the app's addition to run spec
/// §5; `crates/model/run` neither reads nor writes it).
pub const BENCH: &str = "bench";

/// One run in flight or finished: the `run` spine's tables (owner: `run`) plus the runner.
/// The CLI submits every job and waits; the editor submits a few per frame
/// ([`Run::submit_next`]) and polls. Invariant: `next <= jobs.len()`.
pub struct Run {
    /// The bench's plan.
    pub plan: Plan,
    /// Jobs (owner: `run`).
    pub jobs: Jobs,
    /// Results (owner: `run`).
    pub results: Results,
    /// The native runner (its children die with it).
    pub runner: Runner,
    /// The design hash the run was gated and tagged with.
    pub digest: Digest,
    /// The next job index not yet prepared and submitted.
    pub next: u32,
    /// Jobs the last poll finished (cleared by the caller).
    pub finished: Vec<u32>,
    /// Lines every job's ngspice netlist gets before its `.end` (the editor's
    /// `.save`/`.options` for OP annotation and cross-probe, run spec §7a); empty: none.
    pub extra: String,
    /// The dialect every job is netlisted in (one of [`SIMULATORS`]); the design hash
    /// covers it.
    pub dialect: Dialect,
}

/// The run spine up to submission (run spec §1–§4): opens every cell each selected config
/// reaches, reads the plan, expands `select` (`None`: no jobs, for an optimizer or plugin
/// requests that push their own), gates and hashes the design for `dialect`, and creates the
/// results and the runner (`cfg` builds its configuration from the digest; an espice run
/// starts `Tools::espice`). Jobs are not prepared yet.
///
/// Cost: one [`elaborate_all`] per selected config (at least one), then the gate's.
///
/// # Errors
///
/// A cell cannot be opened, the plan or selection is refused, or the gate blocks (one
/// message, blocking violations joined by `; `).
///
/// # Panics
///
/// Panics if `dialect` is not one of [`SIMULATORS`] (the simulators the runner starts).
pub fn start_run<C: Cells, F: Funcs>(
    c: &mut C,
    top: DocHandle,
    select: Option<&Select<'_>>,
    env: &[(&str, &str)],
    dialect: Dialect,
    funcs: &mut F,
    cfg: impl FnOnce(&Digest) -> RunnerConfig,
) -> Result<Run, String> {
    assert!(SIMULATORS.contains(&dialect));
    // Open every cell each selected config reaches (configs select different impls, so
    // different cells); no selection or no config: the bench default. Bounded by the
    // selection, which names each bench config at most once per entry.
    let configs: Vec<Option<&str>> = match select.map(|s| s.configs) {
        Some(cs) if !cs.is_empty() => cs.iter().map(|c| Some(*c)).collect(),
        _ => vec![None],
    };
    for config in configs {
        let probe = Request {
            config,
            corner: None,
            overrides: &[],
            env,
            dialect,
        };
        elaborate_all(c, top, &probe, funcs)?;
    }
    let project = c.project();
    let bench = project.doc(top).expect("checked by elaborate_all");
    let plan = Plan::read(bench).map_err(|e| format!("bench: {e:?}"))?;
    let jobs = match select {
        Some(s) => schemify_run::expand(&plan, s).map_err(|e| format!("bench: {e:?}"))?,
        None => Jobs::new(),
    };
    let digest =
        schemify_run::design(project, top, &jobs, env, dialect, funcs).map_err(|b| match b {
            schemify_run::Blocked::NeedsOpen(rows) => format!("cells not open: rows {rows:?}"),
            schemify_run::Blocked::Violations(v) => format!("run blocked: {}", v.join("; ")),
        })?;
    let results = Results::new(&plan, digest);
    let mut config = cfg(&digest);
    config.dialect = dialect;
    let run = Run {
        plan,
        jobs,
        results,
        runner: Runner::new(config),
        digest,
        next: 0,
        finished: Vec::new(),
        extra: String::new(),
        dialect,
    };
    assert!(run.next <= run.jobs.len());
    Ok(run)
}

impl Run {
    /// Prepares (elaborate with the job's assignments → emit) and submits at most `max` jobs
    /// from [`Run::next`]; a job that cannot be netlisted fails and is reported in
    /// [`Run::finished`]. Returns how many jobs were handled.
    ///
    /// Cost: one elaboration and one emission per job.
    pub fn submit_next<F: Funcs>(
        &mut self,
        project: &Project,
        top: DocHandle,
        env: &[(&str, &str)],
        funcs: &mut F,
        max: u32,
    ) -> u32 {
        assert!(self.next <= self.jobs.len());
        let start = self.next;
        while self.next < self.jobs.len() && self.next - start < max {
            let j = self.next;
            self.next += 1;
            if self.jobs.state(j) != State::Pending {
                continue; // cancelled before it was prepared
            }
            let prepared = schemify_run::prepare(
                project,
                top,
                &mut self.jobs,
                j,
                env,
                self.dialect,
                funcs,
                &mut self.results,
            );
            match prepared {
                Some(mut p) => {
                    // Probe lines are ngspice's (`.save @m.x1.mm2[id]`).
                    let ngspice = matches!(self.dialect, Dialect::Ngspice | Dialect::Espice);
                    if !self.extra.is_empty() && ngspice && p.netlist.ends_with(".end\n") {
                        let at = p.netlist.len() - ".end\n".len();
                        p.netlist.insert_str(at, &self.extra);
                    }
                    self.runner.submit(&mut self.jobs, &mut self.results, j, &p);
                }
                None => self.finished.push(j),
            }
            if self.jobs.state(j) == State::Failed && !self.finished.contains(&j) {
                self.finished.push(j); // the runner could not write its directory
            }
        }
        assert!(self.next <= self.jobs.len());
        assert!(self.next - start <= max);
        self.next - start
    }

    /// Advances the runner without blocking (`Runner::poll`), appending finished jobs to
    /// [`Run::finished`].
    pub fn poll(&mut self) {
        self.runner.poll(
            &self.plan,
            &mut self.jobs,
            &mut self.results,
            &mut self.finished,
        );
    }

    /// Whether every job is submitted and none is queued or running.
    pub fn idle(&self) -> bool {
        self.next >= self.jobs.len() && !self.runner.busy()
    }

    /// Cancels every queued and running job and every job not yet submitted.
    pub fn cancel(&mut self) {
        self.runner.cancel_all(&mut self.jobs);
        for j in self.next..self.jobs.len() {
            self.jobs.cancel_pending(j);
        }
        self.next = self.jobs.len();
        assert!(self.idle());
    }

    /// Writes the results file and, beside it, `bench` (the bench's display name, so the
    /// editor finds a bench's history among the run directories). Returns the run directory.
    ///
    /// # Errors
    ///
    /// The file system's, as text.
    pub fn save(&self, bench: &str) -> Result<PathBuf, String> {
        let path = self
            .runner
            .save(&self.jobs, &self.results)
            .map_err(|e| e.to_string())?;
        let dir = path
            .parent()
            .expect("results live in the run dir")
            .to_path_buf();
        std::fs::write(dir.join(BENCH), format!("{bench}\n")).map_err(|e| e.to_string())?;
        Ok(dir)
    }
}

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Run")
            .field("jobs", &self.jobs.len())
            .field("next", &self.next)
            .finish()
    }
}
