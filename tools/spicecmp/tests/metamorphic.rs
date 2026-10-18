//! Metamorphic and robustness tests for `compare` (docs/dev/testing.md `netlist` row).
//!
//! Circuits are generated from a seed (replayable: proptest prints it), then
//! rendered twice. Renderings that differ only in line order, subckt order,
//! instance names, internal net names, subckt names, number spelling and
//! letter case must compare equal; one moved connection or one changed value
//! must compare different.

use proptest::prelude::*;
use schemify_spicecmp::{Options, Section, compare};

/// splitmix64: tiny deterministic PRNG so a failing seed replays exactly.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, self.below(i + 1));
        }
    }
    fn perm(&mut self, n: usize) -> Vec<usize> {
        let mut p: Vec<usize> = (0..n).collect();
        self.shuffle(&mut p);
        p
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    R,
    C,
    M,
    D,
    V,
    X(usize),
}

#[derive(Clone, Debug)]
struct Dev {
    kind: Kind,
    nets: Vec<usize>,
    value: u32,
}

/// A cell's nets `0..ports` are its ports; top has no ports and net 0 is ground.
#[derive(Clone, Debug)]
struct Cell {
    ports: usize,
    nets: usize,
    devs: Vec<Dev>,
}

#[derive(Clone, Debug)]
struct Ckt {
    cells: Vec<Cell>,
    top: Cell,
}

fn arity(kind: Kind, cells: &[Cell]) -> usize {
    match kind {
        Kind::M => 4,
        Kind::X(j) => cells[j].ports,
        _ => 2,
    }
}

fn gen_devs(
    rng: &mut Rng,
    nets: usize,
    callable: std::ops::Range<usize>,
    cells: &[Cell],
    count: usize,
) -> Vec<Dev> {
    (0..count)
        .map(|_| {
            let mut kind = [Kind::R, Kind::C, Kind::M, Kind::D, Kind::V, Kind::X(0)][rng.below(6)];
            if kind == Kind::X(0) {
                kind = if callable.is_empty() {
                    Kind::R
                } else {
                    Kind::X(callable.start + rng.below(callable.len()))
                };
            }
            let n = arity(kind, cells);
            Dev {
                kind,
                nets: (0..n).map(|_| rng.below(nets)).collect(),
                value: 1 + rng.below(50) as u32,
            }
        })
        .collect()
}

/// Cells only call higher-numbered cells (no recursion); every cell is
/// called at least once so none becomes a root with name-anchored ports.
fn gen_ckt(seed: u64) -> Ckt {
    let mut rng = Rng(seed);
    let n_cells = rng.below(4);
    let mut cells = vec![
        Cell {
            ports: 0,
            nets: 0,
            devs: vec![]
        };
        n_cells
    ];
    // Build from the last cell back so callees exist when callers are made.
    for i in (0..n_cells).rev() {
        let ports = 1 + rng.below(4);
        let nets = ports + rng.below(4);
        let count = 1 + rng.below(6);
        let devs = gen_devs(&mut rng, nets, i + 1..n_cells, &cells, count);
        cells[i] = Cell { ports, nets, devs };
    }
    let nets = 2 + rng.below(8);
    let count = 1 + rng.below(8);
    let mut top = Cell {
        ports: 0,
        nets,
        devs: gen_devs(&mut rng, nets, 0..n_cells, &cells, count),
    };
    let called: Vec<bool> = (0..n_cells)
        .map(|j| {
            top.devs
                .iter()
                .chain(cells.iter().flat_map(|c| &c.devs))
                .any(|d| d.kind == Kind::X(j))
        })
        .collect();
    for (j, was_called) in called.iter().enumerate() {
        if !was_called {
            let nets = (0..cells[j].ports).map(|_| rng.below(top.nets)).collect();
            top.devs.push(Dev {
                kind: Kind::X(j),
                nets,
                value: 1,
            });
        }
    }
    Ckt { cells, top }
}

