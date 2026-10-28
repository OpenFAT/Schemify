//! `schemify`: argv → one command spine → an exit code (D39: the headless core). Reads the
//! user config (config spec §2–§3) unless the command is `edit`/`cmd`, which hand over to the
//! editor binary `schemify-gui`, and prints its notifications on stderr.
//!
//! Exit codes (D27): 0 success, 1 design errors (violations, merge conflicts, non-canonical
//! files under `--check`, failed packages), 2 usage or I/O errors.
//!
//! Contents (binary-private): `USAGE`, `OK`, `DESIGN_ERROR`, `USAGE_ERROR`,
//! `type Exit = Result<u8, u8>`, `fn usage() -> u8`, `fn main() -> ExitCode`.

mod design;
mod files;
mod publish;
mod serve;

use std::path::PathBuf;
use std::process::ExitCode;

use schemify_config::{Config, Level, Request, Startup};

/// The help text, printed on `--help` (stdout) and after every usage error (stderr).
pub const USAGE: &str = "\
usage: schemify [--clean | -u <init.lua>] <command> [args]
       schemify --version

commands:
  edit [<project-dir>|<cell-file>] [--no-default-ui]
                                       open the editor (runs schemify-gui)
  fmt [--check] <file>...              rewrite files in canonical form
  merge-driver <O> <A> <B> [<path>]    git merge driver (writes the result to A)
  diff <A> <B>                         record-level semantic diff of two files
  git-setup                            register the merge driver in this clone
  netlist <cell> [--dialect D] [--config C] [--corner K] [--set VAR=EXPR]...
          [-o FILE] [--allow-errors]   write the netlist (dialects: ngspice espice xyce ltspice
                                       hspice cdl spectre verilog; default: as for run)
  check <cell> [--config C] [--corner K]
                                       report violations (exit 1 on errors)
  run <bench> [--config C]... [--corner K]... [--sweep S]... [--mc M] [--set VAR=EXPR]...
          [--jobs N] [--dialect D]     simulate a .shbench and print outputs and specs
                                       (D: ngspice espice xyce ltspice spectre; default: the
                                       PDK's simulator, schemify.opt.simulator, an installed one
                                       (espice first), else espice)
  cmd <project-dir>|<cell-file> <command> [<arg>...]
                                       run one editor command (a plugin's too) without a
                                       window: exit 1 when a plugin logs an error
  publish <project-dir>... -o <site-dir>
                                       lay out projects, with their saved runs, for the web
                                       editor on a static site (docs/spec/web.md §3.1)
  pack sync|update|restore|clean|status
                                       manage the user packages of init.lua
  rpc [--listen <socket>]              JSON-RPC command stream on stdio, or on a local socket
                                       (a socket path, or 127.0.0.1:<port>; docs/spec/rpc.md)
  mcp                                  the same methods as MCP tools on stdio (for agents)

--clean skips init.lua and user packages; -u runs another init file (docs/spec/config.md).
";

/// Exit code: success.
pub const OK: u8 = 0;
/// Exit code: the design was refused.
pub const DESIGN_ERROR: u8 = 1;
/// Exit code: bad arguments or an I/O failure.
pub const USAGE_ERROR: u8 = 2;

/// A command's outcome: `Ok(code)` when it ran to the end (a `diff` that found changes is
/// `Ok(1)`), `Err(code)` when it stopped early, its reason already printed.
pub type Exit = Result<u8, u8>;

/// Prints [`USAGE`] on stderr and returns [`USAGE_ERROR`].
pub fn usage() -> u8 {
    eprint!("{USAGE}");
    USAGE_ERROR
}

// ---- private ----

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
        return ExitCode::from(usage());
    };
    let rest = &args[first + 1..];
    // The editor reads the config itself (config spec §2): don't run it twice. `cmd` is a
    // command of the editor's table (plugin ones included), which the editor runs headless.
    match command.as_str() {
        "edit" => return ExitCode::from(edit(&start, rest)),
        "cmd" => return ExitCode::from(edit(&start, &args[first..])),
        _ => {}
    }
    let (config, startup) = Config::load(&start);
    report_requests(&startup);
    // Spine selection is the only branch on which work happens (codebase-clean-code rule 1).
    let exit: Exit = match command.as_str() {
        "fmt" => files::fmt(rest),
        "merge-driver" => files::merge_driver(rest),
        "diff" => files::diff(rest),
        "git-setup" => files::git_setup(rest),
        "netlist" => design::netlist(&config, rest),
        "check" => design::check(rest),
        "run" => design::run(&config, rest),
        "publish" => publish::publish(rest),
        "pack" => Ok(schemify_cli::pack::main(&config, rest)),
        "rpc" => serve::rpc(&start, rest),
        "mcp" => serve::mcp(&start, rest),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(OK)
        }
        "-V" | "--version" | "version" => {
            println!("schemify {}", env!("CARGO_PKG_VERSION"));
            Ok(OK)
        }
        _ => {
            eprint!("unknown command `{command}`\n{USAGE}");
            Err(USAGE_ERROR)
        }
    };
    let code = exit.unwrap_or_else(|c| c);
    assert!(code <= USAGE_ERROR, "exit codes are 0, 1 or 2");
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
/// and the remaining arguments; returns its exit code (2 when it cannot start or its code
/// does not fit a byte).
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
