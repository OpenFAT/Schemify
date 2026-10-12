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

mod nelder_mead;

use crate::jobs::{Jobs, NO_SAMPLE, State};
use crate::plan::{Goal, Plan, PlanError, plan_err, var_bounds};
use crate::results::Results;
use nelder_mead::{NelderMead, UNEVALUATED};
use std::fmt;
use std::ops::Range;

/// Seed used for 0, which xorshift cannot leave (the golden-ratio constant).
const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

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

/// What "good" means for one measured objective.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    /// Smaller is better.
    Minimize,
    /// Larger is better.
    Maximize,
    /// Closer to the value is better.
    Approach(f64),
}

/// A named, weighted objective.
#[derive(Clone, Debug, PartialEq)]
pub struct Objective {
    /// Name (the output).
    pub name: String,
    /// Direction.
    pub target: Target,
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

impl Algorithm {
    /// Parses the `algo` word (`random`, `nelder-mead`).
    pub fn from_name(s: &str) -> Option<Algorithm> {
        match s {
            "random" => Some(Algorithm::Random),
            "nelder-mead" => Some(Algorithm::NelderMead),
            _ => None,
        }
    }
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

impl std::error::Error for OptError {}

/// Algorithm-internal state: existence by variant, not by `Option` fields.
#[derive(Clone, Debug, PartialEq)]
enum Search {
    /// The precomputed next candidate.
    Random(Vec<f64>),
    /// The simplex owns its candidate.
    NelderMead(NelderMead),
}

/// Ask-tell optimizer over bounded params and weighted objectives.
#[derive(Clone, Debug, PartialEq)]
pub struct Optimizer {
    algorithm: Algorithm,
    rng_state: u64,
    budget: u32,
    params: Vec<Param>,
    objectives: Vec<Objective>,
    params_flat: Vec<f64>,
    objectives_flat: Vec<f64>,
    scores: Vec<f64>,
    search: Search,
}

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
            algorithm,
            rng_state: if seed == 0 { DEFAULT_SEED } else { seed },
            budget,
            params,
            objectives,
            params_flat: Vec::new(),
            objectives_flat: Vec::new(),
            scores: Vec::new(),
            search,
        };
        assert!(o.rng_state != 0 && o.ask().is_none_or(|x| x.len() == o.params.len()));
        Ok(o)
    }

    /// Returns the algorithm.
    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// Returns the params.
    pub fn params(&self) -> &[Param] {
        &self.params
    }

    /// Returns the objectives.
    pub fn objectives(&self) -> &[Objective] {
        &self.objectives
    }

    /// Returns the candidate to evaluate next (inside the bounds), or `None` once the
    /// budget is spent. A pure read: asking twice returns the same point.
    pub fn ask(&self) -> Option<&[f64]> {
        if self.n_evals() >= self.budget {
            return None;
        }
        Some(match &self.search {
            Search::Random(x) => x,
            Search::NelderMead(nm) => nm.candidate(),
        })
    }

    /// Records the measured objectives of the asked candidate and computes the next one.
    /// Returns its score (lower is better).
    ///
    /// # Errors
    ///
    /// [`OptError::DimMismatch`] for the wrong number of values; [`OptError::BudgetSpent`].
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
        Ok(UNEVALUATED)
    }

    fn advance(&mut self, measured: &[f64], score: f64) -> Result<(), OptError> {
        if self.ask().is_none() {
            return Err(OptError::BudgetSpent);
        }
        assert!(!score.is_nan(), "scores are comparable");
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
            Search::NelderMead(nm) => {
                self.params_flat.extend_from_slice(nm.candidate());
                nm.tell(score);
            }
        }
        self.objectives_flat.extend_from_slice(measured);
        self.scores.push(score);
        assert_eq!(self.params_flat.len(), (n + 1) * self.params.len());
        Ok(())
    }

    /// Returns the number of evaluations told.
    pub fn n_evals(&self) -> u32 {
        self.scores.len() as u32
    }

    /// Returns evaluation `i`.
    pub fn eval(&self, i: u32) -> Option<Evaluation<'_>> {
        let i = i as usize;
        if i >= self.scores.len() {
            return None;
        }
        let (np, no) = (self.params.len(), self.objectives.len());
        Some(Evaluation {
            index: i as u32,
            params: &self.params_flat[i * np..(i + 1) * np],
            objectives: &self.objectives_flat[i * no..(i + 1) * no],
            score: self.scores[i],
        })
    }

    /// Returns the lowest-score evaluation (the first on ties).
    pub fn best(&self) -> Option<Evaluation<'_>> {
        let (i, _) = self
            .scores
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.total_cmp(b))?;
        self.eval(i as u32)
    }
}

