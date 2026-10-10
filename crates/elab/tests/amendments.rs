//! Integrator amendments on m1 (`458ec8b`): `global true` labels are global through
//! the whole hierarchy (netlist §3), impls carry `ignore` (format §4, netlist §7.1),
//! and `${NAME}` expands in code, body and corner paths with `E034` when undefined.

mod common;

use common::*;
use schemify_doc::Project;
use schemify_elab::{CellState, Code, Elab, Ignore, Name, Request, elaborate, no_funcs};

const RES: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nparam R 1k\n\nimpl spice spice\n  format spice \"R{name} {A} {B} {R}\"\n";

fn row(e: &Elab, d: u32, name: &str) -> usize {
    e.insts_of(d)
        .find(|&r| e.names.get(e.insts.name[r]) == name)
        .unwrap()
}

fn conn_names(e: &Elab, r: usize) -> Vec<String> {
    e.conns(r)
        .iter()
        .map(|n| e.names.get(*n).to_owned())
        .collect()
}

#[test]
fn global_labels_are_global_through_the_whole_hierarchy() {
    // `sub` declares VDDA global on one label; the bench's plain `VDDA` label is then
    // the same global net (no W016 for it, listed in the globals).
    let sub = "schemify 1 shcomp\n\npin P inout 0 0\n\nimpl sch schematic\n\ninst 0r0001 ./res\n  at 0 0 R0\n  name R1\n\nport 0p0001 0 0 R0 P\nlabel 0n0001 0 20 R0 VDDA\n  global true\n";
    let bench = "schemify 1 shbench\n\ninst 0x0001 ./sub\n  at 0 0 R0\n  name X1\ninst 0x0002 ./res\n  at 100 0 R0\n  name R2\n\nlabel 0n0001 100 0 R0 VDDA\nlabel 0n0002 0 0 R0 a\nlabel 0n0003 100 20 R0 a\n";
    let (p, hs) = project(&[
        ("res.shprim", RES),
        ("sub.shcomp", sub),
        ("tb.shbench", bench),
    ]);
    let e = elab(&p, hs[2]);
    let globals: Vec<&str> = e.globals.iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(globals, ["VDDA"]);
    let w016_vdda = e
        .violations
        .with(Code::W016)
        .any(|v| e.names.get(v.param) == "VDDA");
    assert!(!w016_vdda, "{:?}", codes(&e));
    // The editor sees its own document's global labels.
    let st = CellState::build(&p, hs[1], &mut no_funcs).unwrap();
    let vdda = st.nets.bit(st.names.find("VDDA").unwrap()).unwrap();
    assert!(st.nets.bit_global[vdda as usize]);
}

#[test]
fn impl_ignore_short_and_open() {
    let jumper = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nimpl short spice\n  ignore short\n";
    let probe = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nimpl open spice\n  ignore open\n";
    let bench = "schemify 1 shbench\n\ninst 0j0001 ./jumper\n  at 0 0 R0\n  name J1\ninst 0j0002 ./probe\n  at 200 0 R0\n  name P1\ninst 0r0001 ./res\n  at 0 0 R0\n  name R1\ninst 0r0002 ./res\n  at 0 20 R0\n  name R2\n\nlabel 0n0001 0 0 R0 zed\nlabel 0n0002 0 20 R0 abc\nlabel 0n0003 200 0 R0 p\nlabel 0n0004 200 20 R0 q\n";
    let (p, hs) = project(&[
        ("res.shprim", RES),
        ("jumper.shprim", jumper),
        ("probe.shprim", probe),
        ("tb.shbench", bench),
    ]);
    let e = elab(&p, hs[3]);
    assert_eq!(
        conn_names(&e, row(&e, e.top, "R1")),
        ["abc", "abc"],
        "zed and abc are one net, the smallest name wins"
    );
    assert_eq!(conn_names(&e, row(&e, e.top, "R2"))[0], "abc");
    assert_eq!(e.insts.ignore[row(&e, e.top, "J1")], Ignore::Short);
    let p1 = row(&e, e.top, "P1");
    assert_eq!(e.insts.ignore[p1], Ignore::Open);
    assert!(e.conns(p1).iter().all(|n| *n == Name::NONE));
}

