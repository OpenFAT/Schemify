//! End to end with the real ESPice (`espice` from PATH; it compiles Verilog-A and Verilog
//! itself with the Zig in `$ZIG`, else on PATH), skipped when absent unless
//! `SCHEMIFY_REQUIRE_ESPICE` is set (the CI shell has no ESPice yet, so
//! `SCHEMIFY_REQUIRE_SIMULATORS` does not cover it): a bench with an inline Verilog-A body
//! and a Verilog-A `file`, netlisted for espice (`.hdl`), and the netlist crate's Verilog
//! benches (`.verilog`, netlist spec §7.2a), each run through the run spine, its raw file
//! read by our reader.

mod common;

use schemify_elab::Dialect;
use schemify_run::{Jobs, NO_SAMPLE, Pass, Plan, Results, RunnerConfig, SimCommand, State};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// `vgain`: out = k·in (k = 3 by default), its source the impl `body`.
const VGAIN: &str = "schemify 1 shprim\n\npin in inout 0 0\npin out inout 0 20\n\nparam k 3\n\
\nimpl va veriloga\n  model vgain_m\n\nbody va\n  | `include \"disciplines.vams\"\n  \
| module vgain(in, out);\n  |   inout in, out;\n  |   electrical in, out;\n  \
|   parameter real k = 3;\n  |   analog V(out) <+ k * V(in);\n  | endmodule\n";

/// `vhalf`: out = in/2, its source the `file` `va/vhalf.va`, which takes the factor from
/// its own include `inc/half.vams`.
const VHALF: &str = "schemify 1 shprim\n\npin in inout 0 0\npin out inout 0 20\n\
\nimpl va veriloga\n  model vhalf_m\n  file va/vhalf.va\n";

const VHALF_VA: &str = "`include \"disciplines.vams\"\n`include \"inc/half.vams\"\n\
module vhalf(in, out);\n  inout in, out;\n  electrical in, out;\n  analog V(out) <+ `HALF * V(in);\n\
endmodule\n";

const BENCH: &str = "schemify 1 shbench\n\ninst 0aaaaa ./vgain\n  at 0 0 R0\n  name N1\n  k 4\n\
inst 0aaaab ./vhalf\n  at 100 0 R0\n  name N2\n\n\
label 0bbbba 0 0 R0 in\nlabel 0bbbbb 0 20 R0 y\nlabel 0bbbbc 100 0 R0 in\nlabel 0bbbbd 100 20 R0 z\n\n\
output vy \"v(y)\"\noutput vz \"v(z)\"\n\ncode op spice\n  | V1 in 0 0.5\n  | .op\n";

/// A BSIM-CMG fin cell as pdk-manager generates it for ASAP7 (pdk spec §2.2), its source
/// the kit's `bsimcmg.va` (copied into the test project's `va/`).
const FIN: &str = "schemify 1 shprim\n\npin d inout 0 -30\npin g in -20 0\npin s inout 0 30\npin b in 20 0\n\n\
param model nmos_rvt\n  kind string\nparam nfin 1\nparam nf 1\nparam m 1\n\nimpl osdi veriloga\n  \
format spice \"N{name} {d} {g} {s} {b} {model} nfin={nfin} nf={nf} m={m}\"\n  \
format espice \"N{name} {d} {g} {s} {b} 0 {model} nfin={nfin} nf={nf} mfactor={m}\"\n  \
file va/bsimcmg.va\n";

// ---- private ----

/// Whether `espice` runs from PATH; skips (or fails under `SCHEMIFY_REQUIRE_ESPICE`).
fn have_espice() -> bool {
    let found = Command::new("espice").arg("--version").output().is_ok();
    if !found {
        assert!(
            env::var_os("SCHEMIFY_REQUIRE_ESPICE").is_none(),
            "espice must be on PATH (SCHEMIFY_REQUIRE_ESPICE is set)"
        );
        eprintln!("espice not on PATH: skipping");
    }
    found
}

