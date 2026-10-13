//! `op(inst, param)` outputs (run spec §4, expr spec §5): the job netlist saves exactly the
//! device vectors the outputs name (top-level, inside a schematic subcircuit, inside a PDK
//! device's own subcircuit), the values come from the operating point, and every way it
//! cannot work is a located error on that output only: an unknown instance, a subcircuit
//! instance, a parameter the device lacks, no operating point, ESPice. A fake simulator
//! (`sh`) checks the save line; the real ngspice (when on PATH) checks a level-1 MOSFET
//! against the square law.

mod common;

use common::{have_ngspice, project, run_dialect, tmp};
use schemify_elab::Dialect;
use schemify_run::{Jobs, NO_SAMPLE, Plan, Results, RunnerConfig, SimCommand, State};

/// A MOSFET leaf netlisted as an `M` element.
const NMOS: &str = "schemify 1 shprim\n\npin d inout 0 0\npin g in 0 20\npin s inout 0 40\n\
pin b inout 0 60\n\nimpl spice spice\n  format spice \"M{name} {d} {g} {s} {b} nch w=1u l=1u\"\n";

/// A PDK-style device: an `X` call of the kit's model subcircuit.
const XFET: &str = "schemify 1 shprim\n\npin d inout 0 0\npin g in 0 20\npin s inout 0 40\n\
pin b inout 0 60\n\nimpl spice spice\n  format spice \"X{name} {d} {g} {s} {b} sky130_fd_pr__nfet_01v8\"\n";

/// A schematic subcircuit holding `M2`.
const AMP: &str = "schemify 1 shcomp\n\npin d inout -40 0\npin g in -40 20\n\nimpl sch schematic\n\n\
inst 0m0001 ./nmos\n  at 0 0 R0\n  name M2\n\n\
label 0n0001 0 0 R0 d\nlabel 0n0002 0 20 R0 g\nlabel 0n0003 0 40 R0 0\nlabel 0n0004 0 60 R0 0\n\n\
port 0p0001 -100 0 R0 d\nport 0p0002 -100 20 R0 g\n";

const BENCH: &str = r#"schemify 1 shbench

inst 0aaaaa ./nmos
  at 0 0 R0
  name M1
inst 0aaaab ./xfet
  at 200 0 R0
  name M3
inst 0aaaac ./amp
  at 400 0 R0
  name X1

label 0b0001 0 0 R0 out
label 0b0002 0 20 R0 in
label 0b0003 0 40 R0 0
label 0b0004 0 60 R0 0
label 0b0005 200 0 R0 out
label 0b0006 200 20 R0 in
label 0b0007 200 40 R0 0
label 0b0008 200 60 R0 0
label 0b0009 360 0 R0 out
label 0b000a 360 20 R0 in

output gm1 "op(\"M1\", \"gm\")"
output id1 "op(M1, id)"
output gmid "op(\"M1\", \"gm\") / op(\"M1\", \"id\")"
output id2 "op(\"/X1/M2\", \"id\")"
output gm3 "op(\"M3/msky130_fd_pr__nfet_01v8\", \"gm\")"
output mixed "op(\"M1\", \"id\") * 1000 + v(out)"
output nodev "op(\"M9\", \"gm\")"
output nopar "op(\"M1\", \"foo\")"
output subckt "op(\"X1\", \"gm\")"

code op spice
  | V1 in 0 0.9
  | VD out 0 1.8
  | .model nch nmos level=1 vto=0.5 kp=200u
  | .subckt sky130_fd_pr__nfet_01v8 d g s b
  | msky130_fd_pr__nfet_01v8 d g s b nch w=1u l=1u
  | .ends
  | .op
"#;

/// The save line `op(…)` adds: every resolvable op signal, sorted by `<inst>[<param>]` as
/// written (`/X1/M2` first).
const SAVES: &str =
    ".save @m.x1.mm2[id] @mm1[foo] @mm1[gm] @mm1[id] @m.xm3.msky130_fd_pr__nfet_01v8[gm]";

