//! Name maps on the fixtures and on small projects (netlist spec §7.6): netlist names to
//! hierarchical paths and back per dialect, ignored instances unnamed, nets resolved
//! upward through ports, case folding exactly where the dialect folds. Oracle: the paths
//! and names read off the hand-written fixtures.

mod common;

use schemify_elab::Dialect;

// ---- private ----

#[test]
fn tb_name_map_round_trips() {
    let files = common::fixtures();
    let (p, top) = common::fixture(&files, "tb.shbench");
    let n = schemify_netlist::emit(&common::elab(&p, top, Dialect::Ngspice), Dialect::Ngspice);
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
    let mid = m
        .net_name("/X1/X1/Y")
        .expect("a net of an emitted instance");
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
    let files = common::fixtures();
    let (p, top) = common::fixture(&files, "tb.shbench");
    let n = schemify_netlist::emit(&common::elab(&p, top, Dialect::Spectre), Dialect::Spectre);
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

    let (p, top) = common::fixture(&files, "dig_tb.shbench");
    let n = schemify_netlist::emit(&common::elab(&p, top, Dialect::Verilog), Dialect::Verilog);
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
    let (p, hs) = common::project(&[("res.shprim", res), ("tb.shbench", bench)]);
    let n = schemify_netlist::emit(&common::elab(&p, hs[1], Dialect::Ngspice), Dialect::Ngspice);
    let m = &n.name_map;
    for name in ["R1", "r1"] {
        assert_eq!(m.inst_path(name).as_deref(), Some(&*format!("/{name}")));
    }
    for name in ["ab", "AB"] {
        assert_eq!(m.net_path(name).as_deref(), Some(&*format!("/{name}")));
    }
    assert!(m.inst_path("R1").is_some() && m.net_path("Ab").is_some());
}