/// The vgain bench of the user's flow: v(y) = 4 × 0.5 = 2 (inline body, the instance's
/// `k=4` on its `N` line), v(z) = 0.25 (`file` and its include, copied beside it), through
/// `.hdl` and ESPice's own Verilog-A compiler.
#[test]
fn espice_simulates_verilog_a_bodies_and_files() {
    if !have_espice() {
        return;
    }
    let (p, top) = common::project(&[
        ("vgain.shprim", VGAIN),
        ("vhalf.shprim", VHALF),
        ("tb.shbench", BENCH),
    ]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let dir = common::tmp("espice-real");
    fs::create_dir_all(dir.join("va/inc")).unwrap();
    fs::write(dir.join("va/vhalf.va"), VHALF_VA).unwrap();
    fs::write(dir.join("va/inc/half.vams"), "`define HALF 0.5\n").unwrap();
    let mut jobs = Jobs::new();
    jobs.push(None, None, NO_SAMPLE, &[]).unwrap();
    let (jobs, results, runner) =
        common::run_dialect(&p, top, &plan, jobs, &[], Dialect::Espice, |d| {
            let mut c = RunnerConfig::new(&dir, d);
            c.cache = dir.join("cache");
            c.tools.set_sims(&SimCommand::default_for(Dialect::Espice));
            c.tools.openvaf = "openvaf-not-used".into();
            c
        });
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.state(0), State::Done, "{errs:?}");
    let netlist = fs::read_to_string(runner.job_dir(0).join("netlist.cir")).unwrap();
    assert!(
        netlist.contains(".hdl \"vgain.va\"\n.hdl \"vhalf.va\"\n")
            && netlist.contains("\nN1 in y vgain_m k=4\n")
            && !netlist.contains("osdi"),
        "{netlist}"
    );
    let vy = results.value(0, plan.output("vy").unwrap()).unwrap();
    let vz = results.value(0, plan.output("vz").unwrap()).unwrap();
    eprintln!("espice: v(y) = {vy}, v(z) = {vz}");
    assert!((vy - 2.0).abs() < 1e-9, "v(y) {vy}");
    assert!((vz - 0.25).abs() < 1e-9, "v(z) {vz}");
}

/// Two ASAP7 inverters, input low (`y0`) and high (`y1`), through the real ESPice: the
/// BSIM-CMG source and its seven `` `include``s copied into the job, the thermal port
/// grounded, `mfactor=` (run spec §5, netlist spec §7.2a). Needs `SCHEMIFY_ASAP7_PDK` (a
/// pdk-manager ASAP7 install: `va/code/bsimcmg/vacode/`, `models/corners.lib`), `espice` on
/// PATH and `ZIG`; the first BSIM-CMG compile takes ≈ 4 min and 2.8 GB (ESPice caches it
/// in `ESPICE_CACHE`).
#[test]
#[ignore = "real ESPice + an ASAP7 install (SCHEMIFY_ASAP7_PDK); minutes on a cold cache"]
fn espice_runs_an_asap7_inverter() {
    let Some(pdk) = env::var_os("SCHEMIFY_ASAP7_PDK").map(PathBuf::from) else {
        panic!("set SCHEMIFY_ASAP7_PDK to a pdk-manager ASAP7 install");
    };
    assert!(have_espice(), "espice must be on PATH");
    let corners = pdk.join("models/corners.lib");
    let inst = |id: &str, x: i32, model: &str, nm: &str, mult: u32| {
        format!(
            "inst 0aaa{id} ./fin\n  at {x} 0 R0\n  name {nm}\n  model {model}\n  nfin 4\n  m {mult}\n"
        )
    };
    let label = |id: u32, x: i32, y: i32, net: &str| format!("label 0bb{id:03} {x} {y} R0 {net}\n");
    let mut bench = String::from("schemify 1 shbench\n\n");
    for (k, (model, d, g, s)) in [
        ("pmos_rvt", "y0", "lo", "vdd"),
        ("nmos_rvt", "y0", "lo", "0"),
        ("pmos_rvt", "y1", "vdd", "vdd"),
        ("nmos_rvt", "y1", "vdd", "0"),
    ]
    .into_iter()
    .enumerate()
    {
        let x = 100 * k as i32;
        bench += &inst(
            &format!("{k:02}"),
            x,
            model,
            &format!("M{k}"),
            1 + k as u32 % 2,
        );
        bench += &label(4 * k as u32, x, -30, d);
        bench += &label(4 * k as u32 + 1, x - 20, 0, g);
        bench += &label(4 * k as u32 + 2, x, 30, s);
        bench += &label(4 * k as u32 + 3, x + 20, 0, s);
    }
    bench += &format!(
        "\noutput vy0 \"v(y0)\"\noutput vy1 \"v(y1)\"\n\ncode op spice\n  | .lib {} tt\n  \
         | VDD vdd 0 0.7\n  | VL lo 0 0\n  | .op\n",
        corners.display()
    );
    let (p, top) = common::project(&[("fin.shprim", FIN), ("tb.shbench", &bench)]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let dir = common::tmp("espice-asap7");
    fs::create_dir_all(dir.join("va")).unwrap();
    for f in fs::read_dir(pdk.join("va/code/bsimcmg/vacode")).unwrap() {
        let f = f.unwrap().path();
        if f.is_file() {
            fs::copy(&f, dir.join("va").join(f.file_name().unwrap())).unwrap();
        }
    }
    let mut jobs = Jobs::new();
    jobs.push(None, None, NO_SAMPLE, &[]).unwrap();
    let (jobs, results, runner) =
        common::run_dialect(&p, top, &plan, jobs, &[], Dialect::Espice, |d| {
            let mut c = RunnerConfig::new(&dir, d);
            c.cache = dir.join("cache");
            c.tools.set_sims(&SimCommand::default_for(Dialect::Espice));
            c
        });
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.state(0), State::Done, "{errs:?}");
    let job = runner.job_dir(0);
    let netlist = fs::read_to_string(job.join("netlist.cir")).unwrap();
    assert!(
        netlist.contains("\nNM1 y0 lo 0 0 0 nmos_rvt nfin=4 nf=1 mfactor=2\n")
            && netlist.contains(".hdl \"bsimcmg.va\"\n"),
        "{netlist}"
    );
    assert!(job.join("bsimcmg_body.include").is_file());
    let vy0 = results.value(0, plan.output("vy0").unwrap()).unwrap();
    let vy1 = results.value(0, plan.output("vy1").unwrap()).unwrap();
    eprintln!("espice asap7: v(y0) = {vy0}, v(y1) = {vy1}");
    assert!(vy0 > 0.69 && vy1 < 0.01, "v(y0) {vy0}, v(y1) {vy1}");
}

