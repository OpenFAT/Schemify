//! Design → `.sfyx` records (spec `interop.md` §6).

use super::{Export, ExportOptions};
use crate::design::Design;
use crate::map::Mapping;

/// See [`super::export`].
pub fn export(
    _d: &Design,
    _tops: &[usize],
    _map: &Mapping,
    _opts: &ExportOptions,
) -> Result<Export, Vec<String>> {
    Err(vec!["virtuoso export: not implemented yet".into()])
}
