//! End to end with the real `ngspice` from PATH (skipped when absent): an RC bench swept
//! over R and two corners against the analytic step response, the optimizer sizing R
//! for a target time constant, Monte Carlo yield, and `jobs=1` vs `jobs=N`.

mod common;

use common::{have_ngspice, rc, run, tmp};
use schemify_elab::{Dialect, no_funcs};
use schemify_run::{
    Jobs, OptRun, Pass, Plan, Results, Runner, RunnerConfig, Select, State, design, expand,
    prepare, read_results, write_results, yield_summary,
};

fn var(results: &Results, j: u32, name: &str) -> f64 {
    let v = results.vars().iter().position(|x| x == name).unwrap() as u16;
    results.var_value(j, v).unwrap()
}

#[test]
fn rc_sweep_and_corners_match_the_analytic_response() {
    if !have_ngspice() {
        return;
    }
    let (p, top) = rc();
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let select = Select {
        corners: &["ff", "tt"],
        sweeps: &["rsw"],
        ..Select::default()
    };
    let jobs = expand(&plan, &select).unwrap();
    assert_eq!(jobs.len(), 6, "3 sweep points × 2 corners");
    let dir = tmp("ngspice-rc");
    let (jobs, results, runner) = run(&p, top, &plan, jobs, |d| {
        let mut c = RunnerConfig::new(&dir, d);
        c.workers = 4;
        c
    });
    let (v1m, tau) = (plan.output("v1m").unwrap(), plan.output("tau").unwrap());
    for j in 0..jobs.len() {
        let errs: Vec<_> = results.errors().collect();
        assert_eq!(jobs.state(j), State::Done, "job {j}: {errs:?}");
        let (r, c) = (var(&results, j, "R"), var(&results, j, "C"));
        let want = 1.0 - (-1e-3 / (r * c)).exp();
        let got = results.value(j, v1m).unwrap();
        let t = results.value(j, tau).unwrap();
        eprintln!(
            "job {j} {:?} R={r} C={c}: v(1m)={got:.6} (analytic {want:.6}), tau={t:.4e} (RC {:.4e})",
            jobs.corner(j),
            r * c
        );
        assert!((got - want).abs() < 2e-3, "v(1m) {got} vs {want}");
        assert!((t / (r * c) - 1.0).abs() < 1e-2, "tau {t} vs RC {}", r * c);
        let pass = if (0.3..=0.9).contains(&want) {
            Pass::Pass
        } else {
            Pass::Fail
        };
        assert_eq!(results.pass(j, 0), pass, "job {j}");
    }
    // ff halves C: corner order is the outer dimension.
    assert_eq!((jobs.corner(0), jobs.corner(3)), (Some("ff"), Some("tt")));
    // The results file round-trips and reloads fresh against the same design.
    let path = runner.save(&jobs, &results).unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    let (j2, r2) = read_results(&text).unwrap();
    assert_eq!(write_results(&j2, &r2), text);
    assert!(!r2.stale(results.design()));
}

#[test]
fn one_worker_and_many_workers_write_identical_results() {
    if !have_ngspice() {
        return;
    }
    let (p, top) = rc();
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let select = Select {
        corners: &["ff", "tt"],
        sweeps: &["rsw"],
        ..Select::default()
    };
    let texts: Vec<String> = [1u16, 6]
        .into_iter()
        .map(|w| {
            let dir = tmp(&format!("ngspice-workers-{w}"));
            let (jobs, results, _) = run(&p, top, &plan, expand(&plan, &select).unwrap(), |d| {
                let mut c = RunnerConfig::new(&dir, d);
                c.workers = w;
                c
            });
            write_results(&jobs, &results)
        })
        .collect();
    assert_eq!(texts[0], texts[1]);
}

#[test]
fn optimizer_sizes_r_for_a_target_time_constant() {
    if !have_ngspice() {
        return;
    }
    let (p, top) = rc();
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let mut opt = OptRun::new(&plan, "fit").unwrap();
    let mut jobs = Jobs::new();
    let digest = design(&p, top, &jobs, &[], &mut no_funcs).unwrap();
    let mut results = Results::new(&plan, digest);
    let dir = tmp("ngspice-opt");
    let mut runner = Runner::new(RunnerConfig::new(&dir, &digest));
    let mut finished = Vec::new();
    // ask → job → tell, one candidate in flight (Nelder–Mead is sequential).
    loop {
        match opt.step(&mut jobs, &results).unwrap() {
            Some(j) => {
                let prepared = prepare(
                    &p,
                    top,
                    &mut jobs,
                    j,
                    &[],
                    Dialect::Ngspice,
                    &mut no_funcs,
                    &mut results,
                );
                runner.submit(&mut jobs, &mut results, j, &prepared.expect("netlists"));
            }
            None if opt.done() => break,
            None => {}
        }
        runner.wait(&plan, &mut jobs, &mut results, &mut finished);
    }
    let best = opt.best_job().unwrap();
    let (r, tau) = (
        var(&results, best, "R"),
        results.value(best, plan.output("tau").unwrap()).unwrap(),
    );
    eprintln!(
        "optimizer: {} evals, best job {best}: R={r:.2} tau={tau:.5e}",
        opt.opt.n_evals()
    );
    assert!((r / 1000.0 - 1.0).abs() < 1e-2, "R {r}");
    assert!((tau / 1e-3 - 1.0).abs() < 1e-2, "tau {tau}");
    assert!(opt.opt.n_evals() <= 40);
}

#[test]
fn monte_carlo_yield_over_real_simulations() {
    if !have_ngspice() {
        return;
    }
    let (p, top) = rc();
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let select = Select {
        montecarlo: Some("mc"),
        ..Select::default()
    };
    let jobs = expand(&plan, &select).unwrap();
    let dir = tmp("ngspice-mc");
    let (jobs, results, _) = run(&p, top, &plan, jobs, |d| RunnerConfig::new(&dir, d));
    // R = 3.3k ± 50 at C = 1u (tt): v(1m) ≈ 0.26 < 0.3, every sample fails the spec.
    let y = yield_summary(&plan, &jobs, &results);
    eprintln!("monte carlo: {y:?}");
    assert_eq!((y[0].pass, y[0].total), (0, 4));
    assert!(y[0].hi < 0.6);
    for j in 0..jobs.len() {
        let r = var(&results, j, "R");
        assert!((r - 3300.0).abs() < 5.0 * 50.0, "sample {j}: R {r}");
    }
}
