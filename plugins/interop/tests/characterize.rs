//! Characterization (spec `interop.md` §8.3) through the SKILL bridge on the mock Virtuoso,
//! against a made-up PDK whose parameter names mean nothing (`tests/fixtures/skill/
//! charpdk.il`): inputs, derived params and rules are found by experiment, never by name.

mod skill;

use interop::virtuoso::characterize::{self, Device, Kind};

const BUILD: &str = include_str!("fixtures/skill/build.il");
const CHARPDK: &str = include_str!("fixtures/skill/charpdk.il");

/// Characterizes `cells` of the made-up PDK with the extra SfyCharacterize keys `keys`.
fn characterize(cells: &[&str], keys: &str) -> Vec<Device> {
    let mut i = skill::virtuoso(&["zpdk", "scratch"]);
    i.load(BUILD).unwrap();
    i.load(CHARPDK).unwrap();
    i.load("(SfyFixCharPdk)").unwrap();
    let list: Vec<String> = cells.iter().map(|c| format!("\"{c}\"")).collect();
    let ok = i
        .load(&format!(
            "(SfyCharacterize \"/t/char.sfyx\" \"zpdk\" (list {}) \"scratch\" {keys})",
            list.join(" ")
        ))
        .unwrap_or_else(|e| panic!("{e}\n{}", i.log));
    let log = i.files.get("/t/char.sfyx.log").cloned().unwrap_or_default();
    assert!(ok.truthy(), "{log}");
    characterize::read(&i.files["/t/char.sfyx"]).unwrap()
}

