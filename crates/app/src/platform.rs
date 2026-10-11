//! The OS boundary (ARCHITECTURE: only `app` talks to the OS), split by `cfg` (D31):
//! native builds use `std::fs`; the web build will use OPFS through generated glue (D2).
//!
//! Every path here is absolute or relative to the process working directory; callers build
//! them from the project root. Errors are plain text for the status line.

use std::path::{Path, PathBuf};

/// Directory entries visited per listing; deeper or larger trees are cut (a library is one
/// directory of cells, D10, so real ones are far below this).
pub const LIST_FILES_MAX: usize = 1 << 16;
/// Directory depth walked by [`list_files`].
pub const LIST_DEPTH_MAX: u32 = 8;

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::*;
    use std::io::Write;

    /// Plugin-ABI `E_GONE` (§8): a spawned binary that cannot start. Spelled here because
    /// the headless build has no plugin crate.
    const E_GONE: i32 = 2;

    pub fn read(path: &Path) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn exists(path: &Path) -> bool {
        path.exists()
    }

    pub fn is_dir(path: &Path) -> bool {
        path.is_dir()
    }

    /// Writes a sibling temp file, then renames it over `path` (edits spec §5).
    pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".schemify-tmp");
        std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn append(path: &Path, bytes: &[u8]) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        // ponytail: no fsync batching (edits spec §3 asks ≤ 100 ms); the OS flushes. Add a
        // timer-driven fsync when the frame loop gets timers.
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        f.write_all(bytes)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn spawn(cwd: &Path, argv: &[String], stdin: &[u8]) -> (i32, Vec<u8>) {
        use std::process::{Command, Stdio};
        let child = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                let gone = -E_GONE;
                return (gone, format!("{}: {e}", argv[0]).into_bytes());
            }
        };
        if let Some(mut pipe) = child.stdin.take() {
            // A child that exits without reading its input is not an error.
            let _ = pipe.write_all(stdin);
        }
        match child.wait_with_output() {
            Ok(o) => {
                let mut bytes = o.stdout;
                bytes.extend_from_slice(&o.stderr);
                // A signal kill has no code; report it as a failure, never as success.
                (o.status.code().unwrap_or(-1).max(-1), bytes)
            }
            Err(e) => (-1, e.to_string().into_bytes()),
        }
    }

    pub fn remove(path: &Path) -> Result<(), String> {
        std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn mtime_unix_s(path: &Path) -> Option<u64> {
        let t = std::fs::metadata(path).ok()?.modified().ok()?;
        t.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs())
    }

    /// The platform's clipboard programs: (write argv, read argv) candidates in order.
    fn clipboard_tools() -> Vec<(Vec<&'static str>, Vec<&'static str>)> {
        if cfg!(target_os = "macos") {
            return vec![(vec!["pbcopy"], vec!["pbpaste"])];
        }
        if cfg!(target_os = "windows") {
            return vec![(
                vec![
                    "powershell",
                    "-NoProfile",
                    "-Command",
                    "$input | Set-Clipboard",
                ],
                vec!["powershell", "-NoProfile", "-Command", "Get-Clipboard -Raw"],
            )];
        }
        let mut t = Vec::new();
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            t.push((vec!["wl-copy"], vec!["wl-paste", "--no-newline"]));
        }
        t.push((
            vec!["xclip", "-selection", "clipboard"],
            vec!["xclip", "-selection", "clipboard", "-o"],
        ));
        t.push((
            vec!["xsel", "--clipboard", "--input"],
            vec!["xsel", "--clipboard", "--output"],
        ));
        t
    }

    pub fn clipboard_set(text: &str) -> Result<(), String> {
        use std::process::{Command, Stdio};
        let mut last = String::from("no clipboard program");
        for (write, _) in clipboard_tools() {
            // stdout/stderr are not piped: X11/Wayland tools fork a server that keeps them
            // open, and waiting for their end would hang the frame.
            let child = Command::new(write[0])
                .args(&write[1..])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    last = format!("{}: {e}", write[0]);
                    continue;
                }
            };
            if let Some(mut pipe) = child.stdin.take() {
                let _ = pipe.write_all(text.as_bytes());
            }
            match child.wait() {
                Ok(s) if s.success() => return Ok(()),
                Ok(s) => last = format!("{}: {s}", write[0]),
                Err(e) => last = format!("{}: {e}", write[0]),
            }
        }
        Err(last)
    }

    pub fn clipboard_get() -> Result<String, String> {
        use std::process::Command;
        let mut last = String::from("no clipboard program");
        for (_, read) in clipboard_tools() {
            match Command::new(read[0]).args(&read[1..]).output() {
                Ok(o) if o.status.success() => {
                    let mut b = o.stdout;
                    b.truncate(super::CLIPBOARD_BYTES_MAX);
                    return String::from_utf8(b).map_err(|_| "clipboard is not UTF-8".into());
                }
                Ok(o) => last = format!("{}: {}", read[0], o.status),
                Err(e) => last = format!("{}: {e}", read[0]),
            }
        }
        Err(last)
    }

    pub fn state_dir() -> Option<PathBuf> {
        let env = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        if cfg!(target_os = "windows") {
            return env("LOCALAPPDATA").map(|d| d.join("schemify"));
        }
        env("XDG_STATE_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".local").join("state")))
            .map(|d| d.join("schemify"))
    }

    pub fn list(dir: &Path, depth: u32, skip: &[PathBuf], out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            if out.len() >= LIST_FILES_MAX {
                return;
            }
            let hidden = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_none_or(|n| n.starts_with('.') || n == "target");
            if hidden || skip.contains(&p) {
                continue;
            }
            if p.is_dir() {
                if depth < LIST_DEPTH_MAX {
                    list(&p, depth + 1, skip, out);
                }
            } else {
                out.push(p);
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    // TODO(D31): OPFS through web-sys (generated glue only, D2). Until then the web build
    // edits in memory: reads fail, writes are refused, listings are empty.
    use super::*;

    const NO_OPFS: &str = "web storage (OPFS) is not wired yet";

    pub fn read(_: &Path) -> Result<Vec<u8>, String> {
        Err(NO_OPFS.into())
    }
    pub fn exists(_: &Path) -> bool {
        false
    }
    pub fn is_dir(_: &Path) -> bool {
        false
    }
    pub fn write_atomic(_: &Path, _: &[u8]) -> Result<(), String> {
        Err(NO_OPFS.into())
    }
    pub fn append(_: &Path, _: &[u8]) -> Result<(), String> {
        Err(NO_OPFS.into())
    }
    pub fn list(_: &Path, _: u32, _: &[PathBuf], _: &mut Vec<PathBuf>) {}
    pub fn remove(_: &Path) -> Result<(), String> {
        Err(NO_OPFS.into())
    }
    pub fn mtime_unix_s(_: &Path) -> Option<u64> {
        None
    }
    // TODO(D2): the async Clipboard API through web-sys; the editor keeps its own clipboard.
    pub fn clipboard_set(_: &str) -> Result<(), String> {
        Err("no system clipboard on the web yet".into())
    }
    pub fn clipboard_get() -> Result<String, String> {
        Err("no system clipboard on the web yet".into())
    }
    pub fn state_dir() -> Option<PathBuf> {
        None
    }
    pub fn spawn(_: &Path, _: &[String], _: &[u8]) -> (i32, Vec<u8>) {
        // D16: no processes in a browser; simulation goes through runner plugins.
        (-1, b"no processes on the web".to_vec())
    }
}

/// Runs `argv` (a binary on `PATH` and its arguments; the plugin host already checked the
/// `spawn:<binary>` grant) in `cwd` with `stdin`, waiting for it to exit. Returns the exit
/// status (`-1` for a signal or a wait failure, a negated `E_GONE` when it cannot start) and
/// stdout followed by stderr: the `IoDone` of a plugin `spawn` (plugin-abi §5).
///
/// Blocks the caller; the editor uses [`spawn_start`] instead.
///
/// # Panics
///
/// Panics if `argv` is empty.
pub fn spawn(cwd: &Path, argv: &[String], stdin: &[u8]) -> (i32, Vec<u8>) {
    assert!(!argv.is_empty(), "spawn needs a binary");
    let (status, out) = imp::spawn(cwd, argv, stdin);
    (status, out)
}

/// A child process started by [`spawn_start`]: poll it with [`Spawned::try_finish`].
#[derive(Debug)]
pub struct Spawned {
    #[cfg(not(target_arch = "wasm32"))]
    rx: std::sync::mpsc::Receiver<(i32, Vec<u8>)>,
    #[cfg(not(target_arch = "wasm32"))]
    taken: bool,
    #[cfg(target_arch = "wasm32")]
    done: Option<(i32, Vec<u8>)>,
}

/// Starts [`spawn`] without waiting: the child runs and its pipes drain on a worker thread
/// (one per child; the plugin host bounds them by its `io` queue, 256 per frame), so the
/// frame spine never blocks on a plugin's process (D39). On the web nothing can start and the
/// result is ready at once (D16).
///
/// # Panics
///
/// Panics if `argv` is empty.
pub fn spawn_start(cwd: &Path, argv: Vec<String>, stdin: Vec<u8>) -> Spawned {
    assert!(!argv.is_empty(), "spawn needs a binary");
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let cwd = cwd.to_path_buf();
        // A failed send means the editor dropped the request (it shut down): nothing to do.
        std::thread::spawn(move || {
            let _ = tx.send(imp::spawn(&cwd, &argv, &stdin));
        });
        Spawned { rx, taken: false }
    }
    #[cfg(target_arch = "wasm32")]
    {
        Spawned {
            done: Some(imp::spawn(cwd, &argv, &stdin)),
        }
    }
}

impl Spawned {
    /// The child's `(status, stdout + stderr)` once it exited; `None` while it runs. Returns
    /// the result once; later calls return `None`.
    pub fn try_finish(&mut self) -> Option<(i32, Vec<u8>)> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.taken {
                return None;
            }
            let r = match self.rx.try_recv() {
                Ok(r) => r,
                Err(std::sync::mpsc::TryRecvError::Empty) => return None,
                // The worker died without sending (only on OOM): a failure, reported once.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    (-1, b"spawn worker vanished".to_vec())
                }
            };
            self.taken = true;
            Some(r)
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.done.take()
        }
    }
}

