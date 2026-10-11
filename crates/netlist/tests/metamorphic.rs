//! Metamorphic and determinism oracles (TESTING.md, netlist row):
//! - hierarchical and flattened emission are equivalent under `schemify_spicecmp`;
//! - renaming an unlabeled net (re-id'ing its wire) changes only `net_<id>` names;
//! - output is byte-identical across runs, across input file order and across record
//!   order inside a file;
//! - the fast template path (bare identifiers) and the expression path (`{(x)}`)
//!   render identical lines for random templates.

mod common;

use common::*;
use proptest::prelude::*;
use schemify_elab::Dialect;
use schemify_netlist::{emit, emit_flat};

fn set(files: &[(String, String)], name: &str, text: String) -> Vec<(String, String)> {
    let mut out = files.to_vec();
    out.iter_mut().find(|f| f.0 == name).expect("fixture").1 = text;
    out
}

#[test]
fn hierarchical_and_flat_emission_are_equivalent() {
    let files = fixtures();
    for (bench, d) in [
        ("tb.shbench", Dialect::Ngspice),
        ("tb.shbench", Dialect::Xyce),
        ("tb.shbench", Dialect::Cdl),
        // Not dig_tb in ngspice: its XSPICE `[ … ]` vectors carry bus-bit nets (`d[0]`),
        // which ngspice's tokenizer splits (spec §7.6, open issue).
        ("mixed_tb.shbench", Dialect::Ngspice),
    ] {
        let (p, top) = fixture(&files, bench);
        let e = elab(&p, top, d);
        let (hier, flat) = (emit(&e, d).text, emit_flat(&e, d));
        assert!(!flat.contains(".subckt chain"), "flattened");
        let r = schemify_spicecmp::compare(&hier, &flat, &Default::default());
        assert!(r.is_equal(), "{bench} {d:?}: {:#?}\n{flat}", r.diffs);
        // Negative space: the oracle notices one moved connection.
        let broken = flat.replacen("VVIN in 0", "VVIN out 0", 1);
        assert_ne!(broken, flat, "{bench}: the probe line exists");
        let r = schemify_spicecmp::compare(&hier, &broken, &Default::default());
        assert!(!r.is_equal(), "{bench}: a moved connection went unnoticed");
    }
}

#[test]
fn renaming_an_unlabeled_net_keeps_labeled_names() {
    let files = fixtures();
    let chain = &files.iter().find(|f| f.0 == "chain.shcomp").unwrap().1;
    let (p, top) = fixture(&files, "tb.shbench");
    let before = emit(&elab(&p, top, Dialect::Ngspice), Dialect::Ngspice).text;
    assert!(before.contains("net_0w0001"));
    for new_id in ["zzzzzz", "000001", "0w0002", "k9m3x2"] {
        let renamed = set(&files, "chain.shcomp", chain.replace("0w0001", new_id));
        let (p, top) = fixture(&renamed, "tb.shbench");
        let after = emit(&elab(&p, top, Dialect::Ngspice), Dialect::Ngspice).text;
        assert_eq!(
            after.replace(&format!("net_{new_id}"), "net_0w0001"),
            before,
            "{new_id}"
        );
        let r = schemify_spicecmp::compare(&before, &after, &Default::default());
        assert!(r.is_equal(), "{new_id}: {:#?}", r.diffs);
    }
}

#[test]
fn output_is_byte_identical_across_runs_and_input_orders() {
    let files = fixtures();
    let mut reversed = files.clone();
    reversed.reverse();
    // Rank-3 records in reverse order inside the bench (the reader accepts any order).
    let tb = &files.iter().find(|f| f.0 == "tb.shbench").unwrap().1;
    let (head, rest) = tb.split_at(tb.find("\ninst ").unwrap() + 1);
    let end = rest.find("\nvar ").unwrap() + 1;
    let mut blocks: Vec<String> = Vec::new();
    for line in rest[..end].lines().filter(|l| !l.is_empty()) {
        if line.starts_with(' ') {
            let last = blocks.last_mut().unwrap();
            last.push_str(line);
            last.push('\n');
        } else {
            blocks.push(format!("{line}\n"));
        }
    }
    blocks.reverse();
    let shuffled_tb = format!("{head}{}\n{}", blocks.concat(), &rest[end..]);
    let shuffled = set(&files, "tb.shbench", shuffled_tb);
    for d in [
        Dialect::Ngspice,
        Dialect::Xyce,
        Dialect::Cdl,
        Dialect::Spectre,
    ] {
        let run = |files: &[(String, String)]| {
            let (p, top) = fixture(files, "tb.shbench");
            emit(&elab(&p, top, d), d).text
        };
        let first = run(&files);
        assert_eq!(first, run(&files), "{d:?}: two runs");
        assert_eq!(first, run(&reversed), "{d:?}: reversed file order");
        assert_eq!(first, run(&shuffled), "{d:?}: reversed record order");
    }
}

