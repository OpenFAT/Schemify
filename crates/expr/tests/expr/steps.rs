//! `steps(x, b0, y0, …, bn)`: the measured step table (spec expr.md §5, D12 as amended
//! 2026-10-15). Oracle: a linear scan over the same table.

use proptest::prelude::*;
use schemify_expr::{Code, Value};

use super::{Owned, code, eval, eval_with};

/// The table's value at `x` by a plain scan, `None` outside `[b0, bn]`.
fn scan(bounds: &[f64], vals: &[f64], x: f64) -> Option<f64> {
    let last = *bounds.last()?;
    if x == last {
        return vals.last().copied();
    }
    (0..vals.len())
        .find(|&k| bounds[k] <= x && x < bounds[k + 1])
        .map(|k| vals[k])
}

fn call(bounds: &[f64], vals: &[f64]) -> String {
    let mut s = "steps(x".to_owned();
    for (b, v) in bounds.iter().zip(vals) {
        s.push_str(&format!(", {b:e}, {v:e}"));
    }
    s.push_str(&format!(", {:e})", bounds.last().unwrap()));
    s
}

#[test]
fn a_non_monotonic_table_reads_back_per_segment() {
    // A measured table that rises and falls (made-up values, as a PDK rule table can).
    let t = "steps(l, 10n, 50n, 12n, 40n, 15n, 60n, 30n, 55n, 50n)";
    for (l, want) in [
        (10e-9, 50e-9),
        (11.9e-9, 50e-9),
        (12e-9, 40e-9),
        (15e-9, 60e-9),
        (30e-9, 55e-9),
        (50e-9, 55e-9),
    ] {
        let got = eval_with(t, &[("l", Value::Num(l))]).unwrap();
        assert_eq!(got, Owned::Num(want), "l = {l}");
    }
    // A length computed in floating point, a few ulp under a breakpoint, is that breakpoint.
    let l = 30e-9 * (1.0 - 1e-15);
    assert_eq!(
        eval_with(t, &[("l", Value::Num(l))]).unwrap(),
        Owned::Num(55e-9)
    );
    // Outside what was measured: an error, never the nearest segment.
    for l in [9e-9, 50.5e-9, 1e-6] {
        let e = super::eval_with(t, &[("l", Value::Num(l))]).unwrap_err();
        assert_eq!(e.code, Code::X010, "l = {l}");
    }
}

#[test]
fn malformed_tables_are_refused() {
    assert_eq!(code("steps(1, 0, 5, 2, 6)"), Code::X008); // odd: a value with no bound
    assert_eq!(code("steps(1, 0, 5)"), Code::X008); // fewer than four arguments
    assert_eq!(code("steps(1, 2, 5, 2)"), Code::X003); // breakpoints must increase
    assert_eq!(code("steps(1, 0, \"a\", 2)"), Code::X003); // values are numbers
    assert_eq!(eval("steps(1, 0, 5, 2)").unwrap(), Owned::Int(5));
}

proptest! {
    /// Any table, any point: `steps` agrees with the scan inside the range and is X010
    /// outside it.
    #[test]
    fn steps_agrees_with_a_scan(
        mut bounds in prop::collection::btree_set(0i32..1000, 2..12),
        vals in prop::collection::vec(-1000i32..1000, 11),
        x in -10i32..1010,
    ) {
        let bounds: Vec<f64> = std::mem::take(&mut bounds).into_iter().map(|b| f64::from(b) * 1e-9).collect();
        let vals: Vec<f64> = vals[..bounds.len() - 1].iter().map(|v| f64::from(*v) * 1e-9).collect();
        let x = f64::from(x) * 1e-9;
        let got = eval_with(&call(&bounds, &vals), &[("x", Value::Num(x))]);
        match scan(&bounds, &vals, x) {
            Some(want) => prop_assert_eq!(got.unwrap(), Owned::Num(want)),
            None => prop_assert_eq!(got.unwrap_err().code, Code::X010),
        }
    }
}
