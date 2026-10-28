//! The native side of the OS boundary (Linux, macOS, Windows, and WASI under a wasm
//! runtime): `std::fs`, child processes, the OS clipboard programs, the system clock. Each
//! function here backs the `platform` function of the same name, which documents it.
//!
//! Contents (for `platform`): `read`, `read_tail`, `exists`, `is_dir`, `write_atomic`,
//! `append`, `spawn`, `remove`, `remove_tree`, `mtime_unix_s`, `clipboard_set`,
//! `clipboard_get`, `clipboard_ready`, `wake_ms`, `export`, `import_project`, `args`,
//! `unix_ms`, `seed`, `list`.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::{CLIPBOARD_BYTES_MAX, LIST_DEPTH_MAX, LIST_FILES_MAX};

/// Plugin-ABI `E_GONE` (§8): a spawned binary that cannot start. Spelled here because the
/// headless build has no plugin crate.
const E_GONE: i32 = 2;

pub(super) fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

pub(super) fn read_tail(path: &Path, max: u64) -> Result<(Vec<u8>, bool), String> {
    use std::io::{Read, Seek, SeekFrom};
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut f = std::fs::File::open(path).map_err(err)?;
    let len = f.metadata().map_err(err)?.len();
    let from = len.saturating_sub(max);
    f.seek(SeekFrom::Start(from)).map_err(err)?;
    let mut buf = Vec::new();
    f.take(max).read_to_end(&mut buf).map_err(err)?;
    Ok((buf, from > 0))
}

pub(super) fn exists(path: &Path) -> bool {
    path.exists()
}

pub(super) fn is_dir(path: &Path) -> bool {
    path.is_dir()
}

/// Writes a sibling temp file, then renames it over `path` (edits spec §5).
pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".schemify-tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

pub(super) fn append(path: &Path, bytes: &[u8]) -> Result<(), String> {
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

pub(super) fn spawn(cwd: &Path, argv: &[String], stdin: &[u8]) -> (i32, Vec<u8>) {
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

pub(super) fn remove(path: &Path) -> Result<(), String> {
    std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))
}

pub(super) fn remove_tree(path: &Path) -> Result<(), String> {
    // `symlink_metadata`: a link is removed itself, never followed out of the tree.
    let meta =
        std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .map_err(|e| format!("{}: {e}", path.display()))
}

pub(super) fn mtime_unix_s(path: &Path) -> Option<u64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    t.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

pub(super) fn clipboard_set(text: &str) -> Result<(), String> {
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

pub(super) fn clipboard_get() -> Result<String, String> {
    use std::process::Command;
    let mut last = String::from("no clipboard program");
    for (_, read) in clipboard_tools() {
        match Command::new(read[0]).args(&read[1..]).output() {
            Ok(o) if o.status.success() => {
                let mut b = o.stdout;
                b.truncate(CLIPBOARD_BYTES_MAX);
                return String::from_utf8(b).map_err(|_| "clipboard is not UTF-8".into());
            }
            Ok(o) => last = format!("{}: {}", read[0], o.status),
            Err(e) => last = format!("{}: {e}", read[0]),
        }
    }
    Err(last)
}

// The clipboard tools answer synchronously: nothing is ever pending.
pub(super) fn clipboard_ready() -> bool {
    false
}

pub(super) fn wake_ms() -> Option<u32> {
    None
}

pub(super) fn export(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_atomic(path, bytes)
}

pub(super) fn import_project(_folder: bool) -> Result<(), String> {
    Err("a native project is a directory: open it (schemify edit <dir>)".into())
}

pub(super) fn args() -> Vec<String> {
    std::env::args().skip(1).collect()
}

pub(super) fn unix_ms() -> u64 {
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

pub(super) fn seed() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    let pid = u64::from(std::process::id());
    #[cfg(target_arch = "wasm32")]
    let pid = 0u64; // WASI: no process ids
    unix_ms() ^ (pid << 32) ^ 0x9e37_79b9_7f4a_7c15
}

pub(super) fn list(dir: &Path, depth: u32, skip: &[&Path], out: &mut Vec<PathBuf>) {
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
        if hidden || skip.contains(&p.as_path()) {
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

// ---- private ----

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
