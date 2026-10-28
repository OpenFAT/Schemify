//! Real simulators and their dialects: ngspice on the RC bench and MOS OP quantities, Xyce's
//! OP naming, and an eSpice deck.

use std::path::Path;

use schemify_editor::checks::SOURCE_OP;
use schemify_run::State;

use super::{FAKE, activate, open, open_with, run_to_idle, sh};
use crate::common::{self, U};

/// With ngspice on `PATH`: the RC bench simulates for real in the editor; tau = RC within
/// 0.5%, and the operating point puts 1 V on both nets (the capacitor is open at DC).
#[test]
fn real_ngspice_simulates_the_rc_bench() {
    let found = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| Path::new(&d).join("ngspice").exists()));
    if !found {
        eprintln!("ngspice not on PATH: skipped");
        return;
    }
    let mut s = open("rc", "tb.shbench", "ngspice", None);
    s.key("s");
    run_to_idle(&mut s);
    let (plan, jobs, res) = s.ed.sim().tables().unwrap();
    assert_eq!(
        jobs.state(0),
        State::Done,
        "{:?}",
        res.errors().collect::<Vec<_>>()
    );
    let tau = res.value(0, plan.output("tau").unwrap()).unwrap();
    assert!((tau - 3.3e-3).abs() < 3.3e-3 * 5e-3, "{tau}");
    let op: Vec<&str> =
        s.ed.annotations()
            .iter()
            .map(|(_, m)| m)
            .filter(|m| m.target == "out")
            .map(|m| m.text.as_str())
            .collect();
    assert_eq!(op, ["1V"]);
    if let Ok(path) = std::env::var("SCHEMIFY_SCREENSHOT") {
        activate(&mut s, "results.waves");
        s.move_screen(640.0, 60.0);
        s.key("f");
        s.click((0, 45 * U));
        let px = s.render();
        common::save_png(&px, Path::new(&path));
    }
}

/// Xyce writes `.op` as a one-point DC plot with bare upper-case node names: the OP marks
/// read it like ngspice's operating point.
#[test]
fn op_annotation_reads_xyce_naming() {
    let xyce = r#"printf 'Title: fake\nPlotname: DC transfer characteristic\nFlags: real\nNo. Variables: 4\nNo. Points: 1\nVariables:\n 0 sweep voltage\n 1 IN voltage\n 2 OUT voltage\n 3 V1#branch current\nValues:\n0 0\n 1\n 0.5\n -0.001\n' > "$2"
"#;
    let mut s = open("rc", "tb.shbench", "xyce-op", Some(sh(xyce)));
    s.key("s");
    run_to_idle(&mut s);
    let op: Vec<(&str, &str)> =
        s.ed.annotations()
            .iter()
            .filter(|(src, _)| *src == SOURCE_OP)
            .map(|(_, m)| (m.target.as_str(), m.text.as_str()))
            .collect();
    assert!(op.contains(&("out", "500mV")), "{op:?}");
    assert!(op.contains(&("in", "1V")), "{op:?}");
    assert!(
        !op.iter().any(|m| m.0 == "sweep"),
        "the sweep scale is no net"
    );
}

