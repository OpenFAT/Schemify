//! The checks panel: the violation counts, the lines of a refused netlist or run, and a
//! clickable row per violation, as UI records.
//!
//! Contents:
//! - `pub fn contribute`

use schemify_elab::{CellState, Severity};

use super::PANEL_ROWS;

/// Appends the checks panel to the state contribution `t`: a header with the counts and a
/// close button, the `blocked` lines of the last refused netlist or run, then one clickable
/// row (`checks.row.<i>`, command `checks.pick`) per violation of `cell`, at most
/// [`PANEL_ROWS`].
///
/// # Panics
///
/// If `cell` holds more than `schemify_elab::VIOLATIONS_MAX` violations (elab's capacity).
pub fn contribute(t: &mut String, cell: Option<&CellState>, blocked: &[String]) {
    use std::fmt::Write as _;
    let start = t.len();
    let rows = cell.map_or(&[][..], |c| &c.violations.rows[..]);
    assert!(rows.len() <= schemify_elab::VIOLATIONS_MAX);
    let count = |s: Severity| rows.iter().filter(|v| v.code.severity() == s).count();
    let (e, w) = (count(Severity::Error), count(Severity::Warning));
    let q = crate::docs::quote;
    let _ = writeln!(
        t,
        "node checks.pane panel\n  parent bottom\n  style bottom-pane"
    );
    let _ = writeln!(
        t,
        "node checks.bar panel\n  parent checks.pane\n  style bottom-bar"
    );
    let title = format!(
        "Checks: {e} error(s), {w} warning(s), {} row(s)",
        rows.len()
    );
    let _ = writeln!(
        t,
        "node checks.title label\n  parent checks.bar\n  label {}\n  style bottom-title",
        q(&title)
    );
    let _ = writeln!(
        t,
        "node checks.close button\n  parent checks.bar\n  label x\n  command view.checks"
    );
    let _ = writeln!(
        t,
        "node checks.list scroll\n  parent checks.pane\n  style bottom-list"
    );
    for (i, b) in blocked.iter().enumerate() {
        let _ = writeln!(
            t,
            "node checks.blocked.{i} label\n  parent checks.list\n  label {}\n  style check-error",
            q(b)
        );
    }
    assert!(
        t.len() > start,
        "the pane, its bar and its list were written"
    );
    let Some(cell) = cell else { return };
    let shown = rows.iter().take(PANEL_ROWS);
    for (i, v) in shown.enumerate() {
        let style = match v.code.severity() {
            Severity::Error => "check-error",
            Severity::Warning => "check-warning",
            Severity::Info => "check-info",
        };
        let _ = writeln!(
            t,
            "node checks.row.{i} item\n  parent checks.list\n  label {}\n  command checks.pick\n  style {style}",
            q(&super::line(&cell.names, v))
        );
    }
    if rows.len() > PANEL_ROWS {
        let more = format!("… {} more", rows.len() - PANEL_ROWS);
        let _ = writeln!(
            t,
            "node checks.more label\n  parent checks.list\n  label {}",
            q(&more)
        );
    }
}