/// How to spell a circuit. `seed` drives every permutation.
#[derive(Clone, Copy)]
struct Style {
    seed: u64,
    rename: bool,
    shuffle: bool,
    upper: bool,
    numbers: usize,
}

const PLAIN: Style = Style {
    seed: 0,
    rename: false,
    shuffle: false,
    upper: false,
    numbers: 0,
};

fn render(ckt: &Ckt, style: Style) -> String {
    let mut rng = Rng(style.seed);
    let cell_perm = rng.perm(ckt.cells.len());
    let cell_name = |j: usize| {
        if style.rename {
            format!("blk{}x", cell_perm[j])
        } else {
            format!("s{j}")
        }
    };
    let mut out = String::from("* generated\n");
    let mut blocks: Vec<String> = Vec::new();
    for (j, cell) in ckt.cells.iter().enumerate() {
        let net_perm = rng.perm(cell.nets);
        let net = |n: usize| match (style.rename, n < cell.ports) {
            (false, true) => format!("p{n}"),
            (false, false) => format!("n{n}"),
            (true, true) => format!("pin_{}", net_perm[n]),
            (true, false) => format!("int{}_w", net_perm[n]),
        };
        let ports: Vec<String> = (0..cell.ports).map(net).collect();
        let mut lines = dev_lines(cell, &net, &cell_name, style, &mut rng);
        if style.shuffle {
            rng.shuffle(&mut lines);
        }
        blocks.push(format!(
            ".subckt {} {} w=1\n{}.ends {}\n",
            cell_name(j),
            ports.join(" "),
            lines.concat(),
            cell_name(j)
        ));
    }
    let top = &ckt.top;
    let net_perm = rng.perm(top.nets);
    let net = |n: usize| match (n, style.rename) {
        (0, _) => "0".to_string(),
        (_, false) => format!("t{n}"),
        (_, true) => format!("node{}", net_perm[n]),
    };
    let mut lines = dev_lines(top, &net, &cell_name, style, &mut rng);
    lines.push(".param vdd=1.8 w=2\n".to_string());
    lines.push(".lib /pdk/models.lib tt\n".to_string());
    lines.push(".tran 1n 10n\n".to_string());
    if style.shuffle {
        rng.shuffle(&mut blocks);
        lines.extend(blocks);
        rng.shuffle(&mut lines);
    } else {
        lines.extend(blocks);
    }
    out.push_str(&lines.concat());
    if style.upper {
        // Paths stay case-sensitive.
        out = out.lines().map(|l| if l.starts_with(".lib") { l.to_string() } else { l.to_uppercase() } + "\n").collect();
    }
    out.push_str(".control\nrun\nwrite out.raw\n.endc\n.end\n");
    out
}

fn dev_lines(
    cell: &Cell,
    net: &impl Fn(usize) -> String,
    cell_name: &impl Fn(usize) -> String,
    style: Style,
    rng: &mut Rng,
) -> Vec<String> {
    let inst_perm = rng.perm(cell.devs.len());
    cell.devs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let letter = match d.kind {
                Kind::R => 'R',
                Kind::C => 'C',
                Kind::M => 'M',
                Kind::D => 'D',
                Kind::V => 'V',
                Kind::X(_) => 'X',
            };
            let name = if style.rename {
                format!("{letter}q{}", inst_perm[i])
            } else {
                format!("{letter}{i}")
            };
            let nets: Vec<String> = d.nets.iter().map(|&n| net(n)).collect();
            let nets = nets.join(" ");
            let v = d.value;
            let body = match d.kind {
                Kind::R => match style.numbers % 3 {
                    0 => format!("{v}k"),
                    1 => format!("{}", v * 1000),
                    _ => format!("{v}e3"),
                },
                Kind::C => match style.numbers % 2 {
                    0 => format!("{v}p"),
                    _ => format!("{v}e-12 m=1"),
                },
                Kind::M => {
                    let model = if v % 2 == 0 { "nch" } else { "pch" };
                    match style.numbers % 2 {
                        0 => format!("{model} W='w*{v}u' L=0.15u"),
                        _ => format!("{model} l = 150n w={{{v}e-6 * w}}"),
                    }
                }
                Kind::D => format!("dmod area={v}"),
                Kind::V => format!("dc {v}"),
                Kind::X(j) => format!("{} w={}", cell_name(j), 1 + v % 3),
            };
            format!("{name} {nets} {body}\n")
        })
        .collect()
}

