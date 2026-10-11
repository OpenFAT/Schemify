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
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // A browser passes no argv: the web build is the editor on its file-system root (D2, D31).
    if cfg!(all(target_arch = "wasm32", target_os = "unknown")) && args.is_empty() {
        args = vec!["edit".into(), schemify_app::platform::WEB_ROOT.into()];
    }
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
        ..schemify_app::editor::Options::default()
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
    schemify_app::spines::env_pairs()
}

/// A loaded project with `file` open (load spine).
struct Opened {
    project: schemify_doc::Project,
    lib_dir: Vec<Option<std::path::PathBuf>>,
    top: schemify_doc::DocHandle,
}

impl schemify_app::spines::Cells for Opened {
    fn project(&self) -> &schemify_doc::Project {
        &self.project
    }

    fn open_row(&mut self, row: u32) -> Result<(), String> {
        use schemify_app::libs;
        let row = row as usize;
        let lib = self.project.cells().lib[row];
        let rel = libs::cell_relpath(&self.project, row);
        let (_, bytes, _) = libs::cell_bytes(&self.lib_dir, &self.project, lib, &rel)
            .map_err(|e| format!("{rel}: {e}"))?;
        let name = self.project.libs().name[usize::from(lib)].clone();
        self.project
            .open(&name, &rel, &bytes, None)
            .map(|_| ())
            .map_err(|e| format!("{name}:{rel}: cannot open: {e:?}"))
    }
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

/// Elaborates `o.top`, opening every cell elab asks for (`spines::elaborate_all`).
fn elaborate_all(
    o: &mut Opened,
    req: &schemify_elab::Request<'_>,
) -> Result<schemify_elab::Elab, u8> {
    let top = o.top;
    schemify_app::spines::elaborate_all(o, top, req, &mut schemify_elab::no_funcs).map_err(|e| {
        eprintln!("{e}");
        DESIGN_ERROR
    })
}

/// Prints violations (`E`/`W`/`I code cell path param: detail`); returns whether any is
/// an error.
fn report(e: &schemify_elab::Elab) -> bool {
    let mut errors = false;
    for v in &e.violations.rows {
        errors |= v.code.as_str().starts_with('E');
        eprintln!("{}", schemify_app::spines::violation_line(&e.names, v));
    }
    errors
}

fn dialect_of(word: Option<&str>) -> Option<schemify_elab::Dialect> {
    schemify_app::spines::dialect_of(word.unwrap_or("ngspice"))
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
/// outputs and specs (run spine, native runner; `spines::start_run`).
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
    let root = schemify_app::libs::project_root(Path::new(a.cell.as_deref().expect("parsed")));
    let top = o.top;
    let started = schemify_app::spines::start_run(
        &mut o,
        top,
        Some(&select),
        &env,
        &mut schemify_elab::no_funcs,
        |digest| {
            let mut cfg = run::RunnerConfig::new(&root, digest);
            if let Some(n) = a.jobs {
                cfg.workers = n;
            }
            cfg
        },
    );
    let mut r = match started {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return DESIGN_ERROR;
        }
    };
    let all = r.jobs.len();
    r.submit_next(&o.project, top, &env, &mut schemify_elab::no_funcs, all);
    r.runner
        .wait(&r.plan, &mut r.jobs, &mut r.results, &mut r.finished);
    if let Err(e) = r.runner.save(&r.jobs, &r.results) {
        eprintln!("results: {e}");
    }
    print!("{}", run::write_results(&r.jobs, &r.results));
    let failed = r.results.errors().count() > 0;
    for (j, _, msg) in r.results.errors() {
        eprintln!("job {j}: {msg}");
    }
    let spec_fail = (0..r.jobs.len()).any(|j| {
        (0..r.results.specs()).any(|s| matches!(r.results.pass(j, s as u16), run::Pass::Fail))
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
