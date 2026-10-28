//! The waves dock: its input split, opening files, and cross-probing into it (D23).

use std::path::PathBuf;

use schemify_doc::DocHandle;
use schemify_ui::{Button, InputEvent, InputEvents, Mods};

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
    /// Splits one input batch between the editor (above the dock) and the waves dock
    /// (below, in its own coordinates): pointer events by position, keys and text to the
    /// side the pointer was last over, `Ctrl+W` always to the dock (it closes it). A left
    /// press within [`DOCK_GRAB_PX`] of the divider drags it (neither side sees the drag).
    /// Returns (editor events, dock events, editor viewport, dock viewport).
    pub(super) fn split_input(
        &mut self,
        events: &InputEvents,
        viewport: (f32, f32),
    ) -> (InputEvents, InputEvents, (f32, f32), (f32, f32)) {
        let split = (viewport.1 * self.dock_split).floor();
        let mut mine = InputEvents {
            events: Vec::new(),
            text: events.text.clone(),
        };
        let mut dock = InputEvents {
            events: Vec::new(),
            text: events.text.clone(),
        };
        let close = schemify_ui::Chord::new(Mods(Mods::CTRL), schemify_ui::Key('w' as u32));
        for &e in &events.events {
            match e {
                InputEvent::PointerDown {
                    y,
                    button: Button::Left,
                    ..
                } if (y - split).abs() <= DOCK_GRAB_PX => {
                    self.dock_drag = true;
                    continue;
                }
                InputEvent::PointerMove { y, .. } if self.dock_drag && viewport.1 > 0.0 => {
                    let (lo, hi) = DOCK_SPLIT_RANGE;
                    self.dock_split = (y / viewport.1).clamp(lo, hi);
                    continue;
                }
                InputEvent::PointerUp {
                    button: Button::Left,
                    ..
                } if self.dock_drag => {
                    self.dock_drag = false;
                    continue;
                }
                _ => {}
            }
            let pointer_y = match e {
                InputEvent::PointerMove { y, .. }
                | InputEvent::PointerDown { y, .. }
                | InputEvent::PointerUp { y, .. }
                | InputEvent::Wheel { y, .. } => Some(y),
                _ => None,
            };
            if let Some(y) = pointer_y {
                self.dock_focus = y >= split;
            }
            let to_dock = match e {
                InputEvent::Key { chord } if chord == close => true,
                InputEvent::Mods { .. } | InputEvent::PointerLeave => {
                    mine.push(e);
                    true
                }
                _ => self.dock_focus,
            };
            if !to_dock {
                mine.push(e);
                continue;
            }
            dock.push(match e {
                InputEvent::PointerMove { x, y } => InputEvent::PointerMove { x, y: y - split },
                InputEvent::PointerDown { x, y, button } => InputEvent::PointerDown {
                    x,
                    y: y - split,
                    button,
                },
                InputEvent::PointerUp { x, y, button } => InputEvent::PointerUp {
                    x,
                    y: y - split,
                    button,
                },
                InputEvent::Wheel { x, y, dx, dy } => InputEvent::Wheel {
                    x,
                    y: y - split,
                    dx,
                    dy,
                },
                other => other,
            });
        }
        // A drag in this batch moves the divider now: both sides lay out at the new split.
        let split = (viewport.1 * self.dock_split).floor();
        assert!(split >= 0.0 && split <= viewport.1);
        (
            mine,
            dock,
            (viewport.0, split),
            (viewport.0, viewport.1 - split),
        )
    }
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
                            .filter(|&p| f.pin_inherit[p] == schemify_elab::Name::NONE)
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
