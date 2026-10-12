//! The runner with a fake simulator (`sh`; on Windows the one Git for Windows puts on `PATH`):
//! `jobs=1` vs `jobs=N` byte-identical results, the results file, the design hash and
//! staleness, gate and job failures, timeouts, cancellation, the Monte Carlo seed lines,
//! bench-relative corner paths, and the compile steps (stand-in compilers that never block
//! the poll, the real Icarus when present).

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
    // A stand-in compiler that counts its runs: one compile, copied into both jobs. A script
    // run as a binary needs a shebang and an exec bit: Unix only.
    #[cfg(unix)]
    {
        let fake = dir.join("fake-openvaf");
        stand_in(&fake, "echo run >> \"$1.count\"\ncp \"$1\" \"$3\"\n");
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
}

/// A Verilog-A cell whose source is its `body` (no `file`).
#[cfg(unix)]
const VBODY: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\n\
impl va veriloga\n  model vres\n\nbody va\n  | module vres(p, n);\n  | endmodule\n";

/// Writes an executable stand-in compiler (`<src> -o <out>`, OpenVAF's argument order).
///
/// A child shell writes it: a file this multi-threaded test process held open for writing
/// could be inherited by another test's simulator between its fork and exec, and running
/// it would then fail with ETXTBSY ("text file busy").
#[cfg(unix)]
fn stand_in(path: &Path, script: &str) {
    let ok = std::process::Command::new("sh")
        .args([
            "-c",
            "printf '%s' \"$2\" > \"$1\" && chmod 755 \"$1\"",
            "sh",
        ])
        .arg(path)
        .arg(format!("#!/bin/sh\n{script}"))
        .status()
        .unwrap()
        .success();
    assert!(ok && path.exists(), "{}", path.display());
}

/// Two jobs of the `VBODY` bench prepared and submitted to a runner whose OpenVAF is
/// `openvaf`, with its own build cache under `dir`.
#[cfg(unix)]
fn body_run(dir: &Path, openvaf: &Path) -> (Plan, Jobs, Results, Runner) {
    let (p, top) = project(&[("vres.shprim", VBODY), ("tb.shbench", VA_BENCH)]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let mut jobs = Jobs::new();
    jobs.push(None, None, NO_SAMPLE, &[]).unwrap();
    jobs.push(None, None, NO_SAMPLE, &[]).unwrap();
    let digest = design(&p, top, &jobs, &[], &mut no_funcs).unwrap();
    let mut results = Results::new(&plan, digest);
    let mut c = cfg(dir, &digest, sh(FAKE), 2);
    c.cache = dir.join("cache");
    c.tools.openvaf = openvaf.to_string_lossy().into_owned();
    let mut runner = Runner::new(c);
    for j in 0..2 {
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
        assert!(
            !prepared.netlist.contains("endmodule"),
            "compiled, not emitted"
        );
        runner.submit(&mut jobs, &mut results, j, &prepared);
    }
    (plan, jobs, results, runner)
}

/// An inline body compiles as a child process beside the poll (run spec §5): the first
/// poll returns at once with both jobs waiting on one compile; they finish through
/// polling; the next run of the same body hits the cache. A failing compile fails every
/// job that needs it, with the compiler's last line.
#[cfg(unix)]
#[test]
fn a_body_compiles_beside_the_poll_once_for_every_job() {
    use std::time::Instant;
    let dir = tmp("osdi-body");
    let slow = dir.join("slow-openvaf");
    stand_in(
        &slow,
        "sleep 1\necho run >> \"$1.count\"\ncp \"$1\" \"$3\"\n",
    );
    let (plan, mut jobs, mut results, mut runner) = body_run(&dir, &slow);
    let mut finished = Vec::new();
    let t = Instant::now();
    runner.poll(&plan, &mut jobs, &mut results, &mut finished);
    let first = t.elapsed();
    assert!(
        first < Duration::from_millis(500),
        "the poll waited for the compiler: {first:?}"
    );
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.states(), [State::Queued, State::Queued], "{errs:?}");
    let progress: Vec<&str> = runner.progress().map(|p| p.1).collect();
    assert_eq!(progress, ["compiling sources"; 2]);
    runner.wait(&plan, &mut jobs, &mut results, &mut finished);
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.states(), [State::Done, State::Done], "{errs:?}");
    let cache = dir.join("cache");
    let src = std::fs::read_to_string(cache.join("vres.va")).unwrap();
    assert_eq!(
        src, "module vres(p, n);\nendmodule\n",
        "the body, written for the compiler"
    );
    let count = std::fs::read_to_string(cache.join("vres.va.count")).unwrap();
    assert_eq!(count.lines().count(), 1, "one compile serves both jobs");
    assert!(
        runner.job_dir(0).join("vres.osdi").exists()
            && runner.job_dir(1).join("vres.osdi").exists()
    );
    drop(runner);
    // Unchanged body, fresh output: no compile, the simulators start in the first poll.
    let (plan, mut jobs, mut results, mut runner) = body_run(&dir, &slow);
    runner.poll(&plan, &mut jobs, &mut results, &mut finished);
    assert_eq!(jobs.states(), [State::Running, State::Running]);
    runner.wait(&plan, &mut jobs, &mut results, &mut finished);
    let count = std::fs::read_to_string(cache.join("vres.va.count")).unwrap();
    assert_eq!(count.lines().count(), 1, "cache hit");
    // A compiler that fails: every job needing its output fails with its last line.
    let dir = tmp("osdi-body-fail");
    let bad = dir.join("bad-openvaf");
    stand_in(&bad, "echo 'vres.va:1: syntax error' >&2\nexit 1\n");
    let (plan, mut jobs, mut results, mut runner) = body_run(&dir, &bad);
    runner.wait(&plan, &mut jobs, &mut results, &mut finished);
    assert_eq!(jobs.states(), [State::Failed, State::Failed]);
    for (_, _, msg) in results.errors() {
        assert!(
            msg.contains("syntax error") && msg.contains("failed"),
            "{msg}"
        );
    }
}

