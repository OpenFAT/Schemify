//! The OS boundary (ARCHITECTURE: only `cli` and `app` talk to the OS), split by `cfg` (D31):
//! native and WASI builds use `std::fs`, child processes and the OS clipboard tools; the web
//! build (`wasm32-unknown-unknown`) keeps OPFS in a synchronous in-memory table written back
//! asynchronously, uses the async Clipboard API, and turns exports into downloads (web spec,
//! `docs/spec/web.md`). Both sides offer the same functions, so the spines never branch on
//! the platform.
//!
//! Every path here is absolute or relative to the process working directory (on the web,
//! `/`); callers build them from the project root. Errors are plain text for the status line.
//! The native side is `native`; the web side is `web` (the API), `opfs` (mount and
//! write-back) over `vfs` (the in-memory table).
//!
//! Contents:
//! - `pub const LIST_FILES_MAX: usize`, `pub const LIST_DEPTH_MAX: u32`,
//!   `pub const WEB_ROOT: &str`, `pub const CLIPBOARD_PENDING: &str`,
//!   `pub const CLIPBOARD_BYTES_MAX: usize`
//! - `pub struct Spawned`, `Spawned::try_finish`
//! - processes: `pub fn spawn`, `pub fn spawn_start`
//! - files: `pub fn read`, `read_tail`, `exists`, `is_dir`, `write_atomic`, `append`,
//!   `remove`, `remove_tree`, `mtime_unix_s`, `list_files`
//! - projects in and out: `pub fn export`, `export_project`, `import_project`, `mount_then`
//!   (web)
//! - clipboard: `pub fn clipboard_set`, `clipboard_get`, `clipboard_ready`, `wake_ms`
//! - environment: `pub fn args`, `env_var`, `state_dir`, `unix_ms`, `seed`

use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc::{self, Receiver, TryRecvError};
#[cfg(not(target_arch = "wasm32"))]
use std::thread;

use schemify_config::dirs::{self, Dir};

// WASI (the CLI under a wasm runtime, CI's determinism job) has `std::fs`: it takes the native
// path. Only the browser (`wasm32-unknown-unknown`) lacks a file system.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod native;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod opfs;
#[cfg(any(test, all(target_arch = "wasm32", target_os = "unknown")))]
mod vfs;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web;

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use native as imp;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use web as imp;

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
/// Bytes read from the system clipboard at most (a fragment of 2^22 records is far below).
pub const CLIPBOARD_BYTES_MAX: usize = 64 << 20;

