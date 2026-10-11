//! End to end with the real `ngspice` (and OpenVAF) from PATH, skipped when absent
//! unless `SCHEMIFY_REQUIRE_SIMULATORS` is set: an RC bench swept over R and two corners
//! against the analytic step response, the optimizer sizing R for a target time constant,
//! Monte Carlo yield, `jobs=1` vs `jobs=N`, reproducible ngspice `agauss` draws per
//! sample, a relative corner `.lib` found beside the bench, and an inline Verilog-A body
//! compiled by OpenVAF and simulated through OSDI.

mod common;

use common::{first_of, have_ngspice, project, rc, run, tmp};
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
    // ask → jobs → tell, one candidate in flight (Nelder–Mead is sequential).
    loop {
        match opt.step(&mut jobs, &results).unwrap() {
            Some(r) => {
                for j in r {
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

/// A bench whose resistor draws its value from ngspice's own `agauss`, as PDK statistical
/// sections do; the output is that value.
fn agauss_bench(seed: u32) -> String {
    format!(
        "schemify 1 shbench\n\nmontecarlo mc\n  samples 20\n  seed {seed}\n\n\
         output r \"v(out)/1m\"\n\ncode dut spice\n  | .param rmc = agauss(1k, 100, 1)\n  \
         | R1 out 0 {{rmc}}\n  | I1 0 out 1m\n  | .op\n"
    )
}

fn agauss_run(seed: u32, tag: &str) -> (Vec<f64>, String) {
    let bench = agauss_bench(seed);
    let (p, top) = project(&[("tb.shbench", &bench)]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let select = Select {
        montecarlo: Some("mc"),
        ..Select::default()
    };
    let dir = tmp(tag);
    let (jobs, results, runner) = run(&p, top, &plan, expand(&plan, &select).unwrap(), |d| {
        RunnerConfig::new(&dir, d)
    });
    let netlist = std::fs::read_to_string(runner.job_dir(0).join("netlist.cir")).unwrap();
    let want = format!(".option seed={}\n.end\n", jobs.seed(0).unwrap());
    assert!(netlist.ends_with(&want), "{netlist}");
    let values = (0..jobs.len())
        .map(|j| {
            let errs: Vec<_> = results.errors().collect();
            assert_eq!(jobs.state(j), State::Done, "{errs:?}");
            results.value(j, 0).unwrap()
        })
        .collect();
    (values, write_results(&jobs, &results))
}

#[test]
fn monte_carlo_seeds_make_ngspice_agauss_reproducible() {
    if !have_ngspice() {
        return;
    }
    // Without a seed ngspice seeds from its process id: two runs would differ.
    let (a, text_a) = agauss_run(11, "agauss-a");
    let (b, text_b) = agauss_run(11, "agauss-b");
    assert_eq!(a, b, "the same seed and sample draw the same PDK values");
    assert_eq!(text_a, text_b, "byte-identical results files");
    let mut distinct = a.clone();
    distinct.sort_by(f64::total_cmp);
    distinct.dedup();
    assert!(distinct.len() >= 18, "every sample draws anew: {a:?}");
    // agauss(1k, 100, 1): mean 1k, sd 100; 5-sigma bounds for 20 samples.
    let n = a.len() as f64;
    let mean = a.iter().sum::<f64>() / n;
    let sd = (a.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
    eprintln!("agauss: mean {mean:.1} sd {sd:.1}");
    assert!(
        (mean - 1000.0).abs() < 5.0 * 100.0 / n.sqrt(),
        "mean {mean}"
    );
    assert!((50.0..200.0).contains(&sd), "sd {sd}");
    let (c, _) = agauss_run(12, "agauss-c");
    assert_ne!(a, c, "another seed draws other values");
}

#[test]
fn a_relative_corner_lib_is_found_beside_the_bench() {
    if !have_ngspice() {
        return;
    }
    let bench = "schemify 1 shbench\n  corner tt\n\ncorner tt\n  lib models/rc.lib tt\n\n\
                 output v \"v(out)\"\n\ncode dut spice\n  | R1 out 0 {rval}\n  | I1 0 out 1m\n  \
                 | .op\n";
    let (p, top) = project(&[("tb.shbench", bench)]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let dir = tmp("relative-lib");
    std::fs::create_dir_all(dir.join("models")).unwrap();
    std::fs::write(
        dir.join("models/rc.lib"),
        "* corner models\n.lib tt\n.param rval=2k\n.endl tt\n",
    )
    .unwrap();
    let mut jobs = Jobs::new();
    jobs.push(None, None, schemify_run::NO_SAMPLE, &[]).unwrap();
    let (jobs, results, _) = run(&p, top, &plan, jobs, |d| RunnerConfig::new(&dir, d));
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.state(0), State::Done, "{errs:?}");
    let v = results.value(0, 0).unwrap();
    assert!(
        (v - 2.0).abs() < 1e-9,
        "the section's rval sets R1: v(out) = {v}"
    );
}

#[test]
fn a_verilog_a_body_compiles_with_openvaf_and_simulates_through_osdi() {
    if !have_ngspice() {
        return;
    }
    // OpenVAF-Reloaded (nixpkgs `openvaf`) installs `openvaf-r`.
    let Some(vaf) = first_of(&["openvaf", "openvaf-r"], "--version") else {
        return;
    };
    let vres = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\n\
                impl va veriloga\n  model vres_m\n\nbody va\n  | `include \"disciplines.vams\"\n  \
                | module vres(p, n);\n  |   inout p, n;\n  |   electrical p, n;\n  \
                |   parameter real r = 2000.0;\n  |   analog I(p, n) <+ V(p, n) / r;\n  \
                | endmodule\n";
    let bench = "schemify 1 shbench\n\ninst 0aaaaa ./vres\n  at 0 0 R0\n  name N1\n\n\
                 label 0bbbba 0 0 R0 out\nlabel 0bbbbb 0 20 R0 0\n\n\
                 output i \"i(v1)\"\n\ncode dut spice\n  | V1 out 0 1\n  | .op\n";
    let (p, top) = project(&[("vres.shprim", vres), ("tb.shbench", bench)]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let dir = tmp("openvaf-body");
    let mut two = Jobs::new();
    two.push(None, None, schemify_run::NO_SAMPLE, &[]).unwrap();
    two.push(None, None, schemify_run::NO_SAMPLE, &[]).unwrap();
    let (jobs, results, runner) = run(&p, top, &plan, two, |d| {
        let mut c = RunnerConfig::new(&dir, d);
        c.cache = dir.join("cache");
        c.tools.openvaf = vaf.to_owned();
        c
    });
    for j in 0..2 {
        let errs: Vec<_> = results.errors().collect();
        assert_eq!(jobs.state(j), State::Done, "{errs:?}");
        // V/R through the compiled model: |i(v1)| = 1 V / 2 kΩ.
        let i = results.value(j, 0).unwrap();
        assert!((i.abs() - 5e-4).abs() < 5e-9, "i(v1) = {i}");
        assert!(runner.job_dir(j).join("vres.osdi").exists());
    }
    let netlist = std::fs::read_to_string(runner.job_dir(0).join("netlist.cir")).unwrap();
    assert!(
        netlist.contains("pre_osdi vres.osdi") && netlist.contains(".model vres_m vres"),
        "{netlist}"
    );
    assert!(
        !netlist.contains("endmodule"),
        "the source is compiled, not emitted: {netlist}"
    );
    let src = std::fs::read_to_string(dir.join("cache/vres.va")).unwrap();
    assert!(
        src.starts_with("`include \"disciplines.vams\"\nmodule vres(p, n);\n"),
        "{src}"
    );
}
