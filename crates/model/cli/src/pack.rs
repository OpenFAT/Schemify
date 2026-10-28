//! The user package manager (D35 as amended 2026-10-11; config spec §5): the
//! `schemify.pack.add` specs of the user config → packages installed under
//! `<data>/pack/<name>`, pinned in `schemify-pack.lock` next to the init file (`lock`), and
//! for the editor where each module is and which capabilities it gets (`locate`).
//!
//! Fetching runs `git` and `curl` as child processes (lazy.nvim, `vim.pack`); hashing and
//! the lockfile rows reuse `doc` (`sha256`, `Locked`). Packages are prebuilt wasm, so nothing
//! here ever builds or runs package code.
//!
//! Data: ≤ `PACKS_MAX` (128) specs and lock rows, processed in spec order; one [`Report`]
//! line per package. Lifetime: one command.
//!
//! Contents:
//! - `pub const LOCKFILE: &str`, `pub const MANIFEST: &str`
//! - `pub struct Paths { pack, lock }`, `Paths::of`
//! - `pub struct Report { lines, failed }`
//! - re-exported from `locate`: `READ_ANY`, `Installed`, `locate`, `installed`
//! - `pub fn main(&Config, &[String]) -> u8`
//! - `pub fn sync, update, restore, clean, status(&[PackSpec], &Paths) -> Report`

mod locate;
mod lock;

pub use locate::{Installed, READ_ANY, installed, locate};

use std::fmt::Display;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use schemify_config::dirs::{self, Dir};
use schemify_config::{Config, PACKS_MAX, PackSpec, Source};
use schemify_doc::{hex, sha256};

use locate::{locate_in, package_dir};
use lock::{read_lock, select, set_row, write_lock};

/// The lockfile name, next to the init file.
pub const LOCKFILE: &str = "schemify-pack.lock";
/// The package manifest inside a package directory (one `plugin` record).
pub const MANIFEST: &str = "schemify.project";

/// Where packages and their lockfile live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// `<data>/pack`: one directory per `git`/`url` package.
    pub pack: PathBuf,
    /// `<config root>/schemify-pack.lock`.
    pub lock: PathBuf,
}

/// What a command did: one line per package, and how many failed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// `<name>: <what happened>` lines, in spec order.
    pub lines: Vec<String>,
    /// Packages that failed (exit 1 when > 0).
    pub failed: u32,
}

/// How far a sync may move an installed package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Install missing ones (at the locked commit when known); leave installed ones.
    Sync,
    /// Move every package to its spec's newest state.
    Update,
    /// Move every package to its lock row.
    Restore,
}

/// A package's lock row, as [`install`] compares against it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LockedRow {
    /// The commit or URL the row pins.
    resolved: String,
    /// The module's SHA-256, 64 lowercase hex digits.
    hash: String,
}

/// What [`install`] did to one package.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Outcome {
    /// The commit, URL or directory installed.
    resolved: String,
    /// The module's SHA-256, 64 lowercase hex digits.
    hash: String,
    /// The report line's text.
    what: String,
}

/// `schemify pack <command>` on the user config's specs; prints the report and returns the
/// exit code (0 ok, 1 a package failed, 2 usage or no config).
#[must_use]
pub fn main(config: &Config, args: &[String]) -> u8 {
    let [command] = args else {
        eprintln!("usage: schemify pack sync|update|restore|clean|status");
        return 2;
    };
    let paths = match Paths::of(config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("schemify pack: {e}");
            return 2;
        }
    };
    let specs = &config.table().packs;
    let report = match command.as_str() {
        "sync" => sync(specs, &paths),
        "update" => update(specs, &paths),
        "restore" => restore(specs, &paths),
        "clean" => clean(specs, &paths),
        "status" => status(specs, &paths),
        other => {
            eprintln!("schemify pack: unknown command `{other}`");
            return 2;
        }
    };
    for l in &report.lines {
        println!("{l}");
    }
    u8::from(report.failed > 0)
}

/// Installs missing packages and writes their lock rows (config spec §5 `sync`).
///
/// # Panics
///
/// Panics if `specs` holds more than [`PACKS_MAX`] packages (the config refuses more).
#[must_use]
pub fn sync(specs: &[PackSpec], paths: &Paths) -> Report {
    run(specs, paths, Mode::Sync)
}

/// Moves every package to its spec's newest state and rewrites the lock rows.
///
/// # Panics
///
/// As [`sync`].
#[must_use]
pub fn update(specs: &[PackSpec], paths: &Paths) -> Report {
    run(specs, paths, Mode::Update)
}

/// Makes every listed package exactly what the lockfile says.
///
/// # Panics
///
/// As [`sync`].
#[must_use]
pub fn restore(specs: &[PackSpec], paths: &Paths) -> Report {
    run(specs, paths, Mode::Restore)
}