/// Template holes for the differential: identifiers the `nmos` fixture defines.
const IDENTS: [&str; 10] = [
    "name", "D", "G", "S", "B", "W", "L", "model", "pins", "cell",
];

fn render_with(template: &str) -> String {
    let files = fixtures();
    let nmos = &files.iter().find(|f| f.0 == "nmos.shprim").unwrap().1;
    let quoted = template.replace('\\', "\\\\").replace('"', "\\\"");
    let at = nmos.find("  format spice ").unwrap();
    let eol = at + nmos[at..].find('\n').unwrap();
    let text = format!("{}  format spice \"{quoted}\"{}", &nmos[..at], &nmos[eol..]);
    let (p, top) = fixture(&set(&files, "nmos.shprim", text), "tb.shbench");
    emit(&elab(&p, top, Dialect::Ngspice), Dialect::Ngspice).text
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn fast_and_expression_template_paths_agree(
        parts in prop::collection::vec((0usize..IDENTS.len(), "[a-z=( ]{0,4}"), 1..6),
    ) {
        let mut bare = String::from("M{name}");
        let mut wrapped = String::from("M{(name)}");
        for (k, lit) in &parts {
            bare.push_str(&format!("{lit} {{{}}}", IDENTS[*k]));
            wrapped.push_str(&format!("{lit} {{({})}}", IDENTS[*k]));
        }
        prop_assert_eq!(render_with(&bare), render_with(&wrapped));
    }
}

/// D37 `jobs = 1` vs `N`: a design large enough that elaboration (connectivity beside the
/// graph, element names beside the geometry) and emission (rows in chunks) use threads
/// gives the same text, name map and violations as the forced-serial run, in every
/// dialect. The design mixes what those threads touch: abutting chains, labelled and
/// unnamed nets, a bus label, an iterated instance, a duplicate name (E009), an
/// off-limit value (E005), floating pins and a failing override (E008).
#[test]
fn threads_change_nothing() {
    let res = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nparam R 1k\n  min 1\n\nimpl spice spice\n  format spice \"R{name} {A} {B} {R}\"\n";
    let n = 40_000;
    let mut s = String::from("schemify 1 shbench\n\n");
    for i in 0..n {
        let name = if i == 7 {
            "R6".to_owned()
        } else {
            format!("R{i}")
        };
        let value = match i % 1000 {
            3 => "0".to_owned(),
            5 => "nope*2".to_owned(),
            _ => format!("{}", i % 7 + 1),
        };
        let y = (i % 1000) as i64 * 30_000 - if i % 3 == 0 { 0 } else { 10_000 };
        s.push_str(&format!(
            "inst {} ./res\n  at {} {} R0\n  name {name}\n  R {value}\n",
            id(i as u64 + 1),
            c((i / 1000) as i64 * 100_000),
            c(y)
        ));
    }
    s.push_str(&format!(
        "inst {} ./res\n  at 9000 0 R0\n  name RB[3:0]\nlabel {} 9000 0 R0 d[3:0]\nlabel {} 0 0 R0 in\nlabel {} 100 0 R0 0\n",
        id(1 << 26),
        id((1 << 26) + 1),
        id((1 << 26) + 2),
        id((1 << 26) + 3)
    ));
    let (p, hs) = project(&[("res.shprim", res), ("tb.shbench", &s)]);
    for d in [
        Dialect::Ngspice,
        Dialect::Cdl,
        Dialect::Spectre,
        Dialect::Verilog,
    ] {
        let run = |serial: bool| {
            schemify_elab::test_support::set_serial(serial);
            schemify_netlist::set_serial(serial);
            let e = elab(&p, hs[1], d);
            let codes: Vec<String> = e
                .violations
                .rows
                .iter()
                .map(|v| format!("{} {}", v.code.as_str(), v.detail))
                .collect();
            let out = emit(&e, d);
            let map = (
                out.name_map.inst_name("/R12345"),
                out.name_map.inst_path("r12345"),
            );
            (out.text, codes, map)
        };
        let serial = run(true);
        let threaded = run(false);
        for code in ["E009", "E005", "E008"] {
            assert!(serial.1.iter().any(|c| c.starts_with(code)), "{d:?} {code}");
        }
        assert_eq!(serial.0.len(), threaded.0.len(), "{d:?}");
        assert!(serial.0 == threaded.0, "{d:?}: texts differ");
        assert_eq!(serial.1, threaded.1, "{d:?}");
        assert_eq!(serial.2, threaded.2, "{d:?}");
    }
    schemify_elab::test_support::set_serial(false);
    schemify_netlist::set_serial(false);
}
