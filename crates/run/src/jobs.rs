//! The `jobs` table and job expansion (run spec §2): configs × corners × sweep points ×
//! Monte Carlo samples, in that nesting order, each job a list of var assignments.
//!
//! Six questions: in, a [`Plan`] and a [`Select`]; out, [`Jobs`] rows. At most
//! [`JOBS_MAX`] rows. Config and corner are `u16` indices into the table's own name
//! lists (bench tables hold ≤ 2^12 rows); samples `u32`; assignments are a CSR of
//! (var index, value text) with all value text in one arena. Access: the spine walks
//! jobs in order (prepare, dispatch); results index by job. Lifetime: one run. Jobs are
//! independent, which is what makes the subprocess workers parallel.

use crate::plan::{Plan, PlanError, plan_err};
use std::fmt::Write;

/// Most jobs in one run (ARCHITECTURE capacity).
pub const JOBS_MAX: u32 = 1 << 20;
/// `config`/`corner` value meaning "the bench default" (its header attribute, if any).
pub const DEFAULT: u16 = u16::MAX;
/// `sample` value of a job that is not a Monte Carlo sample.
pub const NO_SAMPLE: u32 = u32::MAX;

/// Where a job is in its life. Only this crate's transforms change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// Expanded, not yet handed to a runner.
    Pending,
    /// Netlist written, waiting for a worker.
    Queued,
    /// Simulator running.
    Running,
    /// Simulator exited 0 and its raw file was read (outputs may still have failed).
    Done,
    /// Did not produce results: elaboration gate, netlist, compile, simulator or raw
    /// file failure (the message is in the results' error table).
    Failed,
    /// Cancelled before it finished.
    Cancelled,
}

impl State {
    /// Returns the state word used in results files.
    pub fn word(self) -> &'static str {
        [
            "pending",
            "queued",
            "running",
            "done",
            "failed",
            "cancelled",
        ][self as usize]
    }

    /// Parses [`State::word`].
    pub fn from_word(w: &str) -> Option<State> {
        let all = [
            State::Pending,
            State::Queued,
            State::Running,
            State::Done,
            State::Failed,
            State::Cancelled,
        ];
        all.into_iter().find(|s| s.word() == w)
    }

    /// Returns whether the job will not change any more.
    pub fn finished(self) -> bool {
        matches!(self, State::Done | State::Failed | State::Cancelled)
    }
}

/// What to run of a bench: the dimensions a Run expands. Empty `configs`/`corners`
/// mean the bench default; `sweeps` nest with the first outermost.
#[derive(Clone, Copy, Debug, Default)]
pub struct Select<'a> {
    /// Named configs (one job set each).
    pub configs: &'a [&'a str],
    /// Named corners (feature `corners` for more than one).
    pub corners: &'a [&'a str],
    /// Sweeps, outermost first (feature `sweep`).
    pub sweeps: &'a [&'a str],
    /// Monte Carlo record (feature `mc`).
    pub montecarlo: Option<&'a str>,
    /// CLI overrides `(var, expr)`, applied after every engine's assignments.
    pub overrides: &'a [(&'a str, &'a str)],
}

/// The jobs of one run. Rows are appended by [`expand`] and [`Jobs::push`] (plugin `run`
/// requests, the optimizer) and never removed; a job index is stable for the run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Jobs {
    configs: Vec<String>,
    corners: Vec<String>,
    var_names: Vec<String>,
    config: Vec<u16>,
    corner: Vec<u16>,
    sample: Vec<u32>,
    state: Vec<State>,
    /// CSR offsets (`jobs + 1`) into `assign_var` / `assign_end`.
    assign_start: Vec<u32>,
    assign_var: Vec<u16>,
    /// End of each value in `vals` (the start is the previous end).
    assign_end: Vec<u32>,
    vals: String,
}

impl Jobs {
    /// Creates an empty table.
    pub fn new() -> Jobs {
        Jobs {
            assign_start: vec![0],
            ..Jobs::default()
        }
    }

    /// Returns the number of jobs.
    pub fn len(&self) -> u32 {
        self.config.len() as u32
    }

    /// Returns whether there are no jobs.
    pub fn is_empty(&self) -> bool {
        self.config.is_empty()
    }

