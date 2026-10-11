//! D39 budgets for elab (nightly tier):
//! - elaboration of a hierarchy that flattens to 2^20 leaves ≤ 0.5 s (so emission fits
//!   the 1 s netlist budget);
//! - a param edit with 1k dependents recomputes in ≤ 8 ms;
//! - connectivity of a 100k-object cell (reported; the editor rebuilds it per geometry
//!   edit).
//!
//! Run: `cargo test -p schemify-elab --release --test perf -- --ignored --nocapture`.

mod common;

use common::*;
use schemify_doc::{Edit, EditRequest, Mode, Op, Outcome, Target, Transaction, apply_edits};
use schemify_elab::test_support::{Names, iface};
use schemify_elab::{CellState, connect, elaborate, no_funcs};
use schemify_format::RecordKind;
use std::time::{Duration, Instant};

const RES: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nparam R 1k\n  min 1\n\nimpl spice spice\n  format spice \"R{name} {A} {B} {R}\"\n";

/// A level-`k` cell: `fan` instances of `child` chained between ports A and B
/// (`A - n1 - n2 - … - B`), each passing a param down.
fn level(child: &str, fan: usize, param_child: &str) -> String {
    let mut s = String::from(
        "schemify 1 shcomp\n\npin A inout 0 0\npin B inout 0 20\n\nparam r 1k\n\nimpl sch schematic\n\n",
    );
    for i in 0..fan {
        s.push_str(&format!(
            "inst {} ./{child}\n  at {} 0 R0\n  name X{i}\n  {param_child} r*2\n",
            id(i as u64 + 1),
            c(i as i64 * 100_000)
        ));
    }
    for i in 0..fan {
        let a = if i == 0 {
            "A".to_owned()
        } else {
            format!("n{i}")
        };
        let b = if i + 1 == fan {
            "B".to_owned()
        } else {
            format!("n{}", i + 1)
        };
        s.push_str(&format!(
            "label {} {} 0 R0 {a}\n",
            id(1000 + 2 * i as u64),
            c(i as i64 * 100_000)
        ));
        s.push_str(&format!(
            "label {} {} 20 R0 {b}\n",
            id(1001 + 2 * i as u64),
            c(i as i64 * 100_000)
        ));
    }
    s.push_str(&format!(
        "port {} 0 0 R0 A\nport {} {} 20 R0 B\n",
        id(5000),
        id(5001),
        c((fan as i64 - 1) * 100_000)
    ));
    s
}

#[test]
#[ignore]
fn elaborate_a_hierarchy_of_a_million_leaves() {
    let fan = 32;
    let l1 = level("res", fan, "R");
    let l2 = level("l1", fan, "r");
    let l3 = level("l2", fan, "r");
    let l4 = level("l3", fan, "r");
    let bench = "schemify 1 shbench\n\ninst 0x0001 ./l4\n  at 0 0 R0\n  name X\n\nlabel 0n0001 0 0 R0 in\nlabel 0n0002 0 20 R0 0\n";
    let (p, hs) = project(&[
        ("res.shprim", RES),
        ("l1.shcomp", &l1),
        ("l2.shcomp", &l2),
        ("l3.shcomp", &l3),
        ("l4.shcomp", &l4),
        ("tb.shbench", bench),
    ]);
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        let e = elaborate(&p, hs[5], &ngspice(), &mut no_funcs);
        best = best.min(t.elapsed());
        assert_eq!(e.flat_leaves(e.top), (fan as u64).pow(4));
        assert!(!e.violations.blocking(), "{:?}", codes(&e));
        assert_eq!(
            e.defs.cell.len(),
            5,
            "one definition per level plus the top"
        );
    }
    eprintln!(
        "elaborate 4 levels × {fan} = {} leaves: {best:?}",
        (fan as u64).pow(4)
    );
    eprintln!(
        "PERF elab.hier_1m_leaves_ms {:.3}",
        best.as_secs_f64() * 1e3
    );
    assert!(best < Duration::from_millis(500), "D39: {best:?}");
}

