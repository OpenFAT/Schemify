//! Oracle-backed tests for `schemify-wave` (TESTING.md, wave row). One test binary; one module per
//! oracle. Fixtures in `tests/fixtures/` are real ngspice-44 and Xyce-7.10 outputs of the `.cir`
//! decks beside them (binary and ASCII of the same run); `oracle::regenerate_and_compare` reruns
//! the simulators when they are installed.

mod calc;
mod fuzz;
mod oracle;
mod view;

use schemify_wave::{RawPlot, parse_raw};

/// Reads and parses a fixture.
pub(crate) fn fixture(name: &str) -> Vec<RawPlot> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    parse_raw(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Relative closeness: `|a - b| <= rtol * max(|a|, |b|)` (so exact zeros must match exactly).
pub(crate) fn close(a: f64, b: f64, rtol: f64) -> bool {
    a == b || (a - b).abs() <= rtol * a.abs().max(b.abs())
}
