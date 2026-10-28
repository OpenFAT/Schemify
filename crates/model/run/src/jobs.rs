//! The `jobs` table and job expansion (run spec §2): a [`Plan`] and a [`Select`] in, one
//! [`Jobs`] row per (config × corner × sweep point × Monte Carlo sample) out, in that
//! nesting order, each row a list of var assignments.
//!
//! Data: at most [`JOBS_MAX`] rows. Config and corner are `u16` indices into the table's
//! own name lists (bench tables hold ≤ 2^12 rows, [`DEFAULT`] is the bench default);
//! samples `u32` ([`NO_SAMPLE`] outside Monte Carlo); simulator seeds `u32` in `1..=2^30`
//! (0: none, ngspice takes no seed 0); assignments are a CSR of (var index, value text)
//! with every value in one text arena. The spine walks jobs in order (prepare, dispatch);
//! results index by job. Lifetime: one run. Jobs are independent, which is what makes the
//! subprocess workers parallel.
//!
//! Contents:
//! - `pub const JOBS_MAX: u32`, `pub const DEFAULT: u16`, `pub const NO_SAMPLE: u32`
//! - `pub enum State`, `State::word`, `State::from_word`, `State::finished`
//! - `pub struct Select<'a>`
//! - `pub struct Jobs`: `new`, `len`, `is_empty`, `push`, `config`, `corner`, `sample`,
//!   `seed`, `states`, `state`, `cancel_pending`, `assignments`, `assigned`, `write_key`;
//!   crate-only `push_row`, `set_state`
//! - `pub fn expand(plan: &Plan, select: &Select<'_>) -> Result<Jobs, PlanError>`

use crate::plan::{Plan, PlanError, plan_err};
use std::fmt::Write;
use std::ops::Range;

/// Most jobs in one run (architecture.md capacity).
pub const JOBS_MAX: u32 = 1 << 20;
/// `config`/`corner` value meaning "the bench default" (its header attribute, if any).
pub const DEFAULT: u16 = u16::MAX;
/// `sample` value of a job that is not a Monte Carlo sample.
pub const NO_SAMPLE: u32 = u32::MAX;

/// Where a job is in its life. Only this crate's transforms change it; a finished state
/// ([`State::finished`]) never changes again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// Expanded, not yet handed to a runner.
    Pending,
    /// Netlist written, waiting for a worker (or holding one while its sources compile).
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
/// requests, the optimizer) and never removed, so a job index is stable for the run.
/// Every method taking a job index `j` panics when `j >= len()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Jobs {
    configs: Vec<String>,
    corners: Vec<String>,
    var_names: Vec<String>,
    config: Vec<u16>,
    corner: Vec<u16>,
    sample: Vec<u32>,
    /// Simulator random seed (run spec §3), 0 for none.
    seed: Vec<u32>,
    state: Vec<State>,
    /// Per job, one past its last assignment in `assign_var` (CSR ends).
    assign_end: Vec<u32>,
    assign_var: Vec<u16>,
    /// Per assignment, one past its value's last byte in `vals` (CSR ends).
    val_end: Vec<u32>,
    vals: String,
}

