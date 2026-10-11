//! The user package manager (D35 as amended 2026-10-11; config spec §5): installs the
//! `schemify.pack.add` specs of the user config under `<data>/pack/<name>`, pins them in
//! `schemify-pack.lock` next to the init file, and tells the editor where each module is and
//! which capabilities it gets.
//!
//! Fetching runs `git` and `curl` as child processes (lazy.nvim, `vim.pack`); hashing and the
//! lockfile rows reuse `doc` (`sha256`, `Locked`). Packages are prebuilt wasm, so nothing
//! here ever builds or runs package code.
//!
//! Data design: ≤ `PACKS_MAX` (128) specs and lock rows, processed in spec order; one
//! [`Report`] line per package. Lifetime: one command.

use std::path::{Path, PathBuf};
use std::process::Command;

use schemify_config::{Config, Event, PackSpec, Source, Table, dirs};
use schemify_doc::{Locked, Project, hex, sha256};

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
        let data = dirs::here(dirs::Dir::Data)?;
        Ok(Paths {
            pack: data.join("pack"),
            lock: root.join(LOCKFILE),
        })
    }
}

/// What a command did: one line per package, and how many failed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// `<name>: <what happened>` lines, in spec order.
    pub lines: Vec<String>,
    /// Packages that failed (exit 1 when > 0).
    pub failed: u32,
}

impl Report {
    fn ok(&mut self, name: &str, what: impl std::fmt::Display) {
        self.lines.push(format!("{name}: {what}"));
    }

    fn fail(&mut self, name: &str, what: impl std::fmt::Display) {
        self.lines.push(format!("{name}: error: {what}"));
        self.failed += 1;
    }
}

/// `schemify pack <command>` on the user config's specs; prints the report and returns the
/// exit code (0 ok, 1 a package failed, 2 usage or no config).
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

/// How far a sync may move an installed package.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Install missing ones (at the locked commit when known); leave installed ones.
    Sync,
    /// Move every package to its spec's newest state.
    Update,
    /// Move every package to its lock row.
    Restore,
}

/// Installs missing packages and writes their lock rows (config spec §5 `sync`).
pub fn sync(specs: &[PackSpec], paths: &Paths) -> Report {
    run(specs, paths, Mode::Sync)
}

/// Moves every package to its spec's newest state and rewrites the lock rows.
pub fn update(specs: &[PackSpec], paths: &Paths) -> Report {
    run(specs, paths, Mode::Update)
}

/// Makes every listed package exactly what the lockfile says.
pub fn restore(specs: &[PackSpec], paths: &Paths) -> Report {
    run(specs, paths, Mode::Restore)
}

fn run(specs: &[PackSpec], paths: &Paths, mode: Mode) -> Report {
    assert!(specs.len() <= schemify_config::PACKS_MAX);
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
        let locked = row.map(|r| (lock.resolved[r].clone(), lock.hash[r].clone()));
        match install(spec, paths, mode, locked.as_ref()) {
            Ok((resolved, hash, what)) => {
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
    report
}

/// Installs or moves one package; returns (resolved, wasm sha256, what happened).
fn install(
    spec: &PackSpec,
    paths: &Paths,
    mode: Mode,
    locked: Option<&(String, String)>,
) -> Result<(String, String, String), String> {
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
                std::fs::create_dir_all(&paths.pack).map_err(|e| e.to_string())?;
                git(&paths.pack, &["clone", "--quiet", url, &spec.name])?;
            }
            let target = match (mode, locked) {
                (Mode::Restore, Some((commit, _))) => Some(commit.clone()),
                (Mode::Update, _) => {
                    if !fresh {
                        git(&dir, &["fetch", "--quiet", "--tags", "origin"])?;
                    }
                    Some(newest(&dir, rev.as_deref())?)
                }
                // Sync, or restore without a lock row: install missing ones only.
                (_, Some((commit, _))) if fresh => Some(commit.clone()),
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
            let have = std::fs::read(&file).ok().map(|b| hex(&sha256(&b)));
            if have.as_deref() == Some(want.as_str()) {
                (url.clone(), "ok".to_owned())
            } else {
                download(url, want, &paths.pack, &dir, &file)?;
                (url.clone(), "downloaded".to_owned())
            }
        }
    };
    let module = locate_in(&dir, &spec.name)?;
    let bytes =
        std::fs::read(&module.wasm).map_err(|e| format!("{}: {e}", module.wasm.display()))?;
    let hash = hex(&sha256(&bytes));
    if mode == Mode::Restore
        && let Some((_, want)) = locked
        && *want != hash
    {
        return Err(format!(
            "content {} differs from the lock {}",
            short(&hash),
            short(want)
        ));
    }
    assert_eq!(hash.len(), 64);
    Ok((resolved, hash, what))
}

