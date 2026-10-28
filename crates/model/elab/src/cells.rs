//! A `doc::Project` in, facts about its cells out: display names, the row of an open
//! document, and the readable-interface check every instance resolution goes through.
//! Cites netlist spec §1 (resolution) and pdk spec §5 (`tech:` cells).
//!
//! Contents:
//! - `pub(crate) struct CellName`: `top`
//! - `pub(crate) fn display(project: &Project, row: usize) -> CellName`
//! - `pub(crate) fn readable(project, found, cellref) -> Result<usize, String>`
//! - `pub(crate) fn row_of(project: &Project, doc: &Document) -> Option<usize>`

use schemify_doc::{Document, Project};

/// A cell's display name (`lib:dir/stem`) and its stem.
pub(crate) struct CellName {
    pub(crate) cell: String,
    pub(crate) stem: String,
}

impl CellName {
    /// The name of a document that is no project cell (an unsaved or foreign file).
    pub(crate) fn top() -> CellName {
        CellName {
            cell: "top".to_owned(),
            stem: "top".to_owned(),
        }
    }
}

/// Returns the names of cell `row`: `lib:dir/stem`, without `lib:` for the project's own
/// cells and without `dir/` at a library root.
pub(crate) fn display(project: &Project, row: usize) -> CellName {
    let (c, s) = (project.cells(), project.strings());
    assert!(row < c.lib.len(), "cell row {row} exists");
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
    assert!(!stem.is_empty(), "every cell file has a stem");
    assert!(cell.ends_with(stem));
    CellName {
        cell,
        stem: stem.to_owned(),
    }
}

/// Checks a resolution of `cellref` (`Project::resolve` or `Project::resolve_in`, netlist
/// spec §1) for an instance: the cell must also have a readable interface.
///
/// # Errors
///
/// The `E001` reason text: unresolved, or resolved to a cell whose file did not index.
pub(crate) fn readable(
    project: &Project,
    found: Option<usize>,
    cellref: &str,
) -> Result<usize, String> {
    let row = found.ok_or_else(|| match cellref.strip_prefix("tech:") {
        // `tech:<class>` or, from elaboration, `tech:<class> (<flavor>)`.
        Some(rest) => {
            let (class, flavor) = rest
                .split_once(" (")
                .map_or((rest, "core"), |(c, f)| (c, f.trim_end_matches(')')));
            format!("`{cellref}`: {}", project.explain_tech(class, flavor))
        }
        None => format!("cannot resolve `{cellref}`"),
    })?;
    if project.interface(row).is_none() {
        return Err(format!("`{cellref}` has no readable interface"));
    }
    assert!(row < project.cells().lib.len(), "a resolved row is a cell");
    Ok(row)
}

/// Returns the cell row whose current document is `doc` (an open, indexed file).
/// Cost: a scan of the rows of `doc`'s kind.
pub(crate) fn row_of(project: &Project, doc: &Document) -> Option<usize> {
    let cells = project.cells();
    let row = (0..cells.lib.len())
        .filter(|&r| cells.kind[r] == doc.kind())
        .find(|&r| project.interface(r).is_some_and(|d| std::ptr::eq(d, doc)));
    if let Some(r) = row {
        assert_eq!(cells.kind[r], doc.kind());
    }
    row
}
