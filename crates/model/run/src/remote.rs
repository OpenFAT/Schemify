//! A job on another machine (run spec §5, "Remote jobs"): a job directory and its simulator
//! command line in; out, the `ssh` child that runs them on the PDK's host, fed the POSIX
//! ustar stream of the directory on its stdin. The runner polls, times out and reaps that
//! child exactly like a local simulator.
//!
//! On the host the job is one `sh -c` script, so a csh login shell runs it too: a fresh
//! `mktemp -d` directory, the stream unpacked into it, the simulator run there with its
//! stdout joined to its stderr (both stream back into the job's `stderr.log`), the raw file
//! written to ssh's stdout (the job's `out.raw`) after an exit 0 only, the directory
//! removed, and the simulator's exit status returned. The host needs `sh`, `mktemp`, `tar`,
//! `cat`, `rm` and the simulator, nothing of Schemify.
//!
//! Contents:
//! - `pub const REMOTE_WORKERS_MAX: u16`
//! - crate-only `fn spawn(ssh, host, dir, argv, env, err) -> Result<Process, String>`
//! - crate-only `fn tidy(dir: &Path)`
//! - crate-only `fn failure(host, sim, status, why, dir) -> String`

use crate::process::Process;
use crate::runner::{COMMAND, RAW, STDERR, STDOUT};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::UNIX_EPOCH;

/// Most remote jobs one runner runs at once: sshd's default `MaxStartups 10:30:100` starts
/// dropping connections past ten unauthenticated ones.
// ponytail: one cap for every host; a per-host cap (or the user's ControlMaster) when a run
// needs more parallelism than this.
pub const REMOTE_WORKERS_MAX: u16 = 8;

/// The job directory's tar stream, written into the job directory and removed when the job
/// ends.
const UPLOAD: &str = "upload.tar";
/// Options of every remote `ssh`: never prompt (`BatchMode`: a host that asks for a password
/// fails at once with ssh's message), no tty, no X11 or agent forwarding, none of the user's
/// port forwards (two jobs' `LocalForward` would collide), a dead link noticed within a
/// minute. Credentials, jump hosts and host keys stay the user's `~/.ssh/config` and agent.
const SSH_OPTS: [&str; 13] = [
    "-T",
    "-x",
    "-a",
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=10",
    "-o",
    "ServerAliveInterval=15",
    "-o",
    "ServerAliveCountMax=4",
    "-o",
    "ClearAllForwardings=yes",
];
/// Most files one job directory uploads (its netlist, sources and their includes).
const FILES_MAX: usize = 4096;
/// ustar block size.
const BLOCK: usize = 512;

/// Starts job directory `dir`'s simulator `argv` (with the variables `env`, a `sim` row's)
/// on `host` through the ssh client `ssh`: the directory, minus the runner's own files, goes
/// up as `upload.tar` on ssh's stdin, the raw file comes back on its stdout into `out.raw`,
/// and the simulator's whole output on its stderr into `err` (`stderr.log`).
///
/// # Errors
///
/// The upload cannot be built or written ([`ustar`]'s errors), `out.raw` cannot be
/// created, or `ssh` does not start.
///
/// # Panics
///
/// Panics if `host` is empty or starts with `-` (the user config admits only `valid_host`
/// destinations), or `argv` is empty.
pub(crate) fn spawn(
    ssh: &str,
    host: &str,
    dir: &Path,
    argv: &[String],
    env: &[(String, String)],
    err: File,
) -> Result<Process, String> {
    assert!(!argv.is_empty(), "a program");
    let upload = dir.join(UPLOAD);
    let skip = [STDOUT, STDERR, COMMAND, RAW, UPLOAD];
    fs::write(&upload, ustar(dir, &skip)?).map_err(|e| format!("cannot create {UPLOAD}: {e}"))?;
    let stdin = File::open(&upload).map_err(|e| format!("cannot read {UPLOAD}: {e}"))?;
    let raw = File::create(dir.join(RAW)).map_err(|e| format!("cannot create {RAW}: {e}"))?;
    let mut cmd = command(ssh, host, argv, env);
    cmd.current_dir(dir);
    let process = Process::spawn(&mut cmd, Stdio::from(stdin), raw, err)
        .map_err(|e| format!("cannot start `{ssh}` for {host}: {e}"))?;
    assert!(upload.is_file(), "the upload stays until the job ends");
    Ok(process)
}

/// Removes what a remote job leaves in `dir` that is not its output: the upload, and an
/// empty `out.raw` (the host sends the raw file only after an exit 0, so an empty one is
/// none). Missing files are fine.
pub(crate) fn tidy(dir: &Path) {
    let _ = fs::remove_file(dir.join(UPLOAD));
    let raw = dir.join(RAW);
    if fs::metadata(&raw).is_ok_and(|m| m.len() == 0) {
        let _ = fs::remove_file(&raw);
    }
    assert!(!dir.join(UPLOAD).exists(), "the upload is not kept");
}

