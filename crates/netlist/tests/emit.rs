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
    // Leaves: every emitted leaf once, each mapping back to a path; bounded by `max`.
    let leaves = m.leaves(usize::MAX);
    assert!(leaves.iter().any(|l| l == "X2.X1.MMN"), "{leaves:?}");
    assert!(
        !leaves.iter().any(|l| l == "X1" || l == "X2"),
        "subckt rows are not leaves"
    );
    for l in &leaves {
        assert!(m.inst_path(l).is_some(), "{l}");
    }
    let mut sorted = leaves.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), leaves.len(), "no leaf twice");
    assert_eq!(m.leaves(1).len(), 1);
    assert!(m.leaves(0).is_empty());
}

#[test]
fn name_map_lookups_follow_the_dialect() {
    // Spectre and verilog names are case-sensitive and escaped (spec §7.6); reverse
    // lookups take a net name with or without the escapes; a path below a leaf is none.
    let files = fixtures();
    let (p, top) = fixture(&files, "tb.shbench");
    let n = emit(&elab(&p, top, Dialect::Spectre), Dialect::Spectre);
    let m = &n.name_map;
    assert_eq!(m.inst_name("/C[0]").as_deref(), Some(r"C\[0\]"));
    assert_eq!(m.inst_path(r"C\[0\]").as_deref(), Some("/C[0]"));
    assert_eq!(m.inst_path(r"c\[0\]"), None, "no case folding");
    assert_eq!(m.net_name("/ld[0]").as_deref(), Some(r"ld\[0\]"));
    for name in [r"ld\[0\]", "ld[0]"] {
        assert_eq!(m.net_path(name).as_deref(), Some("/ld[0]"), "{name}");
    }
    assert_eq!(m.net_path(r"LD\[0\]"), None);
    assert_eq!(m.net_path(r"vdd\!").as_deref(), Some("/vdd!"));
    assert_eq!(m.inst_name("/X1/X1/MN/deeper"), None);
    assert_eq!(m.net_name("/X1/X1/MN/deeper"), None);

    let (p, top) = fixture(&files, "dig_tb.shbench");
    let n = emit(&elab(&p, top, Dialect::Verilog), Dialect::Verilog);
    let m = &n.name_map;
    assert_eq!(m.inst_path("UX").as_deref(), Some("/UX"));
    assert_eq!(m.inst_path("ux"), None, "no case folding");
    assert_eq!(m.inst_path(r"\B[0]").as_deref(), Some("/B[0]"));
    assert_eq!(m.net_name("/d[0]").as_deref(), Some(r"\d[0] "));
    for name in [r"\d[0] ", "d[0]", r"\B[1].net_0w0001"] {
        assert!(m.net_path(name).is_some(), "{name}");
    }
    assert_eq!(m.net_path(r"\D[0] "), None);
}