/// Reads a whole file.
///
/// # Errors
///
/// The OS error, prefixed with the path.
pub fn read(path: &Path) -> Result<Vec<u8>, String> {
    imp::read(path)
}

/// Returns whether `path` exists.
pub fn exists(path: &Path) -> bool {
    imp::exists(path)
}

/// Returns whether `path` is a directory.
pub fn is_dir(path: &Path) -> bool {
    imp::is_dir(path)
}

/// Writes `bytes` atomically (temp file + rename), creating parent directories, so a crash
/// never leaves a half-written design file (edits spec §5).
///
/// # Errors
///
/// The OS error, prefixed with the path.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    imp::write_atomic(path, bytes)
}

/// Appends `bytes` to `path`, creating it and its directories.
///
/// # Errors
///
/// The OS error, prefixed with the path.
pub fn append(path: &Path, bytes: &[u8]) -> Result<(), String> {
    imp::append(path, bytes)
}

/// Bytes read from the system clipboard at most (a fragment of 2^22 records is far below).
pub const CLIPBOARD_BYTES_MAX: usize = 64 << 20;

/// Deletes a file.
///
/// # Errors
///
/// The OS error, prefixed with the path (the web build refuses).
pub fn remove(path: &Path) -> Result<(), String> {
    imp::remove(path)
}

