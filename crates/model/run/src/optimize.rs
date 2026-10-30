//! Ask-tell optimizer (feature `optimize`, D18, run spec §6), salvaged from UW-ASIC
//! (D26): bounded params, weighted min/max/target objectives, `random` and
//! `nelder-mead` search, deterministic given a seed.
//!
//! [`Optimizer`] is the pure core: [`Optimizer::ask`] is a read returning the pending
//! candidate, [`Optimizer::tell`] records its measured objectives and computes the next.
//! [`OptRun`] binds it to a bench `optimize` record and the run tables: each
//! [`OptRun::step`] tells the finished job's goal outputs and pushes the next candidate
//! as a job, so the spine stays `ask → jobs → tell` with no callbacks.
//!
//! Scoring: `score = Σ weight·err`, err = `value` (min), `−value` (max) or
//! `|value − target|` (target); lower is better. A failed evaluation scores `f64::MAX`.
//! History is SoA: `params_flat` (evals × params), `objectives_flat` (evals ×
//! objectives), `scores`; at most the budget (≤ `BUDGET_MAX`) rows.
//!
//! Contents:
//! - `pub struct Param`, `pub struct Objective`, `pub enum Algorithm` (`from_name`),
//!   `pub struct Evaluation<'a>`, `pub enum OptError`
//! - `pub struct Optimizer`: `new`, `params`, `objectives`, `ask`, `tell`, `tell_failed`,
//!   `n_evals`, `eval`, `best`
//! - `pub struct OptRun`: `new`, `jobs_per_candidate`, `step`, `done`, `eval_job`,
//!   `best_job`

use crate::jobs::{Jobs, NO_SAMPLE, State};
use crate::nelder_mead::{NelderMead, UNEVALUATED};
use crate::plan::{self, Goal, Plan, PlanError};
use crate::results::Results;
use std::error::Error;
use std::fmt;
use std::ops::Range;

/// One bounded search parameter. `init` is clamped into `[min, max]` at use.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    /// Name (the bench var).
    pub name: String,
    /// Lower bound.
    pub min: f64,
    /// Upper bound.
    pub max: f64,
    /// Starting value.
    pub init: f64,
}

/// A named, weighted objective.
#[derive(Clone, Debug, PartialEq)]
pub struct Objective {
    /// Name (the output).
    pub name: String,
    /// What "good" means ([`Goal::err`]).
    pub goal: Goal,
    /// Score weight.
    pub weight: f64,
}

/// Search strategy. Both clamp every candidate into the bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Algorithm {
    /// Uniform random samples after the init point.
    Random,
    /// Downhill simplex from the init point (5% of each range as the first step).
    NelderMead,
}

/// One history row, borrowed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Evaluation<'a> {
    /// Evaluation index.
    pub index: u32,
    /// Param values evaluated.
    pub params: &'a [f64],
    /// Measured objectives (NaN for a failed evaluation).
    pub objectives: &'a [f64],
    /// Score; `f64::MAX` for a failed evaluation.
    pub score: f64,
}

/// Optimizer misuse.
#[derive(Clone, Debug, PartialEq)]
pub enum OptError {
    /// `tell` got the wrong number of objective values.
    DimMismatch {
        /// Objectives declared.
        expected: usize,
        /// Values given.
        got: usize,
    },
    /// No params, or no objectives.
    Empty,
    /// A param has `min > max` or a non-finite bound.
    InvalidBounds(String),
    /// `tell` after the budget was spent.
    BudgetSpent,
}

/// Ask-tell optimizer over bounded params and weighted objectives.
#[derive(Clone, Debug, PartialEq)]
pub struct Optimizer {
    rng_state: u64,
    budget: u32,
    params: Vec<Param>,
    objectives: Vec<Objective>,
    params_flat: Vec<f64>,
    objectives_flat: Vec<f64>,
    scores: Vec<f64>,
    search: Search,
}

/// An optimizer bound to a bench `optimize` record and a run's tables (run spec §6):
/// params are the `vary` vars with their bounds, objectives the `goal` outputs with their
/// weights; each candidate is one job per (config, corner) of the record (config outer,
/// corner inner; the bench defaults when it names none), scored on its worst job per goal.
#[derive(Clone, Debug, PartialEq)]
pub struct OptRun {
    /// The optimizer.
    pub opt: Optimizer,
    goal_outputs: Vec<u16>,
    /// The config and corner of each job of a candidate.
    points: Vec<Point>,
    /// The jobs of the candidate in flight.
    pending: Option<Range<u32>>,
    /// First job of each evaluation's candidate.
    eval_job: Vec<u32>,
}

