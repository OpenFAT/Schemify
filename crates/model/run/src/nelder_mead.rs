//! Ask-tell Nelder-Mead simplex over bounded params (run spec §6): one score in per
//! `tell`, the next candidate out.
//!
//! Classic downhill simplex split into a state machine because evaluations arrive one at
//! a time (tell), instead of the algorithm driving a closure. Every candidate is clamped
//! to `[lo, hi]` before it is handed out, so the simplex only ever holds feasible points.
//!
//! Contents:
//! - crate-only `const UNEVALUATED: f64`
//! - crate-only `struct NelderMead`: `new`, `candidate`, `tell`

use crate::optimize::Param;
use std::mem;

/// Placeholder for not-yet-evaluated vertices; also the score of a failed evaluation,
/// so a failed candidate sorts worst.
pub(crate) const UNEVALUATED: f64 = f64::MAX;

/// The simplex, its scores and the pending candidate. Rows stay inside `[lo, hi]`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NelderMead {
    lo: Vec<f64>,
    hi: Vec<f64>,
    /// (n+1) x n row-major; rows kept sorted by score between iterations.
    simplex: Vec<f64>,
    /// n+1 scores, parallel to simplex rows.
    scores: Vec<f64>,
    phase: Phase,
    /// The point handed out by `candidate()`; the next `tell` scores it.
    candidate: Vec<f64>,
    /// Reflected point kept across Reflecting -> Expanding/Contracting.
    reflected: Vec<f64>,
    reflected_score: f64,
}

const ALPHA: f64 = 1.0; // reflection
const GAMMA: f64 = 2.0; // expansion
const RHO: f64 = 0.5; // contraction
const SIGMA: f64 = 0.5; // shrink

/// Where the state machine is waiting for its next evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Scoring simplex rows `next..=n` one tell at a time: the initial simplex (from row
    /// 0) or the shrunk one (from row 1).
    Scoring {
        next: u32,
    },
    Reflecting,
    Expanding,
    Contracting {
        outside: bool,
    },
}

impl NelderMead {
    /// Initial simplex: clamped init point plus one vertex per param,
    /// perturbed by 5% of the param's range (flipped negative if clamping
    /// would collapse it, jittered if the range itself is degenerate).
    pub(crate) fn new(params: &[Param]) -> Self {
        let n = params.len();
        assert!(n >= 1, "a simplex needs a dimension");
        let lo: Vec<f64> = params.iter().map(|p| p.min).collect();
        let hi: Vec<f64> = params.iter().map(|p| p.max).collect();
        let init: Vec<f64> = params.iter().map(|p| p.init.clamp(p.min, p.max)).collect();

        let mut simplex = Vec::with_capacity((n + 1) * n);
        simplex.extend_from_slice(&init);
        for i in 0..n {
            let mut v = init.clone();
            let step = 0.05 * (hi[i] - lo[i]);
            let up = (v[i] + step).clamp(lo[i], hi[i]);
            v[i] = if up != v[i] {
                up
            } else {
                let down = (v[i] - step).clamp(lo[i], hi[i]);
                if down != v[i] {
                    down
                } else {
                    // min == max (or fp-degenerate range): nudge so the
                    // simplex still spans every dimension.
                    v[i] + 1e-9 * (1.0 + v[i].abs())
                }
            };
            simplex.extend_from_slice(&v);
        }

        assert_eq!(simplex.len(), (n + 1) * n);
        assert_eq!(lo.len(), hi.len());
        NelderMead {
            lo,
            hi,
            candidate: init,
            simplex,
            scores: vec![UNEVALUATED; n + 1],
            phase: Phase::Scoring { next: 0 },
            reflected: Vec::new(),
            reflected_score: UNEVALUATED,
        }
    }

    pub(crate) fn candidate(&self) -> &[f64] {
        assert_eq!(self.candidate.len(), self.n(), "one value per param");
        &self.candidate
    }

