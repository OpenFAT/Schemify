//! Starting a session ([`Editor::new`](super::Editor::new)): the project found from the
//! path given, its plugins loaded (D6, D8) with their notes on the status line, and the file
//! given or a new empty schematic opened.

use super::dialog::Dialog;
use super::files::{default_view, project_root};
use super::opts::EditOpts;
use super::opts::user_layer_colors;
use super::probe::DOCK_SPLIT;
use super::tool::Tool;
use super::{Editor, Options, Panel};
use crate::checks::Members;
use crate::commands::Commands;
use crate::docs::Page;
use crate::platform;
use crate::plugins::{self, Host};
use crate::scene::{self, Scene, Symbols};
use crate::session::{Session, View};
use crate::shell::Shell;
#[cfg(not(target_arch = "wasm32"))]
use crate::sim::Sim;
use crate::spines;
use schemify_canvas::{CanvasPrims, Marks, Palette};
use schemify_elab::Annotations;
use schemify_format::{FileKind, IdRng};
use schemify_plugin::abi::Event;
use schemify_ui::Routed;

impl Editor {
    /// Starts a session: loads the project (the directory given, the directory holding the
    /// file given, or the working directory), loads plugins (D6, D8), and opens the file
    /// given or a new empty schematic.
    ///
    /// # Errors
    ///
    /// The project manifest does not parse, or the file given cannot be opened.
    pub fn new(opts: Options) -> Result<Editor, String> {
        let path = opts.path.clone();
        let root = match &path {
            Some(p) if platform::is_dir(p) => p.clone(),
            Some(p) => project_root(p),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };
        let first = path.as_deref().filter(|p| !platform::is_dir(p));
        let mut session = Session::load(&root, opts.seed, first)?;
        let (host, notes) = Host::new(&session.project, &root, opts.no_default_ui, &opts.config);
        let user = host.user();
        let edit_opts = EditOpts {
            snap: user.opts.snap,
            grid: user.opts.grid,
            show_grid: user.opts.show_grid,
            light: user.opts.light,
            dim: user.opts.dim,
            width_tenths: user.opts.line_width_tenths,
            ..EditOpts::default()
        };
        let layer_colors = user_layer_colors(&user.opts);
        // The PDK's pin, else the config's simulator, else detection (run spec §5).
        let (dialect, pin_err) = match spines::sim_dialect(None, &session.project, user) {
            Ok(d) => (d, None),
            Err(e) => (schemify_elab::Dialect::Espice, Some(e)),
        };
        // The user config's simulator and compilers (`sim` rows), the simulator overridable.
        #[cfg(not(target_arch = "wasm32"))]
        let tools = {
            let mut t = spines::tools(user);
            if let Some(s) = &opts.sim {
                t.set_sims(s);
            }
            t
        };
        let mut status = session.take_status();
        for n in notes.iter().chain(&pin_err) {
            push_note(&mut status, n);
        }
        let mut ed = Editor {
            session,
            shell: Shell::new(),
            host,
            commands: Commands::default(),
            symbols: Symbols::default(),
            scene: Scene::default(),
            overlay: CanvasPrims::default(),
            marks: Marks::default(),
            selected: scene::Selected::default(),
            sel_prims: Vec::new(),
            snapshot_due: false,
            snapshot_fresh: false,
            palette: Palette::xschem_dark(),
            sel_color: 0xe0e0_e0ff,
            tool: Tool::Idle,
            opts: edit_opts,
            pointer: (0.0, 0.0),
            mods: 0,
            pan: None,
            space_pan: false,
            rzoom: None,
            last_click: None,
            dialog: Dialog::None,
            clipboard: Vec::new(),
            rng: IdRng::new(opts.seed ^ 0x5eed),
            routed: Routed::default(),
            viewport: (0.0, 0.0),
            canvas_gen: [0; 5],
            waves: None,
            raster: None,
            confirm: None,
            quit: false,
            values_gen: 0,
            scene_values: 0,
            revalued: Vec::new(),
            sel_changed: false,
            sel_moved: false,
            generation: 0,
            symbols_key: None,
            focus_next: None,
            ui_stale: true,
            kind_block: Default::default(),
            cell: None,
            cell_key: None,
            cell_ops: Vec::new(),
            page: Page::default(),
            port_sync: None,
            amend: None,
            parked: Vec::new(),
            undid: false,
            annotations: Annotations::default(),
            highlight: Vec::new(),
            hl: Members::default(),
            checks_key: None,
            decor: CanvasPrims::default(),
            decor_key: None,
            op_gen: 0,
            markers: crate::checks::Markers::default(),
            marker_targets: None,
            hier: None,
            panel: Panel::None,
            blocked: Vec::new(),
            dialect,
            show_netlist: true,
            op_on: true,
            op_job: None,
            #[cfg(not(target_arch = "wasm32"))]
            op_shown: false,
            #[cfg(not(target_arch = "wasm32"))]
            sim: Sim::new(tools),
            graphs: Vec::new(),
            env: spines::env_pairs(),
            now: 0,
            diff: crate::checks::Diff::default(),
            stage_pick: 0,
            diff_key: None,
            dock_focus: false,
            dock_split: DOCK_SPLIT,
            dock_drag: false,
            staged: None,
            results_for: None,
            recent: opts
                .state_dir
                .as_deref()
                .map_or_else(Vec::new, crate::recent::load),
            state_dir: opts.state_dir.clone(),
            system_clipboard: opts.system_clipboard,
            layer_colors,
            pending_delete: None,
            compare: None,
            extra: CanvasPrims::default(),
            shown: None,
            shown_key: None,
            trim_after: None,
            queued: Vec::new(),
            queued_edits: Vec::new(),
            said: Vec::new(),
            kept_log: None,
            next_ticket: 0,
            startup: opts,
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            ed.sim.dialect = dialect;
        }
        let opened = match &path {
            Some(p) if !platform::is_dir(p) => Some(ed.session.open_path(p)?),
            _ => None,
        };
        match opened {
            Some(h) => {
                let view = default_view(ed.session.project.doc(h));
                ed.session.open_tab(h, view);
                let id = plugins::doc_id(h);
                let _ = ed.host.push(None, &Event::DocOpened { doc: id });
                ed.remember(h);
            }
            None => {
                let h = ed.session.create(FileKind::Shcomp)?;
                ed.session.open_tab(h, View::Schematic);
            }
        }
        if !status.is_empty() {
            ed.session.say(status);
        }
        Ok(ed)
    }
}

pub(super) fn push_note(status: &mut String, note: &str) {
    if !status.is_empty() {
        status.push_str("; ");
    }
    status.push_str(note);
}
