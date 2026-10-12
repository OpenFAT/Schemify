//! `.sfyx` records → Design → Schemify files (spec `interop.md` §6, §7.3).

use super::{Import, ImportOptions};
use crate::design::Files;
use crate::map::Mapping;

/// See [`super::import_targets`].
pub fn targets(_sfyx: &str) -> Result<Vec<String>, Vec<String>> {
    Err(vec!["virtuoso import: not implemented yet".into()])
}

/// See [`super::import`].
pub fn import(
    _sfyx: &str,
    _existing: &Files,
    _map: &Mapping,
    _opts: &ImportOptions,
) -> Result<Import, Vec<String>> {
    Err(vec!["virtuoso import: not implemented yet".into()])
}
