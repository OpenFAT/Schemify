//! Both sides of the Virtuoso bridge together (spec `interop.md` §7.4): the Rust exporter's
//! `.sfyx` is loaded by the shipped SKILL (`SfyImport`, in the test interpreter, into the
//! mock Virtuoso), dumped back by the shipped SKILL (`SfyExport`), and imported by the Rust
//! importer; the cell files must come back byte-identical. This is the only test where a
//! file written by one side is read by the other in both directions, so it is the one that
//! catches disagreements about record forms, ordering, units and stashes.

mod common;
mod skill;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use common::{
    canonical, design_libs, export_cells, import_opts, is_cell, line_of, load_all, load_tops,
    located, project, read_tree, root, strip_fp,
};
use interop::design::Files;
use interop::map::Mapping;
use interop::sfyx;
use interop::virtuoso::{self, ExportOptions, ImportOptions};
use skill::mockdb::MockDb;
use skill::{Interp, V};

const BUILD: &str = include_str!("fixtures/skill/build.il");

/// A mock Virtuoso with `libs`, `schemify.il` and the fixture builders, the standard
/// libraries (`analogLib`, `basic`) drawn.
fn with_stdlib(libs: &[&str]) -> Interp<MockDb> {
    let mut i = skill::virtuoso(libs);
    i.load(BUILD).expect("build.il loads");
    i.load("(SfyFixStdlib)").expect("stdlib fixtures");
    i
}

/// Runs one fixture project through Rust export → SKILL import → SKILL export → Rust
/// import and returns (original cells, round-tripped cells, SKILL logs).
fn round_trip(dir: &Path) -> (BTreeMap<String, String>, BTreeMap<String, String>, String) {
    let files = project(dir);
    let exported = export_cells(&load_all(&files), "e2e");
    let recs = sfyx::read(&exported).unwrap();

    // The SKILL side: every library the dump names exists in cds.lib, plus analogLib/basic.
    let mut libs: BTreeSet<&str> = ["analogLib", "basic"].into();
    for r in &recs {
        if r.tag == "lib" {
            libs.insert(r.str(0).unwrap());
        }
    }
    let mut i = with_stdlib(&libs.into_iter().collect::<Vec<_>>());
    i.files.insert("/e2e/in.sfyx".into(), exported.clone());
    let ok = i
        .load("(SfyImport \"/e2e/in.sfyx\")")
        .unwrap_or_else(|e| panic!("SfyImport: {e}\n{}", i.log));
    let mut logs = i.files.get("/e2e/in.sfyx.log").cloned().unwrap_or_default();
    assert!(
        matches!(ok, V::T),
        "{}: SfyImport failed:\n{logs}\n{}",
        dir.display(),
        i.log
    );

    // Dump every design library back and import each into Rust.
    let design_libs = design_libs(&exported);
    assert!(!design_libs.is_empty(), "{}: no design cell", dir.display());
    let manifest: Files = files
        .iter()
        .filter(|(p, _)| *p == "schemify.project")
        .map(|(p, b)| (p.clone(), b.clone()))
        .collect();
    let io = import_opts();
    let mut got = BTreeMap::new();
    for lib in &design_libs {
        let path = format!("/e2e/{lib}.sfyx");
        let r = i
            .load(&format!("(SfyExport \"{path}\" \"{lib}\" nil)"))
            .unwrap_or_else(|e| panic!("SfyExport {lib}: {e}\n{}", i.log));
        let log = i
            .files
            .get(&format!("{path}.log"))
            .cloned()
            .unwrap_or_default();
        logs.push_str(&log);
        assert!(
            matches!(r, V::T),
            "{}: SfyExport {lib} failed:\n{log}",
            dir.display()
        );
        let text = i.files[&path].clone();
        let imp =
            virtuoso::import(&text, &manifest, &Mapping::default(), &io).unwrap_or_else(|e| {
                panic!(
                    "{}: import of the SKILL dump of {lib}: {e:?}\n{text}",
                    dir.display()
                )
            });
        for (p, b) in imp.files {
            if is_cell(&p) {
                got.insert(p, strip_fp(&String::from_utf8(b).unwrap()));
            }
        }
    }
    let want: BTreeMap<String, String> = files
        .iter()
        .filter(|(p, _)| is_cell(p))
        .map(|(p, b)| (p.clone(), canonical(b)))
        .collect();
    (want, got, logs)
}

