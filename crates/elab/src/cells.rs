//! Reading cells out of a `doc::Project`: resolution from any cell (not only open
//! documents), display names, and whether a cell's full document is loaded.
//!
//! `Project::resolve` needs a `DocHandle` for the referencing file; elaboration walks
//! cells by library row, most of them not open, so `resolve_in_row` applies the same
//! rules (netlist spec §1) from a row's library and directory.

use schemify_doc::{CellRef, Document, Project};
use schemify_format::FileKind;

/// Returns whether cell `row`'s full document is open (so its schematic, vars and
/// bodies are present), as opposed to only its indexed interface (ranks 0–1).
pub(crate) fn is_open(project: &Project, row: usize) -> bool {
    match (project.interface(row), project.cells().iface[row].as_ref()) {
        (Some(d), Some(indexed)) => !std::ptr::eq(d, indexed),
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// Returns `(display name, stem)` of cell `row`: `lib:dir/stem`, without `lib:` for
/// the project's own cells and without `dir/` at a library root.
pub(crate) fn display(project: &Project, row: usize) -> (String, String) {
    let (c, s) = (project.cells(), project.strings());
    let lib = &project.libs().name[c.lib[row] as usize];
    let (dir, stem) = (s.get(c.dir[row]), s.get(c.stem[row]));
    let path = if dir.is_empty() {
        stem.to_owned()
    } else {
        format!("{dir}/{stem}")
    };
    let cell = if lib.is_empty() {
        path
    } else {
        format!("{lib}:{path}")
    };
    assert!(!stem.is_empty());
    (cell, stem.to_owned())
}

/// Resolves `cellref` written in the file of cell `row` (netlist spec §1): `lib:cell`
/// through the manifest, `./cell` in the row's directory, `.shcomp` before `.shprim`.
/// Cost: a linear scan of the cell rows.
///
/// # Errors
///
/// A reason text for `E001`.
pub(crate) fn resolve_in_row(
    project: &Project,
    row: usize,
    cellref: &str,
) -> Result<usize, String> {
    let (cells, s) = (project.cells(), project.strings());
    let missing = || format!("cannot resolve `{cellref}`");
    let (lib, dir, cell) = match CellRef::parse(cellref).ok_or_else(missing)? {
        CellRef::Local { cell } => (cells.lib[row], Some(cells.dir[row]), cell),
        CellRef::Lib { lib, cell } => {
            let l = project
                .libs()
                .find(lib)
                .filter(|&l| l != 0)
                .ok_or_else(missing)?;
            (l as u8, s.find(""), cell)
        }
    };
    let (dir, stem) = (dir.ok_or_else(missing)?, s.find(cell).ok_or_else(missing)?);
    let found = [FileKind::Shcomp, FileKind::Shprim]
        .into_iter()
        .find_map(|k| {
            (0..cells.lib.len()).find(|&r| {
                cells.lib[r] == lib
                    && cells.dir[r] == dir
                    && cells.stem[r] == stem
                    && cells.kind[r] == k
            })
        });
    let r = found.ok_or_else(missing)?;
    if project.interface(r).is_none() {
        return Err(format!("`{cellref}` has no readable interface"));
    }
    assert!(cells.kind[r] != FileKind::Shbench);
    Ok(r)
}

/// Returns the cell row whose current document is `doc` (an open, indexed file).
/// Cost: a scan of the rows of `doc`'s kind.
pub(crate) fn row_of(project: &Project, doc: &Document) -> Option<usize> {
    let cells = project.cells();
    (0..cells.lib.len())
        .filter(|&r| cells.kind[r] == doc.kind())
        .find(|&r| project.interface(r).is_some_and(|d| std::ptr::eq(d, doc)))
}
