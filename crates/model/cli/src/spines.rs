//! The design spines the headless CLI (`schemify netlist|check|run`) and the editor (`n`, `s`,
//! Optimize) share: a cell file → an opened project → an elaboration that opened every cell
//! it needs → violation lines, netlist file names and (in `run`) a run up to the point where
//! the CLI waits and the editor polls (architecture.md § Spines, `netlist` and `run`; D22
//! dialects; run spec §5 simulator choice in `sim`).
//!
//! Both callers own their project differently (the CLI a bare `doc::Project`, the editor a
//! `Session` (`schemify_app::session`) with journals), so opening a cell is the one step
//! behind a trait, [`Cells`]: two implementations today, resolved at compile time (rung 5).
//!
//! Contents:
//! - `pub trait Cells { project, open_row }`
//! - `pub const DIALECTS: [(&str, Dialect, &str); 8]`
//! - `pub struct Opened { project, lib_dir, top }`, `pub enum OpenError { Io, Design }`
//! - `pub fn elaborate_all<C: Cells, F: Funcs>(&mut C, DocHandle, &Request, &mut F)
//!   -> Result<Elab, String>`
//! - `pub fn violation_line(&Names, &Violation) -> String`
//! - `pub fn dialect_of(&str) -> Option<Dialect>`, `pub fn dialect_word(Dialect) -> &str`
//! - `pub fn netlist_file(&Path, &str, Dialect) -> PathBuf`
//! - `pub fn env_pairs() -> Vec<(String, String)>`
//! - `pub fn open_file(&Path, u64) -> Result<(Opened, Vec<String>), OpenError>`
//! - `pub fn bench_name(&Project, impl Fn(u8) -> Option<&Path>, &Path) -> Option<String>`
//! - re-exported from `sim`: `SIMULATORS`, `sim_dialect`, `pin`, `run_dialect`, `tools`
//! - re-exported from `run` (native): `Run`, `start_run`, `BENCH`

#[cfg(not(target_arch = "wasm32"))]
mod run;
mod sim;

#[cfg(not(target_arch = "wasm32"))]
pub use run::{BENCH, Run, start_run};
#[cfg(not(target_arch = "wasm32"))]
pub use sim::tools;
pub use sim::{SIMULATORS, pin, run_dialect, sim_dialect};

use std::path::{Path, PathBuf};

use schemify_doc::{DocHandle, Project};
use schemify_elab::{Dialect, Elab, Funcs, Name, Names, Request, Violation};

use crate::libs;

/// How a caller opens the cell rows elaboration asks for.
pub trait Cells {
    /// The project elaboration reads.
    fn project(&self) -> &Project;
    /// Opens indexed cell `row` (with its journal, for the editor).
    ///
    /// # Errors
    ///
    /// The file cannot be read or does not parse.
    fn open_row(&mut self, row: u32) -> Result<(), String>;
}

/// The core dialects (D22): CLI word, dialect, netlist file extension. Words and extensions
/// are each unique.
pub const DIALECTS: [(&str, Dialect, &str); 8] = [
    ("ngspice", Dialect::Ngspice, "spice"),
    ("espice", Dialect::Espice, "sp"),
    ("xyce", Dialect::Xyce, "cir"),
    ("ltspice", Dialect::Ltspice, "net"),
    ("hspice", Dialect::Hspice, "hsp"),
    ("cdl", Dialect::Cdl, "cdl"),
    ("spectre", Dialect::Spectre, "scs"),
    ("verilog", Dialect::Verilog, "v"),
];

/// A project loaded from disk with one cell file open: the CLI's `load` spine (the editor's
/// equivalent is its session).
#[derive(Debug)]
pub struct Opened {
    /// Design data.
    pub project: Project,
    /// Directory per library row (`libs::Loaded::lib_dir`).
    pub lib_dir: Vec<Option<PathBuf>>,
    /// The opened file.
    pub top: DocHandle,
}

/// Why [`open_file`] failed: the CLI exits 2 on `Io`, 1 on `Design`.
#[derive(Debug, PartialEq, Eq)]
pub enum OpenError {
    /// The project or file cannot be read, or the file is outside every library.
    Io(String),
    /// The file does not parse or is refused by `doc`.
    Design(String),
}

