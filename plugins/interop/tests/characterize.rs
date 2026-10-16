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
