//! xschem-parity commands beyond the M2 core (docs/research/xschem-feature-inventory.md §1–3):
//! File (save as, merge, recent files, delete, image export), Edit (unselect area, stretch
//! moves), Options/View toggles and the canvas palette they shape, Layers, Properties (edit
//! file, header, ignore), Tools (shapes, align, cuts, join/trim, search, connected wires),
//! Symbol (make symbol/schematic, labels and pins), Highlight (duplicates, mismatches,
//! compare), Simulation and Help text tabs, the context menu and the crash-recovery prompt.
//!
//! Kept beside the frame spine as more `impl Editor` steps: each command reads the tables it
//! names and writes editor state or edit requests only (`doc` changes go through
//! `apply_edits`, one transaction per command, so one undo). Pure transforms live in
//! `edit`, `wiring`, `search`, `clip` and `export`.

use std::path::{Path, PathBuf};

use schemify_doc::{DocHandle, EditRequest, Op, RecordBuf, TableId, Target};
use schemify_format::{FileKind, Orient, RecordKind};
use schemify_render::canvas::{self, CanvasPrims, Palette};
use schemify_render::vello::kurbo::Rect as KRect;

use super::{ChooseFor, Dialog, Editor, Field, Form, Placement, Props, PropsFor, TextFor, Tool, q};
use crate::checks;
use crate::commands::Cmd;
use crate::edit::{self, Ids};
use crate::export;
use crate::geometry::{Pt, Xform, snap};
use crate::platform;
use crate::scene::{self, LAYER_SEL, ObjKey, UNIT};
use crate::search;
use crate::session::{TextView, View};
use crate::spines;
use crate::wiring;

/// Layers the rotating highlight colors use (palette layers with distinct hues).
const HILITE_LAYERS: [u8; 8] = [8, 9, 10, 12, 13, 15, 17, 21];
/// Canvas layers a user can draw on and hide (xschem's 22).
const USER_LAYERS: u8 = 22;
/// Names of the special layers (xschem Layers menu).
const LAYER_NAMES: [&str; 6] = ["BG", "WIRE", "GRID", "TEXT", "", "PIN"];
/// Pin pitch of generated symbols and port columns (xschem units).
const PITCH: i32 = 20 * UNIT;
/// Rows of hierarchy the changelog lists at most.
const CHANGELOG_ROWS: usize = 1 << 12;
/// Largest PNG export side in pixels (the CPU raster's target is `u16`; 16k keeps one
/// image under 1 GiB of RGBA).
const PNG_MAX_PX: u16 = 16_384;

/// The comparison of the active document with another cell (Highlight → compare).
#[derive(Clone, Debug)]
pub(crate) struct Compare {
    /// The other cell's index row.
    row: usize,
    /// Shown.
    on: bool,
    /// (document, edit epoch, other's epoch) the drawn diff is current for.
    key: Option<(DocHandle, u32, u32)>,
}

/// One shape's points as a record: rank-2 symbol graphics (`schematic == false`) or a rank-3
/// `shape` with id `id`. `None` when the points do not make the shape (a degenerate rect, a
/// polygon under two points, an arc through collinear points).
pub(super) fn shape_record(
    form: Form,
    pts: &[Pt],
    closed: bool,
    layer: u8,
    id: Option<&str>,
) -> Option<RecordBuf> {
    let c = |v: i32| {
        let mut s = String::new();
        schemify_format::format_coord(v, &mut s);
        s
    };
    let (word, values): (&str, Vec<i32>) = match form {
        Form::Line | Form::Rect => {
            let (a, b) = (*pts.first()?, *pts.get(1)?);
            if a == b || (form == Form::Rect && (a.0 == b.0 || a.1 == b.1)) {
                return None;
            }
            let w = if form == Form::Line { "line" } else { "rect" };
            (w, vec![a.0, a.1, b.0, b.1])
        }
        Form::Circle => {
            let (m, p) = (*pts.first()?, *pts.get(1)?);
            let r = dist(m, p);
            if r == 0 {
                return None;
            }
            ("circle", vec![m.0, m.1, r])
        }
        Form::Arc => {
            let (cx, cy, r, start, sweep) = arc_3(*pts.first()?, *pts.get(1)?, *pts.get(2)?)?;
            ("arc", vec![cx, cy, r, start, sweep])
        }
        Form::Poly => {
            if pts.len() < 2 || (closed && pts.len() < 3) {
                return None;
            }
            ("poly", pts.iter().flat_map(|p| [p.0, p.1]).collect())
        }
    };
    let mut rec = match id {
        Some(id) => RecordBuf::new(RecordKind::Shape).field(id).field(word),
        None => RecordBuf::new(match word {
            "line" => RecordKind::Line,
            "rect" => RecordKind::Rect,
            "circle" => RecordKind::Circle,
            "arc" => RecordKind::Arc,
            _ => RecordKind::Poly,
        }),
    };
    // Rank-2 arc angles are file numbers, everything else (and every rank-3 value) is a
    // coordinate: both print as the shortest decimal of the milli value.
    for v in &values {
        rec = rec.field(&c(*v));
    }
    if layer != 4 {
        rec = rec.attr("layer", &[&layer.to_string()]);
    }
    if form == Form::Poly && closed {
        rec = rec.attr("closed", &["true"]);
    }
    Some(rec)
}

fn dist(a: Pt, b: Pt) -> i32 {
    let (dx, dy) = (
        f64::from(b.0) - f64::from(a.0),
        f64::from(b.1) - f64::from(a.1),
    );
    (dx.hypot(dy).round() as i64).clamp(0, i64::from(i32::MAX)) as i32
}

/// The arc from `a` to `b` through `c` (xschem's 3-point arc): center, radius (milli-units)
/// and start/sweep in milli-degrees, clockwise on screen, sweep in `(0, 360000)`. `None` for
/// collinear or repeated points.
pub(super) fn arc_3(a: Pt, b: Pt, c: Pt) -> Option<(i32, i32, i32, i32, i32)> {
    let (ax, ay) = (f64::from(a.0), f64::from(a.1));
    let (bx, by) = (f64::from(b.0), f64::from(b.1));
    let (cx, cy) = (f64::from(c.0), f64::from(c.1));
    let d = 2.0 * (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by));
    if d.abs() < 1e-6 {
        return None;
    }
    let a2 = ax * ax + ay * ay;
    let b2 = bx * bx + by * by;
    let c2 = cx * cx + cy * cy;
    let ux = (a2 * (by - cy) + b2 * (cy - ay) + c2 * (ay - by)) / d;
    let uy = (a2 * (cx - bx) + b2 * (ax - cx) + c2 * (bx - ax)) / d;
    let r = (ax - ux).hypot(ay - uy);
    let ang = |x: f64, y: f64| ((y - uy).atan2(x - ux).to_degrees() * 1000.0).round();
    let (ta, tb, tc) = (ang(ax, ay), ang(bx, by), ang(cx, cy));
    let cw = |from: f64, to: f64| (to - from).rem_euclid(360_000.0);
    let (start, sweep) = if cw(ta, tc) <= cw(ta, tb) {
        (ta, cw(ta, tb)) // clockwise from a passes c before b
    } else {
        (tb, cw(tb, ta)) // the arc from b clockwise to a holds c
    };
    let fits = |v: f64| v.abs() < f64::from(i32::MAX);
    if !(fits(ux) && fits(uy) && fits(r)) || sweep <= 0.0 {
        return None;
    }
    Some((
        ux.round() as i32,
        uy.round() as i32,
        r.round() as i32,
        start.rem_euclid(360_000.0) as i32,
        sweep as i32,
    ))
}

/// Whether a parity command makes sense on a read-only text tab: it shows another text, a
/// dialog, or changes a view option (it does not edit the level behind the tab).
pub(super) fn text_tab_ok(cmd: Cmd) -> bool {
    matches!(
        cmd,
        Cmd::About
            | Cmd::Help
            | Cmd::Changelog
            | Cmd::ListProcesses
            | Cmd::PackageStatus
            | Cmd::JobData
            | Cmd::JobErrors
            | Cmd::ListNets
            | Cmd::ListNetsBuses
            | Cmd::EditNetlist
            | Cmd::WavesClear
            | Cmd::WavesLoad
            | Cmd::CycleDialect
            | Cmd::FlatNetlist
            | Cmd::Fullscreen
            | Cmd::ToolbarToggle
            | Cmd::OpenRecent
            | Cmd::OpenMostRecent
            | Cmd::NewWindowTab
            | Cmd::Recover
            | Cmd::DiscardRecovery
    )
}

/// A parsed number argument in `[lo, hi]`.
fn number(arg: &str, lo: f64, hi: f64) -> Option<f64> {
    arg.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= lo && *v <= hi)
}