/// Where one job of a candidate runs; `None` is the bench default.
#[derive(Clone, Debug, PartialEq)]
struct Point {
    config: Option<String>,
    corner: Option<String>,
}

/// Algorithm-internal state: existence by variant, not by `Option` fields.
#[derive(Clone, Debug, PartialEq)]
enum Search {
    /// The precomputed next candidate.
    Random(Vec<f64>),
    /// The simplex owns its candidate.
    NelderMead(NelderMead),
}

/// Seed used for 0, which xorshift cannot leave (the golden-ratio constant).
const DEFAULT_SEED: u64 = 0x9e37_79b9_7f4a_7c15;

impl Algorithm {
    /// Parses the `algo` word (`random`, `nelder-mead`).
    #[must_use]
    pub fn from_name(s: &str) -> Option<Algorithm> {
        match s {
            "random" => Some(Algorithm::Random),
            "nelder-mead" => Some(Algorithm::NelderMead),
            _ => None,
        }
    }
}

impl fmt::Display for OptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OptError::DimMismatch { expected, got } => {
                write!(f, "expected {expected} objective values, got {got}")
            }
            OptError::Empty => f.write_str("no params or no objectives"),
            OptError::InvalidBounds(p) => write!(f, "invalid bounds for param {p}"),
            OptError::BudgetSpent => f.write_str("budget spent"),
        }
    }
}

impl Error for OptError {}

impl Optimizer {
    /// Creates an optimizer that will ask at most `budget` candidates. The first
    /// candidate is the (clamped) init point for both algorithms.
    ///
    /// # Errors
    ///
    /// [`OptError::Empty`] without params or objectives; [`OptError::InvalidBounds`].
    pub fn new(
        params: Vec<Param>,
        objectives: Vec<Objective>,
        algorithm: Algorithm,
        seed: u64,
        budget: u32,
    ) -> Result<Optimizer, OptError> {
        if params.is_empty() || objectives.is_empty() {
            return Err(OptError::Empty);
        }
        if let Some(p) = params
            .iter()
            .find(|p| !p.min.is_finite() || !p.max.is_finite() || p.min > p.max)
        {
            return Err(OptError::InvalidBounds(p.name.clone()));
        }
        let init: Vec<f64> = params.iter().map(|p| p.init.clamp(p.min, p.max)).collect();
        let search = match algorithm {
            Algorithm::Random => Search::Random(init),
            Algorithm::NelderMead => Search::NelderMead(NelderMead::new(&params)),
        };
        let o = Optimizer {
            rng_state: if seed == 0 { DEFAULT_SEED } else { seed },
            budget,
            params,
            objectives,
            params_flat: Vec::new(),
            objectives_flat: Vec::new(),
            scores: Vec::new(),
            search,
        };
        // Provable from the lines above: postconditions, not documented panics.
        debug_assert_ne!(o.rng_state, 0, "xorshift cannot leave 0");
        debug_assert!(o.ask().is_none_or(|x| x.len() == o.params.len()));
        Ok(o)
    }

    /// Returns the params.
    #[must_use]
    pub fn params(&self) -> &[Param] {
        &self.params
    }

    /// Returns the objectives.
    #[must_use]
    pub fn objectives(&self) -> &[Objective] {
        &self.objectives
    }

    /// Returns the candidate to evaluate next (inside the bounds), or `None` once the
    /// budget is spent. A pure read: asking twice returns the same point.
    #[must_use]
    pub fn ask(&self) -> Option<&[f64]> {
        if self.n_evals() >= self.budget {
            return None;
        }
        let candidate = match &self.search {
            Search::Random(x) => x.as_slice(),
            Search::NelderMead(simplex) => simplex.candidate(),
        };
        debug_assert_eq!(candidate.len(), self.params.len(), "one value per param");
        debug_assert!(
            candidate
                .iter()
                .zip(&self.params)
                .all(|(&x, p)| (p.min..=p.max).contains(&x)),
            "a candidate lies inside the bounds"
        );
        Some(candidate)
    }