/// The commit `rev` names after a fetch (a remote branch first, then a tag or commit); the
/// remote's default branch when `None`.
fn newest(dir: &Path, rev: Option<&str>) -> Result<String, String> {
    let candidates = match rev {
        None => vec!["origin/HEAD".to_owned()],
        Some(r) => vec![format!("origin/{r}"), r.to_owned()],
    };
    for c in &candidates {
        if let Ok(commit) = git(
            dir,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{c}^{{commit}}"),
            ],
        ) {
            return Ok(commit);
        }
    }
    Err(format!("no revision `{}`", rev.unwrap_or("HEAD")))
}

/// Downloads `url` with `curl` into a temp file under `pack`, checks its sha256 against
/// `want`, and only then moves it to `file` (inside `dir`). On a mismatch nothing is
/// installed and the temp file is deleted.
fn download(url: &str, want: &str, pack: &Path, dir: &Path, file: &Path) -> Result<(), String> {
    std::fs::create_dir_all(pack).map_err(|e| e.to_string())?;
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
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "curl: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let bytes = std::fs::read(&tmp).map_err(|e| e.to_string())?;
    let got = hex(&sha256(&bytes));
    if got != want {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "sha256 mismatch: got {got}, want {want}; nothing installed"
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, file).map_err(|e| e.to_string())?;
    assert!(file.is_file());
    Ok(())
}

/// Deletes package directories no spec names and drops their lock rows.
pub fn clean(specs: &[PackSpec], paths: &Paths) -> Report {
    let mut report = Report::default();
    let listed = |n: &str| {
        specs
            .iter()
            .any(|s| s.name == n && !matches!(s.source, Source::Path(_)))
    };
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&paths.pack)
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
        match std::fs::remove_dir_all(&p) {
            Ok(()) => report.ok(&name, "removed"),
            Err(e) => report.fail(&name, e),
        }
    }
    match read_lock(&paths.lock) {
        Ok(mut lock) => {
            let keep: Vec<bool> = (0..lock.name.len())
                .map(|r| lock.ty[r] != "plugin" || specs.iter().any(|s| s.name == lock.name[r]))
                .collect();
            retain(&mut lock, &keep);
            if let Err(e) = write_lock(&paths.lock, &mut lock) {
                report.fail(LOCKFILE, e);
            }
        }
        Err(e) => report.fail(LOCKFILE, e),
    }
    report
}

/// One line per package: kind, resolved source, state, grants (config spec §5 `status`).
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
                let hash = std::fs::read(&m.wasm)
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
    report
}

/// An installed package, ready for the plugin host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// Package name (the plugin's name in the host).
    pub name: String,
    /// The module.
    pub wasm: PathBuf,
    /// What the plugin gets: requested ∩ granted, in request order (D7).
    pub grants: Vec<String>,
    /// Requested but not granted.
    pub denied: Vec<String>,
    /// Granted but not requested.
    pub unused: Vec<String>,
    /// Spec `fuel`, else the manifest's, else `None` (the host default).
    pub fuel: Option<u64>,
    /// Commands that load it lazily.
    pub lazy_cmd: Vec<String>,
    /// Events that load it lazily.
    pub lazy_event: Vec<Event>,
}

impl Installed {
    fn grant_notes(&self) -> String {
        let mut s = String::new();
        if !self.denied.is_empty() {
            s.push_str(&format!(" denied: {}", self.denied.join(",")));
        }
        if !self.unused.is_empty() {
            s.push_str(&format!(" unused: {}", self.unused.join(",")));
        }
        s
    }
}

/// A module found in a package directory: its file, requested grants (`None` without a
/// manifest) and manifest fuel.
struct Module {
    wasm: PathBuf,
    requested: Option<Vec<String>>,
    fuel: Option<u64>,
}

/// Finds the installed module of `spec` and computes its grants (config spec §5).
///
/// # Errors
///
/// Not installed (run `schemify pack sync`), or a manifest that does not parse.
pub fn locate(spec: &PackSpec, paths: &Paths) -> Result<Installed, String> {
    let dir = package_dir(spec, paths);
    let m = locate_in(&dir, &spec.name)?;
    if !m.wasm.is_file() {
        return Err(format!(
            "not installed (no {}); run `schemify pack sync`",
            m.wasm.display()
        ));
    }
    let requested = m.requested.unwrap_or_else(|| spec.grant.clone());
    let grants: Vec<String> = requested
        .iter()
        .filter(|g| spec.grant.contains(g))
        .cloned()
        .collect();
    let denied = requested
        .iter()
        .filter(|g| !spec.grant.contains(g))
        .cloned()
        .collect();
    let unused = spec
        .grant
        .iter()
        .filter(|g| !requested.contains(g))
        .cloned()
        .collect();
    assert!(grants.len() <= spec.grant.len());
    Ok(Installed {
        name: spec.name.clone(),
        wasm: m.wasm,
        grants,
        denied,
        unused,
        fuel: spec.fuel.or(m.fuel),
        lazy_cmd: spec.lazy_cmd.clone(),
        lazy_event: spec.lazy_event.clone(),
    })
}

