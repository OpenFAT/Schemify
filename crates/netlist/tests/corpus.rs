//! The xschem differential oracle (TESTING.md, netlist row), gated on `SCHEMIFY_CORPUS`
//! (the `tests/corpus/.cache` directory: `src/` from `fetch.sh`, `golden/` from
//! `golden.sh`). Without it the test prints a skip note and passes.
//!
//! For every bench of `tests/corpus/benches.txt`: import its library group with
//! `schemify_import` (the groups and search paths of `import_all.sh`), elaborate the
//! bench's cell (see `bench_cell`), emit ngspice and cdl, and compare each with xschem's golden under
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
    // golden.sh runs xschem with HOME in the cache (`$USER_CONF_DIR` is `~/.xschem`).
    let home = ("HOME", s(&c.join("home")));
    vec![
        Group {
            name: "xschem",
            path: xs.iter().map(|d| (l.join(d), true)).collect(),
            rcs: vec![],
            env: vec![home.clone()],
        },
        Group {
            name: "sky130",
            path: vec![
                (l.join("devices"), true),
                (l.clone(), false),
                (src.join("xschem_sky130"), true),
            ],
            rcs: vec![src.join("xschem_sky130/xschemrc")],
            env: vec![
                ("PDK_ROOT", s(&c.join("pdk"))),
                ("PDK", "sky130A".into()),
                home.clone(),
            ],
        },
        Group {
            name: "gf180",
            path: vec![
                (l.join("devices"), true),
                (l.clone(), false),
                (src.join("gf180mcu_fd_pr/cells/xschem"), true),
            ],
            rcs: vec![src.join("gf180mcu_fd_pr/cells/xschem/xschemrc")],
            env: vec![("PDK_ROOT", s(&src.join("gf180mcu_fd_pr"))), home.clone()],
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
                home,
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
        } else if p
            .extension()
            .is_some_and(|x| x == "sym" || x == "sch" || x == "cir")
        {
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
            files.extend(
                v.into_iter()
                    .map(|p| (slash(&p), std::fs::read(&p).unwrap())),
            );
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
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .collect();
    translate_library(&refs, &opts).0
}

/// Loads translated outputs into a project (core library included), every cell open.
fn load(out: &[(String, Vec<u8>)]) -> (Project, BTreeMap<String, DocHandle>) {
    let manifest = out
        .iter()
        .find(|f| f.0 == "schemify.project")
        .map(|f| f.1.as_slice());
    let mut p = Project::new(manifest, None, 7).expect("manifest parses");
    let mut libs: BTreeMap<&str, Vec<(&str, &[u8])>> = BTreeMap::new();
    libs.insert("core", core_library().to_vec());
    for (path, bytes) in out {
        if let Some((lib, rel)) = path.split_once('/')
            && [".shprim", ".shcomp", ".shbench"]
                .iter()
                .any(|e| rel.ends_with(e))
        {
            libs.entry(lib).or_default().push((rel, bytes));
        }
    }
    let mut handles = BTreeMap::new();
    for (lib, listing) in &libs {
        p.index_library(lib, "import", listing)
            .expect("library in manifest");
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

/// The cell file netlisted for a bench: `import_all.sh`'s rule for SPICE (the bench
/// copy, else the bench, else the component); for CDL the component first, since LVS
/// checks the cell and its ports (the bench copy only adds simulation text, which CDL
/// drops anyway, spec §7.4).
fn bench_cell(handles: &BTreeMap<String, DocHandle>, path: &str, cdl: bool) -> Option<String> {
    let stem = Path::new(path).file_stem()?.to_str()?;
    let dir = Path::new(path).parent()?.file_name()?.to_str()?;
    let mut wants = [
        format!("{stem}__top.shbench"),
        format!("{stem}.shbench"),
        format!("{stem}.shcomp"),
    ];
    if cdl {
        wants.rotate_right(1);
    }
    for want in wants {
        let hits: Vec<&String> = handles
            .keys()
            .filter(|k| k.rsplit_once('/').is_some_and(|(_, f)| f == want))
            .collect();
        if let Some(h) = hits
            .iter()
            .find(|k| k.starts_with(&format!("{dir}/")))
            .or(hits.first())
        {
            return Some((*h).clone());
        }
    }
    None
}

/// Benches whose control blocks write `.raw` files and whose models are in the cache
/// (xschem's self-contained examples; gf180 with its fetched ngspice models; sky130 with
/// the models the nightly fetches with volare into `pdk/sky130A`): the second oracle runs
/// ngspice on ours and on xschem's golden.
const SIMULATED: [(&str, &str); 12] = [
    ("xschem", "xs-ex-rlc"),
    ("xschem", "xs-ex-cmos_example"),
    ("xschem", "xs-ex-test_jfet"),
    ("xschem", "xs-ng-hpf_xschem"),
    ("xschem", "xs-ng-colpitts_xschem"),
    ("gf180", "gf180-test_nfet_03v3"),
    ("gf180", "gf180-test_pfet_03v3"),
    ("gf180", "gf180-test_npolyf_u"),
    ("gf180", "gf180-test_cap_mim_1f0fF"),
    ("gf180", "gf180-test_diode_nd2ps_03v3"),
    ("sky130", "sky130-n_diffamp"),
    ("sky130", "sky130-test_diode"),
];

/// Runs `ngspice -b` on `netlist` in a fresh `dir`; returns the `.raw` files it wrote,
/// by file name.
fn simulate(dir: &Path, netlist: &str) -> BTreeMap<String, Vec<u8>> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("netlist.spice"), netlist).unwrap();
    let out = std::process::Command::new("ngspice")
        .args(["-b", "-r", "batch.raw", "netlist.spice"])
        .current_dir(dir)
        .output()
        .expect("ngspice on PATH");
    std::fs::write(dir.join("ngspice.log"), [out.stdout, out.stderr].concat()).unwrap();
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "raw"))
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

