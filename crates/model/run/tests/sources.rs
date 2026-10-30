//! The compile steps of the runner (run spec §5) with a fake simulator: stand-in compilers
//! that never block the poll and compile once for every job, the real Icarus when present,
//! sources of derived `tech:` cells, and ESPice's copied Verilog-A sources and includes.

mod common;

use common::FAKE;
use schemify_doc::Project;
use schemify_elab::Dialect;
use schemify_run::{Jobs, NO_SAMPLE, Plan, RunnerConfig, SimCommand, State};
// The stand-in compilers are scripts run as binaries: Unix only.
#[cfg(unix)]
use schemify_run::{Results, Runner};
use std::fs;
#[cfg(unix)]
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const VRES: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nimpl va veriloga\n  model vres\n  file vres.va\n";

const VA_BENCH: &str = "schemify 1 shbench\n\ninst 0aaaaa ./vres\n  at 0 0 R0\n  name N1\n\n\
label 0bbbba 0 0 R0 out\nlabel 0bbbbb 0 20 R0 0\n\nvar R 1\n";

/// A Verilog-A cell whose source is its `body` (no `file`).
#[cfg(unix)]
const VBODY: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\n\
impl va veriloga\n  model vres\n\nbody va\n  | module vres(p, n);\n  | endmodule\n";

/// A generated FinFET cell (pdk spec §2.2): an OSDI device whose Verilog-A `file` is relative
/// to the PDK cell, reached through a `tech:` class.
const FINFET: &str = "schemify 1 shprim\n  prefix M\n\npin d inout 20 -30\npin g in -20 0\npin s inout 20 30\npin b in 20 0\n\nparam nfin 1\nparam m 1\n\nimpl osdi veriloga\n  format spice \"N{name} {d} {g} {s} {b} nfet nfin={nfin} m={m}\"\n  file ../../va/cmg.va\n";

const NMOS4: &str = "schemify 1 shprim\n  prefix M\n\npin d inout 20 -30\npin g in -20 0\npin s inout 20 30\npin b in 20 0\n\nparam model nmos\n  kind string\nparam w 1u\nparam l 0.18u\nparam m 1\n\nimpl spice spice\n\nline -20 0 -10 0\n";

const FIN_MAP: &str = "schemify 1 techmap\n  pdk fin\n\nclass nmos core\n  cell fin:nfet\n  param nfin max(1,ceil(w/70.5n))\n";

const FIN_BENCH: &str = "schemify 1 shbench\n\ninst 0aaaaa tech:nmos\n  at 0 0 R0\n  name M1\n  w 1u\n\nlabel 0bbbba 20 -30 R0 d\nlabel 0bbbbb -20 0 R0 g\nlabel 0bbbbc 20 30 R0 0\nlabel 0bbbbd 20 0 R0 0\n";

/// A Verilog-A cell whose source is its `body`, beside `VRES` (a `file`) in one bench.
const VGAIN: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\n\
impl va veriloga\n  model vgain_m\n\nbody va\n  | module vgain(p, n);\n  | endmodule\n";

const ES_BENCH: &str = "schemify 1 shbench\n\ninst 0aaaaa ./vres\n  at 0 0 R0\n  name N1\n\
inst 0aaaab ./vgain\n  at 100 0 R0\n  name N2\n\n\
label 0bbbba 0 0 R0 out\nlabel 0bbbbb 0 20 R0 0\nlabel 0bbbbc 100 0 R0 out\nlabel 0bbbbd 100 20 R0 0\n\nvar R 1\n";

/// A stand-in ESPice: fails unless the netlist loads both sources with `.hdl`, has no OSDI
/// load, finds them in its working directory and gets `ZIG` from the `sim` row's `env`.
const FAKE_ESPICE: &str = r#"grep -qx '.hdl "vres.va"' "$1" && grep -qx '.hdl "vgain.va"' "$1" || { echo 'no .hdl' >&2; exit 3; }
grep -q pre_osdi "$1" && { echo 'pre_osdi' >&2; exit 4; }
[ -f vres.va ] && [ -f vgain.va ] || { echo 'sources missing' >&2; exit 5; }
[ "$ZIG" = /fake/zig ] || { echo "ZIG=$ZIG" >&2; exit 6; }
printf 'Title: fake\nPlotname: Operating Point\nFlags: real\nNo. Variables: 1\nNo. Points: 1\nVariables:\n 0 v(out) voltage\nValues:\n0 1.5\n' > "$2"
"#;

