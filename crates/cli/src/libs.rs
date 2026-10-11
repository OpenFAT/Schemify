//! The project on disk, without any GUI state: the `load` spine (manifest + lockfile →
//! library directories → `doc::index_library` per local library → the bundled `core`
//! library) and cell bytes by (library, path). Shared by the headless CLI spines and the
//! editor's `Session` (`schemify_app::session`).

use std::path::{Path, PathBuf};

use schemify_doc::{LibSource, Project};

use crate::platform;

/// Pseudo-directory of the bundled `core` library: its cells come from the binary.
pub const BUILTIN_DIR: &str = "<builtin>/core";

/// A loaded project: design data plus where each library lives.
#[derive(Debug)]
pub struct Loaded {
    /// Design data (`doc` is the only writer).
    pub project: Project,
    /// Directory per library row (`None`: builtin or not fetched).
    pub lib_dir: Vec<Option<PathBuf>>,
    /// Non-fatal findings (unfetched `git`/`url` libraries, index diagnostics).
    pub notes: Vec<String>,
}

/// Loads the project rooted at `root`. `git`/`url` libraries are reported in
/// [`Loaded::notes`], not fetched.
///
/// # Errors
///
/// The manifest or lockfile does not parse, or the bundled core library is refused.
pub fn load(root: &Path, seed: u64) -> Result<Loaded, String> {
    let manifest = platform::read(&root.join("schemify.project")).ok();
    let lock = platform::read(&root.join("schemify.lock")).ok();
    let mut project = Project::new(manifest.as_deref(), lock.as_deref(), seed)
        .map_err(|e| format!("schemify.project: {:?}", e.diagnostics.first()))?;
    let libs = project.libs();
    let mut lib_dir = Vec::with_capacity(libs.name.len());
    let mut notes = Vec::new();
    for row in 0..libs.name.len() {
        let dir = match &libs.source[row] {
            LibSource::Project => Some(root.to_path_buf()),
            LibSource::Path { path } => {
                let mut d = root.join(path);
                if let Some(sub) = &libs.subdir[row] {
                    d = d.join(sub);
                }
                Some(d)
            }
            LibSource::Builtin => None,
            other => {
                notes.push(format!(
                    "lib {}: {} sources are not fetched yet",
                    libs.name[row],
                    other.word()
                ));
                None
            }
        };
        lib_dir.push(dir);
    }
    // Libraries nested under the project root are indexed only by their own row.
    let nested: Vec<PathBuf> = lib_dir.iter().skip(1).flatten().cloned().collect();
    for (row, dir) in lib_dir.iter().enumerate() {
        let Some(dir) = dir else { continue };
        let others: Vec<PathBuf> = nested.iter().filter(|d| *d != dir).cloned().collect();
        if let Err(e) = index(&mut project, row, dir, &others) {
            notes.push(e);
        }
    }
    // D10: `lib core builtin` is implicit; its cells live in the binary.
    let core = project
        .libs()
        .find("core")
        .expect("doc always declares core");
    if project.libs().source[core] == LibSource::Builtin {
        let (_, diags) = project
            .index_library("core", BUILTIN_DIR, schemify_import::core_library())
            .map_err(|c| format!("lib core: {c}"))?;
        assert!(
            diags.is_empty(),
            "the bundled core library is clean: {diags:?}"
        );
    }
    assert_eq!(lib_dir.len(), project.libs().name.len());
    Ok(Loaded {
        project,
        lib_dir,
        notes,
    })
}

/// Indexes library `row` from the cell files under `dir`, skipping `skip` subtrees (also
/// after the editor writes or deletes a cell file: `Session::reindex`).
///
/// # Errors
///
/// A file cannot be read, or indexing reports a diagnostic (the first).
pub fn index(
    project: &mut Project,
    row: usize,
    dir: &Path,
    skip: &[PathBuf],
) -> Result<(), String> {
    let name = project.libs().name[row].clone();
    let mut rel = Vec::new();
    let mut bytes = Vec::new();
    for p in platform::list_files(dir, skip) {
        let Some(r) = rel_path(dir, &p) else { continue };
        if !is_cell_file(&r) {
            continue;
        }
        bytes.push(platform::read(&p)?);
        rel.push(r);
    }
    let files: Vec<(&str, &[u8])> = rel
        .iter()
        .map(String::as_str)
        .zip(bytes.iter().map(Vec::as_slice))
        .collect();
    let resolved = dir.display().to_string();
    let (_, diags) = project
        .index_library(&name, &resolved, &files)
        .map_err(|c| format!("lib {name}: {c}"))?;
    if let Some((path, d)) = diags.first() {
        return Err(format!("{name}/{path}: {} {}", d.code, d.detail));
    }
    Ok(())
}

/// The bytes of cell file `relpath` in library row `lib`, its path (under
/// [`BUILTIN_DIR`] for the bundled core library) and whether it is builtin (read-only,
/// never journaled).
///
/// # Errors
///
/// The library is not local, or the file cannot be read.
pub fn cell_bytes(
    loaded_dirs: &[Option<PathBuf>],
    project: &Project,
    lib: u8,
    relpath: &str,
) -> Result<(PathBuf, Vec<u8>, bool), String> {
    let row = usize::from(lib);
    if let Some(Some(dir)) = loaded_dirs.get(row) {
        let path = dir.join(relpath);
        let bytes = platform::read(&path)?;
        return Ok((path, bytes, false));
    }
    if project.libs().source.get(row) != Some(&LibSource::Builtin) {
        return Err(format!("library row {lib} is not local"));
    }
    let stem = relpath.rsplit_once('.').map_or(relpath, |(s, _)| s);
    let bytes = schemify_import::core::core_file(stem)
        .ok_or_else(|| format!("core:{stem}: not a bundled cell"))?;
    Ok((Path::new(BUILTIN_DIR).join(relpath), bytes.to_vec(), true))
}

/// The project directory of a file: the nearest ancestor holding `schemify.project`,
/// else the file's directory.
pub fn project_root(file: &Path) -> PathBuf {
    let abs = absolute(file);
    let mut dir = abs.parent().map(Path::to_path_buf);
    // Bounded by the path depth.
    while let Some(d) = dir {
        if platform::exists(&d.join("schemify.project")) {
            return d;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    abs.parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// The relative path of indexed cell `row` within its library.
pub fn cell_relpath(project: &Project, row: usize) -> String {
    let c = project.cells();
    let s = project.strings();
    let (dir, stem) = (s.get(c.dir[row]), s.get(c.stem[row]));
    let ext = c.kind[row].word();
    if dir.is_empty() {
        format!("{stem}.{ext}")
    } else {
        format!("{dir}/{stem}.{ext}")
    }
}

/// Whether `rel` names a cell file.
pub fn is_cell_file(rel: &str) -> bool {
    [".shprim", ".shcomp", ".shbench"]
        .iter()
        .any(|e| rel.ends_with(e))
}

/// `path` relative to `dir` with `/` separators, if inside it.
pub fn rel_path(dir: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(dir).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// `p` made absolute against the working directory.
pub fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| p.to_path_buf(), |d| d.join(p))
    }
}
