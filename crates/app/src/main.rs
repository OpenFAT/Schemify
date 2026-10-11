//! `schemify-gui`: the editor (D39), started directly or by `schemify edit`.
//!
//! `schemify-gui [--clean | -u <init.lua>] [edit] [<project-dir>|<cell-file>] [--no-default-ui]
//! [--rpc <socket>]`
//! Exit codes: 0 closed normally, 2 usage or startup errors.
//!
//! On the web the URL fragment is the argument (`index.html#/projects/inv/top.shcomp`) and
//! the browser storage is mounted before the editor starts (web spec §1–§2).

use schemify_app::platform;
use schemify_config::Startup;

const USAGE: &str = "\
usage: schemify-gui [--clean | -u <init.lua>] [<project-dir>|<cell-file>] [--no-default-ui]
                    [--rpc <socket>]
  --rpc  serve the JSON-RPC command stream on a socket path or 127.0.0.1:<port>
";

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn main() -> std::process::ExitCode {
    match start(platform::args()) {
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
    platform::mount_then(|| start(platform::args()));
}

/// Parses the arguments and runs the editor (native: until its window closes; web: returns
/// once the window is attached). Errors end with a newline.
fn start(args: Vec<String>) -> Result<(), String> {
    let (config, first) = Startup::parse(&args).map_err(|e| format!("{e}\n{USAGE}"))?;
    // `cargo run -p schemify-app -- edit <path>` keeps working: the word is optional.
    let mut rest: Vec<&String> = args[first..]
        .iter()
        .enumerate()
        .filter(|&(i, a)| !(i == 0 && a == "edit"))
        .map(|(_, a)| a)
        .collect();
    // `--rpc <socket>`: serve the JSON-RPC command stream (D27, docs/spec/rpc.md).
    let rpc = match rest.iter().position(|a| *a == "--rpc") {
        Some(i) if i + 1 < rest.len() => {
            let socket = rest.remove(i + 1).clone();
            rest.remove(i);
            Some(socket)
        }
        Some(_) => return Err(USAGE.into()),
        None => None,
    };
    let no_default_ui = rest.iter().any(|a| *a == "--no-default-ui");
    let unknown = rest
        .iter()
        .any(|a| a.starts_with('-') && *a != "--no-default-ui");
    let paths: Vec<&&String> = rest.iter().filter(|a| !a.starts_with('-')).collect();
    if paths.len() > 1 || unknown {
        return Err(USAGE.into());
    }
    // The web build has no `sim` field (D16: no processes), so the update is empty there.
    #[cfg_attr(target_arch = "wasm32", allow(clippy::needless_update))]
    let opts = schemify_app::editor::Options {
        no_default_ui,
        // A URL without a fragment opens the default web project (web spec §1).
        path: paths
            .first()
            .map(|p| std::path::PathBuf::from(p.as_str()))
            .or_else(|| {
                cfg!(all(target_arch = "wasm32", target_os = "unknown"))
                    .then(|| platform::WEB_ROOT.into())
            }),
        seed: platform::seed(),
        config,
        ..schemify_app::editor::Options::default()
    };
    #[cfg(not(target_arch = "wasm32"))]
    let ran = match &rpc {
        Some(socket) => schemify_app::rpc::run(opts, socket),
        None => schemify_app::editor::run(opts),
    };
    #[cfg(target_arch = "wasm32")]
    let ran = match rpc {
        Some(_) => Err("--rpc needs a native build (no sockets in a browser)".to_owned()),
        None => schemify_app::editor::run(opts),
    };
    ran.map_err(|e| {
        format!(
            "{e}
"
        )
    })
}