/// A child process started by [`spawn_start`]: poll it with [`Spawned::try_finish`].
#[derive(Debug)]
pub struct Spawned {
    #[cfg(not(target_arch = "wasm32"))]
    rx: Receiver<(i32, Vec<u8>)>,
    #[cfg(not(target_arch = "wasm32"))]
    taken: bool,
    #[cfg(target_arch = "wasm32")]
    done: Option<(i32, Vec<u8>)>,
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
/// Panics if `argv` is empty or its binary is the empty string.
#[must_use]
pub fn spawn(cwd: &Path, argv: &[String], stdin: &[u8]) -> (i32, Vec<u8>) {
    assert!(!argv.is_empty(), "spawn needs a binary");
    assert!(!argv[0].is_empty(), "spawn needs a binary name");
    let (status, out) = imp::spawn(cwd, argv, stdin);
    assert!(status >= -2, "status {status}: below `-E_GONE`");
    (status, out)
}

/// Starts [`spawn`] without waiting: the child runs and its pipes drain on a worker thread
/// (one per child; the plugin host bounds them by its `io` queue, 256 per frame), so the
/// frame spine never blocks on a plugin's process (D39). On the web nothing can start and the
/// result is ready at once (D16).
///
/// # Panics
///
/// Panics if `argv` is empty or its binary is the empty string.
#[must_use]
pub fn spawn_start(cwd: &Path, argv: Vec<String>, stdin: Vec<u8>) -> Spawned {
    assert!(!argv.is_empty(), "spawn needs a binary");
    assert!(!argv[0].is_empty(), "spawn needs a binary name");
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (tx, rx) = mpsc::channel();
        let cwd = cwd.to_path_buf();
        // A failed send means the editor dropped the request (it shut down): nothing to do.
        thread::spawn(move || {
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
                Err(TryRecvError::Empty) => return None,
                // The worker died without sending (only on OOM): a failure, reported once.
                Err(TryRecvError::Disconnected) => (-1, b"spawn worker vanished".to_vec()),
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

/// The last `max` bytes of a file (the whole file when shorter), and whether bytes before
/// them were left out. Natively a seek, so a growing simulator log costs `max`, not its
/// length.
///
/// # Errors
///
/// The OS error, prefixed with the path.
///
/// # Panics
///
/// Panics if the platform side returns more than `max` bytes (a bug there).
pub fn read_tail(path: &Path, max: u64) -> Result<(Vec<u8>, bool), String> {
    let (bytes, cut) = imp::read_tail(path, max)?;
    assert!(bytes.len() as u64 <= max);
    Ok((bytes, cut))
}

/// The program's arguments after its name. On the web, where there is no command line, the
/// URL fragment is the one argument: `index.html#/projects/inv/top.shcomp` opens that cell,
/// `#/projects/inv` that project (percent-decoded; empty without a fragment, web spec §1).
#[must_use]
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
///
/// # Panics
///
/// Panics if a listed file is not under `root` (a bug in [`list_files`]).
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
        let rel = crate::libs::rel_path(root, f).expect("list_files lists under its root");
        names.push(format!("{name}/{rel}"));
        bytes.push(read(f)?);
    }
    assert_eq!(names.len(), files.len());
    assert_eq!(names.len(), bytes.len());
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
    assert!(out.starts_with(root));
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
#[must_use]
pub fn exists(path: &Path) -> bool {
    imp::exists(path)
}

/// Returns whether `path` is a directory.
#[must_use]
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

/// Deletes a file.
///
/// # Errors
///
/// The OS error, prefixed with the path (also for a directory).
pub fn remove(path: &Path) -> Result<(), String> {
    imp::remove(path)
}

/// Deletes a file, or a directory with everything under it (a plugin's `fs_remove`, plugin
/// ABI §5: the caller has already confined `path` to `.schemify/`). A symbolic link is
/// removed, never followed. On the web every file under the directory is removed from OPFS.
///
/// # Errors
///
/// The OS error, prefixed with the path; a missing `path` is an error.
pub fn remove_tree(path: &Path) -> Result<(), String> {
    imp::remove_tree(path)
}

/// The value of environment variable `name`, `None` when unset or not UTF-8; always `None`
/// on the web (a page has no environment).
#[must_use]
pub fn env_var(name: &str) -> Option<String> {
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    return std::env::var(name).ok();
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        let _ = name;
        None
    }
}

/// Modification time in seconds since the Unix epoch; `None` when unknown.
#[must_use]
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
#[must_use]
pub fn clipboard_ready() -> bool {
    imp::clipboard_ready()
}

/// Milliseconds until the caller should poll again without input because the browser owes
/// an answer ([`clipboard_ready`]); `None` when nothing is pending (always natively), so an
/// idle editor sleeps (D39).
#[must_use]
pub fn wake_ms() -> Option<u32> {
    imp::wake_ms()
}

/// The user state directory, `stdpath("state")` of the config spec §1 (honours
/// `SCHEMIFY_APPNAME`): the recent-files list lives here; `/state` on the web. `None` when
/// the environment names none.
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    dirs::here(Dir::State).ok()
}

/// Lists regular files under `dir` recursively in sorted order, skipping hidden entries,
/// `target`, and the directories in `skip`. Bounded by [`LIST_FILES_MAX`] files and
/// [`LIST_DEPTH_MAX`] levels.
///
/// # Panics
///
/// Panics if the platform side lists past [`LIST_FILES_MAX`] or out of order (a bug there).
#[must_use]
pub fn list_files(dir: &Path, skip: &[&Path]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    imp::list(dir, 0, skip, &mut out);
    assert!(out.len() <= LIST_FILES_MAX);
    // Depth-first over sorted children is component order, which is `Path`'s order.
    assert!(out.is_sorted(), "{}: listing out of order", dir.display());
    out
}

/// Milliseconds since the Unix epoch (journal stamps, file times); the browser's clock on
/// the web; 0 under WASI, which has no clock in CI's determinism runs.
#[must_use]
pub fn unix_ms() -> u64 {
    imp::unix_ms()
}

/// A seed for id generation (D9): clock and process id natively, `Math.random` on the web,
/// so two sessions editing one file do not draw the same ids.
#[must_use]
pub fn seed() -> u64 {
    imp::seed()
}