#[test]
#[ignore]
fn elaborate_a_flat_bench_of_100k_instances() {
    let n = 100_000;
    let mut s = String::from("schemify 1 shbench\n\n");
    for i in 0..n {
        // A chain: R_i's B pin abuts R_{i+1}'s A pin.
        s.push_str(&format!(
            "inst {} ./res\n  at 0 {} R0\n  name R{i}\n  R {}\n",
            id(i as u64 + 1),
            c(i as i64 * 20_000),
            i % 7 + 1
        ));
    }
    let (p, hs) = project(&[("res.shprim", RES), ("tb.shbench", &s)]);
    let t = Instant::now();
    let e = elaborate(&p, hs[1], &ngspice(), &mut no_funcs);
    let dt = t.elapsed();
    assert_eq!(e.flat_leaves(e.top), n as u64);
    eprintln!("elaborate a flat 100k-instance bench: {dt:?} (×10 ≈ 1M flat)");
    eprintln!("PERF elab.flat_100k_ms {:.3}", dt.as_secs_f64() * 1e3);
}

#[test]
#[ignore]
fn a_param_edit_with_1k_dependents_recomputes_within_8ms() {
    // 10k instances; 1k of them read var `a` (W = a*2), whose edit is the measured one.
    let dev = "schemify 1 shprim\n\npin D inout 0 0\n\nparam W 1\n  min 0.1\n  grid 0.01\nparam L W*2\nparam ad 0\n  computed W*L\n\nimpl spice spice\n";
    let mut s = String::from("schemify 1 shcomp\n\npin P inout 0 0\n\nimpl sch schematic\n\n");
    for i in 0..10_000 {
        let w = if i % 10 == 0 {
            "a*2".to_owned()
        } else {
            format!("{}", i % 5 + 1)
        };
        s.push_str(&format!(
            "inst {} ./dev\n  at {} 0 R0\n  name M{i}\n  W {w}\n",
            id(i as u64 + 1),
            c(i as i64 * 10_000)
        ));
    }
    s.push_str(&format!("\nport {} 0 0 R0 P\n\nvar a 1\n", id(99_999)));
    let (mut p, hs) = project(&[("dev.shprim", dev), ("cell.shcomp", &s)]);
    let h = hs[1];
    let t = Instant::now();
    let mut st = CellState::build(&p, h, &mut no_funcs).unwrap();
    let build = t.elapsed();
    let mut best = Duration::MAX;
    for k in 0..10 {
        let op = Op::SetField {
            target: Target::new(RecordKind::Var, "a"),
            index: 1,
            value: format!("{}", k + 2),
        };
        let tx = Transaction {
            label: "a".into(),
            mode: Mode::Commit,
            ops: vec![op.clone()],
        };
        let out = apply_edits(
            &mut p,
            &[EditRequest {
                doc: h,
                edit: Edit::Tx(tx),
                unix_ms: 0,
            }],
        );
        assert!(matches!(out[0], Outcome::Committed { .. }));
        let t = Instant::now();
        let done = st.recompute(&p, h, &[op], &mut no_funcs);
        best = best.min(t.elapsed());
        assert_eq!(
            done.evaluated,
            1 + 1000 * 3,
            "a, then W, L, ad of each dependent"
        );
    }
    eprintln!("1k dependents of 30k nodes: recompute + checks {best:?} (full build {build:?})");
    eprintln!(
        "PERF elab.recompute_1k_dependents_ms {:.3}",
        best.as_secs_f64() * 1e3
    );
    assert!(best < Duration::from_millis(8), "D39: {best:?}");
}

