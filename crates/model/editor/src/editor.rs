//! The editor: every table of an interactive session and the **frame spine** that
//! transforms them, one line per transform (docs/dev/architecture.md § Spines, `frame`):
//!
//! ```text
//! ui::show(ui tables, egui input) in egui's pass       → command_requests, canvas events, ui_layout
//! app::dispatch(commands, command_requests)            → edit_requests, plugin_events, ui state
//! app::gesture(tool_state, canvas events)              → edit_requests, selection, tool_state
//! plugin::deliver(plugins, plugin_events, snapshots)   → request_queues (edits, logs, io)
//! doc::apply_edits(edit_requests ∪ plugin edits)       → doc tables, journal
//! app::load_symbols(active doc)                        → symbol cache from the library index
//! elab::CellState::recompute(doc, ops, plugin fns)     → values (D19; incremental per edit)
//! docs::cache_ops(values) → doc::apply_edits           → D20 `cached` write-back, same undo
//! elab::Annotations::{replace, patch}(checks, op)      → annotations (per source)
//! run::poll(jobs) → results; optimizer ask/tell        → results table, plots, staged edit
//! app::ui_inputs(plugin sets, docs page, images: I/O)  → shell inputs, status
//! ui::compose(base, plugin sets, state + docs page)    → ui_nodes (changed: egui shows them again)
//! scene::build(doc, symbols)                           → canvas prims, objects (on change)
//! checks::decor(scene, violations, highlight, annots)  → decor prims (on change)
//! canvas hook(CanvasPicture) → canvas slot; plots     → egui output; Editor::painted(took_edits)
//! ```
//!
//! Event-driven: `update` runs when the window delivers input, or on a tick the editor asked
//! for through [`Editor::wake_ms`] while a simulation, a plugin's child process or a plugin
//! timer is pending; with nothing pending it asks for none (D39 idle CPU 0%).
//!
//! Tables owned here (single owner): `selection` (per hierarchy level, in `session`),
//! `tool_state` ([`Tool`]), `edit options` ([`EditOpts`]), the dialog state, the clipboard,
//! the scene and its overlay/marks. Design data is only ever changed through
//! `doc::apply_edits`.
//!
//! Contents:
//! - `pub enum Panel`
//! - `pub struct Options`
//! - `pub struct Editor`, `Editor::wake_ms`, `Editor::update`, `Editor::step`
//! - `pub struct Frame`
//! - `pub(crate) fn has_view`

mod apply;
mod camera;
mod canvas;
mod cells;
mod chooser;
mod cmd_dialog;
mod cmd_edit;
mod cmd_file;
mod cmd_net;
mod cmd_panel;
mod cmd_view;
mod compare;
mod compose;
mod contribute;
mod decor;
mod dialog;
mod dispatch;
mod files;
mod finish;
mod gestures;
mod highlight;
mod image;
mod open;
mod opts;
mod picture;
mod probe;
mod props;
mod queue;
mod results;
mod run;
mod select;
mod subckt;
mod symbol;
mod tables;
mod text;
mod tool;
mod values;

use std::cell::RefCell;
use std::fmt;
use std::path::PathBuf;

use schemify_canvas::{CanvasPrims, Marks, Palette};
use schemify_doc::{Assignment, DocHandle, Document, EditRequest, Op, RecordBuf};
use schemify_elab::{Annotations, CellState, Dialect};
use schemify_format::{FileKind, IdRng};
use schemify_ui::{InputEvents, Routed, egui};

use crate::checks::{Diff, Markers, Members};
use crate::commands::{Cmd, Commands};
use crate::docs::Page;
use crate::plugins::Host;
use crate::scene::{self, ObjKey, Scene, Symbols};
use crate::session::{Session, View};
use crate::shell::Shell;
#[cfg(not(target_arch = "wasm32"))]
use crate::sim::Sim;
use crate::waves::WavesView;
use crate::{cell, platform};

use canvas::ShownKey;
use compare::Compare;
use decor::DecorKey;
use dialog::Dialog;

pub use opts::EditOpts;
pub use picture::{CanvasPicture, Raster};
pub use queue::QUEUED_MAX;
use queue::{Queued, Said};
use schemify_config::Startup;
use schemify_netlist::NameMap;
#[cfg(not(target_arch = "wasm32"))]
use schemify_run::SimCommand;
pub use tool::{Carry, Form, Placement, Tool};
use values::Parked;

