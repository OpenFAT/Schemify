//! Monte Carlo (feature `mc`, run spec §3): seeded per-sample variation of the vars that
//! carry `dist`, and the yield summary over a run's samples.
//!
//! A draw is a pure function of `(seed, sample, var name)` (splitmix64 streams), so
//! sample `k` of var `R` is the same whatever other vars, corners or sweeps the run has:
//! corners see common random numbers. Normal draws use Box–Muller evaluated by the
//! expression language, whose transcendentals are `libm` (bit-identical on every
//! target, D32).

use crate::jobs::{Jobs, NO_SAMPLE, State};
use crate::plan::{DistKind, MonteCarlo, Plan};
use crate::results::{Pass, Results};
use schemify_expr::{Mode, Program, Scratch, Value, write_num};
use std::sync::OnceLock;

/// Appends sample `s` of `m`: one assignment per var with a `dist` (plan order, sorted by
/// var), each varying its base (run spec §3) by a fresh draw.
pub(crate) fn sample<'p>(
    plan: &'p Plan,
    corner: Option<&str>,
    m: &MonteCarlo,
    s: u32,
    assigns: &mut Vec<(&'p str, String)>,
) {
    assert!(s < m.samples, "sample {s} of {}", m.samples);
    let before = assigns.len();
    for d in &plan.dists {
        let z = draw(d.kind, m.seed, s, &d.var);
        let base = base_expr(plan, corner, &d.var, assigns).unwrap_or("0");
        let text = vary_text(base, &d.spread, d.rel, z);
        assigns.push((&d.var, text));
    }
    assert_eq!(assigns.len(), before + plan.dists.len());
}

/// The expression a variation applies to: the latest assignment of `var` in this job,
/// else the corner's last `set` of it, else the var's own expression.
fn base_expr<'a>(
    plan: &'a Plan,
    corner: Option<&str>,
    var: &str,
    assigns: &'a [(&str, String)],
) -> Option<&'a str> {
    if let Some((_, x)) = assigns.iter().rev().find(|(v, _)| *v == var) {
        return Some(x);
    }
    let set = corner
        .and_then(|c| plan.corner(c))
        .and_then(|c| c.sets.iter().rev().find(|(v, _)| v == var));
    set.map(|(_, x)| x.as_str())
        .or_else(|| plan.var(var).map(|v| v.expr.as_str()))
}

/// `(base)+(spread)*(z)`, or `(base)*(1+(spread)*(z))` when relative: still an
/// expression, so elab evaluates it in bench scope like any other assignment.
fn vary_text(base: &str, spread: &str, rel: bool, z: f64) -> String {
    let mut zt = String::new();
    write_num(&mut zt, z);
    let out = if rel {
        format!("({base})*(1+({spread})*({zt}))")
    } else {
        format!("({base})+({spread})*({zt})")
    };
    assert!(out.len() > base.len() + spread.len());
    out
}

/// One draw: standard normal for [`DistKind::Gauss`], uniform on `[-1, 1)` for
/// [`DistKind::Uniform`]. Pure in `(seed, sample, var)`.
pub fn draw(kind: DistKind, seed: u64, sample: u32, var: &str) -> f64 {
    let mut st = stream(seed, sample, var);
    // u1 in (0, 1] (ln is finite), u2 in [0, 1).
    let u1 = ((splitmix64(&mut st) >> 11) + 1) as f64 / (1u64 << 53) as f64;
    let u2 = (splitmix64(&mut st) >> 11) as f64 / (1u64 << 53) as f64;
    assert!(u1 > 0.0 && u1 <= 1.0 && (0.0..1.0).contains(&u2));
    match kind {
        DistKind::Uniform => 2.0 * u2 - 1.0,
        DistKind::Gauss => {
            static BOX_MULLER: OnceLock<Program> = OnceLock::new();
            let p = BOX_MULLER.get_or_init(|| {
                Program::compile("sqrt(-2 * ln(u1)) * cos(2 * pi * u2)", Mode::Scalar)
                    .expect("constant source")
            });
            match p.eval(&[Value::Num(u1), Value::Num(u2)], &mut Scratch::default()) {
                Ok(Value::Num(z)) => z,
                other => unreachable!("Box–Muller is finite for u1 in (0, 1]: {other:?}"),
            }
        }
    }
}

/// The simulator's random seed for sample `sample` of a Monte Carlo record seeded `seed`
/// (run spec §3): in `1..=2^30`, pure in `(seed, sample)`. The netlist sets it, so the
/// simulator's own random functions (PDK `agauss` in `.param` and model cards) draw the
/// same values for the same sample in every run, corner and sweep point.
pub fn sim_seed(seed: u64, sample: u32) -> u32 {
    // The stream of the empty name: no var is named "" (format names are nonempty).
    let mut st = stream(seed, sample, "");
    let s = (splitmix64(&mut st) >> 34) as u32 + 1;
    assert!((1..=1 << 30).contains(&s));
    s
}

fn stream(seed: u64, sample: u32, var: &str) -> u64 {
    // FNV-1a 64 of the var name keys the stream by name, not by position.
    let h = var.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    let mut s = seed ^ h.rotate_left(29);
    let a = splitmix64(&mut s);
    a ^ u64::from(sample).wrapping_mul(0xd1b5_4a32_d192_ed03)
}

fn splitmix64(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Pass rate of one spec over a run's finished Monte Carlo samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Yield {
    /// Spec index (into [`Plan::specs`]).
    pub spec: u16,
    /// Samples that passed.
    pub pass: u32,
    /// Finished samples (a failed simulation or a missing output counts as a failure).
    pub total: u32,
    /// Lower end of the 95% Wilson interval of the pass rate.
    pub lo: f64,
    /// Upper end.
    pub hi: f64,
}