/// Compares two raw files' plots: every variable present in both (case-insensitive
/// name) agrees within 1e-3 of its column's peak plus 1e-9, sampled at the golden's
/// scale points (ours linearly interpolated when the time steps differ). Returns the
/// number of compared columns.
fn same_results(golden: &[u8], ours: &[u8]) -> Result<usize, String> {
    let (g, o) = (
        schemify_wave::parse_raw(golden).map_err(|e| format!("golden: {e}"))?,
        schemify_wave::parse_raw(ours).map_err(|e| format!("ours: {e}"))?,
    );
    if g.len() != o.len() {
        return Err(format!("{} plots vs {}", g.len(), o.len()));
    }
    let mut compared = 0;
    for (pg, po) in g.iter().zip(&o) {
        let (xg, xo) = (pg.scale(), po.scale());
        for (k, v) in pg.vars().iter().enumerate().skip(1) {
            let Some(j) = po
                .vars()
                .iter()
                .position(|w| w.name.eq_ignore_ascii_case(&v.name))
            else {
                continue;
            };
            let (cg, co) = (pg.col(k), po.col(j));
            let peak = cg.iter().fold(0f64, |m, x| m.max(x.abs()));
            let tol = 1e-3 * peak + 1e-9;
            for (i, &x) in xg.iter().enumerate() {
                let y = if xg.len() == xo.len() {
                    co[i]
                } else {
                    let at = xo.partition_point(|t| *t < x).clamp(1, xo.len() - 1);
                    let (x0, x1) = (xo[at - 1], xo[at]);
                    let f = if x1 > x0 { (x - x0) / (x1 - x0) } else { 0.0 };
                    co[at - 1] + f * (co[at] - co[at - 1])
                };
                if (cg[i] - y).abs() > tol {
                    return Err(format!("{} at {x}: golden {} ours {y}", v.name, cg[i]));
                }
            }
            compared += 1;
        }
    }
    Ok(compared)
}