/// Moves one leaf-device terminal on a net with ≥ 2 leaf terminals in its
/// cell to a fresh net: every flat copy keeps the old net and gains one, so
/// the flat net count grows and no renaming can make it equal. (Subckt-call
/// terminals are not counted: an unused port carries no flat terminal.)
fn mutate_connection(ckt: &mut Ckt, pick: u64) -> bool {
    let mut rng = Rng(pick);
    let cell_count = ckt.cells.len() + 1;
    for attempt in 0..cell_count * 8 {
        let c = (rng.below(cell_count) + attempt) % cell_count;
        let cell = if c == ckt.cells.len() {
            &mut ckt.top
        } else {
            &mut ckt.cells[c]
        };
        let leaf = |d: &Dev| !matches!(d.kind, Kind::X(_));
        let mut degree = vec![0usize; cell.nets];
        for d in cell.devs.iter().filter(|d| leaf(d)) {
            for &n in &d.nets {
                degree[n] += 1;
            }
        }
        let terms: Vec<(usize, usize)> = cell
            .devs
            .iter()
            .enumerate()
            .filter(|(_, d)| leaf(d))
            .flat_map(|(i, d)| d.nets.iter().enumerate().map(move |(t, &n)| (i, t, n)))
            .filter(|&(_, _, n)| degree[n] >= 2)
            .map(|(i, t, _)| (i, t))
            .collect();
        if terms.is_empty() {
            continue;
        }
        let (i, t) = terms[rng.below(terms.len())];
        cell.devs[i].nets[t] = cell.nets;
        cell.nets += 1;
        return true;
    }
    false
}

/// Gives one non-X device a value no other device has.
fn mutate_value(ckt: &mut Ckt, pick: u64) -> bool {
    let mut rng = Rng(pick);
    let mut slots: Vec<(usize, usize)> = Vec::new();
    for (c, cell) in ckt
        .cells
        .iter()
        .chain(std::iter::once(&ckt.top))
        .enumerate()
    {
        for (i, d) in cell.devs.iter().enumerate() {
            if !matches!(d.kind, Kind::X(_)) {
                slots.push((c, i));
            }
        }
    }
    if slots.is_empty() {
        return false;
    }
    let (c, i) = slots[rng.below(slots.len())];
    let cell = if c == ckt.cells.len() {
        &mut ckt.top
    } else {
        &mut ckt.cells[c]
    };
    cell.devs[i].value = 977;
    true
}