/// Which panel the `bottom` container shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    /// None.
    None,
    /// The checks panel (violations of the active level, refused netlists and runs).
    Checks,
    /// The results table of the last run.
    Results,
}

/// Startup options.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Run without the bundled default UI plugin (proves the core stands alone, D6).
    pub no_default_ui: bool,
    /// A project directory or a cell file to open.
    pub path: Option<PathBuf>,
    /// Id-generation seed (tests pin it; the binary uses OS entropy).
    pub seed: u64,
    /// The simulator command line (`None`: the user config's, by default
    /// `ngspice -b -r {raw} {netlist}`, D7/D15). Tests pass a fake simulator.
    #[cfg(not(target_arch = "wasm32"))]
    pub sim: Option<SimCommand>,
    /// Directory of user state (the recent-files list); `None` keeps it in memory only.
    /// `schemify_app::window::run` uses `platform::state_dir()`; tests leave it `None` or pass a temp dir.
    pub state_dir: Option<PathBuf>,
    /// Copy/cut/paste also go through the system clipboard (`platform::clipboard_*`).
    /// `schemify_app::window::run` turns it on; headless tests keep the editor's own clipboard only.
    pub system_clipboard: bool,
    /// Which user config runs (config spec §2). The default is `--clean`, so tests never read
    /// the developer's own `init.lua`; the binary passes its flags.
    pub config: Startup,
}

