//! The bundled `core` library under every simulation dialect (netlist spec §7.5, §10):
//! one bench with an instance of every core device, netlisted for ngspice and spectre,
//! against snapshots (`tests/fixtures/core_tb.<dialect>`, `UPDATE_SNAPSHOTS=1` rewrites
//! them); the cells Spectre cannot express (`bsource`, `switch`, `vswitch`) are refused
//! with `E013`, and a source spec Spectre has no form for fails its own line only.

mod common;

use schemify_doc::{DocHandle, Project, core_file, core_library};
use schemify_elab::{Dialect, Elab, Severity};
use std::fmt::Write;
use std::path::Path;

/// (cell, instance name, overrides, nets in the cell's pin order).
type Row = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static [&'static str],
);

/// Every core device except the label shorthands (`gnd`, `vdd`) and the three cells
/// Spectre cannot express.
const DEVICES: &[Row] = &[
    ("vsource", "VDD", &[("value", "1.8")], &["vdd", "0"]),
    (
        "vsource",
        "VIN",
        &[("value", "\"dc 0.9 ac 1 0\"")],
        &["in", "0"],
    ),
    (
        "vsource",
        "VCLK",
        &[("value", "\"pulse(0 1.8 1n 10p 10p 5n 10n)\"")],
        &["clk", "0"],
    ),
    (
        "vsource",
        "VPWL",
        &[("value", "\"pwl(0 0 1n 1.8 2n 0)\"")],
        &["ramp", "0"],
    ),
    (
        "isource",
        "IB",
        &[("value", "\"dc 10u sin(10u 1u 1meg)\"")],
        &["vdd", "bias"],
    ),
    (
        "isource",
        "IX",
        &[("value", "\"exp(0 1m 1n 2n 5n 1n)\"")],
        &["x", "0"],
    ),
    ("ammeter", "VM1", &[], &["out", "outm"]),
    ("vcvs", "E1", &[("value", "10")], &["e", "0", "in", "0"]),
    ("vccs", "G1", &[("value", "1m")], &["g", "0", "in", "0"]),
    (
        "ccvs",
        "H1",
        &[("vnam", "VM1"), ("value", "1k")],
        &["h", "0"],
    ),
    (
        "cccs",
        "F1",
        &[("vnam", "VM1"), ("value", "2")],
        &["f", "0"],
    ),
    ("diode", "D1", &[("model", "dmod")], &["in", "0"]),
    ("res", "R1", &[("value", "2.2meg")], &["outm", "0"]),
    ("cap", "C1", &[("value", "1p")], &["out", "0"]),
    ("ind", "L1", &[("value", "10n")], &["in", "l"]),
    ("parax_cap", "CP1", &[("value", "4f")], &["out", "0"]),
    (
        "nmos4",
        "M1",
        &[("model", "nch"), ("w", "2u"), ("l", "0.15u")],
        &["out", "in", "0", "0"],
    ),
    (
        "pmos4",
        "M2",
        &[("model", "pch"), ("w", "4u"), ("l", "0.15u")],
        &["out", "in", "vdd", "vdd"],
    ),
    ("npn", "Q1", &[("model", "qn")], &["c1", "bias", "0"]),
    ("pnp", "Q2", &[("model", "qp")], &["c2", "bias", "vdd"]),
    ("tline", "T1", &[], &["clk", "0", "tout", "0"]),
    ("probe", "P1", &[], &["out"]),
];

/// The pins of core cell `cell`: `(name, x, y)` from its `pin` lines.
fn pins(cell: &str) -> Vec<(String, f64, f64)> {
    let text =
        std::str::from_utf8(core_file(cell).expect("a core cell")).expect("a core cell is UTF-8");
    text.lines()
        .filter_map(|l| l.strip_prefix("pin "))
        .map(|l| {
            let fields: Vec<&str> = l.split_whitespace().collect();
            let x: f64 = fields[2].parse().expect("a pin x coordinate");
            let y: f64 = fields[3].parse().expect("a pin y coordinate");
            (fields[0].to_owned(), x, y)
        })
        .collect()
}

/// A bench of `rows`, each instance on its own column with a label on every pin.
fn bench(rows: &[Row]) -> String {
    let mut s = String::from("schemify 1 shbench\n\n");
    let mut labels = String::new();
    for (k, (cell, name, params, nets)) in rows.iter().enumerate() {
        let x = 1000.0 * k as f64;
        let pins = pins(cell);
        assert_eq!(pins.len(), nets.len(), "{cell}: one net per pin");
        let _ = write!(
            s,
            "inst {} core:{cell}\n  at {x} 0 R0\n  name {name}\n",
            common::id(k as u64)
        );
        for (p, v) in params.iter() {
            let _ = writeln!(s, "  {p} {v}");
        }
        let pin_nets = pins.iter().zip(nets.iter());
        for (j, ((_, px, py), net)) in pin_nets.enumerate() {
            let lid = common::id(10_000 + 16 * k as u64 + j as u64);
            let _ = writeln!(labels, "label {lid} {} {py} R0 {net}", x + px);
        }
    }
    s.push('\n');
    s.push_str(&labels);
    s
}

