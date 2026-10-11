//! Fixtures shared by the run integration tests: an RC bench, project setup, the run
//! spine end to end, and a fake simulator.
#![allow(dead_code)] // each test binary uses a subset

use schemify_doc::{Digest, DocHandle, Project};
use schemify_elab::{Dialect, no_funcs};
use schemify_run::{Jobs, Plan, Results, Runner, RunnerConfig, design, prepare};
use std::path::PathBuf;

pub const RES: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nparam R 1k\n\n\
impl spice spice\n  format spice \"{name} {A} {B} {R}\"\n";

pub const CAP: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nparam C 1p\n\n\
impl spice spice\n  format spice \"{name} {A} {B} {C}\"\n";

/// RC low-pass charged from 1 V at t = 0: v(out) = 1 − exp(−t / RC).
pub const RC_BENCH: &str = r#"schemify 1 shbench
  corner tt

inst 0aaaaa ./res
  at 0 0 R0
  name R1
  R R
inst 0aaaab ./cap
  at 100 0 R0
  name C1
  C C

label 0bbbba 0 0 R0 in
label 0bbbbb 0 20 R0 out
label 0bbbbc 100 0 R0 out
label 0bbbbd 100 20 R0 0

var R 3.3k
  min 100
  max 10k
  dist gauss 50
var C 1u

corner ff
  set C 0.5u
corner tt
  set C 1u

sweep rsw
  var R
  values 1k 2k 3k

montecarlo mc
  samples 4
  seed 11

optimize fit
  algo nelder-mead
  goal tau target 1m
  vary R
  budget 40

output tau "cross(v(out), 0.6321205588285577)"
output v1m "at(v(out), 1m)"

spec v1m
  min 0.3
  max 0.9

code tran spice
  | V1 in 0 1
  | .ic v(out)=0
  | .tran 1u 5m 0 1u uic
"#;

/// Indexes and opens `files` (relpath, text) as the project's own library; returns the
/// project and the handle of the one `.shbench`.
pub fn project(files: &[(&str, &str)]) -> (Project, DocHandle) {
    let mut p = Project::new(None, None, 7).unwrap();
    let listing: Vec<(&str, &[u8])> = files.iter().map(|(n, t)| (*n, t.as_bytes())).collect();
    p.index_library("", "", &listing).unwrap();
    let mut top = None;
    for (name, text) in files {
        let h = p.open("", name, text.as_bytes(), None).unwrap().handle;
        if name.ends_with(".shbench") {
            top = Some(h);
        }
    }
    (p, top.expect("one bench"))
}

pub fn rc() -> (Project, DocHandle) {
    project(&[
        ("res.shprim", RES),
        ("cap.shprim", CAP),
        ("tb.shbench", RC_BENCH),
    ])
}

/// A fresh directory under the cargo test temp dir.
pub fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("schemify-run")
        .join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The run spine end to end: design → prepare → submit → wait.
pub fn run(
    p: &Project,
    top: DocHandle,
    plan: &Plan,
    mut jobs: Jobs,
    cfg: impl FnOnce(&Digest) -> RunnerConfig,
) -> (Jobs, Results, Runner) {
    let digest = design(p, top, &jobs, &[], &mut no_funcs).expect("not blocked");
    let mut results = Results::new(plan, digest);
    let mut runner = Runner::new(cfg(&digest));
    for j in 0..jobs.len() {
        let prepared = prepare(
            p,
            top,
            &mut jobs,
            j,
            &[],
            Dialect::Ngspice,
            &mut no_funcs,
            &mut results,
        );
        if let Some(prepared) = prepared {
            runner.submit(&mut jobs, &mut results, j, &prepared);
        }
    }
    let mut finished = Vec::new();
    runner.wait(plan, &mut jobs, &mut results, &mut finished);
    assert!(jobs.states().iter().all(|s| s.finished()));
    (jobs, results, runner)
}

/// Whether `ngspice` runs from PATH.
pub fn have_ngspice() -> bool {
    have("ngspice", "--version")
}

/// Whether `tool` starts from PATH (called with `arg`). Tests skip without it, except
/// under `SCHEMIFY_REQUIRE_SIMULATORS` (the CI step whose Nix shell provides ngspice,
/// Icarus and OpenVAF), where a missing tool fails the test instead of skipping it.
pub fn have(tool: &str, arg: &str) -> bool {
    let ok = std::process::Command::new(tool).arg(arg).output().is_ok();
    if !ok {
        assert!(
            std::env::var_os("SCHEMIFY_REQUIRE_SIMULATORS").is_none(),
            "{tool} must be on PATH (SCHEMIFY_REQUIRE_SIMULATORS is set)"
        );
        eprintln!("{tool} not on PATH: skipping");
    }
    ok
}
