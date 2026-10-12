//! Hierarchy elaboration (netlist spec §1, §2, §5, §6): impl-selection precedence,
//! specialization naming and its stability, recursion, inherited connections, param
//! scopes, bench lines, dialect support, and D20 cached values.

mod common;

use common::*;
use schemify_doc::Project;
use schemify_elab::{
    CellState, CornerLine, Dialect, Elab, Request, Target, Val, elaborate, no_funcs,
};
use schemify_expr::Value;

/// Row of instance `name` in definition `d`.
fn row(e: &Elab, d: u32, name: &str) -> usize {
    e.insts_of(d)
        .find(|&r| e.names.get(e.insts.name[r]) == name)
        .unwrap_or_else(|| panic!("no {name} in def {d}"))
}

/// What row `r` instantiates: `def:<name>` or `<impl name>`.
fn target(e: &Elab, r: usize) -> String {
    match e.insts.target[r] {
        Target::Def(d) => format!("def:{}", e.names.get(e.defs.name[d as usize])),
        Target::Leaf(l) => {
            let f = &e.cells[e.impls.cell[l as usize] as usize];
            e.names
                .get(f.impl_name[e.impls.index[l as usize] as usize])
                .to_owned()
        }
    }
}

fn conn_names(e: &Elab, r: usize) -> Vec<&str> {
    e.conns(r).iter().map(|n| e.names.get(*n)).collect()
}

fn param(e: &Elab, r: usize, name: &str) -> String {
    let k = e
        .params_of(r)
        .find(|&k| e.names.get(e.insts.param_name[k]) == name)
        .unwrap();
    e.names.get(e.insts.param_text[k]).to_owned()
}

const RES: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nparam R 1k\n  min 1\n\nimpl spice spice\n  format spice \"R{name} {A} {B} {R}\"\n";

/// `amp`: one schematic impl (a resistor between IN and OUT whose value is `r`) and two
/// leaf impls.
const AMP: &str = "schemify 1 shcomp\n  default sch\n\npin IN in 0 0\npin OUT out 0 20\n\nparam r 1k\n\nimpl sch schematic\nimpl beh spice\n  format spice \"X{name} {pins} amp_beh\"\nimpl va veriloga\n  model ampva\n\ninst 0r0000 ./res\n  at 0 0 R0\n  name R1\n  R r\n\nport 0p0001 0 0 R0 IN\nport 0p0002 0 20 R0 OUT\n";

fn bench_with(config: &str) -> String {
    format!(
        "schemify 1 shbench\n\ninst 0x0001 ./amp\n  at 0 0 R0\n  name X1\ninst 0x0002 ./amp\n  at 100 0 R0\n  name X2\n\nlabel 0n0001 0 0 R0 a\nlabel 0n0002 0 20 R0 b\nlabel 0n0003 100 0 R0 c\nlabel 0n0004 100 20 R0 d\n\n{config}"
    )
}

fn elab_cfg(bench: &str, cfg: Option<&str>, dialect: Dialect) -> Elab {
    let (p, hs) = project(&[
        ("res.shprim", RES),
        ("amp.shcomp", AMP),
        ("tb.shbench", bench),
    ]);
    let req = Request {
        config: cfg,
        corner: None,
        overrides: &[],
        env: &[],
        dialect,
    };
    elaborate(&p, hs[2], &req, &mut no_funcs)
}