    /// Appends one job and returns its index. `config`/`corner` `None` is the bench
    /// default; `sample` is [`NO_SAMPLE`] outside Monte Carlo. Assignments apply in
    /// order (a later one of the same var wins, as in elab's request).
    ///
    /// # Errors
    ///
    /// The table already holds [`JOBS_MAX`] jobs, or a value text is empty.
    pub fn push(
        &mut self,
        config: Option<&str>,
        corner: Option<&str>,
        sample: u32,
        assigns: &[(&str, &str)],
    ) -> Result<u32, PlanError> {
        assert_eq!(
            self.assign_start.len(),
            self.config.len() + 1,
            "use Jobs::new"
        );
        if self.len() >= JOBS_MAX {
            return Err(plan_err(
                "select",
                "jobs",
                format!("more than {JOBS_MAX} jobs"),
            ));
        }
        if let Some((v, _)) = assigns.iter().find(|(v, x)| v.is_empty() || x.is_empty()) {
            return Err(plan_err("select", v, "empty var name or value"));
        }
        let config = config.map_or(DEFAULT, |c| intern(&mut self.configs, c));
        let corner = corner.map_or(DEFAULT, |c| intern(&mut self.corners, c));
        for (var, val) in assigns {
            let v = intern(&mut self.var_names, var);
            self.assign_var.push(v);
            self.vals.push_str(val);
            self.assign_end.push(self.vals.len() as u32);
        }
        assert!(
            self.vals.len() < u32::MAX as usize,
            "assignment text arena full"
        );
        self.assign_start.push(self.assign_var.len() as u32);
        self.config.push(config);
        self.corner.push(corner);
        self.sample.push(sample);
        self.state.push(State::Pending);
        assert_eq!(self.state.len(), self.config.len());
        Ok(self.len() - 1)
    }

    /// Returns job `j`'s config name, `None` for the bench default.
    pub fn config(&self, j: u32) -> Option<&str> {
        name(&self.configs, self.config[j as usize])
    }

    /// Returns job `j`'s corner name, `None` for the bench default.
    pub fn corner(&self, j: u32) -> Option<&str> {
        name(&self.corners, self.corner[j as usize])
    }

    /// Returns job `j`'s Monte Carlo sample index, or [`NO_SAMPLE`].
    pub fn sample(&self, j: u32) -> u32 {
        self.sample[j as usize]
    }

    /// Returns every job's state.
    pub fn states(&self) -> &[State] {
        &self.state
    }

    /// Returns job `j`'s state.
    pub fn state(&self, j: u32) -> State {
        self.state[j as usize]
    }

    /// Cancels job `j` before it was submitted (a caller that prepares jobs lazily, like
    /// the editor, stops a run whose tail was never handed to a runner; queued and running
    /// jobs go through `Runner::cancel`). Returns whether `j` was pending.
    pub fn cancel_pending(&mut self, j: u32) -> bool {
        assert!(j < self.len(), "job {j} of {}", self.len());
        let pending = self.state[j as usize] == State::Pending;
        if pending {
            self.state[j as usize] = State::Cancelled;
        }
        assert!(self.state[j as usize] != State::Pending);
        pending
    }

    pub(crate) fn set_state(&mut self, j: u32, s: State) {
        let old = self.state[j as usize];
        assert!(!old.finished() || old == s, "job {j} already {old:?}");
        self.state[j as usize] = s;
    }

    /// Iterates job `j`'s `(var, value)` assignments in application order.
    pub fn assignments(&self, j: u32) -> impl Iterator<Item = (&str, &str)> + '_ {
        let r = self.assign_start[j as usize] as usize..self.assign_start[j as usize + 1] as usize;
        r.map(move |a| {
            let start = if a == 0 {
                0
            } else {
                self.assign_end[a - 1] as usize
            };
            let var = self.var_names[self.assign_var[a] as usize].as_str();
            (var, &self.vals[start..self.assign_end[a] as usize])
        })
    }

    /// Returns the value job `j` assigns to `var` last, if any.
    pub fn assigned(&self, j: u32, var: &str) -> Option<&str> {
        self.assignments(j)
            .filter(|(v, _)| *v == var)
            .last()
            .map(|(_, x)| x)
    }

    /// Appends job `j`'s canonical key line: `config corner sample var=value…`, `-` for a
    /// default or absent field. Part of the design hash (run spec §4) and the label the
    /// app shows for a job.
    pub fn write_key(&self, j: u32, out: &mut String) {
        let len = out.len();
        out.push_str(self.config(j).unwrap_or("-"));
        out.push(' ');
        out.push_str(self.corner(j).unwrap_or("-"));
        match self.sample(j) {
            NO_SAMPLE => out.push_str(" -"),
            s => write!(out, " {s}").expect("String write"),
        }
        for (v, x) in self.assignments(j) {
            write!(out, " {v}={x}").expect("String write");
        }
        assert!(out.len() > len);
    }
}

