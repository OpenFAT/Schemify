//! Both sides of the Virtuoso bridge together (spec `interop.md` §7.4): the Rust exporter's
//! `.sfyx` is loaded by the shipped SKILL (`SfyImport`, in the test interpreter, into the
//! mock Virtuoso), dumped back by the shipped SKILL (`SfyExport`), and imported by the Rust
//! importer; the cell files must come back byte-identical. This is the only test where a
//! file written by one side is read by the other in both directions, so it is the one that
//! catches disagreements about record forms, ordering, units and stashes.

mod skill;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use interop::design::{Design, Files, LoadError, load};
use interop::map::Mapping;
use interop::sfyx;
use interop::virtuoso::{self, ExportOptions, ImportOptions};
use skill::mockdb::MockDb;
use skill::{Interp, V};

const BUILD: &str = include_str!("fixtures/skill/build.il");

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_dir(dir: &Path) -> Files {
    fn walk(base: &Path, d: &Path, out: &mut Files) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = Files::new();
    walk(dir, dir, &mut out);
    out
}

fn is_cell(p: &str) -> bool {
    [".shprim", ".shcomp", ".shbench"]
        .iter()
        .any(|e| p.ends_with(e))
}

fn design(files: &Files) -> Design {
    let tops: Vec<&str> = files
        .keys()
        .map(String::as_str)
        .filter(|p| is_cell(p))
        .collect();
    let mut absent = BTreeSet::new();
    loop {
        match load(files, &absent, &tops) {
            Ok(d) => return d,
            Err(LoadError::Missing(m)) => absent.extend(m),
            Err(LoadError::Invalid(p)) => panic!("{p:?}"),
        }
    }
}

fn canonical(bytes: &[u8]) -> String {
    schemify_format::write(&schemify_format::parse(bytes).unwrap())
}

/// Runs one fixture project through Rust export → SKILL import → SKILL export → Rust
/// import and returns (original cells, round-tripped cells, SKILL logs).
fn round_trip(dir: &Path) -> (BTreeMap<String, String>, BTreeMap<String, String>, String) {
    let files = read_dir(dir);
    let d = design(&files);
    let tops: Vec<usize> = (0..d.cells.len())
        .filter(|&k| d.cells[k].lib != "core")
        .collect();
    let mut eo = ExportOptions {
        producer: "e2e".into(),
        ..ExportOptions::default()
    };
    eo.lib_map.insert(String::new(), "top".into());
    let exported =
        virtuoso::export(&d, &tops, &Mapping::default(), &eo).unwrap_or_else(|e| panic!("{e:?}"));
    let recs = sfyx::read(&exported.sfyx).unwrap();

    // The SKILL side: every library the dump names exists in cds.lib, plus analogLib/basic.
    let mut i = Interp::new(MockDb::default());
    let mut libs: BTreeSet<String> = ["analogLib".into(), "basic".into()].into();
    for r in &recs {
        if r.tag == "lib" {
            libs.insert(r.str(0).unwrap().to_owned());
        }
    }
    for l in &libs {
        i.host.add_lib(l);
    }
    i.load(&interop::skill::schemify_il())
        .expect("schemify.il loads");
    i.load(BUILD).expect("build.il loads");
    i.load("(SfyFixStdlib)").expect("stdlib fixtures");
    i.files.insert("/e2e/in.sfyx".into(), exported.sfyx.clone());
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
    let design_libs: BTreeSet<String> = {
        let mut cur = String::new();
        let mut out = BTreeSet::new();
        for r in &recs {
            match r.tag.as_str() {
                "lib" => cur = r.str(0).unwrap().to_owned(),
                "cell" if r.str(1).unwrap() == "design" => {
                    out.insert(cur.clone());
                }
                _ => {}
            }
        }
        out
    };
    let manifest: Files = files
        .iter()
        .filter(|(p, _)| *p == "schemify.project")
        .map(|(p, b)| (p.clone(), b.clone()))
        .collect();
    let mut io = ImportOptions::default();
    io.lib_map.insert("top".into(), String::new());
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
                let t: String = String::from_utf8(b)
                    .unwrap()
                    .lines()
                    .filter(|l| !l.starts_with("  vsym ") && !l.starts_with("  vsch "))
                    .map(|l| format!("{l}\n"))
                    .collect();
                got.insert(p, t);
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
    let files = read_dir(&dir);
    let d = design(&files);
    let tops: Vec<usize> = (0..d.cells.len())
        .filter(|&k| d.cells[k].lib != "core")
        .collect();
    let mut eo = ExportOptions {
        producer: "e2e".into(),
        ..ExportOptions::default()
    };
    eo.lib_map.insert(String::new(), "top".into());
    let text = virtuoso::export(&d, &tops, &Mapping::default(), &eo)
        .unwrap()
        .sfyx;
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
    let mut i = Interp::new(MockDb::default());
    for r in &recs {
        if r.tag == "lib" {
            i.host.add_lib(r.str(0).unwrap());
        }
    }
    i.host.add_lib("analogLib");
    i.host.add_lib("basic");
    i.load(&interop::skill::schemify_il()).unwrap();
    i.load(BUILD).unwrap();
    i.load("(SfyFixStdlib)").unwrap();
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
        let mut absent = BTreeSet::new();
        let d = loop {
            match load(&files, &absent, &refs) {
                Ok(d) => break d,
                Err(LoadError::Missing(m)) => absent.extend(m),
                Err(LoadError::Invalid(p)) => panic!("{name}: {p:?}"),
            }
        };
        let top_ix: Vec<usize> = (0..tops.len()).collect();
        let eo = ExportOptions {
            producer: "e2e".into(),
            ..ExportOptions::default()
        };
        let second = virtuoso::export(&d, &top_ix, &Mapping::default(), &eo)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"))
            .sfyx;
        // A fresh Virtuoso with the reference libraries, load, dump again.
        let mut i = Interp::new(MockDb::default());
        for l in ["analogLib", "basic", "pdk", "cb", "work"] {
            i.host.add_lib(l);
        }
        i.load(&interop::skill::schemify_il()).unwrap();
        i.load(BUILD).unwrap();
        i.load("(SfyFixStdlib)").unwrap();
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
    let mut absent = BTreeSet::new();
    let d = loop {
        match load(&files, &absent, &[path.as_str()]) {
            Ok(d) => break d,
            Err(LoadError::Missing(m)) => absent.extend(m),
            Err(LoadError::Invalid(p)) => panic!("{p:?}"),
        }
    };
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
    let mut i = Interp::new(MockDb::default());
    for l in ["analogLib", "basic", "work"] {
        i.host.add_lib(l);
    }
    i.load(&interop::skill::schemify_il()).unwrap();
    i.load(BUILD).unwrap();
    i.load("(SfyFixStdlib)").unwrap();
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
