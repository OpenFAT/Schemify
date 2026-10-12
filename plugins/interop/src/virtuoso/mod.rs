//! Cadence Virtuoso bridge (spec `interop.md` §4–§9): Design ⇄ `.sfyx` records.
//!
//! Spine ([`export`], [`import`]): see the spec's §1 table. Submodules: `read` (records →
//! Design), `write` (Design → records), `ael` (values, §3.4), `devices` (§6.5–§6.6),
//! `ade` (maestro ⇄ bench, §9).

use crate::design::{Design, Files};
use crate::map::Mapping;
use crate::notes::Notes;

pub mod ade;
pub mod ael;
pub mod devices;
pub mod read;
pub mod write;

/// Export settings.
#[derive(Clone, Debug, Default)]
pub struct ExportOptions {
    /// Schemify library → Virtuoso library renames (`--lib L=V`); unlisted keep their name.
    pub lib_map: std::collections::BTreeMap<String, String>,
    /// Re-master `core:*` instances onto analogLib (§6.4) instead of exporting `schemify_core`.
    pub analoglib: bool,
    /// Write gm/Id intent for `gmid.w` instances (§10).
    pub retarget: bool,
    /// The producer string of the header.
    pub producer: String,
}

/// An export's result.
#[derive(Clone, Debug, Default)]
pub struct Export {
    /// The `.sfyx` text.
    pub sfyx: String,
    /// Report rows.
    pub notes: Notes,
}

/// Exports cells `tops` of `d` (and every cell they instantiate that is not a Virtuoso
/// mirror) as one `.sfyx` text.
///
/// # Errors
///
/// Problems that make the export meaningless (an unmapped pin, a cell the mapping names but
/// the Design lacks), one per line; nothing is written then.
pub fn export(
    d: &Design,
    tops: &[usize],
    map: &Mapping,
    opts: &ExportOptions,
) -> Result<Export, Vec<String>> {
    write::export(d, tops, map, opts)
}

/// Import settings.
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// Project-relative directory for libraries the manifest does not know (default: the
    /// library name).
    pub lib_dir: std::collections::BTreeMap<String, String>,
    /// Virtuoso library → Schemify library (`--into`, the inverse of an export's `--lib`);
    /// unlisted libraries keep their name, `schemify` maps to the project root and
    /// `schemify_core` to the bundled `core`.
    pub lib_map: std::collections::BTreeMap<String, String>,
    /// Re-master analogLib instances (§6.5 rows) onto core cells (§6.4).
    pub analoglib: bool,
}

/// An import's result: files to write (project-relative path, bytes) and the report.
#[derive(Clone, Debug, Default)]
pub struct Import {
    /// Files to write, sorted by path; includes `schemify.project` when libraries were added.
    pub files: Vec<(String, Vec<u8>)>,
    /// Report rows.
    pub notes: Notes,
}

/// The project files an import of `sfyx` reads to merge (§7.3): the manifest and every
/// target cell file's candidate paths. The guest reads these (a failed read means absent)
/// before calling [`import`].
///
/// # Errors
///
/// The text is not a complete `.sfyx` file.
pub fn import_targets(sfyx: &str) -> Result<Vec<String>, Vec<String>> {
    read::targets(sfyx)
}

/// Converts a `.sfyx` dump into Schemify files, merging with `existing` (§7.3).
///
/// # Errors
///
/// The text is not a complete `.sfyx` file, or a record is malformed (with its line).
pub fn import(
    sfyx: &str,
    existing: &Files,
    map: &Mapping,
    opts: &ImportOptions,
) -> Result<Import, Vec<String>> {
    read::import(sfyx, existing, map, opts)
}
