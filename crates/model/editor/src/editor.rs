//! The editor: every table of an interactive session and the **frame spine** that
//! transforms them, one line per transform (docs/dev/architecture.md § Spines, `frame`):
//!
//! ```text
//! ui::route_input(ui_layout, keymap, events)          → command_requests, canvas events
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
//! ui::compose(base, plugin sets, state + docs page)    → ui_nodes; ui::layout → ui_layout
//! scene::build(doc, symbols)                           → canvas prims, objects (on change)
//! checks::decor(scene, violations, highlight, annots)  → decor prims (on change)
//! (schemify-app) paint(Editor::picture)               → pixels; Editor::painted(took_edits)
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
//! - `pub struct Editor`, `Editor::wake_ms`, `Editor::update`
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

use std::borrow::Cow;
use std::cell::RefCell;
use std::fmt;
use std::path::PathBuf;

use schemify_canvas::{CanvasPrims, Marks, Palette};
use schemify_doc::{DocHandle, Document, EditRequest, Op, RecordBuf};
use schemify_elab::{Annotations, CellState, Dialect};
use schemify_format::{FileKind, IdRng};
use schemify_ui::{InputEvent, InputEvents, Routed};

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
pub use picture::{CanvasPicture, DockPicture, GraphPicture, Picture, Raster, UiView};
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
    routed: Routed,
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
    env: Vec<(String, String)>,
    /// The clock of the last update.
    now: u64,
    /// The visual diff of the staged edit under review and its key (document, stage id).
    diff: Diff,
    diff_key: Option<(DocHandle, u32)>,
    /// Which of the active document's staged edits is under review (index; clamped).
    stage_pick: usize,
    /// Keyboard input goes to the waves dock (the pointer is over it).
    dock_focus: bool,
    /// Share of the window height above the waves dock (the divider, dragged by the pointer).
    dock_split: f32,
    /// The pointer is dragging the dock divider.
    dock_drag: bool,
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

    /// One frame: input in, tables updated. Returns whether the picture changed. `now_ms`
    /// is a monotonic clock (double-click detection, waves UI, plugin timers). An empty
    /// `events` is a tick ([`Editor::wake_ms`]): it polls the run and plugin completions.
    ///
    /// # Panics
    ///
    /// If a `viewport` dimension is negative.
    pub fn update(&mut self, events: &InputEvents, viewport: (f32, f32), now_ms: u64) -> bool {
        assert!(viewport.0 >= 0.0, "a viewport of non-negative width");
        assert!(viewport.1 >= 0.0, "a viewport of non-negative height");
        self.now = now_ms;
        // 0. the waves dock: its share of the window and the input over it
        let mut changed = false;
        let (events, viewport) = match self.waves.is_some() {
            true => {
                let probe::Split {
                    mine,
                    dock,
                    view,
                    dock_view,
                } = self.split_input(events, viewport);
                let w = self.waves.as_mut().expect("checked above");
                let frame = w.update(&dock, dock_view, now_ms);
                changed |= frame.redraw;
                if frame.close {
                    self.waves = None;
                    self.shell.invalidate();
                    changed = true;
                }
                (Cow::Owned(mine), view)
            }
            false => (Cow::Borrowed(events), viewport),
        };
        let events: &InputEvents = &events;
        let snapshot_due = std::mem::take(&mut self.snapshot_due);
        self.snapshot_fresh = false;
        let resized = viewport != self.viewport;
        self.viewport = viewport;
        // UI inputs (plugin sets, docs page, images) → compose + layout when something changed
        // since the last frame's step 8 (or the window's size), so input routes over what is
        // shown now.
        self.ui_step(false, resized);
        let mut routed = std::mem::take(&mut self.routed);
        // 1. input → command_requests + canvas events
        schemify_ui::route_input(
            &self.shell.tables,
            &self.shell.layout,
            &self.shell.strings,
            &mut self.shell.text,
            &mut self.shell.ui,
            events,
            &mut routed,
        );
        self.track_input(events);
        if self.shell.ui.dialog().is_none() {
            self.dialog = Dialog::None; // Escape closed it inside `ui` (cancel)
        }
        changed |= resized || routed.redraw || routed.relayout;
        let mut reqs: Vec<EditRequest> = std::mem::take(&mut self.queued_edits);
        self.undid = false;
        changed |= self.sync_page(events, &routed.commands, &mut reqs);
        // 2. dispatch → edit_requests, plugin_events, ui state
        changed |= self.dispatch_all(&routed.commands, &mut reqs);
        if platform::clipboard_ready() {
            // Web: the browser answered a paste's clipboard read; that paste runs again now.
            self.command(Cmd::Paste.name(), "");
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
        changed |= self.ui_step(routed.relayout, true);
        if let Some(id) = self.focus_next.take() {
            let s = &mut self.shell;
            let sym = s.strings.intern(&id);
            s.ui.focus_text(&s.tables, &s.layout, &s.strings, &mut s.text, sym);
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
        if events.events.is_empty()
            && events.text.is_empty()
            && self.session.project.idle_pending(active)
        {
            if let Some(h) = self.session.project.idle_step(active, IDLE_STEP_BYTES) {
                self.session.bump(h); // compacted: the scene and the cell rebuild from it
                changed = true;
            }
            for h in self.session.project.handles().collect::<Vec<_>>() {
                self.session.flush_journal(h); // a journal sent again once its hash landed
            }
        }
        self.routed = routed;
        changed
    }

    /// Tracks modifiers and, for keys that use the pointer as first point (xschem infix
    /// interface), the pointer position before the first key of the batch.
    fn track_input(&mut self, events: &InputEvents) {
        for e in &events.events {
            match *e {
                InputEvent::Mods { mods } => self.mods = mods.0,
                InputEvent::PointerMove { x, y } => self.pointer = (x, y),
                InputEvent::Key { .. } => break,
                _ => {}
            }
        }
        if let Some(InputEvent::Mods { mods }) = events
            .events
            .iter()
            .rev()
            .find(|e| matches!(e, InputEvent::Mods { .. }))
        {
            self.mods = mods.0;
        }
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
        ed.update(&InputEvents::default(), (1280.0, 800.0), 1);
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
            ed.update(&InputEvents::default(), (1280.0, 800.0), 2 + round);
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
