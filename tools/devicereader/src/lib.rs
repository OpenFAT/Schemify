//! `devicereader`: converts xschem libraries — a PDK's device library or whole design
//! trees — into Schemify libraries, with device limits from model cards, `drc=` procs
//! and tech files (D14), then checks every output with the import oracle.
//!
//! ```text
//! devicereader <lib-dir>... [--root <dir>]... [--models <file>[:<section>]]...
//!              [--spiceinit <file>] [--tech <file>] [--cdf <dump>] [--xschemrc <file>]...
//!              [--no-core] -o <out-dir>
//! ```
//!
//! Each `<lib-dir>` is one element of the xschem library search path, in the order
//! given, and every `.sym`/`.sch` under it is converted. `--root` adds a search-path
//! element whose files are only referenced (xschem's `xschem_library` parent, so that
//! `devices/res.sym` resolves) — they must also be under some `<lib-dir>`. `--xschemrc` (repeatable: pass
//! `xschem-drc` too) supplies PDK path variables and `drc` procs. `--spiceinit` reads an
//! ngspice `.spiceinit`: its `sourcepath` (`$VAR`s from the environment) is where model
//! includes are looked up. A Magic `--tech` file's `include`s are read beside it. Writes the outputs
//! (`<lib>/<cell>.<ext>`, `schemify.project`, `import-report.{md,txt}`) and prints a
//! summary. Exit status: 0 ok, 1 an output failed its check, 2 usage or I/O error.

use std::path::{Path, PathBuf};

use schemify_import::report::Class;
use schemify_import::{Options, check_output, spice, tcl, tech, translate_library};

struct Args {
    /// Search path in order: (dir, convert its files).
    dirs: Vec<(PathBuf, bool)>,
    models: Vec<(PathBuf, Option<String>)>,
    tech: Option<PathBuf>,
    spiceinit: Option<PathBuf>,
    cdf: Option<PathBuf>,
    rcs: Vec<PathBuf>,
    core: bool,
    out: PathBuf,
}

fn usage() -> u8 {
    eprintln!(
        "usage: devicereader <lib-dir>... [--root <dir>]... [--models <file>[:<section>]]... \
         [--spiceinit <file>] [--tech <file>] [--cdf <dump>] [--xschemrc <file>]... \
         [--no-core] -o <out-dir>"
    );
    2
}

fn parse_args(args: &[String]) -> Option<Args> {
    let mut a = Args {
        dirs: Vec::new(),
        models: Vec::new(),
        tech: None,
        spiceinit: None,
        cdf: None,
        rcs: Vec::new(),
        core: true,
        out: PathBuf::new(),
    };
    let mut it = args.iter().cloned();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--models" => {
                let v = it.next()?;
                let (path, section) = match v.rsplit_once(':') {
                    Some((p, s)) if !s.contains('/') && !s.is_empty() => {
                        (p.to_owned(), Some(s.to_owned()))
                    }
                    _ => (v, None),
                };
                a.models.push((PathBuf::from(path), section));
            }
            "--root" => a.dirs.push((PathBuf::from(it.next()?), false)),
            "--tech" => a.tech = Some(PathBuf::from(it.next()?)),
            "--spiceinit" => a.spiceinit = Some(PathBuf::from(it.next()?)),
            "--cdf" => a.cdf = Some(PathBuf::from(it.next()?)),
            "--xschemrc" => a.rcs.push(PathBuf::from(it.next()?)),
            "--no-core" => a.core = false,
            "-o" => a.out = PathBuf::from(it.next()?),
            s if s.starts_with('-') => return None,
            _ => a.dirs.push((PathBuf::from(arg), true)),
        }
    }
    (a.dirs.iter().any(|d| d.1) && !a.out.as_os_str().is_empty()).then_some(a)
}

/// Every `.sym`/`.sch` under `dir`, recursively, sorted, with the `.cir` files netlist
/// text may include (side inputs, import spec §1).
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