#[test]
fn impl_selection_precedence() {
    // (config record, config name, X1 target, X2 target, codes)
    let cases: &[(&str, &str, &str, &str, &[&str])] = &[
        ("", "", "def:amp", "def:amp", &[]),
        ("config c\n  order beh sch\n", "c", "beh", "beh", &[]),
        ("config c\n  order nope va\n", "c", "va", "va", &[]),
        (
            "config c\n  cell ./amp va\n  order beh\n",
            "c",
            "va",
            "va",
            &[],
        ),
        (
            "config c\n  inst /X1 beh\n  cell ./amp va\n",
            "c",
            "beh",
            "va",
            &[],
        ),
        (
            "config c\n  inst /X1 nope\n  order va\n",
            "c",
            "va",
            "va",
            &["E031"],
        ),
        (
            "config c\n  cell ./amp nope\n  order beh\n",
            "c",
            "beh",
            "beh",
            &["E031"],
        ),
        (
            "config c\n  inst /X2 sch\n  order beh\n",
            "c",
            "beh",
            "def:amp",
            &[],
        ),
    ];
    for (rec, name, x1, x2, want) in cases {
        let bench = bench_with(rec);
        let cfg = (!name.is_empty()).then_some(*name);
        let e = elab_cfg(&bench, cfg, Dialect::Ngspice);
        assert_eq!(target(&e, row(&e, e.top, "X1")), *x1, "{rec}");
        assert_eq!(target(&e, row(&e, e.top, "X2")), *x2, "{rec}");
        let got: Vec<String> = codes(&e)
            .into_iter()
            .filter(|c| c.starts_with('E'))
            .collect();
        assert_eq!(got, *want, "{rec}");
    }
    // The bench's `config` header is the default; an unknown name is E035.
    let bench =
        bench_with("config c\n  order beh\n").replacen("shbench\n", "shbench\n  config c\n", 1);
    let e = elab_cfg(&bench, None, Dialect::Ngspice);
    assert_eq!(target(&e, row(&e, e.top, "X1")), "beh");
    let e = elab_cfg(&bench_with(""), Some("missing"), Dialect::Ngspice);
    assert!(codes(&e).contains(&"E035".to_owned()));
}

#[test]
fn inst_rules_naming_no_instance_are_w037() {
    // (config record, paths reported as W037)
    let cases: &[(&str, &[&str])] = &[
        ("config c\n  inst /X1 beh\n", &[]),
        // Nested path through the selected schematic impl: a real instance.
        ("config c\n  inst /X1/R1 spice\n", &[]),
        ("config c\n  inst /X9 beh\n", &["/X9"]),
        ("config c\n  inst X1 beh\n", &["X1"]),
        // X1 is a leaf here, so nothing lives under it.
        (
            "config c\n  inst /X1 beh\n  inst /X1/R1 spice\n",
            &["/X1/R1"],
        ),
        (
            "config c\n  inst /X2 beh\n  inst /x2 va\n  inst /X3 va\n",
            &["/X3", "/x2"],
        ),
    ];
    for (rec, want) in cases {
        let e = elab_cfg(&bench_with(rec), Some("c"), Dialect::Ngspice);
        let got: Vec<&str> = e
            .violations
            .rows
            .iter()
            .filter(|v| v.code.as_str() == "W037")
            .map(|v| {
                assert_eq!(e.names.get(v.cell), "tb", "located on the bench");
                assert!(v.detail.contains("names no instance"), "{}", v.detail);
                e.names.get(v.path)
            })
            .collect();
        assert_eq!(got, *want, "{rec}");
        // A warning: it never blocks the run.
        assert!(!e.violations.blocking(), "{rec}");
    }
}

#[test]
fn dialect_support_and_templates() {
    let cfg = "config c\n  order va\n";
    let e = elab_cfg(&bench_with(cfg), Some("c"), Dialect::Xyce);
    assert!(
        codes(&e).contains(&"E013".to_owned()),
        "veriloga has no xyce form"
    );
    let e = elab_cfg(&bench_with(cfg), Some("c"), Dialect::Ngspice);
    assert!(!codes(&e).contains(&"E013".to_owned()));
    let l = match e.insts.target[row(&e, e.top, "X1")] {
        Target::Leaf(l) => l as usize,
        Target::Def(_) => panic!(),
    };
    assert_eq!(
        e.impls.template[l],
        schemify_elab::Name::NONE,
        "default template"
    );
    // `format spice` serves ngspice, xyce and cdl; spectre needs its own.
    let e = elab_cfg(&bench_with(""), None, Dialect::Spectre);
    assert!(
        codes(&e).contains(&"E013".to_owned()),
        "res has no spectre format: {:?}",
        codes(&e)
    );
    let e = elab_cfg(&bench_with(""), None, Dialect::Cdl);
    let amp = match e.insts.target[row(&e, e.top, "X1")] {
        Target::Def(d) => d,
        Target::Leaf(_) => panic!(),
    };
    let r = row(&e, amp, "R1");
    let Target::Leaf(l) = e.insts.target[r] else {
        panic!()
    };
    assert_eq!(
        e.names.get(e.impls.template[l as usize]),
        "R{name} {A} {B} {R}"
    );
}

