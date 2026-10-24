//! Ask-tell Nelder-Mead simplex over bounded params.
//!
//! Classic downhill simplex split into a state machine because evaluations
//! arrive one at a time (tell), instead of the algorithm driving a closure.
//! Every candidate is clamped to [lo, hi] before being handed out, so the
//! simplex only ever contains feasible points.

use super::Param;

const ALPHA: f64 = 1.0; // reflection
const GAMMA: f64 = 2.0; // expansion
const RHO: f64 = 0.5; // contraction
const SIGMA: f64 = 0.5; // shrink

/// Placeholder for not-yet-evaluated vertices; also the score of a failed evaluation,
/// so a failed candidate sorts worst.
pub(crate) const UNEVALUATED: f64 = f64::MAX;

/// Where the state machine is waiting for its next evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NelderMead {
    lo: Vec<f64>,
    hi: Vec<f64>,
    /// (n+1) x n row-major; rows kept sorted by score between iterations.
    simplex: Vec<f64>,
    /// n+1 scores, parallel to simplex rows.
    fvals: Vec<f64>,
    phase: Phase,
    /// The point handed out by `candidate()`; the next `tell` scores it.
    candidate: Vec<f64>,
    /// Reflected point kept across Reflecting -> Expanding/Contracting.
    reflected: Vec<f64>,
    f_reflected: f64,
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
        NelderMead {
            lo,
            hi,
            candidate: init,
            simplex,
            fvals: vec![UNEVALUATED; n + 1],
            phase: Phase::Scoring { next: 0 },
            reflected: Vec::new(),
            f_reflected: UNEVALUATED,
        }
    }

    pub(crate) fn candidate(&self) -> &[f64] {
        &self.candidate
    }

    /// Record the score for the current candidate and compute the next one.
    pub(crate) fn tell(&mut self, score: f64) {
        assert!(!score.is_nan(), "scores must be comparable");
        let n = self.n();
        match self.phase {
            Phase::Scoring { next } => {
                self.fvals[next as usize] = score;
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
                if score < self.fvals[0] {
                    // Best so far: try going further in the same direction.
                    self.reflected = self.candidate.clone();
                    self.f_reflected = score;
                    self.candidate = self.along_centroid(&self.reflected, GAMMA);
                    self.phase = Phase::Expanding;
                } else if score < self.fvals[n - 1] {
                    let point = std::mem::take(&mut self.candidate);
                    self.replace_worst(&point, score);
                } else {
                    self.reflected = self.candidate.clone();
                    self.f_reflected = score;
                    let outside = score < self.fvals[n];
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
                if score < self.f_reflected {
                    let point = std::mem::take(&mut self.candidate);
                    self.replace_worst(&point, score);
                } else {
                    let point = std::mem::take(&mut self.reflected);
                    self.replace_worst(&point, self.f_reflected);
                }
            }
            Phase::Contracting { outside } => {
                let accept = if outside {
                    score <= self.f_reflected
                } else {
                    score < self.fvals[n]
                };
                if accept {
                    let point = std::mem::take(&mut self.candidate);
                    self.replace_worst(&point, score);
                } else {
                    // Contraction failed: shrink everything toward the best
                    // vertex, then re-evaluate rows 1..=n one tell at a time.
                    let best = self.row(0).to_vec();
                    for i in 1..=n {
                        for (x, &b) in self.simplex[i * n..(i + 1) * n].iter_mut().zip(&best) {
                            *x = b + SIGMA * (*x - b);
                        }
                        self.fvals[i] = UNEVALUATED;
                    }
                    self.candidate = self.row(1).to_vec();
                    self.phase = Phase::Scoring { next: 1 };
                }
            }
        }
    }

    fn n(&self) -> usize {
        self.lo.len()
    }

    fn row(&self, i: usize) -> &[f64] {
        let n = self.n();
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
        order.sort_by(|&a, &b| self.fvals[a].total_cmp(&self.fvals[b]));
        let old_simplex = std::mem::take(&mut self.simplex);
        let old_fvals = std::mem::take(&mut self.fvals);
        self.simplex = Vec::with_capacity(old_simplex.len());
        self.fvals = Vec::with_capacity(old_fvals.len());
        for &src in &order {
            self.simplex
                .extend_from_slice(&old_simplex[src * n..(src + 1) * n]);
            self.fvals.push(old_fvals[src]);
        }
    }

    fn replace_worst(&mut self, point: &[f64], score: f64) {
        let n = self.n();
        assert_eq!(point.len(), n);
        self.simplex[n * n..(n + 1) * n].copy_from_slice(point);
        self.fvals[n] = score;
        self.sort();
        self.begin_iteration();
    }

    /// Start a fresh iteration: reflect the worst vertex through the
    /// centroid of the rest and hand that out as the candidate.
    fn begin_iteration(&mut self) {
        self.candidate = self.along_centroid(self.row(self.n()), -ALPHA);
        self.phase = Phase::Reflecting;
    }
}