fn intern(names: &mut Vec<String>, n: &str) -> u16 {
    if let Some(i) = names.iter().position(|x| x == n) {
        return i as u16;
    }
    assert!(names.len() < DEFAULT as usize, "name table full");
    names.push(n.to_owned());
    (names.len() - 1) as u16
}

fn name(names: &[String], i: u16) -> Option<&str> {
    (i != DEFAULT).then(|| names[i as usize].as_str())
}

/// Expands `select` over `plan` into jobs (run spec §2): for each config, each corner,
/// each sweep point (odometer, first sweep outermost), each Monte Carlo sample — the
/// assignments are the sweep values, then the sample's varied vars (sorted by name),
/// then `select.overrides`. Pure and deterministic: same inputs, same rows.
///
/// Cost: O(jobs × assignments).
///
/// # Errors
///
/// A selected config, corner, sweep or Monte Carlo record the plan lacks; a dimension
/// whose engine feature is compiled out; more than [`JOBS_MAX`] jobs.
pub fn expand(plan: &Plan, select: &Select<'_>) -> Result<Jobs, PlanError> {
    let unknown =
        |kind: &str, n: &str| plan_err("select", n, format!("the bench has no {kind} `{n}`"));
    for c in select.configs {
        if !plan.configs.iter().any(|x| x == c) {
            return Err(unknown("config", c));
        }
    }
    for c in select.corners {
        plan.corner(c).ok_or_else(|| unknown("corner", c))?;
    }
    let sweeps = select
        .sweeps
        .iter()
        .map(|n| {
            plan.sweeps
                .iter()
                .find(|s| s.name == *n)
                .ok_or_else(|| unknown("sweep", n))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mc = select
        .montecarlo
        .map(|n| {
            plan.montecarlos
                .iter()
                .find(|m| m.name == n)
                .ok_or_else(|| unknown("montecarlo", n))
        })
        .transpose()?;
    let feature = |f: &str| plan_err("select", f, format!("built without the `{f}` feature"));
    if !cfg!(feature = "corners") && select.corners.len() > 1 {
        return Err(feature("corners"));
    }
    if !cfg!(feature = "sweep") && !sweeps.is_empty() {
        return Err(feature("sweep"));
    }
    if !cfg!(feature = "mc") && mc.is_some() {
        return Err(feature("mc"));
    }

    let configs: Vec<Option<&str>> = dim(select.configs);
    let corners: Vec<Option<&str>> = dim(select.corners);
    let samples = mc.map_or(1, |m| m.samples);
    let total = sweeps.iter().fold(
        configs.len() as u64 * corners.len() as u64 * u64::from(samples),
        |n, s| n * s.values.len() as u64,
    );
    if total > u64::from(JOBS_MAX) {
        return Err(plan_err(
            "select",
            "jobs",
            format!("{total} jobs exceed {JOBS_MAX}"),
        ));
    }

    let mut jobs = Jobs::new();
    let mut assigns: Vec<(&str, String)> = Vec::new();
    let mut point = vec![0usize; sweeps.len()];
    for &config in &configs {
        for &corner in &corners {
            point.iter_mut().for_each(|p| *p = 0);
            loop {
                for s in 0..samples {
                    assigns.clear();
                    for (sw, &k) in sweeps.iter().zip(&point) {
                        assigns.push((&sw.var, sw.values[k].clone()));
                    }
                    #[cfg(feature = "mc")]
                    if let Some(m) = mc {
                        crate::mc::sample(
                            plan,
                            corner.or(plan.default_corner.as_deref()),
                            m,
                            s,
                            &mut assigns,
                        );
                    }
                    let mut list: Vec<(&str, &str)> =
                        assigns.iter().map(|(v, x)| (*v, x.as_str())).collect();
                    list.extend_from_slice(select.overrides);
                    let sample = if mc.is_some() { s } else { NO_SAMPLE };
                    jobs.push(config, corner, sample, &list)?;
                }
                if !odometer(&mut point, &sweeps) {
                    break;
                }
            }
        }
    }
    assert_eq!(
        u64::from(jobs.len()),
        total,
        "count = product of dimensions"
    );
    Ok(jobs)
}

/// One dimension: the named entries, or the bench default alone.
fn dim<'a>(names: &[&'a str]) -> Vec<Option<&'a str>> {
    if names.is_empty() {
        vec![None]
    } else {
        names.iter().map(|n| Some(*n)).collect()
    }
}

/// Advances the sweep point (last sweep fastest); false after the last point.
fn odometer(point: &mut [usize], sweeps: &[&crate::plan::Sweep]) -> bool {
    for i in (0..point.len()).rev() {
        point[i] += 1;
        if point[i] < sweeps[i].values.len() {
            return true;
        }
        point[i] = 0;
    }
    false
}