/// Yield per spec over the finished jobs that are Monte Carlo samples (run spec §3).
/// Cost: O(jobs × specs).
pub fn yield_summary(plan: &Plan, jobs: &Jobs, results: &Results) -> Vec<Yield> {
    assert_eq!(results.specs(), plan.specs.len());
    let mut out: Vec<Yield> = (0..plan.specs.len())
        .map(|k| Yield {
            spec: k as u16,
            pass: 0,
            total: 0,
            lo: 0.0,
            hi: 1.0,
        })
        .collect();
    for j in 0..jobs.len() {
        let st = jobs.state(j);
        if jobs.sample(j) == NO_SAMPLE || !st.finished() || st == State::Cancelled {
            continue;
        }
        for y in &mut out {
            y.total += 1;
            y.pass += u32::from(st == State::Done && results.pass(j, y.spec) == Pass::Pass);
        }
    }
    for y in &mut out {
        (y.lo, y.hi) = wilson(y.pass, y.total);
        assert!(y.pass <= y.total && y.lo <= y.hi);
    }
    out
}

/// 95% Wilson score interval of `pass / total` (`[0, 1]` when `total` is 0).
pub fn wilson(pass: u32, total: u32) -> (f64, f64) {
    assert!(pass <= total);
    if total == 0 {
        return (0.0, 1.0);
    }
    const Z: f64 = 1.959_963_984_540_054;
    let (n, p) = (f64::from(total), f64::from(pass) / f64::from(total));
    let denom = 1.0 + Z * Z / n;
    let center = (p + Z * Z / (2.0 * n)) / denom;
    let half = Z * (p * (1.0 - p) / n + Z * Z / (4.0 * n * n)).sqrt() / denom;
    let (lo, hi) = ((center - half).max(0.0), (center + half).min(1.0));
    assert!(lo <= p + 1e-12 && p <= hi + 1e-12);
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moments(xs: &[f64]) -> (f64, f64) {
        let n = xs.len() as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
        (mean, var)
    }

    #[test]
    fn draws_match_their_distributions() {
        // N = 20000, fixed seed. 5-sigma bounds on the sample mean and variance.
        let n = 20_000u32;
        let g: Vec<f64> = (0..n).map(|s| draw(DistKind::Gauss, 7, s, "R")).collect();
        let (m, v) = moments(&g);
        let sd_mean = (1.0 / f64::from(n)).sqrt();
        let sd_var = (2.0 / f64::from(n - 1)).sqrt();
        assert!(m.abs() < 5.0 * sd_mean, "gauss mean {m}");
        assert!((v - 1.0).abs() < 5.0 * sd_var, "gauss variance {v}");
        let u: Vec<f64> = (0..n).map(|s| draw(DistKind::Uniform, 7, s, "R")).collect();
        assert!(u.iter().all(|x| (-1.0..1.0).contains(x)));
        let (m, v) = moments(&u);
        // Uniform(-1,1): variance 1/3, variance of the variance (1/n)(1/5 - 1/9).
        assert!(
            m.abs() < 5.0 * (1.0 / 3.0 / f64::from(n)).sqrt(),
            "uniform mean {m}"
        );
        let sd_var_u = ((1.0 / 5.0 - 1.0 / 9.0) / f64::from(n)).sqrt();
        assert!(
            (v - 1.0 / 3.0).abs() < 5.0 * sd_var_u,
            "uniform variance {v}"
        );
    }

    #[test]
    fn streams_are_keyed_by_seed_sample_and_name() {
        let a = draw(DistKind::Gauss, 1, 0, "R");
        assert_eq!(a, draw(DistKind::Gauss, 1, 0, "R"));
        assert_ne!(a, draw(DistKind::Gauss, 2, 0, "R"));
        assert_ne!(a, draw(DistKind::Gauss, 1, 1, "R"));
        assert_ne!(a, draw(DistKind::Gauss, 1, 0, "C"));
        // Independent streams: R and C draws are uncorrelated.
        let n = 10_000u32;
        let r: Vec<f64> = (0..n).map(|s| draw(DistKind::Gauss, 3, s, "R")).collect();
        let c: Vec<f64> = (0..n).map(|s| draw(DistKind::Gauss, 3, s, "C")).collect();
        let cov = r.iter().zip(&c).map(|(a, b)| a * b).sum::<f64>() / f64::from(n);
        assert!(cov.abs() < 5.0 / f64::from(n).sqrt(), "cov {cov}");
    }

    #[test]
    fn sim_seeds_are_pure_distinct_and_in_range() {
        assert_eq!(sim_seed(11, 3), sim_seed(11, 3));
        let mut seen: Vec<u32> = (0..1000).map(|s| sim_seed(11, s)).collect();
        assert!(seen.iter().all(|s| (1..=1 << 30).contains(s)));
        seen.sort_unstable();
        seen.dedup();
        assert!(
            seen.len() > 995,
            "samples get distinct seeds: {}",
            seen.len()
        );
        assert_ne!(
            sim_seed(11, 0),
            sim_seed(12, 0),
            "the record's seed matters"
        );
    }

    #[test]
    fn wilson_matches_reference_values() {
        let close = |(lo, hi): (f64, f64), (a, b): (f64, f64)| {
            assert!(
                (lo - a).abs() < 1e-4 && (hi - b).abs() < 1e-4,
                "{lo} {hi} vs {a} {b}"
            );
        };
        close(wilson(8, 10), (0.4902, 0.9433));
        close(wilson(0, 10), (0.0, 0.2775));
        close(wilson(10, 10), (0.7225, 1.0));
        close(wilson(50, 100), (0.4038, 0.5962));
        assert_eq!(wilson(0, 0), (0.0, 1.0));
    }
}
