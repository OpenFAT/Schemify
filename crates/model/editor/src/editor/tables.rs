//! Read-only views of the editor's tables for paint, the app and tests.
//!
//! Contents:
//! - `Editor::session`
//! - `Editor::shell`
//! - `Editor::tool`
//! - `Editor::opts`
//! - `Editor::active`
//! - `Editor::scene`
//! - `Editor::selection`
//! - `Editor::waves_open`
//! - `Editor::cell`
//! - `Editor::page`
//! - `Editor::docs_shown`
//! - `Editor::docs_source`
//! - `Editor::host`
//! - `Editor::commands`
//! - `Editor::open_text_tab`
//! - `Editor::set_scroll`
//! - `Editor::shown`
//! - `Editor::parity_decor`
//! - `Editor::overlay`
//! - `Editor::recent`
//! - `Editor::wants_quit`
//! - `Editor::settle`
//! - `Editor::annotations`
//! - `Editor::highlighted`
//! - `Editor::highlight_keys`
//! - `Editor::decor`
//! - `Editor::markers`
//! - `Editor::panel`
//! - `Editor::sim`
//! - `Editor::waves_traces`
//! - `Editor::dialect`

use std::path::PathBuf;

use schemify_canvas::CanvasPrims;
use schemify_doc::DocHandle;
use schemify_elab::{Annotations, CellState, Dialect};
use schemify_ui::Sym;

use crate::checks::Members;
use crate::commands::Commands;
use crate::docs::Page;
use crate::plugins::Host;
use crate::scene::{ObjKey, Scene};
use crate::session::{Session, TextView, View};
use crate::shell::Shell;
#[cfg(not(target_arch = "wasm32"))]
use crate::sim::Sim;

use super::opts::EditOpts;
use super::tool::Tool;
use super::{Editor, Panel};

