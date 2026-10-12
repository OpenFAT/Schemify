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

/// `vgain`: out = 3·in, its source the impl `body`.
const VGAIN: &str = "schemify 1 shprim\n\npin in inout 0 0\npin out inout 0 20\n\n\
impl va veriloga\n  model vgain_m\n\nbody va\n  | `include \"disciplines.vams\"\n  \
| module vgain(in, out);\n  |   inout in, out;\n  |   electrical in, out;\n  \
|   parameter real k = 3;\n  |   analog V(out) <+ k * V(in);\n  | endmodule\n";

/// `vhalf`: out = in/2, its source the `file` `va/vhalf.va`.
const VHALF: &str = "schemify 1 shprim\n\npin in inout 0 0\npin out inout 0 20\n\n\
impl va veriloga\n  model vhalf_m\n  file va/vhalf.va\n";
const VHALF_VA: &str = "`include \"disciplines.vams\"\nmodule vhalf(in, out);\n  inout in, out;\n  \
electrical in, out;\n  analog V(out) <+ 0.5 * V(in);\nendmodule\n";

const BENCH: &str = "schemify 1 shbench\n\ninst 0aaaaa ./vgain\n  at 0 0 R0\n  name N1\n\
inst 0aaaab ./vhalf\n  at 100 0 R0\n  name N2\n\n\
label 0bbbba 0 0 R0 in\nlabel 0bbbbb 0 20 R0 y\nlabel 0bbbbc 100 0 R0 in\nlabel 0bbbbd 100 20 R0 z\n\n\
output vy \"v(y)\"\noutput vz \"v(z)\"\n\ncode op spice\n  | V1 in 0 0.5\n  | .op\n";

/// The vgain bench of the user's flow: v(y) = 3 × 0.5 = 1.5 (inline body), v(z) = 0.25
/// (`file`), through `.hdl` and ESPice's own Verilog-A compiler.
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
    std::fs::create_dir_all(dir.join("va")).unwrap();
    std::fs::write(dir.join("va/vhalf.va"), VHALF_VA).unwrap();
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
        netlist.contains(".hdl \"vgain.va\"\n.hdl \"vhalf.va\"\n") && !netlist.contains("osdi"),
        "{netlist}"
    );
    let vy = results.value(0, plan.output("vy").unwrap()).unwrap();
    let vz = results.value(0, plan.output("vz").unwrap()).unwrap();
    eprintln!("espice: v(y) = {vy}, v(z) = {vz}");
    assert!((vy - 1.5).abs() < 1e-9, "v(y) {vy}");
    assert!((vz - 0.25).abs() < 1e-9, "v(z) {vz}");
}
