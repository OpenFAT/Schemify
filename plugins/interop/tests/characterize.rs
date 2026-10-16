//! Characterization (spec `interop.md` §8.3) through the SKILL bridge on the mock Virtuoso,
//! against a made-up PDK whose parameter names mean nothing (`tests/fixtures/skill/
//! charpdk.il`): inputs, derived params and rules are found by experiment, never by name.

mod common;
mod skill;

use interop::design::Files;
use interop::map::Mapping;
use interop::virtuoso::characterize::{self, Device, Kind};
use interop::virtuoso::{self, ExportOptions, ImportOptions};

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

/// The blocking findings and I005s of `cell` in project `files` (`code param` each).
fn findings(files: &Files, cell: &str) -> Vec<String> {
    use schemify_doc::{Project, core_library};
    use schemify_elab::{CellState, no_funcs};
    let manifest = files.get("schemify.project").map(Vec::as_slice);
    let mut p = Project::new(manifest, None, 7).unwrap();
    let dirs = interop::design::manifest_libs(manifest.unwrap()).unwrap();
    let mut libs: std::collections::BTreeMap<String, Vec<(&str, &[u8])>> = Default::default();
    libs.insert("core".into(), core_library().to_vec());
    for (path, bytes) in files.iter().filter(|f| common::is_cell(f.0)) {
        let (dir, rel) = path.rsplit_once('/').unwrap_or(("", path));
        let lib = dirs
            .iter()
            .find(|(_, d)| d.as_str() == dir)
            .map_or(String::new(), |(l, _)| l.clone());
        libs.entry(lib).or_default().push((rel, bytes));
    }
    for (lib, listing) in &libs {
        p.index_library(lib, "t", listing).unwrap();
    }
    let h = p.open("", cell, &files[cell], None).unwrap().handle;
    let st = CellState::build(&p, h, &mut no_funcs).unwrap();
    st.violations
        .rows
        .iter()
        .filter(|v| v.code.as_str().starts_with('E') || v.code.as_str() == "I005")
        .map(|v| format!("{} {}", v.code.as_str(), st.names.get(v.param)))
        .collect()
}

/// The `cached zz` line of the design's instance: (value, digest).
fn cached_zz(files: &Files) -> Option<(String, String)> {
    let d = schemify_format::parse(&files["top.shcomp"]).unwrap();
    let r =
        (1..d.record_count()).find(|&r| d.record_kind(r) == schemify_format::RecordKind::Inst)?;
    let a = d
        .attrs(r)
        .find(|&a| d.attr_key(a) == "cached" && d.attr_value(a, 0) == Some("zz"))?;
    Some((
        d.attr_value(a, 1)?.to_owned(),
        d.attr_value(a, 2)?.to_owned(),
    ))
}

