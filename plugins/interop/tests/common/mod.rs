//! What the Virtuoso-bridge tests share: project files read from disk, whole-project loads,
//! and the canonical forms their oracles compare. The SKILL side's shared state is
//! `skill::virtuoso`.

#![allow(dead_code)] // each test binary uses its own subset

use std::path::{Path, PathBuf};

use interop::design::{Design, Files, load_closed};
use interop::map::Mapping;
use interop::sfyx;
use interop::virtuoso::{self, ExportOptions, ImportOptions};

/// The repository root.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every file under `dir`, keyed by its `/`-separated path relative to `dir`. A directory
/// that does not exist (or cannot be read) contributes nothing: the live kit reads work
/// directories a step may not have written yet.
pub fn read_tree(dir: &Path) -> Files {
    fn walk(base: &Path, d: &Path, out: &mut Files) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
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

/// The files of fixture project `dir`.
///
/// # Panics
///
/// When `dir` holds no cell file, so a moved fixture fails its test instead of passing
/// over nothing.
pub fn project(dir: &Path) -> Files {
    let files = read_tree(dir);
    assert!(
        files.keys().any(|p| is_cell(p)),
        "{}: no cell files",
        dir.display()
    );
    files
}

/// Whether `p` names a Schemify cell file.
pub fn is_cell(p: &str) -> bool {
    [".shprim", ".shcomp", ".shbench"]
        .iter()
        .any(|e| p.ends_with(e))
}

/// The closure of `tops` over `files` alone.
///
/// # Panics
///
/// With the load problems when the closure does not load.
pub fn load_tops(files: &Files, tops: &[&str]) -> Design {
    load_closed(files, tops).unwrap_or_else(|p| panic!("{tops:?}: {p:?}"))
}

/// Every cell file of `files`, loaded.
pub fn load_all(files: &Files) -> Design {
    let tops: Vec<&str> = files
        .keys()
        .map(String::as_str)
        .filter(|p| is_cell(p))
        .collect();
    load_tops(files, &tops)
}

/// Canonical text of a cell file (what the format writer produces).
pub fn canonical(bytes: &[u8]) -> String {
    let d = schemify_format::parse(bytes)
        .unwrap_or_else(|e| panic!("{e:?}\n{}", String::from_utf8_lossy(bytes)));
    schemify_format::write(&d)
}

/// `text` without the import fingerprints (`vsym`, `vsch` header lines, spec §7.5), which
/// an import adds by design.
pub fn strip_fp(text: &str) -> String {
    text.lines()
        .filter(|l| !l.starts_with("  vsym ") && !l.starts_with("  vsch "))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// `.sfyx` text without its stash properties (`schemifyRecords<k>`).
pub fn strip_stash(text: &str) -> String {
    let kept: Vec<sfyx::Record> = sfyx::read(text)
        .unwrap()
        .into_iter()
        .filter(|r| !(r.tag == "prop" && r.str(1).is_ok_and(|n| n.starts_with("schemifyRecords"))))
        .collect();
    sfyx::write(&kept)
}

/// Where a dump record sits (spec §4.2 nesting): the library, cell and cellview it follows,
/// whether that cell is a `"design"` one, and whether the maestro records have begun. Empty
/// before the first of each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct At {
    pub lib: String,
    pub cell: String,
    pub design: bool,
    pub view: String,
    pub maestro: bool,
}

/// Every record of `recs` with the place it sits in, so a check over "the pin figures of
/// symbols" or "the records of design cells" is a filter, not another walk.
pub fn located(recs: &[sfyx::Record]) -> Vec<(At, &sfyx::Record)> {
    let mut at = At::default();
    let mut out = Vec::with_capacity(recs.len());
    for r in recs {
        let name = || r.str(0).unwrap_or_default().to_owned();
        match r.tag.as_str() {
            "lib" => {
                at = At {
                    lib: name(),
                    ..At::default()
                }
            }
            "cell" => {
                at.cell = name();
                at.design = r.str(1).is_ok_and(|k| k == "design");
                at.view.clear();
            }
            "cv" => at.view = name(),
            "maestro" => at.maestro = true,
            _ => {}
        }
        out.push((at.clone(), r));
    }
    assert_eq!(out.len(), recs.len());
    out
}

/// Libraries holding `"design"` cells of a dump, in file order (none when it does not read).
pub fn design_libs(text: &str) -> Vec<String> {
    let recs = sfyx::read(text).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    for (at, r) in located(&recs) {
        if r.tag == "cell" && at.design && !out.contains(&at.lib) {
            out.push(at.lib);
        }
    }
    out
}

/// One record as its `.sfyx` line, without the line end.
pub fn line_of(r: &sfyx::Record) -> String {
    let mut s = String::new();
    sfyx::write_record(r, &mut s);
    s.truncate(s.trim_end().len());
    s
}

/// The record of one `.sfyx` line (the inverse of [`line_of`]); `None` when it does not
/// read.
pub fn record_of(line: &str) -> Option<sfyx::Record> {
    sfyx::read(&format!("(\"sfyx\" 1 \"x\")\n{line}\n(\"end\")\n"))
        .ok()?
        .into_iter()
        .nth(1)
}

/// The `.sfyx` export of every non-core cell of `d` with producer `producer`, the project
/// root as Virtuoso library `top` (what [`import_opts`] maps back).
///
/// # Panics
///
/// With the export's problems when it refuses.
pub fn export_cells(d: &Design, producer: &str) -> String {
    let tops: Vec<usize> = (0..d.cells.len())
        .filter(|&k| d.cells[k].lib != "core")
        .collect();
    let mut opts = ExportOptions {
        producer: producer.into(),
        ..ExportOptions::default()
    };
    opts.lib_map.insert(String::new(), "top".into());
    virtuoso::export(d, &tops, &Mapping::default(), &opts)
        .unwrap_or_else(|e| panic!("{e:?}"))
        .sfyx
}

/// Import settings that map library `top` back to the project root ([`export_cells`]).
pub fn import_opts() -> ImportOptions {
    let mut o = ImportOptions::default();
    o.lib_map.insert("top".into(), String::new());
    o
}