#[test]
#[ignore]
fn connectivity_of_a_100k_object_cell() {
    // The doc perf mix: 35% instances, 45% wires, 12% labels, 8% noconns, on a grid
    // dense enough for many junctions.
    let n = 100_000;
    let mut rng = Mix(42);
    let side = ((n as f64).sqrt() * 15.0) as i64;
    let g = |rng: &mut Mix| (rng.below(side as u64) as i64 - side / 2) * 10_000;
    let mut s = String::from("schemify 1 shbench\n\n");
    for i in 0..n {
        let k = id(i as u64 + 1);
        match i * 100 / n {
            0..35 => s.push_str(&format!(
                "inst {k} ./dev\n  at {} {} R90\n  name X{i}\n",
                c(g(&mut rng)),
                c(g(&mut rng))
            )),
            35..80 => {
                let (x, y) = (g(&mut rng), g(&mut rng));
                let len = (rng.below(8) as i64 + 1) * 10_000;
                let (x2, y2) = if rng.chance(50) {
                    (x + len, y)
                } else {
                    (x, y + len)
                };
                s.push_str(&format!("wire {k} {} {} {} {}\n", c(x), c(y), c(x2), c(y2)));
            }
            80..92 => s.push_str(&format!(
                "label {k} {} {} R0 n{}\n",
                c(g(&mut rng)),
                c(g(&mut rng)),
                rng.below(5000)
            )),
            _ => s.push_str(&format!(
                "noconn {k} {} {}\n",
                c(g(&mut rng)),
                c(g(&mut rng))
            )),
        }
    }
    let (p, hs) = project(&[("dev.shprim", DEV), ("big.shbench", &s)]);
    let doc = p.doc(hs[1]).unwrap();
    let dev_doc = p.doc(hs[0]).unwrap();
    let mut best = Duration::MAX;
    let mut nets = 0;
    for _ in 0..5 {
        let mut names = Names::default();
        let own = iface(doc, "big", &mut names);
        let dev = iface(dev_doc, "dev", &mut names);
        let ifaces = [own, dev];
        let inst_iface = vec![1u32; doc.instances().id.len()];
        let mut found = Vec::new();
        let t = Instant::now();
        let cn = connect(
            doc,
            &ifaces[0],
            &ifaces,
            &inst_iface,
            &[],
            &mut names,
            &mut found,
        );
        best = best.min(t.elapsed());
        nets = cn.net_name.len();
    }
    eprintln!("connect 100k objects ({nets} nets): {best:?}");
    eprintln!("PERF elab.connect_100k_ms {:.3}", best.as_secs_f64() * 1e3);
    assert!(best < Duration::from_millis(200), "{best:?}");
}

/// A schematic of `tiles` tiles of 10 objects on a 200-unit pitch: a three-pin device
/// (`common::DEV`), six wires (pin stubs, an unnamed drop, a ground bar, a supply stub), a
/// tile-local net label `in<t>`, `gnd` and `vdd!` labels shared by every tile (one named
/// bit each across the sheet), and a noconn. Tile `t`'s objects have ids `id(t * 16 + k)`.
fn tiles(tiles: u64) -> String {
    let side = (tiles as f64).sqrt().ceil() as u64;
    let mut s = String::with_capacity(tiles as usize * 400);
    s.push_str("schemify 1 shcomp\n\npin P inout 0 0\n\nimpl sch schematic\n\n");
    for t in 0..tiles {
        let (x, y) = ((t % side) as i64 * 200_000, (t / side) as i64 * 200_000);
        let k = |n: u64| id(t * 16 + n);
        s.push_str(&format!(
            "inst {} ./dev\n  at {} {} R0\n  name X{t}\n",
            k(0),
            c(x),
            c(y)
        ));
        let wires = [
            (x - 40_000, y, x, y),
            (x + 20_000, y, x + 60_000, y),
            (x + 60_000, y, x + 60_000, y + 40_000),
            (x, y + 30_000, x, y + 60_000),
            (x - 40_000, y + 60_000, x + 60_000, y + 60_000),
            (x - 40_000, y + 90_000, x + 20_000, y + 90_000),
        ];
        for (n, w) in wires.iter().enumerate() {
            s.push_str(&format!(
                "wire {} {} {} {} {}\n",
                k(1 + n as u64),
                c(w.0),
                c(w.1),
                c(w.2),
                c(w.3)
            ));
        }
        s.push_str(&format!(
            "label {} {} {} R0 in{t}\nlabel {} {} {} R0 gnd\nlabel {} {} {} R0 vdd!\nnoconn {} {} {}\n",
            k(7),
            c(x - 40_000),
            c(y),
            k(8),
            c(x - 40_000),
            c(y + 60_000),
            k(9),
            c(x - 40_000),
            c(y + 90_000),
            k(10),
            c(x + 100_000),
            c(y)
        ));
    }
    s.push_str(&format!("port {} 0 -100 R0 P\n", id(1 << 28)));
    s
}

/// The ops of one random local edit on tile `t` of `tiles(n)`: delete a wire, join two
/// tiles' nets with a new wire, rename the tile's label to its neighbour's (a merge by
/// name), move the device, or move a wire end.
fn local_edit(rng: &mut Mix, t: u64, side: u64) -> Vec<Op> {
    let (x, y) = ((t % side) as i64 * 200_000, (t / side) as i64 * 200_000);
    let k = |n: u64| id(t * 16 + n);
    match rng.below(5) {
        0 => vec![Op::Delete(Target::new(
            RecordKind::Wire,
            &k(1 + rng.below(6)),
        ))],
        1 => vec![Op::Insert {
            rec: schemify_doc::RecordBuf::new(RecordKind::Wire)
                .field("-")
                .field(&c(x + 60_000))
                .field(&c(y))
                .field(&c(x + 160_000))
                .field(&c(y)),
            at: None,
        }],
        2 => vec![Op::SetField {
            target: Target::new(RecordKind::Label, &k(7)),
            index: 4,
            value: format!("in{}", t + 1),
        }],
        3 => vec![Op::SetAttr {
            target: Target::new(RecordKind::Inst, &k(0)),
            key: "at".into(),
            values: vec![c(x + 10_000), c(y), "R0".into()],
        }],
        _ => vec![Op::SetField {
            target: Target::new(RecordKind::Wire, &k(2)),
            index: 3,
            value: c(x + 80_000),
        }],
    }
}

