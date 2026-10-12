//! `schemify`: the headless core (D39): CLI entry point, the user config and the package
//! manager. `schemify edit` starts the editor binary, `schemify-gui`.
//!
//! Exit codes (D27): 0 success, 1 design errors (violations, merge conflicts,
//! non-canonical files under `--check`, failed packages), 2 usage or I/O errors.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use schemify_cli::platform::write_atomic;
use schemify_cli::spines::{OpenError, Opened, env_pairs};
use schemify_config::{Config, Level, Request, Startup};
use schemify_format as format;

const USAGE: &str = "\
usage: schemify [--clean | -u <init.lua>] <command> [args]

commands:
  edit [<project-dir>|<cell-file>] [--no-default-ui]
                                       open the editor (runs schemify-gui)
  fmt [--check] <file>...              rewrite files in canonical form
  merge-driver <O> <A> <B> [<path>]    git merge driver (writes the result to A)
  diff <A> <B>                         record-level semantic diff of two files
  git-setup                            register the merge driver in this clone
  netlist <cell> [--dialect D] [--config C] [--corner K] [--set VAR=EXPR]...
          [-o FILE] [--allow-errors]   write the netlist (dialects: ngspice xyce cdl spectre verilog)
  check <cell> [--config C] [--corner K]
                                       report violations (exit 1 on errors)
  run <bench> [--config C]... [--corner K]... [--sweep S]... [--mc M] [--set VAR=EXPR]...
          [--jobs N]                   simulate a .shbench and print outputs and specs
  pack sync|update|restore|clean|status
                                       manage the user packages of init.lua
  rpc [--listen <socket>]              JSON-RPC command stream on stdio, or on a local socket
                                       (a socket path, or 127.0.0.1:<port>; docs/spec/rpc.md)
  mcp                                  the same methods as MCP tools on stdio (for agents)

--clean skips init.lua and user packages; -u runs another init file (docs/spec/config.md).
";