/// Weighted-sum score; lower is better.
fn score_of(objectives: &[Objective], measured: &[f64]) -> f64 {
    objectives
        .iter()
        .zip(measured)
        .map(|(o, &v)| {
            let err = match o.target {
                Target::Minimize => v,
                Target::Maximize => -v,
                Target::Approach(t) => (v - t).abs(),
            };
            o.weight * err
        })
        .sum()
}

/// xorshift64*: tiny and deterministic. State must be nonzero.
fn next_unit(state: &mut u64) -> f64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
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
    /// `(config, corner)` of each job of a candidate; `None` is the bench default.
    points: Vec<(Option<String>, Option<String>)>,
    /// The jobs of the candidate in flight.
    pending: Option<Range<u32>>,
    /// First job of each evaluation's candidate.
    eval_job: Vec<u32>,
}

impl OptRun {
    /// Builds the optimizer for record `name` of `plan`.
    ///
    /// # Errors
    ///
    /// No such record, or an unknown `algo`.
    pub fn new(plan: &Plan, name: &str) -> Result<OptRun, PlanError> {
        let rec = plan
            .optimizes
            .iter()
            .find(|o| o.name == name)
            .ok_or_else(|| {
                plan_err(
                    "select",
                    name,
                    format!("the bench has no optimize `{name}`"),
                )
            })?;
        let bad = |why: String| plan_err("optimize", name, why);
        let algo = Algorithm::from_name(&rec.algo)
            .ok_or_else(|| bad(format!("unknown algo `{}`", rec.algo)))?;
        let mut params = Vec::with_capacity(rec.vary.len());
        for v in &rec.vary {
            let (min, max, init) =
                var_bounds(plan, v).map_err(|e| bad(format!("vary {v}: {e}")))?;
            params.push(Param {
                name: v.clone(),
                min,
                max,
                init,
            });
        }
        let objectives = rec
            .goals
            .iter()
            .zip(&rec.weights)
            .map(|(&(o, g), &weight)| Objective {
                name: plan.outputs[o as usize].name.clone(),
                target: match g {
                    Goal::Min => Target::Minimize,
                    Goal::Max => Target::Maximize,
                    Goal::Target(t) => Target::Approach(t),
                },
                weight,
            })
            .collect();
        let opt = Optimizer::new(params, objectives, algo, rec.seed, rec.budget)
            .map_err(|e| bad(e.to_string()))?;
        let dim = |names: &[String]| -> Vec<Option<String>> {
            if names.is_empty() {
                vec![None]
            } else {
                names.iter().cloned().map(Some).collect()
            }
        };
        let corners = dim(&rec.corners);
        let points = dim(&rec.configs)
            .into_iter()
            .flat_map(|cf| corners.iter().map(move |co| (cf.clone(), co.clone())))
            .collect();
        let run = OptRun {
            opt,
            goal_outputs: rec.goals.iter().map(|g| g.0).collect(),
            points,
            pending: None,
            eval_job: Vec::new(),
        };
        assert_eq!(run.goal_outputs.len(), run.opt.objectives().len());
        assert!(!run.points.is_empty());
        Ok(run)
    }