#[test]
fn dollar_names_expand_from_project_sets_then_env() {
    let manifest = "schemify 1 project\n\nset PDK /opt/pdk\n";
    let bench = "schemify 1 shbench\n  corner tt\n\ncorner tt\n  lib ${PDK}/models.lib ${PDK}\n  include ${HOME}/extra.sp\n\ncode a spice\n  | .include ${PDK}/cells.sp\n  | * costs $$5 and $x\ncode b spice\n  | .lib ${NOPE}\n";
    let mut p = Project::new(Some(manifest.as_bytes()), None, 1).unwrap();
    p.index_library("", "", &[("tb.shbench", bench.as_bytes())])
        .unwrap();
    let h = p
        .open("", "tb.shbench", bench.as_bytes(), None)
        .unwrap()
        .handle;
    let env = [("HOME", "/home/u"), ("PDK", "/env/loses")];
    let req = Request {
        env: &env,
        ..ngspice()
    };
    let e = elaborate(&p, h, &req, &mut no_funcs);
    let args: Vec<&str> = e
        .bench
        .corner_args
        .iter()
        .map(|n| e.names.get(*n))
        .collect();
    assert_eq!(
        args,
        ["/opt/pdk/models.lib", "${PDK}", "/home/u/extra.sp"],
        "sections stay literal"
    );
    let lines: Vec<&str> = e.bench.code_lines.iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(
        lines,
        [
            ".include /opt/pdk/cells.sp",
            "* costs $5 and $x",
            ".lib ${NOPE}"
        ]
    );
    let e034: Vec<&str> = e
        .violations
        .with(Code::E034)
        .map(|v| e.names.get(v.param))
        .collect();
    assert_eq!(e034, ["NOPE"]);
}

#[test]
fn dollar_values_expand_recursively_and_cycles_are_e034() {
    // Imported PDK sets are themselves templates (`${PDK_ROOT}/${PDK}/…`).
    let manifest = "schemify 1 project\n\nset PDK_ROOT /pdk\nset PDK sky130A\nset MODELS ${PDK_ROOT}/${PDK}/libs.tech\nset LOOP_A ${LOOP_B}\nset LOOP_B ${LOOP_A}\n";
    let bench = "schemify 1 shbench\n\ncode a spice\n  | .lib ${MODELS}/combined.lib tt\n  | .include ${LOOP_A}\n";
    let mut p = Project::new(Some(manifest.as_bytes()), None, 1).unwrap();
    p.index_library("", "", &[("tb.shbench", bench.as_bytes())])
        .unwrap();
    let h = p
        .open("", "tb.shbench", bench.as_bytes(), None)
        .unwrap()
        .handle;
    let e = elaborate(&p, h, &ngspice(), &mut no_funcs);
    let lines: Vec<&str> = e.bench.code_lines.iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(lines[0], ".lib /pdk/sky130A/libs.tech/combined.lib tt");
    let e034 = e.violations.with(Code::E034).count();
    assert_eq!(e034, 1, "the cyclic definition is reported once");
}

#[test]
fn generated_net_names_never_merge_two_components() {
    // R1 has the smallest id; each of its pins touches its own unnamed wire. Naming
    // both components after R1 would make them one SPICE node (a shorted resistor).
    let bench = "schemify 1 shbench\n\ninst 0aaaaa ./res\n  at 0 0 R0\n  name R1\ninst 0aaaab ./res\n  at 200 0 R0\n  name R2\n\nwire zzzzz1 -40 0 0 0\nwire zzzzz2 -40 20 0 20\n";
    let (p, hs) = project(&[("res.shprim", RES), ("tb.shbench", bench)]);
    let e = elab(&p, hs[1]);
    let top = e.top;
    let r1 = conn_names(&e, row(&e, top, "R1"));
    assert_eq!(r1, ["net_zzzzz1", "net_zzzzz2"]);
    // R2 touches nothing: its pin-only components fall back to (instance, pin) nodes.
    let r2 = conn_names(&e, row(&e, top, "R2"));
    assert_eq!(r2, ["net_0aaaab_0", "net_0aaaab_1"]);
}