/// With ngspice on `PATH`: an NMOS (level 1, kp 100µ, vto 0.5, W/L 1) at Vgs 1 V, Vds 1.8 V
/// is in saturation, so gm = kp·(Vgs − vto) = 50 µS and id = kp/2·(Vgs − vto)² = 12.5 µA.
/// OP annotation saved `@m1[gm]` (a `.save` the editor added), and `.options
/// savecurrents` wrote the drain current under the name cross-probe asks for.
#[test]
fn real_ngspice_saves_mos_op_quantities_and_terminal_currents() {
    let found = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| Path::new(&d).join("ngspice").exists()));
    if !found {
        eprintln!("ngspice not on PATH: skipped");
        return;
    }
    let nmos = "schemify 1 shprim\n  prefix M\n\npin d inout 0 -30\npin g in -30 0\n\
                pin s inout 0 30\npin b inout 30 0\n\nimpl spice spice\n  \
                format spice \"M{name} {d} {g} {s} {b} nm W=1u L=1u\"\n";
    let bench = "schemify 1 shbench\n\ninst 0aaaaa ./nmos\n  at 0 0 R0\n  name 1\n\n\
                 label 0bbbba 0 -30 R0 d\nlabel 0bbbbb -30 0 R0 g\nlabel 0bbbbc 0 30 R0 0\n\
                 label 0bbbbd 30 0 R0 0\n\ncode op spice\n  | Vd d 0 1.8\n  | Vg g 0 1\n  \
                 | .model nm nmos level=1 vto=0.5 kp=100u\n  | .op\n";
    let mut s = open_with(
        "rc",
        &[("nmos.shprim", nmos), ("mos.shbench", bench)],
        "mos.shbench",
        "mos-op",
        None,
    );
    s.key("s");
    run_to_idle(&mut s);
    let (_, jobs, res) = s.ed.sim().tables().unwrap();
    let errs: Vec<_> = res.errors().collect();
    assert_eq!(jobs.state(0), State::Done, "{errs:?}");
    let marks: Vec<&str> =
        s.ed.annotations()
            .iter()
            .map(|(_, m)| m)
            .filter(|m| m.target == "/1")
            .map(|m| m.text.as_str())
            .collect();
    let gm = format!("gm={}", schemify_wave::format_si(50e-6, 4));
    let id = format!("id={}", schemify_wave::format_si(12.5e-6, 4));
    assert!(marks.contains(&gm.as_str()), "{marks:?}");
    assert!(marks.contains(&id.as_str()), "{marks:?}");
    // The vector cross-probe asks for on the drain pin is the one ngspice wrote.
    let probe = schemify_editor::waves::current_probe("M1", "d");
    assert_eq!(probe, "i(\"@m1[id]\")");
    // ngspice writes saved device vectors wrapped by kind; `i("@m1[id]")` binds `i(@m1[id])`.
    let raw = std::fs::read(s.ed.sim().job_raw(0).unwrap()).unwrap();
    let plots = schemify_wave::parse_raw(&raw).unwrap();
    assert!(
        plots
            .iter()
            .any(|p| p.vars().iter().any(|v| v.name == "i(@m1[id])")),
        "savecurrents wrote i(@m1[id])"
    );
}

/// Simulation → Netlist dialect → ESPice: the status shows it, and Simulate netlists the
/// bench for espice (the fake simulator refuses any other deck) and records its results.
/// Back on ngspice the next run is ngspice again (negative space).
#[test]
fn the_espice_dialect_simulates_an_espice_deck() {
    let only = |word: &str| {
        format!(
            "head -1 \"$1\" | grep -q '(schemify {word})' || {{ echo 'not {word}' >&2; exit 3; }}\n{FAKE}"
        )
    };
    let mut s = open("rc", "tb.shbench", "espice", Some(sh(&only("espice"))));
    assert_eq!(s.label("status.mode"), "ngspice");
    activate(&mut s, "sim.dialect.espice");
    assert_eq!(s.label("status.mode"), "espice");
    s.key("s");
    run_to_idle(&mut s);
    let (_, jobs, res) = s.ed.sim().tables().unwrap();
    assert_eq!(
        jobs.state(0),
        State::Done,
        "{:?}",
        res.errors().collect::<Vec<_>>()
    );
    activate(&mut s, "sim.dialect.ngspice");
    s.key("s");
    run_to_idle(&mut s);
    let (_, jobs, res) = s.ed.sim().tables().unwrap();
    assert_eq!(jobs.state(0), State::Failed, "an ngspice deck this time");
    assert!(res.errors().any(|e| e.2.contains("not espice")));
}
