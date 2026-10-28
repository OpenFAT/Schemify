//! The runner with a fake simulator (`sh`; on Windows the one Git for Windows puts on `PATH`):
//! `jobs=1` vs `jobs=N` byte-identical results, the results file, the design hash and
//! staleness, gate and job failures, timeouts, cancellation, the Monte Carlo seed lines,
//! bench-relative and project-relative corner paths. The compile steps are `sources.rs`.

mod common;

use common::{CAP, FAKE, RC_BENCH, RES, cfg, project, rc, run, sh, tmp};
use schemify_doc::{DocHandle, Project};
use schemify_elab::{Dialect, no_funcs};
use schemify_run::{
    Blocked, Jobs, NO_SAMPLE, Plan, Results, Runner, Select, SimCommand, State, design, expand,
    prepare, read_results, write_results,
};
use std::path::Path;
use std::time::Duration;

// ---- private ----

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
    // Every Monte Carlo sample fixes ngspice's seed (run spec §3): in its netlist and in
    // the results file, and sample k has the same seed at every sweep point and corner.
    assert_eq!(text.matches("\n  seed ").count(), 24);
    for j in 0..jobs.len() {
        let seed = jobs.seed(j).expect("an MC sample");
        assert_eq!(seed, schemify_run::sim_seed(11, jobs.sample(j)));
        let netlist = std::fs::read_to_string(runner.job_dir(j).join("netlist.cir")).unwrap();
        assert!(
            netlist.ends_with(&format!(".option seed={seed}\n.end\n")),
            "{netlist}"
        );
    }
    let (j0, j4) = (jobs.seed(0), jobs.seed(4));
    assert_eq!((jobs.sample(0), jobs.sample(4)), (0, 0));
    assert_eq!(j0, j4, "common random numbers across sweep points");
    assert_ne!(jobs.seed(0), jobs.seed(1), "samples differ");
}

/// Relative corner `lib`/`include` paths name files beside the bench: the job's netlist
/// carries them absolute; absolute paths stay as written.
#[test]
fn relative_corner_paths_become_absolute_in_the_job_netlist() {
    let bench = RC_BENCH.replace(
        "corner tt\n  set C 1u\n",
        "corner tt\n  lib models/rc.lib tt\n  include sub/extra.inc\n  lib /pdk/abs.lib ss\n  \
         set C 1u\n",
    );
    assert_ne!(bench, RC_BENCH);
    let (p, top) = project(&[
        ("res.shprim", RES),
        ("cap.shprim", CAP),
        ("tb.shbench", &bench),
    ]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let mut jobs = Jobs::new();
    jobs.push(None, Some("tt"), NO_SAMPLE, &[]).unwrap();
    let dir = tmp("relative-paths");
    let (jobs, _, runner) = run(&p, top, &plan, jobs, |d| cfg(&dir, d, sh(FAKE), 1));
    assert_eq!(jobs.state(0), State::Done);
    let netlist = std::fs::read_to_string(runner.job_dir(0).join("netlist.cir")).unwrap();
    let abs = |rel: &str| {
        let a = dir.join(rel).to_string_lossy().into_owned();
        if a.contains(char::is_whitespace) {
            format!("\"{a}\"")
        } else {
            a
        }
    };
    let want = format!(
        "\n.lib {} tt\n.include {}\n.lib /pdk/abs.lib ss\n",
        abs("models/rc.lib"),
        abs("sub/extra.inc")
    );
    assert!(netlist.contains(&want), "{want}\n{netlist}");
}

fn digest(p: &Project, top: DocHandle, jobs: &Jobs, env: &[(&str, &str)]) -> schemify_doc::Digest {
    design(p, top, jobs, env, Dialect::Ngspice, &mut no_funcs).unwrap()
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
    match design(&p, top, &Jobs::new(), &[], Dialect::Ngspice, &mut no_funcs) {
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
        env: Vec::new(),
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
    let digest = design(&p, top, &jobs, &[], Dialect::Ngspice, &mut no_funcs).unwrap();
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

/// A corner path that starts with a project variable (`${PDK_MODELS}`, written by
/// `pdk.use` as a project-relative path) is relative to the project root, wherever the bench
/// lives; a literal relative path stays relative to the bench's directory (pdk spec Q3).
#[test]
fn project_variable_corner_paths_are_relative_to_the_project_root() {
    let bench = RC_BENCH.replace(
        "corner tt\n  set C 1u\n",
        "corner tt\n  lib ${PDK_MODELS} tt\n  include sub/extra.inc\n  lib ${HOME_LIB}/x.lib ss\n  \
         set C 1u\n",
    );
    let manifest = b"schemify 1 project\n\nset PDK_MODELS .schemify/pdks/k/v/models/corners.lib\n";
    let mut p = Project::new(Some(manifest), None, 7).unwrap();
    let files = [
        ("benches/res.shprim", RES),
        ("benches/cap.shprim", CAP),
        ("benches/tb.shbench", bench.as_str()),
    ];
    let listing: Vec<(&str, &[u8])> = files.iter().map(|(n, t)| (*n, t.as_bytes())).collect();
    p.index_library("", "", &listing).unwrap();
    let mut top = None;
    for (name, text) in files {
        let h = p.open("", name, text.as_bytes(), None).unwrap().handle;
        if name.ends_with(".shbench") {
            top = Some(h);
        }
    }
    let top = top.unwrap();
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let mut jobs = Jobs::new();
    jobs.push(None, Some("tt"), NO_SAMPLE, &[]).unwrap();
    let dir = tmp("project-var-paths");
    // `HOME_LIB` comes from the environment, not the project: it stays bench-relative.
    let env = [("HOME_LIB", "envlib")];
    let (jobs, _, runner) =
        common::run_env(&p, top, &plan, jobs, &env, |d| cfg(&dir, d, sh(FAKE), 1));
    assert_eq!(jobs.state(0), State::Done);
    let netlist = std::fs::read_to_string(runner.job_dir(0).join("netlist.cir")).unwrap();
    // Joined as the runner joins them (one component per base), so the separators match on
    // Windows too.
    let from = |base: &Path, rel: &str| base.join(rel).to_string_lossy().into_owned();
    let bench_dir = dir.join("benches");
    let want = format!(
        "\n.lib {} tt\n.include {}\n.lib {} ss\n",
        from(&dir, ".schemify/pdks/k/v/models/corners.lib"),
        from(&bench_dir, "sub/extra.inc"),
        from(&bench_dir, "envlib/x.lib")
    );
    assert!(netlist.contains(&want), "{want}\n{netlist}");
}