proptest! {
    #[test]
    fn spelling_changes_compare_equal(seed in any::<u64>(), style_seed in any::<u64>(), numbers in 0usize..6, upper in any::<bool>()) {
        let ckt = gen_ckt(seed);
        let a = render(&ckt, PLAIN);
        let b = render(&ckt, Style { seed: style_seed, rename: true, shuffle: true, upper, numbers });
        let report = compare(&a, &b, &Options::default());
        prop_assert!(report.is_equal(), "{report}\n--- a\n{a}\n--- b\n{b}");
    }

    #[test]
    fn one_moved_connection_compares_different(seed in any::<u64>(), pick in any::<u64>(), style_seed in any::<u64>()) {
        let ckt = gen_ckt(seed);
        let mut mutated = ckt.clone();
        prop_assume!(mutate_connection(&mut mutated, pick));
        let a = render(&ckt, PLAIN);
        let b = render(&mutated, Style { seed: style_seed, rename: true, shuffle: true, upper: false, numbers: 1 });
        let report = compare(&a, &b, &Options::default());
        prop_assert!(!report.is_equal(), "--- a\n{a}\n--- b\n{b}");
        prop_assert!(report.diffs.iter().all(|d| d.section == Section::Connectivity), "{report}");
    }

    #[test]
    fn one_changed_value_compares_different(seed in any::<u64>(), pick in any::<u64>(), style_seed in any::<u64>()) {
        let ckt = gen_ckt(seed);
        let mut mutated = ckt.clone();
        prop_assume!(mutate_value(&mut mutated, pick));
        let a = render(&ckt, PLAIN);
        let b = render(&mutated, Style { seed: style_seed, rename: true, shuffle: true, upper: false, numbers: 0 });
        let report = compare(&a, &b, &Options::default());
        prop_assert!(!report.is_equal(), "--- a\n{a}\n--- b\n{b}");
    }

    /// Fuzz-ish: arbitrary text never panics, and comparison is reflexive and symmetric.
    #[test]
    fn arbitrary_text_is_reflexive_and_symmetric(a in ".{0,300}", b in ".{0,300}") {
        prop_assert!(compare(&a, &a, &Options::default()).is_equal());
        let ab = compare(&a, &b, &Options::default()).is_equal();
        let ba = compare(&b, &a, &Options::default()).is_equal();
        prop_assert_eq!(ab, ba);
    }

    /// Same, over lines built from SPICE fragments so the parser's deeper
    /// paths (subckt nesting, continuations, quotes, blocks) are reached.
    #[test]
    fn spice_shaped_text_is_reflexive_and_symmetric(
        a in prop::collection::vec(fragment_line(), 0..40),
        b in prop::collection::vec(fragment_line(), 0..40),
    ) {
        let (a, b) = (a.join("\n"), b.join("\n"));
        prop_assert!(compare(&a, &a, &Options::default()).is_equal(), "{a}");
        let ab = compare(&a, &b, &Options::default()).is_equal();
        let ba = compare(&b, &a, &Options::default()).is_equal();
        prop_assert_eq!(ab, ba);
    }
}

fn fragment_line() -> impl Strategy<Value = String> {
    let frag = prop::sample::select(vec![
        ".subckt", ".ends", ".param", ".include", ".lib", ".model", ".control", ".endc", ".end",
        ".global", "+", "*", "X1", "Xa", "M1", "R1", "C2", "V1", "E1", "B1", "A1", "Q1", "D1",
        "K1", "s1", "inv", "a", "b", "0", "gnd", "vdd!", "w=1u", "l = 2", "=", "'", "\"", "{", "}",
        "(", ")", "[", "]", "1k", "2meg", "-1e-3", "w*2", "$", ";", "/", "params:", "$SUB=x",
        "$[m]", "pulse(0", "1)", "é", "\t",
    ]);
    prop::collection::vec(frag, 0..10).prop_map(|v| v.join(" "))
}

