//! Job expansion: count = product of dimensions, determinism, stable ordering
//! (properties over random plans); Monte Carlo assignment statistics against the
//! declared distributions; bench records that cannot run.

// The oracle loops run `0..n.max(1)` over possibly empty name lists (the default
// dimension), so an iterator over the list does not fit.
#![allow(clippy::needless_range_loop)]

mod common;

use common::project;
use proptest::prelude::*;
use schemify_expr::{Mode, Program, Scratch, Value};
use schemify_run::{
    Corner, Dist, DistKind, MonteCarlo, NO_SAMPLE, Plan, Select, Sweep, Var, expand,
};

fn plan(configs: usize, corners: usize, sweeps: &[usize], samples: u32, dists: usize) -> Plan {
    let mut p = Plan {
        configs: (0..configs).map(|c| format!("cfg{c}")).collect(),
        corners: (0..corners)
            .map(|c| Corner {
                name: format!("k{c}"),
                sets: vec![("v0".into(), format!("{c}00"))],
            })
            .collect(),
        sweeps: sweeps
            .iter()
            .enumerate()
            .map(|(s, &n)| Sweep {
                name: format!("s{s}"),
                var: format!("w{s}"),
                values: (0..n).map(|k| format!("{k}")).collect(),
            })
            .collect(),
        montecarlos: vec![MonteCarlo {
            name: "mc".into(),
            samples: samples.max(1),
            seed: 5,
        }],
        ..Plan::default()
    };
    for d in 0..dists {
        p.vars.push(Var {
            name: format!("v{d}"),
            expr: "1k".into(),
            min: None,
            max: None,
        });
        p.dists.push(Dist {
            var: format!("v{d}"),
            kind: DistKind::Gauss,
            spread: "10".into(),
            rel: false,
        });
    }
    p
}

fn as_refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

fn names(prefix: &str, n: usize) -> Vec<String> {
    (0..n).map(|k| format!("{prefix}{k}")).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn expansion_is_the_ordered_product(
        nc in 0usize..3, nk in 0usize..3,
        sweeps in proptest::collection::vec(1usize..4, 0..3),
        samples in 0u32..4, dists in 0usize..3,
    ) {
        let p = plan(nc, nk, &sweeps, samples, dists);
        let (cfgs, ks, ss) = (names("cfg", nc), names("k", nk), names("s", sweeps.len()));
        let (cfgs, ks, ss) = (as_refs(&cfgs), as_refs(&ks), as_refs(&ss));
        let over = [("cli", "42")];
        let select = Select {
            configs: &cfgs, corners: &ks, sweeps: &ss,
            montecarlo: (samples > 0).then_some("mc"),
            overrides: &over,
        };
        let jobs = expand(&p, &select).unwrap();
        // Count = product of dimensions.
        let n_samples = if samples > 0 { samples as usize } else { 1 };
        let want = nc.max(1) * nk.max(1) * sweeps.iter().product::<usize>() * n_samples;
        prop_assert_eq!(jobs.len() as usize, want);
        // Deterministic.
        prop_assert_eq!(&jobs, &expand(&p, &select).unwrap());
        // Ordering: config, corner, sweep point (first sweep outermost), sample.
        let mut j = 0u32;
        let mut point = vec![0usize; sweeps.len()];
        for c in 0..nc.max(1) {
            for k in 0..nk.max(1) {
                let total_points: usize = sweeps.iter().product();
                for flat in 0..total_points {
                    let mut rest = flat;
                    for i in (0..sweeps.len()).rev() {
                        point[i] = rest % sweeps[i];
                        rest /= sweeps[i];
                    }
                    for s in 0..n_samples as u32 {
                        prop_assert_eq!(jobs.config(j), (nc > 0).then(|| cfgs[c]));
                        prop_assert_eq!(jobs.corner(j), (nk > 0).then(|| ks[k]));
                        let mc = samples > 0;
                        prop_assert_eq!(jobs.sample(j), if mc { s } else { NO_SAMPLE });
                        let a: Vec<(&str, &str)> = jobs.assignments(j).collect();
                        let ndist = if mc { dists } else { 0 };
                        prop_assert_eq!(a.len(), sweeps.len() + ndist + 1);
                        for (i, &pt) in point.iter().enumerate() {
                            let (var, val) = (ss[i].replace('s', "w"), pt.to_string());
                            prop_assert_eq!(a[i], (var.as_str(), val.as_str()));
                        }
                        for d in 0..ndist {
                            let (var, text) = a[sweeps.len() + d];
                            prop_assert_eq!(var, format!("v{d}"));
                            // Base: the corner's `set` for v0, else the var expression.
                            let base = if d == 0 && nk > 0 { format!("({k}00)") } else { "(1k)".into() };
                            prop_assert!(text.starts_with(&base), "{} {}", text, base);
                        }
                        prop_assert_eq!(a[a.len() - 1], ("cli", "42"));
                        j += 1;
                    }
                }
            }
        }
        prop_assert_eq!(j, jobs.len());
    }
}

fn eval(text: &str) -> f64 {
    let p = Program::compile(text, Mode::Scalar).unwrap();
    match p.eval(&[], &mut Scratch::default()).unwrap() {
        Value::Num(x) => x,
        Value::Int(i) => i as f64,
        v => panic!("{v:?}"),
    }
}

fn mc_values(kind: DistKind, spread: &str, rel: bool, n: u32) -> Vec<f64> {
    let mut p = plan(0, 0, &[], n, 0);
    p.vars.push(Var {
        name: "R".into(),
        expr: "1k".into(),
        min: None,
        max: None,
    });
    p.dists.push(Dist {
        var: "R".into(),
        kind,
        spread: spread.into(),
        rel,
    });
    let jobs = expand(
        &p,
        &Select {
            montecarlo: Some("mc"),
            ..Select::default()
        },
    )
    .unwrap();
    assert_eq!(jobs.len(), n);
    (0..n)
        .map(|j| eval(jobs.assigned(j, "R").unwrap()))
        .collect()
}

