//! Connectivity (netlist spec §3–§5) against its oracles (TESTING.md `elab`
//! connectivity): the naive O(n²) reference on random schematics, metamorphic
//! transforms of the whole sheet (8 orientations, translation, record permutation,
//! re-id), and tables of naming, bus, bundle and iterated-instance cases.

mod common;

use common::*;
use proptest::prelude::*;
use schemify_doc::{DocHandle, Project};
use schemify_elab::{CellState, Code, NO_NET, no_funcs};

const PINS: [(i64, i64); 3] = [(0, 0), (20_000, 0), (0, 30_000)];

fn state(p: &Project, h: DocHandle) -> CellState {
    CellState::build(p, h, &mut no_funcs).unwrap()
}

fn bench_state(bench: &str, cells: &[(&str, &str)]) -> (Project, DocHandle, CellState) {
    let mut files: Vec<(&str, &str)> = cells.to_vec();
    files.push(("tb.shbench", bench));
    let (p, hs) = project(&files);
    let h = *hs.last().unwrap();
    let st = state(&p, h);
    (p, h, st)
}

fn check_against_oracle(bench: &str) {
    let (p, h, st) = bench_state(bench, &[("dev.shprim", DEV)]);
    let doc = p.doc(h).unwrap();
    let want = canon(&naive_partition(doc, &PINS));
    let got = canon(&real_partition(doc, &st.nets));
    assert_eq!(got, want, "bench:\n{bench}");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// The extractor's components equal the naive all-pairs union-find.
    #[test]
    fn components_equal_the_naive_reference(seed in any::<u64>(), n in 0usize..80) {
        check_against_oracle(&random_bench(&mut Mix(seed), n, seed % 1000 * 100));
    }
}

/// Rewrites a bench through a whole-sheet transform: `orient` (one of the 8) about the
/// origin, then `shift`; instance orientations compose with it. `perm` reverses the
/// record order; `reid` maps every id through `id(n + 7_777)`.
fn transform(bench: &str, orient: &str, shift: (i64, i64), reverse: bool, reid: bool) -> String {
    let (p, hs) = project(&[("dev.shprim", DEV), ("tb.shbench", bench)]);
    let doc = p.doc(hs[1]).unwrap();
    let s = doc.strings();
    let pt = |x: i32, y: i32| place_ref(x as i64, y as i64, orient, shift);
    let new_id = |old: String| -> String {
        if !reid {
            return old;
        }
        let n = (0..1_000_000u64).find(|&n| id(n) == old).expect("ids come from id()");
        id(n + 7_777)
    };
    let (m, t) = (orient.starts_with('M'), turns(orient));
    let mut recs: Vec<String> = Vec::new();
    let inst = doc.instances();
    for i in 0..inst.id.len() {
        let (x, y) = pt(inst.x[i], inst.y[i]);
        let o = inst.orient[i];
        // T ∘ O = R^(kt ± k) M^(mt ^ m): a mirror reverses the rotation it follows.
        let k = o.quarter_turns() as i64;
        let k2 = (t + if m { -k } else { k }).rem_euclid(4);
        let m2 = m ^ o.mirrored();
        let word = format!("{}R{}", if m2 { "M" } else { "" }, k2 * 90);
        recs.push(format!(
            "inst {} ./dev\n  at {} {} {word}\n  name {}\n",
            new_id(inst.id[i].text(s)),
            c(x),
            c(y),
            s.get(inst.name[i])
        ));
    }
    let w = doc.wires();
    for k in 0..w.id.len() {
        let ((x1, y1), (x2, y2)) = (pt(w.x1[k], w.y1[k]), pt(w.x2[k], w.y2[k]));
        recs.push(format!("wire {} {} {} {} {}\n", new_id(w.id[k].text(s)), c(x1), c(y1), c(x2), c(y2)));
    }
    let l = doc.labels();
    for k in 0..l.id.len() {
        let (x, y) = pt(l.x[k], l.y[k]);
        recs.push(format!("label {} {} {} R0 {}\n", new_id(l.id[k].text(s)), c(x), c(y), s.get(l.text[k])));
    }
    let n = doc.noconns();
    for k in 0..n.id.len() {
        let (x, y) = pt(n.x[k], n.y[k]);
        recs.push(format!("noconn {} {} {}\n", new_id(n.id[k].text(s)), c(x), c(y)));
    }
    if reverse {
        recs.reverse();
    }
    format!("schemify 1 shbench\n\n{}", recs.concat())
}