/// Elaborates `top`, opening every cell elab asks for and elaborating again, at most once per
/// indexed cell plus one (each round opens at least one new cell).
///
/// # Errors
///
/// A cell cannot be opened, or elaboration keeps asking (a bug in the opener).
///
/// # Panics
///
/// Panics if `top` is not an open document of `c`'s project.
pub fn elaborate_all<C: Cells, F: Funcs>(
    c: &mut C,
    top: DocHandle,
    req: &Request<'_>,
    funcs: &mut F,
) -> Result<Elab, String> {
    assert!(c.project().doc(top).is_some(), "elaborate an open document");
    let rounds = c.project().cells().lib.len() + 1;
    for _ in 0..rounds {
        let e = schemify_elab::elaborate(c.project(), top, req, funcs);
        if e.needs_open.is_empty() {
            return Ok(e);
        }
        for &row in &e.needs_open {
            c.open_row(row)?;
        }
    }
    Err("elaboration kept asking for more cells".into())
}

/// One violation as the CLI prints it and the editor lists it:
/// `E005 lib:cell /X1 W: detail` (path and param only when set). `names` is the arena the
/// violation's names live in (`Elab::names` or `CellState::names`).
pub fn violation_line(names: &Names, v: &Violation) -> String {
    let name = |n: Name| (n != Name::NONE).then(|| names.get(n));
    let mut line = format!("{} {}", v.code.as_str(), name(v.cell).unwrap_or_default());
    for extra in [name(v.path), name(v.param)].into_iter().flatten() {
        line.push(' ');
        line.push_str(extra);
    }
    line.push_str(": ");
    line.push_str(&v.detail);
    line
}

/// The dialect a CLI word names (`ngspice espice xyce ltspice hspice cdl spectre verilog`).
pub fn dialect_of(word: &str) -> Option<Dialect> {
    DIALECTS.iter().find(|d| d.0 == word).map(|d| d.1)
}

/// The CLI word of `d`.
pub fn dialect_word(d: Dialect) -> &'static str {
    DIALECTS
        .iter()
        .find(|x| x.1 == d)
        .map_or("ngspice", |x| x.0)
}

/// Where the editor writes the netlist of cell `stem` in dialect `d`:
/// `<root>/.schemify/netlist/<stem>.<ext>` (git-ignored with the rest of `.schemify/`).
///
/// # Panics
///
/// Panics if `stem` is empty or holds a path separator.
pub fn netlist_file(root: &Path, stem: &str, d: Dialect) -> PathBuf {
    assert!(!stem.is_empty(), "a file stem");
    assert!(!stem.contains(['/', '\\']), "a file stem, not a path");
    let ext = DIALECTS.iter().find(|x| x.1 == d).map_or("spice", |x| x.2);
    root.join(".schemify")
        .join("netlist")
        .join(format!("{stem}.{ext}"))
}

/// The process environment for `${NAME}` expansion (netlist spec §7.1) and the design hash
/// (run spec §4), sorted by name so the hash does not depend on the OS's listing order.
/// Empty on the web.
pub fn env_pairs() -> Vec<(String, String)> {
    #[cfg(not(target_arch = "wasm32"))]
    let mut v: Vec<(String, String)> = std::env::vars().collect();
    #[cfg(target_arch = "wasm32")]
    let mut v: Vec<(String, String)> = Vec::new();
    v.sort_unstable();
    v
}