#[test]
fn rust_export_skill_import_skill_export_rust_import_is_exact() {
    let projects = [
        root().join("crates/app/tests/fixtures/rc"),
        root().join("crates/app/tests/fixtures/inv"),
        root().join("crates/app/tests/fixtures/ota"),
        root().join("crates/elab/tests/fixtures"),
    ];
    for dir in projects {
        let (want, got, logs) = round_trip(&dir);
        for (p, w) in &want {
            let g = got.get(p).unwrap_or_else(|| {
                panic!("{}: {p} did not come back\nlogs:\n{logs}", dir.display())
            });
            assert_eq!(
                g,
                w,
                "{}: {p} differs after the round trip through the SKILL side\nlogs:\n{logs}",
                dir.display()
            );
        }
    }
}

#[test]
#[ignore]
fn debug_dump_cell() {
    // SFY_DUMP_DIR=<fixture> SFY_DUMP_CELL=<cell>: prints that cell's records from both sides.
    let dir = PathBuf::from(std::env::var("SFY_DUMP_DIR").unwrap());
    let cell = std::env::var("SFY_DUMP_CELL").unwrap();
    let files = read_tree(&dir);
    let text = export_cells(&load_all(&files), "e2e");
    let section = |t: &str| -> String {
        let mut on = false;
        let mut out = String::new();
        for l in t.lines() {
            if l.starts_with("(\"cell\"") {
                on = l.starts_with(&format!("(\"cell\" \"{cell}\""));
            }
            if on && !l.contains("schemifyRecords") {
                out.push_str(l);
                out.push('\n');
            }
        }
        out
    };
    println!("=== RUST\n{}", section(&text));
    let recs = sfyx::read(&text).unwrap();
    let mut libs: Vec<&str> = recs
        .iter()
        .filter(|r| r.tag == "lib")
        .map(|r| r.str(0).unwrap())
        .collect();
    libs.extend(["analogLib", "basic"]);
    let mut i = with_stdlib(&libs);
    i.files.insert("/e2e/in.sfyx".into(), text.clone());
    i.load("(SfyImport \"/e2e/in.sfyx\")").unwrap();
    let lib = std::env::var("SFY_DUMP_LIB").unwrap_or("dev".into());
    i.load(&format!("(SfyExport \"/e2e/o.sfyx\" \"{lib}\" nil)"))
        .unwrap();
    println!("=== SKILL\n{}", section(&i.files["/e2e/o.sfyx"]));
}

/// The dump without what the round trip adds by design: the header's producer and the
/// `schemify*` properties (ids, stash, net spellings) the Rust exporter writes.
fn without_schemify_props(text: &str) -> String {
    let recs: Vec<sfyx::Record> = sfyx::read(text)
        .unwrap()
        .into_iter()
        .filter(|r| r.tag != "sfyx")
        .filter(|r| !(r.tag == "prop" && r.str(1).is_ok_and(|n| n.starts_with("schemify"))))
        .collect();
    sfyx::write(&recs)
}