    /// Record the score for the current candidate and compute the next one.
    pub(crate) fn tell(&mut self, score: f64) {
        assert!(!score.is_nan(), "scores must be comparable");
        let n = self.n();
        assert_eq!(self.scores.len(), n + 1, "one score per simplex row");
        match self.phase {
            Phase::Scoring { next } => {
                self.scores[next as usize] = score;
                let next = next as usize + 1;
                if next <= n {
                    self.candidate = self.row(next).to_vec();
                    self.phase = Phase::Scoring { next: next as u32 };
                } else {
                    self.sort();
                    self.begin_iteration();
                }
            }
            Phase::Reflecting => {
                if score < self.scores[0] {
                    // Best so far: try going further in the same direction.
                    self.reflected = self.candidate.clone();
                    self.reflected_score = score;
                    self.candidate = self.along_centroid(&self.reflected, GAMMA);
                    self.phase = Phase::Expanding;
                } else if score < self.scores[n - 1] {
                    let point = mem::take(&mut self.candidate);
                    self.replace_worst(&point, score);
                } else {
                    self.reflected = self.candidate.clone();
                    self.reflected_score = score;
                    let outside = score < self.scores[n];
                    let toward = if outside {
                        &self.reflected
                    } else {
                        self.row(n)
                    };
                    self.candidate = self.along_centroid(toward, RHO);
                    self.phase = Phase::Contracting { outside };
                }
            }
            Phase::Expanding => {
                // Keep whichever of expansion/reflection scored lower.
                if score < self.reflected_score {
                    let point = mem::take(&mut self.candidate);
                    self.replace_worst(&point, score);
                } else {
                    let point = mem::take(&mut self.reflected);
                    self.replace_worst(&point, self.reflected_score);
                }
            }
            Phase::Contracting { outside } => {
                let accept = if outside {
                    score <= self.reflected_score
                } else {
                    score < self.scores[n]
                };
                if accept {
                    let point = mem::take(&mut self.candidate);
                    self.replace_worst(&point, score);
                } else {
                    // Contraction failed: shrink everything toward the best
                    // vertex, then re-evaluate rows 1..=n one tell at a time.
                    let best = self.row(0).to_vec();
                    for i in 1..=n {
                        for (x, &b) in self.simplex[i * n..(i + 1) * n].iter_mut().zip(&best) {
                            *x = b + SIGMA * (*x - b);
                        }
                        self.scores[i] = UNEVALUATED;
                    }
                    self.candidate = self.row(1).to_vec();
                    self.phase = Phase::Scoring { next: 1 };
                }
            }
        }
        assert_eq!(self.candidate.len(), n, "the next candidate is a point");
    }
}

// ---- private ----

impl NelderMead {
    fn n(&self) -> usize {
        debug_assert_eq!(self.lo.len(), self.hi.len(), "bounds in step");
        self.lo.len()
    }

    fn row(&self, i: usize) -> &[f64] {
        let n = self.n();
        assert!(i <= n, "row {i} of a {n}-dimensional simplex");
        &self.simplex[i * n..(i + 1) * n]
    }

    /// `c + k·(t − c)` clamped into the bounds, `c` the centroid: reflection (`t` the
    /// worst row, `k = −ALPHA`), expansion and contraction.
    fn along_centroid(&self, t: &[f64], k: f64) -> Vec<f64> {
        let c = self.centroid();
        assert_eq!(t.len(), c.len());
        let x: Vec<f64> = (0..c.len())
            .map(|i| (c[i] + k * (t[i] - c[i])).clamp(self.lo[i], self.hi[i]))
            .collect();
        assert_eq!(x.len(), self.n());
        x
    }

    /// Centroid of the best n vertices (all rows but the worst).
    fn centroid(&self) -> Vec<f64> {
        let n = self.n();
        let mut c = vec![0.0; n];
        for i in 0..n {
            for (c, &x) in c.iter_mut().zip(self.row(i)) {
                *c += x;
            }
        }
        for c in &mut c {
            *c /= n as f64;
        }
        assert_eq!(c.len(), n);
        c
    }

    /// Sort simplex rows by score ascending (best first, worst last).
    fn sort(&mut self) {
        let n = self.n();
        let mut order: Vec<usize> = (0..=n).collect();
        order.sort_by(|&a, &b| self.scores[a].total_cmp(&self.scores[b]));
        let old_simplex = mem::take(&mut self.simplex);
        let old_scores = mem::take(&mut self.scores);
        self.simplex = Vec::with_capacity(old_simplex.len());
        self.scores = Vec::with_capacity(old_scores.len());
        for &src in &order {
            self.simplex
                .extend_from_slice(&old_simplex[src * n..(src + 1) * n]);
            self.scores.push(old_scores[src]);
        }
        assert_eq!(self.simplex.len(), old_simplex.len());
        debug_assert!(self.scores.is_sorted_by(|a, b| a.total_cmp(b).is_le()));
    }

    fn replace_worst(&mut self, point: &[f64], score: f64) {
        let n = self.n();
        assert_eq!(point.len(), n);
        assert!(!score.is_nan(), "scores must be comparable");
        self.simplex[n * n..(n + 1) * n].copy_from_slice(point);
        self.scores[n] = score;
        self.sort();
        self.begin_iteration();
    }

    /// Start a fresh iteration: reflect the worst vertex through the
    /// centroid of the rest and hand that out as the candidate.
    fn begin_iteration(&mut self) {
        assert!(
            self.scores.is_sorted_by(|a, b| a.total_cmp(b).is_le()),
            "reflection takes the worst row last"
        );
        self.candidate = self.along_centroid(self.row(self.n()), -ALPHA);
        self.phase = Phase::Reflecting;
    }
}