/// Expands `select` over `plan` into jobs (run spec §2): for each config, each corner,
/// each sweep point (odometer, first sweep outermost), each Monte Carlo sample. The
/// assignments are the sweep values, then the sample's varied vars (sorted by name), then
/// `select.overrides`. Pure and deterministic: same inputs, same rows.
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
    if let Some(c) = select
        .configs
        .iter()
        .find(|c| !plan.configs.iter().any(|x| x == *c))
    {
        return Err(unknown("config", c));
    }
    if let Some(c) = select.corners.iter().find(|c| plan.corner(c).is_none()) {
        return Err(unknown("corner", c));
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
            let found = plan.montecarlos.iter().find(|m| m.name == n);
            found.ok_or_else(|| unknown("montecarlo", n))
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

    // Every dimension is a digit of one odometer, outermost first (config, corner, each
    // sweep, sample); job `j` is the odometer after `j` steps, last digit fastest.
    let configs = dim(select.configs);
    let corners = dim(select.corners);
    let mut radix = vec![configs.len(), corners.len()];
    radix.extend(sweeps.iter().map(|s| s.values.len()));
    radix.push(mc.map_or(1, |m| m.samples) as usize);
    let total = radix.iter().fold(1u64, |n, &r| n.saturating_mul(r as u64));
    if total > u64::from(JOBS_MAX) {
        let why = format!("{total} jobs exceed {JOBS_MAX}");
        return Err(plan_err("select", "jobs", why));
    }

    let mut jobs = Jobs::new();
    let mut assigns: Vec<(&str, String)> = Vec::new();
    let mut digit = vec![0usize; radix.len()];
    let last = radix.len() - 1;
    for _ in 0..total {
        let (config, corner, s) = (configs[digit[0]], corners[digit[1]], digit[last] as u32);
        assigns.clear();
        for (sw, &k) in sweeps.iter().zip(&digit[2..last]) {
            assigns.push((&sw.var, sw.values[k].clone()));
        }
        #[cfg(feature = "mc")]
        let seed = match mc {
            Some(m) => {
                let corner = corner.or(plan.default_corner.as_deref());
                crate::mc::sample(plan, corner, m, s, &mut assigns);
                crate::mc::sim_seed(m.seed, s)
            }
            None => 0,
        };
        #[cfg(not(feature = "mc"))]
        let seed = 0;
        let mut list: Vec<(&str, &str)> = assigns.iter().map(|(v, x)| (*v, x.as_str())).collect();
        list.extend_from_slice(select.overrides);
        let sample = if mc.is_some() { s } else { NO_SAMPLE };
        jobs.push_row(config, corner, sample, seed, &list)?;
        for d in (0..=last).rev() {
            digit[d] += 1;
            if digit[d] < radix[d] {
                break;
            }
            digit[d] = 0;
        }
    }
    assert_eq!(
        u64::from(jobs.len()),
        total,
        "count = product of dimensions"
    );
    assert!(
        digit.iter().all(|&d| d == 0),
        "the odometer wrapped exactly once"
    );
    Ok(jobs)
}

impl State {
    /// Every state, in `repr` order (the index of [`State::word`]).
    const ALL: [State; 6] = [
        State::Pending,
        State::Queued,
        State::Running,
        State::Done,
        State::Failed,
        State::Cancelled,
    ];

    /// Returns the state word results files use.
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

    /// Parses [`State::word`]; `None` for any other text.
    pub fn from_word(w: &str) -> Option<State> {
        State::ALL.into_iter().find(|s| s.word() == w)
    }

    /// Returns whether the job will not change any more (done, failed or cancelled).
    pub fn finished(self) -> bool {
        matches!(self, State::Done | State::Failed | State::Cancelled)
    }
}

impl Jobs {
    /// Creates an empty table.
    pub fn new() -> Jobs {
        Jobs::default()
    }

    /// Returns the number of jobs.
    pub fn len(&self) -> u32 {
        self.config.len() as u32
    }

    /// Returns whether there are no jobs.
    pub fn is_empty(&self) -> bool {
        self.config.is_empty()
    }

    /// Appends one pending job without a simulator seed and returns its index.
    /// `config`/`corner` `None` is the bench default; `sample` is [`NO_SAMPLE`] outside
    /// Monte Carlo. Assignments apply in order (a later one of the same var wins, as in
    /// elab's request).
    ///
    /// # Errors
    ///
    /// The table already holds [`JOBS_MAX`] jobs, or a var name or value text is empty;
    /// the table is unchanged.
    pub fn push(
        &mut self,
        config: Option<&str>,
        corner: Option<&str>,
        sample: u32,
        assigns: &[(&str, &str)],
    ) -> Result<u32, PlanError> {
        self.push_row(config, corner, sample, 0, assigns)
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

    /// Returns job `j`'s simulator random seed (run spec §3: a Monte Carlo sample's, so
    /// the simulator's own random functions draw the same values every run), if any.
    pub fn seed(&self, j: u32) -> Option<u32> {
        let s = self.seed[j as usize];
        (s != 0).then_some(s)
    }

    /// Returns every job's state, indexed by job.
    pub fn states(&self) -> &[State] {
        &self.state
    }

    /// Returns job `j`'s state.
    pub fn state(&self, j: u32) -> State {
        self.state[j as usize]
    }

    /// Cancels job `j` if it was never submitted (a caller that prepares jobs lazily, like
    /// the editor, stops a run whose tail never reached a runner; queued and running jobs
    /// go through `Runner::cancel`). Returns whether `j` was pending.
    pub fn cancel_pending(&mut self, j: u32) -> bool {
        assert!(j < self.len(), "job {j} of {}", self.len());
        let pending = self.state[j as usize] == State::Pending;
        if pending {
            self.state[j as usize] = State::Cancelled;
        }
        assert_ne!(self.state[j as usize], State::Pending);
        pending
    }

    /// Iterates job `j`'s `(var, value)` assignments in application order.
    pub fn assignments(&self, j: u32) -> impl Iterator<Item = (&str, &str)> + '_ {
        span(&self.assign_end, j as usize).map(move |a| {
            let var = self.var_names[self.assign_var[a] as usize].as_str();
            (var, &self.vals[span(&self.val_end, a)])
        })
    }