/// The message of a remote job that exited with `status`: ssh's own failures (no route, a
/// host key or password refused) exit 255 and name ssh; any other names the simulator
/// `sim` on `host`. `why` is the last line of its `stderr.log`.
///
/// # Panics
///
/// Panics if `status` is a success.
pub(crate) fn failure(host: &str, sim: &str, status: ExitStatus, why: &str, dir: &Path) -> String {
    assert!(!status.success(), "only a failed job has a failure");
    let d = dir.display();
    let msg = if status.code() == Some(255) {
        format!("ssh to `{host}` failed ({status}): {why} (see {d})")
    } else {
        format!("`{sim}` on `{host}` exited with {status}: {why} (see {d})")
    };
    assert!(msg.contains(host), "the message names the host");
    msg
}

// ---- private ----

/// `ssh <options> -- <host> "sh -c '<script>'"` for the program and arguments `argv` with
/// the variables `env`, run in a fresh directory on `host` ([`script`]). stdin, stdout,
/// stderr and the working directory are the caller's to set.
fn command(ssh: &str, host: &str, argv: &[String], env: &[(String, String)]) -> Command {
    assert!(!host.is_empty(), "a destination");
    assert!(!host.starts_with('-'), "a destination, never an ssh option");
    let mut c = Command::new(ssh);
    c.args(SSH_OPTS)
        .arg("--")
        .arg(host)
        .arg(format!("sh -c {}", single_quoted(&script(argv, env))));
    c
}

/// The POSIX `sh` script a remote job runs (the module header's steps), on one line. Each
/// word of `env` (`NAME=value`, through `env`) and `argv` is double-quoted, so spaces,
/// quotes, `$`, backticks and backslashes reach the simulator verbatim. Asserts `argv` is
/// not empty and no word holds a control character (the user config refuses those in a
/// row with a `host`).
fn script(argv: &[String], env: &[(String, String)]) -> String {
    assert!(!argv.is_empty(), "a program");
    let mut cmd = String::new();
    if !env.is_empty() {
        cmd.push_str("env ");
        for (k, v) in env {
            double_quoted(&mut cmd, &format!("{k}={v}"));
            cmd.push(' ');
        }
    }
    for a in argv {
        double_quoted(&mut cmd, a);
        cmd.push(' ');
    }
    let s = format!(
        "umask 077; d=$(mktemp -d \"${{TMPDIR:-/tmp}}/schemify.XXXXXX\") || exit 125; \
         cd \"$d\" && tar -xf - && {cmd}</dev/null 1>&2; s=$?; \
         if [ $s -eq 0 ] && [ -f \"$d/{RAW}\" ]; then cat \"$d/{RAW}\"; fi; \
         cd / && rm -rf \"$d\"; exit $s"
    );
    assert!(!s.contains(['\n', '\r']), "one line for a csh login shell");
    s
}

/// The regular files under `dir`, recursively and sorted by path, as a POSIX ustar stream
/// (mode 0644, owner 0, each file's mtime), minus the top-level names in `skip` (the
/// runner's own files). `tar -xf -` creates the subdirectories on the host.
///
/// Cost: reads every file once; the stream holds them all.
///
/// Errors: a file or directory that cannot be read, a path longer than the 100 bytes of a
/// ustar name, or more than [`FILES_MAX`] files.
fn ustar(dir: &Path, skip: &[&str]) -> Result<Vec<u8>, String> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut dirs: Vec<(String, PathBuf)> = vec![(String::new(), dir.to_path_buf())];
    while let Some((rel, d)) = dirs.pop() {
        let entries = fs::read_dir(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        for e in entries {
            let e = e.map_err(|e| format!("{}: {e}", d.display()))?;
            let name = e.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let kind = e.file_type().map_err(|e| format!("{path}: {e}"))?;
            if kind.is_dir() {
                dirs.push((path, e.path()));
            } else if kind.is_file() && !skip.contains(&path.as_str()) {
                files.push((path, e.path()));
            }
            if files.len() + dirs.len() > FILES_MAX {
                return Err(format!(
                    "{}: more than {FILES_MAX} files to send",
                    dir.display()
                ));
            }
        }
    }
    files.sort_unstable();
    let mut out = Vec::new();
    for (name, path) in &files {
        let data = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mtime = fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        out.extend_from_slice(&header(name, data.len() as u64, mtime)?);
        out.extend_from_slice(&data);
        out.resize(out.len().next_multiple_of(BLOCK), 0);
    }
    // Two zero blocks end the archive.
    out.resize(out.len() + 2 * BLOCK, 0);
    assert!(out.len().is_multiple_of(BLOCK));
    assert!(out.len() >= (files.len() + 2) * BLOCK);
    Ok(out)
}