fn moments(xs: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    (
        mean,
        (xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt(),
    )
}

#[test]
fn monte_carlo_assignments_follow_the_declared_distribution() {
    // 5-sigma bounds for the sample mean and standard deviation, fixed seed.
    let n = 8000u32;
    let sn = f64::from(n).sqrt();
    let (m, sd) = moments(&mc_values(DistKind::Gauss, "50", false, n));
    assert!((m - 1000.0).abs() < 5.0 * 50.0 / sn, "gauss mean {m}");
    assert!(
        (sd - 50.0).abs() < 5.0 * 50.0 / (2.0f64).sqrt() / sn,
        "gauss sd {sd}"
    );
    let (m, sd) = moments(&mc_values(DistKind::Gauss, "0.05", true, n));
    assert!(
        (m - 1000.0).abs() < 5.0 * 50.0 / sn,
        "relative gauss mean {m}"
    );
    assert!(
        (sd - 50.0).abs() < 5.0 * 50.0 / (2.0f64).sqrt() / sn,
        "relative gauss sd {sd}"
    );
    let u = mc_values(DistKind::Uniform, "100", false, n);
    assert!(u.iter().all(|x| (900.0..1100.0).contains(x)));
    let (m, sd) = moments(&u);
    let sd_want = 100.0 / 3f64.sqrt();
    assert!((m - 1000.0).abs() < 5.0 * sd_want / sn, "uniform mean {m}");
    assert!((sd - sd_want).abs() < 0.02 * sd_want, "uniform sd {sd}");
}

#[test]
fn bench_records_read_into_a_plan() {
    let bench = common::RC_BENCH;
    let (p, top) = project(&[
        ("res.shprim", common::RES),
        ("cap.shprim", common::CAP),
        ("tb.shbench", bench),
    ]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    assert_eq!(plan.vars.len(), 2);
    assert_eq!(
        plan.dists.len(),
        1,
        "`dist` is an unknown attribute the format keeps"
    );
    assert_eq!(plan.dists[0].kind, DistKind::Gauss);
    assert_eq!(
        plan.corners
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["ff", "tt"]
    );
    assert_eq!(plan.sweeps[0].values, ["1k", "2k", "3k"]);
    assert_eq!(
        (plan.montecarlos[0].samples, plan.montecarlos[0].seed),
        (4, 11)
    );
    assert_eq!(plan.optimizes[0].budget, 40);
    assert_eq!(
        plan.outputs
            .iter()
            .map(|o| o.name.as_str())
            .collect::<Vec<_>>(),
        ["tau", "v1m"]
    );
    assert_eq!(plan.default_corner.as_deref(), Some("tt"));
}

fn read_err(extra: &str) -> String {
    let bench = format!("schemify 1 shbench\n\nvar R 1k\n\n{extra}");
    let (p, top) = project(&[("tb.shbench", &bench)]);
    Plan::read(p.doc(top).unwrap()).unwrap_err().to_string()
}

#[test]
fn records_that_cannot_run_are_refused() {
    assert!(read_err("sweep s\n  values 1 2\n").contains("no `var`"));
    assert!(read_err("sweep s\n  var R\n  values 1\n  lin 1 2 3\n").contains("exactly one"));
    assert!(read_err("sweep s\n  var R\n  log 0 1 3\n").contains("nonzero"));
    assert!(read_err("montecarlo m\n  seed 1\n").contains("no `samples`"));
    assert!(read_err("montecarlo m\n  samples 0\n").contains("count"));
    assert!(read_err("optimize o\n  goal x min\n  vary R\n").contains("no output"));
    assert!(read_err("output x \"v(a\"\n\nspec y\n").contains("output x"));
    assert!(read_err("output x \"1\"\n\nspec y\n").contains("names no output"));
    assert!(read_err("optimize o\n  goal x min\n  vary R\n\noutput x \"1\"\n").contains("min"));
    let bad_dist = |d: &str| {
        let bench = format!("schemify 1 shbench\n\nvar R 1k\n  dist {d}\n");
        let (p, top) = project(&[("tb.shbench", &bench)]);
        Plan::read(p.doc(top).unwrap()).unwrap_err().detail
    };
    assert!(bad_dist("lognormal 1").contains("gauss|uniform"));
    assert!(bad_dist("gauss 0.1*R").contains("cannot reference"));
    assert!(bad_dist("gauss 1 abs").contains("gauss|uniform"));
}

#[test]
fn selections_naming_what_the_bench_lacks_are_refused() {
    let p = plan(1, 2, &[2], 2, 0);
    let e = |s: Select<'_>| expand(&p, &s).unwrap_err().detail;
    assert!(
        e(Select {
            configs: &["nope"],
            ..Select::default()
        })
        .contains("no config")
    );
    assert!(
        e(Select {
            corners: &["nope"],
            ..Select::default()
        })
        .contains("no corner")
    );
    assert!(
        e(Select {
            sweeps: &["nope"],
            ..Select::default()
        })
        .contains("no sweep")
    );
    assert!(
        e(Select {
            montecarlo: Some("nope"),
            ..Select::default()
        })
        .contains("no montecarlo")
    );
    let big = plan(0, 0, &[1024, 1024], 2, 0);
    let err = expand(
        &big,
        &Select {
            sweeps: &["s0", "s1"],
            montecarlo: Some("mc"),
            ..Select::default()
        },
    );
    assert!(err.unwrap_err().detail.contains("exceed"));
}