impl Editor {
    /// Runs a command added for xschem parity (the rest are in `Editor::run`). Returns whether
    /// anything visible changed.
    #[allow(clippy::too_many_lines)]
    pub(super) fn parity(
        &mut self,
        cmd: Cmd,
        arg: &str,
        node: &str,
        reqs: &mut Vec<EditRequest>,
    ) -> bool {
        let Some((h, view)) = self.active() else {
            return false;
        };
        let ptr = self.pointer_world_snapped();
        let _ = node;
        let toggle = |b: &mut bool| *b = !*b;
        match cmd {
            // ---------------------------------------------------------------- File
            Cmd::SaveAs | Cmd::SaveAsSymbol => {
                let symbol = cmd == Cmd::SaveAsSymbol;
                if arg.trim().is_empty() {
                    let current = self
                        .session
                        .path(h)
                        .and_then(|p| crate::libs::rel_path(&self.session.root, p))
                        .unwrap_or_default();
                    let title = if symbol { "Save symbol as" } else { "Save as" };
                    self.open_text(TextFor::SaveAs(symbol), title);
                    if let Dialog::Text(_, _, typed) = &mut self.dialog {
                        *typed = current;
                    }
                } else {
                    self.save_as(h, arg.trim(), symbol);
                }
            }
            Cmd::Merge => self.open_chooser(ChooseFor::Merge),
            Cmd::OpenMostRecent => self.open_recent(0),
            Cmd::OpenRecent => match arg.trim().parse::<usize>() {
                Ok(i) => self.open_recent(i),
                Err(_) => self.session.status = format!("{arg:?}: not a recent-file index"),
            },
            Cmd::DeleteFiles => {
                self.pending_delete = None;
                self.open_chooser(ChooseFor::Delete);
            }
            Cmd::ExportPng
            | Cmd::ExportPngFull
            | Cmd::ExportSvg
            | Cmd::ExportPdf
            | Cmd::ExportPdfFull
            | Cmd::ExportPdfHier
            | Cmd::ExportEps => self.export(h, cmd, arg),
            // ---------------------------------------------------------------- Edit / tools
            Cmd::UnselectArea => self.tool = Tool::UnselectArmed,
            Cmd::InsertPolygon | Cmd::InsertArc | Cmd::InsertCircle => {
                let form = match cmd {
                    Cmd::InsertPolygon => Form::Poly,
                    Cmd::InsertArc => Form::Arc,
                    _ => Form::Circle,
                };
                self.tool = Tool::Shape {
                    form,
                    pts: vec![ptr],
                    to: ptr,
                };
            }
            Cmd::ClosePolygon => {
                if let Tool::Shape {
                    form: Form::Poly,
                    pts,
                    ..
                } = std::mem::take(&mut self.tool)
                {
                    self.finish_shape(reqs, h, view, Form::Poly, &pts, true);
                }
            }
            Cmd::AlignGrid => {
                let snap_m = self.opts.snap;
                let ops = self.with_doc(h, |d, sel| wiring::align(d, sel, snap_m));
                self.commit(reqs, h, "align to grid", ops);
            }
            Cmd::BreakAtMouse | Cmd::BreakAtMouseAlign => self.cut_wire(reqs, h, cmd),
            Cmd::BreakAtPins | Cmd::RemoveThrough => {
                let sel = self.selection().to_vec();
                let ops = {
                    let p = &self.session.project;
                    let mut ids = Ids::new(&mut self.rng);
                    if cmd == Cmd::RemoveThrough {
                        wiring::remove_through(p, h, &sel, &mut ids)
                    } else {
                        let pts: Vec<Pt> = p.doc(h).map_or_else(Vec::new, |d| {
                            sel.iter()
                                .filter(|k| k.table == TableId::Instances)
                                .filter_map(|&k| edit::loc_of(d, k))
                                .flat_map(|l| wiring::inst_pins(p, h, d, l.row as usize))
                                .collect()
                        });
                        p.doc(h)
                            .map_or_else(Vec::new, |d| wiring::break_at(d, &pts, &mut ids).0)
                    }
                };
                self.commit(reqs, h, cmd.name(), ops);
            }
            Cmd::JoinTrim => {
                let ops = {
                    let mut ids = Ids::new(&mut self.rng);
                    wiring::join_trim(&self.session.project, h, &mut ids)
                };
                if ops.is_empty() {
                    self.session.status = "wires already joined and trimmed".into();
                }
                self.commit(reqs, h, "join/trim wires", ops);
            }
            Cmd::Search => self.open_search(),
            Cmd::SelectConnected | Cmd::SelectConnectedStop => {
                let mut seeds = self.selection().to_vec();
                if seeds.is_empty()
                    && let Some(o) = self.hit_at(self.pointer)
                {
                    seeds.push(self.scene.objs.key[o as usize]);
                }
                let stop = cmd == Cmd::SelectConnectedStop;
                let keys = wiring::connected(&self.session.project, h, &seeds, stop);
                self.session.status = format!("{} connected object(s)", keys.len());
                self.set_selection(keys);
            }
            // ---------------------------------------------------------------- Options / view
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
                self.decor_dirty = true;
            }
            Cmd::IncrementColor => {
                toggle(&mut self.opts.incr_color);
                self.decor_dirty = true;
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
                self.session.status = format!("netlist dialect: {}", next.0);
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
                    self.session.status = format!("{arg:?}: not an rrggbb color");
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
                _ => self.session.status = format!("{arg:?}: not a layer 1..21"),
            },
            Cmd::SetLayer => {
                // `layers.set_current.<n>` or a Layers-menu node `layers.<n>_<name>`.
                let digits: String = arg.chars().take_while(char::is_ascii_digit).collect();
                match digits.parse::<u8>() {
                    Ok(l) if l < USER_LAYERS => {
                        self.opts.layer = l;
                        self.session.status = format!("current layer {l}");
                    }
                    _ => self.session.status = format!("{arg:?}: not a layer 0..21"),
                }
            }
            Cmd::AllLayers => self.opts.hidden = 0,
            Cmd::CurrentLayerOnly => {
                let all = (1u32 << USER_LAYERS) - 2; // every user layer but the background
                self.opts.hidden = all & !(1 << self.opts.layer);
            }
            Cmd::ErcWindow => {
                self.panel = if self.panel == super::Panel::Checks {
                    super::Panel::None
                } else {
                    super::Panel::Checks
                };
            }
            // ---------------------------------------------------------------- Properties
            Cmd::EditFile => {
                let text = self
                    .session
                    .project
                    .doc(h)
                    .map(schemify_doc::Document::canonical)
                    .unwrap_or_default();
                self.tool = Tool::Idle;
                self.dialog = Dialog::File(text);
                self.show_dialog("dialog.edit_file");
                self.focus_next = Some("dialog.edit_file.text".into());
            }
            Cmd::EditFileOk => {
                let Dialog::File(typed) = std::mem::take(&mut self.dialog) else {
                    return false;
                };
                self.close_dialog();
                let text = if arg.is_empty() {
                    typed
                } else {
                    arg.to_owned()
                };
                self.apply_file_text(reqs, h, &text);
            }
            Cmd::EditHeader => self.open_header(h),
            Cmd::ToggleIgnore => {
                let ops = self.with_doc(h, |d, sel| {
                    edit::selected_records(d, sel)
                        .into_iter()
                        .filter(|(_, _, r)| r.kind == RecordKind::Inst)
                        .map(|(_, target, r)| match r.find_attr("ignore") {
                            Some(_) => Op::RemoveAttr {
                                target,
                                key: "ignore".into(),
                                first: None,
                            },
                            None => Op::SetAttr {
                                target,
                                key: "ignore".into(),
                                values: vec!["true".into()],
                            },
                        })
                        .collect::<Vec<_>>()
                });
                if ops.is_empty() {
                    self.session.status = "select instances to toggle `ignore`".into();
                }
                self.commit(reqs, h, "toggle ignore", ops);
            }
            // ---------------------------------------------------------------- Symbol
            Cmd::SymbolsDetails => self.opts.show_symbols = 0,
            Cmd::SymbolsBoxesSub => self.opts.show_symbols = 1,
            Cmd::SymbolsBoxesAll => self.opts.show_symbols = 2,
            Cmd::SymbolsCycle => self.opts.show_symbols = (self.opts.show_symbols + 1) % 3,
            Cmd::MakeSymbol => {
                let ops = self.make_symbol_ops(h);
                self.commit(reqs, h, "make symbol", ops);
            }
            Cmd::MakeSchematic => {
                let ops = self.make_schematic_ops(h, ptr);
                if ops.is_empty() {
                    self.session.status = "every pin already has a port".into();
                }
                self.commit(reqs, h, "make schematic", ops);
            }
            Cmd::MakeSubcircuit => self.make_subcircuit(reqs, h),
            Cmd::AttachLabels => {
                let ops = self.attach_labels_ops(h);
                self.commit(reqs, h, "attach labels", ops);
            }
            Cmd::PinsFromPorts => {
                let ops = self.with_doc(h, pins_from_ports);
                if ops.is_empty() {
                    self.session.status = "select schematic ports without a pin".into();
                }
                self.commit(reqs, h, "pins from ports", ops);
            }
            Cmd::ListNets | Cmd::ListNetsBuses => {
                let mut body = String::from("Highlighted nets\n\n");
                for k in &self.highlight {
                    if cmd == Cmd::ListNetsBuses {
                        for bit in expand_bus(k) {
                            body.push_str(&bit);
                            body.push('\n');
                        }
                    } else {
                        body.push_str(k);
                        body.push('\n');
                    }
                }
                self.text_tab("highlighted nets", "nets", body);
            }
            Cmd::LabelsFromNets | Cmd::LabelsFromNetsI | Cmd::PortsFromNets => {
                let names = self.highlighted_local_names();
                if names.is_empty() {
                    self.session.status = "no highlighted nets at this level (k)".into();
                    return true;
                }
                let kind = if cmd == Cmd::PortsFromNets {
                    RecordKind::Port
                } else {
                    RecordKind::Label
                };
                let prefix = if cmd == Cmd::LabelsFromNetsI { "i" } else { "" };
                let recs: Vec<RecordBuf> = names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| {
                        edit::placed(
                            kind,
                            (0, i as i32 * PITCH),
                            Orient::R0,
                            &format!("{prefix}{n}"),
                            edit::AUTO_ID,
                        )
                    })
                    .collect();
                self.tool = Tool::Place {
                    what: Placement::Paste(recs),
                    at: ptr,
                    lin: Xform::IDENTITY,
                };
            }
            // ---------------------------------------------------------------- Highlight
            Cmd::CompareWith => self.open_chooser(ChooseFor::Compare),
            Cmd::CompareToggle => match self.compare.as_mut() {
                Some(c) => {
                    c.on = !c.on;
                    c.key = None;
                    self.decor_dirty = true;
                }
                None => self.open_chooser(ChooseFor::Compare),
            },
            Cmd::CompareSwap => self.compare_swap(h),
            Cmd::DupNames => {
                let keys = self.with_doc(h, |d, _| duplicate_names(d).0);
                self.session.status = format!("{} instance(s) with a duplicate name", keys.len());
                self.set_selection(keys);
            }
            Cmd::RenameDups => {
                let ops = self.with_doc(h, |d, _| duplicate_names(d).1);
                if ops.is_empty() {
                    self.session.status = "no duplicate instance names".into();
                }
                self.commit(reqs, h, "rename duplicates", ops);
            }
            Cmd::PinMismatch => self.pin_mismatch(h),
            Cmd::SelectHighlighted => {
                let keys = self.hl.keys.clone();
                self.set_selection(keys);
            }
            Cmd::SelectOverlapped => {
                let keys = self.with_doc(h, |d, _| overlapped(d));
                self.session.status = format!("{} overlapped instance(s)", keys.len());
                self.set_selection(keys);
            }
            // ---------------------------------------------------------------- Simulation
            Cmd::Changelog => self.changelog(h),
            Cmd::EditNetlist => {
                let show = self.show_netlist;
                self.show_netlist = true;
                self.netlist(true);
                self.show_netlist = show;
            }
            Cmd::AddGraph => {
                let Some(doc) = self.session.project.doc(h) else {
                    return false;
                };
                let row = crate::docs::insert_row(doc, self.page.sel);
                let rec = RecordBuf::new(RecordKind::Prose)
                    .field(schemify_doc::NEW_ID)
                    .verbatim("```plot")
                    .verbatim("v(out)")
                    .verbatim("```");
                self.commit_docs(
                    reqs,
                    h,
                    "add graph",
                    vec![Op::Insert { rec, at: Some(row) }],
                );
                let tab = self.session.active;
                if let Some(t) = self.session.tabs.get_mut(tab) {
                    t.docs = true;
                }
            }
            Cmd::ListProcesses => self.list_processes(),
            Cmd::PackageStatus => self.package_status(),
            Cmd::JobData | Cmd::JobErrors => self.job_log(cmd == Cmd::JobErrors),
            Cmd::WavesClear => {
                self.waves = None;
                self.shell.invalidate();
            }
            Cmd::WavesLoad => {
                self.open_chooser(ChooseFor::Waves);
                if let Dialog::Choose(c) = &mut self.dialog {
                    c.note = arg.trim().to_owned();
                }
            }
            // ---------------------------------------------------------------- Help
            Cmd::About => self.text_tab(
                "About Schemify",
                "about",
                format!(
                    "Schemify {}\n\nA schematic editor: xschem's keys and menus (D25), \
                     line-record design files that diff and merge (D9, D34), one expression \
                     language (D12), plugins in WASM (D3).\n\nLicensed MIT OR Apache-2.0.\n",
                    env!("CARGO_PKG_VERSION")
                ),
            ),
            Cmd::Help => self.text_tab("Help", "help", help_text()),
            Cmd::ContextMenu => self.context_menu(self.pointer),
            Cmd::Recover | Cmd::DiscardRecovery => {
                let Dialog::Recover(d, _) = std::mem::take(&mut self.dialog) else {
                    return false;
                };
                self.close_dialog();
                self.session.decide_recovery(d, cmd == Cmd::Recover);
                self.scene_dirty = true;
                self.invalidate_cells();
            }
            _ => {
                self.session.status = format!("{}: not available yet", cmd.name());
            }
        }
        true
    }

    // ------------------------------------------------------------------ files

    /// Remembers `h`'s file in the recent-files list (and persists it).
    pub(super) fn remember(&mut self, h: DocHandle) {
        let Some(p) = self.session.path(h) else {
            return;
        };
        if p.starts_with(crate::libs::BUILTIN_DIR) {
            return;
        }
        crate::recent::push(&mut self.recent, crate::libs::absolute(p));
        if let Some(dir) = &self.state_dir
            && let Err(e) = crate::recent::save(dir, &self.recent)
        {
            self.session.status = format!("recent files: {e}");
        }
    }

    fn open_recent(&mut self, i: usize) {
        let Some(path) = self.recent.get(i).cloned() else {
            self.session.status = "no recent file".into();
            return;
        };
        if !platform::exists(&path) {
            self.recent.remove(i);
            self.session.status = format!("{}: no longer exists", path.display());
            return;
        }
        match self.session.open_path(&path) {
            Ok(d) => {
                let view = super::default_view(self.session.project.doc(d));
                self.session.open_tab(d, view);
                self.remember(d);
                self.tool = Tool::Idle;
                self.scene_dirty = true;
            }
            Err(e) => self.session.status = e,
        }
    }

    /// File → Save As: writes the active document's canonical text to `rel` (project-relative
    /// or absolute; the kind's extension is added when missing), indexes it, and edits the
    /// new file in place of the old one. Unsaved edits of the old file move to the new one:
    /// the old document is closed and its journal dropped, so it is not offered for recovery.
    pub(super) fn save_as(&mut self, h: DocHandle, rel: &str, symbol: bool) {
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let ext = doc.kind().word();
        let mut path = PathBuf::from(rel);
        if path.extension().is_none() {
            path.set_extension(ext);
        }
        if path.extension().and_then(|e| e.to_str()) != Some(ext) {
            self.session.status = format!("{}: a {ext} keeps its extension", path.display());
            return;
        }
        let path = if path.is_absolute() {
            path
        } else {
            self.session.root.join(path)
        };
        if self.session.path(h) == Some(path.as_path()) {
            self.session.status = "same file: use Save".into();
            return;
        }
        if platform::exists(&path) {
            self.session.status = format!("{}: exists, choose another name", path.display());
            return;
        }
        let text = doc.canonical();
        if let Err(e) = schemify_format::parse(text.as_bytes()) {
            let d = e.diagnostics.first();
            self.session.status = format!(
                "save as refused: {}",
                d.map_or_else(String::new, |d| format!("{:?} {}", d.code, d.detail))
            );
            return;
        }
        let dirty = self.session.project.is_dirty(h);
        if let Err(e) = platform::write_atomic(&path, text.as_bytes()) {
            self.session.status = e;
            return;
        }
        self.session.reindex(&path);
        self.symbols = scene::Symbols::default();
        self.symbols_key = None;
        self.invalidate_cells();
        let new = match self.session.open_path(&path) {
            Ok(d) => d,
            Err(e) => {
                self.session.status = e;
                return;
            }
        };
        let view = if symbol {
            View::Symbol
        } else {
            View::Schematic
        };
        let shared = self
            .session
            .tabs
            .iter()
            .enumerate()
            .flat_map(|(t, tab)| {
                tab.stack
                    .iter()
                    .enumerate()
                    .map(move |(l, lv)| (t, l, lv.doc))
            })
            .filter(|&(t, l, d)| {
                d == h && !(t == self.session.active && l + 1 == self.session.tabs[t].stack.len())
            })
            .count();
        let at_top = self.session.tabs[self.session.active].stack.len() == 1;
        if at_top {
            if let Some(l) = self.session.level_mut() {
                l.doc = new;
                l.view = view;
                l.sel.clear();
            }
        } else {
            self.session.open_tab(new, view);
        }
        if dirty && shared == 0 && at_top {
            self.session.close_discarding(h);
        }
        self.remember(new);
        let _ = self.host.push(
            None,
            &schemify_plugin::abi::Event::DocOpened {
                doc: crate::plugins::doc_id(new),
            },
        );
        self.tool = Tool::Idle;
        self.scene_dirty = true;
        self.session.status = format!("saved as {}", path.display());
    }

    /// Inserts the schematic of indexed cell `row` attached to the pointer (File → Merge):
    /// its rank-3 records relative to their snapped anchor centroid, like a paste.
    pub(super) fn merge_cell(&mut self, row: usize) {
        let d = match self.session.open_cell(row) {
            Ok(d) => d,
            Err(e) => {
                self.session.status = e;
                return;
            }
        };
        let Some(doc) = self.session.project.doc(d) else {
            return;
        };
        let keys = edit::schematic_keys(doc);
        let recs: Vec<RecordBuf> = edit::selected_records(doc, &keys)
            .into_iter()
            .map(|(_, _, r)| r)
            .collect();
        if recs.is_empty() {
            self.session.status = "that cell has no schematic objects".into();
            return;
        }
        let anchors: Vec<Pt> = recs.iter().filter_map(crate::geometry::anchor).collect();
        let n = anchors.len().max(1) as f64;
        let c = anchors.iter().fold((0.0, 0.0), |a, p| {
            (a.0 + f64::from(p.0) / n, a.1 + f64::from(p.1) / n)
        });
        let base = (snap(c.0, self.opts.snap), snap(c.1, self.opts.snap));
        let back = Xform::translate(-base.0, -base.1);
        let recs: Vec<RecordBuf> = recs
            .into_iter()
            .filter_map(|mut r| crate::geometry::transform_record(&mut r, back).map(|()| r))
            .collect();
        self.session.status = format!("merge: {} object(s), click to place", recs.len());
        self.tool = Tool::Place {
            what: Placement::Paste(recs),
            at: self.pointer_world_snapped(),
            lin: Xform::IDENTITY,
        };
    }

    /// File → Delete files: the first pick of a cell file arms its deletion and reopens the
    /// chooser; picking the same file again deletes it (refused while it is open).
    pub(super) fn delete_cell(&mut self, row: usize) {
        let p = &self.session.project;
        let cells = p.cells();
        let lib = cells.lib[row];
        let Some(dir) = self.session.lib_dir(lib) else {
            self.session.status = "the bundled core library is read-only".into();
            return;
        };
        let path = dir.join(crate::session::cell_relpath(p, row));
        let open = self.session.handle_of_path(&path).is_some();
        if open {
            self.session.status = format!("{}: open in the editor, close it first", path.display());
            return;
        }
        if self.pending_delete.as_ref() != Some(&path) {
            self.session.status = format!("delete {}? pick it again to confirm", path.display());
            self.pending_delete = Some(path);
            self.open_chooser(ChooseFor::Delete);
            return;
        }
        self.pending_delete = None;
        match platform::remove(&path) {
            Ok(()) => {
                self.session.drop_journal(&path);
                self.session.reindex(&path);
                self.symbols = scene::Symbols::default();
                self.symbols_key = None;
                self.recent.retain(|r| *r != crate::libs::absolute(&path));
                self.session.status = format!("deleted {}", path.display());
            }
            Err(e) => self.session.status = e,
        }
    }

    // ------------------------------------------------------------------ export

    /// The palette exports draw with: the view palette without dimming or hidden layers
    /// beyond the user's.
    fn export_look(&self) -> export::Look {
        export::Look {
            palette: self.view_palette(),
            color: self.opts.export_color,
            transparent: self.opts.export_transparent,
        }
    }

    /// Where an export goes: `arg` when given, else `.schemify/export/<cell>.<ext>`.
    fn export_path(&self, h: DocHandle, arg: &str, ext: &str) -> PathBuf {
        if !arg.trim().is_empty() {
            let p = PathBuf::from(arg.trim());
            return if p.is_absolute() {
                p
            } else {
                self.session.root.join(p)
            };
        }
        let stem = self.session.path(h).and_then(Path::file_stem).map_or_else(
            || "untitled".to_owned(),
            |s| s.to_string_lossy().into_owned(),
        );
        self.session
            .root
            .join(".schemify")
            .join("export")
            .join(format!("{stem}.{ext}"))
    }

    /// File → Image export (PNG of the visible canvas or of the whole drawing, at the PNG
    /// scale; SVG, PDF and EPS of the drawing, the selection or the visible area, PDF and EPS
    /// fitted to the export page; a PDF page per schematic of the hierarchy).
    fn export(&mut self, h: DocHandle, cmd: Cmd, arg: &str) {
        let look = self.export_look();
        let page = self.opts.export_page;
        let pad = 10 * UNIT;
        let whole = export::bounds(&self.scene.prims).map(|b| export::padded(b, pad));
        let selected = (!self.selection().is_empty()).then(|| {
            let keep = self.selected.clone();
            export::subset(&self.scene, &keep)
        });
        let (ext, bytes): (&str, Result<Vec<u8>, String>) = match cmd {
            Cmd::ExportPng => ("png", self.export_png()),
            Cmd::ExportPngFull => ("png", self.export_png_full()),
            Cmd::ExportSvg => (
                "svg",
                whole
                    .map(|b| export::svg(&self.scene.prims, b, &look).into_bytes())
                    .ok_or_else(|| "nothing to export".into()),
            ),
            Cmd::ExportEps => {
                let prims = selected.as_ref().unwrap_or(&self.scene.prims);
                (
                    "eps",
                    export::bounds(prims)
                        .map(|b| {
                            export::eps(prims, export::padded(b, pad), &look, page).into_bytes()
                        })
                        .ok_or_else(|| "nothing to export".into()),
                )
            }
            Cmd::ExportPdf => {
                let (prims, b) = match &selected {
                    Some(p) => (p, export::bounds(p).map(|b| export::padded(b, pad))),
                    None => (&self.scene.prims, Some(self.view_world_rect())),
                };
                (
                    "pdf",
                    b.map(|b| export::pdf(&[(prims, b)], &look, page))
                        .ok_or_else(|| "nothing to export".into()),
                )
            }
            Cmd::ExportPdfFull => (
                "pdf",
                whole
                    .map(|b| export::pdf(&[(&self.scene.prims, b)], &look, page))
                    .ok_or_else(|| "nothing to export".into()),
            ),
            _ => ("pdf", self.export_hierarchy(h, &look)),
        };
        let path = self.export_path(h, arg, ext);
        match bytes.and_then(|b| platform::write_atomic(&path, &b)) {
            Ok(()) => self.session.status = format!("exported {}", path.display()),
            Err(e) => self.session.status = format!("export: {e}"),
        }
    }

    /// The visible canvas area in world milli-units.
    fn view_world_rect(&self) -> [i32; 4] {
        let v = self.canvas_rect();
        let cam = self.camera();
        let (x0, y0) = cam.to_world(v, v.x0, v.y0);
        let (x1, y1) = cam.to_world(v, v.x1, v.y1);
        let r = |f: f64| f.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
        [r(x0.min(x1)), r(y0.min(y1)), r(x0.max(x1)), r(y0.max(y1))]
    }

    /// The visible canvas rasterized on the CPU (`render::CpuPainter`) as PNG bytes at the
    /// PNG scale (`EditOpts::png_scale`): the same grid, primitives and palette the window
    /// draws, without selection marks or overlay. At scale 1 the image has the canvas's size
    /// in logical pixels.
    ///
    /// # Errors
    ///
    /// The canvas has no area, the scaled image is over 16384 px a side, or PNG
    /// encoding fails.
    pub fn export_png(&mut self) -> Result<Vec<u8>, String> {
        let v = self.canvas_rect();
        // The camera is relative to the view's center: the image shows what the canvas shows.
        let cam = self.camera();
        self.raster_png(v.width().round(), v.height().round(), cam)
    }

    /// The whole drawing (its bounds plus a 10-unit margin) rasterized as PNG bytes at one
    /// logical pixel per schematic unit times the PNG scale, whatever the canvas shows.
    ///
    /// # Errors
    ///
    /// Nothing is drawn, the image is over 16384 px a side (lower the scale), or PNG
    /// encoding fails.
    pub fn export_png_full(&mut self) -> Result<Vec<u8>, String> {
        let b = export::bounds(&self.scene.prims)
            .map(|b| export::padded(b, 10 * UNIT))
            .ok_or("nothing to export")?;
        let u = f64::from(UNIT);
        let (w, h) = (
            (f64::from(b[2]) - f64::from(b[0])) / u,
            (f64::from(b[3]) - f64::from(b[1])) / u,
        );
        let cam = canvas::Camera {
            cx: (f64::from(b[0]) + f64::from(b[2])) / 2.0,
            cy: (f64::from(b[1]) + f64::from(b[3])) / 2.0,
            scale: 1.0 / u,
        };
        self.raster_png(w.ceil(), h.ceil(), cam)
    }

    /// Rasterizes a `w × h` logical-pixel view through `cam` at the PNG scale and encodes it.
    fn raster_png(&mut self, w: f64, h: f64, cam: canvas::Camera) -> Result<Vec<u8>, String> {
        let k = f64::from(self.opts.png_scale) / 100.0;
        assert!(k > 0.0 && cam.scale > 0.0, "a positive scale");
        let (pw, ph) = ((w * k).round(), (h * k).round());
        if w < 1.0 || h < 1.0 || pw < 1.0 || ph < 1.0 {
            return Err("the canvas has no drawable area".into());
        }
        let max = f64::from(PNG_MAX_PX);
        if pw > max || ph > max {
            return Err(format!(
                "a {pw} × {ph} px image is over {PNG_MAX_PX} px a side: lower the PNG scale \
                 (options.png_export_scale)"
            ));
        }
        let mut p = schemify_render::CpuPainter::new(pw as u16, ph as u16, k);
        let view = KRect::new(0.0, 0.0, w, h);
        let palette = self.view_palette();
        let grid = if self.opts.show_grid {
            self.opts.grid
        } else {
            i32::MAX / 4
        };
        let mut scratch = canvas::CanvasScratch::default();
        schemify_render::Painter::fill_rect(
            &mut p,
            view,
            schemify_render::rgba(palette.background),
        );
        canvas::draw_grid(&mut p, view, &cam, grid, &palette, &mut scratch);
        let prims = self.shown.as_ref().unwrap_or(&self.scene.prims);
        canvas::draw_prims(
            &mut p,
            view,
            &cam,
            prims,
            &palette,
            &mut self.shell.text,
            &mut scratch,
        );
        let mut scratch2 = canvas::CanvasScratch::default();
        canvas::draw_prims(
            &mut p,
            view,
            &cam,
            &self.decor,
            &palette,
            &mut self.shell.text,
            &mut scratch2,
        );
        let px = p.finish(schemify_render::vello::peniko::Color::BLACK);
        assert_eq!((px.width(), px.height()), (pw as u16, ph as u16));
        px.into_png().map_err(|e| format!("png: {e:?}"))
    }

    /// One PDF page per schematic from the active cell down (breadth first, each cell once):
    /// every page is that cell's scene built offscreen with the same symbol cache.
    fn export_hierarchy(&mut self, h: DocHandle, look: &export::Look) -> Result<Vec<u8>, String> {
        let docs = self.hierarchy_docs(h);
        let mut scenes: Vec<scene::Scene> = Vec::with_capacity(docs.len());
        for &d in &docs {
            self.refresh_symbols_of(d);
            let mut sc = scene::Scene::default();
            let epoch = self.session.epoch(d);
            scene::build(
                &mut sc,
                &self.session.project,
                d,
                View::Schematic,
                &self.symbols,
                epoch,
                None,
            );
            scenes.push(sc);
        }
        let pages: Vec<(&CanvasPrims, [i32; 4])> = scenes
            .iter()
            .filter_map(|sc| {
                export::bounds(&sc.prims).map(|b| (&sc.prims, export::padded(b, 10 * UNIT)))
            })
            .collect();
        if pages.is_empty() {
            return Err("nothing to export".into());
        }
        Ok(export::pdf(&pages, look, self.opts.export_page))
    }

    /// The schematic documents of the hierarchy below `h` (`h` first), each once, opening
    /// the cells it instantiates (bounded by `CHANGELOG_ROWS`).
    fn hierarchy_docs(&mut self, h: DocHandle) -> Vec<DocHandle> {
        let mut out = vec![h];
        let mut i = 0;
        while i < out.len() && out.len() < CHANGELOG_ROWS {
            let d = out[i];
            i += 1;
            let refs: Vec<String> = self.session.project.doc(d).map_or_else(Vec::new, |doc| {
                let s = doc.strings();
                let mut c: Vec<String> = doc
                    .instances()
                    .cell
                    .iter()
                    .map(|&x| s.get(x).to_owned())
                    .collect();
                c.sort_unstable();
                c.dedup();
                c
            });
            for r in refs {
                let Ok(row) = self.session.project.resolve(d, &r) else {
                    continue;
                };
                if self.session.project.cells().kind[row] != FileKind::Shcomp {
                    continue;
                }
                if let Ok(child) = self.session.open_cell(row)
                    && !out.contains(&child)
                {
                    out.push(child);
                }
            }
        }
        out
    }

    /// Makes the symbol cache current for every cell `d` instantiates.
    fn refresh_symbols_of(&mut self, d: DocHandle) {
        let refs: Vec<String> = self.session.project.doc(d).map_or_else(Vec::new, |doc| {
            let s = doc.strings();
            doc.instances()
                .cell
                .iter()
                .map(|&x| s.get(x).to_owned())
                .collect()
        });
        for r in refs {
            let Ok(row) = self.session.project.resolve(d, &r) else {
                continue;
            };
            if let Ok(c) = self.session.open_cell(row) {
                let epoch = self.session.epoch(c);
                if let Some(doc) = self.session.project.doc(c) {
                    self.symbols.refresh(row, c, epoch, doc);
                }
            }
        }
    }

    // ------------------------------------------------------------------ shapes and wires

    /// A click while a shape tool is active: adds the point, closes a polygon on its first
    /// vertex, finishes an open polygon on its last vertex, and commits once the shape has
    /// its points. Persistent mode chains lines.
    pub(super) fn shape_click(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        view: View,
        form: Form,
        mut pts: Vec<Pt>,
        p: Pt,
    ) {
        assert!(!pts.is_empty(), "a shape tool starts with its first point");
        if form == Form::Poly {
            if pts.len() >= 3 && p == pts[0] {
                self.finish_shape(reqs, h, view, form, &pts, true);
                return;
            }
            if pts.len() >= 2 && Some(&p) == pts.last() {
                self.finish_shape(reqs, h, view, form, &pts, false);
                return;
            }
        }
        if Some(&p) != pts.last() {
            pts.push(p);
        }
        if form.points() == Some(pts.len()) {
            self.finish_shape(reqs, h, view, form, &pts, false);
            if form == Form::Line && self.opts.persistent {
                self.tool = Tool::Shape {
                    form,
                    pts: vec![p],
                    to: p,
                };
            }
        } else {
            self.tool = Tool::Shape { form, pts, to: p };
        }
        self.opts.constrain = 0;
    }

    /// Commits a drawn shape: symbol graphics in the symbol view, a `shape` record in a
    /// schematic.
    pub(super) fn finish_shape(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        view: View,
        form: Form,
        pts: &[Pt],
        closed: bool,
    ) {
        self.tool = Tool::Idle;
        let schematic = view == View::Schematic;
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        if schematic && doc.kind() == FileKind::Shprim {
            self.session.status = "a primitive has no schematic: draw in its symbol".into();
            return;
        }
        let mut ids = Ids::new(&mut self.rng);
        let (id, raw) = if schematic {
            let (t, r) = ids.draw(doc);
            (Some(t), Some(r))
        } else {
            (None, None)
        };
        let Some(rec) = shape_record(form, pts, closed, self.opts.layer, id.as_deref()) else {
            self.session.status = "that shape has no extent".into();
            return;
        };
        let label = match form {
            Form::Line => "draw line",
            Form::Rect => "draw rect",
            Form::Poly => "draw polygon",
            Form::Arc => "draw arc",
            Form::Circle => "draw circle",
        };
        self.commit(reqs, h, label, vec![Op::Insert { rec, at: None }]);
        if let Some(raw) = raw {
            self.set_selection(vec![ObjKey {
                table: TableId::Shapes,
                key: raw,
            }]);
        }
    }

    /// Ends a move: a plain move, or a stretch (`stretch` 1) / kissing (2) move whose
    /// attached wires follow (translations only; a move rotated in flight is plain).
    pub(super) fn finish_stretch(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        x: Xform,
        stretch: u8,
    ) {
        if !x.is_translation() || stretch == 0 {
            let ops = self.with_doc(h, |doc, sel| edit::transform(doc, sel, x));
            self.commit(reqs, h, "move", ops);
            return;
        }
        let sel = self.selection().to_vec();
        let manhattan = self.opts.manhattan;
        let out = {
            let mut ids = Ids::new(&mut self.rng);
            wiring::stretch(
                &self.session.project,
                h,
                &sel,
                x,
                stretch == 2,
                manhattan,
                &mut ids,
            )
        };
        let label = if stretch == 2 {
            "kissing move"
        } else {
            "stretch move"
        };
        // The selection stays what was moved: stretched wires are whole objects here, so
        // keeping them selected would move them whole next time (xschem's partial wire
        // selection has no equivalent).
        self.commit(reqs, h, label, out.ops);
        self.session.status = format!("{label}: {} wire(s) followed", out.wires.len());
    }

    /// Spine step after `doc::apply_edits`: when a wire edit asked for it (Options → auto
    /// join/trim), joins and trims that document's wires as its own transaction (trim reads
    /// the applied tables, so it cannot share the wire edit's). Returns whether it applied.
    pub(super) fn auto_trim(&mut self, reqs: &mut Vec<EditRequest>) -> bool {
        let Some(h) = self.trim_after.take() else {
            return false;
        };
        let ops = {
            let mut ids = Ids::new(&mut self.rng);
            wiring::join_trim(&self.session.project, h, &mut ids)
        };
        if ops.is_empty() {
            return false;
        }
        self.commit(reqs, h, "join/trim wires", ops);
        self.apply(reqs)
    }

    /// Tools → Break wires at mouse position: cuts the wire under the pointer there (snapped
    /// with the align variant, else the pointer projected onto an axis-aligned wire).
    fn cut_wire(&mut self, reqs: &mut Vec<EditRequest>, h: DocHandle, cmd: Cmd) {
        let Some(o) = self.hit_at(self.pointer) else {
            self.session.status = "no wire under the pointer".into();
            return;
        };
        let key = self.scene.objs.key[o as usize];
        if key.table != TableId::Wires {
            self.session.status = "no wire under the pointer".into();
            return;
        }
        let g = self.scene.objs.geom[o as usize];
        let p = if cmd == Cmd::BreakAtMouseAlign {
            self.pointer_world_snapped()
        } else {
            let (x, y) = self.pointer_world();
            (x.round() as i32, y.round() as i32)
        };
        // Project onto an axis-aligned wire so the cut lies on it.
        let p = if g[1] == g[3] {
            (p.0, g[1])
        } else if g[0] == g[2] {
            (g[0], p.1)
        } else {
            p
        };
        let ops = {
            let Some(doc) = self.session.project.doc(h) else {
                return;
            };
            let mut ids = Ids::new(&mut self.rng);
            wiring::break_at(doc, &[p], &mut ids).0
        };
        if ops.is_empty() {
            self.session.status = "the cut point is not inside the wire".into();
        }
        self.commit(reqs, h, "cut wire", ops);
    }

    // ------------------------------------------------------------------ dialogs

    /// A one-number setting: from `arg`, else a dialog prefilled with the current value.
    fn setting(&mut self, cmd: Cmd, arg: &str) {
        let units = |m: i32| {
            let mut s = String::new();
            schemify_format::format_coord(m, &mut s);
            s
        };
        if arg.trim().is_empty() {
            let (title, value) = match cmd {
                Cmd::CrosshairSize => (
                    "Crosshair size (px, 0 = full)",
                    self.opts.crosshair_px.to_string(),
                ),
                Cmd::LineWidth => (
                    "Line width (px)",
                    format!("{}", f64::from(self.opts.width_tenths) / 10.0),
                ),
                Cmd::DimColors => (
                    "Dim factor (0..1)",
                    format!("{}", f64::from(self.opts.dim) / 100.0),
                ),
                Cmd::SymbolWidth => ("Symbol width (units)", units(self.opts.symbol_width)),
                Cmd::GridDialog => ("Grid spacing (units)", units(self.opts.grid)),
                Cmd::ExportPage => (
                    "Export page (a4, a3, a5, letter, legal, tabloid; portrait)",
                    self.opts.export_page.name(),
                ),
                Cmd::PngScale => (
                    "PNG export scale (1 = screen resolution, 96 dpi)",
                    format!("{}", f64::from(self.opts.png_scale) / 100.0),
                ),
                _ => ("Snap (units)", units(self.opts.snap)),
            };
            self.open_text(TextFor::Setting(cmd), title);
            if let Dialog::Text(_, _, typed) = &mut self.dialog {
                *typed = value;
            }
            return;
        }
        let bad = || format!("{arg:?}: out of range");
        match cmd {
            Cmd::CrosshairSize => match number(arg, 0.0, 10_000.0) {
                Some(v) => self.opts.crosshair_px = v as u16,
                None => self.session.status = bad(),
            },
            Cmd::LineWidth => match number(arg, 0.1, 10.0) {
                Some(v) => self.opts.width_tenths = (v * 10.0).round().clamp(1.0, 100.0) as u8,
                None => self.session.status = bad(),
            },
            Cmd::DimColors => match number(arg, 0.0, 1.0) {
                Some(v) => self.opts.dim = (v * 100.0).round() as u8,
                None => self.session.status = bad(),
            },
            Cmd::SymbolWidth => match number(arg, 10.0, 10_000.0) {
                Some(v) => self.opts.symbol_width = (v * f64::from(UNIT)).round() as i32,
                None => self.session.status = bad(),
            },
            Cmd::GridDialog | Cmd::SnapDialog => match number(arg, 0.001, 1000.0) {
                Some(v) => {
                    let m = ((v * f64::from(UNIT)).round() as i32).max(1);
                    if cmd == Cmd::GridDialog {
                        self.opts.grid = m;
                    } else {
                        self.opts.snap = m;
                    }
                }
                None => self.session.status = bad(),
            },
            Cmd::ExportPage => match export::Page::parse(arg) {
                Some(p) => {
                    self.opts.export_page = p;
                    self.session.status = format!("export page: {}", p.name());
                }
                None => {
                    self.session.status = format!(
                        "{arg:?}: not a page (a4, a3, a5, letter, legal, tabloid; portrait)"
                    );
                }
            },
            Cmd::PngScale => match number(arg, 0.1, 16.0) {
                Some(v) => self.opts.png_scale = (v * 100.0).round().clamp(10.0, 1600.0) as u16,
                None => self.session.status = bad(),
            },
            _ => {}
        }
    }

    /// OK of an enter-text dialog opened by a parity command.
    pub(super) fn text_ok(&mut self, purpose: TextFor, text: &str, reqs: &mut Vec<EditRequest>) {
        match purpose {
            TextFor::SaveAs(symbol) => {
                if let Some((h, _)) = self.active() {
                    self.save_as(h, text, symbol);
                }
            }
            TextFor::Setting(cmd) => self.setting(cmd, text),
            TextFor::LayerColor => {
                self.parity(Cmd::LayerColor, text, "", reqs);
            }
            TextFor::Label | TextFor::Port(_) | TextFor::Text | TextFor::SymPin => {
                unreachable!("placements are handled by enter_text_ok")
            }
        }
    }

    fn open_search(&mut self) {
        self.tool = Tool::Idle;
        let f = |key: &str, label: &str, value: &str| Field {
            key: key.to_owned(),
            label: label.to_owned(),
            value: value.to_owned(),
            original: value.to_owned(),
            editable: true,
        };
        self.dialog = Dialog::Props(Props {
            of: PropsFor::Search,
            title: "Search / replace".into(),
            fields: vec![
                f(
                    "field",
                    "field (name, cell, net, text, a param, * = any)",
                    "*",
                ),
                f("value", "value", ""),
                f("replace", "replace with (empty: select matches)", ""),
                f("exact", "exact match (true/false)", "false"),
            ],
        });
        self.show_dialog("dialog.props");
        self.focus_next = Some("dialog.props.val.1".into());
    }

    /// OK of the search dialog: select the matches, or replace them all (one undo).
    pub(super) fn search_ok(
        &mut self,
        h: DocHandle,
        fields: &[Field],
        reqs: &mut Vec<EditRequest>,
    ) {
        let get = |k: &str| {
            fields
                .iter()
                .find(|f| f.key == k)
                .map_or("", |f| f.value.as_str())
        };
        let q = search::Query {
            field: match get("field") {
                "" => "*".to_owned(),
                f => f.to_owned(),
            },
            value: get("value").to_owned(),
            exact: get("exact") == "true",
        };
        if q.value.is_empty() {
            self.session.status = "search: no value".into();
            return;
        }
        let with = get("replace").to_owned();
        if with.is_empty() {
            let keys = self.with_doc(h, |d, _| search::find(d, &q));
            self.session.status = if keys.is_empty() {
                "not found".into()
            } else {
                format!("{} object(s) found", keys.len())
            };
            self.set_selection(keys);
            if let Some(b) = self.selection_bbox() {
                self.fit(b);
            }
        } else {
            let ops = self.with_doc(h, |d, _| search::replace(d, &q, &with));
            self.session.status = format!("{} replacement(s)", ops.len());
            self.commit(reqs, h, "replace", ops);
        }
    }

    fn open_header(&mut self, h: DocHandle) {
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let f = |key: &str| {
            let v = doc.header_value(key).unwrap_or("").to_owned();
            Field {
                key: key.to_owned(),
                label: key.to_owned(),
                value: v.clone(),
                original: v,
                editable: true,
            }
        };
        let fields = vec![f("doc"), f("prefix"), f("category")];
        self.tool = Tool::Idle;
        self.dialog = Dialog::Props(Props {
            of: PropsFor::Header,
            title: "Header".into(),
            fields,
        });
        self.show_dialog("dialog.props");
        self.focus_next = Some("dialog.props.val.0".into());
    }

    /// OK of the header dialog: one `set_attr`/`remove_attr` per changed field.
    pub(super) fn header_ok(
        &mut self,
        h: DocHandle,
        fields: &[Field],
        reqs: &mut Vec<EditRequest>,
    ) {
        let ops: Vec<Op> = fields
            .iter()
            .filter(|f| f.value != f.original)
            .map(|f| {
                if f.value.is_empty() {
                    Op::RemoveAttr {
                        target: Target::header(),
                        key: f.key.clone(),
                        first: None,
                    }
                } else {
                    Op::SetAttr {
                        target: Target::header(),
                        key: f.key.clone(),
                        values: vec![f.value.clone()],
                    }
                }
            })
            .collect();
        self.commit(reqs, h, "edit header", ops);
    }

    /// Properties → Edit file OK: the typed text replaces the document in one transaction —
    /// changed records `put`, removed ones deleted, new ones inserted (format `diff` by
    /// identity); symbol graphics, which have no identity, are replaced as a block.
    fn apply_file_text(&mut self, reqs: &mut Vec<EditRequest>, h: DocHandle, text: &str) {
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let new = match schemify_format::parse(text.as_bytes()) {
            Ok(d) => d,
            Err(e) => {
                let d = e.diagnostics.first();
                self.session.status = format!(
                    "edit file: {}",
                    d.map_or_else(String::new, |d| format!(
                        "line {}: {:?} {}",
                        d.line, d.code, d.detail
                    ))
                );
                return;
            }
        };
        if new.kind() != doc.kind() {
            self.session.status = "edit file: the file kind cannot change".into();
            return;
        }
        let current = doc.canonical();
        let old = schemify_format::parse(current.as_bytes()).expect("canonical text parses");
        let d = schemify_format::diff(&old, &new);
        let mut ops = Vec::new();
        let target = |f: &schemify_format::Document<'_>, r: u32| -> Target {
            let kind = f.record_kind(r);
            match kind {
                RecordKind::Header => Target::header(),
                _ => Target::new(kind, f.field(r, 0).unwrap_or("")),
            }
        };
        let graphic = |k: RecordKind| k.rank() == schemify_format::RANK_POSITIONAL;
        let mut graphics = false;
        for c in &d.changes {
            let kind = if c.b != schemify_format::NONE {
                new.record_kind(c.b)
            } else {
                old.record_kind(c.a)
            };
            if graphic(kind) {
                graphics = true;
                continue;
            }
            match c.kind {
                schemify_format::ChangeKind::Removed => ops.push(Op::Delete(target(&old, c.a))),
                schemify_format::ChangeKind::Added => ops.push(Op::Insert {
                    rec: RecordBuf::from_format(&new, c.b),
                    at: None,
                }),
                schemify_format::ChangeKind::Changed => ops.push(Op::Put {
                    target: target(&old, c.a),
                    rec: RecordBuf::from_format(&new, c.b),
                }),
            }
        }
        if graphics {
            // Delete every old graphic (row 0 each time), then insert the new ones in order.
            for &kind in &doc.graphics().shape {
                ops.push(Op::Delete(Target::new(kind, "0")));
            }
            for r in 1..new.record_count() {
                if graphic(new.record_kind(r)) {
                    ops.push(Op::Insert {
                        rec: RecordBuf::from_format(&new, r),
                        at: None,
                    });
                }
            }
        }
        if ops.is_empty() {
            self.session.status = "edit file: no change".into();
        }
        self.commit(reqs, h, "edit file", ops);
    }

    // ------------------------------------------------------------------ symbol

    /// Symbol → Make symbol from schematic: pins from the ports (inputs left, outputs and
    /// inouts right, in id order), a box, the cell name and pin names; replaces the cell's
    /// pins and symbol graphics (one undo).
    fn make_symbol_ops(&mut self, h: DocHandle) -> Vec<Op> {
        let Some(doc) = self.session.project.doc(h) else {
            return Vec::new();
        };
        if doc.kind() != FileKind::Shcomp {
            self.session.status = "make symbol: only a .shcomp has a symbol and a schematic".into();
            return Vec::new();
        }
        let s = doc.strings();
        let p = doc.ports();
        let ports: Vec<(String, Orient, Pt)> = (0..p.id.len())
            .map(|i| (s.get(p.text[i]).to_owned(), p.orient[i], (p.x[i], p.y[i])))
            .collect();
        let recs = symbol_records(&ports, self.opts.symbol_width);
        if recs.is_empty() {
            self.session.status = "make symbol: the schematic has no ports".into();
            return Vec::new();
        }
        let mut ops = Vec::new();
        for i in 0..doc.pins().name.len() {
            ops.push(Op::Delete(Target::new(
                RecordKind::Pin,
                s.get(doc.pins().name[i]),
            )));
        }
        // Row 0 each time: the rows of the graphics still to delete shift up.
        for &kind in &doc.graphics().shape {
            ops.push(Op::Delete(Target::new(kind, "0")));
        }
        ops.extend(recs.into_iter().map(|rec| Op::Insert { rec, at: None }));
        ops
    }

    /// Symbol → Make schematic from symbol: a port for every pin without one, in a column
    /// at the pointer (inputs R0, outputs R180).
    fn make_schematic_ops(&mut self, h: DocHandle, at: Pt) -> Vec<Op> {
        let Some(doc) = self.session.project.doc(h) else {
            return Vec::new();
        };
        if doc.kind() != FileKind::Shcomp {
            self.session.status = "make schematic: the active cell is not a .shcomp".into();
            return Vec::new();
        }
        let s = doc.strings();
        let ports: Vec<&str> = doc.ports().text.iter().map(|&t| s.get(t)).collect();
        let pins = doc.pins();
        let mut ops = Vec::new();
        let mut k = 0;
        for i in 0..pins.name.len() {
            let name = s.get(pins.name[i]);
            if ports.contains(&name) {
                continue;
            }
            let o = match pins.dir[i] {
                schemify_doc::PinDir::Out => Orient::R180,
                _ => Orient::R0,
            };
            let rec = edit::placed(
                RecordKind::Port,
                (at.0, at.1 + k * PITCH),
                o,
                name,
                edit::AUTO_ID,
            );
            ops.push(Op::Insert { rec, at: None });
            k += 1;
        }
        ops
    }

    /// Symbol → Attach net labels: a label named after the pin on every pin of the selected
    /// instances that touches nothing else.
    fn attach_labels_ops(&mut self, h: DocHandle) -> Vec<Op> {
        let sel = self.selection().to_vec();
        let p = &self.session.project;
        let Some(doc) = p.doc(h) else {
            return Vec::new();
        };
        let mut occupied: Vec<Pt> = self.scene.conn.iter().map(|c| c.0).collect();
        occupied.sort_unstable();
        let count = |q: Pt| {
            let lo = occupied.partition_point(|&x| x < q);
            let hi = occupied.partition_point(|&x| x <= q);
            hi - lo
        };
        let mut ops = Vec::new();
        for k in sel.iter().filter(|k| k.table == TableId::Instances) {
            let Some(loc) = edit::loc_of(doc, *k) else {
                continue;
            };
            let row = loc.row as usize;
            let cellref = doc.strings().get(doc.instances().cell[row]);
            let Some(iface) = p.resolve(h, cellref).ok().and_then(|r| p.interface(r)) else {
                continue;
            };
            let pts = wiring::inst_pins(p, h, doc, row);
            for (i, q) in pts.into_iter().enumerate() {
                // The pin itself is one connection point; anything else touching it is another.
                if count(q) > 1 {
                    continue;
                }
                let name = iface.strings().get(iface.pins().name[i]).to_owned();
                let rec = edit::placed(RecordKind::Label, q, Orient::R0, &name, edit::AUTO_ID);
                ops.push(Op::Insert { rec, at: None });
            }
        }
        if ops.is_empty() {
            self.session.status = "no unconnected pins on the selected instances".into();
        }
        ops
    }

    /// Symbol → Make schematic and symbol from selected components: the selection moves into
    /// a new cell `<cell>_sub<k>.shcomp` beside this one (its objects, a port wherever a net
    /// crosses the selection boundary, a symbol made from those ports), and an instance of it
    /// replaces the selection, joined to the boundary nets by labels (one undo here; the new
    /// file is written at once).
    fn make_subcircuit(&mut self, reqs: &mut Vec<EditRequest>, h: DocHandle) {
        let sel = self.selection().to_vec();
        if sel.is_empty() {
            self.session.status = "select the components to move into a new cell".into();
            return;
        }
        let Some(path) = self.session.path(h).map(Path::to_path_buf) else {
            return;
        };
        let (Some(cell), Some(doc)) = (self.cell.as_ref(), self.session.project.doc(h)) else {
            self.session.status = "the cell is not elaborated".into();
            return;
        };
        if cell.inst_iface.len() != doc.instances().id.len() {
            return;
        }
        // Components touched by selected and by unselected objects: the boundary nets.
        let selset: std::collections::HashSet<ObjKey> = sel.iter().copied().collect();
        let n = &cell.nets;
        let mut inside: Vec<(u32, Pt)> = Vec::new();
        let mut outside: Vec<u32> = Vec::new();
        let tables = [
            (TableId::Wires, &doc.wires().id, &n.wire_net),
            (TableId::Labels, &doc.labels().id, &n.label_net),
            (TableId::Ports, &doc.ports().id, &n.port_net),
        ];
        for (table, ids, nets) in tables {
            for (r, (id, &c)) in ids.iter().zip(nets).enumerate() {
                let k = ObjKey { table, key: id.0 };
                if selset.contains(&k) {
                    let at = match table {
                        TableId::Wires => (doc.wires().x1[r], doc.wires().y1[r]),
                        TableId::Labels => (doc.labels().x[r], doc.labels().y[r]),
                        _ => (doc.ports().x[r], doc.ports().y[r]),
                    };
                    inside.push((c, at));
                } else {
                    outside.push(c);
                }
            }
        }
        for i in 0..doc.instances().id.len() {
            let k = ObjKey {
                table: TableId::Instances,
                key: doc.instances().id[i].0,
            };
            for (c, at) in checks::pins_of(doc, cell, i) {
                if selset.contains(&k) {
                    inside.push((c, at));
                } else {
                    outside.push(c);
                }
            }
        }
        outside.sort_unstable();
        outside.dedup();
        let mut boundary: Vec<(u32, Pt)> = inside
            .iter()
            .copied()
            .filter(|(c, _)| outside.binary_search(c).is_ok())
            .collect();
        boundary.sort_by_key(|b| b.0);
        boundary.dedup_by_key(|b| b.0);
        let names: Vec<String> = boundary
            .iter()
            .map(|(c, _)| checks::net_name(cell, *c).to_owned())
            .collect();
        // The new cell file.
        let stem = path
            .file_stem()
            .map_or_else(|| "cell".to_owned(), |s| s.to_string_lossy().into_owned());
        let dir = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
        let Some(file) = (1..100)
            .map(|k| dir.join(format!("{stem}_sub{k}.shcomp")))
            .find(|p| !platform::exists(p))
        else {
            self.session.status = format!("{stem}_sub*.shcomp: no free name");
            return;
        };
        let sub_stem = file
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let recs: Vec<RecordBuf> = edit::selected_records(doc, &sel)
            .into_iter()
            .map(|(_, _, r)| r)
            .filter(|r| r.kind.rank() == 3)
            .collect();
        let scratch = schemify_doc::Document::new(FileKind::Shcomp);
        let mut ids = Ids::new(&mut self.rng);
        // The new cell: its symbol made from the boundary ports, the moved objects, the ports.
        let ports: Vec<(String, Orient, Pt)> = boundary
            .iter()
            .zip(&names)
            .map(|((_, at), name)| (name.clone(), Orient::R0, *at))
            .collect();
        let symbol = symbol_records(&ports, self.opts.symbol_width);
        let sub_pins: Vec<(String, Pt)> = symbol
            .iter()
            .filter(|r| r.kind == RecordKind::Pin)
            .filter_map(|r| {
                let x = schemify_format::parse_coord(&r.fields[2]).ok()?;
                let y = schemify_format::parse_coord(&r.fields[3]).ok()?;
                Some((r.fields[0].clone(), (x, y)))
            })
            .collect();
        let mut frag: Vec<RecordBuf> = symbol;
        frag.extend(recs.iter().cloned());
        for ((_, at), name) in boundary.iter().zip(&names) {
            let (id, _) = ids.draw(&scratch);
            frag.push(edit::placed(RecordKind::Port, *at, Orient::R0, name, &id));
        }
        // A clipboard fragment is a `.shcomp` with one schematic impl: the new cell's file.
        let text = crate::clip::encode(&frag).replacen(
            "impl clipboard schematic",
            "impl sch schematic",
            1,
        );
        let parsed = match schemify_format::parse(text.as_bytes()) {
            Ok(d) => schemify_format::write(&d),
            Err(e) => {
                self.session.status = format!("new cell: {:?}", e.diagnostics.first());
                return;
            }
        };
        if let Err(e) = platform::write_atomic(&file, parsed.as_bytes()) {
            self.session.status = e;
            return;
        }
        self.session.reindex(&file);
        self.symbols = scene::Symbols::default();
        self.symbols_key = None;
        self.invalidate_cells();
        // The replacement here: an instance of the new cell where the selection was.
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let centroid = self.selection_pivot();
        let mut ops = edit::delete(doc, &sel);
        let mut ids = Ids::new(&mut self.rng);
        let prefix = "X";
        let name = edit::unique_name(doc, prefix, &mut Default::default());
        let (id, _) = ids.draw(doc);
        ops.push(Op::Insert {
            rec: edit::instance(&format!("./{sub_stem}"), centroid, Orient::R0, &name, &id),
            at: None,
        });
        // Labels at the boundary points keep the outside nets named; the instance's pins get
        // the same names through labels placed on them.
        let sub_pins: Vec<(String, Pt)> = sub_pins
            .into_iter()
            .map(|(n, p)| (n, crate::geometry::place(Orient::R0, centroid, p)))
            .collect();
        for ((_, at), net) in boundary.iter().zip(&names) {
            let (id, _) = ids.draw(doc);
            ops.push(Op::Insert {
                rec: edit::placed(RecordKind::Label, *at, Orient::R0, net, &id),
                at: None,
            });
            if let Some((_, q)) = sub_pins.iter().find(|p| p.0 == *net) {
                let (id, _) = ids.draw(doc);
                ops.push(Op::Insert {
                    rec: edit::placed(RecordKind::Label, *q, Orient::R0, net, &id),
                    at: None,
                });
            }
        }
        self.commit(reqs, h, "make subcircuit", ops);
        self.set_selection(Vec::new());
        self.session.status = format!(
            "new cell {} with {} port(s)",
            file.display(),
            boundary.len()
        );
    }

    /// Local names of the highlighted nets at the active level.
    fn highlighted_local_names(&mut self) -> Vec<String> {
        let names: Vec<String> = match self.cell.as_ref() {
            Some(cell) => (0..cell.nets.net_name.len() as u32)
                .map(|c| checks::net_name(cell, c).to_owned())
                .collect(),
            None => return Vec::new(),
        };
        let mut out: Vec<String> = names
            .into_iter()
            .filter(|n| {
                let key = self.net_key(n);
                self.highlight.binary_search(&key).is_ok()
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Highlight → net/pin mismatches: highlights the nets of the selected instances' pins
    /// whose net is named differently from the pin.
    fn pin_mismatch(&mut self, h: DocHandle) {
        let mut nets: Vec<String> = Vec::new();
        {
            let (Some(cell), Some(doc)) = (self.cell.as_ref(), self.session.project.doc(h)) else {
                self.session.status = "the cell is not elaborated".into();
                return;
            };
            let p = &self.session.project;
            for k in self
                .selection()
                .iter()
                .filter(|k| k.table == TableId::Instances)
            {
                let Some(loc) = edit::loc_of(doc, *k) else {
                    continue;
                };
                let row = loc.row as usize;
                if row >= cell.inst_iface.len() {
                    continue;
                }
                let cellref = doc.strings().get(doc.instances().cell[row]);
                let Some(iface) = p.resolve(h, cellref).ok().and_then(|r| p.interface(r)) else {
                    continue;
                };
                let ordinary: Vec<String> = (0..iface.pins().name.len())
                    .filter(|&i| {
                        iface
                            .pool()
                            .find_attr(iface.strings(), iface.pins().attrs[i], "inherit")
                            .is_none()
                    })
                    .map(|i| iface.strings().get(iface.pins().name[i]).to_owned())
                    .collect();
                for (pin, (c, _)) in ordinary.iter().zip(checks::pins_of(doc, cell, row)) {
                    let net = checks::net_name(cell, c);
                    if net != pin {
                        nets.push(net.to_owned());
                    }
                }
            }
        }
        nets.sort_unstable();
        nets.dedup();
        for n in &nets {
            let key = self.net_key(n);
            if let Err(i) = self.highlight.binary_search(&key)
                && self.highlight.len() < checks::HIGHLIGHT_MAX
            {
                self.highlight.insert(i, key);
            }
        }
        self.decor_dirty = true;
        self.session.status = format!("{} net/pin mismatch(es)", nets.len());
    }

    /// Adds the nets the waves dock plots (`v(<net>)`) to the highlight.
    fn highlight_plotted(&mut self) {
        let traces = self.waves_traces();
        for t in traces {
            let Some(net) = t.strip_prefix("v(").and_then(|r| r.strip_suffix(')')) else {
                continue;
            };
            let key = net.to_ascii_lowercase();
            if let Err(i) = self.highlight.binary_search(&key)
                && self.highlight.len() < checks::HIGHLIGHT_MAX
            {
                self.highlight.insert(i, key);
            }
        }
        self.decor_dirty = true;
    }

    // ------------------------------------------------------------------ compare

    /// Starts comparing the active document with indexed cell `row`.
    pub(super) fn compare_with(&mut self, row: usize) {
        self.compare = Some(Compare {
            row,
            on: true,
            key: None,
        });
        self.decor_dirty = true;
        let name = crate::session::cell_relpath(&self.session.project, row);
        self.session.status = format!("comparing with {name}");
    }

    /// Highlight → Swap compare schematics: the compared cell becomes the active level and
    /// the previous document the reference.
    fn compare_swap(&mut self, h: DocHandle) {
        let Some(c) = self.compare.clone() else {
            self.session.status = "choose a cell to compare with first".into();
            return;
        };
        let Some(prev_row) = self.row_of(h) else {
            return;
        };
        match self.session.open_cell(c.row) {
            Ok(d) => {
                if let Some(l) = self.session.level_mut() {
                    l.doc = d;
                    l.view = View::Schematic;
                    l.sel.clear();
                    l.cam = None;
                }
                self.compare = Some(Compare {
                    row: prev_row,
                    on: true,
                    key: None,
                });
                self.scene_dirty = true;
                self.decor_dirty = true;
            }
            Err(e) => self.session.status = e,
        }
    }

    /// The indexed cell row of open document `h`, if it is a project cell.
    fn row_of(&self, h: DocHandle) -> Option<usize> {
        let path = self.session.path(h)?;
        let p = &self.session.project;
        (0..p.cells().lib.len()).find(|&r| {
            let lib = p.cells().lib[r];
            self.session
                .lib_dir(lib)
                .is_some_and(|d| d.join(crate::session::cell_relpath(p, r)) == path)
        })
    }

    /// Rebuilds the parity decor (`extra`): the comparison (objects added or changed here
    /// outlined, objects only in the other cell drawn), per-net highlight colors and
    /// connected-instance outlines. Called after `build_decor`; returns whether it rebuilt.
    pub(super) fn build_extra(&mut self, decor_rebuilt: bool) -> bool {
        let Some((h, view)) = self.active() else {
            return false;
        };
        let epoch = self.session.epoch(h);
        let compare_stale = self
            .compare
            .as_ref()
            .is_some_and(|c| c.on && c.key.is_none_or(|k| k.0 != h || k.1 != epoch));
        // Follows the decor (scene, highlight, overlay changes); the options that change it
        // mark the decor dirty when toggled.
        if !decor_rebuilt && !compare_stale {
            return false;
        }
        let mut out = CanvasPrims::default();
        if self.opts.incr_color && !self.highlight.is_empty() {
            self.colored_highlight(&mut out);
        }
        if self.opts.highlight_insts {
            let r = 2 * UNIT;
            let mut rows: Vec<u32> = self.hl.pins.iter().map(|p| p.0).collect();
            rows.dedup();
            if let Some(doc) = self.session.project.doc(h) {
                for row in rows {
                    let Some(&id) = doc.instances().id.get(row as usize) else {
                        continue;
                    };
                    let key = ObjKey {
                        table: TableId::Instances,
                        key: id.0,
                    };
                    if let Some(o) = self.scene.find(key) {
                        let b = self.scene.objs.bbox(o as usize);
                        out.rect(b[0] - r, b[1] - r, b[2] + r, b[3] + r, checks::LAYER_HILITE);
                    }
                }
            }
        }
        if let Some(c) = self.compare.clone()
            && c.on
            && view == View::Schematic
        {
            self.draw_compare(h, c.row, &mut out);
            if let Some(cm) = self.compare.as_mut() {
                cm.key = Some((h, epoch, 0));
            }
        }
        self.extra = out;
        self.canvas_gen[2] += 1;
        true
    }

    /// One color per highlighted net (Highlight → increment highlight color): each key gets a
    /// palette layer from `HILITE_LAYERS` by its position in the sorted highlight list.
    fn colored_highlight(&mut self, out: &mut CanvasPrims) {
        let Some((h, _)) = self.active() else { return };
        let names: Vec<String> = match self.cell.as_ref() {
            Some(cell) => (0..cell.nets.net_name.len() as u32)
                .map(|c| checks::net_name(cell, c).to_owned())
                .collect(),
            None => return,
        };
        let keyed: Vec<(u32, usize)> = names
            .iter()
            .enumerate()
            .filter_map(|(c, n)| {
                let key = self.net_key(n);
                self.highlight
                    .binary_search(&key)
                    .ok()
                    .map(|i| (c as u32, i))
            })
            .collect();
        let (Some(cell), Some(doc)) = (self.cell.as_ref(), self.session.project.doc(h)) else {
            return;
        };
        if cell.inst_iface.len() != doc.instances().id.len() {
            return;
        }
        let r = 3 * UNIT;
        for (c, i) in keyed {
            let layer = HILITE_LAYERS[i % HILITE_LAYERS.len()];
            let m = checks::members(doc, cell, &[c]);
            for k in &m.keys {
                let Some(o) = self.scene.find(*k) else {
                    continue;
                };
                let g = self.scene.objs.geom[o as usize];
                if self.scene.objs.seg[o as usize] {
                    out.line(g[0], g[1], g[2], g[3], layer);
                } else {
                    out.rect(g[0] - r, g[1] - r, g[2] + r, g[3] + r, layer);
                }
            }
            for &(_, _, p) in &m.pins {
                out.rect(p.0 - r, p.1 - r, p.0 + r, p.1 + r, layer);
            }
        }
    }

    /// The comparison drawn into `out`: objects of the active document added or changed
    /// against cell `row` outlined (diff layer), objects only in `row` drawn in the error
    /// layer at their place.
    fn draw_compare(&mut self, h: DocHandle, row: usize, out: &mut CanvasPrims) {
        let Ok(other) = self.session.open_cell(row) else {
            return;
        };
        let (Some(a), Some(b)) = (self.session.project.doc(other), self.session.project.doc(h))
        else {
            return;
        };
        let (ta, tb) = (a.canonical(), b.canonical());
        let (Ok(fa), Ok(fb)) = (
            schemify_format::parse(ta.as_bytes()),
            schemify_format::parse(tb.as_bytes()),
        ) else {
            return;
        };
        let d = schemify_format::diff(&fa, &fb);
        let pad = 4 * UNIT;
        let mut removed: Vec<RecordBuf> = Vec::new();
        let mut changed = 0usize;
        for c in &d.changes {
            match c.kind {
                schemify_format::ChangeKind::Removed => {
                    if fa.record_kind(c.a).rank() == 3 {
                        removed.push(RecordBuf::from_format(&fa, c.a));
                    }
                }
                _ => {
                    let Some(id) = fb.field(c.b, 0) else { continue };
                    let Some(oid) = schemify_doc::ObjId::find(id, b.strings()) else {
                        continue;
                    };
                    let Some(loc) = b.find_id(oid) else { continue };
                    let key = ObjKey {
                        table: loc.table,
                        key: oid.0,
                    };
                    if let Some(o) = self.scene.find(key) {
                        let bb = self.scene.objs.bbox(o as usize);
                        out.rect(
                            bb[0] - pad,
                            bb[1] - pad,
                            bb[2] + pad,
                            bb[3] + pad,
                            checks::LAYER_DIFF,
                        );
                        changed += 1;
                    }
                }
            }
        }
        scene::preview_records(&removed, Xform::IDENTITY, out);
        // Preview prims come on the selection layer: recolor them as removed (error layer).
        for l in &mut out.lines.layer {
            if *l == LAYER_SEL {
                *l = checks::LAYER_ERROR;
            }
        }
        for l in &mut out.rects.layer {
            if *l == LAYER_SEL {
                *l = checks::LAYER_ERROR;
            }
        }
        self.session.status = format!(
            "compare: {changed} added/changed here, {} only in the other cell",
            removed.len()
        );
    }

    // ------------------------------------------------------------------ text tabs

    /// Shows `body` in a read-only text tab titled `title` (`key` identifies it, so asking
    /// again refreshes the same tab).
    fn text_tab(&mut self, title: &str, key: &str, body: String) {
        self.session.open_text_tab(TextView {
            title: title.to_owned(),
            path: self.session.root.join(".schemify").join("text").join(key),
            body,
            find: String::new(),
            found: None,
        });
        self.scene_dirty = true;
    }

    fn changelog(&mut self, h: DocHandle) {
        let docs = self.hierarchy_docs(h);
        let mut body = String::from("Files of the hierarchy (modified, seconds since 1970)\n\n");
        for d in docs.into_iter().take(CHANGELOG_ROWS) {
            let Some(p) = self.session.path(d) else {
                continue;
            };
            let t = platform::mtime_unix_s(p).map_or_else(|| "-".to_owned(), |s| s.to_string());
            let dirty = if self.session.project.is_dirty(d) {
                " *"
            } else {
                ""
            };
            body.push_str(&format!("{t:>12}  {}{dirty}\n", p.display()));
        }
        self.text_tab("changelog", "changelog", body);
    }

    /// Tools → Package status: what `schemify pack status` prints for the user config
    /// (config spec §5), in a text tab.
    fn package_status(&mut self) {
        use schemify_cli::pack;
        let config = self.host.config();
        let specs = &config.table().packs;
        let body = match pack::Paths::of(config) {
            Err(e) => format!("user packages: {e}\n"),
            Ok(_) if specs.is_empty() => {
                "no user packages (`schemify.pack.add` in init.lua)\n".to_owned()
            }
            Ok(paths) => {
                let report = pack::status(specs, &paths);
                let mut b = report.lines.join("\n");
                b.push('\n');
                if report.failed > 0 {
                    b.push_str(&format!(
                        "\n{} package(s) missing or broken: run `schemify pack sync`\n",
                        report.failed
                    ));
                }
                b
            }
        };
        assert!(body.ends_with('\n'));
        self.text_tab("packages", "packages", body);
    }

    fn list_processes(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut body = String::from("Simulation jobs\n\n");
            match self.sim.tables() {
                Some((_, jobs, _)) => {
                    for j in 0..jobs.len() {
                        body.push_str(&format!("job {j}: {:?}\n", jobs.state(j)));
                    }
                    body.push('\n');
                    body.push_str(&self.sim.progress());
                    body.push('\n');
                }
                None => body.push_str("no run in this session\n"),
            }
            self.text_tab("processes", "processes", body);
        }
    }

    /// Simulation → View last job data / errors: the selected job's `stdout.log` /
    /// `stderr.log`.
    fn job_log(&mut self, errors: bool) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(dir) = self.sim.dir() else {
                self.session.status = "no run in this session".into();
                return;
            };
            let name = if errors {
                schemify_run::STDERR
            } else {
                schemify_run::STDOUT
            };
            let path = dir.join(self.sim.selected.to_string()).join(name);
            let body = platform::read(&path)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_else(|e| e);
            let title = if errors { "job errors" } else { "job output" };
            self.text_tab(title, title, body);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = errors;
    }

    // ------------------------------------------------------------------ context menu

    /// Opens the canvas context menu at screen point `at` (xschem §1.15): the no-selection or
    /// the selection variant.
    pub(super) fn context_menu(&mut self, at: (f32, f32)) {
        let selected = !self.selection().is_empty();
        self.dialog = Dialog::Ctx(selected);
        let sym = self.shell.sym("dialog.ctx");
        self.shell.ui.set_dialog_at(sym, Some(at));
        self.shell.invalidate();
    }

    /// A right click without a drag: modified clicks select connected wires or cut the wire
    /// (xschem B3 with Shift / Ctrl / Alt / Alt+Shift); a tool in progress ends (a polygon
    /// keeps its points, open); otherwise the context menu.
    pub(super) fn right_click(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        view: View,
        at: (f32, f32),
    ) {
        let (shift, ctrl, alt) = (
            self.mods & schemify_ui::Mods::SHIFT != 0,
            self.mods & schemify_ui::Mods::CTRL != 0,
            self.mods & schemify_ui::Mods::ALT != 0,
        );
        match std::mem::take(&mut self.tool) {
            Tool::Shape {
                form: Form::Poly,
                pts,
                ..
            } if pts.len() >= 2 => {
                self.finish_shape(reqs, h, view, Form::Poly, &pts, false);
                return;
            }
            Tool::Idle | Tool::Press { .. } => {}
            _ => return, // abort the tool in progress
        }
        let cmd = match (shift, ctrl, alt) {
            (true, false, false) => Some(Cmd::SelectConnected),
            (false, true, false) => Some(Cmd::SelectConnectedStop),
            (false, false, true) => Some(Cmd::BreakAtMouseAlign),
            (true, false, true) => Some(Cmd::BreakAtMouse),
            _ => None,
        };
        match cmd {
            Some(c) => {
                self.parity(c, "", "", reqs);
            }
            None => self.context_menu(at),
        }
    }

    // ------------------------------------------------------------------ frame steps

    /// Highlight → auto-highlight: the selection's nets replace the highlight on every
    /// selection change; an empty selection clears it (xschem `auto_hilight`).
    pub(super) fn auto_highlight(&mut self) {
        let Some((h, _)) = self.active() else { return };
        self.highlight.clear();
        if !self.selection().is_empty() {
            self.highlight_cmd(h, Cmd::Highlight);
        }
        self.decor_dirty = true;
    }

    /// Shows the crash-recovery prompt for the next document the journal offers edits for,
    /// when no other dialog is open.
    pub(super) fn prompt_recovery(&mut self) {
        if !matches!(self.dialog, Dialog::None) || !self.shell.ui.dialog().is_none() {
            return;
        }
        let Some((d, n)) = self.session.pending_recovery() else {
            return;
        };
        self.tool = Tool::Idle;
        self.dialog = Dialog::Recover(d, n);
        self.show_dialog("dialog.recover");
    }

    /// The palette the canvas draws with: the theme's, with layer color changes, hidden
    /// layers (alpha 0), dimming toward the background, the fill switch and the line width
    /// (scaled with the zoom when variable).
    pub fn view_palette(&self) -> Palette {
        let mut p = self.palette.clone();
        for &(l, rgb) in &self.layer_colors {
            p.color[usize::from(l)] = (rgb << 8) | 0xff;
        }
        let dim = if self.opts.only_probes && !self.highlight.is_empty() {
            70
        } else {
            self.opts.dim
        };
        if dim > 0 {
            let bg = p.background;
            let mix = |c: u32, shift: u32| {
                let (a, b) = (
                    f64::from((c >> shift) & 0xff),
                    f64::from((bg >> shift) & 0xff),
                );
                ((a + (b - a) * f64::from(dim) / 100.0).round() as u32) << shift
            };
            for (l, c) in p.color.iter_mut().enumerate() {
                // The highlight and annotation layers stay bright ("view only probes").
                if l == usize::from(checks::LAYER_HILITE) || l == 0 {
                    continue;
                }
                *c = mix(*c, 24) | mix(*c, 16) | mix(*c, 8) | (*c & 0xff);
            }
        }
        for l in 0..USER_LAYERS {
            if self.opts.hidden & (1 << l) != 0 {
                p.color[usize::from(l)] &= !0xff;
            }
        }
        if !self.opts.fill {
            p.fill = 0;
        }
        let mut w = f32::from(self.opts.width_tenths) / 10.0;
        if self.opts.variable_width {
            // ponytail: width ∝ zoom around the default fit (scale 0.01 px per milli-unit),
            // clamped; xschem's `change_lw` rule if users ask for its exact curve.
            w *= ((self.camera().scale / 0.01) as f32).clamp(0.5, 4.0);
        }
        for x in &mut p.width {
            *x *= w;
        }
        p
    }

    /// Rebuilds `shown` (the scene as displayed) when the symbol display mode or the symbol
    /// texts switch differ from the defaults: instances drawn as boxes, their texts dropped.
    pub(super) fn build_shown(&mut self) {
        let mode = self.opts.show_symbols;
        let texts = self.opts.symbol_text;
        if mode == 0 && texts {
            if self.shown.take().is_some() {
                self.canvas_gen[0] += 1; // the canvas draws the scene as built again
            }
            self.shown_key = None;
            return;
        }
        let Some(built) = self.scene.built else {
            return;
        };
        let key = (built, mode, texts);
        if self.shown_key == Some(key) {
            return;
        }
        let sc = &self.scene;
        let p = &self.session.project;
        let h = built.0;
        // Objects drawn as boxes: instances (mode 2) or instances of schematic cells (mode 1).
        let boxed: Vec<bool> = match p.doc(h) {
            Some(doc) => sc
                .objs
                .key
                .iter()
                .map(|k| {
                    if k.table != TableId::Instances || mode == 0 {
                        return false;
                    }
                    if mode == 2 {
                        return true;
                    }
                    edit::loc_of(doc, *k).is_some_and(|l| {
                        let cell = doc.strings().get(doc.instances().cell[l.row as usize]);
                        p.resolve(h, cell)
                            .is_ok_and(|r| p.cells().kind[r] == FileKind::Shcomp)
                    })
                })
                .collect(),
            None => vec![false; sc.objs.len()],
        };
        let is_inst: Vec<bool> = sc
            .objs
            .key
            .iter()
            .map(|k| k.table == TableId::Instances)
            .collect();
        let keep: Vec<bool> = boxed.iter().map(|b| !b).collect();
        let mut out = export::subset(sc, &keep);
        if !texts {
            // Rebuild the texts without the instances' own.
            let mut t = CanvasPrims::default();
            let pt = &out.texts;
            let mut start = 0usize;
            let owners: Vec<u32> = sc
                .owner
                .texts
                .iter()
                .copied()
                .filter(|&o| keep.get(o as usize).copied().unwrap_or(true))
                .collect();
            for i in 0..pt.x.len() {
                let end = pt.end[i] as usize;
                let owner = owners.get(i).copied().unwrap_or(u32::MAX);
                if !is_inst.get(owner as usize).copied().unwrap_or(false) {
                    let rot = pt.rot.get(i).copied().unwrap_or(0);
                    t.text_rot(
                        pt.x[i],
                        pt.y[i],
                        pt.size[i],
                        pt.layer[i],
                        &pt.text[start..end],
                        rot,
                    );
                }
                start = end;
            }
            out.texts = t.texts;
        }
        for (i, b) in boxed.iter().enumerate() {
            if *b {
                let bb = sc.objs.bbox(i);
                out.rect(bb[0], bb[1], bb[2], bb[3], 4);
            }
        }
        self.shown = Some(out);
        self.shown_key = Some(key);
        self.canvas_gen[0] += 1;
    }

    /// Overlay additions of the parity options: crosshair, snap cursor, grid axes and the
    /// preview of a shape tool in progress.
    pub(super) fn parity_overlay(&self, o: &mut CanvasPrims) {
        let v = self.canvas_rect();
        let cam = self.camera();
        let w = self.view_world_rect();
        if let Tool::Shape { form, pts, to } = &self.tool {
            let layer = self.opts.layer;
            match form {
                Form::Rect => {
                    o.rect(pts[0].0, pts[0].1, to.0, to.1, layer);
                }
                Form::Circle => {
                    o.arc(pts[0].0, pts[0].1, dist(pts[0], *to), 0, 360_000, layer);
                }
                Form::Arc if pts.len() == 2 => {
                    if let Some((cx, cy, r, s, sw)) = arc_3(pts[0], pts[1], *to) {
                        o.arc(cx, cy, r, s, sw, layer);
                    } else {
                        o.line(pts[0].0, pts[0].1, pts[1].0, pts[1].1, layer);
                    }
                }
                _ => {
                    let mut all = pts.clone();
                    all.push(*to);
                    all.dedup();
                    if all.len() >= 2 {
                        o.poly(&all, false, layer);
                    }
                }
            }
        }
        let (px, py) = self.pointer_world_snapped();
        let inside = |x: f64, y: f64| x >= v.x0 && x <= v.x1 && y >= v.y0 && y <= v.y1;
        let (sx, sy) = (f64::from(self.pointer.0), f64::from(self.pointer.1));
        if self.opts.crosshair && inside(sx, sy) {
            let r = if self.opts.crosshair_px == 0 {
                None
            } else {
                Some((f64::from(self.opts.crosshair_px) / cam.scale) as i32)
            };
            let (x0, x1) = r.map_or((w[0], w[2]), |r| (px - r, px + r));
            let (y0, y1) = r.map_or((w[1], w[3]), |r| (py - r, py + r));
            o.line(x0, py, x1, py, 8);
            o.line(px, y0, px, y1, 8);
        }
        if self.opts.snap_cursor
            && inside(sx, sy)
            && let Some(q) = self.nearest_conn((px, py))
        {
            let d = (6.0 / cam.scale) as i32;
            let near =
                (i64::from(q.0) - i64::from(px)).abs() + (i64::from(q.1) - i64::from(py)).abs();
            if near <= i64::from(d) * 8 {
                o.poly(
                    &[
                        (q.0 - d, q.1),
                        (q.0, q.1 - d),
                        (q.0 + d, q.1),
                        (q.0, q.1 + d),
                    ],
                    true,
                    LAYER_SEL,
                );
            }
        }
        if self.opts.grid_axes {
            if w[1] <= 0 && w[3] >= 0 {
                o.line(w[0], 0, w[2], 0, 2);
            }
            if w[0] <= 0 && w[2] >= 0 {
                o.line(0, w[1], 0, w[3], 2);
            }
        }
    }

    /// The parity part of the state contribution: menu check marks, the recent-files menu,
    /// the context menu, the layers and recovery dialogs, canvas-only view, toolbar.
    pub(super) fn contribute_parity(&self, t: &mut String) {
        use std::fmt::Write as _;
        let o = &self.opts;
        let flags = [
            ("options.auto_join_trim_wires", o.auto_trim),
            ("options.color_postscript_svg", o.export_color),
            ("options.transparent_svg_background", o.export_transparent),
            ("options.crosshair.draw_crosshair", o.crosshair),
            ("options.crosshair.draw_snap_cursor", o.snap_cursor),
            ("options.enable_stretch", o.stretch),
            ("options.netlist_format_symbol_mode.flat_netlist", o.flat),
            ("options.persistent_wire_line_place_command", o.persistent),
            ("view.show_hide.draw_grid_axes", o.grid_axes),
            ("view.show_hide.show_toolbar", o.toolbar),
            ("view.show_hide.symbol_text", o.symbol_text),
            (
                "view.show_hide.show_erc_info_window",
                self.panel == super::Panel::Checks,
            ),
            ("view.toggle_variable_line_width", o.variable_width),
            ("highlight.auto_highlight_net_pins", o.auto_highlight),
            (
                "highlight.enable_highlight_connected_instances",
                o.highlight_insts,
            ),
            ("highlight.increment_hilight_color", o.incr_color),
            ("highlight.view_only_probes", o.only_probes),
            (
                "highlight.compare_schematics",
                self.compare.as_ref().is_some_and(|c| c.on),
            ),
            (
                "simulation.graphs.auto_highlight_plotted_nets",
                o.highlight_plotted,
            ),
            (
                "simulation.graphs.annotate_operating_point_into_schematic",
                self.op_on,
            ),
            (
                "symbol.show_symbols.show_symbol_details",
                o.show_symbols == 0,
            ),
            (
                "symbol.show_symbols.show_instance_bounding_boxes_for_subcirc",
                o.show_symbols == 1,
            ),
            (
                "symbol.show_symbols.show_instance_bounding_boxes_for_all_sym",
                o.show_symbols == 2,
            ),
            (
                "options.netlist_format_symbol_mode.spice_netlist",
                self.dialect == schemify_elab::Dialect::Ngspice,
            ),
            (
                "options.netlist_format_symbol_mode.verilog_netlist",
                self.dialect == schemify_elab::Dialect::Verilog,
            ),
        ];
        for (id, on) in flags {
            let _ = writeln!(t, "set {id} checked {}", u8::from(on));
        }
        let _ = writeln!(
            t,
            "set status.mode label {}",
            spines::dialect_word(self.dialect)
        );
        let _ = writeln!(t, "set status.stretch label {}", u8::from(o.stretch));
        // Open recent: one row per remembered file.
        if self.recent.is_empty() {
            let _ = writeln!(
                t,
                "node file.open_recent.none item\n  parent menu.file.open_recent\n  label \"(none)\"\n  disabled 1"
            );
        }
        for (i, p) in self.recent.iter().enumerate() {
            let name = p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let _ = writeln!(
                t,
                "node file.open_recent.{i} item\n  parent menu.file.open_recent\n  label {}\n  command file.open_recent.{i}\n  tooltip {}",
                q(&name),
                q(&p.display().to_string())
            );
        }
        if o.fullscreen {
            for id in ["menubar", "toolbar", "tabs", "status"] {
                let _ = writeln!(t, "remove {id}");
            }
        } else if !o.toolbar {
            let _ = writeln!(t, "remove toolbar");
        }
        match &self.dialog {
            Dialog::Ctx(selected) => {
                let rows: &[(&str, &str)] = if *selected {
                    &[
                        ("Abort command", "view.redraw"),
                        ("Edit attributes", "properties.edit"),
                        ("Edit attr in editor", "properties.edit_with_editor"),
                        ("Descend schematic", "edit.push_schematic"),
                        ("Descend symbol", "edit.push_symbol"),
                        ("Delete selection", "edit.delete"),
                        ("Rotate selection", "edit.rotate_selected_objects"),
                        ("Flip selection", "edit.horizontal_flip_selected_objects"),
                        (
                            "Rotate in-place sel.",
                            "edit.rotate_in_place_selected_objects",
                        ),
                        (
                            "Flip in-place sel.",
                            "edit.horizontal_flip_in_place_selected_object",
                        ),
                        ("Cut selection", "edit.cut"),
                        ("Copy selection", "edit.copy"),
                        ("Move selection", "edit.move_objects"),
                        ("Duplicate selection", "edit.duplicate_objects"),
                    ]
                } else {
                    &[
                        ("Abort command", "view.redraw"),
                        ("Open most recent", "file.open_most_recent"),
                        ("Edit attributes", "properties.edit"),
                        ("Edit attr in editor", "properties.edit_with_editor"),
                        ("Go to upper level", "edit.pop"),
                        ("Insert symbol", "tools.insert_symbol"),
                        ("Insert wire", "tools.insert_wire"),
                        ("Insert line", "tools.insert_line"),
                        ("Insert box", "tools.insert_rect"),
                        ("Insert polygon", "tools.insert_polygon"),
                        ("Insert arc", "tools.insert_arc"),
                        ("Insert circle", "tools.insert_circle"),
                        ("Insert text", "tools.insert_text"),
                        ("Paste clipboard", "edit.paste"),
                    ]
                };
                for (i, (label, cmd)) in rows.iter().enumerate() {
                    let _ = writeln!(
                        t,
                        "node dialog.ctx.row.{i} item\n  parent dialog.ctx\n  label {}\n  command {cmd}\n  style list-row",
                        q(label)
                    );
                }
            }
            Dialog::Layers => {
                for l in 1..USER_LAYERS {
                    let name = LAYER_NAMES.get(usize::from(l)).copied().unwrap_or("");
                    let label = if name.is_empty() {
                        format!("{l:02}")
                    } else {
                        format!("{l:02}-{name}")
                    };
                    let _ = writeln!(
                        t,
                        "node dialog.layers.row.{l} item\n  parent dialog.layers.list\n  label {}\n  command layers.toggle.{l}\n  checked {}\n  style list-row",
                        q(&label),
                        u8::from(o.hidden & (1 << l) == 0)
                    );
                }
            }
            Dialog::File(text) => {
                let _ = writeln!(t, "set dialog.edit_file.text value {}", q_multiline(text));
                let _ = writeln!(t, "set dialog.edit_file.text input field.typed");
            }
            Dialog::Recover(d, n) => {
                let name = self.session.name(*d);
                let _ = writeln!(
                    t,
                    "set dialog.recover.title label {}",
                    q(&format!(
                        "{name}: {n} unsaved edit(s) from a crash can be recovered"
                    ))
                );
            }
            _ => {}
        }
    }
}

