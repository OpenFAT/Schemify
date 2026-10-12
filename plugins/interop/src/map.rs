//! `interop.map`: cell, pin and parameter translation between Schemify libraries and
//! Virtuoso libraries, and gm/Id retarget classes (spec `interop.md` §8.2).

/// A parsed mapping file. Empty = identity everywhere (mirrors, `schemify_core`).
#[derive(Clone, Debug, Default)]
pub struct Mapping {}

impl Mapping {
    /// Parses `interop.map` text.
    ///
    /// # Errors
    ///
    /// Every malformed row, one per line.
    pub fn parse(_text: &str) -> Result<Mapping, Vec<String>> {
        Ok(Mapping::default())
    }
}