/// FNV-1a 64 + 6 Crockford base32 digits, written independently of elab.
fn spec_hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    let a = b"0123456789abcdefghjkmnpqrstvwxyz";
    (0..6)
        .map(|i| a[((h >> (59 - 5 * i)) & 31) as usize] as char)
        .collect()
}

#[test]
fn specialization_names_are_stable_and_shared() {
    let bench = "schemify 1 shbench\n\ninst 0x0001 ./amp\n  at 0 0 R0\n  name X1\ninst 0x0002 ./amp\n  at 100 0 R0\n  name X2\n  r 2k\ninst 0x0003 ./amp\n  at 200 0 R0\n  name X3\n  r 2000\ninst 0x0004 ./amp\n  at 300 0 R0\n  name X4\n  r 1000\ninst 0x0005 ./amp\n  at 400 0 R0\n  name X5\n  r 3k\n";
    let e = elab_cfg(bench, None, Dialect::Ngspice);
    let names: Vec<String> = ["X1", "X2", "X3", "X4", "X5"]
        .iter()
        .map(|x| target(&e, row(&e, e.top, x)))
        .collect();
    let two = format!("def:amp__{}", spec_hash("r=2000"));
    let three = format!("def:amp__{}", spec_hash("r=3000"));
    assert_eq!(
        names,
        [
            "def:amp",
            two.as_str(),
            two.as_str(),
            "def:amp",
            three.as_str()
        ]
    );
    let amp_defs = e
        .defs
        .cell
        .iter()
        .filter(|&&c| e.names.get(e.cells[c as usize].stem) == "amp")
        .count();
    assert_eq!(amp_defs, 3, "identical tuples share one definition");
    // Inside the specialization the value is concrete.
    let d = match e.insts.target[row(&e, e.top, "X2")] {
        Target::Def(d) => d,
        Target::Leaf(_) => panic!(),
    };
    assert_eq!(param(&e, row(&e, d, "R1"), "R"), "2000");
    // Stable across unrelated edits and record order: reverse the instances, add one.
    let shuffled = "schemify 1 shbench\n\ninst 0x0005 ./amp\n  at 0 0 R0\n  name X5\n  r 3k\ninst 0x0002 ./amp\n  at 9 9 R90\n  name X2\n  r 2k\ninst 0x0009 ./res\n  at 500 0 R0\n  name R9\n";
    let e2 = elab_cfg(shuffled, None, Dialect::Ngspice);
    assert_eq!(target(&e2, row(&e2, e2.top, "X2")), two);
    assert_eq!(target(&e2, row(&e2, e2.top, "X5")), three);
}

