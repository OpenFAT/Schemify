//! The design commands: a cell file and flags → a netlist, a violation report, or a finished
//! simulation run (architecture.md § Spines, `netlist` and `run`; netlist spec §7.1, run spec
//! §1–§7a). Each is one spine over `schemify_cli::spines`, the steps the editor shares.
//!
//! Contents: `fn netlist(&Config, &[String]) -> Exit`, `fn check(&[String]) -> Exit`,
//! `fn run(&Config, &[String]) -> Exit`.

use std::path::Path;

use schemify_cli::spines::{self, OpenError, Opened};
use schemify_cli::{libs, platform};
use schemify_config::Config;
use schemify_elab::{self as elab, Dialect};

use crate::{DESIGN_ERROR, Exit, OK, USAGE_ERROR, usage};

/// The flags `netlist`, `check` and `run` share; each command reads the ones it lists.
#[derive(Default)]
struct DesignArgs {
    cell: String,
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

/// `netlist`: load → elaborate → check → gate → emit (netlist spine), to `-o` or stdout.
pub fn netlist(config: &Config, args: &[String]) -> Exit {
    let a = design_args(args).ok_or_else(usage)?;
    let explicit = explicit_dialect(&a)?;
    let mut o = open_design(&a.cell)?;
    let dialect = dialect(config, explicit, &o)?;
    let env = spines::env_pairs();
    let env = pairs(&env);
    let sets = pairs(&a.sets);
    let req = elab::Request {
        config: a.configs.first().map(String::as_str),
        corner: a.corners.first().map(String::as_str),
        overrides: &sets,
        env: &env,
        dialect,
    };
    let e = elaborate_all(&mut o, &req)?;
    if report(&e) && !a.allow_errors {
        eprintln!("not netlisted: fix the errors above or pass --allow-errors");
        return Err(DESIGN_ERROR);
    }
    let text = schemify_netlist::emit(&e, dialect).text;
    match &a.out {
        Some(out) => platform::write_atomic(Path::new(out), text.as_bytes()).map_err(|e| {
            eprintln!("{e}");
            USAGE_ERROR
        })?,
        None => print!("{text}"),
    }
    Ok(OK)
}

/// `check`: load → elaborate → print violations; exit 1 on any error.
pub fn check(args: &[String]) -> Exit {
    let a = design_args(args).ok_or_else(usage)?;
    let mut o = open_design(&a.cell)?;
    let env = spines::env_pairs();
    let env = pairs(&env);
    let req = elab::Request {
        config: a.configs.first().map(String::as_str),
        corner: a.corners.first().map(String::as_str),
        overrides: &[],
        env: &env,
        dialect: Dialect::Ngspice,
    };
    let e = elaborate_all(&mut o, &req)?;
    Ok(if report(&e) { DESIGN_ERROR } else { OK })
}

/// `run`: plan → expand → design hash (gate) → per job prepare + submit → wait → outputs
/// and specs (run spine, native runner; `spines::start_run`). The run is saved under the
/// bench name the editor's history looks for (`spines::bench_name`).
#[cfg(not(target_arch = "wasm32"))]
pub fn run(config: &Config, args: &[String]) -> Exit {
    use schemify_run as run;
    let a = design_args(args).ok_or_else(usage)?;
    let explicit = explicit_dialect(&a)?;
    if let Some(d) = explicit.filter(|d| !spines::SIMULATORS.contains(d)) {
        eprintln!(
            "run: no simulator for `{}` (ngspice espice xyce ltspice spectre)",
            d.as_str()
        );
        return Err(USAGE_ERROR);
    }
    let mut o = open_design(&a.cell)?;
    let chosen = dialect(config, explicit, &o)?;
    let dialect = spines::run_dialect(chosen, &o.project, config.table()).map_err(|e| {
        eprintln!("run: {e}");
        DESIGN_ERROR
    })?;
    assert!(
        spines::SIMULATORS.contains(&dialect),
        "run_dialect picks a simulator"
    );
    let env = spines::env_pairs();
    let env = pairs(&env);
    let (configs, corners, sweeps) = (strs(&a.configs), strs(&a.corners), strs(&a.sweeps));
    let sets = pairs(&a.sets);
    let select = run::Select {
        configs: &configs,
        corners: &corners,
        sweeps: &sweeps,
        montecarlo: a.mc.as_deref(),
        overrides: &sets,
    };
    let cell = Path::new(&a.cell);
    let root = libs::project_root(cell);
    let top = o.top;
    // Every library row's directory (the project root standing in for builtin ones): a
    // Verilog-A `file` of a library cell (a generated PDK device) is compiled from there.
    let lib_dirs: Vec<_> = o
        .lib_dir
        .iter()
        .map(|d| d.clone().unwrap_or_else(|| root.clone()))
        .collect();
    assert_eq!(lib_dirs.len(), o.lib_dir.len());
    let started = spines::start_run(
        &mut o,
        top,
        Some(&select),
        &env,
        dialect,
        &mut elab::no_funcs,
        |digest| {
            let mut cfg = run::RunnerConfig::new(&root, digest);
            cfg.lib_dirs = lib_dirs;
            cfg.tools = spines::tools(config.table());
            if let Some(n) = a.jobs {
                cfg.workers = n;
            }
            cfg
        },
    );
    let mut r = started.map_err(|e| {
        eprintln!("{e}");
        DESIGN_ERROR
    })?;
    let all = r.jobs.len();
    r.submit_next(&o.project, top, &env, &mut elab::no_funcs, all);
    r.runner
        .wait(&r.plan, &mut r.jobs, &mut r.results, &mut r.finished);
    assert!(r.idle(), "wait returns with every job finished");
    let bench = spines::bench_name(
        &o.project,
        |lib| o.lib_dir.get(usize::from(lib))?.as_deref(),
        cell,
    )
    .unwrap_or_else(|| {
        cell.file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
    });
    if let Err(e) = r.save(&bench) {
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
    Ok(if failed || spec_fail {
        DESIGN_ERROR
    } else {
        OK
    })
}

/// `run` on the web: no native simulator (D16: a runner plugin simulates there).
#[cfg(target_arch = "wasm32")]
pub fn run(_: &Config, _: &[String]) -> Exit {
    eprintln!("schemify run: no native simulator on the web (D16: use a runner plugin)");
    Err(USAGE_ERROR)
}

// ---- private ----

/// Parses the shared flags; `None` for an unknown flag, a missing value, a second cell, or
/// no cell at all.
fn design_args(args: &[String]) -> Option<DesignArgs> {
    let mut a = DesignArgs::default();
    let mut cell = None;
    let mut it = args.iter();
    // Bounded by `args`: every pass consumes at least one argument.
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
            _ if cell.is_none() => cell = Some(arg.clone()),
            _ => return None,
        }
    }
    a.cell = cell?;
    assert!(a.jobs != Some(0));
    Some(a)
}

