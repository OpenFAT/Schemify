//! The runner with a fake simulator (`sh`, so Unix only): `jobs=1` vs `jobs=N`
//! byte-identical results, the results file, the design hash and staleness, gate and
//! job failures, timeouts, cancellation, and the OSDI compile step.
#![cfg(unix)]

mod common;

use common::{CAP, RC_BENCH, RES, project, rc, run, tmp};
use schemify_doc::{DocHandle, Project};
use schemify_elab::{Dialect, no_funcs};
use schemify_run::{
    Blocked, Jobs, NO_SAMPLE, Plan, Results, Runner, RunnerConfig, Select, SimCommand, State,
    design, expand, prepare, read_results, write_results,
};
use std::path::Path;
use std::time::Duration;

/// Reads `.param R=` from the netlist and writes a two-point ASCII raw file with
/// v(out) going linearly from 0 to R over 1 s; sleeps a PID-dependent few ms so jobs
/// finish out of order.
const FAKE: &str = r#"R=$(sed -n 's/^\.param R=//p' "$1")
sleep "0.0$(($$ % 7))"
printf 'Title: fake\nPlotname: Transient Analysis\nFlags: real\nNo. Variables: 2\nNo. Points: 2\nVariables:\n 0 time time\n 1 v(out) voltage\nValues:\n0 0\n 0\n1 1\n %s\n' "$R" > "$2"
"#;

fn sh(script: &str) -> SimCommand {
    SimCommand {
        path: "sh".into(),
        args: ["-c", script, "fake", "{netlist}", "{raw}"]
            .map(String::from)
            .to_vec(),
    }
}

fn cfg(dir: &Path, d: &schemify_doc::Digest, sim: SimCommand, workers: u16) -> RunnerConfig {
    let mut c = RunnerConfig::new(dir, d);
    c.tools.sim = sim;
    c.workers = workers;
    c
}

fn all_jobs(plan: &Plan) -> Jobs {
    let select = Select {
        corners: &["ff", "tt"],
        sweeps: &["rsw"],
        montecarlo: Some("mc"),
        ..Select::default()
    };
    expand(plan, &select).unwrap()
}

#[test]
fn one_worker_and_eight_workers_write_identical_results() {
    let (p, top) = rc();
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let mut texts = Vec::new();
    for w in [1u16, 8] {
        let dir = tmp(&format!("fake-workers-{w}"));
        let (jobs, results, _) = run(&p, top, &plan, all_jobs(&plan), |d| {
            cfg(&dir, d, sh(FAKE), w)
        });
        assert_eq!(jobs.len(), 24, "2 corners × 3 points × 4 samples");
        let (v1m, r) = (plan.output("v1m").unwrap(), 0u16);
        assert_eq!(results.vars()[r as usize], "R");
        for j in 0..jobs.len() {
            assert_eq!(
                jobs.state(j),
                State::Done,
                "{:?}",
                results.errors().collect::<Vec<_>>()
            );
            let want = results.var_value(j, r).unwrap() * 1e-3;
            assert!((results.value(j, v1m).unwrap() - want).abs() < 1e-9 * want);
        }
        texts.push(write_results(&jobs, &results));
    }
    assert_eq!(texts[0], texts[1]);
    // MC varies R around each sweep point.
    let (jobs, _) = read_results(&texts[0]).unwrap();
    assert_eq!(jobs.sample(1), 1);
    assert!(jobs.assigned(1, "R").unwrap().starts_with("(1k)+(50)*("));
}

#[test]
fn results_file_round_trips() {
    let (p, top) = rc();
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let dir = tmp("fake-roundtrip");
    let (jobs, results, runner) = run(&p, top, &plan, all_jobs(&plan), |d| {
        cfg(&dir, d, sh(FAKE), 4)
    });
    let text = std::fs::read_to_string(runner.save(&jobs, &results).unwrap()).unwrap();
    assert_eq!(text, write_results(&jobs, &results));
    let (j2, r2) = read_results(&text).unwrap();
    assert_eq!(j2, jobs);
    assert_eq!(write_results(&j2, &r2), text);
    assert_eq!(r2.design(), results.design());
    // v1m = R·1e-3 ≥ 1 with the fake simulator: above the spec's max 0.9.
    assert_eq!(text.matches("\n  pass v1m fail\n").count(), 24);
}

fn digest(p: &Project, top: DocHandle, jobs: &Jobs, env: &[(&str, &str)]) -> schemify_doc::Digest {
    design(p, top, jobs, env, &mut no_funcs).unwrap()
}