#[test]
fn path_rules_split_definitions_with_equal_params() {
    // Two `mid` instances with equal params; a config `inst` rule changes the impl of
    // one leaf under /M2 only, so the two `mid` definitions differ and must not share
    // a name.
    let mid = "schemify 1 shcomp\n\npin P inout 0 0\n\nimpl sch schematic\n\ninst 0a0001 ./amp\n  at 0 0 R0\n  name A1\n\nport 0p0001 0 0 R0 P\n";
    let bench = "schemify 1 shbench\n\ninst 0x0001 ./mid\n  at 0 0 R0\n  name M1\ninst 0x0002 ./mid\n  at 100 0 R0\n  name M2\n\nconfig c\n  inst /M2/A1 beh\n";
    let (p, hs) = project(&[
        ("res.shprim", RES),
        ("amp.shcomp", AMP),
        ("mid.shcomp", mid),
        ("tb.shbench", bench),
    ]);
    let req = Request {
        config: Some("c"),
        ..ngspice()
    };
    let e = elaborate(&p, hs[3], &req, &mut no_funcs);
    let (m1, m2) = (
        target(&e, row(&e, e.top, "M1")),
        target(&e, row(&e, e.top, "M2")),
    );
    assert_ne!(m1, m2, "different children, different definitions");
    assert!(
        m1.starts_with("def:mid__") && m2.starts_with("def:mid__"),
        "{m1} {m2}"
    );
    let names: Vec<&str> = e.defs.name.iter().map(|n| e.names.get(*n)).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        names.len(),
        "definition names are unique: {names:?}"
    );
}

#[test]
fn recursion_is_reported_not_followed() {
    let a = "schemify 1 shcomp\n\npin P inout 0 0\n\nimpl sch schematic\n\ninst 0a0001 ./b\n  at 0 0 R0\n  name XB\n\nport 0p0001 0 0 R0 P\n";
    let b = "schemify 1 shcomp\n\npin P inout 0 0\n\nimpl sch schematic\n\ninst 0b0001 ./a\n  at 0 0 R0\n  name XA\n\nport 0p0002 0 0 R0 P\n";
    let selfie = "schemify 1 shcomp\n\npin P inout 0 0\n\nimpl sch schematic\n\ninst 0c0001 ./selfie\n  at 0 0 R0\n  name XS\n\nport 0p0003 0 0 R0 P\n";
    let bench = "schemify 1 shbench\n\ninst 0x0001 ./a\n  at 0 0 R0\n  name X1\ninst 0x0002 ./selfie\n  at 100 0 R0\n  name X2\n";
    let (p, hs) = project(&[
        ("a.shcomp", a),
        ("b.shcomp", b),
        ("selfie.shcomp", selfie),
        ("tb.shbench", bench),
    ]);
    let e = elab(&p, hs[3]);
    let paths: Vec<&str> = e
        .violations
        .with(schemify_elab::Code::E014)
        .map(|v| e.names.get(v.path))
        .collect();
    assert_eq!(paths, ["/X1/XB/XA", "/X2/XS"]);
}

const INV: &str = "schemify 1 shprim\n\npin A in 0 0\npin Y out 20 0\npin VDD inout 10 -10\n  inherit vdd!\npin VSS inout 10 10\n  inherit 0\n\nimpl spice spice\n";