    /// Returns the value job `j` assigns to `var` last, if any.
    pub fn assigned(&self, j: u32, var: &str) -> Option<&str> {
        self.assignments(j)
            .filter(|(v, _)| *v == var)
            .last()
            .map(|(_, x)| x)
    }

    /// Appends job `j`'s canonical key line, `config corner sample var=value…` with `-`
    /// for a default or absent field: part of the design hash (run spec §4) and the label
    /// the app shows for a job.
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
        assert!(out.len() > len + 4, "at least `- - -`");
    }

    /// [`Jobs::push`] with a simulator seed (0: none): Monte Carlo expansion and the
    /// results-file reader.
    pub(crate) fn push_row(
        &mut self,
        config: Option<&str>,
        corner: Option<&str>,
        sample: u32,
        seed: u32,
        assigns: &[(&str, &str)],
    ) -> Result<u32, PlanError> {
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
            assert!(
                self.vals.len() < u32::MAX as usize,
                "assignment text arena full"
            );
            self.val_end.push(self.vals.len() as u32);
        }
        self.assign_end.push(self.assign_var.len() as u32);
        self.config.push(config);
        self.corner.push(corner);
        self.sample.push(sample);
        self.seed.push(seed);
        self.state.push(State::Pending);
        assert_eq!(self.assign_end.len(), self.config.len());
        assert_eq!(self.val_end.len(), self.assign_var.len());
        Ok(self.len() - 1)
    }

    /// Moves job `j` to `s`.
    ///
    /// # Panics
    ///
    /// Panics if `j` already finished in another state: a finished job never changes.
    pub(crate) fn set_state(&mut self, j: u32, s: State) {
        let old = self.state[j as usize];
        assert!(!old.finished() || old == s, "job {j} already {old:?}");
        self.state[j as usize] = s;
    }
}

// ---- private ----

/// One dimension: the named entries, or the bench default alone.
fn dim<'a>(names: &[&'a str]) -> Vec<Option<&'a str>> {
    if names.is_empty() {
        return vec![None];
    }
    names.iter().map(|n| Some(*n)).collect()
}

/// Row `i`'s range in a CSR whose `ends` hold one past each row's last element.
fn span(ends: &[u32], i: usize) -> Range<usize> {
    let start = if i == 0 { 0 } else { ends[i - 1] as usize };
    let end = ends[i] as usize;
    assert!(start <= end);
    start..end
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_words_round_trip_and_reject_others() {
        for s in State::ALL {
            assert_eq!(State::from_word(s.word()), Some(s));
        }
        assert_eq!(State::from_word("Done"), None);
        assert_eq!(State::from_word(""), None);
    }

    #[test]
    fn push_keeps_assignments_and_refuses_empty_text() {
        let mut jobs = Jobs::new();
        assert_eq!(jobs, Jobs::default(), "the default table is valid");
        assert_eq!(
            jobs.push(None, Some("ff"), NO_SAMPLE, &[("R", "1k"), ("C", "1p")]),
            Ok(0)
        );
        assert_eq!(
            jobs.push(Some("a"), None, 3, &[("R", "2k"), ("R", "3k")]),
            Ok(1)
        );
        assert!(jobs.push(None, None, NO_SAMPLE, &[("R", "")]).is_err());
        assert!(jobs.push(None, None, NO_SAMPLE, &[("", "1")]).is_err());
        assert_eq!(jobs.len(), 2, "a refused push leaves the table unchanged");
        let a: Vec<_> = jobs.assignments(0).collect();
        assert_eq!(a, [("R", "1k"), ("C", "1p")]);
        assert_eq!(
            jobs.assigned(1, "R"),
            Some("3k"),
            "the later assignment wins"
        );
        assert_eq!(jobs.assigned(1, "C"), None);
        let mut key = String::new();
        jobs.write_key(0, &mut key);
        key.push('|');
        jobs.write_key(1, &mut key);
        assert_eq!(key, "- ff - R=1k C=1p|a - 3 R=2k R=3k");
    }

    #[test]
    fn finished_states_never_change() {
        let mut jobs = Jobs::new();
        jobs.push(None, None, NO_SAMPLE, &[]).unwrap();
        assert!(jobs.cancel_pending(0));
        assert!(!jobs.cancel_pending(0), "only a pending job cancels");
        jobs.set_state(0, State::Cancelled);
        let changed = std::panic::catch_unwind(move || jobs.set_state(0, State::Done));
        assert!(changed.is_err());
    }
}
