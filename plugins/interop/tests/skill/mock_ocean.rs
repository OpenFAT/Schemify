//! Mock OCEAN functions (simulator setup, DC sweeps, results) for the retarget flow.

use super::super::V;
use super::MockDb;

/// Simulation state.
#[derive(Default)]
pub struct Ocean {}

/// Handles an OCEAN function; `None` when `name` is not one.
pub fn call(_db: &mut MockDb, _name: &str, _args: &[V]) -> Option<Result<V, String>> {
    None
}