/// Deletes package directories no spec names and drops their lock rows.
///
/// # Panics
///
/// Never: the asserts check the report's own counts.
#[must_use]
pub fn clean(specs: &[PackSpec], paths: &Paths) -> Report {
    let mut report = Report::default();
    let listed = |n: &str| {
        specs
            .iter()
            .any(|s| s.name == n && !matches!(s.source, Source::Path(_)))
    };
    let mut entries: Vec<PathBuf> = fs::read_dir(&paths.pack)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    entries.sort();
    for p in entries {
        let name = p
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        if !p.is_dir() || name.starts_with('.') || listed(&name) {
            continue;
        }
        match fs::remove_dir_all(&p) {
            Ok(()) => report.ok(&name, "removed"),
            Err(e) => report.fail(&name, e),
        }
    }
    match read_lock(&paths.lock) {
        Ok(mut lock) => {
            let keep: Vec<usize> = (0..lock.name.len())
                .filter(|&r| lock.ty[r] != "plugin" || specs.iter().any(|s| s.name == lock.name[r]))
                .collect();
            select(&mut lock, &keep);
            if let Err(e) = write_lock(&paths.lock, &mut lock) {
                report.fail(LOCKFILE, e);
            }
        }
        Err(e) => report.fail(LOCKFILE, e),
    }
    assert!(report.failed as usize <= report.lines.len());
    report
}

/// One line per package: kind, resolved source, state, grants (config spec §5 `status`).
/// A package that is not installed counts as failed.
///
/// # Panics
///
/// Never: the assert checks that every spec got its line.
#[must_use]
pub fn status(specs: &[PackSpec], paths: &Paths) -> Report {
    let mut report = Report::default();
    let lock = match read_lock(&paths.lock) {
        Ok(l) => l,
        Err(e) => {
            report.fail(LOCKFILE, e);
            return report;
        }
    };
    for spec in specs {
        let row = lock.find("plugin", &spec.name);
        let found = locate(spec, paths);
        let state = match (&found, row) {
            (Err(_), _) => "missing".to_owned(),
            (Ok(m), None) => format!("unlocked{}", m.grant_notes()),
            (Ok(m), Some(r)) => {
                let hash = fs::read(&m.wasm)
                    .map(|b| hex(&sha256(&b)))
                    .unwrap_or_default();
                let s = if hash == lock.hash[r] {
                    "ok"
                } else {
                    "changed"
                };
                format!("{s}{}", m.grant_notes())
            }
        };
        let resolved = row.map_or("-", |r| lock.resolved[r].as_str());
        report.ok(
            &spec.name,
            format!("{} {resolved} {state}", spec.source.word()),
        );
        if found.is_err() {
            report.failed += 1;
        }
    }
    assert_eq!(report.lines.len(), specs.len());
    assert!(
        report.failed as usize <= specs.len(),
        "one failure per spec at most"
    );
    report
}

impl Paths {
    /// The paths of this process's config: the data directory (config spec §1) and the
    /// config root.
    ///
    /// # Errors
    ///
    /// No data directory (unset `HOME`, …) or no config root (`--clean`).
    pub fn of(config: &Config) -> Result<Paths, String> {
        let root = config
            .root()
            .ok_or("no user config (--clean, or no config directory)")?;
        let data = dirs::here(Dir::Data)?;
        Ok(Paths {
            pack: data.join("pack"),
            lock: root.join(LOCKFILE),
        })
    }
}

// ---- private ----

impl Report {
    fn ok(&mut self, name: &str, what: impl Display) {
        self.lines.push(format!("{name}: {what}"));
        assert!(self.failed as usize <= self.lines.len());
    }

    fn fail(&mut self, name: &str, what: impl Display) {
        self.lines.push(format!("{name}: error: {what}"));
        self.failed += 1;
        assert!(
            self.failed as usize <= self.lines.len(),
            "a failure has its line"
        );
    }
}

/// `sync`, `update` and `restore`: [`install`] per spec in spec order, then the lock rows.
fn run(specs: &[PackSpec], paths: &Paths, mode: Mode) -> Report {
    assert!(specs.len() <= PACKS_MAX, "the config caps the specs");
    let mut report = Report::default();
    let mut lock = match read_lock(&paths.lock) {
        Ok(l) => l,
        Err(e) => {
            report.fail(LOCKFILE, e);
            return report;
        }
    };
    for spec in specs {
        let row = lock.find("plugin", &spec.name);
        let locked = row.map(|r| LockedRow {
            resolved: lock.resolved[r].clone(),
            hash: lock.hash[r].clone(),
        });
        match install(spec, paths, mode, locked.as_ref()) {
            Ok(Outcome {
                resolved,
                hash,
                what,
            }) => {
                set_row(&mut lock, &spec.name, spec.source.word(), &resolved, &hash);
                report.ok(&spec.name, what);
            }
            Err(e) => report.fail(&spec.name, e),
        }
    }
    if let Err(e) = write_lock(&paths.lock, &mut lock) {
        report.fail(LOCKFILE, e);
    }
    assert!(report.lines.len() >= specs.len());
    assert!(
        report.lines.len() <= specs.len() + 1,
        "one line per spec, plus the lockfile's"
    );
    report
}