/// The netlist crate's fixture directory: its cells, benches, sources (`hdl/`, `va/`) and
/// espice goldens.
fn fixture_dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("SCHEMIFY_ROOT"),
        "/crates/model/netlist/tests/fixtures"
    ))
}

/// Runs fixture `bench` (with every fixture cell, as the netlist tests load them) once
/// through the real ESPice, its sources read from the fixture directory; asserts the job is
/// done and returns its netlist, for [`assert_golden`].
fn run_fixture(bench: &str) -> (Plan, Results, String) {
    let mut files: Vec<(String, String)> = fs::read_dir(fixture_dir())
        .expect("the netlist fixtures exist")
        .flatten()
        .map(|f| f.path())
        .filter(|p| {
            let cell = p
                .extension()
                .is_some_and(|x| x == "shprim" || x == "shcomp");
            cell || p.file_name().is_some_and(|n| n == bench)
        })
        .map(|p| {
            let name = p.file_name().expect("a directory entry has a name");
            let text = fs::read_to_string(&p).expect("a fixture is UTF-8 text");
            (name.to_string_lossy().into_owned(), text)
        })
        .collect();
    files.sort();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    let (p, top) = common::project(&refs);
    let plan = Plan::read(p.doc(top).expect("the bench is open")).expect("the bench plan reads");
    let dir = common::tmp(&format!("espice-{bench}"));
    let mut jobs = Jobs::new();
    jobs.push(None, None, NO_SAMPLE, &[]).expect("one job fits");
    let (jobs, results, runner) =
        common::run_dialect(&p, top, &plan, jobs, &[], Dialect::Espice, |d| {
            let mut c = RunnerConfig::new(&dir, d);
            c.cache = dir.join("cache");
            c.lib_dirs = vec![fixture_dir()];
            c.tools.set_sims(&SimCommand::default_for(Dialect::Espice));
            c.tools.openvaf = "openvaf-not-used".into();
            c.tools.iverilog = "iverilog-not-used".into();
            c
        });
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.state(0), State::Done, "{bench}: {errs:?}");
    let netlist = fs::read_to_string(runner.job_dir(0).join("netlist.cir"))
        .expect("a done job has its netlist");
    (plan, results, netlist)
}

/// Asserts that the deck ESPice ran for `bench` is its espice golden byte for byte, so the
/// golden (`netlist/tests/emit.rs`) is a deck ESPice accepted. Called after the analytic
/// checks, which are the independent oracle.
fn assert_golden(bench: &str, netlist: &str) {
    let golden = bench.replace(".shbench", ".espice");
    let want = fs::read_to_string(fixture_dir().join(&golden)).expect("the golden exists");
    assert_eq!(
        netlist, want,
        "{bench}: the job's deck is the golden {golden}"
    );
}