#[test]
fn hand_cases() {
    let opts = Options::default();
    let base = "* t\nR1 a b 1k\nR2 b 0 2k\nC1 a 0 1p\n";
    // Same circuit, different spelling.
    let same = "* x\nc9 n1 0 1e-12\nR8 N2 0 2000\nrr n1 n2 1000\n";
    assert!(compare(base, same, &opts).is_equal());
    // Swapped terminals of R2 between existing nets: wiring differs.
    let swapped = "* t\nR1 a b 1k\nR2 a 0 2k\nC1 b 0 1p\n";
    let r = compare(base, swapped, &opts);
    assert!(!r.is_equal());
    assert!(r.diffs[0].text.contains("wiring differs"), "{r}");
    // A changed value is reported as a same-name signature change.
    let changed = "* t\nR1 a b 1k\nR2 b 0 3k\nC1 a 0 1p\n";
    let r = compare(base, changed, &opts);
    assert!(r.diffs.iter().any(|d| d.text.starts_with("~ r2")), "{r}");
    // m=1 is the default multiplier.
    assert!(compare("M1 d g s b n w=1\n", "M1 d g s b n w=1 m=1\n", &opts).is_equal());
    assert!(!compare("M1 d g s b n w=1\n", "M1 d g s b n w=1 m=2\n", &opts).is_equal());
    // Ground aliases; globals are anchored by name.
    assert!(compare("R1 a 0 1\n", "R1 a GND 1\n", &opts).is_equal());
    assert!(
        !compare(
            ".global vdd\nR1 vdd x 1\n",
            ".global vss\nR1 vss x 1\n",
            &opts
        )
        .is_equal()
    );
    // Root (CDL top) ports are anchored by name; internal nets are not.
    let cdl = ".subckt inv a y vdd vss\nM1 y a vss vss n\nM2 y a vdd vdd p\n.ends\n";
    let renamed_internal = ".SUBCKT INV A Y VDD VSS\nMx Y A VSS VSS N\nMy Y A VDD VDD P\n.ENDS\n";
    let renamed_port = ".subckt inv a z vdd vss\nM1 z a vss vss n\nM2 z a vdd vdd p\n.ends\n";
    assert!(compare(cdl, renamed_internal, &opts).is_equal());
    assert!(!compare(cdl, renamed_port, &opts).is_equal());
    // Hierarchical params evaluate through the call.
    let hier = ".subckt cell a b w=1\nR1 a b 'w*2'\n.ends\nX1 n 0 cell w=3\n";
    assert!(compare(hier, "R9 q 0 6\n", &opts).is_equal());
    assert!(!compare(hier, "R9 q 0 5\n", &opts).is_equal());
    // ngspice 45 evaluates a subckt's defaults after the call's overrides: `r='w*l'`
    // with `w=3` on the call is 6 (ngspice -b on this netlist: i(v1) = -1/6).
    let late = ".subckt foo a b w=1 l=2 r='w*l'\nR1 a b 'r'\n.ends\nx1 n1 0 foo w=3\n";
    assert!(compare(late, "R1 n1 0 6\n", &opts).is_equal());
    assert!(!compare(late, "R1 n1 0 2\n", &opts).is_equal());
    // ngspice ignores a bare word after a `.subckt` line's parameters (`delay=200 ps`
    // simulates as `delay=200`): no difference.
    let stray = ".subckt bar a b r=2 delay=200 ps\nR1 a b 'r'\n.ends\nx1 n1 0 bar\n";
    let clean = ".subckt bar a b r=2 delay=200\nR1 a b 'r'\n.ends\nx1 n1 0 bar\n";
    assert!(compare(stray, clean, &opts).is_equal());
    assert!(!compare(stray, "R1 n1 0 3\n", &opts).is_equal());
    // Text sections.
    let r = compare(
        ".lib /a/m.lib tt\n.control\nrun\n.endc\n",
        ".lib /a/m.lib ff\n.control\nop\n.endc\n",
        &opts,
    );
    let sections: Vec<Section> = r.diffs.iter().map(|d| d.section).collect();
    assert!(
        sections.contains(&Section::Includes) && sections.contains(&Section::Control),
        "{r}"
    );
    // Path aliases.
    let aliased = Options {
        path_aliases: vec![
            ("/home/ci".into(), "$R".into()),
            ("/tmp/x".into(), "$R".into()),
        ],
        ..Options::default()
    };
    assert!(
        compare(
            ".include /home/ci/m.lib\n",
            ".include /tmp/x/m.lib\n",
            &aliased
        )
        .is_equal()
    );
    assert!(
        !compare(
            ".include /home/ci/m.lib\n",
            ".include /tmp/x/m.lib\n",
            &opts
        )
        .is_equal()
    );
}