/// Installs or moves one package.
fn install(
    spec: &PackSpec,
    paths: &Paths,
    mode: Mode,
    locked: Option<&LockedRow>,
) -> Result<Outcome, String> {
    let dir = package_dir(spec, paths);
    let (resolved, what) = match &spec.source {
        Source::Path(p) => {
            if !p.is_dir() {
                return Err(format!("{} is not a directory", p.display()));
            }
            (p.display().to_string(), "local".to_owned())
        }
        Source::Git { url, rev } => {
            let fresh = !dir.join(".git").exists();
            if fresh {
                fs::create_dir_all(&paths.pack).map_err(|e| e.to_string())?;
                git(&paths.pack, &["clone", "--quiet", url, &spec.name])?;
            }
            let target = match (mode, locked) {
                (Mode::Restore, Some(row)) => Some(row.resolved.clone()),
                (Mode::Update, _) => {
                    if !fresh {
                        git(&dir, &["fetch", "--quiet", "--tags", "origin"])?;
                    }
                    Some(newest(&dir, rev.as_deref())?)
                }
                // Sync, or restore without a lock row: install missing ones only.
                (_, Some(row)) if fresh => Some(row.resolved.clone()),
                _ if fresh => Some(newest(&dir, rev.as_deref())?),
                _ => None,
            };
            if let Some(commit) = &target {
                if git(&dir, &["cat-file", "-e", &format!("{commit}^{{commit}}")]).is_err() {
                    git(&dir, &["fetch", "--quiet", "--tags", "origin"])?;
                }
                git(&dir, &["checkout", "--quiet", "--detach", commit])?;
            }
            let head = git(&dir, &["rev-parse", "HEAD"])?;
            let what = if fresh {
                "installed"
            } else if target.is_some() {
                "checked out"
            } else {
                "ok"
            };
            let shown = format!("{what} {}", short(&head));
            (head, shown)
        }
        Source::Url { url, sha256: want } => {
            let file = dir.join(format!("{}.wasm", spec.name));
            let have = fs::read(&file).ok().map(|b| hex(&sha256(&b)));
            if have.as_deref() == Some(want.as_str()) {
                (url.clone(), "ok".to_owned())
            } else {
                download(url, want, &paths.pack, &dir, &file)?;
                (url.clone(), "downloaded".to_owned())
            }
        }
    };
    let module = locate_in(&dir, &spec.name)?;
    let bytes = fs::read(&module.wasm).map_err(|e| format!("{}: {e}", module.wasm.display()))?;
    let hash = hex(&sha256(&bytes));
    if mode == Mode::Restore
        && let Some(row) = locked
        && row.hash != hash
    {
        return Err(format!(
            "content {} differs from the lock {}",
            short(&hash),
            short(&row.hash)
        ));
    }
    assert_eq!(hash.len(), 64);
    Ok(Outcome {
        resolved,
        hash,
        what,
    })
}

/// The commit `rev` names after a fetch (a remote branch first, then a tag or commit); the
/// remote's default branch when `None`.
fn newest(dir: &Path, rev: Option<&str>) -> Result<String, String> {
    let candidates = match rev {
        None => vec!["origin/HEAD".to_owned()],
        Some(r) => vec![format!("origin/{r}"), r.to_owned()],
    };
    assert!(!candidates.is_empty());
    for c in &candidates {
        let spec = format!("{c}^{{commit}}");
        if let Ok(commit) = git(dir, &["rev-parse", "--verify", "--quiet", &spec]) {
            return Ok(commit);
        }
    }
    Err(format!("no revision `{}`", rev.unwrap_or("HEAD")))
}

/// Downloads `url` with `curl` into a temp file under `pack`, checks its sha256 against
/// `want`, and only then moves it to `file` (inside `dir`). On a mismatch nothing is
/// installed and the temp file is deleted.
fn download(url: &str, want: &str, pack: &Path, dir: &Path, file: &Path) -> Result<(), String> {
    assert_eq!(want.len(), 64, "the config checked `sha256`");
    assert!(
        file.starts_with(dir),
        "the module lands in its package directory"
    );
    fs::create_dir_all(pack).map_err(|e| e.to_string())?;
    let name = dir
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let tmp = pack.join(format!(".{name}.download"));
    let out = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&tmp)
        .arg(url)
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !out.status.success() {
        let _ = fs::remove_file(&tmp);
        return Err(format!(
            "curl: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let bytes = fs::read(&tmp).map_err(|e| e.to_string())?;
    let got = hex(&sha256(&bytes));
    if got != want {
        let _ = fs::remove_file(&tmp);
        return Err(format!(
            "sha256 mismatch: got {got}, want {want}; nothing installed"
        ));
    }
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    fs::rename(&tmp, file).map_err(|e| e.to_string())?;
    assert!(file.is_file());
    Ok(())
}

/// Runs `git -C dir args…` without prompts; returns trimmed stdout.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    assert!(!args.is_empty(), "a git subcommand");
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git {}: {}", args[0], err.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The first 12 characters of a commit or content hash, for report lines.
fn short(hash: &str) -> &str {
    debug_assert!(hash.is_ascii(), "{hash}: a hex hash");
    &hash[..hash.len().min(12)]
}