#[test]
fn spice_reverse_lookups_prefer_the_exact_spelling() {
    // ngspice folds case, so `R1` and `r1` (or nets `ab` and `AB`) fold together; the
    // exact spelling picks one, any other spelling the first.
    let res = "schemify 1 shprim\n\npin p inout 0 -30\npin n inout 0 30\n\nimpl s spice\n  \
               format spice \"{name} {p} {n} 1k\"\n";
    let bench = "schemify 1 shbench\n\ninst 0a0001 ./res\n  at 0 0 R0\n  name R1\n\
                 inst 0a0002 ./res\n  at 100 0 R0\n  name r1\n\n\
                 label 0n0001 0 -30 R0 ab\nlabel 0n0002 0 30 R0 0\n\
                 label 0n0003 100 -30 R0 AB\nlabel 0n0004 100 30 R0 0\n";
    let (p, hs) = project(&[("res.shprim", res), ("tb.shbench", bench)]);
    let n = emit(&elab(&p, hs[1], Dialect::Ngspice), Dialect::Ngspice);
    let m = &n.name_map;
    for name in ["R1", "r1"] {
        assert_eq!(m.inst_path(name).as_deref(), Some(&*format!("/{name}")));
    }
    for name in ["ab", "AB"] {
        assert_eq!(m.net_path(name).as_deref(), Some(&*format!("/{name}")));
    }
    assert!(m.inst_path("R1").is_some() && m.net_path("Ab").is_some());
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

/// The instance lines of a leaf prim `u` (spice impl with `attrs` and `format`),
/// instantiated as U1 (defaults) and U2 (param lines `params2`), in ngspice.
fn u_lines(attrs: &str, format: &str, params2: &str) -> Vec<String> {
    let u = format!(
        "schemify 1 shprim\n\npin A[1:0] in -20 0\npin C in -20 20\npin Y out 20 0\npin Z inout 20 20\n\n\
         param X 0\nparam W 1\nparam P 3\nparam Q 4\nparam ad 0\n  computed W*0.3\n\n\
         impl s spice\n{attrs}  format spice \"{format}\"\n"
    );
    let bench = format!(
        "schemify 1 shbench\n\ninst 0a0001 ./u\n  at 0 0 R0\n  name U1\ninst 0a0002 ./u\n  at 100 0 R0\n  name U2\n{params2}\n\
         label 0n0001 -20 0 R0 a[1:0]\nlabel 0n0002 -20 20 R0 c\nlabel 0n0003 20 0 R0 y\nlabel 0n0004 20 20 R0 z\n\
         label 0n0005 80 0 R0 b[1:0]\nlabel 0n0006 80 20 R0 c\nlabel 0n0007 120 0 R0 y\nlabel 0n0008 120 20 R0 z\n"
    );
    let (p, hs) = project(&[("u.shprim", &u), ("tb.shbench", &bench)]);
    let text = emit(&elab(&p, hs[1], Dialect::Ngspice), Dialect::Ngspice).text;
    text.lines()
        .skip(1)
        .filter(|l| *l != ".end")
        .map(str::to_owned)
        .collect()
}

#[test]
fn template_identifiers_render_their_sources() {
    // Every identifier kind of spec §6 / §7.1, hand-checked: `params` lists the non-default,
    // non-computed params in schema order; `out_pins` is every pin but the `in` ones.
    let ids = [
        "name", "cell", "pins", "in_pins", "out_pins", "params", "model", "file", "A", "C", "Y",
        "Z", "X", "W", "P", "Q", "ad",
    ];
    let bare: Vec<String> = ids.iter().map(|i| format!("{{{i}}}")).collect();
    let attrs = "  model mx\n  file lib/u.inc\n";
    let set = "  W 5\n  P 7\n";
    let lines = u_lines(attrs, &bare.join("|"), set);
    assert_eq!(
        lines,
        [
            "U1|u|a[1] a[0] c y z|a[1] a[0] c|y z||mx|lib/u.inc|a[1] a[0]|c|y|z|0|1|3|4|0.3",
            "U2|u|b[1] b[0] c y z|b[1] b[0] c|y z|W=5 P=7|mx|lib/u.inc|b[1] b[0]|c|y|z|0|5|7|4|1.5",
        ]
    );
    // The expression path (more holes than its inline input buffer) renders the same.
    let wrapped: Vec<String> = ids.iter().map(|i| format!("{{({i})}}")).collect();
    assert_eq!(u_lines(attrs, &wrapped.join("|"), set), lines);
    // Negative space: `model` and `file` without the impl attr name nothing (X004).
    for (id, attrs) in [("model", "  file f\n"), ("file", "  model m\n")] {
        let lines = u_lines(attrs, &format!("U{{name}} {{{id}}}"), "");
        let err = format!("* error in U1: X004: unknown identifier `{id}`");
        assert_eq!(lines[0], err);
    }
}

#[test]
fn a_cnum_param_fails_its_line_only() {
    // The fast form copies texts; a `cnum` value must take the template path and fail
    // with X003 (spec §7.6.1), on that row only. Error comments sort before names.
    let lines = u_lines("", "U{name} {A} {X} {P}", "  P cplx(1,2)\n");
    let err = "* error in U2: X003 at 16..19: a cnum has no text form; use re/im/mag/ph";
    assert_eq!(lines, [err, "UU1 a[1] a[0] 0 3"]);
}

/// The ngspice text of prim `v` (`in` pins A[1:0], B, C; `out` pin Y) with `format`,
/// whose pins carry the bus-bit nets d[1:0], e[0], f[0] and y[0].
fn xspice_line(format: &str) -> String {
    let v = format!(
        "schemify 1 shprim\n\npin A[1:0] in -20 0\npin B in -20 20\npin C in -20 40\npin Y out 20 0\n\n\
         impl s spice\n  format spice \"{format}\"\n"
    );
    let bench = "schemify 1 shbench\n\ninst 0a0001 ./v\n  at 0 0 R0\n  name 1\n\n\
                 label 0n0001 -20 0 R0 d[1:0]\nlabel 0n0002 -20 20 R0 e[0]\n\
                 label 0n0003 -20 40 R0 f[0]\nlabel 0n0004 20 0 R0 y[0]\n";
    let (p, hs) = project(&[("v.shprim", &v), ("tb.shbench", bench)]);
    let text = emit(&elab(&p, hs[1], Dialect::Ngspice), Dialect::Ngspice).text;
    text.lines().nth(1).expect("one line").to_owned()
}

#[test]
fn template_port_vectors_respell_exactly_their_nets() {
    // A hole inside `[ … ]` of the literal text is in an XSPICE vector (spec §7.2); an
    // escaped `{{[}}` is a literal bracket too, and `]` closes the vector.
    for (format, want) in [
        (
            "A{name} {{[}} {B} ] [{A}] [{out_pins}] {C} m",
            "A1 {[} e_0 ] [d_1 d_0] [y_0] f[0] m",
        ),
        ("A{name} [{pins}] m", "A1 [d_1 d_0 e_0 f_0 y_0] m"),
        ("A{name} [{in_pins}] {Y} m", "A1 [d_1 d_0 e_0 f_0] y[0] m"),
        (
            "A{name} {A} {pins} m",
            "A1 d[1] d[0] d[1] d[0] e[0] f[0] y[0] m",
        ),
    ] {
        assert_eq!(xspice_line(format), want, "{format}");
    }
}

#[test]
fn verilog_default_template_connects_ordinary_then_inherited_pins() {
    // Spec §7.5: `<model> <name> (.<bit>(<net>), …);` over the ordinary pins' bits (an
    // alias pin is its target's connection, listed once), then the inherited pins.
    let g = "schemify 1 shprim\n\npin A in -20 0\n  domain digital\npin A_2 in -20 20\n  alias A\n\
             pin B in -20 40\n  domain digital\npin V inout 0 -40\n  inherit vdd\n\
             pin Y out 20 0\n  domain digital\n\nimpl v verilog\n  model gm\n  file g.v\n";
    let bench = "schemify 1 shbench\n\ninst 0a0001 ./g\n  at 0 0 R0\n  name U1\n\n\
                 label 0n0001 -20 0 R0 a\nlabel 0n0002 -20 40 R0 b\nlabel 0n0003 20 0 R0 y\n";
    let (p, hs) = project(&[("g.shprim", g), ("tb.shbench", bench)]);
    let text = emit(&elab(&p, hs[1], Dialect::Verilog), Dialect::Verilog).text;
    assert!(
        text.contains("\n  gm U1 (.A(a), .B(b), .Y(y), .V(vdd));\n"),
        "{text}"
    );
    // ngspice d_cosim: `in` pins in the first vector, the others in pin order.
    let text = emit(&elab(&p, hs[1], Dialect::Ngspice), Dialect::Ngspice).text;
    assert!(text.contains("\nAU1 [ a b ] [ vdd y ] gm\n"), "{text}");
}

#[test]
fn once_only_text_follows_the_rows_that_use_it() {
    // Spec §7.1/§7.2: a compiled Verilog-A impl without a `model` attr is loaded once per
    // file stem and gets one `.model` per distinct `model` param of its rows; a verilog
    // impl's model is a d_cosim of its stem; a spice body is emitted once, wrapped in
    // `lang=spice` for spectre; an impl no row uses emits nothing.
    let va = "schemify 1 shprim\n\npin p inout 0 -30\npin n inout 0 30\n\nparam model m1\n  kind string\n\n\
              impl va veriloga\n  file va/amp1.va\n";
    let dv = "schemify 1 shprim\n\npin A in -20 0\n  domain digital\npin Y out 20 0\n  domain digital\n\n\
              impl v verilog\n  model dvm\n  file hdl/dv.v\n";
    let sp = "schemify 1 shprim\n\npin p inout 0 -30\npin n inout 0 30\n\nimpl s spice\n  \
              format spice \"R{name} {p} {n} rm\"\n  format spectre \"{name} ({p} {n}) rm\"\n\n\
              body s\n  | .model rm r\n";
    let unused = "schemify 1 shprim\n\npin p inout 0 -30\n\nimpl s spice\n  format spice \"Q{name}\"\n\n\
                  body s\n  | .model never r\n";
    let bench = "schemify 1 shbench\n\ninst 0a0001 ./va\n  at 0 0 R0\n  name 1\n\
                 inst 0a0002 ./va\n  at 100 0 R0\n  name 2\n  model m2\n\
                 inst 0a0003 ./va\n  at 200 0 R0\n  name 3\n  model m2\n\
                 inst 0a0004 ./dv\n  at 300 0 R0\n  name 4\ninst 0a0005 ./sp\n  at 400 0 R0\n  name 5\n\n\
                 label 0n0001 0 -30 R0 a\nlabel 0n0002 0 30 R0 0\nlabel 0n0003 100 -30 R0 a\n\
                 label 0n0004 100 30 R0 0\nlabel 0n0005 200 -30 R0 a\nlabel 0n0006 200 30 R0 0\n\
                 label 0n0007 280 0 R0 a\nlabel 0n0008 320 0 R0 y\nlabel 0n0009 400 -30 R0 a\n\
                 label 0n000a 400 30 R0 0\n";
    let (p, hs) = project(&[
        ("va.shprim", va),
        ("dv.shprim", dv),
        ("sp.shprim", sp),
        ("unused.shprim", unused),
        ("tb.shbench", bench),
    ]);
    let text = emit(&elab(&p, hs[4], Dialect::Ngspice), Dialect::Ngspice).text;
    let once = &text[text.find(".control").expect("an OSDI load")..];
    assert_eq!(
        once,
        ".control\npre_osdi amp1.osdi\n.endc\n.model dvm d_cosim simulation=\"dv.vvp\"\n\
         .model m1 amp1\n.model m2 amp1\n.model rm r\n.end\n"
    );
    assert!(
        text.contains("\nN1 a 0 m1\nN2 a 0 m2\nN3 a 0 m2\n"),
        "{text}"
    );
    let s = emit(&elab(&p, hs[4], Dialect::Spectre), Dialect::Spectre).text;
    assert!(
        s.ends_with("\n5 (a 0) rm\nsimulator lang=spice\n.model rm r\nsimulator lang=spectre\n"),
        "{s}"
    );
}

#[test]
fn espice_loads_verilog_a_sources_with_hdl_and_is_ngspice_text_otherwise() {
    // Spec §7.2a: the espice text is the ngspice text with each OSDI load block
    // `.control`/`pre_osdi <stem>.osdi`/`.endc` replaced by `.hdl "<stem>.va"` (and the
    // header's dialect word); `.model` and `N` lines unchanged. One inline-body source and
    // one `file` source.
    let body = "schemify 1 shprim\n\npin p inout 0 -30\npin n inout 0 30\n\nimpl va veriloga\n  \
                model vgain_m\n\nbody va\n  | `include \"disciplines.vams\"\n  \
                | module vgain(p, n);\n  |   electrical p, n;\n  \
                |   analog V(n) <+ 3 * V(p);\n  | endmodule\n";
    let file = "schemify 1 shprim\n\npin p inout 0 -30\npin n inout 0 30\n\nparam model m1\n  kind string\n\n\
                impl va veriloga\n  file va/amp1.va\n";
    let bench = "schemify 1 shbench\n\ninst 0a0001 ./vgain\n  at 0 0 R0\n  name N1\n\
                 inst 0a0002 ./amp\n  at 100 0 R0\n  name 2\n\n\
                 label 0n0001 0 -30 R0 a\nlabel 0n0002 0 30 R0 y\nlabel 0n0003 100 -30 R0 a\n\
                 label 0n0004 100 30 R0 0\n";
    let (p, hs) = project(&[
        ("vgain.shprim", body),
        ("amp.shprim", file),
        ("tb.shbench", bench),
    ]);
    let ng = emit(&elab(&p, hs[2], Dialect::Ngspice), Dialect::Ngspice).text;
    let e = elab(&p, hs[2], Dialect::Espice);
    assert!(blocking(&e).is_empty(), "{:?}", blocking(&e));
    let es = emit(&e, Dialect::Espice).text;
    let want = ng
        .replace("(schemify ngspice)", "(schemify espice)")
        .replace(
            ".control\npre_osdi amp1.osdi\n.endc\n",
            ".hdl \"amp1.va\"\n",
        )
        .replace(
            ".control\npre_osdi vgain.osdi\n.endc\n",
            ".hdl \"vgain.va\"\n",
        );
    assert_ne!(es, ng);
    assert_eq!(es, want);
    assert!(!es.contains("pre_osdi") && !es.contains(".control"), "{es}");
    assert!(
        es.contains(".hdl \"amp1.va\"\n.hdl \"vgain.va\"\n.model m1 amp1\n.model vgain_m vgain\n"),
        "{es}"
    );
    assert!(
        es.contains("\nN1 a y vgain_m\n") && es.contains("\nN2 a 0 m1\n"),
        "{es}"
    );
    // No checkout path in the text (§7.6.14): the runner copies the sources beside it.
    assert!(!es.contains('/'), "{es}");
}

#[test]
fn espice_has_no_default_template_for_verilog_leaves() {
    // ESPice has no XSPICE d_cosim: a `verilog` leaf without a `format spice` is E013.
    let g = "schemify 1 shprim\n\npin A in -20 0\n  domain digital\npin Y out 20 0\n  domain digital\n\n\
             impl v verilog\n  model gm\n  file g.v\n";
    let bench = "schemify 1 shbench\n\ninst 0a0001 ./g\n  at 0 0 R0\n  name U1\n\n\
                 label 0n0001 -20 0 R0 a\nlabel 0n0003 20 0 R0 y\n";
    let (p, hs) = project(&[("g.shprim", g), ("tb.shbench", bench)]);
    let e = elab(&p, hs[1], Dialect::Espice);
    assert!(
        blocking(&e).iter().any(|b| b.starts_with("E013")),
        "{:?}",
        blocking(&e)
    );
    // ngspice still takes it (positive space).
    assert!(blocking(&elab(&p, hs[1], Dialect::Ngspice)).is_empty());
}

#[test]
fn espice_verilog_a_lines_carry_instance_params_and_mfactor() {
    // Spec §7.2a: on the default template ESPice gets every param that differs from its
    // default on the `N` line (it reads instance parameters there), `m` as `mfactor`, and the
    // `model` param only as the model name; ngspice keeps `N{name} {pins} {model}`. A
    // `format espice` overrides the `spice` one for espice only.
    let va = "schemify 1 shprim\n\npin p inout 0 -30\npin n inout 0 30\n\n\
              param model gain_m\n  kind string\nparam a 10\nparam m 1\nparam k 2\n\n\
              impl va veriloga\n  file gain.va\n";
    let fin = "schemify 1 shprim\n\npin d inout 0 -30\npin g in -20 0\npin s inout 0 30\npin b in 20 0\n\n\
               param nfin 1\nparam m 1\n\nimpl osdi veriloga\n  \
               format spice \"N{name} {d} {g} {s} {b} nch nfin={nfin} m={m}\"\n  \
               format espice \"N{name} {d} {g} {s} {b} 0 nch nfin={nfin} mfactor={m}\"\n  \
               file fin.va\n";
    let bench = "schemify 1 shbench\n\ninst 0a0001 ./gain\n  at 0 0 R0\n  name G1\n  a 4\n  m 2\n  model gain_x\n\
                 inst 0a0002 ./gain\n  at 100 0 R0\n  name G2\n\
                 inst 0a0003 ./fin\n  at 200 0 R0\n  name M1\n  nfin 3\n\n\
                 label 0n0001 0 -30 R0 in\nlabel 0n0002 0 30 R0 out\nlabel 0n0003 100 -30 R0 in\n\
                 label 0n0004 100 30 R0 0\nlabel 0n0005 200 -30 R0 out\nlabel 0n0006 180 0 R0 in\n\
                 label 0n0007 200 30 R0 0\nlabel 0n0008 220 0 R0 0\n";
    let (p, hs) = project(&[
        ("gain.shprim", va),
        ("fin.shprim", fin),
        ("tb.shbench", bench),
    ]);
    let e = elab(&p, hs[2], Dialect::Espice);
    assert!(blocking(&e).is_empty(), "{:?}", blocking(&e));
    let es = emit(&e, Dialect::Espice).text;
    assert!(es.contains("\nNG1 in out gain_x a=4 mfactor=2\n"), "{es}");
    assert!(
        es.contains("\nNG2 in 0 gain_m\n"),
        "defaults stay off the line: {es}"
    );
    assert!(
        es.contains("\nNM1 out in 0 0 0 nch nfin=3 mfactor=1\n"),
        "format espice: {es}"
    );
    assert!(!es.contains(" m="), "{es}");
    let ng = emit(&elab(&p, hs[2], Dialect::Ngspice), Dialect::Ngspice).text;
    assert!(
        ng.contains("\nNG1 in out gain_x\n") && ng.contains("\nNG2 in 0 gain_m\n"),
        "{ng}"
    );
    assert!(ng.contains("\nNM1 out in 0 0 nch nfin=3 m=1\n"), "{ng}");
}