/// Fails unless the netlist saves exactly those vectors (after `.save all`, before `.end`);
/// writes an operating point holding them (all but `foo`) and v(out).
const FAKE: &str = r#"grep -qxF '.save all' "$1" && grep -qxF "$SAVES" "$1" || { echo 'saves' >&2; exit 3; }
tail -n 1 "$1" | grep -qx '.end' || exit 4
printf 'Title: fake\nPlotname: Operating Point\nFlags: real\nNo. Variables: 5\nNo. Points: 1\nVariables:\n 0 v(out) voltage\n 1 @mm1[gm] notype\n 2 @mm1[id] current\n 3 @m.x1.mm2[id] current\n 4 @m.xm3.msky130_fd_pr__nfet_01v8[gm] notype\nValues:\n0 1.8\n 8e-05\n 1.6e-05\n 2.5e-05\n 3e-04\n' > "$2"
"#;

/// Like [`FAKE`] but a transient run: no operating point.
const FAKE_TRAN: &str = r#"printf 'Title: fake\nPlotname: Transient Analysis\nFlags: real\nNo. Variables: 2\nNo. Points: 2\nVariables:\n 0 time time\n 1 v(out) voltage\nValues:\n0 0\n 0\n1 1\n 1\n' > "$2"
"#;

fn sh(script: &str) -> SimCommand {
    SimCommand {
        path: "sh".into(),
        args: ["-c", script, "fake", "{netlist}", "{raw}"]
            .map(String::from)
            .to_vec(),
        env: vec![("SAVES".into(), SAVES.into())],
    }
}

fn one() -> Jobs {
    let mut j = Jobs::new();
    j.push(None, None, NO_SAMPLE, &[]).unwrap();
    j
}

fn run(name: &str, dialect: Dialect, sim: SimCommand) -> (Plan, Jobs, Results, String) {
    let (p, top) = project(&[
        ("nmos.shprim", NMOS),
        ("xfet.shprim", XFET),
        ("amp.shcomp", AMP),
        ("tb.shbench", BENCH),
    ]);
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let dir = tmp(name);
    let (jobs, results, runner) = run_dialect(&p, top, &plan, one(), &[], dialect, |d| {
        let mut c = RunnerConfig::new(&dir, d);
        c.tools.sim = sim;
        c.workers = 1;
        c
    });
    let netlist = std::fs::read_to_string(runner.job_dir(0).join("netlist.cir")).unwrap();
    (plan, jobs, results, netlist)
}

fn value(plan: &Plan, r: &Results, o: &str) -> Option<f64> {
    r.value(0, plan.output(o).unwrap())
}

fn error(plan: &Plan, r: &Results, o: &str) -> String {
    let k = plan.output(o).unwrap();
    let errs: Vec<&str> = r.errors().filter(|e| e.1 == k).map(|e| e.2).collect();
    assert_eq!(errs.len(), 1, "{o}: {errs:?}");
    errs[0].to_owned()
}