/// Equivalences added for the xschem differential (each with its negative):
/// `.save` vectors are a set and spacing inside `v( x )` is not significant;
/// parameter names inside unevaluable expressions read as their values in
/// scope; `bind_free` binds the reference's undefined names consistently.
#[test]
fn save_sets_expression_params_and_free_bindings() {
    let opts = Options::default();
    let eq = |a: &str, b: &str| compare(a, b, &opts).is_equal();
    // `.save`: ngspice saves the union of every `.save` line's vectors.
    assert!(eq(".save v( b1 )\n", ".save v(b1)\n"));
    assert!(eq(
        ".save v(a) v(b)\n",
        ".save v(b)\n.save v(a)\n.save v(a)\n"
    ));
    assert!(!eq(".save v(a)\n", ".save v(b)\n"));
    assert!(!eq(".save v(a) v(b)\n", ".save v(a)\n"));
    assert!(!eq(".save v(a,b)\n", ".save v(a)\n"));
    // Once connectivity matches, saved nets and devices are compared by
    // their place in the graph: unnamed nets, and saves in a subckt whose
    // instances are split over two specialized definitions.
    let named = "R1 a 0 1k\nR2 a b 2k\n.save v(a) i(r2)\n";
    assert!(eq(
        named,
        "R7 x 0 1k\nR8 x net_9 2k\n.save i(r8)\n.save v(x)\n"
    ));
    assert!(!eq(
        named,
        "R7 x 0 1k\nR8 x net_9 2k\n.save v(net_9) i(r8)\n"
    ));
    assert!(!eq(named, "R7 x 0 1k\nR8 x net_9 2k\n.save v(x) i(r7)\n"));
    let passed = ".subckt inv y a r=1\nR1 y a 'r'\nV1 a 0 0\n.save i(v1)\n.ends\nX1 n1 n2 inv\nX2 n3 n4 inv r=2\n";
    let split = ".subckt inv y a\nR1 y a 1\nV1 a 0 0\n.save i(v1)\n.ends\n.subckt inv__h y a\nR1 y a 2\nV1 a 0 0\n.save i(v1)\n.ends\nX1 n1 n2 inv\nX2 n3 n4 inv__h\n";
    assert!(eq(passed, split));
    assert!(!eq(passed, &split.replacen(".save i(v1)\n", "", 1)));
    // Control nodes of B/E/G sources are terminals: renaming an unnamed one
    // is fine, pointing it at another net is not.
    let b = "B1 o 0 v='v(a)*2'\nR1 a 0 1k\nR2 o 0 2k\n";
    assert!(eq(b, "B9 x 0 V = 'V( n ) * 2'\nR1 n 0 1k\nR2 x 0 2k\n"));
    assert!(!eq(b, "B9 x 0 v='v(x)*2'\nR1 n 0 1k\nR2 x 0 2k\n"));
    let e = "E1 o 0 pwl(1) a 0 0 0 1 1\nR1 a 0 1k\nR2 o 0 2k\n";
    assert!(eq(e, "E1 x 0 pwl(1) n 0 0 0 1 1\nR1 n 0 1k\nR2 x 0 2k\n"));
    assert!(!eq(e, "E1 x 0 pwl(1) x 0 0 0 1 1\nR1 n 0 1k\nR2 x 0 2k\n"));
    let gate = "E2 o 0 and(2) a 0 b 0 0 0 1 1\nR1 a 0 1k\nR2 b 0 2k\nR3 o 0 3k\n";
    assert!(eq(
        gate,
        "E2 o 0 and(2) n1 0 n2 0 0 0 1 1\nR1 n1 0 1k\nR2 n2 0 2k\nR3 o 0 3k\n"
    ));
    assert!(!eq(
        gate,
        "E2 o 0 and(2) n2 0 n1 0 0 0 1 1\nR1 n1 0 1k\nR2 n2 0 2k\nR3 o 0 3k\n"
    ));
    // A node named like a gate is still a node.
    assert!(!eq(
        "E1 o 0 orx 0 2\nR1 orx 0 1\n",
        "E1 o 0 0 orx 2\nR1 orx 0 1\n"
    ));
    // CDL multi-terminal C/D: extra bare names before the model are terminals.
    let var = "C1 a b n3 sub! cvar w=1\nR1 n3 0 1k\nR2 a 0 2k\n";
    assert!(eq(var, "C9 a b x7 sub! cvar w=1\nR1 x7 0 1k\nR2 a 0 2k\n"));
    assert!(!eq(var, "C9 a b a sub! cvar w=1\nR1 x7 0 1k\nR2 a 0 2k\n"));
    assert!(
        !eq("R1 a b rout\n", "R1 a b rmod\n"),
        "one trailing name is the model"
    );
    // Params in scope inside a behavioural expression (passed vs specialized).
    let passed_b = ".subckt d p m ron=0.1 roff=1e9\nB1 p m i='v(p,m)>0?v(p,m)/ron:v(p,m)/roff'\n.ends\nX1 a 0 d\n";
    let special = ".subckt d__x p m\nB1 p m I = 'v( p , m ) > 0 ? v(p,m) / 0.1 : v(p,m) / 1G'\n.ends\nX1 a 0 d__x\n";
    assert!(eq(passed_b, special));
    assert!(!eq(passed_b, &special.replace("/ 0.1", "/ 0.2")));
    // A node named like a param is a node, not the param.
    assert!(!eq(
        ".subckt d p ron ron=1\nB1 p 0 v=v(ron)\n.ends\nX1 a b d\n",
        ".subckt d p ron\nB1 p 0 v=v(1)\n.ends\nX1 a b d\n",
    ));
    // Free names of the reference: bound only with the option, consistently.
    let free = "M1 d g 0 0 n w=wp l=lp\nR1 d 0 'res/2'\n";
    let ours = "M1 d g 0 0 n w=1u l=0.15u\nR1 d 0 5k\n";
    assert!(!eq(free, ours));
    let bind = Options {
        bind_free: true,
        ..Options::default()
    };
    let r = compare(free, ours, &bind);
    assert!(r.is_equal(), "{r}");
    assert_eq!(r.bindings, ["lp=1.5e-7", "res=1e4", "wp=1e-6"]);
    // A free name passed down a hierarchy (`WP=WP_FB`) binds at the top.
    let passed_free = ".subckt inv a y wp=1u\nM1 y a 0 0 p w=wp\n.ends\nX1 n1 n2 inv wp=wp_fb\n";
    let r = compare(
        passed_free,
        ".subckt inv a y\nM1 y a 0 0 p w=2u\n.ends\nX1 n1 n2 inv\n",
        &bind,
    );
    assert!(r.is_equal(), "{r}");
    assert_eq!(r.bindings, ["wp_fb=2e-6"]);
    // One name, two values: no assignment makes them equal.
    let twice = "M1 d g 0 0 n w=wp\nM2 d g 0 0 n w=wp\n";
    let differ = "M1 d g 0 0 n w=1u\nM2 d g 0 0 n w=2u\n";
    assert!(!compare(twice, differ, &bind).is_equal());
    // A name the reference defines is never rebound.
    assert!(!compare(&format!(".param wp=2u\n{twice}"), differ, &bind).is_equal());
    // Wiring still counts, and the other side's `.param` binds and is excused.
    assert!(!compare(free, "M1 g d 0 0 n w=1u l=0.15u\nR1 d 0 5k\n", &bind).is_equal());
    let with_param = ".param wp=1u lp=0.15u res=10k\nM1 d g 0 0 n w=1u l=0.15u\nR1 d 0 5k\n";
    assert!(compare(free, with_param, &bind).is_equal());
    assert!(!compare(free, &with_param.replace("res=10k", "res=10k x=1"), &bind).is_equal());
}

