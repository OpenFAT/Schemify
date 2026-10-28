//! The editor shell: boot without the default UI plugin (D6), keymap coverage against the
//! xschem inventory (D25), hit-testing and box-selection properties, and the CPU golden image
//! of the editor on the inverter fixture (dark theme).

mod boot;
#[path = "../common/mod.rs"]
mod common;
mod hits;
mod keymap;
mod views;

use common::Session;

/// Prop `p` of UI node `id` as text ("" when absent).
fn node_prop(s: &Session, id: &str, p: schemify_ui::Prop) -> String {
    let sh = &s.ed.shell();
    let n = sh
        .strings()
        .find(id)
        .and_then(|i| sh.tables().nodes.find(i))
        .unwrap_or_else(|| panic!("{id} composed"));
    sh.strings()
        .get(sh.tables().nodes.prop(p)[n as usize])
        .to_owned()
}
