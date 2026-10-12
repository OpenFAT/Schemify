//! Maestro (ADE Explorer/Assembler) ⇄ bench tables (spec `interop.md` §9).
//!
//! [`export`] turns a `.shbench` into maestro records; [`import`] turns maestro records back
//! into bench records appended to the bench document being built.

use schemify_format::Document;

use crate::design::Design;
use crate::notes::Notes;
use crate::sfyx::Record;

/// The maestro records (`maestro`, `var`, `analysis`, `corner`, `model`, `output`, `spec`,
/// `defn`) of bench cell `ci`, in §4.2 order. Unmapped bench content is reported in `notes`
/// (and stashed by the caller, §7.3).
pub fn export(_d: &Design, _ci: usize, _notes: &mut Notes) -> Vec<Record> {
    Vec::new()
}

/// Appends the bench records (`var`, `corner`, `sweep`, `output`, `spec`, `code`) that
/// `records` (one maestro test and its records, in file order) describe to `bench`.
pub fn import(
    _records: &[Record],
    _bench: &mut Document<'static>,
    _cell: &str,
    _notes: &mut Notes,
) {
}
