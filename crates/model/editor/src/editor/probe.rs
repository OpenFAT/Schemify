//! The waves dock: opening files and cross-probing into it (D23).
//!
//! Contents:
//! - `pub(super) const DOCK_SPLIT`
//! - `Editor::open_waves`
//! - `Editor::send_to_viewer`
//! - `Editor::cross_probe`

use std::path::PathBuf;

use schemify_doc::DocHandle;
use schemify_elab::Name;

use crate::geometry::Pt;
use crate::waves::WavesView;
use crate::{checks, platform};

use super::Editor;
use super::highlight::PIN_PX;

/// Share of the window height the editor keeps when the waves dock opens.
pub(super) const DOCK_SPLIT: f32 = 0.6;

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
}
