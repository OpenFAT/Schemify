//! Impl selection precedence, instance rules, dialect support and unresolved or
//! unreadable cells.

use schemify_doc::Project;
use schemify_elab::{CellState, Dialect, Target, elaborate, no_funcs};

use super::{AMP, RES, bench_with, elab_cfg, row, target};
use crate::common::{codes, elab, ngspice};

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
    let e = elaborate(&p, h, &ngspice(), &mut no_funcs);
    assert_eq!(
        details(&e.violations),
        [unreadable, unreadable, "cannot resolve `./nope`"]
    );
}