#[test]
fn inherited_connections_resolve_up_the_path() {
    // buf: I1 overrides VSS locally; I2 inherits both. So buf takes VDD and VSS as
    // propagated inherited ports (sorted), and the top resolves them per instance.
    let buf = "schemify 1 shcomp\n\npin A in 0 0\npin Y out 100 0\n\nimpl sch schematic\n\ninst 0j0001 ./inv\n  at 0 0 R0\n  name I1\n  inherit VSS gnd_local\ninst 0j0002 ./inv\n  at 20 0 R0\n  name I2\n\nport 0p0001 0 0 R0 A\nport 0p0002 40 0 R0 Y\n";
    let bench = "schemify 1 shbench\n\ninst 0x0001 ./buf\n  at 0 0 R0\n  name X1\ninst 0x0002 ./buf\n  at 200 0 R0\n  name X2\n  inherit VDD vccb\n\nlabel 0n0001 0 0 R0 in1\nlabel 0n0002 100 0 R0 out1\nlabel 0n0003 200 0 R0 in2\nlabel 0n0004 300 0 R0 out2\n";
    let (p, hs) = project(&[
        ("inv.shprim", INV),
        ("buf.shcomp", buf),
        ("tb.shbench", bench),
    ]);
    let e = elab(&p, hs[2]);
    assert!(!e.violations.blocking(), "{:?}", codes(&e));
    let x1 = row(&e, e.top, "X1");
    assert_eq!(conn_names(&e, x1), ["in1", "out1", "vdd!", "0"]);
    assert_eq!(
        conn_names(&e, row(&e, e.top, "X2")),
        ["in2", "out2", "vccb", "0"]
    );
    let Target::Def(d) = e.insts.target[x1] else {
        panic!()
    };
    let ports: Vec<&str> = e.ports(d).iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(ports, ["A", "Y", "VDD", "VSS"]);
    assert_eq!(
        conn_names(&e, row(&e, d, "I1")),
        // A net of instance pins only is `net_<inst id>_<pin index>` (spec §3).
        ["A", "net_0j0001_1", "VDD", "gnd_local"]
    );
    assert_eq!(
        conn_names(&e, row(&e, d, "I2")),
        ["net_0j0001_1", "Y", "VDD", "VSS"]
    );
    let globals: Vec<&str> = e.globals.iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(globals, ["0", "vdd!"]);
    // A cell netlisted on its own: its declared/propagated pins are bound to defaults.
    let e = elab(&p, hs[1]);
    assert_eq!(conn_names(&e, row(&e, e.top, "I2"))[2..], ["vdd!", "0"]);
}

#[test]
fn params_evaluate_in_parent_then_child_scope_with_corner_and_overrides() {
    let dev = "schemify 1 shprim\n\npin D inout 0 0\n\nparam W 1\nparam L 0.25\nparam ad 0\n  computed W*L\n\nimpl spice spice\n";
    let amp = "schemify 1 shcomp\n\npin P inout 0 0\n\nparam Wp 2\n\nimpl sch schematic\n\ninst 0m0001 ./dev\n  at 0 0 R0\n  name M1\n  W Wp*k\n  ad 9\n  bogus 1\n\nport 0p0001 0 0 R0 P\n\nvar k 3\n";
    let bench = "schemify 1 shbench\n  corner tt\n\ninst 0x0001 ./amp\n  at 0 0 R0\n  name X1\n  Wp vdd*2\n\nvar vdd 1.8\n\ncorner ff\n  lib ${PDK}/models.lib ff\n  set vdd 2\n  temp -40\ncorner tt\n  include ./tt.spice\n  set vdd 1.8\n  temp vdd*10\n\ncode b2 ngspice\n  | .op\ncode a1 spice\n  | .control\n  | run\n  | .endc\ncode z9 xyce\n  | .tran 1n 1u\n";
    let (p, hs) = project(&[
        ("dev.shprim", dev),
        ("amp.shcomp", amp),
        ("tb.shbench", bench),
    ]);
    let run = |corner: Option<&str>, ovr: &[(&str, &str)], d: Dialect| {
        let req = Request {
            config: None,
            corner,
            overrides: ovr,
            env: &[],
            dialect: d,
        };
        elaborate(&p, hs[2], &req, &mut no_funcs)
    };
    let e = run(Some("ff"), &[], Dialect::Ngspice);
    let Target::Def(d) = e.insts.target[row(&e, e.top, "X1")] else {
        panic!()
    };
    let m1 = row(&e, d, "M1");
    assert_eq!(
        param(&e, m1, "W"),
        "12",
        "Wp = vdd*2 in the bench scope, W = Wp*k in amp's"
    );
    assert_eq!(
        param(&e, m1, "ad"),
        "3",
        "computed in the child scope; the override is E030"
    );
    let c = codes(&e);
    assert!(
        c.contains(&"E030".to_owned()) && c.contains(&"W007".to_owned()),
        "{c:?}"
    );
    // Bench lines: vars after corner set, corner lines in file order, code by dialect.
    let vars: Vec<(&str, &str)> = e
        .bench
        .var_name
        .iter()
        .zip(&e.bench.var_text)
        .map(|(a, b)| (e.names.get(*a), e.names.get(*b)))
        .collect();
    assert_eq!(vars, [("vdd", "2")]);
    assert_eq!(e.bench.corner_kind, [CornerLine::Lib, CornerLine::Temp]);
    let args: Vec<&str> = e
        .bench
        .corner_args
        .iter()
        .map(|n| e.names.get(*n))
        .collect();
    assert_eq!(args, ["${PDK}/models.lib", "ff", "-40"]);
    let blocks: Vec<&str> = e.bench.code_name.iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(blocks, ["a1", "b2"]);
    // Default corner from the header; overrides win over the corner's set, in order.
    let e = run(None, &[("vdd", "2"), ("vdd", "2.5")], Dialect::Ngspice);
    let Target::Def(d) = e.insts.target[row(&e, e.top, "X1")] else {
        panic!()
    };
    assert_eq!(param(&e, row(&e, d, "M1"), "W"), "15");
    let args: Vec<&str> = e
        .bench
        .corner_args
        .iter()
        .map(|n| e.names.get(*n))
        .collect();
    assert_eq!(args, ["./tt.spice", "25"]);
    // Xyce: `.control` in a spice block is E033 and not emitted.
    let e = run(None, &[], Dialect::Xyce);
    let blocks: Vec<&str> = e.bench.code_name.iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(blocks, ["z9"]);
    assert!(codes(&e).contains(&"E033".to_owned()));
    assert!(codes(&run(Some("nope"), &[], Dialect::Ngspice)).contains(&"E035".to_owned()));
    // CDL takes no code blocks.
    assert!(run(None, &[], Dialect::Cdl).bench.code_name.is_empty());
}

