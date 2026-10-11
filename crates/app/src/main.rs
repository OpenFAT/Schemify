//! `schemify`: CLI entry point and the spines (ARCHITECTURE.md § Spines).
//!
//! Exit codes (D27): 0 success, 1 design errors (violations, merge conflicts,
//! non-canonical files under `--check`), 2 usage or I/O errors.

use std::path::Path;
use std::process::ExitCode;

use schemify_format as format;

const USAGE: &str = "\
usage: schemify <command> [args]

commands:
  edit [<project-dir>|<cell-file>] [--no-default-ui]
                                       open the interactive editor (GUI)
  fmt [--check] <file>...              rewrite files in canonical form
  merge-driver <O> <A> <B> [<path>]    git merge driver (writes the result to A)
  diff <A> <B>                         record-level semantic diff of two files
  git-setup                            register the merge driver in this clone
  import <xschem-dir>... -o <out-dir> [--root D]... [--xschemrc F]... [--models F[:S]]...
         [--tech F] [--cdf F] [--no-core]
                                       convert xschem libraries (designs or PDK devices)
  netlist <cell> [--dialect D] [--config C] [--corner K] [--set VAR=EXPR]...
          [-o FILE] [--allow-errors]   write the netlist (dialects: ngspice xyce cdl spectre verilog)
  check <cell> [--config C] [--corner K]
                                       report violations (exit 1 on errors)
  run <bench> [--config C]... [--corner K]... [--sweep S]... [--mc M] [--set VAR=EXPR]...
          [--jobs N]                   simulate a .shbench and print outputs and specs
";

const OK: u8 = 0;
const DESIGN_ERROR: u8 = 1;
const USAGE_ERROR: u8 = 2;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        eprint!("{USAGE}");
        return ExitCode::from(USAGE_ERROR);
    };
    let rest = &args[1..];
    // Spine selection is the only branch on which work happens (codebase-clean-code rule 1).
    let code = match command.as_str() {
        "fmt" => fmt(rest),
        "merge-driver" => merge_driver(rest),
        "diff" => diff(rest),
        "edit" => edit(rest),
        "git-setup" => git_setup(rest),
        "import" => schemify_devicereader::run(rest),
        "netlist" => netlist(rest),
        "check" => check(rest),
        "run" => run(rest),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            OK
        }
        _ => {
            eprint!("unknown command `{command}`\n{USAGE}");
            USAGE_ERROR
        }
    };
    ExitCode::from(code)
}

/// `edit`: the interactive editor (`frame` spine) on a project directory or cell file.
#[cfg(feature = "gui")]
fn edit(args: &[String]) -> u8 {
    let no_default_ui = args.iter().any(|a| a == "--no-default-ui");
    let unknown = args
        .iter()
        .any(|a| a.starts_with("--") && a != "--no-default-ui");
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if paths.len() > 1 || unknown {
        eprint!("{USAGE}");
        return USAGE_ERROR;
    }
    let opts = schemify_app::editor::Options {
        no_default_ui,
        path: paths.first().map(|p| std::path::PathBuf::from(p.as_str())),
        seed: schemify_app::platform::seed(),
    };
    match schemify_app::editor::run(opts) {
        Ok(()) => OK,
        Err(e) => {
            eprintln!("schemify edit: {e}");
            USAGE_ERROR
        }
    }
}

/// `edit` without the `gui` feature (the M1 headless build).
#[cfg(not(feature = "gui"))]
fn edit(_: &[String]) -> u8 {
    eprintln!("schemify edit: this build has no GUI (feature `gui`)");
    USAGE_ERROR
}

/// Reads a file, mapping I/O errors to a printed message.
fn read(path: &str) -> Result<Vec<u8>, u8> {
    std::fs::read(path).map_err(|e| {
        eprintln!("{path}: {e}");
        USAGE_ERROR
    })
}

/// Parses `bytes` from `path`, printing diagnostics as `path:line:col: …` on failure.
fn parse<'b>(path: &str, bytes: &'b [u8]) -> Result<format::Document<'b>, u8> {
    format::parse(bytes).map_err(|e| {
        for d in &e.diagnostics {
            eprintln!("{path}:{d}");
        }
        DESIGN_ERROR
    })
}

