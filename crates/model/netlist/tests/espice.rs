//! The espice dialect (netlist spec §7.2a): ngspice text but for its Verilog-A loads
//! (`.hdl`), its `N` lines' instance parameters, and its Verilog leaves, `N` devices whose
//! sources load with `.verilog`. Oracles: the ngspice text of the same design, respelled,
//! and the hand-written lines of the spec.

mod common;

use schemify_elab::Dialect;

// ---- private ----

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
    let (p, hs) = common::project(&[
        ("vgain.shprim", body),
        ("amp.shprim", file),
        ("tb.shbench", bench),
    ]);
    let ng =
        schemify_netlist::emit(&common::elab(&p, hs[2], Dialect::Ngspice), Dialect::Ngspice).text;
    let e = common::elab(&p, hs[2], Dialect::Espice);
    assert!(
        common::blocking(&e).is_empty(),
        "{:?}",
        common::blocking(&e)
    );
    let es = schemify_netlist::emit(&e, Dialect::Espice).text;
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
fn espice_writes_verilog_leaves_as_n_devices() {
    // Spec §7.2a: a `verilog` leaf is ESPice's `.v` device, `N{name} {pins} {model}
    // {params}`: pins in port order, a bus bit by bit from its left index (`B[7:0]` is
    // `b[7] … b[0]`, as ESPice numbers the module's pins), then every param, defaults
    // included (the A2D/D2A bridge's defaults are VerA's), but the computed ones and the
    // `model` param the card names. Each source loads once, `.verilog "<stem>.v"` (a
    // SystemVerilog `file` keeps `.sv`, a `body` is the cell stem's), with `.model <model>
    // <stem>` per model; an impl with no source gets neither.
    let pins = "pin A in -20 0\n  domain digital\npin Y out 20 0\n  domain digital\n";
    let g = format!(
        "schemify 1 shprim\n\n{pins}pin B[7:0] in -20 20\n  domain digital\n\n\
         param model gm\n  kind string\nparam vth 0.9\nparam rout 100\nparam tr 1n\n  \
         computed 2*rout*1p\n\nimpl v verilog\n  file rtl/g.v\n"
    );
    let s = format!("schemify 1 shprim\n\n{pins}\nimpl v verilog\n  model sm\n  file rtl/s.sv\n");
    let b = format!(
        "schemify 1 shprim\n\n{pins}\nimpl v verilog\n\nbody v\n  \
         | module b(input A, output Y);\n  |   assign Y = A;\n  | endmodule\n"
    );
    let n = format!("schemify 1 shprim\n\n{pins}\nimpl v verilog\n  model nm\n");
    let inst = |id: u32, cell: &str, x: i32, attrs: &str| {
        format!("inst 0a000{id} ./{cell}\n  at {x} 0 R0\n  name U{id}\n{attrs}")
    };
    let label =
        |id: u32, x: i32, y: i32, net: &str| format!("label 0n00{id:02} {x} {y} R0 {net}\n");
    let mut bench = String::from("schemify 1 shbench\n\n");
    bench += &inst(1, "g", 0, "");
    bench += &inst(2, "g", 100, "  model gx\n  vth 0.6\n");
    bench += &inst(3, "s", 200, "");
    bench += &inst(4, "b", 300, "");
    bench += &inst(5, "n", 400, "");
    bench.push('\n');
    for (k, (x, y, net)) in [
        (-20, 0, "a"),
        (-20, 20, "b[7:0]"),
        (20, 0, "y"),
        (80, 0, "a"),
        (80, 20, "b[7:0]"),
        (120, 0, "z"),
        (180, 0, "a"),
        (220, 0, "s"),
        (280, 0, "a"),
        (320, 0, "w"),
        (380, 0, "a"),
        (420, 0, "v"),
    ]
    .into_iter()
    .enumerate()
    {
        bench += &label(k as u32 + 1, x, y, net);
    }
    let (p, hs) = common::project(&[
        ("g.shprim", &g),
        ("s.shprim", &s),
        ("b.shprim", &b),
        ("n.shprim", &n),
        ("tb.shbench", &bench),
    ]);
    let e = common::elab(&p, hs[4], Dialect::Espice);
    assert!(
        common::blocking(&e).is_empty(),
        "{:?}",
        common::blocking(&e)
    );
    let es = schemify_netlist::emit(&e, Dialect::Espice).text;
    let bus = "b[7] b[6] b[5] b[4] b[3] b[2] b[1] b[0]";
    // `g`'s pins are A, Y, B[7:0] in that order.
    for line in [
        format!("NU1 a y {bus} gm vth=0.9 rout=100"),
        format!("NU2 a z {bus} gx vth=0.6 rout=100"),
        "NU3 a s sm".to_owned(),
        "NU4 a w b".to_owned(),
        "NU5 a v nm".to_owned(),
    ] {
        assert!(es.contains(&format!("\n{line}\n")), "{line}:\n{es}");
    }
    assert!(
        es.ends_with(
            "\n.model b b\n.model gm g\n.model gx g\n.model sm s\n\
             .verilog \"b.v\"\n.verilog \"g.v\"\n.verilog \"s.sv\"\n.end\n"
        ),
        "{es}"
    );
    // No checkout path (§7.6.14), no `.hdl` or XSPICE text, no computed or `model=` param.
    for no in ["/", ".hdl", "d_cosim", "[ ", "tr=", "model="] {
        assert!(!es.contains(no), "{no}: {es}");
    }
    // ngspice keeps the d_cosim form (positive space); xyce and cdl stay E013.
    let ng =
        schemify_netlist::emit(&common::elab(&p, hs[4], Dialect::Ngspice), Dialect::Ngspice).text;
    assert!(
        ng.contains("\nAU3 [ a ] [ s ] sm\n") && ng.contains("d_cosim") && !ng.contains(".verilog"),
        "{ng}"
    );
    for d in [Dialect::Xyce, Dialect::Cdl] {
        let errs = common::blocking(&common::elab(&p, hs[4], d));
        assert_eq!(errs.len(), 4, "{d:?}: one E013 per verilog cell: {errs:?}");
        assert!(errs.iter().all(|b| b.starts_with("E013")), "{errs:?}");
    }
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
    let (p, hs) = common::project(&[
        ("gain.shprim", va),
        ("fin.shprim", fin),
        ("tb.shbench", bench),
    ]);
    let e = common::elab(&p, hs[2], Dialect::Espice);
    assert!(
        common::blocking(&e).is_empty(),
        "{:?}",
        common::blocking(&e)
    );
    let es = schemify_netlist::emit(&e, Dialect::Espice).text;
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
    let ng =
        schemify_netlist::emit(&common::elab(&p, hs[2], Dialect::Ngspice), Dialect::Ngspice).text;
    assert!(
        ng.contains("\nNG1 in out gain_x\n") && ng.contains("\nNG2 in 0 gain_m\n"),
        "{ng}"
    );
    assert!(ng.contains("\nNM1 out in 0 0 nch nfin=3 m=1\n"), "{ng}");
}
