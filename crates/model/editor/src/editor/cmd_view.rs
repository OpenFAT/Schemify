//! View, Options and Layers commands: camera, toggles, settings, panels.
//!
//! Contents:
//! - `Editor::run_view`

use schemify_doc::{Document, EditRequest};

use crate::commands::Cmd;
use crate::scene::UNIT;
use crate::session::View;
use crate::spines;

use super::canvas::USER_LAYERS;
use super::dialog::{Dialog, TextFor};
use super::tool::{Tool, ZOOM_STEP};
use super::{Editor, Panel, has_view};

impl Editor {
    /// View, Options and Layers commands: camera, toggles, settings, panels. Returns whether anything visible changed.
    pub(super) fn run_view(
        &mut self,
        cmd: Cmd,
        arg: &str,
        _node: &str,
        _reqs: &mut Vec<EditRequest>,
    ) -> bool {
        let Some((h, view)) = self.active() else {
            return false;
        };
        let toggle = |b: &mut bool| *b = !*b;
        match cmd {
            Cmd::ChecksToggle | Cmd::ResultsToggle => {
                let want = if cmd == Cmd::ChecksToggle {
                    Panel::Checks
                } else {
                    Panel::Results
                };
                self.panel = if self.panel == want {
                    Panel::None
                } else {
                    want
                };
            }
            Cmd::HalfSnap => self.opts.snap = (self.opts.snap / 2).max(1),
            Cmd::DoubleSnap => self.opts.snap = (self.opts.snap * 2).min(1000 * UNIT),
            Cmd::DrawGrid => self.opts.show_grid = !self.opts.show_grid,
            Cmd::Orthogonal => self.opts.orthogonal = !self.opts.orthogonal,
            Cmd::ToggleTheme => {
                self.opts.light = !self.opts.light;
                self.refresh_palette();
            }
            Cmd::SetSnap | Cmd::SetGrid => match arg.trim().parse::<f64>() {
                Ok(v) if v > 0.0 && v <= 1000.0 => {
                    let m = (v * f64::from(UNIT)).round() as i32;
                    if cmd == Cmd::SetSnap {
                        self.opts.snap = m.max(1);
                    } else {
                        self.opts.grid = m.max(1);
                    }
                }
                _ => self
                    .session
                    .say(format!("{arg:?}: not a positive number of units")),
            },
            Cmd::PanLeft | Cmd::PanRight | Cmd::PanUp | Cmd::PanDown => {
                let v = self.canvas_rect();
                let step = v.width().min(v.height()) / 8.0;
                let (dx, dy) = match cmd {
                    Cmd::PanLeft => (step, 0.0),
                    Cmd::PanRight => (-step, 0.0),
                    Cmd::PanUp => (0.0, step),
                    _ => (0.0, -step),
                };
                let mut cam = self.camera();
                cam.pan(dx, dy);
                self.set_camera(cam);
            }
            Cmd::PanCenter => {
                let v = self.canvas_rect();
                let mut cam = self.camera();
                let (wx, wy) =
                    cam.to_world(v, f64::from(self.pointer.0), f64::from(self.pointer.1));
                cam.cx = wx;
                cam.cy = wy;
                self.set_camera(cam);
            }
            Cmd::ZoomIn | Cmd::ZoomOut => {
                let v = self.canvas_rect();
                let f = if cmd == Cmd::ZoomIn {
                    ZOOM_STEP
                } else {
                    1.0 / ZOOM_STEP
                };
                let mut cam = self.camera();
                cam.zoom_at(v, v.center().x, v.center().y, f);
                self.set_camera(cam);
            }
            Cmd::ZoomFull => self.fit(self.scene.extent),
            Cmd::ZoomSelection => {
                if let Some(b) = self.selection_bbox() {
                    self.fit(b);
                }
            }
            Cmd::ZoomBox => self.tool = Tool::ZoomArmed,
            Cmd::Redraw => {
                self.tool = Tool::Idle;
                self.opts.constrain = 0;
                self.space_pan = false;
                self.pan = None;
                self.set_selection(Vec::new());
                self.set_partial(Vec::new());
            }
            Cmd::DocsToggle => {
                if let Some(t) = self.session.tab_view_mut() {
                    *t.docs = !*t.docs;
                }
            }
            Cmd::DocsSource => {
                if let Some(t) = self.session.tab_view_mut() {
                    *t.docs_source = !*t.docs_source;
                }
            }
            Cmd::ViewSchematic | Cmd::ViewSymbol => {
                let want = if cmd == Cmd::ViewSymbol {
                    View::Symbol
                } else {
                    View::Schematic
                };
                let kind = self.session.project.doc(h).map(Document::kind);
                if !kind.is_some_and(|k| has_view(k, want)) {
                    self.session.say(match want {
                        View::Schematic => "a primitive has no schematic".to_owned(),
                        View::Symbol => "a testbench has no symbol".to_owned(),
                    });
                    return true;
                }
                if view != want {
                    // Schematic and symbol coordinates are unrelated: refit, drop the selection.
                    let doc = self.session.level().expect("an active level").doc;
                    self.sel_moved |= self.session.show(doc, want, true);
                    self.tool = Tool::Idle;
                }
                debug_assert_eq!(self.active().map(|(_, v)| v), Some(want));
            }
            Cmd::CellToggle => {
                if let Some(t) = self.session.tab_view_mut() {
                    *t.cell = !*t.cell;
                }
            }
            Cmd::AutoTrim => toggle(&mut self.opts.auto_trim),
            Cmd::ExportColor => toggle(&mut self.opts.export_color),
            Cmd::ExportTransparent => toggle(&mut self.opts.export_transparent),
            Cmd::Crosshair => toggle(&mut self.opts.crosshair),
            Cmd::SnapCursor => toggle(&mut self.opts.snap_cursor),
            Cmd::EnableStretch => toggle(&mut self.opts.stretch),
            Cmd::FlatNetlist => toggle(&mut self.opts.flat),
            Cmd::PersistentWire => toggle(&mut self.opts.persistent),
            Cmd::GridAxes => toggle(&mut self.opts.grid_axes),
            Cmd::ToolbarToggle => toggle(&mut self.opts.toolbar),
            Cmd::Fullscreen => toggle(&mut self.opts.fullscreen),
            Cmd::SymbolText => toggle(&mut self.opts.symbol_text),
            Cmd::FillToggle => toggle(&mut self.opts.fill),
            Cmd::VariableWidth => toggle(&mut self.opts.variable_width),
            Cmd::AutoHighlight => toggle(&mut self.opts.auto_highlight),
            Cmd::HighlightInstances => {
                toggle(&mut self.opts.highlight_insts);
            }
            Cmd::IncrementColor => {
                toggle(&mut self.opts.incr_color);
            }
            Cmd::OnlyProbes => toggle(&mut self.opts.only_probes),
            Cmd::AutoHighlightPlotted => {
                toggle(&mut self.opts.highlight_plotted);
                if self.opts.highlight_plotted {
                    self.highlight_plotted();
                }
            }
            Cmd::CrosshairSize
            | Cmd::LineWidth
            | Cmd::DimColors
            | Cmd::SymbolWidth
            | Cmd::GridDialog
            | Cmd::SnapDialog
            | Cmd::ExportPage
            | Cmd::PngScale => self.setting(cmd, arg),
            Cmd::CycleDialect => {
                let i = spines::DIALECTS
                    .iter()
                    .position(|d| d.1 == self.dialect)
                    .unwrap_or(0);
                let next = spines::DIALECTS[(i + 1) % spines::DIALECTS.len()];
                self.dialect = next.1;
                self.session.say(format!("netlist dialect: {}", next.0));
            }
            Cmd::LineWidthDown | Cmd::LineWidthUp => {
                let w = i16::from(self.opts.width_tenths);
                let w = if cmd == Cmd::LineWidthUp {
                    w + 5
                } else {
                    w - 5
                };
                self.opts.width_tenths = w.clamp(1, 100) as u8;
            }
            Cmd::LayerColor => {
                let v = arg.trim().trim_start_matches('#');
                if v.is_empty() {
                    self.open_text(TextFor::LayerColor, "Color of the current layer (rrggbb)");
                } else if let Some(rgb) = u32::from_str_radix(v, 16).ok().filter(|_| v.len() == 6) {
                    let l = self.opts.layer;
                    self.layer_colors.retain(|c| c.0 != l);
                    self.layer_colors.push((l, rgb));
                } else {
                    self.session.say(format!("{arg:?}: not an rrggbb color"));
                }
            }
            Cmd::ResetColors => {
                self.layer_colors.clear();
                self.opts.dim = 0;
                self.opts.hidden = 0;
                self.opts.width_tenths = 10;
            }
            Cmd::LayersDialog => {
                self.tool = Tool::Idle;
                self.dialog = Dialog::Layers;
                self.show_dialog("dialog.layers");
            }
            Cmd::ToggleLayer => match arg.trim().parse::<u8>() {
                Ok(l) if (1..USER_LAYERS).contains(&l) => self.opts.hidden ^= 1 << l,
                _ => self.session.say(format!("{arg:?}: not a layer 1..21")),
            },
            Cmd::SetLayer => {
                // `layers.set_current.<n>` or a Layers-menu node `layers.<n>_<name>`.
                let digits: String = arg.chars().take_while(char::is_ascii_digit).collect();
                match digits.parse::<u8>() {
                    Ok(l) if l < USER_LAYERS => {
                        self.opts.layer = l;
                        self.session.say(format!("current layer {l}"));
                    }
                    _ => self.session.say(format!("{arg:?}: not a layer 0..21")),
                }
            }
            Cmd::AllLayers => self.opts.hidden = 0,
            Cmd::CurrentLayerOnly => {
                let all = (1u32 << USER_LAYERS) - 2; // every user layer but the background
                self.opts.hidden = all & !(1 << self.opts.layer);
            }
            Cmd::ErcWindow => {
                self.panel = if self.panel == Panel::Checks {
                    Panel::None
                } else {
                    Panel::Checks
                };
            }
            Cmd::SymbolsDetails => self.opts.show_symbols = 0,
            Cmd::SymbolsBoxesSub => self.opts.show_symbols = 1,
            Cmd::SymbolsBoxesAll => self.opts.show_symbols = 2,
            Cmd::SymbolsCycle => self.opts.show_symbols = (self.opts.show_symbols + 1) % 3,
            _ => unreachable!("{cmd:?}: not a view command"),
        }
        true
    }
}