/// Loads the project around `file` (nearest `schemify.project`) and opens `file` in the
/// library whose directory holds it (deepest wins). Returns the load notes (unfetched
/// libraries, index findings) beside the result.
///
/// # Errors
///
/// See [`OpenError`].
pub fn open_file(file: &Path, seed: u64) -> Result<(Opened, Vec<String>), OpenError> {
    let root = libs::project_root(file);
    let abs = libs::absolute(file);
    let loaded = libs::load(&root, seed, Some(&abs)).map_err(OpenError::Io)?;
    let mut project = loaded.project;
    // The deepest library directory holding the file: (row, relative path, depth).
    let mut best: Option<(usize, String, usize)> = None;
    for (row, dir) in loaded.lib_dir.iter().enumerate() {
        let Some(dir) = dir else { continue };
        let dir = libs::absolute(dir);
        if let Some(rel) = libs::rel_path(&dir, &abs) {
            let depth = dir.components().count();
            if best.as_ref().is_none_or(|b| depth > b.2) {
                best = Some((row, rel, depth));
            }
        }
    }
    let Some((row, rel, _)) = best else {
        return Err(OpenError::Io(format!(
            "outside every library of the project at {}",
            root.display()
        )));
    };
    // The index read the file already (a cell file of the library); else read it now.
    let name = project.libs().name[row].clone();
    let opened = match loaded.kept {
        Some((_, bytes)) => project.open_indexed(&name, &rel, bytes, None),
        None => {
            let bytes = crate::platform::read(file).map_err(OpenError::Io)?;
            project.open(&name, &rel, &bytes, None)
        }
    };
    let top = opened
        .map_err(|e| OpenError::Design(format!("cannot open: {e:?}")))?
        .handle;
    assert!(project.doc(top).is_some());
    assert_eq!(loaded.lib_dir.len(), project.libs().name.len());
    let opened = Opened {
        project,
        lib_dir: loaded.lib_dir,
        top,
    };
    Ok((opened, loaded.notes))
}

/// The name a run's `bench` sidecar records for the bench at `path`: `lib:dir/stem` for the
/// library cell whose file is `path` (`stem` alone in the unnamed library), `None` when no
/// cell row's file is `path` (relative paths are taken from the working directory).
/// `lib_dir` gives a library row's directory. One function for the editor and `schemify
/// run`, so a run either one saved shows in the other's history.
///
/// Cost: one path join and compare per indexed cell.
pub fn bench_name<'a>(
    project: &Project,
    lib_dir: impl Fn(u8) -> Option<&'a Path>,
    path: &Path,
) -> Option<String> {
    // Library directories hang off the absolute project root (`libs::project_root`).
    let path = libs::absolute(path);
    let cells = project.cells();
    let (row, rel) = (0..cells.lib.len()).find_map(|r| {
        let dir = lib_dir(cells.lib[r])?;
        let rel = libs::cell_relpath(project, r);
        (dir.join(&rel) == path).then_some((r, rel))
    })?;
    let lib = &project.libs().name[usize::from(cells.lib[row])];
    let stem = rel.rsplit_once('.').map_or(rel.as_str(), |(s, _)| s);
    assert!(!stem.is_empty(), "a cell file has a stem");
    Some(if lib.is_empty() {
        stem.to_owned()
    } else {
        format!("{lib}:{stem}")
    })
}

impl Cells for Opened {
    fn project(&self) -> &Project {
        &self.project
    }

    fn open_row(&mut self, row: u32) -> Result<(), String> {
        let row = row as usize;
        assert!(row < self.project.cells().lib.len(), "an indexed cell row");
        let lib = self.project.cells().lib[row];
        let rel = libs::cell_relpath(&self.project, row);
        let (_, bytes, _) = libs::cell_bytes(&self.lib_dir, &self.project, lib, &rel)
            .map_err(|e| format!("{rel}: {e}"))?;
        let name = self.project.libs().name[usize::from(lib)].clone();
        self.project
            .open(&name, &rel, &bytes, None)
            .map(|_| ())
            .map_err(|e| format!("{name}:{rel}: cannot open: {e:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_words_round_trip_and_files_land_in_dot_schemify() {
        for (w, d, ext) in DIALECTS {
            assert_eq!(dialect_of(w), Some(d));
            assert_eq!(dialect_word(d), w);
            let f = netlist_file(Path::new("/p"), "top", d);
            assert_eq!(
                f,
                Path::new("/p/.schemify/netlist").join(format!("top.{ext}"))
            );
        }
        assert_eq!(dialect_of("vacask"), None);
        assert_eq!(dialect_of(""), None);
        // The simulators are netlist dialects; each dialect has its own netlist file.
        assert!(
            SIMULATORS
                .iter()
                .all(|s| DIALECTS.iter().any(|d| d.1 == *s))
        );
        let exts: std::collections::BTreeSet<&str> = DIALECTS.iter().map(|d| d.2).collect();
        assert_eq!(exts.len(), DIALECTS.len(), "one netlist file per dialect");
    }

    #[test]
    #[should_panic(expected = "a file stem, not a path")]
    fn netlist_file_refuses_a_path_as_stem() {
        let _ = netlist_file(Path::new("/p"), "a/b", Dialect::Ngspice);
    }
}