/// A Verilog-A `file` cell whose source includes other files (BSIM-CMG's layout).
const VTOP: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nimpl va veriloga\n  model vtop\n  file va/vtop.va\n";

const INC_BENCH: &str = "schemify 1 shbench\n\ninst 0aaaaa ./vtop\n  at 0 0 R0\n  name N1\n\n\
label 0bbbba 0 0 R0 out\nlabel 0bbbbb 0 20 R0 0\n\nvar R 1\n";

/// A stand-in ESPice that fails unless the includes sit beside `vtop.va` in their layout.
const FAKE_INC: &str = r#"for f in vtop.va macros.include sub/a.inc sub/b.inc; do [ -f "$f" ] || { echo "missing $f" >&2; exit 5; }; done
[ -e nope.inc ] && { echo 'copied a commented include' >&2; exit 6; }
printf 'Title: fake\nPlotname: Operating Point\nFlags: real\nNo. Variables: 1\nNo. Points: 1\nVariables:\n 0 v(out) voltage\nValues:\n0 1\n' > "$2"
"#;

// ---- private ----

#[test]
fn verilog_a_compiles_once_into_every_job_or_fails_clearly() {
    let (p, top) = common::project(&[("vres.shprim", VRES), ("tb.shbench", VA_BENCH)]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let dir = common::tmp("osdi");
    fs::write(dir.join("vres.va"), "module vres; endmodule\n").unwrap();
    let two = || {
        let mut j = Jobs::new();
        j.push(None, None, NO_SAMPLE, &[]).unwrap();
        j.push(None, None, NO_SAMPLE, &[]).unwrap();
        j
    };
    // No OpenVAF: each job fails, naming the tool and the source.
    let (jobs, results, _) = common::run(&p, top, &plan, two(), |d| {
        let mut c = common::cfg(&dir, d, common::sh(FAKE), 2);
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
        let (jobs, _, runner) = common::run(&p, top, &plan, two(), |d| {
            let mut c = common::cfg(&dir, d, common::sh(FAKE), 1);
            c.cache = dir.join("cache");
            c.tools.openvaf = fake.to_string_lossy().into_owned();
            c
        });
        assert_eq!(jobs.states(), [State::Done, State::Done]);
        assert!(
            runner.job_dir(0).join("vres.osdi").exists()
                && runner.job_dir(1).join("vres.osdi").exists()
        );
        let count = fs::read_to_string(dir.join("vres.va.count")).unwrap();
        assert_eq!(
            count.lines().count(),
            1,
            "mtime cache hit for the second job"
        );
        let netlist = fs::read_to_string(runner.job_dir(0).join("netlist.cir")).unwrap();
        assert!(netlist.contains("\nN1 out 0 vres\n"), "{netlist}");
    }
}

/// Writes an executable stand-in compiler (`<src> -o <out>`, OpenVAF's argument order).
///
/// A child shell writes it: a file this multi-threaded test process held open for writing
/// could be inherited by another test's simulator between its fork and exec, and running
/// it would then fail with ETXTBSY ("text file busy").
#[cfg(unix)]
fn stand_in(path: &Path, script: &str) {
    let ok = Command::new("sh")
        .args([
            "-c",
            "printf '%s' \"$2\" > \"$1\" && chmod 755 \"$1\"",
            "sh",
        ])
        .arg(path)
        .arg(format!("#!/bin/sh\n{script}"))
        .status()
        .expect("sh runs")
        .success();
    assert!(ok && path.exists(), "{}", path.display());
}

/// Two jobs of the `VBODY` bench prepared and submitted to a runner whose OpenVAF is
/// `openvaf`, with its own build cache under `dir`.
#[cfg(unix)]
fn body_run(dir: &Path, openvaf: &Path) -> (Plan, Jobs, Results, Runner) {
    let (p, top) = common::project(&[("vres.shprim", VBODY), ("tb.shbench", VA_BENCH)]);
    let plan = Plan::read(p.doc(top).expect("the bench is open")).expect("the bench reads");
    let mut jobs = Jobs::new();
    jobs.push(None, None, NO_SAMPLE, &[])
        .expect("the table takes a job");
    jobs.push(None, None, NO_SAMPLE, &[])
        .expect("the table takes a job");
    let digest = schemify_run::design(
        &p,
        top,
        &jobs,
        &[],
        Dialect::Ngspice,
        &mut schemify_elab::no_funcs,
    )
    .expect("not blocked");
    let mut results = Results::new(&plan, digest);
    let mut c = common::cfg(dir, &digest, common::sh(FAKE), 2);
    c.cache = dir.join("cache");
    c.tools.openvaf = openvaf.to_string_lossy().into_owned();
    let mut runner = Runner::new(c);
    for j in 0..2 {
        let prepared = schemify_run::prepare(
            &p,
            top,
            &mut jobs,
            j,
            &[],
            Dialect::Ngspice,
            &mut schemify_elab::no_funcs,
            &mut results,
        )
        .expect("the job netlists");
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
    let dir = common::tmp("osdi-body");
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
    let src = fs::read_to_string(cache.join("vres.va")).unwrap();
    assert_eq!(
        src, "module vres(p, n);\nendmodule\n",
        "the body, written for the compiler"
    );
    let count = fs::read_to_string(cache.join("vres.va.count")).unwrap();
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
    let count = fs::read_to_string(cache.join("vres.va.count")).unwrap();
    assert_eq!(count.lines().count(), 1, "cache hit");
    // A compiler that fails: every job needing its output fails with its last line.
    let dir = common::tmp("osdi-body-fail");
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
    let (p, top) = common::project(&[("inv.shprim", inv), ("tb.shbench", bench)]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let mut jobs = Jobs::new();
    jobs.push(None, None, NO_SAMPLE, &[]).unwrap();
    let dir = common::tmp("icarus-body");
    let (jobs, results, runner) = common::run(&p, top, &plan, jobs, |d| {
        let mut c = common::cfg(&dir, d, common::sh(FAKE), 1);
        c.cache = dir.join("cache");
        c
    });
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.state(0), State::Done, "{errs:?}");
    let netlist = fs::read_to_string(runner.job_dir(0).join("netlist.cir")).unwrap();
    assert!(
        netlist.contains("d_cosim simulation=\"inv.vvp\""),
        "{netlist}"
    );
    let vvp = fs::read_to_string(runner.job_dir(0).join("inv.vvp")).unwrap();
    assert!(vvp.contains(":ivl_version") && vvp.contains("inv"), "{vvp}");
    let src = fs::read_to_string(dir.join("cache/inv.v")).unwrap();
    assert!(src.starts_with("module inv(input a, output y);\n"), "{src}");
}

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
    let e = schemify_elab::elaborate(&p, top, &req, &mut schemify_elab::no_funcs);
    assert!(!e.violations.blocking(), "{:?}", e.violations.rows);
    // The source is the PDK cell's file, from the PDK cell's library and directory (not
    // `core/…` in the `tech` library: the derived row has no directory on disk).
    let fin = p.libs().find("fin").unwrap() as u8;
    assert_eq!(
        schemify_run::sources(&p, &e, Dialect::Ngspice),
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

/// The espice dialect (run spec §5): no compiler runs (OpenVAF is absent), each Verilog-A
/// source (`file` and `body`) lands in the job directory as `<stem>.va` for `.hdl`, the
/// simulator gets its `env`, and the design hash differs from the ngspice run's. A
/// missing source file fails the job naming it.
#[test]
fn espice_copies_verilog_a_sources_without_compiling() {
    let (p, top) = common::project(&[
        ("vres.shprim", VRES),
        ("vgain.shprim", VGAIN),
        ("tb.shbench", ES_BENCH),
    ]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let dir = common::tmp("espice");
    fs::write(dir.join("vres.va"), "module vres(p, n); endmodule\n").unwrap();
    let one = || {
        let mut j = Jobs::new();
        j.push(None, None, NO_SAMPLE, &[]).unwrap();
        j
    };
    let espice = |c: &mut RunnerConfig| {
        c.cache = dir.join("cache");
        c.tools.openvaf = "openvaf-not-installed".into();
        c.tools.set_sims(&SimCommand {
            env: vec![("ZIG".into(), "/fake/zig".into())],
            ..common::sh(FAKE_ESPICE)
        });
    };
    let (jobs, results, runner) =
        common::run_dialect(&p, top, &plan, one(), &[], Dialect::Espice, |d| {
            let mut c = common::cfg(&dir, d, common::sh(FAKE), 1);
            espice(&mut c);
            c
        });
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.states(), [State::Done], "{errs:?}");
    let job = runner.job_dir(0);
    assert_eq!(
        fs::read_to_string(job.join("vres.va")).unwrap(),
        "module vres(p, n); endmodule\n"
    );
    assert_eq!(
        fs::read_to_string(job.join("vgain.va")).unwrap(),
        "module vgain(p, n);\nendmodule\n"
    );
    assert!(!job.join("vres.osdi").exists() && !dir.join("cache/vres.osdi").exists());
    let netlist = fs::read_to_string(job.join("netlist.cir")).unwrap();
    assert!(
        netlist.contains("\n.model vgain_m vgain\n.model vres vres\n"),
        "{netlist}"
    );
    let ng = schemify_run::design(
        &p,
        top,
        &one(),
        &[],
        Dialect::Ngspice,
        &mut schemify_elab::no_funcs,
    )
    .unwrap();
    assert_ne!(*results.design(), ng, "another simulator, another run");
    // Negative space: the `file` is gone, so the job fails naming it.
    fs::remove_file(dir.join("vres.va")).unwrap();
    let (jobs, results, _) =
        common::run_dialect(&p, top, &plan, one(), &[], Dialect::Espice, |d| {
            let mut c = common::cfg(&dir, d, common::sh(FAKE), 1);
            espice(&mut c);
            c
        });
    assert_eq!(jobs.states(), [State::Failed]);
    let msg = results.errors().next().unwrap().2.to_owned();
    assert!(msg.contains("vres.va"), "{msg}");
}

/// Run spec §5: an espice job gets a copied source's `` `include``s beside it, each
/// resolved against the file that names it (`sub/a.inc` includes `b.inc` = `sub/b.inc`),
/// standard headers (`disciplines.vams`) left to the compiler, commented ones skipped and
/// cycles ended. Negative space: a missing include, or one above the source's directory,
/// fails the job naming it.
#[test]
fn espice_copies_verilog_a_includes_beside_the_source() {
    let (p, top) = common::project(&[("vtop.shprim", VTOP), ("tb.shbench", INC_BENCH)]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let dir = common::tmp("espice-inc");
    let va = dir.join("va");
    fs::create_dir_all(va.join("sub")).unwrap();
    let write = |f: &str, t: &str| fs::write(va.join(f), t).unwrap();
    write(
        "vtop.va",
        "`include \"disciplines.vams\"\n`include \"macros.include\"\n  `include \"sub/a.inc\" // two\n\
         // `include \"nope.inc\"\nmodule vtop(p, n); endmodule\n",
    );
    write("macros.include", "`define K 2\n");
    write("sub/a.inc", "`include \"b.inc\"\n");
    write(
        "sub/b.inc",
        "`include \"../macros.include\"\n`include \"a.inc\"\n",
    );
    let go = |name: &str| {
        let mut j = Jobs::new();
        j.push(None, None, NO_SAMPLE, &[]).unwrap();
        common::run_dialect(&p, top, &plan, j, &[], Dialect::Espice, |d| {
            let mut c = common::cfg(&dir, d, common::sh(FAKE_INC), 1);
            c.dir = dir.join(name);
            c.cache = dir.join("cache");
            c
        })
    };
    let (jobs, results, runner) = go("ok");
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.states(), [State::Done], "{errs:?}");
    let job = runner.job_dir(0);
    assert_eq!(
        fs::read_to_string(job.join("sub/b.inc")).unwrap(),
        "`include \"../macros.include\"\n`include \"a.inc\"\n"
    );
    assert!(!job.join("disciplines.vams").exists() && !job.join("nope.inc").exists());

    fs::remove_file(va.join("sub/b.inc")).unwrap();
    let (jobs, results, _) = go("missing");
    assert_eq!(jobs.states(), [State::Failed]);
    let msg = results.errors().next().unwrap().2.to_owned();
    assert!(msg.contains("a.inc") && msg.contains("\"b.inc\""), "{msg}");

    write("sub/b.inc", "`include \"../../outside.inc\"\n");
    fs::write(dir.join("outside.inc"), "").unwrap();
    let (jobs, results, _) = go("outside");
    assert_eq!(jobs.states(), [State::Failed]);
    let msg = results.errors().next().unwrap().2.to_owned();
    assert!(msg.contains("outside the source's directory"), "{msg}");
}
