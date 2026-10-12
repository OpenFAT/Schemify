//! Reading cells out of a `doc::Project`: display names, the row of an open document,
//! and the readable-interface check every instance resolution goes through.

use schemify_doc::{Document, Project};

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
    Ok(row)
}

/// Returns the cell row whose current document is `doc` (an open, indexed file).
/// Cost: a scan of the rows of `doc`'s kind.
pub(crate) fn row_of(project: &Project, doc: &Document) -> Option<usize> {
    let cells = project.cells();
    (0..cells.lib.len())
        .filter(|&r| cells.kind[r] == doc.kind())
        .find(|&r| project.interface(r).is_some_and(|d| std::ptr::eq(d, doc)))
}