/// `fmt`: parse → canonical write; rewrite in place unless `--check`.
fn fmt(args: &[String]) -> u8 {
    let check = args.iter().any(|a| a == "--check");
    let files: Vec<&String> = args.iter().filter(|a| *a != "--check").collect();
    if files.is_empty() {
        eprint!("{USAGE}");
        return USAGE_ERROR;
    }
    let mut worst = OK;
    for path in files {
        let bytes = match read(path) {
            Ok(b) => b,
            Err(c) => return c,
        };
        let doc = match parse(path, &bytes) {
            Ok(d) => d,
            Err(c) => {
                worst = worst.max(c);
                continue;
            }
        };
        let canonical = format::write(&doc);
        if canonical.as_bytes() == bytes.as_slice() {
            continue;
        }
        if check {
            eprintln!("{path}: not in canonical form");
            worst = worst.max(DESIGN_ERROR);
        } else if let Err(e) = write_atomic(Path::new(path), canonical.as_bytes()) {
            eprintln!("{path}: {e}");
            return USAGE_ERROR;
        }
    }
    worst
}

/// `merge-driver O A B [P]`: semantic 3-way merge (format spec §8); the result
/// replaces A, as git expects. Exit 1 when conflict blocks remain.
fn merge_driver(args: &[String]) -> u8 {
    let [o, a, b, ..] = args else {
        eprint!("{USAGE}");
        return USAGE_ERROR;
    };
    let shown = args.get(3).unwrap_or(a);
    let (ob, ab, bb) = match (read(o), read(a), read(b)) {
        (Ok(o), Ok(a), Ok(b)) => (o, a, b),
        _ => return USAGE_ERROR,
    };
    // A side that doesn't parse can't be merged by record: leave A untouched so
    // git reports the conflict and the user resolves it by hand.
    let (od, ad, bd) = match (parse(shown, &ob), parse(shown, &ab), parse(shown, &bb)) {
        (Ok(o), Ok(a), Ok(b)) => (o, a, b),
        _ => return DESIGN_ERROR,
    };
    let merged = format::merge3(&od, &ad, &bd);
    if let Err(e) = write_atomic(Path::new(a), merged.text.as_bytes()) {
        eprintln!("{a}: {e}");
        return USAGE_ERROR;
    }
    if merged.conflicts > 0 {
        eprintln!("{shown}: {} conflicting record(s)", merged.conflicts);
        return DESIGN_ERROR;
    }
    OK
}

/// `diff A B`: one line per added (`+`), removed (`-`) or changed (`~`) record.
/// Exit 1 when the files differ, like `diff(1)`.
fn diff(args: &[String]) -> u8 {
    let [a, b] = args else {
        eprint!("{USAGE}");
        return USAGE_ERROR;
    };
    let (ab, bb) = match (read(a), read(b)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return USAGE_ERROR,
    };
    let (ad, bd) = match (parse(a, &ab), parse(b, &bb)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return DESIGN_ERROR,
    };
    let d = format::diff(&ad, &bd);
    let mut out = String::new();
    for c in &d.changes {
        let (sign, doc, r) = match c.kind {
            format::ChangeKind::Added => ('+', &bd, c.b),
            format::ChangeKind::Removed => ('-', &ad, c.a),
            format::ChangeKind::Changed => ('~', &bd, c.b),
        };
        let (k1, k2) = doc.record_key(r);
        out.push(sign);
        out.push(' ');
        out.push_str(doc.record_word(r));
        for k in [k1, k2] {
            if !k.is_empty() {
                out.push(' ');
                out.push_str(k);
            }
        }
        if c.kind == format::ChangeKind::Changed {
            let what = [
                (format::Parts::FIELDS, "fields"),
                (format::Parts::VERBATIM, "text"),
                (format::Parts::COMMENTS, "comments"),
            ];
            for (part, name) in what {
                if c.parts.contains(part) {
                    out.push_str(" [");
                    out.push_str(name);
                    out.push(']');
                }
            }
            let keys = &d.keys[c.keys.start as usize..c.keys.end as usize];
            if !keys.is_empty() {
                out.push_str(" [attrs: ");
                out.push_str(&keys.join(", "));
                out.push(']');
            }
        }
        out.push('\n');
    }
    print!("{out}");
    if d.changes.is_empty() {
        OK
    } else {
        DESIGN_ERROR
    }
}

