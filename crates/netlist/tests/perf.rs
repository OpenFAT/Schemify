//! D39 budget for emission (nightly tier): netlisting a 1M-instance design ≤ 1 s end to
//! end. Elaboration is elab's (its `tests/perf.rs`); here emission alone is measured on
//! elab's two perf designs: a hierarchy that flattens to 2^20 leaves (5 definitions)
//! and a flat bench of 10^5 instances (×10 extrapolates to 10^6 flat).
//!
//! Run: `cargo test -p schemify-netlist --release --test perf -- --ignored --nocapture`.

mod common;

use common::*;
use schemify_elab::Dialect;
use schemify_netlist::emit;
use std::time::{Duration, Instant};

const RES: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nparam R 1k\n  min 1\n\nimpl spice spice\n  format spice \"R{name} {A} {B} {R}\"\n";

/// elab's perf hierarchy level: `fan` instances of `child` chained between ports A, B.
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
        let x = c(i as i64 * 100_000);
        s.push_str(&format!("label {} {x} 0 R0 {a}\n", id(1000 + 2 * i as u64)));
        s.push_str(&format!(
            "label {} {x} 20 R0 {b}\n",
            id(1001 + 2 * i as u64)
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

fn best_of<T>(n: usize, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut best = Duration::MAX;
    let mut out = None;
    for _ in 0..n {
        let t = Instant::now();
        let v = f();
        best = best.min(t.elapsed());
        out = Some(v);
    }
    (best, out.expect("n > 0"))
}

#[test]
#[ignore]
fn emit_a_hierarchy_of_a_million_leaves() {
    let fan = 32;
    let (l1, l2, l3, l4) = (
        level("res", fan, "R"),
        level("l1", fan, "r"),
        level("l2", fan, "r"),
        level("l3", fan, "r"),
    );
    let bench = "schemify 1 shbench\n\ninst 0x0001 ./l4\n  at 0 0 R0\n  name X\n\nlabel 0n0001 0 0 R0 in\nlabel 0n0002 0 20 R0 0\n";
    let (p, hs) = project(&[
        ("res.shprim", RES),
        ("l1.shcomp", &l1),
        ("l2.shcomp", &l2),
        ("l3.shcomp", &l3),
        ("l4.shcomp", &l4),
        ("tb.shbench", bench),
    ]);
    let (te, e) = best_of(3, || elab(&p, hs[5], Dialect::Ngspice));
    assert_eq!(e.flat_leaves(e.top), (fan as u64).pow(4));
    let (dt, n) = best_of(10, || emit(&e, Dialect::Ngspice));
    assert_eq!(n.text.matches(".subckt").count(), 4);
    assert_eq!(
        n.name_map.inst_name("/X/X3/X4/X5/X6").as_deref(),
        Some("X.X3.X4.X5.RX6")
    );
    eprintln!(
        "1M-leaf hierarchy: elaborate {te:?}, emit {dt:?} ({} bytes)",
        n.text.len()
    );
    eprintln!("PERF netlist.emit_hier_1m_ms {:.3}", dt.as_secs_f64() * 1e3);
    assert!(dt < Duration::from_millis(5), "{dt:?}");
}

#[test]
#[ignore]
fn emit_a_flat_bench_of_100k_instances() {
    let n = 100_000;
    let mut s = String::from("schemify 1 shbench\n\n");
    for i in 0..n {
        s.push_str(&format!(
            "inst {} ./res\n  at 0 {} R0\n  name R{i}\n  R {}\n",
            id(i as u64 + 1),
            c(i as i64 * 20_000),
            i % 7 + 1
        ));
    }
    let (p, hs) = project(&[("res.shprim", RES), ("tb.shbench", &s)]);
    let (te, e) = best_of(1, || elab(&p, hs[1], Dialect::Ngspice));
    let (dt, out) = best_of(5, || emit(&e, Dialect::Ngspice));
    assert_eq!(out.text.lines().filter(|l| l.starts_with("RR")).count(), n);
    let (dl, path) = best_of(1, || out.name_map.inst_path("rr99999"));
    assert_eq!(path.as_deref(), Some("/R99999"));
    eprintln!(
        "flat 100k: elaborate {te:?}, emit {dt:?} ({} bytes), first reverse lookup {dl:?}; ×10 ≈ 1M flat",
        out.text.len()
    );
    eprintln!(
        "PERF netlist.emit_flat_100k_ms {:.3}",
        dt.as_secs_f64() * 1e3
    );
    // D39: 1M flat ≤ 1 s end to end (measured at full size below). Regression guard for
    // emission:
    assert!(dt < Duration::from_millis(15), "{dt:?}");
}

/// D39 at its stated size: a flat bench of 10^6 instances netlisted end to end (elaborate +
/// emit, the document already open) within 1 s.
#[test]
#[ignore]
fn netlist_a_flat_bench_of_1m_instances() {
    let n = 1_000_000;
    let mut s = String::with_capacity(n * 48);
    s.push_str("schemify 1 shbench\n\n");
    // Ten columns of 10^5 resistors in series (one column would leave the i32 range).
    for i in 0..n {
        s.push_str(&format!(
            "inst {} ./res\n  at {} {} R0\n  name R{i}\n  R {}\n",
            id(i as u64 + 1),
            c((i / 100_000) as i64 * 100_000),
            c((i % 100_000) as i64 * 20_000),
            i % 7 + 1
        ));
    }
    let (p, hs) = project(&[("res.shprim", RES), ("tb.shbench", &s)]);
    drop(s);
    let t = Instant::now();
    let e = elab(&p, hs[1], Dialect::Ngspice);
    let te = t.elapsed();
    let out = emit(&e, Dialect::Ngspice);
    let total = t.elapsed();
    assert_eq!(e.flat_leaves(e.top), n as u64);
    assert_eq!(out.text.lines().filter(|l| l.starts_with("RR")).count(), n);
    eprintln!(
        "flat 1M: elaborate {te:?}, emit {:?}, total {total:?} ({} bytes)",
        total - te,
        out.text.len()
    );
    assert!(
        total < Duration::from_secs(1),
        "D39: flat 1M netlist {total:?} > 1 s"
    );
}