/// A Verilog `body` compiles with the real Icarus (`iverilog`, skipped when absent unless
/// `SCHEMIFY_REQUIRE_SIMULATORS` is set) into the `.vvp` the `d_cosim` model names.
#[test]
fn a_verilog_body_compiles_with_icarus() {
    if !common::have("iverilog", "-V") {
        return;
    }
    let inv = "schemify 1 shprim\n\npin a in -20 0\n  domain digital\npin y out 20 0\n  \
               domain digital\n\nimpl v verilog\n  model inv_m\n\nbody v\n  \
               | module inv(input a, output y);\n  |   assign y = ~a;\n  | endmodule\n";
    let bench = "schemify 1 shbench\n\ninst 0aaaaa ./inv\n  at 0 0 R0\n  name U1\n\n\
                 label 0bbbba -20 0 R0 a\nlabel 0bbbbb 20 0 R0 y\n\nvar R 1\n";
    let (p, top) = project(&[("inv.shprim", inv), ("tb.shbench", bench)]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let mut jobs = Jobs::new();
    jobs.push(None, None, NO_SAMPLE, &[]).unwrap();
    let dir = tmp("icarus-body");
    let (jobs, results, runner) = run(&p, top, &plan, jobs, |d| {
        let mut c = cfg(&dir, d, sh(FAKE), 1);
        c.cache = dir.join("cache");
        c
    });
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.state(0), State::Done, "{errs:?}");
    let netlist = std::fs::read_to_string(runner.job_dir(0).join("netlist.cir")).unwrap();
    assert!(
        netlist.contains("d_cosim simulation=\"inv.vvp\""),
        "{netlist}"
    );
    let vvp = std::fs::read_to_string(runner.job_dir(0).join("inv.vvp")).unwrap();
    assert!(vvp.contains(":ivl_version") && vvp.contains("inv"), "{vvp}");
    let src = std::fs::read_to_string(dir.join("cache/inv.v")).unwrap();
    assert!(src.starts_with("module inv(input a, output y);\n"), "{src}");
}

/// A generated FinFET cell (pdk spec §2.2): an OSDI device whose Verilog-A `file` is relative
/// to the PDK cell, reached through a `tech:` class.
const FINFET: &str = "schemify 1 shprim\n  prefix M\n\npin d inout 20 -30\npin g in -20 0\npin s inout 20 30\npin b in 20 0\n\nparam nfin 1\nparam m 1\n\nimpl osdi veriloga\n  format spice \"N{name} {d} {g} {s} {b} nfet nfin={nfin} m={m}\"\n  file ../../va/cmg.va\n";
const NMOS4: &str = "schemify 1 shprim\n  prefix M\n\npin d inout 20 -30\npin g in -20 0\npin s inout 20 30\npin b in 20 0\n\nparam model nmos\n  kind string\nparam w 1u\nparam l 0.18u\nparam m 1\n\nimpl spice spice\n\nline -20 0 -10 0\n";
const FIN_MAP: &str = "schemify 1 techmap\n  pdk fin\n\nclass nmos core\n  cell fin:nfet\n  param nfin max(1,ceil(w/70.5n))\n";
const FIN_BENCH: &str = "schemify 1 shbench\n\ninst 0aaaaa tech:nmos\n  at 0 0 R0\n  name M1\n  w 1u\n\nlabel 0bbbba 20 -30 R0 d\nlabel 0bbbbb -20 0 R0 g\nlabel 0bbbbc 20 30 R0 0\nlabel 0bbbbd 20 0 R0 0\n";

#[test]
fn tech_cells_compile_their_pdk_cells_sources() {
    let manifest = "schemify 1 project\nlib fin path pdk/lib/fin\nlib tech map pdk/tech.map\n";
    let mut p = Project::new(Some(manifest.as_bytes()), None, 7).unwrap();
    p.index_library("core", "<builtin>", &[("nmos4.shprim", NMOS4.as_bytes())])
        .unwrap();
    p.index_library("", "", &[("tb.shbench", FIN_BENCH.as_bytes())])
        .unwrap();
    p.index_library("fin", "pdk/lib/fin", &[("nfet.shprim", FINFET.as_bytes())])
        .unwrap();
    p.index_library("tech", "pdk/tech.map", &[("tech.map", FIN_MAP.as_bytes())])
        .unwrap();
    let top = p
        .open("", "tb.shbench", FIN_BENCH.as_bytes(), None)
        .unwrap()
        .handle;
    let req = schemify_elab::Request {
        config: None,
        corner: None,
        overrides: &[],
        env: &[],
        dialect: Dialect::Ngspice,
    };
    let e = schemify_elab::elaborate(&p, top, &req, &mut no_funcs);
    assert!(!e.violations.blocking(), "{:?}", e.violations.rows);
    // The source is the PDK cell's file, from the PDK cell's library and directory (not
    // `core/…` in the `tech` library: the derived row has no directory on disk).
    let fin = p.libs().find("fin").unwrap() as u8;
    assert_eq!(
        schemify_run::sources(&p, &e),
        [schemify_run::Source {
            tool: schemify_run::Tool::Openvaf,
            origin: schemify_run::Origin::File {
                lib: fin,
                path: "../../va/cmg.va".into()
            },
            out: "cmg.osdi".into(),
        }]
    );
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
