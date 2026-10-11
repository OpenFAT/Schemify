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
