//! The ratchet: every command the default UI's records name resolves in the command table.

use schemify_editor::commands::Target;

use crate::common::{Session, Ui};

const UI: &str = include_str!(concat!(
    env!("SCHEMIFY_ROOT"),
    "/ui/default-plugin/ui.records"
));
const KEYMAP: &str = include_str!(concat!(
    env!("SCHEMIFY_ROOT"),
    "/ui/default-plugin/keymap.records"
));

/// Unresolved command names allowed. Lower it when a command lands; never raise it.
/// 2026-10-11: 198 before the parity pass, 0 after it.
const RATCHET: usize = 0;

/// Every command name the default UI records reference, sorted and distinct.
fn named_commands() -> Vec<&'static str> {
    let mut names: Vec<&str> = UI
        .lines()
        .filter_map(|l| l.strip_prefix("  command "))
        .chain(KEYMAP.lines().filter_map(|l| {
            let mut t = l.split_whitespace();
            (t.next() == Some("key")).then(|| t.nth(1)).flatten()
        }))
        .map(str::trim)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

#[test]
#[allow(clippy::absurd_extreme_comparisons)] // the ratchet reached 0; `<=` stays its form
fn every_default_ui_command_resolves() {
    let s = Session::open("inv", "top.shcomp", "parity", Ui::Default);
    let names = named_commands();
    assert!(names.len() > 200, "the records were read: {}", names.len());
    let unresolved: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| s.ed.commands().resolve_arg(n).0 == Target::Unknown)
        .collect();
    assert!(
        unresolved.len() <= RATCHET,
        "{} unresolved command(s) (ratchet {RATCHET}):\n{}",
        unresolved.len(),
        unresolved.join("\n")
    );
    // Negative space: a name nobody implements does not resolve.
    assert_eq!(
        s.ed.commands().resolve_arg("no.such_command").0,
        Target::Unknown
    );
}
