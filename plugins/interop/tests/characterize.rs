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
