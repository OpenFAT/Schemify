//! `headless`: one command and its arguments → the command run in an editor with no
//! window, its plugin log on stdout and an exit status (`schemify cmd <project-dir|cell-file>
//! <command> [args…]`, `schemify-gui cmd …`; D27, `docs/spec/rpc.md`). The command is a
//! core command or a plugin's (from the user config's packages or the project manifest):
//! the command line drives the same command table the UI and plugins use, so a plugin's
//! command (an import, a characterization) runs in CI exactly as from a menu.
//!
//! The editor opens as the window would, the command is queued as the JSON-RPC stream
//! queues one (`schemify_editor::rpc::dispatch`), and frames run until it has answered and
//! nothing is pending (no plugin event, I/O completion, child process, worker or run in
//! flight: `Editor::wake_ms` is `None`). Every plugin log line is printed as it arrives,
//! then the command's answer.
//!
//! Exit status: 0 done; 1 a plugin logged an error or a call aborted (out of fuel, a trap),
//! or an hour passed (a run's timeout, run spec §5); 2 the editor could not open, or the
//! command or its arguments are refused.
//!
//! Contents:
//! - `pub fn command(opts: Options, name: &str, args: &[String]) -> u8`

use schemify_cli::rpc::Json;
use schemify_editor::editor::{Editor, Options};
use schemify_editor::platform;
use schemify_editor::rpc::{self, Dispatched};
use schemify_ui::InputEvents;

/// How long one command may keep the editor busy (a run's timeout, run spec §5).
const TIMEOUT_MS: u64 = 3_600_000;
/// The viewport the frames run at (no window: layout only).
const VIEW: (f32, f32) = (1280.0, 800.0);
/// Longest sleep between frames while something is pending.
const TICK_MAX_MS: u32 = 20;

/// One command's frame clock and the last plugin log line printed.
struct Run {
    start: u64,
    last: String,
}

/// Runs `name` with `args` in the editor `opts` opens; returns the exit status (0, 1 or 2,
/// as the module header lists). Prints the editor's open error or the refusal on stderr.
pub fn command(opts: Options, name: &str, args: &[String]) -> u8 {
    let mut ed = match Editor::new(opts) {
        Ok(ed) => ed,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    ed.keep_plugin_log();
    let mut run = Run {
        start: platform::unix_ms(),
        last: String::new(),
    };
    // The first frames load the plugins; a plugin on a worker (a raised `fuel`,
    // plugin-abi §2) composes its commands a delivery later, so run until nothing is pending.
    run.frame(&mut ed);
    run.frame(&mut ed);
    while let Some(ms) = ed.wake_ms() {
        if run.timed_out() {
            break;
        }
        sleep(ms);
        run.frame(&mut ed);
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
    // Bounded by TIMEOUT_MS.
    let mut said = None;
    loop {
        run.frame(&mut ed);
        said = said.or_else(|| ed.take_said(ticket));
        let pending = ed.wake_ms();
        if said.is_some() && pending.is_none() {
            break;
        }
        if run.timed_out() {
            eprintln!("{name}: still busy after {} s", TIMEOUT_MS / 1000);
            return 1;
        }
        sleep(pending.unwrap_or(1));
    }
    run.frame(&mut ed);
    let failed = ed.take_plugin_log().1;
    let said = said.expect("the loop leaves with an answer");
    // The answer of a plugin command is its last log line: printed once.
    if !said.is_empty() && !run.last.ends_with(&said) {
        println!("{said}");
    }
    u8::from(failed)
}

// ---- private ----

impl Run {
    /// One frame with no input, a PNG export it asked for, then the new plugin log lines.
    fn frame(&mut self, ed: &mut Editor) {
        ed.update(&InputEvents::default(), VIEW, platform::unix_ms());
        crate::paint::serve_raster(ed);
        for line in ed.take_plugin_log().0 {
            println!("{line}");
            self.last = line;
        }
    }

    fn timed_out(&self) -> bool {
        platform::unix_ms().saturating_sub(self.start) > TIMEOUT_MS
    }
}

/// Sleeps until the editor's next deadline, at least 1 ms and at most [`TICK_MAX_MS`].
fn sleep(ms: u32) {
    let ms = ms.clamp(1, TICK_MAX_MS);
    std::thread::sleep(std::time::Duration::from_millis(u64::from(ms)));
}