impl Editor {
    /// Project, open documents, tabs, status line. Read-only outside the crate: changes go
    /// through [`Editor::command`] or a frame, so the spine shows every write.
    pub fn session(&self) -> &Session {
        &self.session
    }
    /// Composed UI tables and their layout, as of the last frame.
    pub fn shell(&self) -> &Shell {
        &self.shell
    }
    /// The gesture in progress.
    pub fn tool(&self) -> &Tool {
        &self.tool
    }
    /// Interaction options (layers, colours, widths); changed by commands.
    pub fn opts(&self) -> &EditOpts {
        &self.opts
    }
    /// The active document and how it is shown.
    pub fn active(&self) -> Option<(DocHandle, View)> {
        self.session.level().map(|l| (l.doc, l.view))
    }
    /// The scene of the active level (rebuilt by `update` when stale).
    pub fn scene(&self) -> &Scene {
        &self.scene
    }
    /// The active level's selection, sorted.
    pub fn selection(&self) -> &[ObjKey] {
        self.session.level().map_or(&[], |l| &l.sel)
    }
    /// Whether the waveform window is shown.
    pub fn waves_open(&self) -> bool {
        self.waves.is_some()
    }
    /// Derived values of the active level's document; `None` before the first frame or for
    /// a stale handle. Current after every [`Editor::update`].
    pub fn cell(&self) -> Option<&CellState> {
        self.cell.as_ref()
    }
    /// The docs page state (selection, open text editor).
    pub fn page(&self) -> &Page {
        &self.page
    }
    /// Whether the active tab shows its docs page beside the canvas.
    pub fn docs_shown(&self) -> bool {
        self.session
            .tabs()
            .get(self.session.active())
            .is_some_and(|t| t.docs)
    }
    /// Whether the active tab's docs page shows prose blocks as source (`docs.source`).
    pub fn docs_source(&self) -> bool {
        self.session
            .tabs()
            .get(self.session.active())
            .is_some_and(|t| t.docs_source)
    }
    /// The plugin host (loaded plugins, D20 function registry).
    pub fn host(&self) -> &Host {
        &self.host
    }
    /// The command table: core commands plus the composed plugin rows (current after the
    /// first [`Editor::update`]). What a menu row, key or JSON-RPC call names resolves here.
    pub fn commands(&self) -> &Commands {
        &self.commands
    }
    /// Shows `view` in a read-only text tab (the request a netlist or report command makes;
    /// a tab already showing the same file is replaced). Drawn from the next frame.
    ///
    /// # Panics
    ///
    /// Never: the assert checks that at most one tab opened.
    pub fn open_text_tab(&mut self, view: TextView) {
        let tabs = self.session.tabs().len();
        self.session.open_text_tab(view);
        self.ui_stale = true;
        assert!(self.session.tabs().len() <= tabs + 1, "one tab at most");
    }
    /// Sets the scroll offset of the UI node named `id` (as a wheel or scrollbar drag
    /// would), clamped to `[0, max]`; layout reads it on the next frame.
    ///
    /// # Panics
    ///
    /// If `offset` or `max` is NaN.
    pub fn set_scroll(&mut self, id: Sym, offset: f32, max: f32) {
        assert!(
            !offset.is_nan() && !max.is_nan(),
            "scroll offsets are numbers"
        );
        self.shell.ui.set_scroll(id, offset, max);
        self.ui_stale = true;
        debug_assert!(self.shell.ui.scroll_of(id) <= max.max(0.0));
    }
    /// The scene as the canvas shows it: instances as boxes or without their texts when the
    /// Symbol / View options say so, else the built scene itself.
    pub fn shown(&self) -> &CanvasPrims {
        self.shown.as_ref().unwrap_or(&self.scene.prims)
    }
    /// The parity decor of the last frame: per-net highlight colors, outlines of instances
    /// on highlighted nets, the comparison with another cell.
    pub fn parity_decor(&self) -> &CanvasPrims {
        &self.extra
    }
    /// The overlay primitives of the last frame (rubber bands, previews, crosshair, snap
    /// cursor, grid axes).
    pub fn overlay(&self) -> &CanvasPrims {
        &self.overlay
    }
    /// The recent-files list, most recent first.
    pub fn recent(&self) -> &[PathBuf] {
        &self.recent
    }
    /// Whether the user asked to quit (the window shell exits the process).
    pub fn wants_quit(&self) -> bool {
        self.quit
    }
    /// Finishes the idle work every journal waits for (disk hash, compaction, first
    /// encoding) and writes it, before the process leaves: Ctrl+Q and the window's close
    /// button. A journal then names its file's hash, so a restart offers its edits against
    /// the file. Cost: O(open files' bytes).
    pub fn settle(&mut self) {
        let docs: Vec<DocHandle> = self.session.project.handles().collect();
        for d in docs {
            self.session.project.settle_journal(d);
            self.session.flush_journal(d);
        }
    }
    /// The annotation overlay (sources `checks` and `op`).
    pub fn annotations(&self) -> &Annotations {
        &self.annotations
    }
    /// What the highlight covers at the active level (wires, labels, ports, instance pins).
    pub fn highlighted(&self) -> &Members {
        &self.hl
    }
    /// The highlighted net keys (emitted hierarchical names, lowercase for SPICE).
    pub fn highlight_keys(&self) -> &[String] {
        &self.highlight
    }
    /// The decor primitives drawn over the scene (highlight, markers, annotations, diff).
    pub fn decor(&self) -> &CanvasPrims {
        &self.decor
    }
    /// The violation markers drawn on the canvas (rects and codes; dead rows are an update's
    /// removals, on `LAYER_DEAD`).
    pub fn markers(&self) -> &CanvasPrims {
        &self.markers.prims
    }
    /// The panel under the work area.
    pub fn panel(&self) -> Panel {
        self.panel
    }
    /// The run state.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn sim(&self) -> &Sim {
        &self.sim
    }
    /// The source of every trace in the waves dock (empty when closed).
    pub fn waves_traces(&self) -> Vec<String> {
        self.waves.as_ref().map_or_else(Vec::new, |w| {
            w.traces().iter().map(|t| (*t).to_owned()).collect()
        })
    }
    /// The netlist dialect of `n` / `Shift+N`.
    pub fn dialect(&self) -> Dialect {
        self.dialect
    }
}
