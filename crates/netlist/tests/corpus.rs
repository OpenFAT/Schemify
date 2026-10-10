//! The xschem differential oracle (TESTING.md, netlist row), gated on `SCHEMIFY_CORPUS`
//! (the `tests/corpus/.cache` directory: `src/` from `fetch.sh`, `golden/` from
//! `golden.sh`). Without it the test prints a skip note and passes.
//!
//! For every bench of `tests/corpus/benches.txt`: import its library group with
//! `schemify_import` (the groups and search paths of `import_all.sh`), elaborate the
//! bench's cell (the bench copy `<stem>__top.shbench`, else `<stem>.shbench`, else the
//! component), emit ngspice and cdl, and compare each with xschem's golden under
//! `schemify_spicecmp::compare`. Prints the ratchet table (`tests/corpus/RATCHET.md`)
//! and writes the imported files, our netlists and the diffs to
//! `$SCHEMIFY_CORPUS/netlist-out/`.
//!
//! `SCHEMIFY_CORPUS_FILTER=<substring>` restricts the benches (for working one down).

use schemify_doc::{DocHandle, Project};
use schemify_elab::{Dialect, Request, elaborate, no_funcs};
use schemify_import::{Options, core_library, tcl, translate_library};
use schemify_netlist::emit;
use schemify_spicecmp::{Options as CmpOptions, Section, compare};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One xschem rcfile group of `golden.sh`: search path (converted or root-only),
/// xschemrc files, and the PDK environment the goldens were netlisted with.
struct Group {
    name: &'static str,
    path: Vec<(PathBuf, bool)>,
    rcs: Vec<PathBuf>,
    env: Vec<(&'static str, String)>,
}

fn groups(c: &Path) -> Vec<Group> {
    let src = c.join("src");
    let l = src.join("xschem/xschem_library");
    let p = src.join("ihp_open_pdk/ihp-sg13g2");
    let xs = [
        "devices",
        "examples",
        "ngspice",
        "ngspice_verilog_cosim",
        "logic",
        "xschem_simulator",
        "generators",
        "inst_sch_select",
        "binto7seg",
        "pcb",
        "rom8k",
    ];
    let s = |p: &Path| p.to_string_lossy().into_owned();
    vec![
        Group {
            name: "xschem",
            path: xs.iter().map(|d| (l.join(d), true)).collect(),
            rcs: vec![],
            env: vec![],
        },
        Group {
            name: "sky130",
            path: vec![
                (l.join("devices"), true),
                (l.clone(), false),
                (src.join("xschem_sky130"), true),
            ],
            rcs: vec![src.join("xschem_sky130/xschemrc")],
            env: vec![("PDK_ROOT", s(&c.join("pdk"))), ("PDK", "sky130A".into())],
        },
        Group {
            name: "gf180",
            path: vec![
                (l.join("devices"), true),
                (l.clone(), false),
                (src.join("gf180mcu_fd_pr/cells/xschem"), true),
            ],
            rcs: vec![src.join("gf180mcu_fd_pr/cells/xschem/xschemrc")],
            env: vec![("PDK_ROOT", s(&src.join("gf180mcu_fd_pr")))],
        },
        Group {
            name: "ihp",
            path: vec![
                (l.join("devices"), true),
                (l.clone(), false),
                (p.join("libs.tech/xschem"), true),
                (p.join("libs.tech/xschem/sg13g2_pr"), false),
                (p.join("libs.ref/sg13g2_stdcell/sym/xschem"), true),
                (p.join("libs.ref/sg13g2_stdcell/sch/xschem"), true),
            ],
            rcs: vec![
                p.join("libs.tech/xschem/xschemrc"),
                p.join("libs.tech/xschem/xschem-drc"),
            ],
            env: vec![
                ("PDK_ROOT", s(&src.join("ihp_open_pdk"))),
                ("PDK", "ihp-sg13g2".into()),
            ],
        },
    ]
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut v: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    v.sort();
    for p in v {
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "sym" || x == "sch") {
            out.push(p);
        }
    }
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Translates a group the way `devicereader` does (search path, PDK vars, drc procs).
fn import(g: &Group) -> Vec<(String, Vec<u8>)> {
    let mut roots = Vec::new();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for (d, convert) in &g.path {
        let d = std::fs::canonicalize(d).unwrap_or(d.clone());
        roots.push(slash(&d));
        if *convert {
            let mut v = Vec::new();
            walk(&d, &mut v);
            files.extend(v.into_iter().map(|p| (slash(&p), std::fs::read(&p).unwrap())));
        }
    }
    files.sort();
    files.dedup_by(|a, b| a.0 == b.0);
    let (mut vars, mut procs) = (Vec::new(), Vec::new());
    for rc in &g.rcs {
        let text = std::fs::read_to_string(rc).unwrap_or_default();
        for v in tcl::pdk_vars(&text) {
            if !vars.iter().any(|(n, _): &(String, String)| *n == v.0) {
                vars.push(v);
            }
        }
        procs.extend(tcl::drc_procs(&text));
    }
    let opts = Options {
        roots: &roots,
        pdk_vars: &vars,
        drc: &procs,
        ..Options::default()
    };
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    translate_library(&refs, &opts).0
}

/// Loads translated outputs into a project (core library included), every cell open.
fn load(out: &[(String, Vec<u8>)]) -> (Project, BTreeMap<String, DocHandle>) {
    let manifest = out.iter().find(|f| f.0 == "schemify.project").map(|f| f.1.as_slice());
    let mut p = Project::new(manifest, None, 7).expect("manifest parses");
    let mut libs: BTreeMap<&str, Vec<(&str, &[u8])>> = BTreeMap::new();
    libs.insert("core", core_library().to_vec());
    for (path, bytes) in out {
        if let Some((lib, rel)) = path.split_once('/')
            && [".shprim", ".shcomp", ".shbench"].iter().any(|e| rel.ends_with(e))
        {
            libs.entry(lib).or_default().push((rel, bytes));
        }
    }
    let mut handles = BTreeMap::new();
    for (lib, listing) in &libs {
        p.index_library(lib, "import", listing).expect("library in manifest");
    }
    for (lib, listing) in &libs {
        for (rel, bytes) in listing {
            if let Ok(o) = p.open(lib, rel, bytes, None) {
                handles.insert(format!("{lib}/{rel}"), o.handle);
            }
        }
    }
    (p, handles)
}

/// The cell file netlisted for a bench (`import_all.sh`'s rule).
fn bench_cell(handles: &BTreeMap<String, DocHandle>, path: &str) -> Option<String> {
    let stem = Path::new(path).file_stem()?.to_str()?;
    let dir = Path::new(path).parent()?.file_name()?.to_str()?;
    for want in [format!("{stem}__top.shbench"), format!("{stem}.shbench"), format!("{stem}.shcomp")] {
        let hits: Vec<&String> = handles
            .keys()
            .filter(|k| k.rsplit_once('/').is_some_and(|(_, f)| f == want))
            .collect();
        if let Some(h) = hits.iter().find(|k| k.starts_with(&format!("{dir}/"))).or(hits.first()) {
            return Some((*h).clone());
        }
    }
    None
}

#[derive(Default)]
struct Tally {
    agree: u32,
    differ: u32,
    no_import: u32,
    no_elab: u32,
    /// Differences confined to sections other than connectivity.
    conn_agree: u32,
}

#[test]
fn xschem_differential() {
    let Some(cache) = std::env::var_os("SCHEMIFY_CORPUS").map(PathBuf::from) else {
        eprintln!("SCHEMIFY_CORPUS unset: xschem differential skipped");
        return;
    };
    let cache = std::fs::canonicalize(&cache).expect("corpus cache exists");
    let filter = std::env::var("SCHEMIFY_CORPUS_FILTER").unwrap_or_default();
    let list = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/benches.txt"),
    )
    .unwrap();
    let benches: Vec<(&str, &str, &str)> = list
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            Some((w.next()?, w.next()?, w.next()?))
        })
        .filter(|b| b.0.contains(&filter))
        .collect();
    let outdir = cache.join("netlist-out");
    std::fs::create_dir_all(&outdir).unwrap();
    let ours = slash(&cache);
    let mut tally: BTreeMap<(String, &str), Tally> = BTreeMap::new();
    let mut lines = Vec::new();
    for g in groups(&cache) {
        let mine: Vec<&(&str, &str, &str)> = benches.iter().filter(|b| b.1 == g.name).collect();
        if mine.is_empty() {
            continue;
        }
        let t0 = std::time::Instant::now();
        let out = import(&g);
        for (rel, bytes) in &out {
            let path = outdir.join("import").join(g.name).join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let (project, handles) = load(&out);
        eprintln!("{}: imported {} files in {:?}", g.name, out.len(), t0.elapsed());
        let env: Vec<(&str, &str)> = g.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        for &&(name, _, path) in &mine {
            for (mode, dialect) in [("spice", Dialect::Ngspice), ("cdl", Dialect::Cdl)] {
                let t = tally.entry((g.name.to_owned(), mode)).or_default();
                let Some(cell) = bench_cell(&handles, path) else {
                    t.no_import += 1;
                    lines.push(format!("{name} {mode} failed-to-import"));
                    continue;
                };
                let req = Request {
                    config: None,
                    corner: None,
                    overrides: &[],
                    env: &env,
                    dialect,
                };
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let e = elaborate(&project, handles[&cell], &req, &mut no_funcs);
                    let blocking: Vec<String> = e
                        .violations
                        .rows
                        .iter()
                        .filter(|v| v.code.severity() == schemify_elab::Severity::Error)
                        .map(|v| format!("{} {}", v.code.as_str(), v.detail))
                        .collect();
                    (emit(&e, dialect).text, blocking)
                }));
                let Ok((text, blocking)) = run else {
                    t.no_elab += 1;
                    lines.push(format!("{name} {mode} panic"));
                    continue;
                };
                std::fs::write(outdir.join(format!("{name}.{mode}")), &text).unwrap();
                let golden_path = cache.join("golden").join(format!("{name}.{mode}"));
                let golden = std::fs::read_to_string(&golden_path).unwrap_or_default();
                // Goldens embed the absolute cache path of the checkout that made them.
                let theirs = golden
                    .find("/tests/corpus/.cache")
                    .and_then(|i| golden[..i].rfind(' ').map(|s| &golden[s + 1..i]))
                    .map(|p| format!("{p}/tests/corpus/.cache"));
                let mut aliases = vec![(ours.clone(), "$CORPUS".to_owned())];
                aliases.extend(theirs.map(|p| (p, "$CORPUS".to_owned())));
                let report = compare(&golden, &text, &CmpOptions { path_aliases: aliases });
                let diff: Vec<String> = report
                    .diffs
                    .iter()
                    .map(|d| format!("{:?} {}", d.section, d.text))
                    .collect();
                std::fs::write(outdir.join(format!("{name}.{mode}.diff")), diff.join("\n")).unwrap();
                if !blocking.is_empty() {
                    t.no_elab += 1;
                    lines.push(format!(
                        "{name} {mode} failed-to-elaborate ({} errors, first: {}) equal={}",
                        blocking.len(),
                        blocking[0],
                        report.is_equal()
                    ));
                } else if report.is_equal() {
                    t.agree += 1;
                } else {
                    t.differ += 1;
                    if report.diffs.iter().all(|d| d.section != Section::Connectivity) {
                        t.conn_agree += 1;
                    }
                    lines.push(format!("{name} {mode} differ: {}", diff.first().map_or("", |s| s)));
                }
            }
        }
    }
    let mut table = String::from(
        "| set | mode | agree | differ | (connectivity agrees) | failed-to-import | failed-to-elaborate |\n|---|---|---|---|---|---|---|\n",
    );
    let mut total = Tally::default();
    for ((set, mode), t) in &tally {
        table.push_str(&format!(
            "| {set} | {mode} | {} | {} | {} | {} | {} |\n",
            t.agree, t.differ, t.conn_agree, t.no_import, t.no_elab
        ));
        total.agree += t.agree;
        total.differ += t.differ;
        total.conn_agree += t.conn_agree;
        total.no_import += t.no_import;
        total.no_elab += t.no_elab;
    }
    table.push_str(&format!(
        "| total | | {} | {} | {} | {} | {} |\n",
        total.agree, total.differ, total.conn_agree, total.no_import, total.no_elab
    ));
    std::fs::write(outdir.join("RESULTS.txt"), lines.join("\n") + "\n").unwrap();
    std::fs::write(outdir.join("TABLE.md"), &table).unwrap();
    eprintln!("{}\n{table}", lines.join("\n"));
}
