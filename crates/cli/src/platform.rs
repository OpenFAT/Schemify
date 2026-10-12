//! The OS boundary (ARCHITECTURE: only `cli` and `app` talk to the OS), split by `cfg` (D31):
//! native and WASI builds use `std::fs`, child processes and the OS clipboard tools; the web
//! build (`wasm32-unknown-unknown`) keeps OPFS in a synchronous in-memory table written back
//! asynchronously, uses the async Clipboard API, and turns exports into downloads (web spec,
//! `docs/spec/web.md`). Both sides offer the same functions, so the spines never branch on
//! the platform.
//!
//! Every path here is absolute or relative to the process working directory (on the web,
//! `/`); callers build them from the project root. Errors are plain text for the status line.

use std::path::{Path, PathBuf};

/// Directory entries visited per listing; deeper or larger trees are cut (a library is one
/// directory of cells, D10, so real ones are far below this).
pub const LIST_FILES_MAX: usize = 1 << 16;
/// Directory depth walked by [`list_files`].
pub const LIST_DEPTH_MAX: u32 = 8;
/// The project the web build opens when its URL names none (web spec §1); imported projects
/// sit beside it under `/projects`.
pub const WEB_ROOT: &str = "/projects/default";
/// The error [`clipboard_get`] returns on the web while the browser has not answered yet;
/// [`clipboard_ready`] turns true once it has (web spec §4). Never returned natively.
pub const CLIPBOARD_PENDING: &str = "clipboard: waiting for the browser";

#[cfg(any(test, all(target_arch = "wasm32", target_os = "unknown")))]
mod vfs;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use web as imp;

// WASI (the CLI under a wasm runtime, CI's determinism job) has `std::fs`: it takes the native
// path. Only the browser (`wasm32-unknown-unknown`) lacks a file system.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
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
        schemify_config::dirs::here(schemify_config::dirs::Dir::State).ok()
    }

    // The clipboard tools answer synchronously: nothing is ever pending.
    pub fn clipboard_ready() -> bool {
        false
    }

    pub fn wake_ms() -> Option<u32> {
        None
    }

    pub fn export(path: &Path, bytes: &[u8]) -> Result<(), String> {
        write_atomic(path, bytes)
    }

    pub fn import_project(_folder: bool) -> Result<(), String> {
        Err("a native project is a directory: open it (schemify edit <dir>)".into())
    }

    pub fn args() -> Vec<String> {
        std::env::args().skip(1).collect()
    }

    pub fn unix_ms() -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64)
        }
        // WASI (CI's determinism runs under wasmtime): no clock.
        #[cfg(target_arch = "wasm32")]
        {
            0
        }
    }

    pub fn seed() -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        let pid = u64::from(std::process::id());
        #[cfg(target_arch = "wasm32")]
        let pid = 0u64; // WASI: no process ids
        unix_ms() ^ (pid << 32) ^ 0x9e37_79b9_7f4a_7c15
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
    imp::spawn(cwd, argv, stdin)
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

/// The program's arguments after its name. On the web, where there is no command line, the
/// URL fragment is the one argument: `index.html#/projects/inv/top.shcomp` opens that cell,
/// `#/projects/inv` that project (percent-decoded; empty without a fragment, web spec §1).
pub fn args() -> Vec<String> {
    imp::args()
}

/// Mounts the web storage (loads every OPFS file into memory, web spec §2), installs the
/// guard that asks before the page closes with writes still pending, then calls `run`.
/// Returns at once; `run` follows when the browser has answered. A storage the browser
/// refuses leaves the page editing in memory, reported on the console, as is an error from
/// `run`.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub fn mount_then(run: impl FnOnce() -> Result<(), String> + 'static) {
    imp::mount_then(run);
}

/// Delivers an export: natively writes `bytes` to `path` ([`write_atomic`]); on the web the
/// browser downloads them as `path`'s file name and nothing is written (web spec §3).
///
/// # Errors
///
/// The write failed, or the page cannot start a download.
pub fn export(path: &Path, bytes: &[u8]) -> Result<(), String> {
    imp::export(path, bytes)
}