/// The core library plus `bench` as the project's `tb.shbench`, everything open.
fn project(bench: &str) -> (Project, DocHandle) {
    let mut p = Project::new(None, None, 11).expect("an empty project");
    let (_, diags) = p
        .index_library("core", "", core_library())
        .expect("the core library indexes");
    assert!(diags.is_empty(), "{diags:?}");
    for (name, bytes) in core_library() {
        p.open("core", name, bytes, None)
            .expect("a core cell opens");
    }
    let (_, diags) = p
        .index_library("", "", &[("tb.shbench", bench.as_bytes())])
        .expect("the bench indexes");
    assert!(diags.is_empty(), "{diags:?}");
    let top = p
        .open("", "tb.shbench", bench.as_bytes(), None)
        .expect("the bench opens")
        .handle;
    (p, top)
}

fn errors(e: &Elab) -> Vec<String> {
    e.violations
        .rows
        .iter()
        .filter(|v| v.code.severity() == Severity::Error)
        .map(|v| format!("{} {}", v.code.as_str(), v.detail))
        .collect()
}

fn snapshot(name: &str, text: &str) {
    let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).join(name);
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, text).expect("the snapshot is writable");
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing snapshot {name}: run with UPDATE_SNAPSHOTS=1"));
    assert_eq!(text, want, "snapshot {name} differs");
}

#[test]
fn every_core_device_netlists_for_spice_and_spectre() {
    let (p, top) = project(&bench(DEVICES));
    for d in [Dialect::Ngspice, Dialect::Spectre] {
        let e = common::elab(&p, top, d);
        assert!(errors(&e).is_empty(), "{d:?}: {:?}", errors(&e));
        let text = schemify_netlist::emit(&e, d).text;
        assert!(!text.contains("error in"), "{d:?}:\n{text}");
        snapshot(&format!("core_tb.{}", d.as_str()), &text);
    }
    // Spectre numbers are canonical, never a SPICE suffix Spectre reads otherwise
    // (`1M` is milli in SPICE, mega in Spectre; Spectre has no `meg`).
    let text =
        schemify_netlist::emit(&common::elab(&p, top, Dialect::Spectre), Dialect::Spectre).text;
    for line in text.lines() {
        assert!(!line.contains("meg"), "{line}");
    }
    assert!(
        text.contains("R1 (outm 0) resistor r=2200000 m=1\n"),
        "{text}"
    );
    assert!(text.contains("VM1 (out outm) vsource dc=0\n"), "{text}");
    assert!(text.contains("F1 (f 0) cccs probe=VM1 gain=2\n"), "{text}");
    // The probe is a SPICE `.save`; Spectre saves through its own analyses: no line.
    assert!(
        !text
            .lines()
            .any(|l| l.starts_with("P1") || l.contains("save")),
        "{text}"
    );
}

#[test]
fn cells_spectre_cannot_express_are_refused() {
    for (cell, nets) in [
        ("bsource", &["a", "0"][..]),
        ("switch", &["a", "0"][..]),
        ("vswitch", &["a", "0", "c", "0"][..]),
    ] {
        let rows: Vec<Row> = vec![(cell, "U1", &[], nets)];
        let (p, top) = project(&bench(&rows));
        let errs = errors(&common::elab(&p, top, Dialect::Spectre));
        assert!(
            errs.iter()
                .any(|e| e.starts_with("E013") && e.contains("no format for spectre")),
            "{cell}: {errs:?}"
        );
        // The SPICE dialects keep them.
        assert!(
            errors(&common::elab(&p, top, Dialect::Ngspice)).is_empty(),
            "{cell}"
        );
    }
}

#[test]
fn a_source_spec_spectre_cannot_say_fails_its_line_only() {
    let rows: Vec<Row> = vec![
        (
            "vsource",
            "VFM",
            &[("value", "\"sffm(0 1 1k 5 100)\"")],
            &["a", "0"],
        ),
        ("res", "RX", &[("value", "\"1k tc1=0.01\"")], &["a", "0"]),
        ("res", "R1", &[], &["a", "0"]),
    ];
    let (p, top) = project(&bench(&rows));
    let e = common::elab(&p, top, Dialect::Spectre);
    assert!(errors(&e).is_empty(), "{:?}", errors(&e));
    let n = schemify_netlist::emit(&e, Dialect::Spectre);
    assert!(
        n.text.contains("// error in VFM: X009") && n.text.contains("sffm"),
        "{}",
        n.text
    );
    assert!(n.text.contains("// error in RX: X001"), "{}", n.text);
    assert!(
        n.text.contains("R1 (a 0) resistor r=1000 m=1\n"),
        "{}",
        n.text
    );
    // A failed line has no name-map entry.
    assert_eq!(n.name_map.inst_name("/VFM"), None);
    assert_eq!(n.name_map.inst_name("/R1").as_deref(), Some("R1"));
}