#[test]
fn skill_dump_rust_import_rust_export_skill_import_skill_dump_is_exact() {
    let dumps: [(&str, &str, &[&str]); 5] = [
        ("inv.sfyx", "work", &["inv"]),
        ("bus.sfyx", "work", &["bus"]),
        ("symkit.sfyx", "work", &["symkit"]),
        ("netx.sfyx", "work", &["netx"]),
        ("vamod.sfyx", "work", &["vamod"]),
    ];
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skill");
    for (name, lib, cells) in dumps {
        let Ok(first) = std::fs::read_to_string(dir.join(name)) else {
            panic!("missing golden {name}");
        };
        // Rust import into an empty project.
        let imp = virtuoso::import(
            &first,
            &Files::new(),
            &Mapping::default(),
            &ImportOptions::default(),
        )
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let files: Files = imp.files.into_iter().collect();
        // Rust export of the imported design cells.
        let tops: Vec<String> = cells
            .iter()
            .map(|c| format!("{lib}/{c}"))
            .flat_map(|b| {
                [
                    format!("{b}.shcomp"),
                    format!("{b}.shprim"),
                    format!("{b}.shbench"),
                ]
            })
            .filter(|p| files.contains_key(p))
            .collect();
        assert!(
            !tops.is_empty(),
            "{name}: import wrote no cell for {cells:?}: {:?}",
            files.keys().collect::<Vec<_>>()
        );
        let refs: Vec<&str> = tops.iter().map(String::as_str).collect();
        let d = load_tops(&files, &refs);
        let top_ix: Vec<usize> = (0..tops.len()).collect();
        let eo = ExportOptions {
            producer: "e2e".into(),
            ..ExportOptions::default()
        };
        let second = virtuoso::export(&d, &top_ix, &Mapping::default(), &eo)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"))
            .sfyx;
        // Every instance, pin, pin figure and label of a design cell goes out as it came in
        // (a basic/gnd stays basic's, a bare pin shape stays one, a pin keeps its master, a
        // pin-name label its layer), checked on the export itself: the load below keeps
        // unchanged cellviews (§7.5). Refs and label parents are renumbered.
        // (A symbol-only cell goes out as a reference to the original, §8.1.)
        let placed = |text: &str| -> BTreeSet<(String, String)> {
            let recs = sfyx::read(text).unwrap_or_else(|e| panic!("{name}: {}", e.msg));
            let mut out = BTreeSet::new();
            for (at, r) in located(&recs) {
                if !at.design || !matches!(r.tag.as_str(), "inst" | "pinfig" | "spin" | "label") {
                    continue;
                }
                let mut fields = r.fields[1..].to_vec();
                if r.tag == "label" {
                    fields[0] = sfyx::Value::Int(0);
                }
                out.insert((at.cell, line_of(&sfyx::Record::new(&r.tag, fields))));
            }
            out
        };
        let m2 = placed(&second);
        let sent: BTreeSet<&String> = m2.iter().map(|x| &x.0).collect();
        let m1: BTreeSet<_> = placed(&first)
            .into_iter()
            .filter(|x| sent.contains(&x.0))
            .collect();
        assert!(
            m1.is_subset(&m2),
            "{name}: changed on export: {:?}",
            m1.difference(&m2)
        );
        // A fresh Virtuoso with the reference libraries, load, dump again.
        let mut i = with_stdlib(&["analogLib", "basic", "pdk", "cb", "work"]);
        i.load("(SfyFixPdk)").unwrap();
        // The team's library already holds the cells (the first dump came from it).
        i.files.insert("/e2e/first.sfyx".into(), first.clone());
        let ok = i.load("(SfyImport \"/e2e/first.sfyx\")").unwrap();
        assert!(
            matches!(ok, V::T),
            "{name}: loading the original dump failed"
        );
        i.files.insert("/e2e/second.sfyx".into(), second.clone());
        let ok = i
            .load("(SfyImport \"/e2e/second.sfyx\" ?overwrite t)")
            .unwrap();
        let log = i
            .files
            .get("/e2e/second.sfyx.log")
            .cloned()
            .unwrap_or_default();
        assert!(
            matches!(ok, V::T),
            "{name}: SfyImport of the Rust export failed:\n{log}\n{second}"
        );
        let list: Vec<String> = cells.iter().map(|c| format!("\"{c}\"")).collect();
        let ok = i
            .load(&format!(
                "(SfyExport \"/e2e/third.sfyx\" \"{lib}\" (list {}))",
                list.join(" ")
            ))
            .unwrap();
        assert!(matches!(ok, V::T), "{name}: SfyExport failed");
        let third = i.files["/e2e/third.sfyx"].clone();
        let (a, b) = (
            without_schemify_props(&first),
            without_schemify_props(&third),
        );
        if a != b {
            let diff: Vec<String> = a
                .lines()
                .zip(b.lines())
                .filter(|(x, y)| x != y)
                .take(6)
                .map(|(x, y)| format!("- {x}\n+ {y}"))
                .collect();
            panic!(
                "{name}: the Virtuoso dump changed after Rust import/export:\n{}\n(lines {} vs {})",
                diff.join("\n"),
                a.lines().count(),
                b.lines().count()
            );
        }
    }
}