    /// Records the measured objectives of the asked candidate and computes the next one.
    /// Returns its score (lower is better).
    ///
    /// # Errors
    ///
    /// [`OptError::DimMismatch`] for the wrong number of values; [`OptError::BudgetSpent`].
    ///
    /// # Panics
    ///
    /// Panics if the score is NaN (a NaN value, or `inf − inf` under a target): scores
    /// must be comparable.
    pub fn tell(&mut self, measured: &[f64]) -> Result<f64, OptError> {
        if measured.len() != self.objectives.len() {
            return Err(OptError::DimMismatch {
                expected: self.objectives.len(),
                got: measured.len(),
            });
        }
        let score = score_of(&self.objectives, measured);
        self.advance(measured, score)?;
        Ok(score)
    }

    /// Records that the asked candidate could not be evaluated (simulation or output
    /// failure): it scores `f64::MAX`, so the search moves away from it.
    ///
    /// # Errors
    ///
    /// [`OptError::BudgetSpent`].
    pub fn tell_failed(&mut self) -> Result<f64, OptError> {
        let nan = vec![f64::NAN; self.objectives.len()];
        self.advance(&nan, UNEVALUATED)?;
        debug_assert_eq!(
            self.scores.last(),
            Some(&UNEVALUATED),
            "a failure scores worst"
        );
        Ok(UNEVALUATED)
    }

    /// Returns the number of evaluations told.
    #[must_use]
    pub fn n_evals(&self) -> u32 {
        debug_assert!(
            self.scores.len() <= self.budget as usize,
            "advance stops at the budget"
        );
        self.scores.len() as u32
    }

    /// Returns evaluation `i`.
    #[must_use]
    pub fn eval(&self, i: u32) -> Option<Evaluation<'_>> {
        let i = i as usize;
        if i >= self.scores.len() {
            return None;
        }
        let (param_count, objective_count) = (self.params.len(), self.objectives.len());
        debug_assert_eq!(
            self.params_flat.len(),
            self.scores.len() * param_count,
            "columns in step"
        );
        debug_assert_eq!(
            self.objectives_flat.len(),
            self.scores.len() * objective_count,
            "columns in step"
        );
        Some(Evaluation {
            index: i as u32,
            params: &self.params_flat[i * param_count..(i + 1) * param_count],
            objectives: &self.objectives_flat[i * objective_count..(i + 1) * objective_count],
            score: self.scores[i],
        })
    }

    /// Returns the lowest-score evaluation (the first on ties).
    #[must_use]
    pub fn best(&self) -> Option<Evaluation<'_>> {
        let (i, _) = self
            .scores
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.total_cmp(b))?;
        let best = self.eval(i as u32)?;
        debug_assert!(
            self.scores.iter().all(|s| best.score <= *s),
            "no evaluation scores lower"
        );
        Some(best)
    }
}

impl OptRun {
    /// Builds the optimizer for record `name` of `plan`.
    ///
    /// # Errors
    ///
    /// No such record, or an unknown `algo`.
    ///
    /// # Panics
    ///
    /// Panics if the record's goals name outputs `plan` lacks or its weights are not
    /// parallel to its goals, which [`Plan::read`] never builds.
    pub fn new(plan: &Plan, name: &str) -> Result<OptRun, PlanError> {
        let record = plan
            .optimizes
            .iter()
            .find(|o| o.name == name)
            .ok_or_else(|| {
                plan::plan_err(
                    "select",
                    name,
                    format!("the bench has no optimize `{name}`"),
                )
            })?;
        let bad = |why: String| plan::plan_err("optimize", name, why);
        assert_eq!(
            record.goals.len(),
            record.weights.len(),
            "weights are parallel to goals"
        );
        let algo = Algorithm::from_name(&record.algo)
            .ok_or_else(|| bad(format!("unknown algo `{}`", record.algo)))?;
        let mut params = Vec::with_capacity(record.vary.len());
        for v in &record.vary {
            let bounds = plan::var_bounds(plan, v).map_err(|e| bad(format!("vary {v}: {e}")))?;
            params.push(Param {
                name: v.clone(),
                min: bounds.min,
                max: bounds.max,
                init: bounds.init,
            });
        }
        let objectives: Vec<Objective> = record
            .goals
            .iter()
            .zip(&record.weights)
            .map(|(&(o, goal), &weight)| Objective {
                name: plan.outputs[o as usize].name.clone(),
                goal,
                weight,
            })
            .collect();
        let opt = Optimizer::new(params, objectives, algo, record.seed, record.budget)
            .map_err(|e| bad(e.to_string()))?;
        let dim = |names: &[String]| -> Vec<Option<String>> {
            if names.is_empty() {
                vec![None]
            } else {
                names.iter().cloned().map(Some).collect()
            }
        };
        let corners = dim(&record.corners);
        let mut points = Vec::with_capacity(record.configs.len().max(1) * corners.len());
        for config in dim(&record.configs) {
            for corner in &corners {
                points.push(Point {
                    config: config.clone(),
                    corner: corner.clone(),
                });
            }
        }
        let run = OptRun {
            opt,
            goal_outputs: record.goals.iter().map(|&(output, _)| output).collect(),
            points,
            pending: None,
            eval_job: Vec::new(),
        };
        assert_eq!(run.goal_outputs.len(), run.opt.objectives().len());
        assert!(!run.points.is_empty());
        Ok(run)
    }

