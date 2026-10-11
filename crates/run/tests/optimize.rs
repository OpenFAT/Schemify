//! The optimizer bound to a bench `optimize` record, without a simulator: each job of a
//! candidate gets a synthetic raw file whose outputs are analytic functions of the
//! candidate's R and the job's corner, so the optimum is known in closed form (TESTING.md:
//! analytic optima). Goal weights and the worst-case aggregation over configs × corners
//! are checked against it, and a failed job makes its candidate a failed evaluation.

mod common;

use common::project;
use schemify_run::{Jobs, OptRun, Plan, Results, State};

/// An ASCII raw file of one point holding `v(f)` and `v(g)`.
fn raw(f: f64, g: f64) -> Vec<u8> {
    format!(
        "Title: t\nPlotname: Transient Analysis\nFlags: real\nNo. Variables: 3\n\
         No. Points: 1\nVariables:\n 0 time time\n 1 v(f) voltage\n 2 v(g) voltage\n\
         Values:\n0 0\n {f}\n {g}\n"
    )
    .into_bytes()
}

/// Runs optimize record `o` of a bench with var `R` in `[0, 10]` (start 1) and outputs
/// `f = v(f)`, `g = v(g)`; `sim(R, config, corner)` gives a job's `(f, g)`, or `None` to
/// fail the job.
fn optimize(
    records: &str,
    sim: impl Fn(f64, Option<&str>, Option<&str>) -> Option<(f64, f64)>,
) -> (OptRun, Jobs) {
    let bench = format!(
        "schemify 1 shbench\n\nvar R 1\n  min 0\n  max 10\n\n{records}\n\
         output f \"v(f)\"\noutput g \"v(g)\"\n"
    );
    let (p, top) = project(&[("tb.shbench", &bench)]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let mut opt = OptRun::new(&plan, "o").unwrap();
    let mut jobs = Jobs::new();
    let mut results = Results::new(&plan, [7; 32]);
    for _ in 0..100_000 {
        if opt.done() {
            break;
        }
        let Some(r) = opt.step(&mut jobs, &results).unwrap() else {
            continue;
        };
        assert_eq!(r.end - r.start, opt.jobs_per_candidate());
        for j in r {
            let x = schemify_expr::parse_num(jobs.assigned(j, "R").unwrap()).unwrap();
            let fg = sim(x, jobs.config(j), jobs.corner(j));
            match fg {
                Some((f, g)) => results.record(&plan, &mut jobs, j, &raw(f, g)),
                None => results.fail(&mut jobs, j, "simulator failed"),
            }
            assert!(jobs.state(j).finished());
        }
    }
    assert!(opt.done(), "the budget ends the loop");
    assert_eq!(jobs.len(), opt.opt.n_evals() * opt.jobs_per_candidate());
    (opt, jobs)
}

#[test]
fn goal_weights_scale_their_errors() {
    // Score 3·(R − 1)² + (R − 4)²: zero slope at R = 1.75, score 6.75.
    let (opt, jobs) = optimize(
        "optimize o\n  goal f min weight 3\n  goal g min\n  vary R\n  budget 200\n",
        |r, _, _| Some(((r - 1.0).powi(2), (r - 4.0).powi(2))),
    );
    let best = opt.opt.best().unwrap();
    assert!((best.params[0] - 1.75).abs() < 1e-3, "{:?}", best.params);
    assert!((best.score - 6.75).abs() < 1e-5, "{}", best.score);
    assert_eq!(opt.jobs_per_candidate(), 1);
    assert_eq!(
        (jobs.config(0), jobs.corner(0)),
        (None, None),
        "the bench defaults"
    );
    // Unit weights would put the optimum at R = 2.5 instead.
    let (unit, _) = optimize(
        "optimize o\n  goal f min\n  goal g min\n  vary R\n  budget 200\n",
        |r, _, _| Some(((r - 1.0).powi(2), (r - 4.0).powi(2))),
    );
    assert!((unit.opt.best().unwrap().params[0] - 2.5).abs() < 1e-3);
}

#[test]
fn candidates_run_every_config_and_corner_and_score_their_worst() {
    // f = (R − c)², c = 1 at corner a and 3 at corner b: the worst case max((R−1)², (R−3)²)
    // is smallest at R = 2 (score 1). Candidates past R = 5 fail at corner b.
    let records = "config x\n  order spice\nconfig y\n  order spice\n\ncorner a\ncorner b\n\n\
                   optimize o\n  goal f min\n  vary R\n  config x\n  config y\n  corner a\n  \
                   corner b\n  budget 200\n";
    let (opt, jobs) = optimize(records, |r, cfg, corner| {
        assert!(matches!(cfg, Some("x" | "y")), "{cfg:?}");
        match corner {
            Some("a") => Some(((r - 1.0).powi(2), 0.0)),
            Some("b") if r > 5.0 => None,
            Some("b") => Some(((r - 3.0).powi(2), 0.0)),
            other => panic!("corner {other:?}"),
        }
    });
    assert_eq!(opt.jobs_per_candidate(), 4);
    let points: Vec<(Option<&str>, Option<&str>)> =
        (0..4).map(|j| (jobs.config(j), jobs.corner(j))).collect();
    assert_eq!(
        points,
        [
            (Some("x"), Some("a")),
            (Some("x"), Some("b")),
            (Some("y"), Some("a")),
            (Some("y"), Some("b")),
        ],
        "config outer, corner inner"
    );
    let best = opt.opt.best().unwrap();
    assert!((best.params[0] - 2.0).abs() < 1e-3, "{:?}", best.params);
    assert!((best.score - 1.0).abs() < 1e-3, "{}", best.score);
    assert_eq!(opt.best_job(), Some(opt.eval_job(best.index)));
    // Every job of a candidate shares its assignment; a failed job fails the evaluation.
    let mut failed = 0;
    for i in 0..opt.opt.n_evals() {
        let e = opt.opt.eval(i).unwrap();
        let first = opt.eval_job(i);
        for j in first..first + 4 {
            assert_eq!(jobs.assigned(j, "R"), jobs.assigned(first, "R"));
        }
        if e.params[0] > 5.0 {
            assert_eq!(e.score, f64::MAX, "candidate {i} at R = {}", e.params[0]);
            assert!((first..first + 4).any(|j| jobs.state(j) == State::Failed));
            failed += 1;
        }
    }
    eprintln!("{} evaluations, {failed} failed", opt.opt.n_evals());
}
