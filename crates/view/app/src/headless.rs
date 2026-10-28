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
//! - `pub fn command(options: Options, name: &str, args: &[String]) -> u8`

use std::thread;
use std::time::Duration;

use schemify_cli::rpc::Json;
use schemify_editor::editor::{Editor, Options};
use schemify_editor::platform;
use schemify_editor::rpc::{self, Dispatched};
use schemify_ui::InputEvents;

use crate::paint;

/// How long one command may keep the editor busy (a run's timeout, run spec §5).
const TIMEOUT_MS: u64 = 3_600_000;
/// The viewport the frames run at (no window: layout only).
const VIEW: (f32, f32) = (1280.0, 800.0);
/// Longest sleep between frames while something is pending.
const TICK_MAX_MS: u32 = 20;

// The timeout message prints whole seconds; a tick is shorter than the timeout.
const _: () = assert!(TIMEOUT_MS >= 1000);
const _: () = assert!(TICK_MAX_MS >= 1);
const _: () = assert!((TICK_MAX_MS as u64) < TIMEOUT_MS);

/// One command's frame clock and the last plugin log line printed.
struct Run {
    start_ms: u64,
    last_line: String,
}

/// Runs `name` with `args` in the editor `options` opens; returns the exit status (0, 1 or
/// 2, as the module header lists). Prints the editor's open error or the refusal on stderr.
///
/// # Panics
///
/// A plugin log line is left untaken after the last frame (a bug: every frame takes and
/// prints the log).
#[must_use = "the exit status says whether the command failed"]
pub fn command(options: Options, name: &str, args: &[String]) -> u8 {
    let mut editor = match Editor::new(options) {
        Ok(editor) => editor,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    editor.keep_plugin_log();
    let mut run = Run {
        start_ms: platform::unix_ms(),
        last_line: String::new(),
    };
    // The first frames load the plugins; a plugin on a worker (a raised `fuel`,
    // plugin-abi §2) composes its commands a delivery later, so run until nothing is pending.
    run.frame(&mut editor);
    run.frame(&mut editor);
    while let Some(ms) = editor.wake_ms() {
        if run.timed_out() {
            break;
        }
        sleep(ms);
        run.frame(&mut editor);
    }
    let params: Vec<Json> = args.iter().map(|arg| Json::Str(arg.clone())).collect();
    let ticket = match rpc::dispatch(&mut editor, name, &Json::Arr(params)) {
        Dispatched::Queued(ticket) => ticket,
        Dispatched::Ready(Ok(_)) => return 0,
        Dispatched::Ready(Err((_, error))) => {
            eprintln!("{error}");
            return 2;
        }
    };
    // Bounded by TIMEOUT_MS.
    let mut said = None;
    let said = loop {
        run.frame(&mut editor);
        said = said.or_else(|| editor.take_said(ticket));
        let pending = editor.wake_ms();
        if pending.is_none()
            && let Some(said) = said.take()
        {
            break said;
        }
        if run.timed_out() {
            eprintln!("{name}: still busy after {} s", TIMEOUT_MS / 1000);
            return 1;
        }
        sleep(pending.unwrap_or(1));
    };
    run.frame(&mut editor);
    let (lines, failed) = editor.take_plugin_log();
    assert!(
        lines.is_empty(),
        "`Run::frame` printed and took every log line"
    );
    // The answer of a plugin command is its last log line: printed once.
    if !said.is_empty() && !run.last_line.ends_with(&said) {
        println!("{said}");
    }
    u8::from(failed)
}

// ---- private ----

impl Run {
    /// One frame with no input, a PNG export it asked for, then the new plugin log lines.
    fn frame(&mut self, editor: &mut Editor) {
        editor.update(&InputEvents::default(), VIEW, platform::unix_ms());
        paint::serve_raster(editor);
        let (lines, _) = editor.take_plugin_log();
        for line in lines {
            println!("{line}");
            self.last_line = line;
        }
    }

    fn timed_out(&self) -> bool {
        platform::unix_ms().saturating_sub(self.start_ms) > TIMEOUT_MS
    }
}

/// Sleeps until the editor's next deadline, at least 1 ms and at most [`TICK_MAX_MS`].
fn sleep(ms: u32) {
    let ms = ms.clamp(1, TICK_MAX_MS);
    thread::sleep(Duration::from_millis(u64::from(ms)));
}