    /// Returns the number of jobs each candidate runs (configs × corners of the record).
    #[must_use]
    pub fn jobs_per_candidate(&self) -> u32 {
        debug_assert!(
            !self.points.is_empty(),
            "`new` gives every candidate a point"
        );
        self.points.len() as u32
    }

    /// Advances the loop: once every job of the pending candidate finished, tells the
    /// optimizer its goal outputs (per goal, the value of the job that is worst for it; a
    /// failed job or a missing output makes the evaluation failed); then, if nothing is
    /// pending and budget remains, pushes the next candidate's jobs and returns them for
    /// the caller to prepare and dispatch. `Ok(None)`: still waiting, or done
    /// ([`OptRun::done`]).
    ///
    /// # Errors
    ///
    /// The jobs table cannot hold another candidate.
    ///
    /// # Panics
    ///
    /// Panics if `jobs` and `results` are not the tables the previous step pushed the
    /// candidate into.
    pub fn step(
        &mut self,
        jobs: &mut Jobs,
        results: &Results,
    ) -> Result<Option<Range<u32>>, PlanError> {
        if let Some(r) = self.pending.clone() {
            if !r.clone().all(|j| jobs.state(j).finished()) {
                return Ok(None);
            }
            match self.worst(jobs, results, r.clone()) {
                Some(m) => self.opt.tell(&m),
                None => self.opt.tell_failed(),
            }
            .expect("one tell per asked candidate, with one value per goal");
            self.eval_job.push(r.start);
            self.pending = None;
        }
        let Some(x) = self.opt.ask() else {
            return Ok(None);
        };
        if u64::from(jobs.len()) + self.points.len() as u64 > u64::from(crate::JOBS_MAX) {
            return Err(plan::plan_err(
                "select",
                "jobs",
                format!("more than {} jobs", crate::JOBS_MAX),
            ));
        }
        let texts: Vec<String> = x
            .iter()
            .map(|&v| {
                let mut t = String::new();
                schemify_expr::write_num(&mut t, v);
                t
            })
            .collect();
        let assigns: Vec<(&str, &str)> = self
            .opt
            .params()
            .iter()
            .zip(&texts)
            .map(|(p, t)| (p.name.as_str(), t.as_str()))
            .collect();
        let start = jobs.len();
        for point in &self.points {
            let (config, corner) = (point.config.as_deref(), point.corner.as_deref());
            jobs.push(config, corner, NO_SAMPLE, &assigns)?;
        }
        let r = start..jobs.len();
        assert_eq!(r.len(), self.points.len());
        self.pending = Some(r.clone());
        assert_eq!(self.eval_job.len() as u32, self.opt.n_evals());
        Ok(Some(r))
    }

    /// Returns whether the budget is spent and nothing is pending.
    #[must_use]
    pub fn done(&self) -> bool {
        debug_assert_eq!(
            self.eval_job.len() as u32,
            self.opt.n_evals(),
            "`step` records the job of every evaluation it tells"
        );
        self.pending.is_none() && self.opt.ask().is_none()
    }

    /// Returns the first job of evaluation `i`'s candidate (its jobs are the next
    /// [`OptRun::jobs_per_candidate`]).
    ///
    /// # Panics
    ///
    /// Panics if `i` is not an evaluation told so far.
    #[must_use]
    pub fn eval_job(&self, i: u32) -> u32 {
        assert!(
            i < self.opt.n_evals(),
            "evaluation {i} of {}",
            self.opt.n_evals()
        );
        self.eval_job[i as usize]
    }

