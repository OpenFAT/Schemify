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

#[test]
fn xspice_vector_bus_bits_are_respelled_everywhere() {
    // ngspice splits `d[0]` inside an XSPICE port vector (spec §7.2): every net written
    // in `[ … ]` is spelled `d_0` in the whole netlist, never merging with an existing
    // `d_1`; other dialects keep the bracket spelling.
    let dbus = "schemify 1 shprim\n\npin A[1:0] in -20 0\n  domain digital\npin Y out 20 0\n  domain digital\n\nimpl verilog verilog\n  model dbus\n";
    let res = "schemify 1 shprim\n  prefix R\n\npin p inout 0 -30\npin n inout 0 30\n\nimpl spice spice\n  format spice \"R{name} {p} {n} 1k\"\n";
    let bench = "schemify 1 shbench\n\ninst 0a0001 ./dbus\n  at 0 0 R0\n  name U1\ninst 0r0001 ./res\n  at 100 0 R0\n  name R1\ninst 0r0002 ./res\n  at 200 0 R0\n  name R2\n\nlabel 0n0001 -20 0 R0 d[1:0]\nlabel 0n0002 20 0 R0 y\nlabel 0n0003 100 -30 R0 d[0]\nlabel 0n0004 100 30 R0 0\nlabel 0n0005 200 -30 R0 d_1\nlabel 0n0006 200 30 R0 0\n";
    let (p, hs) = project(&[
        ("dbus.shprim", dbus),
        ("res.shprim", res),
        ("tb.shbench", bench),
    ]);
    let e = elab(&p, hs[2], Dialect::Ngspice);
    assert!(blocking(&e).is_empty(), "{:?}", blocking(&e));
    let text = emit(&e, Dialect::Ngspice).text;
    assert!(text.contains("AU1 [ d_1_ d_0 ] [ y ] dbus"), "{text}");
    assert!(
        text.contains("RR1 d_0 0 1k"),
        "the bit is respelled everywhere: {text}"
    );
    assert!(
        text.contains("RR2 d_1 0 1k"),
        "an existing d_1 keeps its net: {text}"
    );
    assert!(!text.contains("d[0]") && !text.contains("d[1]"), "{text}");
    // Spectre/CDL keep brackets; xyce has no verilog default template.
    let n = emit(&elab(&p, hs[2], Dialect::Spectre), Dialect::Spectre).text;
    assert!(!n.contains("d_0"), "{n}");
}

#[test]
fn component_code_is_emitted_inside_every_definition() {
    // D17 as amended (spec §7.6.15): a `.shcomp`'s `code` is its own netlist text,
    // written after the instance lines of each of its specializations; the bench's code
    // stays at the top, once; CDL takes none; xyce refuses `.control` (E033).
    let res = "schemify 1 shprim\n\npin p inout 0 -30\npin n inout 0 30\n\nparam R 1k\n\nimpl spice spice\n  format spice \"R{name} {p} {n} {R}\"\n";
    let comp = "schemify 1 shcomp\n\npin A inout -20 0\npin B inout 20 0\n\nparam r 1k\n\nimpl sch schematic\n\ninst 0r0001 ./res\n  at 0 0 R0\n  name R1\n  R r\n\nport 0p0001 0 -30 R0 A\nport 0p0002 0 30 R0 B\n\ncode ctl spice\n  | .control\n  | .endc\n\ncode models spice\n  | .model dm d is=1e-14\n";
    let bench = "schemify 1 shbench\n\ninst 0x0001 ./comp\n  at 0 0 R0\n  name X1\ninst 0x0002 ./comp\n  at 100 0 R0\n  name X2\n  r 2k\n\nlabel 0n0001 -20 0 R0 a\nlabel 0n0002 20 0 R0 0\nlabel 0n0003 80 0 R0 a\nlabel 0n0004 120 0 R0 0\n\ncode tran spice\n  | .tran 1n 10n\n";
    let (p, hs) = project(&[
        ("res.shprim", res),
        ("comp.shcomp", comp),
        ("tb.shbench", bench),
    ]);
    let e = elab(&p, hs[2], Dialect::Ngspice);
    assert!(blocking(&e).is_empty(), "{:?}", blocking(&e));
    let text = emit(&e, Dialect::Ngspice).text;
    let blocks: Vec<&str> = text.split(".subckt ").skip(1).collect();
    assert_eq!(blocks.len(), 2, "two specializations: {text}");
    for b in &blocks {
        let body = &b[..b.find(".ends").expect("closed")];
        let (inst, model) = (body.find("\nRR1 "), body.find("\n.model dm d is=1e-14\n"));
        assert!(inst.is_some() && model.is_some(), "{text}");
        assert!(inst < model, "code after the instance lines: {text}");
        assert!(
            body.contains(".control\n.endc\n.model dm"),
            "blocks by name: {text}"
        );
    }
    assert_eq!(text.matches(".model dm d").count(), 2, "{text}");
    assert_eq!(text.matches(".tran 1n 10n").count(), 1, "{text}");
    assert!(
        text.find(".tran 1n 10n") < text.find(".subckt "),
        "bench code stays at the top: {text}"
    );
    // CDL: no code at all. Xyce: the `.control` block is E033, the model is kept.
    let cdl = emit(&elab(&p, hs[2], Dialect::Cdl), Dialect::Cdl).text;
    assert!(
        !cdl.contains(".model") && !cdl.contains(".control"),
        "{cdl}"
    );
    let x = elab(&p, hs[2], Dialect::Xyce);
    assert!(
        blocking(&x).iter().any(|b| b.starts_with("E033")),
        "{:?}",
        blocking(&x)
    );
    let xyce = emit(&x, Dialect::Xyce).text;
    assert_eq!(xyce.matches(".model dm d").count(), 2, "{xyce}");
    assert!(!xyce.contains(".control"), "{xyce}");
    // The component as the top: its code sits in the (unwrapped) top block.
    let top = emit(&elab(&p, hs[1], Dialect::Ngspice), Dialect::Ngspice).text;
    assert_eq!(top.matches(".model dm d").count(), 1, "{top}");
    assert!(!top.contains(".subckt"), "{top}");
}