/// An interactive editing session: project, UI, plugins, scene, tool and dialogs.
pub struct Editor {
    /// Project, open documents, tabs, status line.
    pub(crate) session: Session,
    /// Composed UI and its layout.
    pub(crate) shell: Shell,
    host: Host,
    commands: Commands,
    symbols: Symbols,
    scene: Scene,
    /// The selection's primitive rows (`scene::selected_prims`), with the marks.
    sel_prims: Vec<(u8, u32)>,
    /// The plugin snapshot lags a check pass that came after this frame's delivery: the next
    /// tick refills it (`Editor::wake_ms`), so the next input frame does not.
    snapshot_due: bool,
    /// The snapshot was refilled this frame.
    snapshot_fresh: bool,
    overlay: CanvasPrims,
    marks: Marks,
    /// The active level's selection as scene object indices (rebuilt with the marks).
    selected: scene::Selected,
    palette: Palette,
    sel_color: u32,
    /// The gesture in progress.
    pub(crate) tool: Tool,
    /// Interaction options.
    pub(crate) opts: EditOpts,
    pointer: (f32, f32),
    mods: u8,
    pan: Option<(f32, f32)>,
    space_pan: bool,
    rzoom: Option<(f32, f32)>,
    last_click: Option<(u64, f32, f32)>,
    dialog: Dialog,
    clipboard: Vec<RecordBuf>,
    rng: IdRng,
    /// egui texture updates of headless frames ([`Editor::step`]), handed to the next painter.
    textures: Kept,
    viewport: (f32, f32),
    /// Advance whenever a table the canvas layer draws changes: the scene as shown, decor,
    /// parity decor (`extra`), marks, violation markers.
    canvas_gen: [u64; 5],
    waves: Option<WavesView>,
    /// The PNG export the last command asked for (`schemify-app` rasterizes it).
    raster: Option<Raster>,
    confirm: Option<Cmd>,
    quit: bool,
    /// Advances whenever the value graph is built afresh (every evaluated param may have
    /// changed), and the scene's values key: the generation it was last built or updated
    /// for. A scene behind `values_gen` is built in full; everything else the scene draws
    /// is in its own built key (document, view, edit epoch, symbol epoch).
    values_gen: u32,
    scene_values: u32,
    /// Instances whose evaluated params changed since the scene was last updated (elab's
    /// value delta, `CellState::revalued`): the scene re-stamps them with the edit's own
    /// objects. Bounded by the active document's instances (cleared by every scene pass).
    revalued: Vec<ObjKey>,
    /// The selection marks need rebuilding (a new selection, scene or palette).
    sel_changed: bool,
    /// The selection itself changed since plugins were last told (`SelectionChanged`). Kept
    /// apart from `sel_changed`: a palette refresh after a worker plugin's outputs arrive must
    /// not tell that plugin about a selection change, which would send it an event, whose
    /// outputs refresh the palette again, every frame.
    sel_moved: bool,
    /// Advances on every applied edit of any document (symbol cache freshness).
    generation: u32,
    symbols_key: Option<(DocHandle, u32)>,
    /// Text field to focus once the dialog that holds it is laid out.
    focus_next: Option<String>,
    /// What the UI projects changed outside a frame (a queued command or edit, a text tab, a
    /// scroll offset): the next frame composes before it routes its input. Otherwise the
    /// last frame's composition is what is shown, and routing reads it as it is.
    ui_stale: bool,
    /// The state contribution's block of commands refused by the active file kind, and the
    /// key it was made for (see `state_contribution`); a projection cache.
    kind_block: RefCell<(Option<u64>, String)>,
    /// Derived values of the active level's document (owner: `elab`; D19, D20).
    cell: Option<CellState>,
    /// What `cell` is current for: (document, its edit epoch, plugin composition epoch).
    /// Kept in step by `apply` while edits are incremental; `None` forces a rebuild.
    cell_key: Option<(DocHandle, u32, u32)>,
    /// Ops committed to the active document that `cell` has not seen yet.
    cell_ops: Vec<Op>,
    /// The docs page.
    page: Page,
    /// The cell panel's pending "Sync pins from ports" with the document and edit epoch it
    /// was read at; stale (ignored, then dropped) once either changes.
    port_sync: Option<(DocHandle, u32, cell::PortSync)>,
    /// This frame's docs-page commit (document, label, ops, its journal seq once applied): a
    /// D20 cache write it causes extends the same undo entry (`Edit::Extend`), and its
    /// journal record waits for it (flushed at the end of the elab step).
    amend: Option<(DocHandle, String, Vec<Op>, Option<u64>)>,
    /// States of documents shown before, kept while another level is active (D19: values
    /// for every open document): ops committed to them since are queued; an edit to a cell
    /// they instantiate or a plugin change drops them. At most [`PARKED_MAX`].
    parked: Vec<Parked>,
    /// An undo or redo ran this frame: cache writes wait (a commit would clear redo).
    undid: bool,
    /// The annotation overlay (owner: `elab`, written through `Annotations::replace` and `patch`): sources
    /// `checks` (live violations) and `op` (OP back-annotation).
    annotations: Annotations,
    /// Highlighted nets, keyed across the hierarchy (emitted names, else level paths);
    /// sorted, ≤ `checks::HIGHLIGHT_MAX`.
    highlight: Vec<String>,
    /// What the highlight covers at the active level (rebuilt with the decor).
    hl: Members,
    /// (document, fingerprint) of the violations last posted to the overlay.
    checks_key: Option<(Option<DocHandle>, u64)>,
    /// Decor primitives: highlight, annotation texts, staged diff.
    decor: CanvasPrims,
    /// The decor's inputs besides the scene as it was last built from them (`None`: never).
    decor_key: Option<DecorKey>,
    /// Advances whenever the operating-point overlay is written (the one annotation source
    /// the decor draws).
    op_gen: u32,
    /// The violation markers of the `checks` source, kept per marked object.
    markers: Markers,
    /// Targets of the `checks` marks patched since the markers were placed (`None`: the
    /// overlay was replaced, so every marker is placed anew).
    marker_targets: Option<Vec<String>>,
    /// Name map of the active tab's top cell (ngspice, default request), for hierarchy-wide
    /// net keys, cross-probe names and OP targets: (top document, edit generation, map).
    hier: Option<(DocHandle, u32, NameMap)>,
    /// The panel under the work area.
    panel: Panel,
    /// Blocking violation lines of the last refused netlist or run (checks panel).
    blocked: Vec<String>,
    /// Netlist dialect of `n` / `Shift+N` (D22).
    dialect: Dialect,
    /// Open the netlist in a text tab after netlisting (xschem `Shift+A`).
    show_netlist: bool,
    /// OP back-annotation shown.
    op_on: bool,
    /// The selected job whose OP marks the overlay holds.
    op_job: Option<(u32, u32)>,
    /// Whether the overlay may hold `op` marks (only `step_sim` posts them).
    #[cfg(not(target_arch = "wasm32"))]
    op_shown: bool,
    /// The run state (D15–D18).
    #[cfg(not(target_arch = "wasm32"))]
    sim: Sim,
    /// `(graph node id, plot expression)` of the docs page's filled plot slots.
    graphs: Vec<(String, String)>,
    /// The process environment (netlists, design hash).
    env: Vec<Assignment>,
    /// The clock of the last update.
    now: u64,
    /// The visual diff of the staged edit under review and its key (document, stage id).
    diff: Diff,
    diff_key: Option<(DocHandle, u32)>,
    /// Which of the active document's staged edits is under review (index; clamped).
    stage_pick: usize,
    /// Share of the window height above the waves dock (the divider, dragged by the pointer).
    dock_split: f32,
    /// The staged edit under review: its label and the document with it applied.
    staged: Option<(String, Document)>,
    /// The bench whose saved results were looked up (once per activation).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    results_for: Option<DocHandle>,
    /// Recently opened files, most recent first (≤ `recent::RECENT_MAX`, user state).
    recent: Vec<PathBuf>,
    /// Where `recent` is persisted (`None`: memory only).
    state_dir: Option<PathBuf>,
    /// Copy/paste also use the system clipboard.
    system_clipboard: bool,
    /// Layer color changes (layer, `0xRRGGBB`), applied over the theme's palette.
    layer_colors: Vec<(u8, u32)>,
    /// A cell file picked once for deletion: the same pick again deletes it.
    pending_delete: Option<PathBuf>,
    /// The comparison with another cell (Highlight → compare schematics).
    compare: Option<Compare>,
    /// Parity decor: per-net highlight colors, connected-instance outlines, the
    /// comparison; rebuilt with the decor.
    extra: CanvasPrims,
    /// The scene as shown when instances draw as boxes or without texts (`None`: as built).
    shown: Option<CanvasPrims>,
    /// (scene build, symbol mode) `shown` was made for.
    shown_key: Option<ShownKey>,
    /// Commands queued by [`Editor::command`].
    queued: Vec<Queued>,
    /// Edit requests from outside the frame ([`Editor::request_edits`]), joined to the next
    /// frame's `edit_requests` before dispatch (at most [`QUEUED_MAX`]).
    queued_edits: Vec<EditRequest>,
    /// What each answered command ([`Editor::command_answered`]) said, at most
    /// [`QUEUED_MAX`] waiting to be taken.
    said: Vec<Said>,
    /// Every plugin log line and whether one failed, kept for a headless caller
    /// ([`Editor::keep_plugin_log`]); `None` in the window, which shows only the last line.
    kept_log: Option<(Vec<String>, bool)>,
    /// The next answered command's ticket.
    next_ticket: u32,
    /// A wire edit to follow with join/trim once applied (Options → auto join/trim).
    trim_after: Option<DocHandle>,
    /// The options this session started with: opening a recent file of another project
    /// starts that project's session with them (`files::switch_project`).
    startup: Options,
}