#[test]
fn design_hash_tracks_reached_cells_bench_jobs_and_env() {
    let (p, top) = rc();
    let jobs = Jobs::new();
    let base = digest(&p, top, &jobs, &[]);
    assert_eq!(base, digest(&rc().0, rc().1, &jobs, &[]), "deterministic");
    let (pr, tr) = project(&[
        ("tb.shbench", RC_BENCH),
        ("cap.shprim", CAP),
        ("res.shprim", RES),
    ]);
    assert_eq!(
        base,
        digest(&pr, tr, &jobs, &[]),
        "independent of the listing order"
    );
    let unrelated = "schemify 1 shprim\n\npin A inout 0 0\n\nimpl spice spice\n";
    let (p2, t2) = project(&[
        ("res.shprim", RES),
        ("cap.shprim", CAP),
        ("tb.shbench", RC_BENCH),
        ("zz.shprim", unrelated),
    ]);
    assert_eq!(
        base,
        digest(&p2, t2, &jobs, &[]),
        "an unreached cell does not matter"
    );
    let res2 = RES.replace("param R 1k", "param R 2k");
    let (p3, t3) = project(&[
        ("res.shprim", &res2),
        ("cap.shprim", CAP),
        ("tb.shbench", RC_BENCH),
    ]);
    assert_ne!(base, digest(&p3, t3, &jobs, &[]), "a reached cell changed");
    let bench2 = RC_BENCH.replace("var R 3.3k", "var R 3.4k");
    let (p4, t4) = project(&[
        ("res.shprim", RES),
        ("cap.shprim", CAP),
        ("tb.shbench", &bench2),
    ]);
    let changed = digest(&p4, t4, &jobs, &[]);
    assert_ne!(base, changed, "the bench changed");
    assert_ne!(
        base,
        digest(&p, top, &jobs, &[("PDK", "/x")]),
        "environment"
    );
    let mut more = Jobs::new();
    more.push(None, None, NO_SAMPLE, &[("R", "1k")]).unwrap();
    assert_ne!(base, digest(&p, top, &more, &[]), "job list");
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let results = Results::new(&plan, base);
    assert!(!results.stale(&base) && results.stale(&changed));
}

#[test]
fn blocked_designs_and_unnetlistable_jobs() {
    let broken = RC_BENCH.replace("./cap", "./missing");
    let (p, top) = project(&[
        ("res.shprim", RES),
        ("cap.shprim", CAP),
        ("tb.shbench", &broken),
    ]);
    match design(&p, top, &Jobs::new(), &[], &mut no_funcs) {
        Err(Blocked::Violations(v)) => assert!(v.iter().any(|l| l.starts_with("E001")), "{v:?}"),
        other => panic!("{other:?}"),
    }
    let (p, top) = rc();
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let mut jobs = Jobs::new();
    let j = jobs.push(None, Some("nope"), NO_SAMPLE, &[]).unwrap();
    let mut results = Results::new(&plan, [1; 32]);
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
    assert!(prepared.is_none());
    assert_eq!(jobs.state(j), State::Failed);
    let (_, _, msg) = results.errors().next().unwrap();
    assert!(msg.contains("E035"), "{msg}");
}

fn single(sim: SimCommand, name: &str, timeout: Duration) -> (Jobs, Results) {
    let (p, top) = rc();
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let mut jobs = Jobs::new();
    jobs.push(None, None, NO_SAMPLE, &[]).unwrap();
    let dir = tmp(name);
    let (jobs, results, _) = run(&p, top, &plan, jobs, |d| {
        let mut c = cfg(&dir, d, sim, 1);
        c.timeout = timeout;
        c
    });
    (jobs, results)
}

fn failure(sim: SimCommand, name: &str, timeout: Duration) -> String {
    let (jobs, results) = single(sim, name, timeout);
    assert_eq!(jobs.state(0), State::Failed);
    results.errors().map(|e| e.2.to_owned()).next().unwrap()
}

#[test]
fn simulator_failures_fail_the_job_with_a_reason() {
    let hour = Duration::from_secs(3600);
    let missing = SimCommand {
        path: "no-such-simulator-xyz".into(),
        args: vec![],
    };
    assert!(failure(missing, "fail-missing", hour).contains("not found on PATH"));
    assert!(failure(sh("echo boom >&2; exit 3"), "fail-exit", hour).contains("boom"));
    assert!(failure(sh("exit 0"), "fail-noraw", hour).contains("no raw file"));
    assert!(failure(sh("echo garbage > \"$2\""), "fail-garbage", hour).contains("raw file"));
    let slow = failure(sh("sleep 5"), "fail-timeout", Duration::from_millis(200));
    assert!(slow.contains("timed out"), "{slow}");
    // A raw file without the output's signal: the job is done, the output has an error.
    let (jobs, results) = single(sh(&FAKE.replace("v(out)", "v(other)")), "fail-signal", hour);
    assert_eq!(jobs.state(0), State::Done);
    assert!(
        results
            .errors()
            .all(|(_, o, m)| o != schemify_run::JOB_LEVEL && m.contains("X004"))
    );
}