fn turns(orient: &str) -> i64 {
    match orient.trim_start_matches('M') {
        "R0" => 0,
        "R90" => 1,
        "R180" => 2,
        _ => 3,
    }
}

/// Partition of a bench keyed by object *index in the original id order*, so re-id'd
/// copies compare.
fn partition_by_id_order(bench: &str, reid: bool) -> std::collections::BTreeMap<String, String> {
    let (p, hs) = project(&[("dev.shprim", DEV), ("tb.shbench", bench)]);
    let st = state(&p, hs[1]);
    let doc = p.doc(hs[1]).unwrap();
    let part = real_partition(doc, &st.nets);
    let back = |key: &str| -> String {
        if !reid {
            return key.to_owned();
        }
        let mut parts: Vec<String> = key.split(':').map(str::to_owned).collect();
        let n = (0..1_000_000u64).find(|&n| id(n) == parts[1]).unwrap();
        parts[1] = id(n - 7_777);
        parts.join(":")
    };
    canon(&part.into_iter().map(|(k, v)| (back(&k), v)).collect())
}

#[test]
fn partitions_survive_every_sheet_orientation_shift_permutation_and_reid() {
    let mut rng = Mix(0x5eed);
    for round in 0..12 {
        let bench = random_bench(&mut rng, 70, 50_000 + round * 1000);
        let base = partition_by_id_order(&bench, false);
        for orient in ["R0", "R90", "R180", "R270", "MR0", "MR90", "MR180", "MR270"] {
            let shift = (rng.range(-50, 50) * 1000, rng.range(-50, 50) * 1000);
            let moved = transform(&bench, orient, shift, round % 2 == 0, false);
            assert_eq!(partition_by_id_order(&moved, false), base, "{orient} {shift:?}\n{bench}");
        }
        let reid = transform(&bench, "R0", (0, 0), true, true);
        assert_eq!(partition_by_id_order(&reid, true), base, "re-id");
    }
}

#[test]
fn the_orientation_transform_matches_the_spec_examples() {
    // Format spec §3: mirror x, then rotate clockwise on screen (y down), then translate.
    use schemify_elab::place;
    assert_eq!(place(10, 0, false, 1, 0, 0), (0, 10));
    assert_eq!(place(10, 0, false, 2, 0, 0), (-10, 0));
    assert_eq!(place(10, 0, false, 3, 0, 0), (0, -10));
    assert_eq!(place(10, 5, true, 0, 100, 100), (90, 105));
    assert_eq!(place(10, 5, true, 1, 0, 0), (-5, -10));
    for o in ["R0", "R90", "R180", "R270", "MR0", "MR90", "MR180", "MR270"] {
        let (m, t) = (o.starts_with('M'), turns(o) as u8);
        let got = place(7, 3, m, t, 11, 13);
        assert_eq!((got.0 as i64, got.1 as i64), place_ref(7, 3, o, (11, 13)), "{o}");
    }
    assert_eq!(place(i32::MAX, 0, false, 0, i32::MAX, 0).0, i32::MAX, "saturates");
}

// ---------------------------------------------------------------- tables of cases

/// (bench body records, expected groups of objects that must share a component,
/// pairs that must not).
fn assert_groups(body: &str, same: &[&[&str]], apart: &[(&str, &str)]) {
    let bench = format!("schemify 1 shbench\n\n{body}");
    check_against_oracle(&bench);
    let (p, hs) = project(&[("dev.shprim", DEV), ("tb.shbench", &bench)]);
    let st = state(&p, hs[1]);
    let part = real_partition(p.doc(hs[1]).unwrap(), &st.nets);
    for group in same {
        let nets: Vec<usize> = group.iter().map(|k| part[*k]).collect();
        assert!(nets.windows(2).all(|w| w[0] == w[1]), "{group:?} apart in\n{body}");
    }
    for (a, b) in apart {
        assert_ne!(part[*a], part[*b], "{a} {b} joined in\n{body}");
    }
}