/// What one [`Editor::update`] produced.
pub struct Frame {
    /// egui's output: the shapes to paint (canvas and graph slots filled), texture deltas the
    /// caller must apply or clear, the platform output (cursor, clipboard, IME, accessibility)
    /// and when egui wants the next frame.
    pub output: egui::FullOutput,
    /// Whether any editor table changed (the window repaints regardless; tests read it).
    pub changed: bool,
}

/// Texture updates no painter took yet; dropped unapplied when the editor goes (egui asserts
/// that a delta is applied or cleared).
#[derive(Default)]
struct Kept(egui::TexturesDelta);

/// Bytes an idle tick hashes or encodes for the documents' journals (`Project::idle_step`):
/// ~1 ms of sha256, so the 6 MB of a 100k-object cell take a few dozen ticks after it opens.
const IDLE_STEP_BYTES: usize = 128 << 10;

impl fmt::Debug for Editor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Editor")
            .field("tabs", &self.session.tabs().len())
            .field("tool", &self.tool)
            .finish()
    }
}

impl Editor {
    /// Milliseconds until the editor needs [`Editor::update`] without input: a run in flight
    /// or a plugin's child process or timer pending, or the active document's disk hash still
    /// being taken (a step per tick after it opens). `None` when nothing is pending, so the
    /// window sleeps until input (D39 idle CPU 0%).
    pub fn wake_ms(&self) -> Option<u32> {
        #[cfg(not(target_arch = "wasm32"))]
        let sim = self.sim.wake_ms();
        #[cfg(target_arch = "wasm32")]
        let sim: Option<u32> = None;
        // Events queued for plugins (a `RunDone`, a completion) are delivered on the next tick.
        let queued = self.host.has_work().then_some(1);
        // The plugin snapshot behind a new check pass is refilled on the next tick.
        let snapshot = self.snapshot_due.then_some(0);
        // Work commits leave to idle ticks: disk hashes (a journal's first record), a
        // compaction, a first journal's history.
        let active = self.active().map(|(h, _)| h);
        let hash = self.session.project.idle_pending(active).then_some(0);
        // The browser owes an answer (web: a clipboard read).
        let platform = platform::wake_ms();
        [
            sim,
            self.host.wake_ms(self.now),
            queued,
            snapshot,
            hash,
            platform,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    // ------------------------------------------------------------------ the frame spine

    /// One frame: egui's input in, tables updated, the window's egui output out. `now_ms` is a
    /// monotonic clock (double clicks, waves UI, plugin timers). Input without events is a tick
    /// ([`Editor::wake_ms`]): it polls the run and plugin completions.
    ///
    /// The input's egui frame: the view (`ui::show`) routes the input over what is shown and
    /// the spine runs on its commands and canvas events; `canvas` then paints the canvas slot
    /// from the tables as they are now: it returns the slot's shape and whether it took the
    /// scene's reported edits ([`Editor::painted`]). When a table changed, the output asks for
    /// a repaint, and the next frame's view shows the change (the canvas shows it in this one;
    /// measured 2026-10-31, a second egui pass in the same frame cost 0.2–0.4 ms of every
    /// changed frame at 100k). The caller applies the output's texture deltas.
    ///
    /// # Panics
    ///
    /// If the input's screen rectangle has a negative size.
    pub fn update(
        &mut self,
        input: egui::RawInput,
        now_ms: u64,
        canvas: &mut impl FnMut(&egui::Context, &CanvasPicture<'_>) -> (egui::Shape, bool),
    ) -> Frame {
        self.frame(input, now_ms, canvas, false)
    }

    /// One frame from scripted input with no window (`schemify cmd`, the RPC server's ticks,
    /// tests): `events` in a `viewport`-sized window, the canvas left unpainted and egui's
    /// texture updates kept for the next [`Editor::update`]. Returns whether a table changed.
    pub fn step(&mut self, events: &InputEvents, viewport: (f32, f32), now_ms: u64) -> bool {
        let mut f = self.frame(
            events.raw(viewport),
            now_ms,
            &mut |_, _| (egui::Shape::Noop, false),
            true,
        );
        // No painter here: keep egui's texture updates for the next `update`'s painter.
        self.textures
            .0
            .append(std::mem::take(&mut f.output.textures_delta));
        f.changed
    }

    /// [`Editor::update`], and with `settle` a second egui pass in the same frame when a table
    /// changed (headless callers read the layout it made).
    fn frame(
        &mut self,
        mut input: egui::RawInput,
        now_ms: u64,
        canvas: &mut impl FnMut(&egui::Context, &CanvasPicture<'_>) -> (egui::Shape, bool),
        settle: bool,
    ) -> Frame {
        let viewport = input
            .screen_rect
            .map_or((0.0, 0.0), |r| (r.width(), r.height()));
        assert!(viewport.0 >= 0.0, "a viewport of non-negative width");
        assert!(viewport.1 >= 0.0, "a viewport of non-negative height");
        self.now = now_ms;
        input.time = Some(now_ms as f64 / 1000.0);
        let ctx = self.shell.ctx.clone();
        // The input's frame: the view routes it and the spine runs once on what it routed.
        let rest = egui::RawInput {
            screen_rect: input.screen_rect,
            time: input.time,
            max_texture_side: input.max_texture_side,
            viewports: input.viewports.clone(),
            focused: input.focused,
            ..egui::RawInput::default()
        };
        let (mut changed, mut spined) = (false, false);
        let mut output = ctx.run_ui(input, |ui| {
            let spine = !std::mem::replace(&mut spined, true);
            changed |= self.pass(ui, viewport, now_ms, spine, canvas);
        });
        // A table changed: headless, the view shows the result in a pass of its own (no
        // input), so a caller reads the layout it made; a window draws it next frame.
        if changed && settle {
            let second = ctx.run_ui(rest, |ui| {
                self.pass(ui, viewport, now_ms, false, canvas);
            });
            output.append(second);
        } else if changed {
            ctx.request_repaint();
        }
        // Texture updates of headless frames go first, so this painter has every texture.
        let mut textures = std::mem::take(&mut self.textures.0);
        textures.append(std::mem::take(&mut output.textures_delta));
        output.textures_delta = textures;
        Frame { output, changed }
    }

    /// One egui pass of the frame: the dock and the window's view, the spine on the first pass,
    /// the slots painted on the pass whose output is kept.
    fn pass(
        &mut self,
        ui: &mut egui::Ui,
        viewport: (f32, f32),
        now_ms: u64,
        first: bool,
        canvas: &mut impl FnMut(&egui::Context, &CanvasPicture<'_>) -> (egui::Shape, bool),
    ) -> bool {
        let ctx = ui.ctx().clone();
        let mut changed = false;
        if first {
            // UI inputs (plugin sets, docs page, images) → compose when something changed since
            // the last frame (or the window's size), so input routes over what is shown now.
            let resized = viewport != self.viewport;
            self.viewport = viewport;
            changed |= resized;
            self.ui_step(false, resized);
            self.track_input(&ctx);
        }
        // 0. the waves dock below the editor at `dock_split`, its divider dragged by the pointer;
        //    keys go to the side under the pointer
        let pointer = ctx.pointer_latest_pos();
        let full = ui.max_rect();
        let split = (full.top() + full.height() * self.dock_split).floor();
        let mut dock = None;
        if let Some(w) = self.waves.as_mut() {
            let rect = egui::Rect::from_x_y_ranges(full.x_range(), split..=full.bottom());
            let over = pointer.is_some_and(|p| rect.contains(p));
            let builder = egui::UiBuilder::new()
                .id_salt("schemify.dock")
                .max_rect(rect);
            let mut child = ui.new_child(builder);
            dock = Some((w.view(&mut child, over, first, now_ms), over));
        }
        let keys = dock.is_none_or(|(_, over)| !over);
        // 1. the view: input → command_requests + canvas events
        let mut routed = Routed::default();
        let rest = match dock {
            Some(_) => egui::Rect::from_x_y_ranges(full.x_range(), full.top()..=split),
            None => full,
        };
        let mut top = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("schemify.window")
                .max_rect(rest),
        );
        self.shell.view(&mut top, keys, &mut routed);
        if dock.is_some() {
            self.dock_divider(ui, full, split);
        }
        if first {
            if let Some((frame, _)) = dock {
                changed |= frame.redraw;
                if frame.close {
                    self.waves = None;
                    self.shell.invalidate();
                    changed = true;
                }
            }
            changed |= self.spine(&ctx, &routed, now_ms);
        }
        // Every pass paints the slots (egui reruns a pass a sizing asked to discard); a repeat
        // paint finds the rows and label meshes current and repacks nothing.
        if !ctx.will_discard() {
            self.paint_graphs(&ctx);
            self.paint_canvas(&ctx, canvas);
        }
        changed
    }

    /// Steps 2–9 of the frame spine over the view's output. Returns whether a table changed.
    fn spine(&mut self, ctx: &egui::Context, routed: &Routed, now_ms: u64) -> bool {
        let mut changed = false;
        let snapshot_due = std::mem::take(&mut self.snapshot_due);
        self.snapshot_fresh = false;
        if self.shell.ui.dialog().is_none() {
            self.dialog = Dialog::None; // Escape closed it inside `ui` (cancel)
        }
        let mut reqs: Vec<EditRequest> = std::mem::take(&mut self.queued_edits);
        self.undid = false;
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        changed |= self.sync_page(escape, &routed.commands, &mut reqs);
        // 2. dispatch → edit_requests, plugin_events, ui state
        changed |= self.dispatch_all(&routed.commands, &mut reqs);
        if platform::clipboard_ready() {
            // Web: the browser answered a paste's clipboard read; that paste runs again now.
            self.command(Cmd::Paste.as_str(), "");
        }
        changed |= self.dispatch_queued(&mut reqs);
        for &ev in &routed.canvas {
            changed |= self.gesture(ev, &mut reqs, now_ms);
        }
        // 3–4. plugin completions (child processes, timers) → events; plugin::deliver →
        // request_queues; doc::apply_edits (twice at most, so plugins see this frame's
        // DocChanged and their answers land in the same frame).
        changed |= self.host.poll(now_ms);
        for _ in 0..2 {
            changed |= self.deliver(&mut reqs);
            if reqs.is_empty() {
                break;
            }
            changed |= self.apply(&mut reqs);
        }
        assert!(reqs.is_empty(), "every delivered request was applied");
        changed |= self.auto_trim(&mut reqs);
        // 5. symbols of the active document (I/O); a journal's crash recovery asks first
        self.load_symbols();
        self.prompt_recovery();
        // 6. elab::recompute → values and violations (D13, D19), then the D20 cache
        //    write-back folded into the edit's own undo entry, then the `checks` source of
        //    the annotation overlay; before compose so page, panel and canvas agree.
        let pass_moved = self.recompute_elab();
        if pass_moved {
            changed = true;
            changed |= self.sync_cache(&mut reqs);
            self.post_checks();
        }
        // A docs-page commit's journal record waited for its cache ops (`Edit::Extend`).
        if let Some((h, ..)) = self.amend.take() {
            self.session.flush_journal(h);
        }
        // 7. run::poll → results; optimizer ask/tell; plugin run requests; OP annotation
        #[cfg(not(target_arch = "wasm32"))]
        {
            changed |= self.step_sim(&mut reqs);
        }
        self.track_stage();
        // 8. ui inputs (plugin sets, docs page, prose images: I/O) → ui compose + layout
        //    (a projection) → palette
        changed |= self.ui_step(false, true);
        if let Some(id) = self.focus_next.take() {
            let s = &mut self.shell;
            let sym = s.strings.intern(&id);
            s.ui.focus_text(sym);
        }
        // 9. scene, decor, overlay, marks (auto highlight follows the selection first)
        if self.sel_changed && self.opts.auto_highlight {
            self.auto_highlight();
        }
        let cam0 = self.camera();
        let stale = self.build_scene();
        changed |= stale;
        if self.camera() != cam0 {
            // The first fit runs after compose (it needs the laid-out canvas): compose again
            // so the status line reads the fitted camera this frame, not at the next input.
            self.compose_ui(false);
        }
        changed |= self.build_markers();
        let decor = self.build_decor(stale);
        changed |= decor;
        self.build_shown();
        changed |= self.build_extra(decor || stale);
        self.build_overlay();
        // The plugin snapshot: a pass that moved after this frame's delivery is refilled on
        // the next tick (a frame nobody waits on), unless that tick's pass moves again.
        if snapshot_due && !self.snapshot_fresh && !pass_moved {
            self.snapshot_now();
        }
        self.snapshot_due = !self.host.plugins.ids().is_empty()
            && (pass_moved || snapshot_due && !self.snapshot_fresh);
        // A tick without input does a step of the work commits leave to idle frames (the
        // active document's disk hash first, so its first commit hashes nothing; compactions;
        // a first journal's history), so no commit frame does O(document) work.
        let active = self.active().map(|(h, _)| h);
        if ctx.input(|i| i.events.is_empty()) && self.session.project.idle_pending(active) {
            if let Some(h) = self.session.project.idle_step(active, IDLE_STEP_BYTES) {
                self.session.bump(h); // compacted: the scene and the cell rebuild from it
                changed = true;
            }
            for h in self.session.project.handles().collect::<Vec<_>>() {
                self.session.flush_journal(h); // a journal sent again once its hash landed
            }
        }
        changed
    }

    /// Paints the canvas slot through `canvas` and takes back what it consumed.
    fn paint_canvas(
        &mut self,
        ctx: &egui::Context,
        canvas: &mut impl FnMut(&egui::Context, &CanvasPicture<'_>) -> (egui::Shape, bool),
    ) {
        let Some(slot) = self.shell.canvas_slot() else {
            return;
        };
        let Some(picture) = self.canvas_picture() else {
            return;
        };
        let (shape, took) = canvas(ctx, &picture);
        ctx.layer_painter(slot.layer).set(slot.shape, shape);
        self.painted(took);
    }
}

impl Drop for Kept {
    fn drop(&mut self) {
        self.0.clear();
    }
}

impl fmt::Debug for Kept {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Kept")
            .field("textures", &self.0.set.len())
            .finish()
    }
}

/// Whether a cell of `kind` has the `view` side: a `.shcomp` has both, a `.shprim` only a
/// symbol, a `.shbench` only a schematic; project, lock, config and journal files neither.
pub(crate) fn has_view(kind: FileKind, view: View) -> bool {
    matches!(
        (kind, view),
        (FileKind::Shcomp, _)
            | (FileKind::Shprim, View::Symbol)
            | (FileKind::Shbench, View::Schematic)
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use schemify_doc::TableId;

    use super::*;

    /// Copies the fixture tree `src` to `dst`, leaving out derived `.schemify/` state.
    fn copy_dir(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for e in std::fs::read_dir(src).unwrap() {
            let p = e.unwrap().path();
            if p.file_name().is_some_and(|n| n == ".schemify") {
                continue;
            }
            let to = dst.join(p.file_name().unwrap());
            if p.is_dir() {
                copy_dir(&p, &to);
            } else {
                std::fs::copy(&p, &to).unwrap();
            }
        }
    }

    /// `Editor::unselected` (the Alt-box unselect): its bitset pass over the scene and its
    /// per-key fallback (a selected key the scene does not draw) both give the set
    /// difference of the selection and the boxed objects' keys. Oracle: `BTreeSet`
    /// difference, over seeded random selections and hits.
    #[test]
    fn unselect_is_the_set_difference_on_both_paths() {
        let src = Path::new(concat!(
            env!("SCHEMIFY_ROOT"),
            "/crates/view/app/tests/fixtures/ota"
        ));
        let dir = std::env::temp_dir().join(format!("schemify-unselect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        copy_dir(src, &dir);
        let mut ed = Editor::new(Options {
            path: Some(dir.join("ota.shcomp")),
            no_default_ui: true,
            seed: 3,
            ..Default::default()
        })
        .unwrap();
        ed.step(&InputEvents::default(), (1280.0, 800.0), 1);
        let n = ed.scene.objs.len() as u32;
        assert!(n > 20, "{n} objects");
        let mut rng = 0x9e37_79b9_u64;
        let mut next = |m: u32| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % u64::from(m)) as u32
        };
        let ghost = ObjKey {
            table: TableId::Wires,
            key: u32::MAX,
        };
        for round in 0..64 {
            let mut sel: Vec<ObjKey> = (0..next(n))
                .map(|_| ed.scene.objs.key[next(n) as usize])
                .collect();
            if round % 2 == 1 {
                sel.push(ghost); // not drawn: the fallback path
            }
            ed.set_selection(sel);
            ed.step(&InputEvents::default(), (1280.0, 800.0), 2 + round);
            let hits: Vec<u32> = (0..next(n)).map(|_| next(n)).collect();
            let gone: BTreeSet<ObjKey> = hits
                .iter()
                .map(|&o| ed.scene.objs.key[o as usize])
                .collect();
            let want: Vec<ObjKey> = ed
                .selection()
                .iter()
                .copied()
                .filter(|k| !gone.contains(k))
                .collect();
            let mut got = ed.unselected(&hits);
            got.sort_unstable();
            assert_eq!(got, want, "round {round}");
            assert_eq!(got.contains(&ghost), round % 2 == 1);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `compose_ui` is a projection: run twice over the same tables it gives the same layout,
    /// reads no image file and leaves the status line alone; the frame's `ui_inputs` step is
    /// where a missing prose image is tried once and reported.
    #[test]
    fn compose_is_a_projection_and_the_ui_inputs_step_does_the_io() {
        let src = Path::new(concat!(
            env!("SCHEMIFY_ROOT"),
            "/crates/view/app/tests/fixtures/ota"
        ));
        let dir =
            std::env::temp_dir().join(format!("schemify-compose-pure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        copy_dir(src, &dir);
        let file = dir.join("ota.shcomp");
        let head = "  | # Five-transistor OTA, sized by gm/Id\n";
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(head));
        let with = format!("{head}  |\n  | ![gone](img/missing.png)\n");
        std::fs::write(&file, text.replace(head, &with)).unwrap();
        let mut ed = Editor::new(Options {
            path: Some(file),
            seed: 7,
            ..Default::default()
        })
        .expect("fixture opens");
        assert!(ed.docs_shown(), "the OTA opens beside its docs page");
        ed.viewport = (1280.0, 800.0);
        ed.session.say("before");
        // The first layout gives the canvas its rect, which the status line then reads.
        ed.compose_ui(false);
        ed.compose_ui(false);
        let first = ed.shell.layout.rect().to_vec();
        assert!(!ed.compose_ui(false), "the same tables lay out nothing new");
        assert_eq!(ed.shell.layout.rect(), &first[..]);
        assert_eq!(ed.session.status(), "before", "compose writes no status");
        assert!(ed.page.images_failed.is_empty(), "compose reads no file");
        assert_eq!(
            ed.ui_inputs(),
            compose::UiInputs {
                loaded: false,
                restyle: true
            },
            "nothing loaded; the plugins composed"
        );
        assert_eq!(ed.page.images_failed, ["img/missing.png"]);
        assert!(
            ed.session.status().contains("missing.png"),
            "{}",
            ed.session.status()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
