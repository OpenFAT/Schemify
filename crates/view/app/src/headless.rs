//! `schemify cmd <project-dir|cell-file> <command> [args…]` (`schemify-gui cmd …`): one
//! command of the editor's command table, a core command or a plugin's (from the user
//! config's packages or the project manifest), run without a window. D27: the command line
//! drives the same command table the UI and plugins use, so a plugin's command (an import, a
//! characterization) runs in CI exactly as from a menu.
//!
//! The editor opens as the window would, the command is queued as the JSON-RPC stream queues
//! one ([`rpc::dispatch`]), and frames run until it has answered and nothing is pending (no
//! plugin event, I/O completion, child process, worker or run in flight: [`Editor::wake_ms`]
//! is `None`). Every plugin log line is printed as it arrives, then the command's answer.
//!
//! Exit status: 0 done; 1 a plugin logged an error or a call aborted (out of fuel, a trap),
//! or [`TIMEOUT_MS`] passed; 2 the editor could not open, or the command or its arguments
//! are refused.

use schemify_cli::rpc::Json;
use schemify_ui::InputEvents;

use schemify_editor::editor::{Editor, Options};
use schemify_editor::platform;
use schemify_editor::rpc::{self, Dispatched};

/// How long one command may keep the editor busy (a run's timeout, run spec §5).
pub const TIMEOUT_MS: u64 = 3_600_000;
/// The viewport the frames run at (no window: layout only).
const VIEW: (f32, f32) = (1280.0, 800.0);
/// Longest sleep between frames while something is pending.
const TICK_MAX_MS: u32 = 20;

/// Runs `name` with `args` in the editor `opts` opens; returns the exit status.
pub fn command(opts: Options, name: &str, args: &[String]) -> u8 {
    let mut ed = match Editor::new(opts) {
        Ok(ed) => ed,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    ed.keep_plugin_log();
    let start = platform::unix_ms();
    // The answer of a plugin command is its last log line: printed once.
    let mut last = String::new();
    let mut frame = |ed: &mut Editor| {
        ed.update(&InputEvents::default(), VIEW, platform::unix_ms());
        crate::paint::serve_raster(ed);
        for line in ed.take_plugin_log().0 {
            println!("{line}");
            last = line;
        }
    };
    // The first frames load the plugins; a plugin on a worker (a raised `fuel`, plugin-abi
    // §2) composes its commands a delivery later, so run until nothing is pending.
    frame(&mut ed);
    frame(&mut ed);
    while let Some(ms) = ed.wake_ms() {
        if platform::unix_ms().saturating_sub(start) > TIMEOUT_MS {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(u64::from(
            ms.clamp(1, TICK_MAX_MS),
        )));
        frame(&mut ed);
    }
    let params = Json::Arr(args.iter().map(|a| Json::Str(a.clone())).collect());
    let ticket = match rpc::dispatch(&mut ed, name, &params) {
        Dispatched::Queued(t) => t,
        Dispatched::Ready(Ok(_)) => return 0,
        Dispatched::Ready(Err((_, e))) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let mut said = None;
    // Bounded by TIMEOUT_MS.
    loop {
        frame(&mut ed);
        if said.is_none() {
            said = ed.take_said(ticket);
        }
        let pending = ed.wake_ms();
        if said.is_some() && pending.is_none() {
            break;
        }
        if platform::unix_ms().saturating_sub(start) > TIMEOUT_MS {
            eprintln!("{name}: still busy after {} s", TIMEOUT_MS / 1000);
            return 1;
        }
        let ms = pending.unwrap_or(1).clamp(1, TICK_MAX_MS);
        std::thread::sleep(std::time::Duration::from_millis(u64::from(ms)));
    }
    frame(&mut ed);
    let failed = ed.take_plugin_log().1;
    let said = said.expect("loop leaves with an answer");
    if !said.is_empty() && !last.ends_with(&said) {
        println!("{said}");
    }
    u8::from(failed)
}
