//! Schemify → xschem `.sch`/`.sym` export (spec `interop.md` §11).

use crate::design::Design;
use crate::notes::Notes;

/// An xschem export: files (output-relative paths `<lib>/<cell>.sym|.sch`, an `xschemrc`
/// fragment) and the report.
#[derive(Clone, Debug, Default)]
pub struct XschemExport {
    /// (path, bytes), sorted by path.
    pub files: Vec<(String, Vec<u8>)>,
    /// Report rows.
    pub notes: Notes,
}

/// Exports cells `tops` of `d` and every cell they instantiate that has no xschem original.
///
/// # Errors
///
/// Problems that make the export meaningless, one per line.
pub fn export(_d: &Design, _tops: &[usize]) -> Result<XschemExport, Vec<String>> {
    Err(vec!["xschem export: not implemented yet".into()])
}