/// The issue #63 gate: an 8-bit SAR ADC whose SAR logic is Verilog (`.verilog`, an `N`
/// card with the A2D/D2A bridge parameters, the 8-bit `d` bus as 8 pins MSB first), its
/// CDAC Verilog-A (`.hdl`) and its comparator transistors (a `schematic`), in one ESPice
/// job, converts vin = 1.8 V × 183.5/256 to 183, the code an ideal 8-bit ADC gives
/// (analytic: vin sits half an LSB above code 183's level). Needs `espice` on PATH and
/// `ZIG`.
#[test]
fn espice_converts_with_verilog_sar_logic() {
    if !have_espice() {
        return;
    }
    let (plan, results, netlist) = run_fixture("sar_tb.shbench");
    let code = results.value(0, plan.output("code").unwrap()).unwrap();
    eprintln!("espice sar: code = {code}");
    assert_eq!(code, 183.0);
    assert_eq!(results.pass(0, 0), Pass::Pass);
    assert_golden("sar_tb.shbench", &netlist);
}

/// A 3-bit counter's outputs read back in the raw file count the clock's rising edges
/// (one every 20 ns from 10 ns): the bus pins connect in ESPice's order, MSB first (read
/// LSB first, 1 would be 4). The second counter's `vth 1.2` instance attribute overrides
/// the cell's 0.9 and sits above the 1 V clock, so it never counts.
#[test]
fn espice_counts_through_verilog_bus_pins() {
    if !have_espice() {
        return;
    }
    let (plan, results, netlist) = run_fixture("cnt_tb.shbench");
    let value = |o: &str| results.value(0, plan.output(o).unwrap()).unwrap();
    for (o, count) in [
        ("u1_20n", 1.0),
        ("u1_60n", 3.0),
        ("u1_80n", 4.0),
        ("u1_140n", 7.0),
        ("u1_160n", 0.0),
        ("u2_80n", 0.0),
        ("u2_160n", 0.0),
    ] {
        assert_eq!(value(o), count, "{o}");
    }
    assert_golden("cnt_tb.shbench", &netlist);
}

/// A Verilog source VerA rejects fails the job (run spec §5): ESPice exits 1, its last
/// stderr line, `DigitalFailed`, is the job's error, and VerA's diagnostic (an error code
/// and the source's line) is in the job's `stderr.log`, the run log view's tail.
#[test]
fn espice_fails_a_job_whose_verilog_does_not_compile() {
    if !have_espice() {
        return;
    }
    let bad = "schemify 1 shprim\n\npin clk in -20 0\npin q out 20 0\n\nimpl v verilog\n\nbody v\n  \
               | module bad(clk, q);\n  |   input clk;\n  |   output q;\n  |   reg q;\n  \
               |   always @(posedge clk) q <= ;\n  | endmodule\n";
    let bench = "schemify 1 shbench\n\ninst 0aaaaa ./bad\n  at 0 0 R0\n  name U1\n\n\
                 label 0bbbba -20 0 R0 clk\nlabel 0bbbbb 20 0 R0 q\n\ncode tran spice\n  \
                 | VCLK clk 0 PULSE(0 1.8 10n 1n 1n 9n 20n)\n  | .tran 1n 50n\n";
    let (p, top) = common::project(&[("bad.shprim", bad), ("tb.shbench", bench)]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let dir = common::tmp("espice-bad-verilog");
    let mut jobs = Jobs::new();
    jobs.push(None, None, NO_SAMPLE, &[]).unwrap();
    let (jobs, results, runner) =
        common::run_dialect(&p, top, &plan, jobs, &[], Dialect::Espice, |d| {
            let mut c = RunnerConfig::new(&dir, d);
            c.cache = dir.join("cache");
            c.tools.set_sims(&SimCommand::default_for(Dialect::Espice));
            c
        });
    assert_eq!(jobs.state(0), State::Failed);
    let msg = results.errors().next().unwrap().message.to_owned();
    assert!(msg.contains("DigitalFailed"), "{msg}");
    let stderr = fs::read_to_string(runner.job_dir(0).join("stderr.log")).unwrap();
    assert!(
        stderr.contains("error[E") && stderr.contains("bad.v:5:"),
        "{stderr}"
    );
}
