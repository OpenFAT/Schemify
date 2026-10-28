//! `schemify-gui`: command-line arguments → the editor in a window, a served window
//! (`--rpc`) or one headless command (`cmd`); D39, started directly or by `schemify edit`.
//!
//! `schemify-gui [--clean | -u <init.lua>] [edit] [<project-dir>|<cell-file>]
//! [--no-default-ui] [--rpc <socket>]` (parsed by [`schemify_config::Launch`]), or
//! `schemify-gui cmd <path> <command> [<arg>...]` (`schemify_app::headless`).
//! Exit codes: 0 closed normally, 2 usage or startup errors; `cmd` exits with the
//! command's status.
//!
//! On the web the URL fragment is the argument (`index.html#/projects/inv/top.shcomp`) and
//! the browser storage is mounted before the editor starts (web spec §1–§2).
//!
//! Contents: `fn main` only.

use schemify_config::Launch;
use schemify_editor::editor::Options;
use schemify_editor::platform;

// ---- private ----

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn main() -> std::process::ExitCode {
    let args = platform::args();
    if let Some(code) = headless(&args) {
        return std::process::ExitCode::from(code);
    }
    match start(&args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprint!("schemify-gui: {e}");
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn main() {
    // Errors go to the browser console; the page stays (there is no process to exit).
    platform::mount_then(|| start(&platform::args()));
}

/// `cmd <path> <command> [<arg>...]` (`schemify cmd`): runs the command headless and
/// returns its exit status; `None` when the arguments are not a `cmd` invocation.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn headless(args: &[String]) -> Option<u8> {
    let (config, first) = schemify_config::Startup::parse(args).ok()?;
    let [word, path, name, rest @ ..] = &args[first..] else {
        // `cmd` with too few arguments is a usage error, anything else a launch.
        return (args.get(first).map(String::as_str) == Some("cmd")).then(|| {
            eprint!("{}", schemify_config::GUI_USAGE);
            2
        });
    };
    if word != "cmd" {
        return None;
    }
    let opts = Options {
        path: Some(std::path::PathBuf::from(path)),
        seed: platform::seed(),
        config,
        ..Options::default()
    };
    Some(schemify_app::headless::command(opts, name, rest))
}

/// Parses the arguments and runs the editor (native: until its window closes; web: returns
/// once the window is attached). Errors end with a newline.
fn start(args: &[String]) -> Result<(), String> {
    let launch = Launch::parse(args)?;
    // The web build has no `sim` field (D16: no processes), so the update is empty there.
    #[cfg_attr(target_arch = "wasm32", allow(clippy::needless_update))]
    let opts = Options {
        no_default_ui: launch.no_default_ui,
        // A URL without a fragment opens the default web project (web spec §1).
        path: launch.path.or_else(|| {
            cfg!(all(target_arch = "wasm32", target_os = "unknown"))
                .then(|| platform::WEB_ROOT.into())
        }),
        seed: platform::seed(),
        config: launch.startup,
        ..Options::default()
    };
    // `--rpc <socket>`: serve the JSON-RPC command stream (D27, docs/spec/rpc.md).
    #[cfg(not(target_arch = "wasm32"))]
    let ran = match &launch.rpc {
        Some(socket) => schemify_app::serve::run(opts, socket),
        None => schemify_app::window::run(opts),
    };
    #[cfg(target_arch = "wasm32")]
    let ran = match launch.rpc {
        Some(_) => Err("--rpc needs a native build (no sockets in a browser)".to_owned()),
        None => schemify_app::window::run(opts),
    };
    ran.map_err(|e| format!("{e}\n"))
}
