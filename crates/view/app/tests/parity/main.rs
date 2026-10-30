//! xschem parity, end to end through the frame spine: the ratchet over every command the
//! default UI names, then one oracle-backed test per parity feature (docs/dev/testing.md: oracles,
//! negative space). Oracles: the canonical record text of the edited document (and undo
//! restoring it byte for byte), files written by exports and saves (decoded or parsed back),
//! elab connectivity before/after (stretch), and properties over seeded inputs (stretch,
//! search/replace, join/trim).

#[path = "../common/mod.rs"]
mod common;
mod edit;
mod file;
mod menus;
mod oracles;
mod ratchet;
mod tools;

use std::path::PathBuf;

use schemify_ui::{Button, InputEvent};

use crate::common::Session;

const SHIFT: u8 = schemify_ui::Mods::SHIFT;
const CTRL: u8 = schemify_ui::Mods::CTRL;

/// Lines of the active document starting with `prefix`, owned.
fn owned(s: &Session, prefix: &str) -> Vec<String> {
    common::lines(&s.text(), prefix)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn active_path(s: &Session) -> PathBuf {
    let (h, _) = s.ed.active().expect("a cell is open");
    s.ed.session()
        .path(h)
        .expect("the open cell has a file")
        .to_path_buf()
}

/// Right click (press + release) at world point `p` with modifiers `mods`.
fn right_click(s: &mut Session, p: (i32, i32), mods: u8) {
    let (x, y) = s.ed.to_screen(p);
    for down in [true, false] {
        s.send([
            InputEvent::Mods {
                mods: schemify_ui::Mods(mods),
            },
            InputEvent::PointerMove { x, y },
            if down {
                InputEvent::PointerDown {
                    x,
                    y,
                    button: Button::Right,
                }
            } else {
                InputEvent::PointerUp {
                    x,
                    y,
                    button: Button::Right,
                }
            },
        ]);
    }
    s.send([InputEvent::Mods {
        mods: schemify_ui::Mods::default(),
    }]);
}

/// Whether UI node `id` is composed (exists in the UI table this frame).
fn composed(s: &Session, id: &str) -> bool {
    let sh = &s.ed.shell();
    sh.strings()
        .find(id)
        .is_some_and(|sym| sh.tables().nodes.find(sym).is_some())
}

/// The open dialog's node id ("" when none).
fn dialog(s: &Session) -> String {
    let d = s.ed.shell().ui().dialog();
    if d.is_none() {
        String::new()
    } else {
        s.ed.shell().strings().get(d).to_owned()
    }
}

/// Connectivity of the active schematic: (pin identity, component), pins of instances by
/// (instance id, pin index), ports by id.
fn pin_nets(s: &Session) -> Vec<(String, u32)> {
    let (h, _) = s.ed.active().expect("a cell is open");
    let doc =
        s.ed.session()
            .project
            .doc(h)
            .expect("the open cell's document is loaded");
    let cell = s.ed.cell().expect("elaborated");
    let mut out = Vec::new();
    let inst = doc.instances();
    for i in 0..inst.id.len() {
        for (k, (c, _)) in schemify_editor::checks::pins_of(doc, cell, i)
            .iter()
            .enumerate()
        {
            out.push((format!("{}.{k}", inst.id[i].0), *c));
        }
    }
    for (r, id) in doc.ports().id.iter().enumerate() {
        out.push((format!("port.{}", id.0), cell.nets.port_net[r]));
    }
    out.sort();
    out
}

/// A tiny deterministic generator for the seeded property tests (no dependency).
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }
}