/// One ustar header block (POSIX.1-1988): name, octal fields, checksum, type `0` (a file).
fn header(name: &str, size: u64, mtime: u64) -> Result<[u8; BLOCK], String> {
    if name.len() > 100 {
        return Err(format!("{name}: longer than the 100 bytes of a tar name"));
    }
    // Field widths: digits then a NUL; size and mtime have 11 digits (< 8 GiB, < year 2242).
    assert!(size < 8u64.pow(11), "{name}: 8 GiB or more");
    assert!(mtime < 8u64.pow(11), "{name}: mtime past 2242");
    let mut h = [0u8; BLOCK];
    h[..name.len()].copy_from_slice(name.as_bytes());
    for (at, width, value) in [
        (100, 7, 0o644),
        (108, 7, 0),
        (116, 7, 0),
        (124, 11, size),
        (136, 11, mtime),
    ] {
        h[at..at + width].copy_from_slice(format!("{value:0width$o}").as_bytes());
    }
    h[148..156].fill(b' ');
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    assert!(sum < 8u32.pow(6), "the checksum fits its six digits");
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    Ok(h)
}

/// `"w"` for sh, with `"`, `\`, `$` and backtick escaped: the word survives as itself.
fn double_quoted(out: &mut String, w: &str) {
    assert!(
        !w.contains(char::is_control),
        "{w:?}: no control characters"
    );
    let start = out.len();
    out.push('"');
    for c in w.chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    assert!(out.len() >= start + w.len() + 2);
}

/// `'s'` for the login shell (sh or csh), each `'` written `'\''`.
fn single_quoted(s: &str) -> String {
    let q = format!("'{}'", s.replace('\'', r"'\''"));
    assert!(q.len() >= s.len() + 2);
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The independent oracle is the system's `tar`: what it unpacks equals the files.
    #[test]
    fn ustar_unpacks_with_the_system_tar() {
        let base = std::env::temp_dir().join(format!("schemify-ustar-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (src, dst) = (base.join("src"), base.join("dst"));
        fs::create_dir_all(src.join("sub/deeper")).unwrap();
        fs::create_dir_all(&dst).unwrap();
        let binary: Vec<u8> = (0..=255u8).cycle().take(1300).collect();
        fs::write(src.join("netlist.cir"), "* x\n.end\n").unwrap();
        fs::write(src.join("sub/deeper/k.inc"), "`define K 1\n").unwrap();
        fs::write(src.join("m.osdi"), &binary).unwrap();
        fs::write(src.join("empty"), "").unwrap();
        fs::write(src.join("stdout.log"), "the runner's own").unwrap();
        let bytes = ustar(&src, &["stdout.log"]).unwrap();
        assert_eq!(
            bytes,
            ustar(&src, &["stdout.log"]).unwrap(),
            "deterministic"
        );
        let tar = base.join("x.tar");
        fs::write(&tar, &bytes).unwrap();
        let ok = Command::new("tar")
            .arg("-xf")
            .arg(&tar)
            .arg("-C")
            .arg(&dst)
            .status()
            .expect("tar on PATH");
        assert!(ok.success());
        assert_eq!(fs::read(dst.join("m.osdi")).unwrap(), binary);
        assert_eq!(
            fs::read_to_string(dst.join("netlist.cir")).unwrap(),
            "* x\n.end\n"
        );
        assert_eq!(
            fs::read_to_string(dst.join("sub/deeper/k.inc")).unwrap(),
            "`define K 1\n"
        );
        assert_eq!(fs::read(dst.join("empty")).unwrap(), b"");
        assert!(!dst.join("stdout.log").exists(), "skipped");
        // Negative space: a name past ustar's 100 bytes is refused, naming it.
        fs::write(src.join("n".repeat(101)), "x").unwrap();
        let e = ustar(&src, &[]).unwrap_err();
        assert!(
            e.contains(&"n".repeat(101)) && e.contains("100 bytes"),
            "{e}"
        );
        let _ = fs::remove_dir_all(&base);
    }

    /// The protocol on the host, as a spec example: one line, every word double-quoted.
    #[test]
    fn the_host_script_is_one_quoted_line() {
        let argv = ["espice", "-b", "netlist.cir"].map(String::from);
        let env = [("ZIG".to_owned(), "/opt/zig 0.17/zig".to_owned())];
        assert_eq!(
            script(&argv, &env),
            "umask 077; d=$(mktemp -d \"${TMPDIR:-/tmp}/schemify.XXXXXX\") || exit 125; \
             cd \"$d\" && tar -xf - && env \"ZIG=/opt/zig 0.17/zig\" \"espice\" \"-b\" \
             \"netlist.cir\" </dev/null 1>&2; s=$?; if [ $s -eq 0 ] && [ -f \"$d/out.raw\" ]; \
             then cat \"$d/out.raw\"; fi; cd / && rm -rf \"$d\"; exit $s"
        );
        let mut w = String::new();
        double_quoted(&mut w, "a\"b\\c$d`e'f");
        assert_eq!(w, r#""a\"b\\c\$d\`e'f""#);
        assert_eq!(single_quoted("it's"), r"'it'\''s'");
        let c = command("ssh", "pdk@h", &argv, &[]);
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[..SSH_OPTS.len()], SSH_OPTS.map(String::from));
        assert_eq!(args[SSH_OPTS.len()..SSH_OPTS.len() + 2], ["--", "pdk@h"]);
        assert!(args[SSH_OPTS.len() + 2].starts_with("sh -c 'umask 077; "));
        assert_eq!(
            args.len(),
            SSH_OPTS.len() + 3,
            "the remote command is one word"
        );
    }
}