/// Real-data metamorphic check over the xschem goldens: shuffling statements
/// and subckt order never changes the verdict. `SCHEMIFY_GOLDEN` names the goldens directory
/// that the interop plugin's corpus scripts write (github.com/OmarSiwy/Schemify-Interop,
/// `tests/corpus/golden.sh`: `tests/corpus/.cache/golden`).
#[test]
#[ignore = "needs SCHEMIFY_GOLDEN (Schemify-Interop: corpus/fetch.sh && corpus/golden.sh)"]
fn goldens_survive_shuffling() {
    let dir =
        std::path::PathBuf::from(std::env::var_os("SCHEMIFY_GOLDEN").expect("SCHEMIFY_GOLDEN set"));
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("golden dir exists") {
        let path = entry.expect("dir entry").path();
        let ext = path.extension().and_then(|e| e.to_str());
        if !matches!(ext, Some("spice" | "cdl")) {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("utf-8 golden");
        for seed in 0..3 {
            let shuffled = shuffle_statements(&text, seed);
            let r = compare(&text, &shuffled, &Options::default());
            assert!(r.is_equal(), "{}: seed {seed}\n{r}", path.display());
        }
        // Negative space: dropping one device line is always noticed.
        if let Some(dropped) = drop_first_device(&text) {
            let r = compare(&text, &dropped, &Options::default());
            assert!(
                !r.is_equal(),
                "{}: dropped device not detected",
                path.display()
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "no goldens in {}", dir.display());
}

/// Removes the first device statement (and its `+` lines) outside `.control`.
fn drop_first_device(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut in_control = false;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let head = t
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        in_control = (in_control || head == ".control") && head != ".endc";
        if head == ".end" {
            return None;
        }
        if in_control || !t.starts_with(|c: char| c.is_ascii_alphabetic()) {
            continue;
        }
        let end = i
            + 1
            + lines[i + 1..]
                .iter()
                .take_while(|l| l.trim_start().starts_with('+'))
                .count();
        let kept: Vec<&str> = lines[..i].iter().chain(&lines[end..]).copied().collect();
        return Some(kept.join("\n"));
    }
    None
}

/// Test-only statement shuffler: groups continuation lines, keeps
/// `.control` blocks whole, shuffles top-level items and every subckt body.
/// Comments and everything after `.end` are dropped.
fn shuffle_statements(text: &str, seed: u64) -> String {
    let mut stmts: Vec<String> = Vec::new();
    let mut in_control = false;
    for line in text.lines() {
        let t = line.trim_start();
        let head = t
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if !in_control && head == ".end" {
            break;
        }
        in_control = (in_control || head == ".control") && head != ".endc";
        if t.is_empty() || (t.starts_with('*') && !in_control) {
            continue;
        }
        match stmts.last_mut() {
            Some(last) if t.starts_with('+') && !in_control => {
                last.push('\n');
                last.push_str(line);
            }
            _ => stmts.push(line.to_string()),
        }
    }
    let mut rng = Rng(seed);
    let mut i = 0;
    let items = group(&stmts, &mut i, &mut rng);
    let mut out = String::from("* shuffled\n");
    for s in items {
        out.push_str(&s);
        out.push('\n');
    }
    out.push_str(".end\n");
    out
}

/// Items from `stmts[*i..]` up to (not including) a closing `.ends`, shuffled.
fn group(stmts: &[String], i: &mut usize, rng: &mut Rng) -> Vec<String> {
    let head = |s: &str| {
        s.split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    };
    let mut items = Vec::new();
    let mut unterminated = None; // swallows the rest of the file: stays last
    while *i < stmts.len() {
        let h = head(&stmts[*i]);
        if h == ".ends" {
            break;
        }
        let start = *i;
        *i += 1;
        match h.as_str() {
            ".control" => {
                while *i < stmts.len() && head(&stmts[*i - 1]) != ".endc" {
                    *i += 1;
                }
                let block = stmts[start..*i].join("\n");
                if head(&stmts[*i - 1]) == ".endc" {
                    items.push(block);
                } else {
                    unterminated = Some(block);
                }
            }
            ".subckt" => {
                let mut block = vec![stmts[start].clone()];
                block.extend(group(stmts, i, rng));
                if *i < stmts.len() {
                    block.push(stmts[*i].clone()); // .ends
                    *i += 1;
                }
                items.push(block.join("\n"));
            }
            _ => items.push(stmts[start].clone()),
        }
    }
    rng.shuffle(&mut items);
    items.extend(unterminated);
    items
}