#[test]
fn op_outputs_read_the_operating_point_through_saved_device_vectors() {
    let (plan, jobs, r, netlist) = run("op-fake", Dialect::Ngspice, sh(FAKE));
    assert_eq!(
        jobs.state(0),
        State::Done,
        "{:?}\n{netlist}",
        r.errors().collect::<Vec<_>>()
    );
    assert!(
        netlist.ends_with(&format!(".save all\n{SAVES}\n.end\n")),
        "{netlist}"
    );
    assert_eq!(value(&plan, &r, "gm1"), Some(8e-5));
    assert_eq!(value(&plan, &r, "id1"), Some(1.6e-5));
    assert_eq!(value(&plan, &r, "gmid"), Some(8e-5 / 1.6e-5));
    assert_eq!(value(&plan, &r, "id2"), Some(2.5e-5));
    assert_eq!(value(&plan, &r, "gm3"), Some(3e-4));
    assert_eq!(value(&plan, &r, "mixed"), Some(1.6e-5 * 1000.0 + 1.8));
    // Located errors, one per output; the other outputs are unaffected.
    let e = error(&plan, &r, "nodev");
    assert!(
        e.starts_with("X004 at 0..14:") && e.contains("no instance `M9`"),
        "{e}"
    );
    let e = error(&plan, &r, "nopar");
    assert!(
        e.starts_with("X004 at 0..15:") && e.contains("@mm1[foo]"),
        "{e}"
    );
    let e = error(&plan, &r, "subckt");
    assert!(
        e.contains("`X1` is a subcircuit instance, not a device"),
        "{e}"
    );
    assert_eq!(r.errors().count(), 3);
}

#[test]
fn op_without_an_operating_point_or_on_espice_is_an_error_not_zero() {
    let (plan, jobs, r, netlist) = run("op-tran", Dialect::Ngspice, sh(FAKE_TRAN));
    assert_eq!(jobs.state(0), State::Done);
    assert!(netlist.contains(SAVES), "{netlist}");
    let e = error(&plan, &r, "gm1");
    assert!(
        e.starts_with("X010 at 0..14:") && e.contains("add `.op`"),
        "{e}"
    );
    assert_eq!(value(&plan, &r, "gm1"), None);
    // ESPice has no device operating-point vectors: nothing is saved, every op() output
    // says why, and an output without op() still has its value.
    let (plan, jobs, r, netlist) = run("op-espice", Dialect::Espice, sh(FAKE_TRAN));
    assert_eq!(jobs.state(0), State::Done);
    assert!(!netlist.contains(".save"), "{netlist}");
    for o in [
        "gm1", "id1", "gmid", "id2", "gm3", "mixed", "nodev", "nopar", "subckt",
    ] {
        let e = error(&plan, &r, o);
        assert!(
            e.starts_with("X006") && e.contains("which espice does not provide"),
            "{o}: {e}"
        );
        assert_eq!(value(&plan, &r, o), None);
    }
}

/// The real ngspice on a level-1 NMOS in saturation (vgs = 0.9, vt = 0.5, kp = 200µ,
/// W/L = 1): id = kp/2·(vgs − vt)² = 16 µA, gm = kp·(vgs − vt) = 80 µS.
#[test]
fn ngspice_op_values_match_the_square_law() {
    if !have_ngspice() {
        return;
    }
    let ngspice = SimCommand {
        env: Vec::new(),
        ..SimCommand::ngspice()
    };
    let (plan, jobs, r, netlist) = run("op-ngspice", Dialect::Ngspice, ngspice);
    let errs: Vec<_> = r.errors().collect();
    assert_eq!(jobs.state(0), State::Done, "{errs:?}\n{netlist}");
    let gm = value(&plan, &r, "gm1").unwrap_or_else(|| panic!("{errs:?}"));
    let id = value(&plan, &r, "id1").unwrap();
    eprintln!("ngspice: gm = {gm:e}, id = {id:e}, gm/id = {}", gm / id);
    assert!((gm / 80e-6 - 1.0).abs() < 1e-6, "gm {gm}");
    assert!((id / 16e-6 - 1.0).abs() < 1e-6, "id {id}");
    assert!((value(&plan, &r, "gmid").unwrap() - 5.0).abs() < 1e-5);
    // Inside a schematic subcircuit and inside a PDK device's own model subcircuit.
    let id2 = value(&plan, &r, "id2").unwrap();
    assert!((id2 / 16e-6 - 1.0).abs() < 1e-6, "id2 {id2}");
    let gm3 = value(&plan, &r, "gm3").unwrap();
    assert!((gm3 / 80e-6 - 1.0).abs() < 1e-6, "gm3 {gm3}");
}
