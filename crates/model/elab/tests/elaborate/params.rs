//! Param scopes, corners and overrides, plugin functions, expression errors and
//! cycles, gating waivers, valid rules and D20 cached values.

use schemify_elab::{CellState, CornerLine, Dialect, Request, Target, Val, elaborate, no_funcs};
use schemify_expr::Value;

use super::{RES, param, row};
use crate::common::{codes, project};

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
    // So for HSPICE and LTspice; ESPice runs ngspice's control blocks.
    for d in [Dialect::Hspice, Dialect::Ltspice] {
        let e = run(None, &[], d);
        assert!(codes(&e).contains(&"E033".to_owned()), "{d:?}");
    }
    assert!(!codes(&run(None, &[], Dialect::Espice)).contains(&"E033".to_owned()));
    assert!(codes(&run(Some("nope"), &[], Dialect::Ngspice)).contains(&"E035".to_owned()));
    // CDL takes no code blocks.
    assert!(run(None, &[], Dialect::Cdl).bench.code_name.is_empty());
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

/// D13 as amended 2026-10-15: a param's `valid` rule. Lengths 10n, 12n and 14n..20n are legal
/// (a measured PDK rule shape); every other length is E038, waivable to I005; a rule that is
/// not a bool is E038 too; a computed param reads a legal length.
#[test]
fn valid_rules_gate_like_bounds() {
    let dev = "schemify 1 shprim\n\npin D inout 0 0\n\nparam l 10n\n  valid \"abs(l - 10n) <= 1e-12 || abs(l - 12n) <= 1e-12 || (l >= 14n - 1e-12 && l <= 20n + 1e-12)\"\nparam k 1\n  valid k\nparam ext 0\n  computed \"steps(l, 10n, 5n, 14n, 7n, 20n)\"\n\nimpl spice spice\n";
    for (l, legal) in [
        ("10n", true),
        ("11n", false),
        ("12n", true),
        ("13n", false),
        ("14n", true),
        ("17n", true),
        ("20n", true),
        ("21n", false),
    ] {
        for waive in [false, true] {
            let w = if waive {
                "\nwaive 0w0001 0m0001 l\n  reason \"deliberate\"\n"
            } else {
                ""
            };
            let bench = format!(
                "schemify 1 shbench\n\ninst 0m0001 ./dev\n  at 0 0 R0\n  name M1\n  l {l}\n{w}"
            );
            let (p, hs) = project(&[("dev.shprim", dev), ("tb.shbench", &bench)]);
            let st = CellState::build(&p, hs[1], &mut no_funcs).unwrap();
            let mut got: Vec<(&str, &str)> = st
                .violations
                .rows
                .iter()
                .filter(|v| !v.code.as_str().starts_with('W'))
                .map(|v| (v.code.as_str(), st.names.get(v.param)))
                .collect();
            got.sort_unstable();
            // `k`'s rule is an int, never a bool: always E038 (not waived: the waiver names l).
            let mut want = vec![("E038", "k")];
            if !legal {
                want.push((if waive { "I005" } else { "E038" }, "l"));
            }
            // Past the measured table, the computed param errors too: never extrapolated.
            if l == "21n" {
                want.push(("E008", "ext"));
            }
            want.sort_unstable();
            assert_eq!(got, want, "l={l} waive={waive}");
        }
    }
}

/// FNV-1a 64 as 16 hex digits, written independently of elab.
fn fnv_hex(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// D20 as amended 2026-10-15: a `vderived` param's value comes from the instance's
/// `cached <param> <value> <digest>` line. Fresh (the digest of the instance's `vinputs`
/// values matches): the value, no finding. Stale (an input changed since): E039, waivable
/// to I005, the cached value still used. No cache: fresh while no input is set, E039 once
/// one is. The param's own override is the user's value, never stale.
#[test]
fn virtuoso_derived_values_are_cached_and_go_stale() {
    let dev = "schemify 1 shprim\n  vinputs w nf\n\npin D inout 0 0\n\nparam w 1u\nparam nf 1\n  kind int\nparam ad 5f\n  vderived true\nparam m 1\n\nimpl spice spice\n  format spice \"M{name} {D} w={w} ad={ad}\"\n";
    let at_2u = fnv_hex("w=2e-6\nnf=1\n");
    let cases: [(&str, &str, Option<&str>, Option<&str>); 7] = [
        // (overrides, cached line, finding, value of ad)
        (
            "  w 2u\n",
            &format!("  cached ad 7f {at_2u}\n"),
            None,
            Some("7e-15"),
        ),
        (
            "  w 3u\n",
            &format!("  cached ad 7f {at_2u}\n"),
            Some("E039"),
            Some("7e-15"),
        ),
        (
            "  w 3u\n  nf 1\n",
            &format!("  cached ad 7f {at_2u}\n"),
            Some("E039"),
            None,
        ),
        ("", "", None, Some("5e-15")),
        ("  w 3u\n", "", Some("E039"), Some("5e-15")),
        (
            "  w 3u\n  ad 9f\n",
            &format!("  cached ad 7f {at_2u}\n"),
            None,
            Some("9e-15"),
        ),
        // An input that is no vinput (m) never stales the value.
        (
            "  w 2u\n  m 4\n",
            &format!("  cached ad 7f {at_2u}\n"),
            None,
            Some("7e-15"),
        ),
    ];
    for (ovr, cached, want, value) in cases {
        for waive in [false, true] {
            let w = if waive {
                "\nwaive 0w0001 0m0001 ad\n  reason \"layout pending\"\n"
            } else {
                ""
            };
            let bench = format!(
                "schemify 1 shbench\n\ninst 0m0001 ./dev\n  at 0 0 R0\n  name M1\n{ovr}{cached}{w}"
            );
            let (p, hs) = project(&[("dev.shprim", dev), ("tb.shbench", &bench)]);
            let st = CellState::build(&p, hs[1], &mut no_funcs).unwrap();
            let got: Vec<(&str, &str)> = st
                .violations
                .rows
                .iter()
                .filter(|v| !v.code.as_str().starts_with('W'))
                .map(|v| (v.code.as_str(), st.names.get(v.param)))
                .collect();
            let want: Vec<(&str, &str)> = want
                .map(|c| (if waive { "I005" } else { c }, "ad"))
                .into_iter()
                .collect();
            assert_eq!(got, want, "{ovr}{cached} waive={waive}");
            assert_eq!(st.violations.blocking(), !want.is_empty() && !waive);
            if let Some(v) = value {
                let g = &st.graph;
                let ad = g.inst_param(0, 2).unwrap() as usize;
                let mut t = String::new();
                g.vals[ad].write_text(&mut t);
                assert_eq!(t, v, "{ovr}{cached}");
            }
        }
    }
}
