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
/// # Panics
///
/// Panics if `argv` is empty.
// ponytail: blocks the frame while the child runs (a characterization takes seconds); move to
// a worker thread with an event-loop wake-up when the run spine lands in the editor.
pub fn spawn(cwd: &Path, argv: &[String], stdin: &[u8]) -> (i32, Vec<u8>) {
    assert!(!argv.is_empty(), "spawn needs a binary");
    let (status, out) = imp::spawn(cwd, argv, stdin);
    (status, out)
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