/// `git-setup`: configures this clone's merge driver (git never reads driver
/// commands from the repo, for security) and adds the `.gitattributes` lines.
fn git_setup(args: &[String]) -> u8 {
    if !args.is_empty() {
        eprint!("{USAGE}");
        return USAGE_ERROR;
    }
    let settings = [
        ("merge.schemify.name", "Schemify semantic merge"),
        ("merge.schemify.driver", "schemify merge-driver %O %A %B %P"),
    ];
    for (key, value) in settings {
        let status = std::process::Command::new("git")
            .args(["config", key, value])
            .status();
        if !matches!(status, Ok(s) if s.success()) {
            eprintln!("git config {key} failed (is this a git clone?)");
            return USAGE_ERROR;
        }
    }
    let path = Path::new(".gitattributes");
    let mut text = std::fs::read_to_string(path).unwrap_or_default();
    let before = text.len();
    for ext in ["shprim", "shcomp", "shbench"] {
        let line = format!("*.{ext} merge=schemify");
        if !text.lines().any(|l| l.trim() == line) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&line);
            text.push('\n');
        }
    }
    if text.len() != before
        && let Err(e) = std::fs::write(path, text)
    {
        eprintln!(".gitattributes: {e}");
        return USAGE_ERROR;
    }
    println!("merge driver configured; commit .gitattributes so clones share it");
    OK
}

/// Writes `bytes` to a sibling temp file, then renames it over `path`, so a
/// crash never leaves a half-written design file (edits spec §5).
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".schemify-tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

// ---------------------------------------------------------------- design spines

/// Flags shared by `netlist`, `check` and `run`.
#[derive(Default)]
struct DesignArgs {
    cell: Option<String>,
    dialect: Option<String>,
    configs: Vec<String>,
    corners: Vec<String>,
    sweeps: Vec<String>,
    mc: Option<String>,
    sets: Vec<(String, String)>,
    out: Option<String>,
    allow_errors: bool,
    jobs: Option<u16>,
}

fn design_args(args: &[String]) -> Option<DesignArgs> {
    let mut a = DesignArgs::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dialect" => a.dialect = Some(it.next()?.clone()),
            "--config" => a.configs.push(it.next()?.clone()),
            "--corner" => a.corners.push(it.next()?.clone()),
            "--sweep" => a.sweeps.push(it.next()?.clone()),
            "--mc" => a.mc = Some(it.next()?.clone()),
            "--set" => {
                let (k, v) = it.next()?.split_once('=')?;
                a.sets.push((k.to_owned(), v.to_owned()));
            }
            "-o" => a.out = Some(it.next()?.clone()),
            "--allow-errors" => a.allow_errors = true,
            "--jobs" => a.jobs = Some(it.next()?.parse().ok().filter(|&n| n > 0)?),
            s if s.starts_with('-') => return None,
            _ if a.cell.is_none() => a.cell = Some(arg.clone()),
            _ => return None,
        }
    }
    a.cell.is_some().then_some(a)
}

/// The process environment for `${NAME}` expansion (netlist spec §7.1).
fn env_pairs() -> Vec<(String, String)> {
    std::env::vars().collect()
}

/// A loaded project with `file` open (load spine).
struct Opened {
    project: schemify_doc::Project,
    lib_dir: Vec<Option<std::path::PathBuf>>,
    top: schemify_doc::DocHandle,
}

fn open_design(file: &str) -> Result<Opened, u8> {
    use schemify_app::libs;
    let path = Path::new(file);
    let root = libs::project_root(path);
    let loaded = libs::load(&root, schemify_app::platform::seed()).map_err(|e| {
        eprintln!("{file}: {e}");
        USAGE_ERROR
    })?;
    for n in &loaded.notes {
        eprintln!("note: {n}");
    }
    let mut project = loaded.project;
    // The library whose directory holds the file (deepest wins), else the project.
    let abs = libs::absolute(path);
    let mut best: Option<(usize, String, usize)> = None;
    for (row, dir) in loaded.lib_dir.iter().enumerate() {
        let Some(dir) = dir else { continue };
        let dir = libs::absolute(dir);
        if let Some(rel) = libs::rel_path(&dir, &abs) {
            let depth = dir.components().count();
            if best.as_ref().is_none_or(|b| depth > b.2) {
                best = Some((row, rel, depth));
            }
        }
    }
    let Some((row, rel, _)) = best else {
        eprintln!(
            "{file}: outside every library of the project at {}",
            root.display()
        );
        return Err(USAGE_ERROR);
    };
    let bytes = read(file)?;
    let name = project.libs().name[row].clone();
    let top = match project.open(&name, &rel, &bytes, None) {
        Ok(o) => o.handle,
        Err(e) => {
            eprintln!("{file}: cannot open: {e:?}");
            return Err(DESIGN_ERROR);
        }
    };
    Ok(Opened {
        project,
        lib_dir: loaded.lib_dir,
        top,
    })
}

