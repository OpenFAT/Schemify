//! A mock Virtuoso database for the SKILL test interpreter (spec `interop.md` §5.4).
//!
//! Each Cadence function checks its arity and argument types against the documented
//! signature (research notes) and fails as documented (most return `nil`). The maestro and
//! OCEAN parts live in `mock_mae.rs` and `mock_ocean.rs`.

use super::{Host, V};

#[path = "mock_mae.rs"]
pub mod mae;
#[path = "mock_ocean.rs"]
pub mod ocean;

/// The mock database: libraries, cells, cellviews and their objects, CDF, maestro setups.
#[derive(Default)]
pub struct MockDb {
    /// Maestro state.
    pub mae: mae::Mae,
    /// OCEAN / simulation state.
    pub ocean: ocean::Ocean,
}

impl MockDb {
    fn call_db(&mut self, _name: &str, _args: &[V]) -> Option<Result<V, String>> {
        None
    }
}

impl Host for MockDb {
    fn call(&mut self, name: &str, args: &[V]) -> Option<Result<V, String>> {
        if let Some(r) = self.call_db(name, args) {
            return Some(r);
        }
        if let Some(r) = mae::call(self, name, args) {
            return Some(r);
        }
        ocean::call(self, name, args)
    }

    fn get(&mut self, obj: u32, attr: &str) -> Result<V, String> {
        Err(format!("db:{obj}~>{attr}: no such object"))
    }

    fn set(&mut self, obj: u32, attr: &str, _value: V) -> Result<V, String> {
        Err(format!("db:{obj}~>{attr}: no such object"))
    }
}