/// Packs the project at `root` into `<root>/.schemify/export/<name>.zip` (`<name>`: the
/// directory's name; every entry under `<name>/`), or downloads it on the web: every file
/// [`list_files`] lists, so hidden entries (`.schemify`, `.git`, journals) stay out. Returns
/// the archive's path. Stored entries, fixed timestamps: the same project gives the same
/// bytes everywhere (rule 4).
///
/// # Errors
///
/// A file cannot be read, the archive is past the zip limits ([`crate::zip::write`]), or
/// the export fails.
pub fn export_project(root: &Path) -> Result<PathBuf, String> {
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("project");
    let files = list_files(root, &[]);
    let mut names = Vec::with_capacity(files.len());
    let mut bytes = Vec::with_capacity(files.len());
    for f in &files {
        let Some(rel) = crate::libs::rel_path(root, f) else {
            continue;
        };
        names.push(format!("{name}/{rel}"));
        bytes.push(read(f)?);
    }
    let entries: Vec<(&str, &[u8])> = names
        .iter()
        .map(String::as_str)
        .zip(bytes.iter().map(Vec::as_slice))
        .collect();
    let zip = crate::zip::write(&entries)?;
    let out = root
        .join(".schemify")
        .join("export")
        .join(format!("{name}.zip"));
    export(&out, &zip)?;
    assert_eq!(names.len(), bytes.len());
    Ok(out)
}

/// Starts a project import on the web: the browser's file picker for a `.zip` (or, with
/// `folder`, a directory); the picked files land in `/projects/<name>` and the page reloads
/// on that project (web spec §3). Must run inside a user gesture (a menu click or key).
///
/// # Errors
///
/// Natively always (a native project is a directory to open); on the web when the page
/// cannot show a picker. Failures after the pick go to the browser console.
pub fn import_project(folder: bool) -> Result<(), String> {
    imp::import_project(folder)
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
/// never leaves a half-written design file (edits spec §5). On the web the write is
/// visible at once and reaches OPFS shortly after, whole (web spec §2).
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
/// The OS error, prefixed with the path (also for a directory).
pub fn remove(path: &Path) -> Result<(), String> {
    imp::remove(path)
}

/// Modification time in seconds since the Unix epoch; `None` when unknown.
pub fn mtime_unix_s(path: &Path) -> Option<u64> {
    imp::mtime_unix_s(path)
}

/// Puts `text` on the system clipboard through the platform's clipboard program
/// (`wl-copy`, `xclip`, `xsel`, `pbcopy`, PowerShell `Set-Clipboard`), blocking until the
/// program has read it (milliseconds). On the web: the Clipboard API's `writeText`, which
/// completes later; a refusal is logged to the console (web spec §4).
///
/// # Errors
///
/// No clipboard program could take the text (the last failure); on the web, no clipboard
/// in this context (an insecure origin).
pub fn clipboard_set(text: &str) -> Result<(), String> {
    imp::clipboard_set(text)
}

/// The system clipboard's text (at most [`CLIPBOARD_BYTES_MAX`] bytes), read the same way.
/// On the web the browser answers asynchronously: the first call starts the read and
/// returns [`CLIPBOARD_PENDING`]; once [`clipboard_ready`] is true, the next call returns
/// the answer.
///
/// # Errors
///
/// No clipboard program answered, or the text is not UTF-8; on the web also
/// [`CLIPBOARD_PENDING`], or the browser refused the read (permission).
pub fn clipboard_get() -> Result<String, String> {
    imp::clipboard_get()
}

/// True once after a pending web clipboard read finished: the caller repeats the paste
/// that got [`CLIPBOARD_PENDING`]. Always false natively.
pub fn clipboard_ready() -> bool {
    imp::clipboard_ready()
}

/// Milliseconds until the caller should poll again without input because the browser owes
/// an answer ([`clipboard_ready`]); `None` when nothing is pending (always natively), so an
/// idle editor sleeps (D39).
pub fn wake_ms() -> Option<u32> {
    imp::wake_ms()
}

/// The user state directory, `stdpath("state")` of the config spec §1 (honours
/// `SCHEMIFY_APPNAME`): the recent-files list lives here; `/state` on the web. `None` when
/// the environment names none.
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

/// Milliseconds since the Unix epoch (journal stamps, file times); the browser's clock on
/// the web; 0 under WASI, which has no clock in CI's determinism runs.
pub fn unix_ms() -> u64 {
    imp::unix_ms()
}

/// A seed for id generation (D9): clock and process id natively, `Math.random` on the web,
/// so two sessions editing one file do not draw the same ids.
pub fn seed() -> u64 {
    imp::seed()
}