/// P2 end to end (D20 as amended 2026-10-15): a derived value no rule explains (`zz`, a
/// hash of `pp` and `qq`) comes into Schemify from Virtuoso cached with the digest of its
/// inputs, fresh; an edit of an input in Schemify makes it stale (E039) and the export
/// leaves it out; Virtuoso's callbacks (`SfyImport ?callbacks t`) derive it again and the
/// next import brings it back fresh, with the callbacks' value.
#[test]
fn a_derived_value_goes_stale_in_schemify_and_comes_back_fresh() {
    let mut i = skill::virtuoso(&["zpdk", "scratch", "work", "analogLib", "basic"]);
    i.load(BUILD).unwrap();
    i.load("(SfyFixStdlib)").unwrap();
    i.load(CHARPDK).unwrap();
    i.load("(SfyFixCharPdk)").unwrap();
    // The PDK mirror, with the characterization's rules.
    assert!(
        i.load("(SfyCharacterize \"/t/char.sfyx\" \"zpdk\" (list \"chaos\") \"scratch\")")
            .unwrap()
            .truthy()
    );
    let dev = characterize::read(&i.files["/t/char.sfyx"])
        .unwrap()
        .remove(0);
    let rules = dev.rules();
    assert_eq!(rules.unexplained, ["zz"]);
    assert!(
        i.load("(SfyExport \"/t/pdk.sfyx\" \"zpdk\" nil)")
            .unwrap()
            .truthy()
    );
    let pdk = virtuoso::import(
        &i.files["/t/pdk.sfyx"],
        &Files::new(),
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap();
    let mut files: Files = pdk.files.into_iter().collect();
    let mirror = files
        .keys()
        .find(|k| k.ends_with("chaos.shprim"))
        .unwrap()
        .clone();
    let applied = characterize::apply_rules(&files[&mirror], &rules).unwrap();
    assert!(
        applied.contains("  vinputs pp qq\n") && applied.contains("  vderived true"),
        "{applied}"
    );
    files.insert(mirror, applied.into_bytes());
    // A design instantiating it, sent to Virtuoso with its callbacks.
    let design = |pp: &str| {
        format!(
            "schemify 1 shcomp\n\npin A inout 0 0\n\nimpl schematic schematic\n\ninst 0x0001 zpdk:chaos\n  at 0 0 R0\n  name X1\n  pp {pp}\n\nport 0p0001 0 0 R0 A\n"
        )
    };
    files.insert("top.shcomp".into(), design("2.5").into_bytes());
    let mut io = ImportOptions::default();
    io.lib_map.insert("work".into(), String::new());
    let mut eo = ExportOptions {
        producer: "t".into(),
        ..ExportOptions::default()
    };
    eo.lib_map.insert(String::new(), "work".into());
    let mut trip = |files: &mut Files, overwrite: bool| -> (String, String) {
        let d = common::load_tops(files, &["top.shcomp"]);
        let out = virtuoso::export(&d, &[0], &Mapping::default(), &eo)
            .unwrap()
            .sfyx;
        i.files.insert("/t/d.sfyx".into(), out.clone());
        let keys = if overwrite { " ?overwrite t" } else { "" };
        let ok = i
            .load(&format!("(SfyImport \"/t/d.sfyx\" ?callbacks t{keys})"))
            .unwrap();
        assert!(ok.truthy(), "{}", i.files["/t/d.sfyx.log"]);
        assert!(
            i.load("(SfyExport \"/t/back.sfyx\" \"work\" (list \"top\"))")
                .unwrap()
                .truthy()
        );
        let back =
            virtuoso::import(&i.files["/t/back.sfyx"], files, &Mapping::default(), &io).unwrap();
        for (p, b) in back.files {
            files.insert(p, b);
        }
        (out, i.files["/t/back.sfyx"].clone())
    };
    // zz is a string param (no parseAsNumber): its canonical value is quoted.
    let zz = |pp: i64, qq: i64| format!("\"{}\"", (7 * pp + 13 * qq) % 11);
    let (_, back) = trip(&mut files, false);
    let top = String::from_utf8_lossy(&files["top.shcomp"]).into_owned();
    let (v, d1) = cached_zz(&files).unwrap_or_else(|| panic!("{top}\n{back}"));
    assert_eq!(v, zz(2500, 1));
    assert!(
        findings(&files, "top.shcomp").is_empty(),
        "{:?}",
        findings(&files, "top.shcomp")
    );
    // The edit: pp 2.5 -> 3.5. The cached value is stale.
    let edited = String::from_utf8(files["top.shcomp"].clone())
        .unwrap()
        .replace("  pp 2.5\n", "  pp 3.5\n");
    files.insert("top.shcomp".into(), edited.into_bytes());
    assert_eq!(findings(&files, "top.shcomp"), ["E039 zz"]);
    // The export leaves the stale value to the callbacks; they derive it again.
    let (sent, _) = trip(&mut files, true);
    assert!(!sent.contains("\"zz\""), "a stale value was sent:\n{sent}");
    let (v, d2) = cached_zz(&files).expect("a cached zz line");
    assert_eq!(v, zz(3500, 1));
    assert_ne!(d1, d2);
    assert!(
        findings(&files, "top.shcomp").is_empty(),
        "{:?}",
        findings(&files, "top.shcomp")
    );
}

/// Property: whatever a device's step table, the sweep stays within its 4000-point budget,
/// and a table sparse enough to read as steps (at most a third of the scan's intervals
/// change) has every breakpoint bracketed to 1e-5. Tables of 0 to 300 breakpoints at
/// seeded log-spread positions between 0.3u and 90u.
#[test]
fn the_sweep_is_bounded_for_any_step_table() {
    let mut seed: u64 = 7;
    for k in [0usize, 1, 4, 9, 40, 300] {
        let mut breaks: Vec<f64> = (0..k)
            .map(|_| {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let u = (seed >> 11) as f64 / (1u64 << 53) as f64;
                // Three significant digits: a breakpoint a PDK would write.
                let x = 0.3e-6 * 300f64.powf(u);
                let e = x.log10().floor() - 2.0;
                (x / 10f64.powf(e)).round() * 10f64.powf(e)
            })
            .collect();
        breaks.sort_by(f64::total_cmp);
        breaks.dedup();
        let list: Vec<String> = breaks.iter().map(|b| format!("{b:e}")).collect();
        let mut i = skill::virtuoso(&["zpdk", "scratch"]);
        i.load(BUILD).unwrap();
        i.load(CHARPDK).unwrap();
        i.load(&format!(
            r#"(setq rndBreaks (list {}))
(procedure (rndCB)
  (let ((x (atof (zpdkGet "xx"))) (n 0))
    (foreach b rndBreaks (when (geqp x b) (setq n (add1 n))))
    (zpdkSet "yy" (sprintf nil "%d" n))
    t))
(SfyFixSymbol "zpdk" "rnd" (list (list "A" "inputOutput" 0 40) (list "B" "inputOutput" 0 -40)))
(let ((cdf (SfyFixCdf "zpdk" "rnd")))
  (cdfCreateParam cdf ?name "xx" ?type "string" ?defValue "1e-06" ?prompt "xx" ?parseAsNumber "yes" ?callback "(rndCB)")
  (cdfCreateParam cdf ?name "yy" ?type "string" ?defValue "0" ?prompt "yy")
  (setSGq cdf (list nil 'spectre (list nil 'termOrder '(A B) 'instParameters '(xx yy))) simInfo)
  (cdfSaveCDF cdf))"#,
            list.join(" ")
        ))
        .unwrap();
        let ok = i
            .load(r#"(SfyCharacterize "/t/r.sfyx" "zpdk" (list "rnd") "scratch" ?inputs (list "xx"))"#)
            .unwrap();
        assert!(ok.truthy(), "{}", i.files["/t/r.sfyx.log"]);
        let d = characterize::read(&i.files["/t/r.sfyx"]).unwrap().remove(0);
        assert!(d.points.len() <= 4000, "k={k}: {} points", d.points.len());
        let mut axis: Vec<(f64, String)> = d
            .points
            .iter()
            .filter(|p| p.status.is_ok())
            .map(|p| {
                (
                    characterize::ael_num(d.set_value(p, 0)).unwrap(),
                    p.values[1].clone(),
                )
            })
            .collect();
        axis.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Scan intervals that change: the steps are located when few enough.
        if k <= 9 {
            for b in &breaks {
                let w = axis
                    .windows(2)
                    .find(|w| w[0].0 < *b && *b <= w[1].0)
                    .unwrap_or_else(|| panic!("k={k}: {b} outside the scan"));
                assert!(w[1].0 - w[0].0 <= 1e-5 * b * 1.01, "k={k}: {b}: {w:?}");
            }
        }
    }
}

/// Negative space from a live run: a count input at 0 next to a rewritten number input. The
/// floor per count reading divides by it and is skipped; the floor itself is found.
#[test]
fn a_zero_count_does_not_break_legality() {
    let mut d = characterize(&["zeta"], "").remove(0);
    let ct = d.inputs.iter().position(|i| i == "ct").unwrap();
    let zq = d.inputs.iter().position(|i| i == "zq").unwrap();
    for p in d.points.iter_mut().take(5) {
        p.set[ct] = "0".into();
        p.back[ct] = "0".into();
        // 0 over 0: no number at all.
        p.set[zq] = "0".into();
        p.back[zq] = "0".into();
    }
    let r = d.rules();
    assert!(r.unruled.iter().all(|u| u != "kappa"), "{r:?}");
}