/// `text` with every `$NAME` / `${NAME}` replaced from the environment; `None` when one
/// is undefined.
fn expand_env(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let (name, tail) = match after.strip_prefix('{') {
            Some(b) => {
                let end = b.find('}')?;
                (&b[..end], &b[end + 1..])
            }
            None => {
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                (&after[..end], &after[end..])
            }
        };
        out.push_str(&std::env::var(name).ok()?);
        rest = tail;
    }
    out.push_str(rest);
    Some(out)
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Runs the importer on `args` (the command line without the program name) and returns
/// the exit status: 0 ok, 1 an output failed its check, 2 usage or I/O error. Shared by
/// `devicereader` and `schemify import`.
pub fn run(args: &[String]) -> u8 {
    let Some(args) = parse_args(args) else {
        return usage();
    };
    let mut roots = Vec::new();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (d, convert) in &args.dirs {
        let Ok(d) = std::fs::canonicalize(d) else {
            eprintln!("import: cannot open {}", d.display());
            return 2;
        };
        roots.push(slash(&d));
        if !convert {
            continue;
        }
        let mut paths = Vec::new();
        walk(&d, &mut paths);
        for p in paths {
            let key = slash(&p);
            if seen.insert(key.clone()) {
                match std::fs::read(&p) {
                    Ok(b) => files.push((key, b)),
                    Err(e) => eprintln!("import: {}: {e}", p.display()),
                }
            }
        }
    }
    let mut vars = Vec::new();
    let mut procs = Vec::new();
    for rc in &args.rcs {
        let Ok(text) = std::fs::read_to_string(rc) else {
            eprintln!("import: cannot read {}", rc.display());
            return 2;
        };
        for v in tcl::pdk_vars(&text) {
            if !vars.iter().any(|(n, _): &(String, String)| *n == v.0) {
                vars.push(v);
            }
        }
        procs.extend(tcl::drc_procs(&text));
    }
    let search_dirs: Vec<PathBuf> = args
        .spiceinit
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| spice::spiceinit_sourcepath(&t))
        .unwrap_or_default()
        .iter()
        .filter_map(|d| match expand_env(d) {
            Some(d) => Some(PathBuf::from(d)),
            None => {
                eprintln!("import: sourcepath {d}: undefined variable, skipped");
                None
            }
        })
        .collect();
    let scans: Vec<spice::ModelScan> = args
        .models
        .iter()
        .map(|(p, section)| {
            let opts = spice::ScanOptions {
                section: section.as_deref(),
                search_dirs: &search_dirs,
            };
            let s = spice::scan(p, &opts, |q| std::fs::read_to_string(q).ok());
            eprintln!(
                "models {} [{}]: {} files, {} models, {} bins, {} issues",
                p.display(),
                section.as_deref().unwrap_or("-"),
                s.files.len(),
                s.models.name.len(),
                s.bins.model.len(),
                s.issues.len()
            );
            s
        })
        .collect();
    let grid_pm = args.tech.as_ref().and_then(|t| {
        let text = std::fs::read_to_string(t).ok()?;
        if t.extension().is_some_and(|x| x == "lyt") {
            tech::read_lyt(&text).ok()?.default_grid_pm
        } else {
            // Magic splices `include`d files in; they sit beside the tech file.
            let dir = t.parent().unwrap_or(Path::new(""));
            let read = |n: &str| {
                std::fs::read_to_string(dir.join(n))
                    .or_else(|_| std::fs::read_to_string(dir.join(format!("{n}.tech"))))
                    .ok()
            };
            let magic = tech::read_magic_with(&text, read);
            for n in &magic.unresolved {
                eprintln!("import: tech include {n}: not found beside {}", t.display());
            }
            magic.manufacturing_grid_pm()
        }
    });
    let cdf = args
        .cdf
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| schemify_import::cdf::read(&t));
    let opts = Options {
        roots: &roots,
        pdk_vars: &vars,
        map_core: args.core,
        models: &scans,
        drc: &procs,
        grid_pm,
        cdf: cdf.as_ref(),
    };
    let inputs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .collect();
    let (outputs, report) = translate_library(&inputs, &opts);
    for (rel, bytes) in &outputs {
        let path = args.out.join(rel);
        let ok = path
            .parent()
            .is_none_or(|d| std::fs::create_dir_all(d).is_ok())
            && std::fs::write(&path, bytes).is_ok();
        if !ok {
            eprintln!("import: cannot write {}", path.display());
            return 2;
        }
    }
    summarize(&outputs, &report, procs.len(), grid_pm);
    let problems = check_output(&outputs);
    for p in problems.iter().take(60) {
        eprintln!("check: {p}");
    }
    println!("check: {} problems", problems.len());
    if problems.is_empty() { 0 } else { 1 }
}

fn summarize(
    outputs: &[(String, Vec<u8>)],
    report: &schemify_import::Report,
    procs: usize,
    grid_pm: Option<u32>,
) {
    let count = |ext: &str| outputs.iter().filter(|o| o.0.ends_with(ext)).count();
    println!(
        "inputs {} files; outputs {} shprim, {} shcomp, {} shbench; {} drc procs; grid {:?} pm",
        report.files.len(),
        count(".shprim"),
        count(".shcomp"),
        count(".shbench"),
        procs,
        grid_pm
    );
    println!(
        "devices instances mapped to core: {} / {}",
        report.core_instances, report.device_instances
    );
    let mut by_class = std::collections::BTreeMap::new();
    for it in &report.items {
        *by_class.entry(it.class.word()).or_insert(0u32) += 1;
    }
    for (c, n) in &by_class {
        if matches!(
            c.split(':').next(),
            Some("needs_review" | "dropped" | "alt-cell" | "unknown-token" | "rounding")
        ) {
            println!("  {c}: {n}");
        }
    }
    let reviewed: std::collections::BTreeSet<&str> = report
        .items
        .iter()
        .filter(|i| matches!(i.class, Class::Review(_)))
        .map(|i| report.files[i.file as usize].as_str())
        .collect();
    println!("  files with needs_review items: {}", reviewed.len());
    // Limits extracted: params carrying min/max/grid.
    let mut limits = 0;
    for (path, bytes) in outputs.iter().filter(|o| o.0.ends_with(".shprim")) {
        let Ok(d) = schemify_format::parse(bytes) else {
            continue;
        };
        for r in 1..d.record_count() {
            if d.record_kind(r) != schemify_format::RecordKind::Param {
                continue;
            }
            let get = |k| d.find_attr(r, k).and_then(|a| d.attr_value(a, 0));
            let (min, max, grid) = (get("min"), get("max"), get("grid"));
            if min.is_some() || max.is_some() {
                limits += 1;
                println!(
                    "  limit {path} {}: min {} max {} grid {}",
                    d.field(r, 0).unwrap_or(""),
                    min.unwrap_or("-"),
                    max.unwrap_or("-"),
                    grid.unwrap_or("-")
                );
            }
        }
    }
    println!("  params with min/max: {limits}");
}