    /// Returns the number of jobs each candidate runs (configs × corners of the record).
    pub fn jobs_per_candidate(&self) -> u32 {
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
            return Err(plan_err(
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
        for (config, corner) in &self.points {
            jobs.push(config.as_deref(), corner.as_deref(), NO_SAMPLE, &assigns)?;
        }
        let r = start..jobs.len();
        assert_eq!(r.len(), self.points.len());
        self.pending = Some(r.clone());
        assert_eq!(self.eval_job.len() as u32, self.opt.n_evals());
        Ok(Some(r))
    }

    /// Per goal, the value of the job in `r` that is worst for it (largest error), or
    /// `None` when a job failed or lacks a goal output.
    fn worst(&self, jobs: &Jobs, results: &Results, r: Range<u32>) -> Option<Vec<f64>> {
        let mut out = Vec::with_capacity(self.goal_outputs.len());
        for (g, &o) in self.goal_outputs.iter().enumerate() {
            let target = self.opt.objectives()[g].target;
            let mut worst: Option<(f64, f64)> = None;
            for j in r.clone() {
                if jobs.state(j) != State::Done {
                    return None;
                }
                let v = results.value(j, o)?;
                let err = match target {
                    Target::Minimize => v,
                    Target::Maximize => -v,
                    Target::Approach(t) => (v - t).abs(),
                };
                if worst.is_none_or(|(e, _)| err > e) {
                    worst = Some((err, v));
                }
            }
            out.push(worst?.1);
        }
        assert_eq!(out.len(), self.goal_outputs.len());
        Some(out)
    }

    /// Returns whether the budget is spent and nothing is pending.
    pub fn done(&self) -> bool {
        self.pending.is_none() && self.opt.ask().is_none()
    }

    /// Returns the first job of evaluation `i`'s candidate (its jobs are the next
    /// [`OptRun::jobs_per_candidate`]).
    pub fn eval_job(&self, i: u32) -> u32 {
        self.eval_job[i as usize]
    }

    /// Returns the first job of the best evaluation so far.
    pub fn best_job(&self) -> Option<u32> {
        self.opt.best().map(|e| self.eval_job[e.index as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt(algo: Algorithm, bounds: &[(f64, f64, f64)], target: Target, budget: u32) -> Optimizer {
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
            target,
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
            Target::Minimize,
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
            Target::Minimize,
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
            Target::Approach(4.0),
            200,
        );
        run(&mut o, |x| x[0] * x[0]);
        let b = o.best().unwrap();
        assert!((b.params[0] - 2.0).abs() < 1e-4, "{:?}", b.params);
    }

    #[test]
    fn random_improves_a_sphere_inside_bounds() {
        let mut o = opt(
            Algorithm::Random,
            &[(-5.0, 5.0, 4.0); 3],
            Target::Minimize,
            400,
        );
        run(&mut o, |x| x.iter().map(|v| v * v).sum());
        assert_eq!(o.n_evals(), 400);
        let (first, best) = (o.eval(0).unwrap().score, o.best().unwrap().score);
        assert!(best < first && best < 3.0, "first {first} best {best}");
    }

    #[test]
    fn seeded_runs_are_deterministic() {
        for algo in [Algorithm::Random, Algorithm::NelderMead] {
            let mut a = opt(algo, &[(-5.0, 5.0, 4.0); 2], Target::Maximize, 60);
            let mut b = a.clone();
            run(&mut a, |x| -x[0].abs() - x[1].abs());
            run(&mut b, |x| -x[0].abs() - x[1].abs());
            assert_eq!(a, b);
        }
    }

    #[test]
    fn failures_budget_and_errors() {
        let mut o = opt(
            Algorithm::NelderMead,
            &[(0.0, 1.0, 0.5)],
            Target::Minimize,
            2,
        );
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
                target: Target::Minimize,
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
