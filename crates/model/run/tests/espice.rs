//! End to end with the real ESPice (`espice` from PATH; it compiles Verilog-A itself with the
//! Zig in `$ZIG`, else on PATH), skipped when absent unless `SCHEMIFY_REQUIRE_ESPICE` is set
//! (the CI shell has no ESPice yet, so `SCHEMIFY_REQUIRE_SIMULATORS` does not cover it): a
//! bench with an inline Verilog-A body and a Verilog-A `file`, netlisted for espice (`.hdl`),
//! run through the run spine, its raw file read by our reader.

mod common;

use common::{project, run_dialect, tmp};
use schemify_elab::Dialect;
use schemify_run::{Jobs, NO_SAMPLE, Plan, RunnerConfig, SimCommand, State};

/// Whether `espice` runs from PATH; skips (or fails under `SCHEMIFY_REQUIRE_ESPICE`).
fn have_espice() -> bool {
    let found = std::process::Command::new("espice")
        .arg("--version")
        .output()
        .is_ok();
    if !found {
        assert!(
            std::env::var_os("SCHEMIFY_REQUIRE_ESPICE").is_none(),
            "espice must be on PATH (SCHEMIFY_REQUIRE_ESPICE is set)"
        );
        eprintln!("espice not on PATH: skipping");
    }
    found
}

/// `vgain`: out = k·in (k = 3 by default), its source the impl `body`.
const VGAIN: &str = "schemify 1 shprim\n\npin in inout 0 0\npin out inout 0 20\n\nparam k 3\n\n\
impl va veriloga\n  model vgain_m\n\nbody va\n  | `include \"disciplines.vams\"\n  \
| module vgain(in, out);\n  |   inout in, out;\n  |   electrical in, out;\n  \
|   parameter real k = 3;\n  |   analog V(out) <+ k * V(in);\n  | endmodule\n";

/// `vhalf`: out = in/2, its source the `file` `va/vhalf.va`, which takes the factor from
/// its own include `inc/half.vams`.
const VHALF: &str = "schemify 1 shprim\n\npin in inout 0 0\npin out inout 0 20\n\n\
impl va veriloga\n  model vhalf_m\n  file va/vhalf.va\n";
const VHALF_VA: &str = "`include \"disciplines.vams\"\n`include \"inc/half.vams\"\n\
module vhalf(in, out);\n  inout in, out;\n  electrical in, out;\n  analog V(out) <+ `HALF * V(in);\n\
endmodule\n";

const BENCH: &str = "schemify 1 shbench\n\ninst 0aaaaa ./vgain\n  at 0 0 R0\n  name N1\n  k 4\n\
inst 0aaaab ./vhalf\n  at 100 0 R0\n  name N2\n\n\
label 0bbbba 0 0 R0 in\nlabel 0bbbbb 0 20 R0 y\nlabel 0bbbbc 100 0 R0 in\nlabel 0bbbbd 100 20 R0 z\n\n\
output vy \"v(y)\"\noutput vz \"v(z)\"\n\ncode op spice\n  | V1 in 0 0.5\n  | .op\n";

/// The vgain bench of the user's flow: v(y) = 4 × 0.5 = 2 (inline body, the instance's
/// `k=4` on its `N` line), v(z) = 0.25 (`file` and its include, copied beside it), through
/// `.hdl` and ESPice's own Verilog-A compiler.
#[test]
fn espice_simulates_verilog_a_bodies_and_files() {
    if !have_espice() {
        return;
    }
    let (p, top) = project(&[
        ("vgain.shprim", VGAIN),
        ("vhalf.shprim", VHALF),
        ("tb.shbench", BENCH),
    ]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let dir = tmp("espice-real");
    std::fs::create_dir_all(dir.join("va/inc")).unwrap();
    std::fs::write(dir.join("va/vhalf.va"), VHALF_VA).unwrap();
    std::fs::write(dir.join("va/inc/half.vams"), "`define HALF 0.5\n").unwrap();
    let mut jobs = Jobs::new();
    jobs.push(None, None, NO_SAMPLE, &[]).unwrap();
    let (jobs, results, runner) = run_dialect(&p, top, &plan, jobs, &[], Dialect::Espice, |d| {
        let mut c = RunnerConfig::new(&dir, d);
        c.cache = dir.join("cache");
        c.tools.sim = SimCommand::espice();
        c.tools.openvaf = "openvaf-not-used".into();
        c
    });
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.state(0), State::Done, "{errs:?}");
    let netlist = std::fs::read_to_string(runner.job_dir(0).join("netlist.cir")).unwrap();
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

/// A BSIM-CMG fin cell as pdk-manager generates it for ASAP7 (pdk spec §2.2), its source
/// the kit's `bsimcmg.va` (copied into the test project's `va/`).
const FIN: &str = "schemify 1 shprim\n\npin d inout 0 -30\npin g in -20 0\npin s inout 0 30\npin b in 20 0\n\n\
param model nmos_rvt\n  kind string\nparam nfin 1\nparam nf 1\nparam m 1\n\nimpl osdi veriloga\n  \
format spice \"N{name} {d} {g} {s} {b} {model} nfin={nfin} nf={nf} m={m}\"\n  \
format espice \"N{name} {d} {g} {s} {b} 0 {model} nfin={nfin} nf={nf} mfactor={m}\"\n  \
file va/bsimcmg.va\n";

/// Two ASAP7 inverters, input low (`y0`) and high (`y1`), through the real ESPice: the
/// BSIM-CMG source and its seven `` `include``s copied into the job, the thermal port
/// grounded, `mfactor=` (run spec §5, netlist spec §7.2a). Needs `SCHEMIFY_ASAP7_PDK` (a
/// pdk-manager ASAP7 install: `va/code/bsimcmg/vacode/`, `models/corners.lib`), `espice` on
/// PATH and `ZIG`; the first BSIM-CMG compile takes ≈ 4 min and 2.8 GB (ESPice caches it
/// in `ESPICE_CACHE`).
#[test]
#[ignore = "real ESPice + an ASAP7 install (SCHEMIFY_ASAP7_PDK); minutes on a cold cache"]
fn espice_runs_an_asap7_inverter() {
    let Some(pdk) = std::env::var_os("SCHEMIFY_ASAP7_PDK").map(std::path::PathBuf::from) else {
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
    let (p, top) = project(&[("fin.shprim", FIN), ("tb.shbench", &bench)]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let dir = tmp("espice-asap7");
    std::fs::create_dir_all(dir.join("va")).unwrap();
    for f in std::fs::read_dir(pdk.join("va/code/bsimcmg/vacode")).unwrap() {
        let f = f.unwrap().path();
        if f.is_file() {
            std::fs::copy(&f, dir.join("va").join(f.file_name().unwrap())).unwrap();
        }
    }
    let mut jobs = Jobs::new();
    jobs.push(None, None, NO_SAMPLE, &[]).unwrap();
    let (jobs, results, runner) = run_dialect(&p, top, &plan, jobs, &[], Dialect::Espice, |d| {
        let mut c = RunnerConfig::new(&dir, d);
        c.cache = dir.join("cache");
        c.tools.sim = SimCommand::espice();
        c
    });
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.state(0), State::Done, "{errs:?}");
    let job = runner.job_dir(0);
    let netlist = std::fs::read_to_string(job.join("netlist.cir")).unwrap();
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