/// Modification time in seconds since the Unix epoch; `None` when unknown.
pub fn mtime_unix_s(path: &Path) -> Option<u64> {
    imp::mtime_unix_s(path)
}

/// Puts `text` on the system clipboard through the platform's clipboard program
/// (`wl-copy`, `xclip`, `xsel`, `pbcopy`, PowerShell `Set-Clipboard`). Blocks until the
/// program has read it (milliseconds).
///
/// # Errors
///
/// No clipboard program could take the text (the last failure); always on the web.
pub fn clipboard_set(text: &str) -> Result<(), String> {
    imp::clipboard_set(text)
}

/// The system clipboard's text (at most [`CLIPBOARD_BYTES_MAX`] bytes), read the same way.
///
/// # Errors
///
/// No clipboard program answered, or the text is not UTF-8.
pub fn clipboard_get() -> Result<String, String> {
    imp::clipboard_get()
}

/// The user state directory (`$XDG_STATE_HOME/schemify`, else `~/.local/state/schemify`;
/// `%LOCALAPPDATA%\schemify` on Windows): the recent-files list lives here. `None` when the
/// environment names none (and on the web).
// ponytail: a local copy of the Neovim-style `stdpath("state")`; the config crate's
// stdpath replaces it at merge (integrator note, 2026-10-11).
pub fn state_dir() -> Option<PathBuf> {
    imp::state_dir()
}

/// Lists regular files under `dir` recursively in sorted order, skipping hidden entries,
/// `target`, and the directories in `skip`. Bounded by [`LIST_FILES_MAX`] files and
/// [`LIST_DEPTH_MAX`] levels.
pub fn list_files(dir: &Path, skip: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    imp::list(dir, 0, skip, &mut out);
    assert!(out.len() <= LIST_FILES_MAX);
    out
}

/// Milliseconds since the Unix epoch (journal stamps); 0 where no clock is available.
pub fn unix_ms() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// A seed for id generation (OS entropy where available, D9).
pub fn seed() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    let pid = u64::from(std::process::id());
    #[cfg(target_arch = "wasm32")]
    let pid = 0u64; // no process ids in a browser; TODO(D31): crypto.getRandomValues
    unix_ms() ^ (pid << 32) ^ 0x9e37_79b9_7f4a_7c15
}