#[test]
fn junction_rules() {
    // T-junction: an endpoint on another wire's interior connects.
    assert_groups(
        "wire 000001 0 0 100 0\nwire 000002 50 0 50 -40\n",
        &[&["w:000001", "w:000002"]],
        &[],
    );
    // Interior crossing does not.
    assert_groups("wire 000001 0 0 100 0\nwire 000002 50 -50 50 50\n", &[], &[("w:000001", "w:000002")]);
    // Labels and pins on interiors, and pin-to-pin abutment.
    assert_groups(
        "wire 000001 0 0 100 0\nlabel 000003 30 0 R0 a\ninst 000004 ./dev\n  at 60 0 R0\n  name X1\ninst 000005 ./dev\n  at 80 0 R0\n  name X2\n",
        &[&["w:000001", "l:000003", "p:000004:0", "p:000004:1", "p:000005:0"]],
        &[("p:000005:0", "p:000005:2")],
    );
    // Diagonal wires: lattice points on the segment connect, others do not.
    assert_groups(
        "wire 000001 0 0 100 100\nwire 000002 50 50 50 120\nwire 000003 50 49 0 49\n",
        &[&["w:000001", "w:000002"]],
        &[("w:000001", "w:000003")],
    );
    // Collinear overlap through an endpoint; a gap of one milli-unit stays apart.
    assert_groups(
        "wire 000001 0 0 100 0\nwire 000002 100 0 200 0\nwire 000003 200.001 0 300 0\n",
        &[&["w:000001", "w:000002"]],
        &[("w:000002", "w:000003")],
    );
}

fn inst_row(p: &Project, h: DocHandle, name: &str) -> usize {
    let doc = p.doc(h).unwrap();
    let s = doc.strings();
    (0..doc.instances().id.len()).find(|&i| s.get(doc.instances().name[i]) == name).unwrap()
}

/// Net names of every connection of instance `name`, element by element.
fn conns(p: &Project, h: DocHandle, st: &CellState, name: &str) -> Vec<Vec<String>> {
    let i = inst_row(p, h, name);
    let f = &st.ifaces[st.inst_iface[i] as usize];
    let bits = f.bits_total as usize;
    let start = st.nets.conn_start[i] as usize;
    (0..st.nets.elems(i).len())
        .map(|k| {
            st.nets.conns[start + k * bits..start + (k + 1) * bits]
                .iter()
                .map(|&b| if b == NO_NET { "-".to_owned() } else { st.names.get(st.nets.bit_name[b as usize]).to_owned() })
                .collect()
        })
        .collect()
}

fn codes_of(st: &CellState) -> Vec<&'static str> {
    st.violations.rows.iter().map(|v| v.code.as_str()).collect()
}

const ONE: &str = "schemify 1 shprim\n\npin P inout 0 0\n\nimpl spice spice\n";
const BUS: &str = "schemify 1 shprim\n\npin D[3:0] inout 0 0\npin Q inout 0 20\n\nimpl spice spice\n";

#[test]
fn naming_rules() {
    // Two labels on one net: E003, the bytewise-smallest label wins; names merge nets.
    let bench = "schemify 1 shbench\n\ninst 000001 ./one\n  at 0 0 R0\n  name X1\ninst 000002 ./one\n  at 100 0 R0\n  name X2\ninst 000003 ./one\n  at 200 0 R0\n  name X3\nlabel 000004 0 0 R0 zed\nlabel 000005 0 0 R0 abc\nlabel 000006 100 0 R0 zed\n";
    let (p, h, st) = bench_state(bench, &[("one.shprim", ONE)]);
    assert_eq!(conns(&p, h, &st, "X1"), [["abc"]]);
    assert_eq!(conns(&p, h, &st, "X2"), [["abc"]], "same name, other component: one net");
    let unnamed = &conns(&p, h, &st, "X3")[0][0];
    assert_eq!(unnamed, "net_000003", "unnamed nets take the smallest owning id");
    assert_eq!(codes_of(&st).iter().filter(|c| **c == "E003").count(), 1);
    assert!(codes_of(&st).contains(&"W004"), "X3's pin touches nothing");
    // Globals win over labels; `0` and `!` names are global.
    let bench = "schemify 1 shbench\n\ninst 000001 ./one\n  at 0 0 R0\n  name X1\nlabel 000004 0 0 R0 a\nlabel 000005 0 0 R0 vdd!\ninst 000002 ./one\n  at 50 0 R0\n  name X2\nlabel 000006 50 0 R0 0\n";
    let (p, h, st) = bench_state(bench, &[("one.shprim", ONE)]);
    assert_eq!(conns(&p, h, &st, "X1"), [["vdd!"]]);
    assert_eq!(conns(&p, h, &st, "X2"), [["0"]]);
    assert!(st.nets.bit_global.iter().filter(|g| **g).count() == 2);
}

