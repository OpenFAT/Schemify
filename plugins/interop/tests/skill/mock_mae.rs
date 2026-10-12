//! Mock maestro (`mae*`, `axl*`, `asi*`) functions.

use super::super::V;
use super::MockDb;

/// Maestro setups by (lib, cell).
#[derive(Default)]
pub struct Mae {}

/// Handles a maestro function; `None` when `name` is not one.
pub fn call(_db: &mut MockDb, _name: &str, _args: &[V]) -> Option<Result<V, String>> {
    None
}
