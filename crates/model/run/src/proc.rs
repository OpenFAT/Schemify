//! The crate's one subprocess boundary (run spec §5, D15): simulators and compilers are
//! spawned, reaped and killed only through [`Proc`], never waited for while they run. In:
//! a `Command` and its log files; out: an exit status or why the child was killed.
//!
//! Contents:
//! - crate-only `struct Proc`: `spawn`, `reap`
//! - crate-only `fn spawn_error(prog, what, config, e) -> String`
//! - crate-only `fn last_line(path: &Path) -> Option<String>`

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// A child process and its start time. Dropping it kills and reaps the child, so no
/// table that holds one leaks a process or a zombie.
pub(crate) struct Proc {
    child: Child,
    started: Instant,
}

impl Proc {
    /// Spawns `cmd` with stdin closed and stdout/stderr into `out`/`err`.
    pub(crate) fn spawn(cmd: &mut Command, out: File, err: File) -> io::Result<Proc> {
        let child = cmd.stdin(Stdio::null()).stdout(out).stderr(err).spawn()?;
        Ok(Proc {
            child,
            started: Instant::now(),
        })
    }

    /// Returns `None` while the child runs within `timeout`, else its outcome: the exit
    /// status, or why it was killed (timeout, or `try_wait` failed). Never blocks on a
    /// live child.
    pub(crate) fn reap(
        &mut self,
        timeout: Duration,
        what: &str,
    ) -> Option<Result<ExitStatus, String>> {
        let outcome = match self.child.try_wait() {
            Ok(Some(status)) => Ok(status),
            Ok(None) if self.started.elapsed() <= timeout => return None,
            Ok(None) => Err(format!("timed out after {timeout:?}")),
            Err(e) => Err(format!("cannot wait for the {what}: {e}")),
        };
        if outcome.is_err() {
            self.kill();
        }
        Some(outcome)
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The message for a program that did not start: not found (with where to set its path)
/// or the OS error.
pub(crate) fn spawn_error(prog: &str, what: &str, config: &str, e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::NotFound {
        format!("{what} `{prog}` not found on PATH (install it or set its path in {config})")
    } else {
        format!("cannot start {what} `{prog}`: {e}")
    }
}

/// The last non-blank line of the file at `path`, trimmed; `None` when it cannot be
/// read or has none.
pub(crate) fn last_line(path: &Path) -> Option<String> {
    let text = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&text);
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_owned())
}

// ---- private ----

impl Proc {
    /// Kills the child and reaps it (no zombie); errors mean it already exited.
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
