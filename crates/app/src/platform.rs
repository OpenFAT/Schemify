//! The OS boundary (ARCHITECTURE: only `app` talks to the OS), split by `cfg` (D31):
//! native and WASI builds use `std::fs`; the web build will use OPFS through generated glue
//! (D2) and until then sees an empty file system holding only [`WEB_ROOT`].
//!
//! Every path here is absolute or relative to the process working directory; callers build
//! them from the project root. Errors are plain text for the status line.

use std::path::{Path, PathBuf};

/// Directory entries visited per listing; deeper or larger trees are cut (a library is one
/// directory of cells, D10, so real ones are far below this).
pub const LIST_FILES_MAX: usize = 1 << 16;
/// Directory depth walked by [`list_files`].
pub const LIST_DEPTH_MAX: u32 = 8;
/// The web build's project directory, opened by its argv-less start (`schemify edit /`): the
/// root of its file system and, until OPFS is wired (D31), the only entry that exists there.
pub const WEB_ROOT: &str = "/";

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

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod imp {
    // TODO(D31): OPFS through web-sys (generated glue only, D2). Until then the web build
    // edits in memory over an empty file system: only the root `/` exists (the editor's
    // project directory on the web), reads fail, writes are refused, listings are empty.
    // ponytail: OPFS is async on the main thread while this API is synchronous; the upgrade
    // is an in-memory mirror loaded from OPFS before the editor starts, written back async.
    use super::*;

    const NO_OPFS: &str = "web storage (OPFS) is not wired yet";

    pub fn read(_: &Path) -> Result<Vec<u8>, String> {
        Err(NO_OPFS.into())
    }
    pub fn exists(path: &Path) -> bool {
        is_dir(path)
    }
    pub fn is_dir(path: &Path) -> bool {
        path == Path::new(WEB_ROOT)
    }
    pub fn write_atomic(_: &Path, _: &[u8]) -> Result<(), String> {
        Err(NO_OPFS.into())
    }
    pub fn append(_: &Path, _: &[u8]) -> Result<(), String> {
        Err(NO_OPFS.into())
    }
    pub fn list(_: &Path, _: u32, _: &[PathBuf], _: &mut Vec<PathBuf>) {}
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