/// Elaborates `o.top`, opening every cell elab asks for (bounded by the cell count).
fn elaborate_all(
    o: &mut Opened,
    req: &schemify_elab::Request<'_>,
) -> Result<schemify_elab::Elab, u8> {
    use schemify_app::libs;
    let rounds = o.project.cells().lib.len() + 1;
    for _ in 0..rounds {
        let e = schemify_elab::elaborate(&o.project, o.top, req, &mut schemify_elab::no_funcs);
        if e.needs_open.is_empty() {
            return Ok(e);
        }
        for &row in &e.needs_open {
            let row = row as usize;
            let lib = o.project.cells().lib[row];
            let rel = libs::cell_relpath(&o.project, row);
            let (_, bytes, _) =
                libs::cell_bytes(&o.lib_dir, &o.project, lib, &rel).map_err(|e| {
                    eprintln!("{rel}: {e}");
                    USAGE_ERROR
                })?;
            let name = o.project.libs().name[usize::from(lib)].clone();
            if let Err(e) = o.project.open(&name, &rel, &bytes, None) {
                eprintln!("{name}:{rel}: cannot open: {e:?}");
                return Err(DESIGN_ERROR);
            }
        }
    }
    eprintln!("elaboration kept asking for more cells");
    Err(DESIGN_ERROR)
}

/// Prints violations (`E`/`W`/`I code cell path param: detail`); returns whether any is
/// an error.
fn report(e: &schemify_elab::Elab) -> bool {
    let mut errors = false;
    for v in &e.violations.rows {
        let name = |n: schemify_elab::Name| {
            (n != schemify_elab::Name::NONE).then(|| e.names.get(n).to_owned())
        };
        errors |= v.code.as_str().starts_with('E');
        let mut line = format!("{} {}", v.code.as_str(), name(v.cell).unwrap_or_default());
        for extra in [name(v.path), name(v.param)].into_iter().flatten() {
            line.push(' ');
            line.push_str(&extra);
        }
        eprintln!("{line}: {}", v.detail);
    }
    errors
}

fn dialect_of(word: Option<&str>) -> Option<schemify_elab::Dialect> {
    use schemify_elab::Dialect;
    Some(match word.unwrap_or("ngspice") {
        "ngspice" => Dialect::Ngspice,
        "xyce" => Dialect::Xyce,
        "cdl" => Dialect::Cdl,
        "spectre" => Dialect::Spectre,
        "verilog" => Dialect::Verilog,
        _ => return None,
    })
}

/// `netlist`: load → elaborate → check → gate → emit (netlist spine).
fn netlist(args: &[String]) -> u8 {
    let Some(a) = design_args(args) else {
        eprint!("{USAGE}");
        return USAGE_ERROR;
    };
    let Some(dialect) = dialect_of(a.dialect.as_deref()) else {
        eprintln!("unknown dialect (ngspice xyce cdl spectre verilog)");
        return USAGE_ERROR;
    };
    let mut o = match open_design(a.cell.as_deref().expect("parsed")) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let env = env_pairs();
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let sets: Vec<(&str, &str)> = a
        .sets
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let req = schemify_elab::Request {
        config: a.configs.first().map(String::as_str),
        corner: a.corners.first().map(String::as_str),
        overrides: &sets,
        env: &env,
        dialect,
    };
    let e = match elaborate_all(&mut o, &req) {
        Ok(e) => e,
        Err(c) => return c,
    };
    if report(&e) && !a.allow_errors {
        eprintln!("not netlisted: fix the errors above or pass --allow-errors");
        return DESIGN_ERROR;
    }
    let text = schemify_netlist::emit(&e, dialect).text;
    match &a.out {
        Some(out) => {
            if let Err(err) = write_atomic(Path::new(out), text.as_bytes()) {
                eprintln!("{out}: {err}");
                return USAGE_ERROR;
            }
        }
        None => print!("{text}"),
    }
    OK
}

