//! `schemify publish`: project directories → a static site the web editor serves them from
//! (web spec §3.1): `<site>/projects/<name>/` per project with its listed files, its saved
//! runs and the manifest (`schemify_cli::hosted`), plus `<site>/projects/index.html`.
//!
//! Contents: `fn publish(&[String]) -> Exit`.

#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

#[cfg(not(target_arch = "wasm32"))]
use schemify_cli::{hosted, spines};

#[cfg(target_arch = "wasm32")]
use crate::USAGE_ERROR;
use crate::Exit;
#[cfg(not(target_arch = "wasm32"))]
use crate::{OK, USAGE_ERROR, usage};

/// `publish <project-dir>... -o <site-dir>`: writes each project to `<site>/projects/<name>/`
/// (its listed files plus its saved runs) with the manifest the web editor fetches, and
/// `<site>/projects/index.html` linking every bench with how its newest run went. The web
/// editor's own files (`index.html`, `pkg/`) go at `<site>/` beside it. A project already in
/// the site is replaced whole.
#[cfg(not(target_arch = "wasm32"))]
pub fn publish(args: &[String]) -> Exit {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" => out = it.next().map(PathBuf::from),
            s if s.starts_with('-') => out = None,
            s => dirs.push(PathBuf::from(s)),
        }
    }
    let (Some(site), false) = (out, dirs.is_empty()) else {
        return Err(usage());
    };
    let mut index: Vec<(String, Vec<hosted::Bench>)> = Vec::with_capacity(dirs.len());
    for dir in &dirs {
        let (name, benches) = publish_one(dir, &site, &index)?;
        index.push((name, benches));
    }
    assert_eq!(index.len(), dirs.len());
    let page = site.join("projects").join("index.html");
    fail(&page, std::fs::write(&page, hosted::index_html(&index)))?;
    Ok(OK)
}

/// `publish` on the web: there is no site directory to write.
#[cfg(target_arch = "wasm32")]
pub fn publish(_: &[String]) -> Exit {
    eprintln!("schemify publish: not on the web (it writes a site directory)");
    Err(USAGE_ERROR)
}

// ---- private ----

/// Publishes the project at `dir` into `site` beside the projects of `index`; returns its
/// name and benches for the index page.
#[cfg(not(target_arch = "wasm32"))]
fn publish_one(
    dir: &Path,
    site: &Path,
    index: &[(String, Vec<hosted::Bench>)],
) -> Result<(String, Vec<hosted::Bench>), u8> {
    let root = schemify_cli::libs::absolute(dir);
    if !root.join("schemify.project").is_file() {
        eprintln!("{}: no schemify.project", dir.display());
        return Err(USAGE_ERROR);
    }
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if hosted::project_name(&format!("/projects/{name}")).is_none() {
        eprintln!(
            "{}: `{name}` is not a project name (A-Z a-z 0-9 . _ -)",
            dir.display()
        );
        return Err(USAGE_ERROR);
    }
    if index.iter().any(|(n, _)| *n == name) {
        eprintln!("{}: a second project named `{name}`", dir.display());
        return Err(USAGE_ERROR);
    }
    // Read every file before the site is touched.
    let mut paths = schemify_cli::platform::list_files(&root, &[]);
    walk_files(&root.join(".schemify").join("runs"), &mut paths);
    let mut files: Vec<(String, Vec<u8>)> = Vec::with_capacity(paths.len());
    for p in &paths {
        let rel = p.strip_prefix(&root).expect("listed under root");
        let rel = rel.to_string_lossy().replace('\\', "/");
        files.push((rel, fail(p, std::fs::read(p))?));
    }
    if files.len() > hosted::FILES_MAX {
        eprintln!("{name}: more than {} files", hosted::FILES_MAX);
        return Err(USAGE_ERROR);
    }
    // Replace the published copy whole.
    let dest = site.join("projects").join(&name);
    let _ = std::fs::remove_dir_all(&dest);
    let manifest = hosted::manifest(&files);
    for (rel, bytes) in &files {
        let to = dest.join(rel);
        let written = to
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&to, bytes));
        fail(&to, written)?;
    }
    fail(&dest, std::fs::write(dest.join(hosted::MANIFEST), &manifest))?;
    let benches: Vec<hosted::Bench> = files
        .iter()
        .filter(|(rel, _)| rel.ends_with(".shbench") && !rel.starts_with(".schemify/"))
        .map(|(rel, _)| hosted::Bench {
            path: rel.clone(),
            jobs: newest_run_jobs(&root, &root.join(rel)),
        })
        .collect();
    println!("{name}: {} files", files.len());
    Ok((name, benches))
}

/// Every file under `dir`, recursively and in sorted order, hidden ones included (a run
/// directory's files); nothing when `dir` is missing. Stops past `hosted::FILES_MAX` files
/// (the caller refuses that many).
#[cfg(not(target_arch = "wasm32"))]
fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if out.len() > hosted::FILES_MAX {
            return;
        }
        if p.is_dir() {
            walk_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// `(done, failed)` jobs of the newest saved run of the bench at `bench` (by its results
/// file's time), from the `bench` sidecars (run spec §7a); `None` without one.
#[cfg(not(target_arch = "wasm32"))]
fn newest_run_jobs(root: &Path, bench: &Path) -> Option<(u32, u32)> {
    let (o, _) = spines::open_file(bench, 0).ok()?;
    let name = spines::bench_name(
        &o.project,
        |lib| o.lib_dir.get(usize::from(lib))?.as_deref(),
        bench,
    )?;
    let runs = std::fs::read_dir(root.join(".schemify").join("runs")).ok()?;
    let (_, results) = runs
        .flatten()
        .map(|e| e.path())
        .filter(|d| {
            std::fs::read_to_string(d.join(spines::BENCH)).is_ok_and(|s| s.trim() == name)
        })
        .filter_map(|d| {
            let results = d.join(schemify_run::RESULTS);
            let t = std::fs::metadata(&results)
                .and_then(|m| m.modified())
                .ok()?;
            Some((t, results))
        })
        .max()?;
    let text = std::fs::read_to_string(results).ok()?;
    let jobs = |end: &str| {
        text.lines()
            .filter(|l| l.starts_with("job ") && l.ends_with(end))
            .count() as u32
    };
    Some((jobs(" done"), jobs(" failed")))
}

/// Prints `<path>: <error>` for a failed I/O step and turns it into [`USAGE_ERROR`].
#[cfg(not(target_arch = "wasm32"))]
fn fail<T>(path: &Path, r: std::io::Result<T>) -> Result<T, u8> {
    r.map_err(|e| {
        eprintln!("{}: {e}", path.display());
        USAGE_ERROR
    })
}