#[test]
fn veriloga_bodies_are_source_or_precompiled_text() {
    // A `veriloga` body with an `endmodule` line is source the runner compiles (§7.6.8):
    // loaded and modelled like a `file`, never emitted; one without is precompiled SPICE
    // text, emitted as is. A `verilog` body is always source.
    let src = "schemify 1 shprim\n\npin p inout 0 -30\npin n inout 0 30\n\nimpl va veriloga\n  \
               model vsrc_m\n\nbody va\n  | `include \"disciplines.vams\"\n  \
               | module vsrc(p, n);\n  |   electrical p, n;\n  \
               |   analog I(p, n) <+ V(p, n) / 1000.0;\n  | endmodule\n";
    let pre = "schemify 1 shprim\n\npin p inout 0 -30\npin n inout 0 30\n\nimpl va veriloga\n  \
               model pre_m\n\nbody va\n  | .control\n  | pre_osdi pre.osdi\n  | .endc\n  \
               | .model pre_m pre\n";
    let bench = "schemify 1 shbench\n\ninst 0a0001 ./vsrc\n  at 0 0 R0\n  name N1\n\
                 inst 0a0002 ./pre\n  at 100 0 R0\n  name N2\n\nlabel 0n0001 0 -30 R0 a\n\
                 label 0n0002 0 30 R0 0\nlabel 0n0003 100 -30 R0 a\nlabel 0n0004 100 30 R0 0\n";
    let (p, hs) = project(&[
        ("vsrc.shprim", src),
        ("pre.shprim", pre),
        ("tb.shbench", bench),
    ]);
    let e = elab(&p, hs[2], Dialect::Ngspice);
    assert!(blocking(&e).is_empty(), "{:?}", blocking(&e));
    let compiled: Vec<(&str, bool)> = (0..e.impls.cell.len())
        .map(|l| {
            let cell = e.names.get(e.cells[e.impls.cell[l] as usize].stem);
            (cell, schemify_netlist::compiled_source(&e, l))
        })
        .collect();
    assert!(
        compiled.contains(&("vsrc", true)) && compiled.contains(&("pre", false)),
        "{compiled:?}"
    );
    let text = emit(&e, Dialect::Ngspice).text;
    assert!(
        text.contains(".control\npre_osdi vsrc.osdi\n.endc\n"),
        "{text}"
    );
    assert!(text.contains("\n.model vsrc_m vsrc\n"), "{text}");
    assert!(
        !text.contains("endmodule") && !text.contains("disciplines"),
        "{text}"
    );
    assert!(
        text.contains("\npre_osdi pre.osdi\n.endc\n.model pre_m pre\n"),
        "{text}"
    );
    assert!(
        text.contains("\nN1 a 0 vsrc_m\n") && text.contains("\nN2 a 0 pre_m\n"),
        "{text}"
    );
    // CDL: neither is LVS text.
    let cdl = emit(&elab(&p, hs[2], Dialect::Cdl), Dialect::Cdl).text;
    assert!(!cdl.contains("osdi") && !cdl.contains("endmodule"), "{cdl}");
}
