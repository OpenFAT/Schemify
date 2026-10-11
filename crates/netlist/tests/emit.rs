//! Emission on the hand-written fixtures (`tests/fixtures/`, original designs):
//! each dialect's text is compared byte for byte with a snapshot of our own output
//! (`tests/fixtures/<bench>.<dialect>`; `UPDATE_SNAPSHOTS=1` rewrites them, to be
//! reviewed in the diff), and the SPICE texts with hand-written expected netlists
//! under `schemify_spicecmp` (independent of our naming and order).

mod common;

use common::*;
use schemify_elab::{Dialect, Elab};
use schemify_netlist::emit;
use std::path::Path;

const DIALECTS: [Dialect; 5] = [
    Dialect::Ngspice,
    Dialect::Xyce,
    Dialect::Cdl,
    Dialect::Spectre,
    Dialect::Verilog,
];

fn fixture_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"))
}

/// Compares `text` with the snapshot `name`, or rewrites it under `UPDATE_SNAPSHOTS`.
fn snapshot(name: &str, text: &str) {
    let path = fixture_dir().join(name);
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing snapshot {name}: run with UPDATE_SNAPSHOTS=1"));
    assert_eq!(text, want, "snapshot {name} differs");
}

fn blocking(e: &Elab) -> Vec<String> {
    e.violations
        .rows
        .iter()
        .filter(|v| v.code.severity() == schemify_elab::Severity::Error)
        .map(|v| format!("{} {}", v.code.as_str(), v.detail))
        .collect()
}

#[test]
fn tb_snapshots_per_dialect() {
    let files = fixtures();
    let (p, top) = fixture(&files, "tb.shbench");
    for d in DIALECTS {
        let e = elab(&p, top, d);
        if matches!(d, Dialect::Verilog) {
            // Leaf impls without `format verilog` are E013 (spec §7.5): gated.
            assert!(
                blocking(&e).iter().all(|b| b.starts_with("E013")),
                "{:?}",
                blocking(&e)
            );
            continue;
        }
        assert!(blocking(&e).is_empty(), "{d:?}: {:?}", blocking(&e));
        let n = emit(&e, d);
        snapshot(&format!("tb.{}", d.as_str()), &n.text);
    }
}

#[test]
fn tb_ngspice_matches_the_expected_netlist() {
    let files = fixtures();
    let (p, top) = fixture(&files, "tb.shbench");
    for (d, expected) in [
        (Dialect::Ngspice, "tb.expected.spice"),
        (Dialect::Cdl, "tb.expected.cdl"),
    ] {
        let n = emit(&elab(&p, top, d), d);
        let want = std::fs::read_to_string(fixture_dir().join(expected)).unwrap();
        let r = schemify_spicecmp::compare(&want, &n.text, &Default::default());
        assert!(r.is_equal(), "{d:?}:\n{:#?}\n{}", r.diffs, n.text);
    }
}

#[test]
fn tb_name_map_round_trips() {
    let files = fixtures();
    let (p, top) = fixture(&files, "tb.shbench");
    let n = emit(&elab(&p, top, Dialect::Ngspice), Dialect::Ngspice);
    let m = &n.name_map;
    for (path, name) in [
        ("/X1", "X1"),
        ("/X1/X2", "X1.X2"),
        ("/X2/X1/MN", "X2.X1.MMN"),
        ("/C[0]", "CC[0]"),
        ("/F1", "XF1"),
    ] {
        assert_eq!(m.inst_name(path).as_deref(), Some(name), "{path}");
        assert_eq!(m.inst_path(name).as_deref(), Some(path), "{name}");
        assert_eq!(
            m.inst_path(&name.to_ascii_lowercase()).as_deref(),
            Some(path)
        );
    }
    // Ignored instances are not emitted, so they have no name.
    assert_eq!(m.inst_name("/RIG"), None);
    assert_eq!(m.inst_name("/RSH"), None);
    assert_eq!(m.inst_name("/X9"), None);
    assert_eq!(m.inst_path("X1.X9"), None);
    // Nets: ports resolve upward, globals are bare, internal nets get the path.
    assert_eq!(m.net_name("/out").as_deref(), Some("out"));
    assert_eq!(m.net_name("/X1/OUT").as_deref(), Some("out"));
    assert_eq!(
        m.net_name("/X1/X1/Y").as_deref(),
        m.net_name("/X1/X2/A").as_deref()
    );
    assert_eq!(m.net_name("/X2/X1/VDD").as_deref(), Some("vdd!"));
    assert_eq!(m.net_name("/X1/X1/0").as_deref(), Some("0"));
    assert_eq!(m.net_name("/X1/nope"), None);
    let mid = m.net_name("/X1/X1/Y").unwrap();
    assert!(mid.starts_with("X1.net_"), "{mid}");
    assert_eq!(
        m.net_path(&mid).as_deref(),
        Some(&*format!("/X1/{}", &mid[3..]))
    );
    assert_eq!(m.net_path("VDD!").as_deref(), Some("/vdd!"));
    assert_eq!(m.net_path("filt").as_deref(), Some("/filt"));
    // `ignore short` merged out2 and outs into the smaller name.
    assert_eq!(m.net_name("/outs"), None);
    assert!(n.text.contains("CCOUT out2 0 1e-14"), "{}", n.text);
}

#[test]
fn logic_and_mixed_benches() {
    let files = fixtures();
    for (bench, dialects) in [
        ("dig_tb.shbench", &[Dialect::Ngspice, Dialect::Verilog][..]),
        ("mixed_tb.shbench", &[Dialect::Ngspice][..]),
    ] {
        let (p, top) = fixture(&files, bench);
        for &d in dialects {
            let e = elab(&p, top, d);
            assert!(blocking(&e).is_empty(), "{bench} {d:?}: {:?}", blocking(&e));
            let n = emit(&e, d);
            let stem = bench.trim_end_matches(".shbench");
            snapshot(&format!("{stem}.{}", d.as_str()), &n.text);
        }
    }
    // Veriloga and Verilog leaves have no default template for xyce or cdl (E013).
    let (p, top) = fixture(&files, "mixed_tb.shbench");
    for d in [Dialect::Xyce, Dialect::Cdl] {
        assert!(
            blocking(&elab(&p, top, d))
                .iter()
                .any(|b| b.starts_with("E013"))
        );
    }
}