/// [`locate`] for every spec of `table`, in spec order: the installed packages and one
/// message per package that cannot load.
pub fn installed(table: &Table, paths: &Paths) -> (Vec<Installed>, Vec<String>) {
    let mut ok = Vec::with_capacity(table.packs.len());
    let mut notes = Vec::new();
    for spec in &table.packs {
        match locate(spec, paths) {
            Ok(i) => {
                if !i.denied.is_empty() {
                    notes.push(format!("{}: not granted: {}", i.name, i.denied.join(", ")));
                }
                ok.push(i);
            }
            Err(e) => notes.push(format!("{}: {e}", spec.name)),
        }
    }
    (ok, notes)
}

fn package_dir(spec: &PackSpec, paths: &Paths) -> PathBuf {
    match &spec.source {
        Source::Path(p) => p.clone(),
        Source::Git { .. } | Source::Url { .. } => paths.pack.join(&spec.name),
    }
}

fn locate_in(dir: &Path, name: &str) -> Result<Module, String> {
    let manifest = dir.join(MANIFEST);
    let Ok(bytes) = std::fs::read(&manifest) else {
        return Ok(Module {
            wasm: dir.join(format!("{name}.wasm")),
            requested: None,
            fuel: None,
        });
    };
    let project = Project::new(Some(&bytes), None, 0)
        .map_err(|e| format!("{}: {:?}", manifest.display(), e.diagnostics.first()))?;
    let Some(rec) = project.plugins().first() else {
        return Err(format!("{}: no `plugin` record", manifest.display()));
    };
    let rel = match (rec.fields.get(1).map(String::as_str), rec.fields.get(2)) {
        (Some("path"), Some(rel)) if !rel.contains("..") && !Path::new(rel).is_absolute() => rel,
        _ => {
            return Err(format!(
                "{}: the plugin record must be `plugin <name> path <file>.wasm`",
                manifest.display()
            ));
        }
    };
    let requested = rec
        .attrs
        .iter()
        .filter(|a| a.key == "grant")
        .filter_map(|a| a.values.first().cloned())
        .collect();
    let fuel = rec
        .attrs
        .iter()
        .find(|a| a.key == "fuel")
        .and_then(|a| a.values.first()?.parse().ok());
    Ok(Module {
        wasm: dir.join(rel),
        requested: Some(requested),
        fuel,
    })
}

// ------------------------------------------------------------------ lockfile

fn read_lock(path: &Path) -> Result<Locked, String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(Locked::default());
    };
    Project::new(None, Some(&bytes), 0)
        .map(|p| p.lock().clone())
        .map_err(|e| format!("{}: {:?}", path.display(), e.diagnostics.first()))
}

fn set_row(lock: &mut Locked, name: &str, kind: &str, resolved: &str, hash: &str) {
    let row = match lock.find("plugin", name) {
        Some(r) => r,
        None => {
            lock.ty.push("plugin".into());
            lock.name.push(name.to_owned());
            lock.kind.push(String::new());
            lock.resolved.push(String::new());
            lock.hash.push(String::new());
            lock.name.len() - 1
        }
    };
    lock.kind[row] = kind.to_owned();
    lock.resolved[row] = resolved.to_owned();
    lock.hash[row] = hash.to_owned();
    assert_eq!(lock.ty.len(), lock.hash.len());
}

fn retain(lock: &mut Locked, keep: &[bool]) {
    assert_eq!(keep.len(), lock.name.len());
    let mut it = keep.iter();
    lock.ty.retain(|_| *it.next().expect("one flag per row"));
    for col in [
        &mut lock.name,
        &mut lock.kind,
        &mut lock.resolved,
        &mut lock.hash,
    ] {
        let mut it = keep.iter();
        col.retain(|_| *it.next().expect("one flag per row"));
    }
    assert_eq!(lock.ty.len(), lock.hash.len());
}

/// Writes the lockfile sorted by (type, name); an empty lock deletes the file.
fn write_lock(path: &Path, lock: &mut Locked) -> Result<(), String> {
    let mut order: Vec<usize> = (0..lock.name.len()).collect();
    order.sort_by(|&a, &b| (&lock.ty[a], &lock.name[a]).cmp(&(&lock.ty[b], &lock.name[b])));
    let sorted = Locked {
        ty: order.iter().map(|&i| lock.ty[i].clone()).collect(),
        name: order.iter().map(|&i| lock.name[i].clone()).collect(),
        kind: order.iter().map(|&i| lock.kind[i].clone()).collect(),
        resolved: order.iter().map(|&i| lock.resolved[i].clone()).collect(),
        hash: order.iter().map(|&i| lock.hash[i].clone()).collect(),
    };
    *lock = sorted;
    if lock.name.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    crate::platform::write_atomic(path, lock.write().as_bytes())
}

// ------------------------------------------------------------------ processes

/// Runs `git -C dir args…` without prompts; returns trimmed stdout.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            err.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn short(hash: &str) -> &str {
    &hash[..hash.len().min(12)]
}