const OK: u8 = 0;
const DESIGN_ERROR: u8 = 1;
const USAGE_ERROR: u8 = 2;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (start, first) = match Startup::parse(&args) {
        Ok(s) => s,
        Err(e) => {
            eprint!("schemify: {e}\n{USAGE}");
            return ExitCode::from(USAGE_ERROR);
        }
    };
    let Some(command) = args.get(first) else {
        eprint!("{USAGE}");
        return ExitCode::from(USAGE_ERROR);
    };
    let rest = &args[first + 1..];
    if command == "edit" {
        // The editor reads the config itself (config spec §2): don't run it twice.
        return ExitCode::from(edit(&start, rest));
    }
    let (config, startup) = Config::load(&start);
    report_requests(&startup);
    // Spine selection is the only branch on which work happens (codebase-clean-code rule 1).
    let code = match command.as_str() {
        "fmt" => fmt(rest),
        "merge-driver" => merge_driver(rest),
        "diff" => diff(rest),
        "git-setup" => git_setup(rest),
        "netlist" => netlist(rest),
        "check" => check(rest),
        "run" => run(&config, rest),
        "pack" => schemify_cli::pack::main(&config, rest),
        "rpc" => rpc(&start, rest),
        "mcp" => mcp(&start, rest),
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

/// Prints the user config's notifications and errors on stderr. Queued commands are for the
/// editor: the core has no command table, so it ignores them (config spec §3).
fn report_requests(out: &[Request]) {
    for r in out {
        if let Request::Notify { level, text } = r {
            let tag = match level {
                Level::Error => "error",
                Level::Warn => "warning",
                Level::Info => "note",
            };
            eprintln!("{tag}: {text}");
        }
    }
}

/// `edit`: runs `schemify-gui` (next to this binary, else on `PATH`) with the startup flags
/// and the remaining arguments; returns its exit code.
fn edit(start: &Startup, args: &[String]) -> u8 {
    let name = format!("schemify-gui{}", std::env::consts::EXE_SUFFIX);
    let sibling = std::env::current_exe()
        .ok()
        .map(|p| p.with_file_name(&name))
        .filter(|p| p.is_file());
    let program = sibling.unwrap_or_else(|| PathBuf::from(&name));
    let status = std::process::Command::new(&program)
        .args(start.flags())
        .args(args)
        .status();
    match status {
        Ok(s) => s
            .code()
            .map_or(USAGE_ERROR, |c| u8::try_from(c).unwrap_or(USAGE_ERROR)),
        Err(e) => {
            eprintln!(
                "schemify edit: cannot run {} ({e}); install the editor (schemify-gui) next to \
                 schemify or on PATH",
                program.display()
            );
            USAGE_ERROR
        }
    }
}

/// `rpc [--listen <socket>]`: the JSON-RPC command stream (D27, docs/spec/rpc.md) on stdio
/// until it ends, or on a local socket serving one client after another until the listener
/// fails. Exit 0 at the end of stdin, 2 for usage or I/O errors.
fn rpc(start: &Startup, args: &[String]) -> u8 {
    use schemify_cli::rpc;
    let h = rpc::Headless {
        exe: std::env::current_exe().ok(),
        flags: start.flags(),
        seed: schemify_cli::platform::seed(),
    };
    let served = match args {
        [] => rpc::serve(
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
            &h,
        ),
        [flag, addr] if flag == "--listen" => rpc::listen(addr).and_then(|listener| {
            // A server: runs until the listener fails; a client's I/O error ends only it.
            loop {
                let mut stream = listener.accept()?;
                let mut input = std::io::BufReader::new(stream.try_clone()?);
                if let Err(e) = rpc::serve(&mut input, &mut stream, &h) {
                    eprintln!("rpc client: {e}");
                }
            }
        }),
        _ => {
            eprint!("{USAGE}");
            return USAGE_ERROR;
        }
    };
    match served {
        Ok(()) => OK,
        Err(e) => {
            eprintln!("rpc: {e}");
            USAGE_ERROR
        }
    }
}

/// `mcp`: the MCP bridge (D27) on stdio until it ends; exit 0 then, 2 for usage or I/O errors.
fn mcp(start: &Startup, args: &[String]) -> u8 {
    if !args.is_empty() {
        eprint!("{USAGE}");
        return USAGE_ERROR;
    }
    let h = schemify_cli::rpc::Headless {
        exe: std::env::current_exe().ok(),
        flags: start.flags(),
        seed: schemify_cli::platform::seed(),
    };
    let (stdin, stdout) = (std::io::stdin(), std::io::stdout());
    match schemify_cli::mcp::serve(&mut stdin.lock(), &mut stdout.lock(), &h) {
        Ok(()) => OK,
        Err(e) => {
            eprintln!("mcp: {e}");
            USAGE_ERROR
        }
    }
}

/// Reads a file, mapping I/O errors to a printed message.
fn read(path: &str) -> Result<Vec<u8>, u8> {
    schemify_cli::platform::read(Path::new(path)).map_err(|e| {
        eprintln!("{e}");
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
            eprintln!("{e}");
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
        eprintln!("{e}");
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

fn open_design(file: &str) -> Result<Opened, u8> {
    match schemify_cli::spines::open_file(Path::new(file), schemify_cli::platform::seed()) {
        Ok((o, notes)) => {
            for n in &notes {
                eprintln!("note: {n}");
            }
            Ok(o)
        }
        Err(OpenError::Io(e)) => {
            eprintln!("{file}: {e}");
            Err(USAGE_ERROR)
        }
        Err(OpenError::Design(e)) => {
            eprintln!("{file}: {e}");
            Err(DESIGN_ERROR)
        }
    }
}

/// Elaborates `o.top`, opening every cell elab asks for (`spines::elaborate_all`).
fn elaborate_all(
    o: &mut Opened,
    req: &schemify_elab::Request<'_>,
) -> Result<schemify_elab::Elab, u8> {
    let top = o.top;
    schemify_cli::spines::elaborate_all(o, top, req, &mut schemify_elab::no_funcs).map_err(|e| {
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
        eprintln!("{}", schemify_cli::spines::violation_line(&e.names, v));
    }
    errors
}

/// Borrows owned `(key, value)` pairs as the `&str` pairs the elab and run requests take.
fn pairs(owned: &[(String, String)]) -> Vec<(&str, &str)> {
    owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

/// `netlist`: load → elaborate → check → gate → emit (netlist spine).
fn netlist(args: &[String]) -> u8 {
    let Some(a) = design_args(args) else {
        eprint!("{USAGE}");
        return USAGE_ERROR;
    };
    let word = a.dialect.as_deref().unwrap_or("ngspice");
    let Some(dialect) = schemify_cli::spines::dialect_of(word) else {
        eprintln!("unknown dialect (ngspice xyce cdl spectre verilog)");
        return USAGE_ERROR;
    };
    let mut o = match open_design(a.cell.as_deref().expect("parsed")) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let env = env_pairs();
    let env = pairs(&env);
    let sets = pairs(&a.sets);
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
            if let Err(e) = write_atomic(Path::new(out), text.as_bytes()) {
                eprintln!("{e}");
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
    let env = pairs(&env);
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
fn run(config: &Config, args: &[String]) -> u8 {
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
    let env = pairs(&env);
    let configs: Vec<&str> = a.configs.iter().map(String::as_str).collect();
    let corners: Vec<&str> = a.corners.iter().map(String::as_str).collect();
    let sweeps: Vec<&str> = a.sweeps.iter().map(String::as_str).collect();
    let sets = pairs(&a.sets);
    let select = run::Select {
        configs: &configs,
        corners: &corners,
        sweeps: &sweeps,
        montecarlo: a.mc.as_deref(),
        overrides: &sets,
    };
    let root = schemify_cli::libs::project_root(Path::new(a.cell.as_deref().expect("parsed")));
    let top = o.top;
    // Every library row's directory (the project root standing in for builtin ones): a
    // Verilog-A `file` of a library cell (a generated PDK device) is compiled from there.
    let lib_dirs: Vec<PathBuf> = o
        .lib_dir
        .iter()
        .map(|d| d.clone().unwrap_or_else(|| root.clone()))
        .collect();
    assert_eq!(lib_dirs.len(), o.lib_dir.len());
    let started = schemify_cli::spines::start_run(
        &mut o,
        top,
        Some(&select),
        &env,
        &mut schemify_elab::no_funcs,
        |digest| {
            let mut cfg = run::RunnerConfig::new(&root, digest);
            cfg.lib_dirs = lib_dirs.clone();
            cfg.tools = schemify_cli::spines::tools(config.table());
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
fn run(_: &Config, _: &[String]) -> u8 {
    eprintln!("schemify run: no native simulator on the web (D16: use a runner plugin)");
    USAGE_ERROR
}