fn names(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

#[test]
fn inputs_and_derived_params_are_found_by_experiment() {
    let devs = characterize(&["zeta", "tab", "chaos"], "?span 2");
    let zeta = &devs[0];
    assert_eq!(names(&zeta.inputs), ["zq", "kappa", "ct"]);
    assert_eq!(zeta.kinds, [Kind::Num, Kind::Num, Kind::Count]);
    assert_eq!(names(&zeta.defaults), ["5e-08", "1e-08", "1"]);
    // Negative space: a boolean and a number whose callbacks change nothing were probed
    // and are not inputs; a param without a callback that is not netlisted is no candidate.
    let probed: Vec<&str> = zeta.probes.iter().map(|p| p.name.as_str()).collect();
    assert!(
        probed.contains(&"flip") && probed.contains(&"knob"),
        "{probed:?}"
    );
    assert!(!probed.contains(&"memo"));
    for p in &zeta.probes {
        let input = zeta.inputs.contains(&p.name);
        assert_eq!(!p.changed.is_empty() && p.status.is_ok(), input, "{p:?}");
    }
    assert_eq!(
        zeta.probes.iter().find(|p| p.name == "flip").unwrap().kind,
        Kind::Bool
    );
    assert_eq!(zeta.summary().derived, ["rho", "sig"]);
    let tab = &devs[1];
    assert_eq!(names(&tab.inputs), ["mu"]);
    assert_eq!(tab.summary().derived, ["nu"]);
    let chaos = &devs[2];
    assert_eq!(names(&chaos.inputs), ["pp", "qq"]);
    assert_eq!(chaos.kinds, [Kind::Num, Kind::Count]);
    assert_eq!(chaos.summary().derived, ["zz"]);
    // ?inputs overrides discovery.
    let named = characterize(&["zeta"], "?span 2 ?inputs (list \"kappa\")");
    assert_eq!(names(&named[0].inputs), ["kappa"]);
}

/// The axes find every place a derived value or a rewrite changes, by bisection, to a
/// relative width of 1e-5; the point count stays within the budget.
#[test]
fn bisection_brackets_every_breakpoint_tightly() {
    let devs = characterize(&["zeta", "tab", "chaos"], "");
    for d in &devs {
        assert!(
            d.points.len() <= 4000,
            "{}: {} points",
            d.cell,
            d.points.len()
        );
        // A continuously varying value (chaos's hash, zeta's areas) is not bisected: no
        // device here comes near the budget.
        assert!(
            d.points.len() < 400,
            "{}: {} points",
            d.cell,
            d.points.len()
        );
    }
    let tab = &devs[1];
    let nu = tab.outputs.iter().position(|o| o == "nu").unwrap();
    // Points on mu's axis (every point that set mu), by the value set.
    let mut axis: Vec<(f64, String)> = tab
        .points
        .iter()
        .filter(|p| p.status.is_ok() && !p.set[0].is_empty())
        .map(|p| {
            (
                characterize::ael_num(&p.set[0]).unwrap(),
                p.values[nu].clone(),
            )
        })
        .collect();
    axis.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Each true breakpoint (2u, 5u, 9u) lies in a bracket of two neighbours whose values
    // differ, and the bracket is tight.
    for want in [2e-6, 5e-6, 9e-6] {
        let pair = axis
            .windows(2)
            .find(|w| w[0].1 != w[1].1 && w[0].0 < want && want <= w[1].0)
            .unwrap_or_else(|| panic!("no bracket around {want}: {axis:?}"));
        assert!(
            pair[1].0 - pair[0].0 <= 1e-5 * want * 1.01,
            "{want}: {pair:?}"
        );
    }
    // The floor at 0.5u (set below it reads back 0.5u) is bracketed too.
    let rewritten = |p: &characterize::Point| {
        !p.set[0].is_empty()
            && characterize::ael_num(&p.set[0]) != characterize::ael_num(&p.back[0])
    };
    assert!(tab.points.iter().any(rewritten), "no point below the floor");
}

/// `src` evaluated by schemify-expr at `p`, its inputs as read back.
fn eval(d: &Device, src: &str, p: &characterize::Point) -> f64 {
    use schemify_expr::{Mode, Program, Scratch, Value};
    let prog = Program::compile(src, Mode::Scalar).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    let mut inputs = Vec::new();
    prog.bind(&mut inputs, |n| {
        let k = d.inputs.iter().position(|i| i == n)?;
        let t = p.back[k].as_str();
        Some(match d.kinds[k] {
            Kind::Num | Kind::Count => Value::Num(characterize::ael_num(t)?),
            Kind::Bool => Value::Bool(t == "t"), // false reads back ""
            Kind::Choice => Value::Str(t),
        })
    })
    .unwrap();
    match prog.eval(&inputs, &mut Scratch::default()) {
        Ok(Value::Num(x)) => x,
        Ok(Value::Int(x)) => x as f64,
        other => panic!("{src} at {:?}: {other:?}", p.back),
    }
}

/// Rules found by role: a step table with its breakpoints recovered exactly, a table
/// times the multi-finger shares, an affine perimeter; a hash stays unexplained. Oracle:
/// every rule, evaluated by schemify-expr, reproduces every measured point within the
/// digits it was printed with, and the points of a holdout run the characterization never
/// measured.
#[test]
fn rules_are_found_by_role_and_reproduce_every_point() {
    let devs = characterize(&["zeta", "tab", "chaos"], "");
    let rules: Vec<characterize::Rules> = devs.iter().map(Device::rules).collect();
    let rule = |d: usize, n: &str| {
        rules[d]
            .computed
            .iter()
            .find(|c| c.0 == n)
            .unwrap_or_else(|| panic!("no rule for {n}: {:?}", rules[d]))
            .1
            .clone()
    };
    assert_eq!(
        rule(1, "nu"),
        "steps(mu, 5e-7, 3, 2e-6, 7, 5e-6, 2, 9e-6, 5, 0.0001)"
    );
    // The breakpoints are the true ones though kappa below 18n snaps to even nanometres:
    // the table is over what the device holds.
    assert_eq!(
        rule(0, "rho"),
        "zq*(floor(ct/2)+1)/ct*steps(kappa, 2e-9, 5e-8, 1.4e-8, 4.4e-8, 1.6e-8, 4.8e-8, 2e-8, 6e-8, 1e-6)"
    );
    assert_eq!(
        rule(0, "sig"),
        "ceil(ct/2)*(2*zq/ct + steps(kappa, 2e-9, 1e-7, 1.4e-8, 8.8e-8, 1.6e-8, 9.6e-8, 2e-8, 1.2e-7, 1e-6))"
    );
    assert_eq!(rules[2].unexplained, ["zz"]);
    assert!(rules[2].computed.is_empty());
    for (d, r) in devs.iter().zip(&rules) {
        for (name, src) in &r.computed {
            let k = d.outputs.iter().position(|o| o == name).unwrap();
            for p in d.points.iter().filter(|p| p.status.is_ok()) {
                let (want, tol) = characterize::printed_tol(&p.values[k]).unwrap();
                let got = eval(d, src, p);
                assert!(
                    (got - want).abs() <= tol * 1.000001,
                    "{name} = {src} at {:?}: {got} vs {want}",
                    p.back
                );
            }
        }
    }
    // The holdout: points no sweep measured, through the same callbacks.
    for (d, r) in devs.iter().zip(&rules).take(2) {
        let pts = characterize::holdout_points(d);
        let mut i = skill::virtuoso(&["zpdk", "scratch"]);
        i.load(BUILD).unwrap();
        i.load(CHARPDK).unwrap();
        i.load("(SfyFixCharPdk)").unwrap();
        let ok = i
            .load(&format!(
                "(SfyCharacterize \"/t/h.sfyx\" \"zpdk\" (list {:?}) \"scratch\" ?points (list {}))",
                d.cell,
                pts.join(" ")
            ))
            .unwrap();
        assert!(ok.truthy(), "{}", i.files["/t/h.sfyx.log"]);
        let h = characterize::read(&i.files["/t/h.sfyx"]).unwrap().remove(0);
        assert_eq!(h.points.len(), 3);
        let res = characterize::check(r, &h);
        assert!(
            !res.is_empty() && res.iter().all(|x| x.2),
            "{}: {res:?} at {pts:?}",
            d.cell
        );
    }
}

/// Negative space: one point that disagrees with an otherwise clean step table leaves its
/// parameter unexplained (the extra steps are not located by any bisection).
#[test]
fn a_point_against_the_rule_leaves_it_unexplained() {
    let mut tab = characterize(&["tab"], "").remove(0);
    assert!(tab.rules().unexplained.is_empty());
    let nu = tab.outputs.iter().position(|o| o == "nu").unwrap();
    let p = tab
        .points
        .iter_mut()
        .filter(|p| p.status.is_ok())
        .min_by(|a, b| {
            let d = |p: &characterize::Point| {
                (characterize::ael_num(&p.back[0]).unwrap() - 3.3e-6).abs()
            };
            d(a).total_cmp(&d(b))
        })
        .unwrap();
    p.values[nu] = "4".into();
    let r = tab.rules();
    assert_eq!(r.unexplained, ["nu"], "{r:?}");
}

/// A rule per value of a choice or a boolean input: `if()` over the selector.
#[test]
fn a_selector_input_multiplexes_rules() {
    let d = characterize(&["sel"], "").remove(0);
    assert_eq!(names(&d.inputs), ["gx", "mode", "dbl"]);
    assert_eq!(d.kinds, [Kind::Num, Kind::Choice, Kind::Bool]);
    let r = d.rules();
    assert!(r.unexplained.is_empty(), "{r:?}");
    let rule = |n: &str| r.computed.iter().find(|c| c.0 == n).unwrap().1.as_str();
    assert_eq!(
        rule("pz"),
        "if(mode == \"a\", 3*gx, if(mode == \"b\", 5*gx, 7e-7))"
    );
    assert_eq!(rule("qz"), "if(dbl, 2*gx, gx)");
    for (name, src) in &r.computed {
        let k = d.outputs.iter().position(|o| o == name).unwrap();
        for p in d.points.iter().filter(|p| p.status.is_ok()) {
            let (want, tol) = characterize::printed_tol(&p.values[k]).unwrap();
            assert!(
                (eval(&d, src, p) - want).abs() <= tol * 1.000001,
                "{src} at {:?}",
                p.back
            );
        }
    }
}

/// `src`, a legality rule, at `p`: its inputs as set.
fn legal(d: &Device, src: &str, p: &characterize::Point) -> bool {
    use schemify_expr::{Mode, Program, Scratch, Value};
    let prog = Program::compile(src, Mode::Scalar).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    let mut inputs = Vec::new();
    prog.bind(&mut inputs, |n| {
        let k = d.inputs.iter().position(|i| i == n)?;
        let t = d.set_value(p, k);
        Some(match d.kinds[k] {
            Kind::Num | Kind::Count => Value::Num(characterize::ael_num(t)?),
            Kind::Bool => Value::Bool(t == "t"),
            Kind::Choice => Value::Str(t),
        })
    })
    .unwrap();
    match prog.eval(&inputs, &mut Scratch::default()) {
        Ok(Value::Bool(b)) => b,
        other => panic!("{src} at {:?}: {other:?}", p.set),
    }
}

/// Legality from the rewrites alone: a floor, a floor per count, a grid below a kept
/// stretch, a floor that depends on a choice; a rewrite with no pattern is reported.
/// Oracle: each rule, evaluated by schemify-expr at the values set, is true exactly at
/// the points whose value the callbacks kept.
#[test]
fn legality_accepts_exactly_the_values_the_callbacks_keep() {
    let devs = characterize(&["zeta", "tab", "chaos", "sel"], "");
    let rules: Vec<characterize::Rules> = devs.iter().map(Device::rules).collect();
    let valid = |d: usize, n: &str| {
        rules[d]
            .legal
            .iter()
            .find(|c| c.0 == n)
            .unwrap_or_else(|| panic!("no legality for {n}: {:?}", rules[d]))
            .1
            .clone()
    };
    assert_eq!(valid(1, "mu"), "mu >= 5e-7");
    assert_eq!(valid(0, "zq"), "zq/ct >= 4e-8");
    assert_eq!(
        valid(0, "kappa"),
        "(kappa < 1.8e-8 && abs(kappa/2e-9 - round(kappa/2e-9)) <= 1e-6) || kappa >= 1.8e-8"
    );
    assert_eq!(
        valid(3, "gx"),
        // Mode b was never measured below its floor: nothing there is illegal.
        "if(mode == \"a\", gx >= 1e-6, if(mode == \"b\", true, gx >= 3e-6))"
    );
    assert_eq!(rules[2].unruled, ["pp"]);
    // Inputs never rewritten get no rule.
    assert!(rules[0].legal.iter().all(|c| c.0 != "ct"), "{:?}", rules[0]);
    for (d, r) in devs.iter().zip(&rules) {
        for (name, src) in &r.legal {
            let k = d.inputs.iter().position(|i| i == name).unwrap();
            for p in d.points.iter().filter(|p| p.status.is_ok()) {
                let (s, b) = (
                    characterize::ael_num(d.set_value(p, k)).unwrap(),
                    characterize::ael_num(&p.back[k]).unwrap(),
                );
                let kept = (s - b).abs() <= 1e-6 * s.abs();
                assert_eq!(
                    legal(d, src, p),
                    kept,
                    "{name}: {src} at {:?} -> {:?}",
                    p.set,
                    p.back
                );
            }
        }
    }
}