#[test]
fn ports_take_precedence_and_bind_interface_pins() {
    let cell = "schemify 1 shcomp\n\npin IN in 0 0\npin OUT out 20 0\npin VDD inout 0 -20\n  inherit vdd!\n\nimpl sch schematic\n\ninst 000001 ./one\n  at 0 0 R0\n  name X1\ninst 000002 ./one\n  at 100 0 R0\n  name X2\nport 000003 0 0 R0 IN\nlabel 000004 0 0 R0 zz\nport 000005 100 0 R0 OUT\nport 000006 300 0 R0 NOPE\nlabel 000007 500 0 R0 VDD\n";
    let (p, hs) = project(&[("one.shprim", ONE), ("amp.shcomp", cell)]);
    let st = state(&p, hs[1]);
    assert_eq!(conns(&p, hs[1], &st, "X1"), [["IN"]], "port name beats a label");
    let codes = codes_of(&st);
    assert!(codes.contains(&"E003"));
    assert_eq!(codes.iter().filter(|c| **c == "E012").count(), 1, "NOPE names no pin; VDD is inherited");
    assert!(codes.contains(&"E002"));
    let port = |k: usize| -> Vec<String> { st.nets.port(k).iter().map(|&b| st.names.get(st.nets.bit_name[b as usize]).to_owned()).collect() };
    assert_eq!(port(0), ["IN"]);
    assert_eq!(port(1), ["OUT"]);
    assert_eq!(port(2), ["VDD"], "an inherited pin is the net of its own name");
}

#[test]
fn buses_bundles_repeats_and_iterated_instances() {
    let cases: &[(&str, &str, &[&[&str]], &[&str])] = &[
        // (records, instance, expected bits per element, expected codes)
        ("inst 000001 ./bus\n  at 0 0 R0\n  name X1\nlabel 000002 0 0 R0 d[3:0]\nlabel 000003 0 20 R0 q\n", "X1", &[&["d[3]", "d[2]", "d[1]", "d[0]", "q"]], &[]),
        ("inst 000001 ./bus\n  at 0 0 R0\n  name X1\nlabel 000002 0 0 R0 d[0:3]\nlabel 000003 0 20 R0 q\n", "X1", &[&["d[0]", "d[1]", "d[2]", "d[3]", "q"]], &[]),
        ("inst 000001 ./bus\n  at 0 0 R0\n  name X1\nlabel 000002 0 0 R0 a,b[1:0],c\nlabel 000003 0 20 R0 q\n", "X1", &[&["a", "b[1]", "b[0]", "c", "q"]], &[]),
        ("inst 000001 ./bus\n  at 0 0 R0\n  name X1\nlabel 000002 0 0 R0 2*s[1:0]\nlabel 000003 0 20 R0 q\n", "X1", &[&["s[1]", "s[0]", "s[1]", "s[0]", "q"]], &[]),
        // Iterated instance, 1-bit pin on a 4-bit net: element k takes slice k.
        ("inst 000001 ./one\n  at 0 0 R0\n  name X[3:0]\nlabel 000002 0 0 R0 d[3:0]\n", "X[3:0]", &[&["d[3]"], &["d[2]"], &["d[1]"], &["d[0]"]], &[]),
        // Broadcast of a scalar net.
        ("inst 000001 ./one\n  at 0 0 R0\n  name X[0:1]\nlabel 000002 0 0 R0 clk\n", "X[0:1]", &[&["clk"], &["clk"]], &[]),
        // Iterated bus pins: 2 elements × 4 bits on an 8-bit net.
        ("inst 000001 ./bus\n  at 0 0 R0\n  name Y[1:0]\nlabel 000002 0 0 R0 w[7:0]\nlabel 000003 0 20 R0 q\n", "Y[1:0]", &[&["w[7]", "w[6]", "w[5]", "w[4]", "q"], &["w[3]", "w[2]", "w[1]", "w[0]", "q"]], &[]),
        // Width mismatch: 4 elements × 1 bit on a 3-bit net.
        ("inst 000001 ./one\n  at 0 0 R0\n  name X[3:0]\nlabel 000002 0 0 R0 d[2:0]\n", "X[3:0]", &[&["-"], &["-"], &["-"], &["-"]], &["E011"]),
        // Names of different width on one net.
        ("inst 000001 ./one\n  at 0 0 R0\n  name X1\nlabel 000002 0 0 R0 a\nlabel 000003 0 0 R0 b[1:0]\n", "X1", &[&["a"]], &["E011"]),
        // Unnamed bus between two bus pins: width from the pins, net_<id>[k] names.
        ("inst 000001 ./bus\n  at 0 0 R0\n  name X1\ninst 000002 ./bus\n  at 0 0 R0\n  name X2\nlabel 000003 0 20 R0 q\n", "X2", &[&["net_000001[0]", "net_000001[1]", "net_000001[2]", "net_000001[3]", "q"]], &["E009"][..0]),
    ];
    for (records, inst, want, want_codes) in cases {
        let bench = format!("schemify 1 shbench\n\n{records}");
        let (p, h, st) = bench_state(&bench, &[("one.shprim", ONE), ("bus.shprim", BUS)]);
        let got = conns(&p, h, &st, inst);
        let want: Vec<Vec<String>> = want.iter().map(|e| e.iter().map(|s| (*s).to_owned()).collect()).collect();
        assert_eq!(got, want, "{records}");
        let codes = codes_of(&st);
        for c in *want_codes {
            assert!(codes.contains(c), "{c} missing for {records}: {codes:?}");
        }
        assert!(!codes.contains(&"E011") || want_codes.contains(&"E011"), "{records}: {codes:?}");
    }
}