/// p50 / p95 / p99 of `samples` (sorted in place), in ms.
fn percentiles(samples: &mut [Duration]) -> (f64, f64, f64) {
    samples.sort_unstable();
    let at = |q: f64| {
        let i = ((samples.len() as f64 - 1.0) * q).round() as usize;
        samples[i].as_secs_f64() * 1e3
    };
    (at(0.5), at(0.95), at(0.99))
}

/// The elab cost of one local edit, `CellState::recompute` (connectivity patch + checks
/// delta), at `objects` objects: 200 random edits on random tiles, each followed by its
/// undo. Returns (p50, p95, p99) in ms.
fn edit_costs(objects: u64) -> (f64, f64, f64) {
    let n = objects / 10;
    let side = (n as f64).sqrt().ceil() as u64;
    let (mut p, hs) = project(&[("dev.shprim", DEV), ("big.shcomp", &tiles(n))]);
    let h = hs[1];
    let t0 = Instant::now();
    let mut st = CellState::build(&p, h, &mut no_funcs).unwrap();
    let build = t0.elapsed();
    let mut rng = Mix(7);
    let mut samples = Vec::new();
    let mut patched = 0;
    let mut step = |p: &mut schemify_doc::Project, edit: Edit, ops: &[Op], st: &mut CellState| {
        let out = apply_edits(
            p,
            &[EditRequest {
                doc: h,
                edit,
                unix_ms: 0,
            }],
        );
        if !matches!(
            out[0],
            Outcome::Committed { .. } | Outcome::Undone { .. } | Outcome::Redone { .. }
        ) {
            return;
        }
        let t = Instant::now();
        let done = st.recompute(p, h, ops, &mut no_funcs);
        samples.push(t.elapsed());
        patched += usize::from(done.patched);
    };
    for _ in 0..100 {
        let t = rng.below(n);
        let ops = local_edit(&mut rng, t, side);
        let tx = Transaction {
            label: "e".into(),
            mode: Mode::Commit,
            ops: ops.clone(),
        };
        step(&mut p, Edit::Tx(tx), &ops, &mut st);
        step(&mut p, Edit::Undo, &ops, &mut st);
    }
    assert!(
        patched * 10 >= samples.len() * 9,
        "{patched} of {} patched",
        samples.len()
    );
    let (p50, p95, p99) = percentiles(&mut samples);
    eprintln!(
        "local edit at {objects} objects ({} edits, build {build:?}): elab p50 {p50:.3} ms, p95 {p95:.3} ms, p99 {p99:.3} ms",
        samples.len()
    );
    if objects <= 10_000 {
        // The patched rows equal a fresh build's (names compared as text: two arenas).
        let fresh = CellState::build(&p, h, &mut no_funcs).unwrap();
        let text = |st: &CellState| -> Vec<String> {
            let n = |x: schemify_elab::Name| {
                (x != schemify_elab::Name::NONE).then(|| st.names.get(x).to_owned())
            };
            let rows = st.violations.rows.iter();
            rows.map(|v| {
                format!(
                    "{} {:?} {} {:?} {}",
                    v.code.as_str(),
                    n(v.param),
                    v.object.0,
                    n(v.path),
                    v.detail
                )
            })
            .collect()
        };
        assert_eq!(text(&st), text(&fresh));
    }
    (p50, p95, p99)
}

/// D39 (editing): a local edit costs ≤ 0.5 ms of elab at 100k objects (p95), reported at
/// 10k, 100k and 1M objects.
#[test]
#[ignore]
fn a_local_edit_costs_half_a_millisecond_of_elab() {
    edit_costs(10_000);
    let (_, p95, _) = edit_costs(100_000);
    edit_costs(1_000_000);
    assert!(p95 <= 0.5, "p95 {p95:.3} ms at 100k objects");
}