/// `check`: load → elaborate → print violations; exit 1 on any error.
fn check(args: &[String]) -> u8 {
    let Some(a) = design_args(args) else {
        eprint!("{USAGE}");
        return USAGE_ERROR;
    };
    let mut o = match open_design(a.cell.as_deref().expect("parsed")) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let env = env_pairs();
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let req = schemify_elab::Request {
        config: a.configs.first().map(String::as_str),
        corner: a.corners.first().map(String::as_str),
        overrides: &[],
        env: &env,
        dialect: schemify_elab::Dialect::Ngspice,
    };
    match elaborate_all(&mut o, &req) {
        Ok(e) if report(&e) => DESIGN_ERROR,
        Ok(_) => OK,
        Err(c) => c,
    }
}

/// `run`: plan → expand → design hash (gate) → per job prepare + submit → wait →
/// outputs and specs (run spine, native runner).
#[cfg(not(target_arch = "wasm32"))]
fn run(args: &[String]) -> u8 {
    use schemify_run as run;
    let Some(a) = design_args(args) else {
        eprint!("{USAGE}");
        return USAGE_ERROR;
    };
    let mut o = match open_design(a.cell.as_deref().expect("parsed")) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let env = env_pairs();
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    // Open every reached cell first (each config may reach different ones).
    let probe = schemify_elab::Request {
        config: a.configs.first().map(String::as_str),
        corner: None,
        overrides: &[],
        env: &env,
        dialect: schemify_elab::Dialect::Ngspice,
    };
    if let Err(c) = elaborate_all(&mut o, &probe) {
        return c;
    }
    let plan = match run::Plan::read(o.project.doc(o.top).expect("open")) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("bench: {e:?}");
            return DESIGN_ERROR;
        }
    };
    let configs: Vec<&str> = a.configs.iter().map(String::as_str).collect();
    let corners: Vec<&str> = a.corners.iter().map(String::as_str).collect();
    let sweeps: Vec<&str> = a.sweeps.iter().map(String::as_str).collect();
    let sets: Vec<(&str, &str)> = a
        .sets
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let select = run::Select {
        configs: &configs,
        corners: &corners,
        sweeps: &sweeps,
        montecarlo: a.mc.as_deref(),
        overrides: &sets,
    };
    let mut jobs = match run::expand(&plan, &select) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("bench: {e:?}");
            return DESIGN_ERROR;
        }
    };
    let digest = match run::design(&o.project, o.top, &jobs, &env, &mut schemify_elab::no_funcs) {
        Ok(d) => d,
        Err(b) => {
            eprintln!("run blocked: {b:?}");
            return DESIGN_ERROR;
        }
    };
    let mut results = run::Results::new(&plan, digest);
    let root = schemify_app::libs::project_root(Path::new(a.cell.as_deref().expect("parsed")));
    let mut cfg = run::RunnerConfig::new(&root, &digest);
    if let Some(n) = a.jobs {
        cfg.workers = n;
    }
    let mut runner = run::Runner::new(cfg);
    for j in 0..jobs.len() {
        let prepared = run::prepare(
            &o.project,
            o.top,
            &mut jobs,
            j,
            &env,
            schemify_elab::Dialect::Ngspice,
            &mut schemify_elab::no_funcs,
            &mut results,
        );
        if let Some(p) = prepared {
            runner.submit(&mut jobs, &mut results, j, &p);
        }
    }
    let mut finished = Vec::new();
    runner.wait(&plan, &mut jobs, &mut results, &mut finished);
    if let Err(e) = runner.save(&jobs, &results) {
        eprintln!("results: {e}");
    }
    print!("{}", run::write_results(&jobs, &results));
    let failed = results.errors().count() > 0;
    for (j, _, msg) in results.errors() {
        eprintln!("job {j}: {msg}");
    }
    let spec_fail = (0..jobs.len()).any(|j| {
        (0..results.specs()).any(|s| matches!(results.pass(j, s as u16), run::Pass::Fail))
    });
    if failed || spec_fail {
        DESIGN_ERROR
    } else {
        OK
    }
}

#[cfg(target_arch = "wasm32")]
fn run(_: &[String]) -> u8 {
    eprintln!("schemify run: no native simulator on the web (D16: use a runner plugin)");
    USAGE_ERROR
}