/// Quotes a multi-line value: like `q` but keeps line breaks as `\n` escapes.
fn q_multiline(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Every bit of a bus net key `a[3:0]` (`a[3] … a[0]`); other keys as they are.
fn expand_bus(key: &str) -> Vec<String> {
    let parsed = key.strip_suffix(']').and_then(|k| {
        let (base, range) = k.rsplit_once('[')?;
        let (hi, lo) = range.split_once(':')?;
        Some((base, hi.parse::<i64>().ok()?, lo.parse::<i64>().ok()?))
    });
    match parsed {
        Some((base, hi, lo)) if (hi - lo).abs() < 1 << 16 => {
            let step = if hi >= lo { -1 } else { 1 };
            let mut out = Vec::new();
            let mut i = hi;
            loop {
                out.push(format!("{base}[{i}]"));
                if i == lo {
                    break;
                }
                i += step;
            }
            out
        }
        _ => vec![key.to_owned()],
    }
}

/// Instances whose name is used more than once (every holder), and the ops renaming all
/// but the first holder of each name to the next free `<prefix><n>`.
fn duplicate_names(doc: &schemify_doc::Document) -> (Vec<ObjKey>, Vec<Op>) {
    let s = doc.strings();
    let inst = doc.instances();
    let mut by_name: std::collections::BTreeMap<&str, Vec<usize>> =
        std::collections::BTreeMap::new();
    for i in 0..inst.id.len() {
        by_name.entry(s.get(inst.name[i])).or_default().push(i);
    }
    let mut keys = Vec::new();
    let mut ops = Vec::new();
    let mut taken = std::collections::HashSet::new();
    for (name, rows) in by_name.iter().filter(|e| e.1.len() > 1) {
        for (k, &i) in rows.iter().enumerate() {
            keys.push(ObjKey {
                table: TableId::Instances,
                key: inst.id[i].0,
            });
            if k == 0 {
                continue;
            }
            let end = name
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(name.len());
            let prefix = if end == 0 { "X" } else { &name[..end] };
            let new = edit::unique_name(doc, prefix, &mut taken);
            ops.push(Op::SetAttr {
                target: Target::new(RecordKind::Inst, &inst.id[i].text(s)),
                key: "name".into(),
                values: vec![new],
            });
        }
    }
    keys.sort_unstable();
    (keys, ops)
}

/// Instances placed exactly like an earlier instance of the same cell (all but the first of
/// each group).
fn overlapped(doc: &schemify_doc::Document) -> Vec<ObjKey> {
    let inst = doc.instances();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for i in 0..inst.id.len() {
        let place = (inst.cell[i], inst.x[i], inst.y[i], inst.orient[i] as u8);
        if !seen.insert(place) {
            out.push(ObjKey {
                table: TableId::Instances,
                key: inst.id[i].0,
            });
        }
    }
    out.sort_unstable();
    out
}

/// A generated symbol for ports `(name, orientation, position)` (Symbol → Make symbol):
/// pins first (inputs — `R0`/`MR180` ports — on the left, outputs `R180`/`MR0` and inouts
/// on the right, each side top to bottom in the ports' (y, x) order, one per name), then the
/// graphics (a box `width` wide, pin leads, pin names, the `{name}` text). Empty when there
/// are no ports.
fn symbol_records(ports: &[(String, Orient, Pt)], width: i32) -> Vec<RecordBuf> {
    let mut order: Vec<usize> = (0..ports.len()).collect();
    order.sort_by_key(|&i| (ports[i].2.1, ports[i].2.0));
    let mut left: Vec<&str> = Vec::new();
    let mut right: Vec<(&str, &str)> = Vec::new();
    for i in order {
        let name = ports[i].0.as_str();
        if left.contains(&name) || right.iter().any(|r| r.0 == name) {
            continue;
        }
        match ports[i].1 {
            Orient::R0 | Orient::MR180 => left.push(name),
            Orient::R180 | Orient::MR0 => right.push((name, "out")),
            _ => right.push((name, "inout")),
        }
    }
    if left.is_empty() && right.is_empty() {
        return Vec::new();
    }
    let c = |v: i32| {
        let mut t = String::new();
        schemify_format::format_coord(v, &mut t);
        t
    };
    let n = left.len().max(right.len()) as i32;
    let half_w = (width / 2).max(2 * PITCH);
    let lead = PITCH;
    let top = -(n * PITCH) / 2 - PITCH / 2;
    let bottom = top + (n + 1) * PITCH;
    let pin = |name: &str, dir: &str, x: i32, y: i32| {
        RecordBuf::new(RecordKind::Pin)
            .field(name)
            .field(dir)
            .field(&c(x))
            .field(&c(y))
    };
    let label = |name: &str, x: i32, y: i32| {
        RecordBuf::new(RecordKind::Stext)
            .field(&c(x))
            .field(&c(y - 5 * UNIT))
            .field("R0")
            .field(name)
            .attr("size", &["0.2"])
    };
    let mut pins = Vec::with_capacity(left.len() + right.len());
    let mut graphics = vec![
        RecordBuf::new(RecordKind::Rect)
            .field(&c(-half_w + lead))
            .field(&c(top))
            .field(&c(half_w - lead))
            .field(&c(bottom)),
        RecordBuf::new(RecordKind::Stext)
            .field("0")
            .field(&c(top - 15 * UNIT))
            .field("R0")
            .field("{name}")
            .attr("hcenter", &[]),
    ];
    for (k, name) in left.iter().enumerate() {
        let y = top + (k as i32 + 1) * PITCH;
        pins.push(pin(name, "in", -half_w, y));
        graphics.push(edit::shape(false, (-half_w, y), (-half_w + lead, y)));
        graphics.push(label(name, -half_w + lead + 2 * UNIT, y));
    }
    for (k, (name, dir)) in right.iter().enumerate() {
        let y = top + (k as i32 + 1) * PITCH;
        pins.push(pin(name, dir, half_w, y));
        graphics.push(edit::shape(false, (half_w - lead, y), (half_w, y)));
        let w = (name.chars().count() as i32) * 6 * UNIT;
        graphics.push(label(name, half_w - lead - 2 * UNIT - w, y));
    }
    assert_eq!(pins.len(), left.len() + right.len());
    pins.extend(graphics);
    pins
}

/// Symbol → Create symbol pins from selected schematic ports: one pin per selected port
/// whose name has no pin yet, at the port's position (R180/MR0 ports are outputs).
fn pins_from_ports(doc: &schemify_doc::Document, sel: &[ObjKey]) -> Vec<Op> {
    if doc.kind() != FileKind::Shcomp {
        return Vec::new();
    }
    let s = doc.strings();
    let have: Vec<&str> = doc.pins().name.iter().map(|&n| s.get(n)).collect();
    let mut made: Vec<String> = Vec::new();
    let mut ops = Vec::new();
    for (_, _, r) in edit::selected_records(doc, sel) {
        if r.kind != RecordKind::Port {
            continue;
        }
        let name = r.fields[4].clone();
        if have.contains(&name.as_str()) || made.contains(&name) {
            continue;
        }
        let dir = match r.fields[3].as_str() {
            "R180" | "MR0" => "out",
            "R0" | "MR180" => "in",
            _ => "inout",
        };
        let rec = RecordBuf::new(RecordKind::Pin)
            .field(&name)
            .field(dir)
            .field(&r.fields[1])
            .field(&r.fields[2]);
        ops.push(Op::Insert { rec, at: None });
        made.push(name);
    }
    ops
}

/// The Help text tab: the key summary and where the manuals are.
fn help_text() -> String {
    "Schemify help\n\n\
     Keys are xschem's (D25); `/` toggles a key summary over the canvas.\n\n\
     Editing: w wire, l line, r rect, p polygon, Shift+C arc, Ctrl+Shift+C circle, t text,\n\
     Ins insert symbol, m move, Ctrl+M stretch move, Shift+M kissing move, c duplicate,\n\
     Del delete, Shift+R rotate, Shift+F flip, u / Shift+U undo / redo, Ctrl+F search,\n\
     & join/trim wires, Alt+U align to grid, right click: context menu.\n\n\
     View: f zoom full, z zoom box, Shift+Z / Ctrl+Z zoom in / out, % grid, Alt+X crosshair.\n\n\
     Hierarchy: e / Ctrl+E descend / return, i symbol, Alt+E / Alt+I in a new tab.\n\n\
     Files: Ctrl+S save, Ctrl+Shift+S save as, Ctrl+Shift+O most recent, B merge.\n\n\
     Specs: docs/spec/ (file format, expressions, netlisting); decisions: docs/DECISIONS.md.\n"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_point_arcs_pass_through_their_points() {
        // A quarter circle of radius 10 around the origin, clockwise on screen from +x to +y.
        let (cx, cy, r, start, sweep) = arc_3((10_000, 0), (0, 10_000), (6_000, 8_000)).unwrap();
        assert_eq!((cx, cy, r), (0, 0, 10_000));
        assert_eq!((start, sweep), (0, 90_000));
        // The same points with the middle one on the other side: the long way round.
        let (_, _, _, s2, w2) = arc_3((10_000, 0), (0, 10_000), (-10_000, 0)).unwrap();
        assert_eq!((s2, w2), (90_000, 270_000));
        assert!(arc_3((0, 0), (1, 1), (2, 2)).is_none(), "collinear");
    }

    #[test]
    fn shapes_need_extent() {
        assert!(shape_record(Form::Rect, &[(0, 0), (0, 10)], false, 4, Some("abcdef")).is_none());
        let r = shape_record(
            Form::Poly,
            &[(0, 0), (10, 0), (10, 10)],
            true,
            7,
            Some("abcdef"),
        )
        .unwrap();
        assert_eq!(
            r.fields,
            ["abcdef", "poly", "0", "0", "0.01", "0", "0.01", "0.01"]
        );
        assert_eq!(r.find_attr("layer").unwrap().values, ["7"]);
        assert!(r.find_attr("closed").is_some());
        let sym = shape_record(Form::Circle, &[(0, 0), (3_000, 4_000)], false, 4, None).unwrap();
        assert_eq!(sym.kind, RecordKind::Circle);
        assert_eq!(sym.fields, ["0", "0", "5"]);
    }

    #[test]
    fn buses_expand_per_bit() {
        assert_eq!(expand_bus("d[2:0]"), ["d[2]", "d[1]", "d[0]"]);
        assert_eq!(expand_bus("d[0:1]"), ["d[0]", "d[1]"]);
        assert_eq!(expand_bus("vin"), ["vin"]);
    }
}