#[test]
fn unresolved_cells_and_unopened_bodies() {
    let bench = "schemify 1 shbench\n\ninst 0x0001 ./ghost\n  at 0 0 R0\n  name X1\ninst 0x0002 nolib:res\n  at 0 0 R0\n  name X2\ninst 0x0003 ./amp\n  at 0 0 R0\n  name X3\n";
    // amp is indexed but not opened: its schematic is needed.
    let mut p = Project::new(None, None, 3).unwrap();
    let files = [
        ("res.shprim", RES),
        ("amp.shcomp", AMP),
        ("tb.shbench", bench),
    ];
    let listing: Vec<(&str, &[u8])> = files.iter().map(|(n, t)| (*n, t.as_bytes())).collect();
    p.index_library("", "", &listing).unwrap();
    let top = p
        .open("", "tb.shbench", bench.as_bytes(), None)
        .unwrap()
        .handle;
    let e = elab(&p, top);
    assert_eq!(codes(&e).iter().filter(|c| *c == "E001").count(), 2);
    assert_eq!(e.needs_open.len(), 1);
    assert_eq!(e.insts_of(e.top).len(), 0);
    p.open("", "amp.shcomp", AMP.as_bytes(), None).unwrap();
    let e = elab(&p, top);
    assert!(
        e.needs_open.len() == 1,
        "res is a leaf whose body may be needed"
    );
    assert_eq!(target(&e, row(&e, e.top, "X3")), "def:amp");
}

