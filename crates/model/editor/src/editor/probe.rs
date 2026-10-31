//! The waves dock: opening files and cross-probing into it (D23), its divider, and the
//! per-frame input tracking (modifiers, the pointer before the first key) the whole editor
//! reads.
//!
//! Contents:
//! - `pub(super) const DOCK_SPLIT`, `DOCK_SPLIT_RANGE`, `DOCK_GRAB_PX`
//! - `Editor::open_waves`
//! - `Editor::send_to_viewer`
//! - `Editor::cross_probe`
//! - `Editor::dock_divider`
//! - `Editor::track_input`

use std::path::PathBuf;

use schemify_doc::DocHandle;
use schemify_elab::Name;
use schemify_ui::{Mods, egui};

use crate::geometry::Pt;
use crate::waves::WavesView;
use crate::{checks, platform};

use super::Editor;
use super::highlight::PIN_PX;

/// Share of the window height the editor keeps when the waves dock opens.
pub(super) const DOCK_SPLIT: f32 = 0.6;

/// Range the dock divider can be dragged in, as shares of the window height.
pub(super) const DOCK_SPLIT_RANGE: (f32, f32) = (0.15, 0.85);

/// Logical px either side of the dock divider that grab it.
pub(super) const DOCK_GRAB_PX: f32 = 4.0;

impl Editor {
    /// Opens the waves dock on `path`, or points an open dock at it keeping its traces and panes
    /// (spec waves §9).
    pub(super) fn open_waves(&mut self, path: PathBuf) {
        self.close_dialog();
        let title = path.display().to_string();
        let bytes = match platform::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.session.say(e);
                return;
            }
        };
        match self.waves.as_mut() {
            Some(w) => match w.reload(&title, &bytes) {
                Ok(s) | Err(s) => self.session.say(s),
            },
            None => match WavesView::open(&title, &bytes) {
                Ok(w) => self.waves = Some(w),
                Err(e) => self.session.say(e),
            },
        }
    }
    /// `Alt+G`: adds `v(<net>)` to the waves dock for every highlighted net (keys the name map
    /// resolved; level paths are skipped), opening the dock on the selected job when closed.
    pub(super) fn send_to_viewer(&mut self) {
        if self.highlight.is_empty() {
            self.session.say("no highlighted nets (k highlights)");
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.waves.is_none() {
            self.open_job_waves();
        }
        let Some(w) = self.waves.as_mut() else {
            self.session
                .say("send to viewer: no waves open (simulate first)");
            return;
        };
        let mut sent = Vec::new();
        let mut failed = 0;
        for key in self.highlight.iter().filter(|k| !k.starts_with('/')) {
            let src = format!("v({key})");
            match w.add_trace(&src) {
                Ok(()) => sent.push(src),
                Err(_) => failed += 1, // ground and nets the simulator did not save
            }
        }
        self.session.say(format!(
            "sent to viewer: {} ({failed} not in the data)",
            sent.join(" ")
        ));
        assert!(sent.len() + failed <= self.highlight.len());
    }
    /// Cross-probe (D23): a click on the canvas with the waves dock open adds `v(<net>)` for
    /// a net (its emitted name through the name map, lowercase for ngspice) and `i(<dev>)` for
    /// a pin of a branch device (`v`, `e`, `h`, `l`: ngspice keeps their currents).
    pub(super) fn cross_probe(&mut self, h: DocHandle, hit: Option<u32>, at: Pt) {
        let Some(o) = hit else { return };
        let key = self.scene.objs.key[o as usize];
        let tol = (PIN_PX / self.camera().scale) as i64;
        let found = {
            let (Some(cell), Some(doc)) = (self.cell.as_ref(), self.session.project.doc(h)) else {
                return;
            };
            checks::component_of(doc, cell, key, at, tol).map(|(c, pin)| {
                let inst = pin.map(|(row, k)| {
                    let name = doc.strings().get(doc.instances().name[row as usize]);
                    let f = cell.ifaces.get(cell.inst_iface[row as usize] as usize);
                    // `k` counts the ordinary (not inherited) pins, as `pins_of` does.
                    let pin = f.and_then(|f| {
                        (0..f.pin_name.len())
                            .filter(|&p| f.pin_inherit[p] == Name::NONE)
                            .nth(k as usize)
                            .map(|p| cell.names.get(f.pin_name[p]).to_owned())
                    });
                    (name.to_owned(), pin.unwrap_or_default())
                });
                (checks::net_name(cell, c).to_owned(), inst)
            })
        };
        let Some((net, inst)) = found else { return };
        let volts = format!("v({})", self.net_key(&net));
        let src = match inst {
            Some((name, pin)) => {
                let path = format!("{}/{name}", self.level_prefix());
                let Some(emitted) = self.hier_map().and_then(|m| m.inst_name(&path)) else {
                    self.session.say(format!("{path}: not in the netlist"));
                    return;
                };
                crate::waves::current_probe(&emitted, &pin)
            }
            None => volts.clone(),
        };
        let Some(w) = self.waves.as_mut() else { return };
        // A device current the raw file lacks (a run without OP annotation saves no
        // `savecurrents`): probe the pin's net instead.
        let same = volts == src;
        let added = match w.add_trace(&src) {
            Ok(()) => Ok(src),
            Err(e) if !same => w.add_trace(&volts).map(|()| volts).map_err(|_| e),
            Err(e) => Err(e),
        };
        self.session.say(match added {
            Ok(src) => format!("cross-probe: {src}"),
            Err(e) => format!("cross-probe: {e}"),
        });
    }

    /// The dock's divider: a strip across the window at `split` the pointer drags (on top of
    /// both views), moving `dock_split` within [`DOCK_SPLIT_RANGE`].
    pub(super) fn dock_divider(&mut self, ui: &mut egui::Ui, full: egui::Rect, split: f32) {
        let strip = egui::Rect::from_x_y_ranges(
            full.x_range(),
            split - DOCK_GRAB_PX..=split + DOCK_GRAB_PX,
        );
        let r = ui.interact(
            strip,
            egui::Id::unique("schemify.dock.divider"),
            egui::Sense::drag(),
        );
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if let (true, Some(p)) = (r.dragged(), r.interact_pointer_pos()) {
            let (lo, hi) = DOCK_SPLIT_RANGE;
            self.dock_split = ((p.y - full.top()) / full.height().max(1.0)).clamp(lo, hi);
        }
        debug_assert!((DOCK_SPLIT_RANGE.0..=DOCK_SPLIT_RANGE.1).contains(&self.dock_split));
    }

    /// Tracks modifiers and, for keys that use the pointer as first point (xschem infix
    /// interface), the pointer position before the first key of the frame.
    pub(super) fn track_input(&mut self, ctx: &egui::Context) {
        ctx.input(|i| {
            for e in &i.events {
                match e {
                    egui::Event::PointerMoved(p) => self.pointer = (p.x, p.y),
                    egui::Event::Key { .. } | egui::Event::Text(_) => break,
                    _ => {}
                }
            }
            let m = i.modifiers;
            self.mods = (u8::from(m.ctrl) * Mods::CTRL)
                | (u8::from(m.shift) * Mods::SHIFT)
                | (u8::from(m.alt) * Mods::ALT)
                | (u8::from(m.mac_cmd) * Mods::SUPER);
        });
    }
}