/// The second oracle (TESTING.md): ngspice results of our netlist and of xschem's
/// agree within tolerance. Needs `SCHEMIFY_CORPUS` and ngspice on PATH.
#[test]
#[ignore]
fn ngspice_results_agree() {
    let Some(cache) = std::env::var_os("SCHEMIFY_CORPUS").map(PathBuf::from) else {
        eprintln!("SCHEMIFY_CORPUS unset: ngspice oracle skipped");
        return;
    };
    let cache = std::fs::canonicalize(&cache).unwrap();
    let list = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/benches.txt"),
    )
    .unwrap();
    // gf180 benches name the models `*.ngspice`; the repository ships `*.spice`
    // (open_pdks renames them, tests/corpus/README.md).
    let models = cache.join("src/gf180mcu_fd_pr/models/ngspice");
    for stem in ["design", "sm141064"] {
        let to = models.join(format!("{stem}.ngspice"));
        if !to.exists() {
            std::fs::copy(models.join(format!("{stem}.spice")), &to).unwrap();
        }
    }
    let ours_root = slash(&cache);
    let mut failures = Vec::new();
    for g in groups(&cache) {
        let mine: Vec<&str> = SIMULATED
            .iter()
            .filter(|s| s.0 == g.name)
            .map(|s| s.1)
            .collect();
        if mine.is_empty() {
            continue;
        }
        let (project, handles) = load(&import(&g));
        let env: Vec<(&str, &str)> = g.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        for name in mine {
            let path = list
                .lines()
                .find_map(|l| l.strip_prefix(name).filter(|r| r.starts_with(' ')))
                .and_then(|r| r.split_whitespace().nth(1))
                .expect("bench listed");
            let cell = bench_cell(&handles, path, false).expect("imported");
            let req = Request {
                env: &env,
                ..request(Dialect::Ngspice)
            };
            let e = elaborate(&project, handles[&cell], &req, &mut no_funcs);
            let ours = emit(&e, Dialect::Ngspice).text;
            let golden =
                std::fs::read_to_string(cache.join(format!("golden/{name}.spice"))).unwrap();
            // The golden's absolute cache paths point into the checkout that made it.
            let golden = match golden.find("/tests/corpus/.cache") {
                Some(i) => {
                    let from = golden[..i].rfind(' ').map_or(0, |s| s + 1);
                    let theirs = format!("{}/tests/corpus/.cache", &golden[from..i]);
                    golden.replace(&theirs, &ours_root)
                }
                None => golden,
            };
            let dir = cache.join("netlist-out/sim").join(name);
            let (rg, ro) = (
                simulate(&dir.join("golden"), &golden),
                simulate(&dir.join("ours"), &ours),
            );
            if rg.is_empty() {
                failures.push(format!("{name}: golden wrote no raw file"));
                continue;
            }
            for (file, bytes) in &rg {
                match ro.get(file).map(|o| same_results(bytes, o)) {
                    Some(Ok(n)) if n > 0 => eprintln!("{name}/{file}: {n} columns agree"),
                    Some(Ok(_)) => failures.push(format!("{name}/{file}: no common vector")),
                    Some(Err(why)) => failures.push(format!("{name}/{file}: {why}")),
                    None => failures.push(format!("{name}: ours wrote no {file}")),
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

fn request(dialect: Dialect) -> Request<'static> {
    Request {
        config: None,
        corner: None,
        overrides: &[],
        env: &[],
        dialect,
    }
}

#[derive(Default)]
struct Tally {
    agree: u32,
    differ: u32,
    no_import: u32,
    no_elab: u32,
    /// Differences confined to sections other than connectivity.
    conn_agree: u32,
    /// CDL agreement against the golden without xschem's user-architecture-code blocks
    /// (spec §7.4: CDL carries no code; xschem's lvs netlist keeps it).
    stripped_agree: u32,
    /// Differences that vanish once the golden's undefined parameter names are bound
    /// (`spicecmp` `bind_free`): xschem left a top-level component's parameters
    /// unevaluated. Counted apart from agreement (SPICE: against the golden; CDL:
    /// against the code-stripped golden).
    unevaluated: u32,
}

/// The golden without its `**** begin/end user architecture code` blocks.
fn strip_code(golden: &str) -> String {
    let mut out = String::with_capacity(golden.len());
    let mut inside = false;
    for line in golden.lines() {
        if line.starts_with("**** begin user architecture code") {
            inside = true;
        } else if line.starts_with("**** end user architecture code") {
            inside = false;
        } else if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
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
        eprintln!(
            "{}: imported {} files in {:?}",
            g.name,
            out.len(),
            t0.elapsed()
        );
        let env: Vec<(&str, &str)> = g.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        for &&(name, _, path) in &mine {
            for (mode, dialect) in [("spice", Dialect::Ngspice), ("cdl", Dialect::Cdl)] {
                let t = tally.entry((g.name.to_owned(), mode)).or_default();
                let Some(cell) = bench_cell(&handles, path, dialect == Dialect::Cdl) else {
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
                let opts = CmpOptions {
                    path_aliases: aliases,
                    ..CmpOptions::default()
                };
                let report = compare(&golden, &text, &opts);
                let stripped_report =
                    (mode == "cdl").then(|| compare(&strip_code(&golden), &text, &opts));
                let stripped = stripped_report.as_ref().is_some_and(|r| r.is_equal());
                // Bound comparison against what this mode is judged on.
                let reference = if mode == "cdl" {
                    strip_code(&golden)
                } else {
                    golden.clone()
                };
                let bound = (!report.is_equal() && !stripped).then(|| {
                    let o = CmpOptions {
                        bind_free: true,
                        ..opts.clone()
                    };
                    compare(&reference, &text, &o)
                });
                let unevaluated = bound.as_ref().is_some_and(|r| r.is_equal());
                // The diff file of a cdl bench is against the code-stripped golden.
                let diff: Vec<String> = stripped_report
                    .as_ref()
                    .unwrap_or(&report)
                    .diffs
                    .iter()
                    .map(|d| format!("{:?} {}", d.section, d.text))
                    .collect();
                std::fs::write(outdir.join(format!("{name}.{mode}.diff")), diff.join("\n"))
                    .unwrap();
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
                    t.stripped_agree += u32::from(mode == "cdl");
                } else if unevaluated {
                    t.unevaluated += 1;
                    lines.push(format!(
                        "{name} {mode} golden-unevaluated: {}",
                        bound
                            .as_ref()
                            .map_or(String::new(), |r| r.bindings.join(" "))
                    ));
                } else {
                    t.stripped_agree += u32::from(stripped);
                    t.differ += 1;
                    if report
                        .diffs
                        .iter()
                        .all(|d| d.section != Section::Connectivity)
                    {
                        t.conn_agree += 1;
                    }
                    lines.push(format!(
                        "{name} {mode} differ: {}",
                        diff.first().map_or("", |s| s)
                    ));
                }
            }
        }
    }
    let mut table = String::from(
        "| set | mode | agree | golden-unevaluated | differ | (connectivity agrees) | cdl agree, golden code stripped | failed-to-import | failed-to-elaborate |\n|---|---|---|---|---|---|---|---|---|\n",
    );
    let mut total = Tally::default();
    for ((set, mode), t) in &tally {
        table.push_str(&format!(
            "| {set} | {mode} | {} | {} | {} | {} | {} | {} | {} |\n",
            t.agree,
            t.unevaluated,
            t.differ,
            t.conn_agree,
            if *mode == "cdl" {
                t.stripped_agree.to_string()
            } else {
                "-".into()
            },
            t.no_import,
            t.no_elab
        ));
        total.stripped_agree += t.stripped_agree;
        total.unevaluated += t.unevaluated;
        total.agree += t.agree;
        total.differ += t.differ;
        total.conn_agree += t.conn_agree;
        total.no_import += t.no_import;
        total.no_elab += t.no_elab;
    }
    table.push_str(&format!(
        "| total | | {} | {} | {} | {} | {} | {} | {} |\n",
        total.agree,
        total.unevaluated,
        total.differ,
        total.conn_agree,
        total.stripped_agree,
        total.no_import,
        total.no_elab
    ));
    std::fs::write(outdir.join("RESULTS.txt"), lines.join("\n") + "\n").unwrap();
    std::fs::write(outdir.join("TABLE.md"), &table).unwrap();
    eprintln!("{}\n{table}", lines.join("\n"));
}