#[test]
fn plugin_functions_fall_back_to_cached_values() {
    let cell = "schemify 1 shcomp\n\npin P inout 0 0\n\nparam w 1\n\nimpl sch schematic\n\ninst 0m0001 ./res\n  at 0 0 R0\n  name R1\n  R k*1000\n\nport 0p0001 0 0 R0 P\n\nvar k gmid.width(w)\n  cached 2.5 abc123\nvar j gmid.width(w)\n";
    let (p, hs) = project(&[("res.shprim", RES), ("cell.shcomp", cell)]);
    let st = CellState::build(&p, hs[1], &mut no_funcs).unwrap();
    let g = &st.graph;
    let (k, j) = (g.params as usize, g.params as usize + 1);
    assert_eq!(g.vals[k], Val::Num(2.5));
    assert!(g.unverified[k], "cached stand-in is unverified");
    let r = g.inst_param(0, 0).unwrap() as usize;
    assert_eq!(g.vals[r], Val::Num(2500.0));
    assert!(g.unverified[r], "unverified propagates to dependents");
    assert!(g.vals[j].is_err(), "no cached value: X006");
    let e008: Vec<_> = st.violations.with(schemify_elab::Code::E008).collect();
    assert_eq!(e008.len(), 1);
    assert!(e008[0].detail.starts_with("X006"));
    // With the plugin loaded the value is computed and verified.
    let mut funcs = |name: &str, args: &[Value<'_>]| -> Option<Result<Value<'static>, _>> {
        (name == "gmid.width").then(|| match args[0] {
            Value::Int(i) => Ok(Value::Num(i as f64 * 4.0)),
            _ => Ok(Value::Num(0.0)),
        })
    };
    let st = CellState::build(&p, hs[1], &mut funcs).unwrap();
    assert_eq!(st.graph.vals[k], Val::Num(4.0));
    assert!(!st.graph.unverified[k] && !st.violations.blocking());
}

#[test]
fn expression_errors_and_cycles_are_e008() {
    let cell = "schemify 1 shcomp\n\npin P inout 0 0\n\nparam a b+1\nparam b a+1\nparam c 1/0\n\nimpl sch schematic\n\ninst 0m0001 ./res\n  at 0 0 R0\n  name R1\n  R nope\n\nport 0p0001 0 0 R0 P\n";
    let (p, hs) = project(&[("res.shprim", RES), ("cell.shcomp", cell)]);
    let st = CellState::build(&p, hs[1], &mut no_funcs).unwrap();
    let details: Vec<String> = st
        .violations
        .with(schemify_elab::Code::E008)
        .map(|v| v.detail.clone())
        .collect();
    assert_eq!(details.len(), 4, "{details:?}");
    assert_eq!(details.iter().filter(|d| d.starts_with("X005")).count(), 2);
    assert!(
        details.iter().any(|d| d.starts_with("X010"))
            && details.iter().any(|d| d.starts_with("X004"))
    );
}

/// Netlist spec §1: an instance of a cell that indexed but whose symbol does not parse is
/// `E001` (not a panic), from the top and below it, in the editor state and elaboration;
/// an unknown cell is `E001` with its own reason.
#[test]
fn an_unreadable_or_missing_cell_is_e001() {
    let bad = "schemify 1 shprim\n\npin A sideways 0 0\n";
    let mid = "schemify 1 shcomp\n\npin P inout 0 0\n\nimpl sch schematic\n\ninst 0m0001 ./bad\n  at 0 0 R0\n  name X1\ninst 0m0002 ./nope\n  at 0 40 R0\n  name X2\n\nport 0p0001 0 0 R0 P\n";
    let top = "schemify 1 shbench\n\ninst 0t0001 ./mid\n  at 0 0 R0\n  name X1\ninst 0t0002 ./bad\n  at 100 0 R0\n  name X2\n";
    let mut p = Project::new(None, None, 11).unwrap();
    let files = [
        ("bad.shprim", bad),
        ("mid.shcomp", mid),
        ("tb.shbench", top),
    ];
    let listing: Vec<(&str, &[u8])> = files.iter().map(|(n, t)| (*n, t.as_bytes())).collect();
    let (_, diags) = p.index_library("", "", &listing).unwrap();
    assert!(!diags.is_empty(), "bad.shprim does not index its symbol");
    p.open("", "mid.shcomp", mid.as_bytes(), None).unwrap();
    let h = p
        .open("", "tb.shbench", top.as_bytes(), None)
        .unwrap()
        .handle;
    let details = |v: &schemify_elab::Violations| -> Vec<String> {
        let mut d: Vec<String> = v
            .with(schemify_elab::Code::E001)
            .map(|v| v.detail.clone())
            .collect();
        d.sort();
        d
    };
    let unreadable = "`./bad` has no readable interface";
    let st = CellState::build(&p, h, &mut no_funcs).unwrap();
    assert_eq!(details(&st.violations), [unreadable]);
    let e = elaborate(&p, h, &common::ngspice(), &mut no_funcs);
    assert_eq!(
        details(&e.violations),
        [unreadable, unreadable, "cannot resolve `./nope`"]
    );
}

/// Expr spec §4: every expression on a cycle is `X005`, and no other. `a` lies on two
/// cycles (`a -> b -> a`, `a -> c -> a`) of one component; `d` only reads it.
#[test]
fn every_node_of_a_cyclic_component_is_x005() {
    let cell = "schemify 1 shcomp\n\npin P inout 0 0\n\nparam a b+c\nparam b a\nparam c a+1\nparam d a*2\nparam e 3\n\nimpl sch schematic\n\nport 0p0001 0 0 R0 P\n";
    let (p, hs) = project(&[("res.shprim", RES), ("cell.shcomp", cell)]);
    let st = CellState::build(&p, hs[1], &mut no_funcs).unwrap();
    let mut x005: Vec<&str> = st
        .violations
        .with(schemify_elab::Code::E008)
        .filter(|v| v.detail.starts_with("X005"))
        .map(|v| st.names.get(v.param))
        .collect();
    x005.sort_unstable();
    assert_eq!(x005, ["a", "b", "c"]);
    let g = &st.graph;
    assert!((0..3).all(|k| g.vals[k].is_err()), "cycle nodes are errors");
    assert_eq!(g.vals[3], Val::Upstream, "d waits upstream");
    assert_eq!(g.vals[4], Val::Int(3));
}

#[test]
fn gating_waivers_flip_only_the_named_param() {
    // Exhaustive small domain: L over a lattice × waiver on L (on/off), with W always
    // violating; a waiver on L turns only L's E005/E032 into I005.
    let dev = "schemify 1 shprim\n\npin D inout 0 0\n\nparam L 0.15\n  min 0.15\n  max 0.3\n  grid 0.05\nparam W 1\n  min 1\nparam model n\n  kind choice\n  choices n p\n\nimpl spice spice\n";
    for l in ["0.1", "0.15", "0.175", "0.2", "0.3", "0.35", "0.325"] {
        for waive in [false, true] {
            for model in ["n", "q"] {
                let w = if waive {
                    "\nwaive 0w0001 0m0001 L\n  reason \"char\"\n"
                } else {
                    ""
                };
                let bench = format!(
                    "schemify 1 shbench\n\ninst 0m0001 ./dev\n  at 0 0 R0\n  name M1\n  L {l}\n  W 0.5\n  model {model}\n{w}"
                );
                let (p, hs) = project(&[("dev.shprim", dev), ("tb.shbench", &bench)]);
                let st = CellState::build(&p, hs[1], &mut no_funcs).unwrap();
                let x: f64 = l.parse().unwrap();
                let mut want: Vec<(&str, &str)> = Vec::new();
                let out = !(0.15..=0.3).contains(&x);
                let off = ((x / 0.05) - (x / 0.05).round()).abs() > 1e-9;
                let lc = |c| if waive { "I005" } else { c };
                if out {
                    want.push((lc("E005"), "L"));
                }
                if off {
                    want.push((lc("E032"), "L"));
                }
                want.push(("E005", "W"));
                if model == "q" {
                    want.push(("E006", "model"));
                }
                let mut got: Vec<(&str, &str)> = st
                    .violations
                    .rows
                    .iter()
                    .filter(|v| !v.code.as_str().starts_with('W'))
                    .map(|v| (v.code.as_str(), st.names.get(v.param)))
                    .collect();
                got.sort_unstable();
                want.sort_unstable();
                // Two L findings of one code collapse into one row per key.
                want.dedup();
                assert_eq!(got, want, "L={l} waive={waive} model={model}");
            }
        }
    }
}