#[test]
fn a_bench_that_writes_its_own_raw_file_still_records_results() {
    // xschem-style benches `write <name>.raw` from their `.control` block, so the `-r`
    // file never appears: the runner falls back to the raw files the job wrote.
    let own = FAKE.replace("> \"$2\"", "> \"$(dirname \"$2\")/own.raw\"");
    assert_ne!(own, FAKE, "the fake writes elsewhere");
    let (jobs, results) = single(sh(&own), "own-raw", Duration::from_secs(3600));
    assert_eq!(jobs.state(0), State::Done);
    assert_eq!(results.errors().count(), 0);
    assert!(
        results.value(0, 0).is_some(),
        "the output was evaluated from own.raw"
    );
}

#[test]
fn cancel_kills_running_and_drops_queued_jobs() {
    let (p, top) = rc();
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let mut jobs = Jobs::new();
    for _ in 0..3 {
        jobs.push(None, None, NO_SAMPLE, &[]).unwrap();
    }
    let digest = design(&p, top, &jobs, &[], &mut no_funcs).unwrap();
    let mut results = Results::new(&plan, digest);
    let dir = tmp("cancel");
    let mut runner = Runner::new(cfg(&dir, &digest, sh("sleep 5"), 1));
    for j in 0..3 {
        let prepared = prepare(
            &p,
            top,
            &mut jobs,
            j,
            &[],
            Dialect::Ngspice,
            &mut no_funcs,
            &mut results,
        )
        .unwrap();
        runner.submit(&mut jobs, &mut results, j, &prepared);
    }
    let mut finished = Vec::new();
    runner.poll(&plan, &mut jobs, &mut results, &mut finished);
    assert_eq!(
        jobs.states(),
        [State::Running, State::Queued, State::Queued]
    );
    assert_eq!(runner.progress().count(), 1);
    runner.cancel(&mut jobs, 1);
    assert_eq!(jobs.state(1), State::Cancelled);
    runner.cancel_all(&mut jobs);
    assert!(!runner.busy() && finished.is_empty());
    assert!(jobs.states().iter().all(|s| *s == State::Cancelled));
}

const VRES: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nimpl va veriloga\n  model vres\n  file vres.va\n";
const VA_BENCH: &str = "schemify 1 shbench\n\ninst 0aaaaa ./vres\n  at 0 0 R0\n  name N1\n\n\
label 0bbbba 0 0 R0 out\nlabel 0bbbbb 0 20 R0 0\n\nvar R 1\n";

#[test]
fn verilog_a_compiles_once_into_every_job_or_fails_clearly() {
    let (p, top) = project(&[("vres.shprim", VRES), ("tb.shbench", VA_BENCH)]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let dir = tmp("osdi");
    std::fs::write(dir.join("vres.va"), "module vres; endmodule\n").unwrap();
    let two = || {
        let mut j = Jobs::new();
        j.push(None, None, NO_SAMPLE, &[]).unwrap();
        j.push(None, None, NO_SAMPLE, &[]).unwrap();
        j
    };
    // No OpenVAF: each job fails, naming the tool and the source.
    let (jobs, results, _) = run(&p, top, &plan, two(), |d| {
        let mut c = cfg(&dir, d, sh(FAKE), 2);
        c.cache = dir.join("cache");
        c.tools.openvaf = "openvaf-not-installed".into();
        c
    });
    assert_eq!(jobs.states(), [State::Failed, State::Failed]);
    let msg = results.errors().next().unwrap().2.to_owned();
    assert!(
        msg.contains("openvaf-not-installed")
            && msg.contains("not found")
            && msg.contains("vres.va"),
        "{msg}"
    );
    // A stand-in compiler that counts its runs: one compile, copied into both jobs.
    let fake = dir.join("fake-openvaf");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho run >> \"$1.count\"\ncp \"$1\" \"$3\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let (jobs, _, runner) = run(&p, top, &plan, two(), |d| {
        let mut c = cfg(&dir, d, sh(FAKE), 1);
        c.cache = dir.join("cache");
        c.tools.openvaf = fake.to_string_lossy().into_owned();
        c
    });
    assert_eq!(jobs.states(), [State::Done, State::Done]);
    assert!(
        runner.job_dir(0).join("vres.osdi").exists()
            && runner.job_dir(1).join("vres.osdi").exists()
    );
    let count = std::fs::read_to_string(dir.join("vres.va.count")).unwrap();
    assert_eq!(
        count.lines().count(),
        1,
        "mtime cache hit for the second job"
    );
    let netlist = std::fs::read_to_string(runner.job_dir(0).join("netlist.cir")).unwrap();
    assert!(netlist.contains("\nN1 out 0 vres\n"), "{netlist}");
}