    /// Returns the first job of the best evaluation so far.
    #[must_use]
    pub fn best_job(&self) -> Option<u32> {
        debug_assert_eq!(
            self.eval_job.len() as u32,
            self.opt.n_evals(),
            "one job per told"
        );
        self.opt.best().map(|e| self.eval_job[e.index as usize])
    }
}

// ---- private ----

/// Weighted-sum score; lower is better.
fn score_of(objectives: &[Objective], measured: &[f64]) -> f64 {
    assert_eq!(objectives.len(), measured.len(), "tell checks the count");
    objectives
        .iter()
        .zip(measured)
        .map(|(o, &v)| o.weight * o.goal.err(v))
        .sum()
}

/// xorshift64*: tiny and deterministic. State must be nonzero.
fn next_unit(state: &mut u64) -> f64 {
    debug_assert_ne!(*state, 0, "xorshift cannot leave 0");
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    let unit = (x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64;
    debug_assert!((0.0..1.0).contains(&unit), "53 bits over 2^53");
    unit
}

impl Optimizer {
    fn advance(&mut self, measured: &[f64], score: f64) -> Result<(), OptError> {
        if self.ask().is_none() {
            return Err(OptError::BudgetSpent);
        }
        assert!(!score.is_nan(), "scores are comparable");
        assert_eq!(measured.len(), self.objectives.len());
        let n = self.scores.len();
        match &mut self.search {
            Search::Random(pending) => {
                self.params_flat.extend_from_slice(pending);
                let mut state = self.rng_state;
                for (x, p) in pending.iter_mut().zip(&self.params) {
                    *x = p.min + next_unit(&mut state) * (p.max - p.min);
                }
                self.rng_state = state;
            }
            Search::NelderMead(simplex) => {
                self.params_flat.extend_from_slice(simplex.candidate());
                simplex.tell(score);
            }
        }
        self.objectives_flat.extend_from_slice(measured);
        self.scores.push(score);
        assert_eq!(
            self.params_flat.len(),
            (n + 1) * self.params.len(),
            "columns in step"
        );
        assert_eq!(
            self.objectives_flat.len(),
            (n + 1) * self.objectives.len(),
            "columns in step"
        );
        Ok(())
    }
}

impl OptRun {
    /// Per goal, the value of the job in `r` that is worst for it (largest error), or
    /// `None` when a job failed or lacks a goal output.
    fn worst(&self, jobs: &Jobs, results: &Results, r: Range<u32>) -> Option<Vec<f64>> {
        assert_eq!(r.len(), self.points.len(), "one job per point");
        let mut out = Vec::with_capacity(self.goal_outputs.len());
        for (g, &o) in self.goal_outputs.iter().enumerate() {
            let goal = self.opt.objectives()[g].goal;
            let mut worst: Option<(f64, f64)> = None;
            for j in r.clone() {
                if jobs.state(j) != State::Done {
                    return None;
                }
                let v = results.value(j, o)?;
                let err = goal.err(v);
                if worst.is_none_or(|(e, _)| err > e) {
                    worst = Some((err, v));
                }
            }
            out.push(worst?.1);
        }
        assert_eq!(out.len(), self.goal_outputs.len());
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt(algo: Algorithm, bounds: &[(f64, f64, f64)], goal: Goal, budget: u32) -> Optimizer {
        let params = bounds
            .iter()
            .enumerate()
            .map(|(i, &(min, max, init))| Param {
                name: format!("x{i}"),
                min,
                max,
                init,
            })
            .collect();
        let obj = vec![Objective {
            name: "f".into(),
            goal,
            weight: 1.0,
        }];
        Optimizer::new(params, obj, algo, 42, budget).unwrap()
    }

    fn run(o: &mut Optimizer, f: impl Fn(&[f64]) -> f64) {
        while let Some(x) = o.ask() {
            let x = x.to_vec();
            for (v, p) in x.iter().zip(o.params()) {
                assert!(
                    (p.min..=p.max).contains(v),
                    "{v} outside [{}, {}]",
                    p.min,
                    p.max
                );
            }
            o.tell(&[f(&x)]).unwrap();
        }
    }

    fn rosenbrock(x: &[f64]) -> f64 {
        (1.0 - x[0]).powi(2) + 100.0 * (x[1] - x[0] * x[0]).powi(2)
    }

    #[test]
    fn nelder_mead_finds_the_rosenbrock_minimum() {
        let mut o = opt(
            Algorithm::NelderMead,
            &[(-2.0, 2.0, -1.2), (-2.0, 2.0, 1.0)],
            Goal::Min,
            2000,
        );
        run(&mut o, rosenbrock);
        let b = o.best().unwrap();
        assert!(
            (b.params[0] - 1.0).abs() < 1e-2 && (b.params[1] - 1.0).abs() < 2e-2,
            "{:?} f={}",
            b.params,
            b.score
        );
        assert!(b.score < 1e-4);
    }

    #[test]
    fn nelder_mead_stops_at_the_bound_of_a_clipped_bowl() {
        // Unconstrained minimum (3, -3) lies outside; the bounded optimum is the corner.
        let mut o = opt(
            Algorithm::NelderMead,
            &[(-1.0, 1.0, 0.0), (-1.0, 1.0, 0.0)],
            Goal::Min,
            400,
        );
        run(&mut o, |x| (x[0] - 3.0).powi(2) + (x[1] + 3.0).powi(2));
        let b = o.best().unwrap();
        assert!(
            (b.params[0] - 1.0).abs() < 1e-6 && (b.params[1] + 1.0).abs() < 1e-6,
            "{:?}",
            b.params
        );
    }

    #[test]
    fn nelder_mead_hits_a_target_and_handles_one_dimension() {
        let mut o = opt(
            Algorithm::NelderMead,
            &[(0.0, 10.0, 9.0)],
            Goal::Target(4.0),
            200,
        );
        run(&mut o, |x| x[0] * x[0]);
        let b = o.best().unwrap();
        assert!((b.params[0] - 2.0).abs() < 1e-4, "{:?}", b.params);
    }

    #[test]
    fn random_improves_a_sphere_inside_bounds() {
        let mut o = opt(Algorithm::Random, &[(-5.0, 5.0, 4.0); 3], Goal::Min, 400);
        run(&mut o, |x| x.iter().map(|v| v * v).sum());
        assert_eq!(o.n_evals(), 400);
        let (first, best) = (o.eval(0).unwrap().score, o.best().unwrap().score);
        assert!(best < first && best < 3.0, "first {first} best {best}");
    }

    #[test]
    fn seeded_runs_are_deterministic() {
        for algo in [Algorithm::Random, Algorithm::NelderMead] {
            let mut a = opt(algo, &[(-5.0, 5.0, 4.0); 2], Goal::Max, 60);
            let mut b = a.clone();
            run(&mut a, |x| -x[0].abs() - x[1].abs());
            run(&mut b, |x| -x[0].abs() - x[1].abs());
            assert_eq!(a, b);
        }
    }

    #[test]
    fn failures_budget_and_errors() {
        let mut o = opt(Algorithm::NelderMead, &[(0.0, 1.0, 0.5)], Goal::Min, 2);
        let x0 = o.ask().unwrap().to_vec();
        assert_eq!(o.ask().unwrap(), x0, "ask is a pure read");
        assert_eq!(
            o.tell(&[1.0, 2.0]),
            Err(OptError::DimMismatch {
                expected: 1,
                got: 2
            })
        );
        assert_eq!(o.tell_failed(), Ok(f64::MAX));
        o.tell(&[0.25]).unwrap();
        assert!(o.ask().is_none());
        assert_eq!(o.tell(&[0.0]), Err(OptError::BudgetSpent));
        assert_eq!(o.best().unwrap().index, 1, "a failed evaluation never wins");
        let p = |min, max| {
            vec![Param {
                name: "p".into(),
                min,
                max,
                init: 0.0,
            }]
        };
        let obj = || {
            vec![Objective {
                name: "f".into(),
                goal: Goal::Min,
                weight: 1.0,
            }]
        };
        assert_eq!(
            Optimizer::new(p(2.0, 1.0), obj(), Algorithm::Random, 1, 5),
            Err(OptError::InvalidBounds("p".into()))
        );
        assert_eq!(
            Optimizer::new(p(0.0, f64::NAN), obj(), Algorithm::Random, 1, 5),
            Err(OptError::InvalidBounds("p".into()))
        );
        assert_eq!(
            Optimizer::new(vec![], obj(), Algorithm::Random, 1, 5),
            Err(OptError::Empty)
        );
        assert_eq!(Algorithm::from_name("anneal"), None);
    }
}