/// §7.5 negative space: a Schemify-side edit clears `schemifyKeep` for that cellview only,
/// so the edit reaches Virtuoso while the untouched symbol (and its CDF) stays as it was.
#[test]
fn an_edit_in_schemify_reaches_virtuoso_and_untouched_views_stay() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skill");
    let first = std::fs::read_to_string(dir.join("inv.sfyx")).unwrap();
    let imp = virtuoso::import(
        &first,
        &Files::new(),
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap();
    let mut files: Files = imp.files.into_iter().collect();
    let path = "work/inv.shcomp".to_owned();
    let edited = String::from_utf8(files[&path].clone())
        .unwrap()
        .replace("  w 2u\n", "  w 3u\n");
    assert!(
        edited.contains("w 3u"),
        "fixture changed: no `w 2u` override"
    );
    files.insert(path.clone(), edited.into_bytes());
    let d = load_tops(&files, &[path.as_str()]);
    let eo = ExportOptions {
        producer: "e2e".into(),
        ..ExportOptions::default()
    };
    let second = virtuoso::export(&d, &[0], &Mapping::default(), &eo)
        .unwrap()
        .sfyx;
    // Only the symbol is marked kept.
    let keeps: Vec<&str> = second
        .lines()
        .filter(|l| l.contains("schemifyKeep"))
        .collect();
    assert_eq!(keeps.len(), 1, "{second}");
    let mut i = with_stdlib(&["analogLib", "basic", "work"]);
    i.files.insert("/e2e/first.sfyx".into(), first.clone());
    assert!(matches!(
        i.load("(SfyImport \"/e2e/first.sfyx\")").unwrap(),
        V::T
    ));
    i.files.insert("/e2e/second.sfyx".into(), second);
    assert!(matches!(
        i.load("(SfyImport \"/e2e/second.sfyx\" ?overwrite t)")
            .unwrap(),
        V::T
    ));
    let log = &i.files["/e2e/second.sfyx.log"];
    assert!(
        log.contains("count kept unchanged 1") && log.contains("count kept unchanged CDF 1"),
        "{log}"
    );
    i.load("(SfyExport \"/e2e/third.sfyx\" \"work\" (list \"inv\"))")
        .unwrap();
    let third = &i.files["/e2e/third.sfyx"];
    assert!(
        third.contains("\"w\" \"string\" \"3u\""),
        "the edit did not reach Virtuoso:\n{third}"
    );
    // The symbol section is byte-identical to the original dump's.
    let sym = |t: &str| -> String {
        let mut on = false;
        t.lines()
            .filter(|l| {
                if l.starts_with("(\"cv\"") {
                    on = l.contains("\"symbol\"");
                }
                on && !l.contains("schemify")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(sym(third), sym(&first));
}

/// The reverse stash of the CDF (§7.5): a component from Virtuoso goes back with its CDF
/// exactly as Virtuoso had it while its interface is unchanged, and with a CDF derived from
/// the cell once a param changed in Schemify.
#[test]
fn an_unedited_component_keeps_its_virtuoso_cdf_and_an_edit_regenerates_it() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skill");
    let first = std::fs::read_to_string(dir.join("netx.sfyx")).unwrap();
    let cdf_of = |text: &str| -> Vec<String> {
        let mut inside = false;
        let mut out = Vec::new();
        for r in sfyx::read(text).unwrap() {
            if r.tag == "cell" {
                inside = r.str(0) == Ok("netx");
            }
            if inside && matches!(r.tag.as_str(), "cdf" | "param" | "siminfo") {
                let mut s = String::new();
                sfyx::write_record(&r, &mut s);
                out.push(s);
            }
        }
        out
    };
    let imp = virtuoso::import(
        &first,
        &Files::new(),
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap();
    let mut files: Files = imp.files.into_iter().collect();
    let export = |files: &Files| {
        let d = load_tops(files, &["work/netx.shcomp"]);
        let eo = ExportOptions {
            producer: "e2e".into(),
            ..ExportOptions::default()
        };
        virtuoso::export(&d, &[0], &Mapping::default(), &eo)
            .unwrap()
            .sfyx
    };
    let original = cdf_of(&first);
    assert!(!original.is_empty(), "netx has a CDF");
    assert_eq!(
        cdf_of(&export(&files)),
        original,
        "unchanged: Virtuoso's CDF"
    );
    // A param edited in Schemify: the CDF is derived again, with the new default.
    let path = "work/netx.shcomp".to_owned();
    let edited = String::from_utf8(files[&path].clone())
        .unwrap()
        .replace("param gain 2", "param gain 3");
    files.insert(path, edited.into_bytes());
    let regenerated = cdf_of(&export(&files));
    assert_ne!(regenerated, original);
    assert!(
        regenerated
            .iter()
            .any(|r| r.contains("\"gain\"") && r.contains("\"3\"")),
        "{regenerated:#?}"
    );
}