/// Opens the project around `file` with `file` open; notes go to stderr as `note: …`.
fn open_design(file: &str) -> Result<Opened, u8> {
    match spines::open_file(Path::new(file), platform::seed()) {
        Ok((o, notes)) => {
            assert!(o.project.doc(o.top).is_some(), "open_file opens its file");
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
fn elaborate_all(o: &mut Opened, req: &elab::Request<'_>) -> Result<elab::Elab, u8> {
    let top = o.top;
    spines::elaborate_all(o, top, req, &mut elab::no_funcs).map_err(|e| {
        eprintln!("{e}");
        DESIGN_ERROR
    })
}

/// Prints violations (`E`/`W`/`I code cell path param: detail`); returns whether any is an
/// error.
fn report(e: &elab::Elab) -> bool {
    let mut errors = false;
    for v in &e.violations.rows {
        errors |= v.code.as_str().starts_with('E');
        eprintln!("{}", spines::violation_line(&e.names, v));
    }
    errors
}

/// Borrows owned `(key, value)` pairs as the `&str` pairs the elab and run requests take.
fn pairs(owned: &[(String, String)]) -> Vec<(&str, &str)> {
    let v: Vec<(&str, &str)> = owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(v.len(), owned.len());
    v
}

/// Borrows owned strings as the `&str` lists the run selection takes.
#[cfg(not(target_arch = "wasm32"))]
fn strs(owned: &[String]) -> Vec<&str> {
    let borrowed: Vec<&str> = owned.iter().map(String::as_str).collect();
    assert_eq!(borrowed.len(), owned.len());
    borrowed
}

/// The `--dialect` of `a` (`Ok(None)` without one); an unknown word is reported and is a
/// usage error.
fn explicit_dialect(a: &DesignArgs) -> Result<Option<Dialect>, u8> {
    let Some(word) = a.dialect.as_deref() else {
        return Ok(None);
    };
    let d = spines::dialect_of(word).ok_or_else(|| {
        eprintln!(
            "unknown dialect `{word}` (ngspice espice xyce ltspice hspice cdl spectre verilog)"
        );
        USAGE_ERROR
    })?;
    assert_eq!(spines::dialect_word(d), word, "dialect words round-trip");
    Ok(Some(d))
}

/// The dialect of the request (`spines::sim_dialect`: `--dialect`, the PDK's pin, the user
/// config, detection); a refused pin is reported and is a design error.
fn dialect(config: &Config, explicit: Option<Dialect>, o: &Opened) -> Result<Dialect, u8> {
    let chosen = spines::sim_dialect(explicit, &o.project, config.table()).map_err(|e| {
        eprintln!("{e}");
        DESIGN_ERROR
    })?;
    if let Some(d) = explicit {
        assert_eq!(chosen, d, "an explicit --dialect wins");
    }
    Ok(chosen)
}