#[test]
fn ignore_modes_short_open_and_true() {
    let bench = "schemify 1 shbench\n\ninst 000001 ./dev\n  at 0 0 R0\n  name S\n  ignore short\nlabel 000002 0 0 R0 a\nlabel 000003 20 0 R0 b\ninst 000004 ./dev\n  at 100 0 R0\n  name O\n  ignore open\nlabel 000005 100 0 R0 c\ninst 000006 ./dev\n  at 200 0 R0\n  name T\n  ignore true\nlabel 000007 200 0 R0 d\n";
    let (p, h, st) = bench_state(bench, &[("dev.shprim", DEV)]);
    let s = conns(&p, h, &st, "S");
    assert!(s[0].iter().all(|n| *n == s[0][0]), "short merges every pin net: {s:?}");
    assert_eq!(s[0][0], "a", "the merged net keeps the smallest label");
    assert_eq!(conns(&p, h, &st, "O"), [["-", "-", "-"]]);
    assert_eq!(conns(&p, h, &st, "T")[0][0], "d");
    assert!(st.nets.ignore.iter().any(|i| *i == schemify_elab::Ignore::True));
}

#[test]
fn duplicate_names_single_connections_and_domains() {
    let real = "schemify 1 shprim\n\npin R inout 0 0\n  domain real\n\nimpl spice spice\n";
    let bench = "schemify 1 shbench\n\ninst 000001 ./one\n  at 0 0 R0\n  name X[1:0]\ninst 000002 ./one\n  at 100 0 R0\n  name X[1]\nwire 000003 100 0 150 0\ninst 000004 ./real\n  at 0 0 R0\n  name R1\nlabel 000005 0 0 R0 n\n";
    let (_, _, st) = bench_state(bench, &[("one.shprim", ONE), ("real.shprim", real)]);
    let codes = codes_of(&st);
    assert!(codes.contains(&"E009"), "X[1] collides with X[1:0]: {codes:?}");
    assert!(codes.contains(&"W016"), "X[1] alone on its wire: {codes:?}");
    assert!(codes.contains(&"W010"), "real pin on an analog net: {codes:?}");
    assert!(!codes.contains(&"W004"));
    let e009: Vec<_> = st.violations.with(Code::E009).collect();
    assert_eq!(st.names.get(e009[0].param), "X[1]");
}
