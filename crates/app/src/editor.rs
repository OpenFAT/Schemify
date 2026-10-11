//! The editor: every table of an interactive session and the **frame spine** that
//! transforms them, one line per transform (ARCHITECTURE.md § Spines, `frame`):
//!
//! ```text
//! ui::route_input(ui_layout, keymap, events)          → command_requests, canvas events
//! app::dispatch(commands, command_requests)            → edit_requests, plugin_events, ui state
//! app::gesture(tool_state, canvas events)              → edit_requests, selection, tool_state
//! plugin::deliver(plugins, plugin_events, snapshots)   → request_queues (edits, logs, io)
//! doc::apply_edits(edit_requests ∪ plugin edits)       → doc tables, journal
//! app::load_symbols(active doc)                        → open symbol cells (I/O)
//! elab::CellState::recompute(doc, ops, plugin fns)     → values (D19; incremental per edit)
//! docs::cache_ops(values) → doc::apply_edits           → D20 `cached` write-back, same undo
//! elab::apply_annotations(checks, op)                  → annotations (replace per source)
//! run::poll(jobs) → results; optimizer ask/tell        → results table, plots, staged edit
//! ui::compose(base, plugin sets, state + docs page)    → ui_nodes; ui::layout → ui_layout
//! scene::build(doc, symbols)                           → canvas prims, objects (on change)
//! checks::decor(scene, violations, highlight, annots)  → decor prims (on change)
//! render::draw_ui / draw_canvas / waves::draw_pane     → scene (in `paint`)
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

use std::path::{Path, PathBuf};

use schemify_doc::{
    DocHandle, Document, Edit, EditRequest, External, Mode, Op, Outcome, RecordBuf, TableId,
    Transaction, apply_edits, ops_from_text,
};
use schemify_elab::{Annotations, CellState, Dialect};
use schemify_expr::Value;
use schemify_format::{FileKind, IdRng, Orient, RecordKind};
use schemify_plugin::abi::Event;
use schemify_render::canvas::{
    self, Camera, CanvasCache, CanvasDiff, CanvasPrims, CanvasScratch, Marks, Palette,
};
use schemify_render::vello::kurbo::Rect as KRect;
use schemify_render::widgets::{UiScratch, draw_ui};
use schemify_render::{App, Painter};
use schemify_ui::{
    Button, CommandRequests, InputEvent, InputEvents, Layer, Mods, Routed, route_input,
};

use crate::checks::Members;
use crate::commands::{Cmd, Commands, Target};
use crate::docs::{self, Page};
use crate::edit::{self, Ids};
use crate::geometry::{Pt, Xform, snap};
use crate::platform;
use crate::plugins::{self, Host};
use crate::scene::{self, LAYER_SEL, ObjKey, Scene, Symbols, UNIT};
use crate::session::{Session, View};
use crate::shell::Shell;
#[cfg(not(target_arch = "wasm32"))]
use crate::sim::Sim;
use crate::spines;
use crate::waves::{Shifted, WavesView};

mod gestures;
mod integrate;
mod parity;
mod subckt;
use integrate::text_tab_cmd;

/// Pointer travel (logical px) before a press becomes a drag (xschem: 8 px).
const DRAG_PX: f32 = 8.0;
/// Hit-test tolerance in logical px.
const HIT_PX: f64 = 5.0;
/// Double-click window.
const DOUBLE_CLICK_MS: u64 = 400;
/// Zoom factor per wheel notch and per zoom key (xschem: 1.2).
const ZOOM_STEP: f64 = 1.2;
/// A zoom counts as finished this long after the camera scale last changed; until then the
/// canvas shows its recorded layer stretched (D39: a zoom frame re-encodes nothing).
const ZOOM_SETTLE_MS: u32 = 120;
/// Chooser rows listed (the list scrolls; typing filters). Every row is a composed node, so
/// the cap bounds the per-frame compose cost while a chooser is open.
// ponytail: a fixed cap; a virtualized list (rows composed for the visible window only) when
// libraries list more cells than this.
const CHOOSER_ROWS: usize = 512;
/// Commands [`Editor::command`] queues between frames.
pub const QUEUED_MAX: usize = 256;
/// Edit requests one frame may carry.
const REQUESTS_MAX: usize = 1 << 10;
/// Commits whose ops the editor keeps to recompute an undo or redo of them incrementally.
// ponytail: older undos rebuild the derived state (136 ms at 100k objects); doc reporting an
// undo's touched records would retire this mirror.
const COMMITS_KEPT: usize = 64;
/// Cell states kept for documents that are not active ([`Editor`]'s `parked`).
const PARKED_MAX: usize = 8;

/// The [`CellState`] of a document that is not active, with the doc epoch it is current for
/// (after `ops`), the plugin composition epoch, and the ops committed to it since.
#[derive(Debug)]
struct Parked {
    doc: DocHandle,
    key: (u32, u32),
    state: CellState,
    ops: Vec<Op>,
}
/// Share of the window height the editor keeps when the waves dock opens.
const DOCK_SPLIT: f32 = 0.6;
/// Range the dock divider can be dragged in, as shares of the window height.
const DOCK_SPLIT_RANGE: (f32, f32) = (0.15, 0.85);
/// Logical px either side of the dock divider that grab it.
const DOCK_GRAB_PX: f32 = 4.0;
/// Netlist lines a text tab shows (the prose widget's source cap); the file has them all.
const TEXT_TAB_LINES: usize = 1 << 14;

/// What a displayed scene (`shown`) was made for: (scene build key, symbol mode, symbol texts).
type ShownKey = ((DocHandle, View, u32, u32), u8, bool);

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
    pub sim: Option<schemify_run::SimCommand>,
    /// Directory of user state (the recent-files list); `None` keeps it in memory only.
    /// [`run`] uses `platform::state_dir()`; tests leave it `None` or pass a temp dir.
    pub state_dir: Option<PathBuf>,
    /// Copy/cut/paste also go through the system clipboard (`platform::clipboard_*`).
    /// [`run`] turns it on; headless tests keep the editor's own clipboard only.
    pub system_clipboard: bool,
    /// Which user config runs (config spec §2). The default is `--clean`, so tests never read
    /// the developer's own `init.lua`; the binary passes its flags.
    pub config: schemify_config::Startup,
}

/// Interaction options (xschem Options/View state).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditOpts {
    /// Snap step in milli-units (xschem `cadsnap`, default 10 units).
    pub snap: i32,
    /// Grid step in milli-units (display only, default 20 units).
    pub grid: i32,
    /// Grid shown.
    pub show_grid: bool,
    /// Light canvas colors.
    pub light: bool,
    /// Manhattan mode while drawing/moving: 0 straight, 1 horizontal first, 2 vertical
    /// first (Space cycles).
    pub manhattan: u8,
    /// Axis constraint: 0 none, 1 horizontal (`h`), 2 vertical (`v`).
    pub constrain: u8,
    /// Pick the Manhattan mode from the dominant axis (Shift+L).
    pub orthogonal: bool,
    /// Plain moves stretch attached wires (xschem `enable_stretch`, `y`).
    pub stretch: bool,
    /// Wire and line modes continue from each committed segment.
    pub persistent: bool,
    /// Join/trim wires after every wire edit.
    pub auto_trim: bool,
    /// Crosshair at the snapped pointer.
    pub crosshair: bool,
    /// Crosshair half-size in px; 0 = across the canvas.
    pub crosshair_px: u16,
    /// Diamond on the connection point nearest the pointer.
    pub snap_cursor: bool,
    /// The x = 0 / y = 0 axes.
    pub grid_axes: bool,
    /// Current drawing layer of lines, rects, polygons and arcs, `[0, 21]`.
    pub layer: u8,
    /// Hidden canvas layers (bit `l`).
    pub hidden: u32,
    /// Filled layers drawn filled.
    pub fill: bool,
    /// Line width in tenths of a pixel (10 = 1 px), `[1, 100]`.
    pub width_tenths: u8,
    /// Line width follows the zoom.
    pub variable_width: bool,
    /// Dimming toward the background, percent `[0, 100]`.
    pub dim: u8,
    /// Texts inside symbol instances drawn.
    pub symbol_text: bool,
    /// Instances: 0 symbol graphics, 1 boxes for schematic cells, 2 boxes for all.
    pub show_symbols: u8,
    /// Exports use layer colors (else black on white).
    pub export_color: bool,
    /// SVG exports have no background rectangle.
    pub export_transparent: bool,
    /// Netlists are flat (SPICE dialects).
    pub flat: bool,
    /// Every selection change highlights the selection's nets.
    pub auto_highlight: bool,
    /// Instances with a pin on a highlighted net are outlined.
    pub highlight_insts: bool,
    /// Everything but the highlight is dimmed.
    pub only_probes: bool,
    /// Each highlighted net gets its own color.
    pub incr_color: bool,
    /// The nets plotted in the waves dock are highlighted.
    pub highlight_plotted: bool,
    /// The toolbar is shown.
    pub toolbar: bool,
    /// Canvas-only view (menus, toolbar, tabs and status hidden).
    pub fullscreen: bool,
    /// Box width make-symbol draws, milli-units.
    pub symbol_width: i32,
    /// Paper PDF and EPS exports are fitted to.
    pub export_page: crate::export::Page,
    /// PNG export scale, percent of screen resolution (100 = one image pixel per logical
    /// pixel, 96 dpi), `[10, 1600]`.
    pub png_scale: u16,
}

impl Default for EditOpts {
    fn default() -> Self {
        EditOpts {
            snap: 10 * UNIT,
            grid: 20 * UNIT,
            show_grid: true,
            light: false,
            manhattan: 0,
            constrain: 0,
            orthogonal: false,
            stretch: false,
            persistent: false,
            auto_trim: false,
            crosshair: false,
            crosshair_px: 0,
            snap_cursor: false,
            grid_axes: false,
            layer: 4,
            hidden: 0,
            fill: true,
            width_tenths: 10,
            variable_width: false,
            dim: 0,
            symbol_text: true,
            show_symbols: 0,
            export_color: true,
            export_transparent: false,
            flat: false,
            auto_highlight: false,
            highlight_insts: false,
            only_probes: false,
            incr_color: false,
            highlight_plotted: false,
            toolbar: true,
            fullscreen: false,
            symbol_width: 150 * UNIT,
            export_page: crate::export::Page::default(),
            png_scale: 100,
        }
    }
}

/// What a placement attaches to the pointer.
#[derive(Clone, Debug, PartialEq)]
pub enum Placement {
    /// An instance of a cell reference.
    Inst(String),
    /// A net label.
    Label(String),
    /// A port (`true` = output).
    Port(String, bool),
    /// An annotation text.
    Text(String),
    /// A symbol pin (symbol view).
    SymPin(String),
    /// Clipboard records, relative to the pointer.
    Paste(Vec<RecordBuf>),
}

/// The `tool_state` table: one row, the gesture in progress.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Tool {
    /// Nothing in progress.
    #[default]
    Idle,
    /// Left button down at `at` (screen) over object `hit`; becomes a drag past 8 px.
    Press {
        /// Screen position of the press.
        at: (f32, f32),
        /// Snapped world position of the press.
        world: Pt,
        /// Object under the press.
        hit: Option<u32>,
    },
    /// Box selection from `from` to `to` (world, unsnapped).
    Box {
        /// First corner.
        from: (f64, f64),
        /// Current corner.
        to: (f64, f64),
        /// Shift: add to the selection.
        additive: bool,
        /// Alt: remove from the selection.
        unselect: bool,
        /// Ctrl (a plain drag with Options → enable stretch): wires crossing the box edge get
        /// only their inside end selected (`gestures::box_partial`).
        partial: bool,
    },
    /// Moving or copying the selection from `from` to `to`, transformed by `lin` (rotations
    /// and flips about `from` made in flight).
    Move {
        /// Grab point.
        from: Pt,
        /// Current point.
        to: Pt,
        /// Rotation/flip about `from`.
        lin: Xform,
        /// Copy instead of move.
        copy: bool,
        /// Ends on button release (a drag) instead of on click.
        drag: bool,
        /// 0 plain move, 1 stretch attached wires, 2 stretch and add "kissing" wires.
        stretch: u8,
    },
    /// Drawing a wire from `from` to `to`.
    Wire {
        /// Start point.
        from: Pt,
        /// Current end.
        to: Pt,
        /// Ends on release.
        drag: bool,
    },
    /// Drawing a graphic (symbol graphics in the symbol view, `shape` records in a
    /// schematic): the points clicked so far and the pointer.
    Shape {
        /// What is drawn.
        form: Form,
        /// Points clicked so far (the first point is where the tool started).
        pts: Vec<Pt>,
        /// Current pointer position.
        to: Pt,
    },
    /// `D`: the next press starts a box that removes objects from the selection.
    UnselectArmed,
    /// An object attached to the pointer; a click drops it.
    Place {
        /// What.
        what: Placement,
        /// Current position.
        at: Pt,
        /// Rotation/flip made in flight.
        lin: Xform,
    },
    /// Dragging vertex `index` of the one selected wire, line, rectangle or polygon
    /// (`gestures::vertex_press`); ends on release.
    Vertex {
        /// The object.
        key: ObjKey,
        /// Its vertex (`gestures::vertices` order).
        index: u32,
        /// Where the vertex was.
        from: Pt,
        /// Current (snapped) position.
        to: Pt,
    },
    /// `z`: the next press starts a zoom box.
    ZoomArmed,
    /// Zoom box from `from` to `to` (world).
    Zoom {
        /// First corner.
        from: (f64, f64),
        /// Current corner.
        to: (f64, f64),
    },
}

/// What a shape tool draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// Two points.
    Line,
    /// Two corners.
    Rect,
    /// Vertices until closed (Enter, or a click on the first vertex).
    Poly,
    /// Three points: start, end, a point on the arc.
    Arc,
    /// Center and a point on the circle.
    Circle,
}

impl Form {
    /// Points that complete the shape (`None`: open-ended polygon).
    fn points(self) -> Option<usize> {
        match self {
            Form::Line | Form::Rect | Form::Circle => Some(2),
            Form::Arc => Some(3),
            Form::Poly => None,
        }
    }
}

#[derive(Clone, Debug)]
enum Pick {
    Cell(usize),
    File(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChooseFor {
    Insert,
    Open,
    Waves,
    /// Insert another cell's schematic at the pointer (File → Merge).
    Merge,
    /// Delete a cell file (picked twice to confirm).
    Delete,
    /// The cell to compare the active document with.
    Compare,
}

#[derive(Clone, Debug)]
struct Chooser {
    purpose: ChooseFor,
    filter: String,
    rows: Vec<(String, Pick)>,
    /// Analysis a waves chooser was opened for (`waves.load.<a>`), shown in its title.
    note: String,
}

#[derive(Clone, Debug)]
struct Field {
    key: String,
    label: String,
    value: String,
    original: String,
    editable: bool,
}

/// What a fields dialog (the `dialog.props` node) edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PropsFor {
    /// One object's properties.
    Object(ObjKey),
    /// The header attributes (Properties → Edit Header).
    Header,
    /// Tools → Search: field, value, replacement, exact.
    Search,
}

#[derive(Clone, Debug)]
struct Props {
    of: PropsFor,
    title: String,
    fields: Vec<Field>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextFor {
    Label,
    Port(bool),
    Text,
    SymPin,
    /// File → Save As (`true`: as symbol).
    SaveAs(bool),
    /// A one-number setting: snap, grid, line width, crosshair, symbol width, dim.
    Setting(Cmd),
    /// Layer color `rrggbb` of the current layer.
    LayerColor,
}

#[derive(Clone, Debug, Default)]
enum Dialog {
    #[default]
    None,
    Choose(Chooser),
    Props(Props),
    /// Purpose, title, typed text (follows the typing: `field.typed`).
    Text(TextFor, String, String),
    /// The canvas context menu (right click); `true` when something was selected.
    Ctx(bool),
    /// View → Visible layers.
    Layers,
    /// Properties → Edit file: the document's text (follows the typing: `field.typed`).
    File(String),
    /// Crash recovery of a document: (document, journal records offered).
    Recover(DocHandle, u32),
}

/// An interactive editing session: project, UI, plugins, scene, tool and dialogs.
pub struct Editor {
    /// Project, open documents, tabs, status line.
    pub session: Session,
    /// Composed UI and its layout.
    pub shell: Shell,
    host: Host,
    commands: Commands,
    symbols: Symbols,
    scene: Scene,
    overlay: CanvasPrims,
    marks: Marks,
    selected: Vec<bool>,
    palette: Palette,
    sel_color: u32,
    /// The gesture in progress.
    pub tool: Tool,
    /// Interaction options.
    pub opts: EditOpts,
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
    /// Label glyphs and path buffers of every canvas table (scene, decor, overlay).
    canvas_scratch: CanvasScratch,
    /// The canvas's raster layer (D39: pan/zoom frames reuse it instead of re-encoding).
    canvas_cache: CanvasCache,
    /// Advance whenever a table the canvas layer draws changes: the scene as shown, decor,
    /// parity decor (`extra`), marks.
    canvas_gen: [u64; 4],
    /// What changed in those tables since the last painted frame (an edit patches only
    /// there): the diff's snapshot, the `canvas_gen` and style key it was taken at, and the
    /// changed boxes (`None` = unknown).
    canvas_diff: (CanvasDiff, [u64; 4], u64, Option<Vec<[i32; 4]>>),
    /// Clock (ms) until which the zoom counts as changing: frames before it stretch the
    /// recorded canvas layer instead of re-recording it.
    zoom_until: u64,
    /// The last frame showed a stretched layer: a crisp frame is due at `zoom_until`.
    stretched: bool,
    ui_scratch: UiScratch,
    waves: Option<WavesView>,
    confirm: Option<Cmd>,
    quit: bool,
    scene_dirty: bool,
    sel_changed: bool,
    /// Advances on every applied edit of any document (symbol cache freshness).
    generation: u32,
    symbols_key: Option<(DocHandle, u32)>,
    /// Text field to focus once the dialog that holds it is laid out.
    focus_next: Option<String>,
    /// Derived values of the active level's document (owner: `elab`; D19, D20).
    cell: Option<CellState>,
    /// What `cell` is current for: (document, its edit epoch, plugin composition epoch).
    /// Kept in step by `apply` while edits are incremental; `None` forces a rebuild.
    cell_key: Option<(DocHandle, u32, u32)>,
    /// Ops committed to the active document that `cell` has not seen yet.
    cell_ops: Vec<Op>,
    /// The last [`COMMITS_KEPT`] commits' (document, journal seq, ops): undoing or redoing
    /// one touches the records its ops name, so `cell` recomputes over those ops instead
    /// of rebuilding (`doc` reports only the seq).
    commits: std::collections::VecDeque<(DocHandle, u64, Vec<Op>)>,
    /// The docs page.
    page: Page,
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
    /// The annotation overlay (owner: `elab`, written through `Annotations::apply`): sources
    /// `checks` (live violations) and `op` (OP back-annotation).
    annotations: Annotations,
    /// Highlighted nets, keyed across the hierarchy (emitted names, else level paths);
    /// sorted, ≤ `checks::HIGHLIGHT_MAX`.
    highlight: Vec<String>,
    /// What the highlight covers at the active level (rebuilt with the decor).
    hl: Members,
    /// (document, fingerprint) of the violations last posted to the overlay.
    checks_key: Option<(Option<DocHandle>, u64)>,
    /// Decor primitives: highlight, violation markers, annotation texts, staged diff.
    decor: CanvasPrims,
    decor_dirty: bool,
    /// Name map of the active tab's top cell (ngspice, default request), for hierarchy-wide
    /// net keys, cross-probe names and OP targets: (top document, edit generation, map).
    hier: Option<(DocHandle, u32, schemify_netlist::NameMap)>,
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
    /// Objects the active document's staged edit changes (visual diff) and its key.
    diff: Vec<ObjKey>,
    diff_key: Option<(DocHandle, u32)>,
    /// Keyboard input goes to the waves dock (the pointer is over it).
    dock_focus: bool,
    /// Share of the window height above the waves dock (the divider, dragged by the pointer).
    dock_split: f32,
    /// The pointer is dragging the dock divider.
    dock_drag: bool,
    /// The active document's first staged edit: its label and the document with it applied.
    staged: Option<(String, Document)>,
    /// Embedded graph chrome and buffers (docs plot slots).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    plot_look: schemify_render::waves::WaveLook,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    plot_scratch: schemify_render::waves::WaveScratch,
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
    compare: Option<parity::Compare>,
    /// Parity decor: per-net highlight colors, connected-instance outlines, the
    /// comparison; rebuilt with the decor.
    extra: CanvasPrims,
    /// The scene as shown when instances draw as boxes or without texts (`None`: as built).
    shown: Option<CanvasPrims>,
    /// (scene build, symbol mode) `shown` was made for.
    shown_key: Option<ShownKey>,
    /// Commands queued by [`Editor::command`]: (name, argument).
    queued: Vec<(String, String)>,
    /// A wire edit to follow with join/trim once applied (Options → auto join/trim).
    trim_after: Option<DocHandle>,
    /// The options this session started with: opening a recent file of another project
    /// starts that project's session with them (`parity::switch_project`).
    startup: Options,
}

impl std::fmt::Debug for Editor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Editor")
            .field("tabs", &self.session.tabs.len())
            .field("tool", &self.tool)
            .finish()
    }
}

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
        let mut session = Session::load(&root, opts.seed)?;
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
        let layer_colors = parity::user_layer_colors(&user.opts);
        #[cfg(not(target_arch = "wasm32"))]
        let sim_cmd = opts.sim.clone().or_else(|| {
            user.sim(schemify_config::SimKind::Ngspice)
                .map(|s| schemify_run::SimCommand {
                    path: s.path.clone(),
                    args: s.args.clone(),
                })
        });
        let mut status = std::mem::take(&mut session.status);
        for n in notes {
            push_note(&mut status, &n);
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
            selected: Vec::new(),
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
            canvas_scratch: CanvasScratch::default(),
            canvas_cache: CanvasCache::default(),
            canvas_gen: [0; 4],
            canvas_diff: (CanvasDiff::default(), [0; 4], 0, None),
            zoom_until: 0,
            stretched: false,
            ui_scratch: UiScratch::default(),
            waves: None,
            confirm: None,
            quit: false,
            scene_dirty: true,
            sel_changed: false,
            generation: 0,
            symbols_key: None,
            focus_next: None,
            cell: None,
            cell_key: None,
            cell_ops: Vec::new(),
            commits: std::collections::VecDeque::new(),
            page: Page::default(),
            amend: None,
            parked: Vec::new(),
            undid: false,
            annotations: Annotations::default(),
            highlight: Vec::new(),
            hl: Members::default(),
            checks_key: None,
            decor: CanvasPrims::default(),
            decor_dirty: true,
            hier: None,
            panel: Panel::None,
            blocked: Vec::new(),
            dialect: Dialect::Ngspice,
            show_netlist: true,
            op_on: true,
            op_job: None,
            #[cfg(not(target_arch = "wasm32"))]
            op_shown: false,
            #[cfg(not(target_arch = "wasm32"))]
            sim: Sim::new(sim_cmd),
            graphs: Vec::new(),
            env: spines::env_pairs(),
            now: 0,
            diff: Vec::new(),
            diff_key: None,
            dock_focus: false,
            dock_split: DOCK_SPLIT,
            dock_drag: false,
            staged: None,
            plot_look: schemify_render::waves::WaveLook::default(),
            plot_scratch: schemify_render::waves::WaveScratch::default(),
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
            startup: opts,
        };
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
        // The user config's `sim openvaf` / `sim iverilog` programs, as the CLI runner gets
        // them (`spines::tools`; config spec §3).
        #[cfg(not(target_arch = "wasm32"))]
        {
            let tools = spines::tools(ed.host.user());
            ed.sim.set_compilers(tools.openvaf, tools.iverilog);
        }
        if !status.is_empty() {
            ed.session.status = status;
        }
        Ok(ed)
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

    /// The canvas rectangle in logical px.
    pub fn canvas_rect(&self) -> KRect {
        let r = self.shell.canvas_rect(self.viewport);
        KRect::new(
            f64::from(r.x),
            f64::from(r.y),
            f64::from(r.x + r.w),
            f64::from(r.y + r.h),
        )
    }

    /// The active level's camera (fitted on first build).
    pub fn camera(&self) -> Camera {
        self.session.level().and_then(|l| l.cam).unwrap_or(Camera {
            cx: 0.0,
            cy: 0.0,
            scale: 0.01,
        })
    }

    fn set_camera(&mut self, cam: Camera) {
        if let Some(l) = self.session.level_mut() {
            l.cam = Some(cam);
        }
    }

    /// Screen position (logical px) of a world point, for scripted input.
    pub fn to_screen(&self, p: Pt) -> (f32, f32) {
        let (x, y) = self.camera().to_screen(self.canvas_rect(), p.0, p.1);
        (x as f32, y as f32)
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
            .tabs
            .get(self.session.active)
            .is_some_and(|t| t.docs)
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

    /// Queues command `name` with argument `arg` for the next [`Editor::update`], dispatched
    /// after that frame's input exactly as a menu row or key would run it (`<name>.<word>`
    /// suffix arguments included). The entry point of the CLI / JSON-RPC command stream (D27)
    /// and of scripted tests. At most [`QUEUED_MAX`] commands wait; more are dropped with a
    /// status message.
    pub fn command(&mut self, name: &str, arg: &str) {
        if self.queued.len() >= QUEUED_MAX {
            self.session.status = format!("{name}: command queue full");
            return;
        }
        self.queued.push((name.to_owned(), arg.to_owned()));
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

    /// Milliseconds until the editor needs [`Editor::update`] without input: a run in flight
    /// or a plugin's child process or timer pending. `None` when nothing is pending, so the
    /// window sleeps until input (D39 idle CPU 0%).
    pub fn wake_ms(&self) -> Option<u32> {
        #[cfg(not(target_arch = "wasm32"))]
        let sim = self.sim.wake_ms();
        #[cfg(target_arch = "wasm32")]
        let sim: Option<u32> = None;
        // Events queued for plugins (a `RunDone`, a completion) are delivered on the next tick.
        let queued = self.host.has_work().then_some(1);
        // A stretched canvas (mid-zoom) is redrawn crisp once the zoom has settled.
        let crisp = self.stretched.then(|| {
            self.zoom_until
                .saturating_sub(self.now)
                .min(u64::from(ZOOM_SETTLE_MS)) as u32
        });
        [sim, self.host.wake_ms(self.now), queued, crisp]
            .into_iter()
            .flatten()
            .min()
    }

    // ------------------------------------------------------------------ the frame spine

    /// One frame: input in, tables updated. Returns whether the picture changed. `now_ms`
    /// is a monotonic clock (double-click detection, waves UI, plugin timers). An empty
    /// `events` is a tick ([`Editor::wake_ms`]): it polls the run and plugin completions.
    pub fn update(&mut self, events: &InputEvents, viewport: (f32, f32), now_ms: u64) -> bool {
        assert!(viewport.0 >= 0.0 && viewport.1 >= 0.0, "negative viewport");
        self.now = now_ms;
        // 0. the waves dock: its share of the window and the input over it
        let mut changed = false;
        let (events, viewport) = match self.waves.is_some() {
            true => {
                let (mine, dock, view, dock_view) = self.split_input(events, viewport);
                let w = self.waves.as_mut().expect("checked above");
                let (redraw, close) = w.update(&dock, dock_view, now_ms);
                changed |= redraw;
                if close {
                    self.waves = None;
                    self.shell.invalidate();
                    changed = true;
                }
                (std::borrow::Cow::Owned(mine), view)
            }
            false => (std::borrow::Cow::Borrowed(events), viewport),
        };
        let events: &InputEvents = &events;
        let zoom0 = self.camera().scale;
        let resized = viewport != self.viewport;
        self.viewport = viewport;
        self.compose_ui(false);
        let mut routed = std::mem::take(&mut self.routed);
        // 1. input → command_requests + canvas events
        route_input(
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
        let mut reqs: Vec<EditRequest> = Vec::new();
        self.undid = false;
        changed |= self.sync_page(events, &routed.commands, &mut reqs);
        // 2. dispatch → edit_requests, plugin_events, ui state
        changed |= self.dispatch_all(&routed.commands, &mut reqs);
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
        changed |= self.auto_trim(&mut reqs);
        // 5. symbols of the active document (I/O); a journal's crash recovery asks first
        self.load_symbols();
        self.prompt_recovery();
        // 6. elab::recompute → values and violations (D13, D19), then the D20 cache
        //    write-back folded into the edit's own undo entry, then the `checks` source of
        //    the annotation overlay; before compose so page, panel and canvas agree.
        if self.recompute_elab() {
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
        // 8. ui compose + layout
        changed |= self.compose_ui(routed.relayout);
        if let Some(id) = self.focus_next.take() {
            let s = &mut self.shell;
            let sym = s.strings.intern(&id);
            s.ui.focus_text(&s.tables, &s.layout, &s.strings, &mut s.text, sym);
        }
        // 9. scene, decor, overlay, marks (auto highlight follows the selection first)
        if self.sel_changed && self.opts.auto_highlight {
            self.auto_highlight();
        }
        let stale = self.build_scene();
        changed |= stale;
        let decor = self.build_decor(stale);
        changed |= decor;
        self.build_shown();
        changed |= self.build_extra(decor);
        self.build_overlay();
        // 10. zoom settle: while the scale keeps changing the canvas layer is stretched; the
        //     first frame ZOOM_SETTLE_MS after the last change records it crisp.
        if self.camera().scale != zoom0 {
            self.zoom_until = now_ms + u64::from(ZOOM_SETTLE_MS);
        } else if self.stretched && now_ms >= self.zoom_until {
            self.stretched = false;
            changed = true;
        }
        self.routed = routed;
        changed
    }

    /// Splits one input batch between the editor (above the dock) and the waves dock
    /// (below, in its own coordinates): pointer events by position, keys and text to the
    /// side the pointer was last over, `Ctrl+W` always to the dock (it closes it). A left
    /// press within [`DOCK_GRAB_PX`] of the divider drags it (neither side sees the drag).
    /// Returns (editor events, dock events, editor viewport, dock viewport).
    fn split_input(
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

    /// Paints one frame: UI main layer, canvas (grid, scene, decor, overlay, marks) or the
    /// text tab, the docs page's graphs, the UI overlay (menus, dialogs), then the waves dock
    /// below.
    pub fn paint<P: Painter>(&mut self, p: &mut P) {
        draw_ui(
            p,
            &self.shell.tables,
            &self.shell.layout,
            &self.shell.strings,
            &mut self.shell.text,
            &self.shell.ui,
            Layer::Main,
            &[],
            &mut self.ui_scratch,
        );
        if self.session.text_view().is_none() {
            self.paint_canvas(p);
        }
        self.paint_graphs(p);
        draw_ui(
            p,
            &self.shell.tables,
            &self.shell.layout,
            &self.shell.strings,
            &mut self.shell.text,
            &self.shell.ui,
            Layer::Overlay,
            &[],
            &mut self.ui_scratch,
        );
        let dy = f64::from(self.viewport.1);
        if let Some(w) = self.waves.as_mut() {
            w.paint(&mut Shifted { p, dy });
        }
    }

    fn paint_canvas<P: Painter>(&mut self, p: &mut P) {
        let view = self.canvas_rect();
        let cam = self.camera();
        let grid = if self.opts.show_grid {
            self.opts.grid
        } else {
            i32::MAX / 4
        };
        // Everything but the gesture preview goes through the raster layer: a pan or zoom
        // frame re-encodes nothing and an edit redraws only where its tables changed (D39).
        // The palette as the View/Layers options shape it (hidden layers, dimming, colors,
        // widths) and the grid are the `style`: no damage box describes a change of either,
        // so it records afresh. The scene as displayed (instances as boxes, symbol texts off).
        let palette = &self.view_palette();
        use std::hash::{Hash, Hasher};
        let style = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (
                grid,
                palette.color,
                palette.fill,
                palette.background,
                palette.grid,
            )
                .hash(&mut h);
            palette.width.map(f32::to_bits).hash(&mut h);
            h.finish()
        };
        let key = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (self.canvas_gen, style).hash(&mut h);
            h.finish()
        };
        let shown = self.shown.as_ref().unwrap_or(&self.scene.prims);
        let (diff, diff_gen, diff_style, boxes) = &mut self.canvas_diff;
        if *diff_gen != self.canvas_gen || *diff_style != style {
            // ponytail: hashes every row of a changed table (~2 ms per 10^5 rows); the scene
            // and decor builders reporting their own changed rows would make it O(change).
            let mut out = boxes.take().unwrap_or_default();
            out.clear();
            let changed = |k: usize| diff_gen[k] != self.canvas_gen[k];
            let tables = [
                (shown, changed(0)),
                (&self.decor, changed(1)),
                (&self.extra, changed(2)),
            ];
            let marks = (&self.scene.prims, &self.marks, changed(0) || changed(3));
            let known = diff.update(&tables, marks, &mut out);
            *boxes = (known && *diff_style == style).then_some(out);
            (*diff_gen, *diff_style) = (self.canvas_gen, style);
        } else if let Some(b) = boxes.as_mut() {
            b.clear(); // the same key: nothing changed since the last frame
        }
        let damage = boxes.as_deref();
        let crisp = self.now >= self.zoom_until;
        let (prims, decor, extra, marks) =
            (&self.scene.prims, &self.decor, &self.extra, &self.marks);
        let (text, scratch) = (&mut self.shell.text, &mut self.canvas_scratch);
        let drawn = canvas::draw_cached(
            p,
            &mut self.canvas_cache,
            view,
            &cam,
            key,
            damage,
            crisp,
            schemify_render::rgba(palette.background),
            |p, v, c| {
                p.fill_rect(v, schemify_render::rgba(palette.background));
                canvas::draw_grid(p, v, c, grid, palette, scratch);
                canvas::draw_prims(p, v, c, shown, palette, text, scratch);
                canvas::draw_prims(p, v, c, decor, palette, text, scratch);
                canvas::draw_prims(p, v, c, extra, palette, text, scratch);
                canvas::draw_marks(p, v, c, prims, marks, scratch);
            },
        );
        self.stretched = drawn == canvas::Drawn::Stretched;
        if !self.overlay.is_empty() {
            p.push_clip(view);
            canvas::draw_prims(p, view, &cam, &self.overlay, palette, text, scratch);
            p.pop_clip();
        }
    }

    /// Draws each filled plot slot of the docs page (`graph` nodes) as an embedded waves
    /// pane over the selected job's results, clipped to the page.
    fn paint_graphs<P: Painter>(&mut self, p: &mut P) {
        #[cfg(not(target_arch = "wasm32"))]
        for (id, expr) in &self.graphs {
            let s = &self.shell;
            let Some(sym) = s.strings.find(id) else {
                continue;
            };
            let Some(row) = s.tables.nodes.find(sym).and_then(|n| s.layout.row_of(n)) else {
                continue;
            };
            let Some(Ok(w)) = self
                .sim
                .plots
                .iter()
                .find(|x| x.expr == *expr)
                .map(|x| &x.waves)
            else {
                continue;
            };
            let r = s.layout.rect()[row];
            let clip = s.layout.clip()[row];
            if r.w < 16.0 || r.h < 16.0 {
                continue;
            }
            let clipped = clip != schemify_ui::NO_CLIP;
            if clipped {
                p.push_clip(KRect::new(
                    f64::from(clip.x),
                    f64::from(clip.y),
                    f64::from(clip.x + clip.w),
                    f64::from(clip.y + clip.h),
                ));
            }
            schemify_render::waves::draw_pane(
                p,
                r,
                w,
                0,
                false,
                schemify_ui::waves::Drag::None,
                &self.plot_look,
                &mut self.shell.text,
                &mut self.plot_scratch,
            );
            if clipped {
                p.pop_clip();
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = p;
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

    // ------------------------------------------------------------------ dispatch

    fn dispatch_all(&mut self, cmds: &CommandRequests, reqs: &mut Vec<EditRequest>) -> bool {
        let mut changed = false;
        for i in 0..cmds.len() {
            let name = self.shell.strings.get(cmds.command[i]).to_owned();
            let node = if cmds.node[i].is_none() {
                String::new()
            } else {
                self.shell.strings.get(cmds.node[i]).to_owned()
            };
            let (target, suffix) = self.commands.resolve_arg(&name);
            let arg = match cmds.arg(i) {
                "" => suffix.to_owned(),
                a => a.to_owned(),
            };
            changed |= match target {
                Target::Core(cmd) => self.run(cmd, &arg, &node, reqs),
                Target::Plugin(id) => {
                    let ev = Event::Command {
                        name: &name,
                        args: &arg,
                    };
                    if let Err(e) = self.host.push(Some(id), &ev) {
                        self.session.status = e;
                    }
                    true
                }
                // A Lua keymap function or a lazy package's first command (config spec §3, §5).
                Target::Unknown if self.host.user_command(&name, &arg) => true,
                Target::Unknown => {
                    self.session.status = format!("{name}: not available yet");
                    true
                }
            };
        }
        changed
    }

    /// Runs the commands [`Editor::command`] queued, in order.
    fn dispatch_queued(&mut self, reqs: &mut Vec<EditRequest>) -> bool {
        let mut changed = false;
        for (name, arg) in std::mem::take(&mut self.queued) {
            let (target, suffix) = self.commands.resolve_arg(&name);
            let arg = if arg.is_empty() {
                suffix.to_owned()
            } else {
                arg
            };
            changed |= match target {
                Target::Core(cmd) => self.run(cmd, &arg, "", reqs),
                Target::Plugin(id) => {
                    let ev = Event::Command {
                        name: &name,
                        args: &arg,
                    };
                    if let Err(e) = self.host.push(Some(id), &ev) {
                        self.session.status = e;
                    }
                    true
                }
                Target::Unknown => {
                    self.session.status = format!("{name}: not available yet");
                    true
                }
            };
        }
        assert!(self.queued.is_empty());
        changed
    }

    /// Runs one core command. Returns whether anything visible changed.
    #[allow(clippy::too_many_lines)]
    fn run(&mut self, cmd: Cmd, arg: &str, node: &str, reqs: &mut Vec<EditRequest>) -> bool {
        if cmd == Cmd::FieldTyped {
            // Typing in a field: allowed in every dialog and tab, changes no document.
            return self.field_typed(node, arg);
        }
        if matches!(self.dialog, Dialog::Ctx(_)) && cmd != Cmd::DialogClose {
            // A context-menu row: the menu closes and its command runs as from the menubar.
            self.close_dialog();
        }
        let modal = !matches!(self.dialog, Dialog::None) || !self.shell.ui.dialog().is_none();
        let dialog_cmd = matches!(
            cmd,
            Cmd::DialogClose
                | Cmd::ChooseFilter
                | Cmd::ChoosePick
                | Cmd::EnterTextOk
                | Cmd::PropsOk
                | Cmd::PropsField
                | Cmd::EditFileOk
                | Cmd::Recover
                | Cmd::DiscardRecovery
        ) || (cmd == Cmd::ToggleLayer && matches!(self.dialog, Dialog::Layers));
        if modal && !dialog_cmd {
            return false; // xschem: editing keys are no-ops while a dialog is open
        }
        if cmd != Cmd::Close && cmd != Cmd::Quit {
            self.confirm = None;
        }
        let Some((h, view)) = self.active() else {
            return false;
        };
        if self.session.text_view().is_some() && !text_tab_cmd(cmd) && !parity::text_tab_ok(cmd) {
            self.session.status = "read-only text view: switch to a schematic tab".into();
            return true;
        }
        let ptr = self.pointer_world_snapped();
        match cmd {
            Cmd::Undo | Cmd::Redo => {
                self.tool = Tool::Idle;
                let edit = if cmd == Cmd::Undo {
                    Edit::Undo
                } else {
                    Edit::Redo
                };
                push_req(reqs, h, edit);
            }
            Cmd::Delete => {
                let ops = self.with_doc(h, edit::delete);
                self.commit(reqs, h, "delete", ops);
                self.set_selection(Vec::new());
            }
            Cmd::SelectAll => {
                let all: Vec<ObjKey> = self.scene.objs.key.clone();
                self.set_selection(all);
            }
            Cmd::Copy | Cmd::Cut => {
                let base = self.selection_pivot();
                let recs = self.with_doc(h, |doc, sel| {
                    edit::selected_records(doc, sel)
                        .into_iter()
                        .map(|(_, _, r)| r)
                        .collect::<Vec<_>>()
                });
                let back = Xform::translate(-base.0, -base.1);
                self.clipboard = recs
                    .into_iter()
                    .filter_map(|mut r| crate::geometry::transform_record(&mut r, back).map(|()| r))
                    .collect();
                self.session.status = format!("{} object(s) copied", self.clipboard.len());
                if self.system_clipboard
                    && !self.clipboard.is_empty()
                    && let Err(e) = platform::clipboard_set(&crate::clip::encode(&self.clipboard))
                {
                    self.session.status = format!("system clipboard: {e} (copied here only)");
                }
                if cmd == Cmd::Cut {
                    let ops = self.with_doc(h, edit::delete);
                    self.commit(reqs, h, "cut", ops);
                    self.set_selection(Vec::new());
                }
            }
            Cmd::Paste => {
                // The system clipboard wins when it holds a Schemify fragment (another
                // window, another process, a text editor); else the editor's own.
                if self.system_clipboard
                    && let Ok(text) = platform::clipboard_get()
                    && let Ok(recs) = crate::clip::decode(&text)
                {
                    self.clipboard = recs;
                }
                if !self.clipboard.is_empty() {
                    self.tool = Tool::Place {
                        what: Placement::Paste(self.clipboard.clone()),
                        at: ptr,
                        lin: Xform::IDENTITY,
                    };
                }
            }
            Cmd::Duplicate | Cmd::Move | Cmd::MoveKiss | Cmd::MoveStretch => {
                // xschem: `m` moves, Ctrl+M stretches (swapped by `enable_stretch`), Shift+M
                // stretches and adds "kissing" wires.
                let stretch = match cmd {
                    Cmd::Move => u8::from(self.opts.stretch),
                    Cmd::MoveStretch => u8::from(!self.opts.stretch),
                    Cmd::MoveKiss => 2,
                    _ => 0,
                };
                let partial = cmd != Cmd::Duplicate && !self.partial_ends().is_empty();
                if !self.selection().is_empty() || partial {
                    self.tool = Tool::Move {
                        from: ptr,
                        to: ptr,
                        lin: Xform::IDENTITY,
                        copy: cmd == Cmd::Duplicate,
                        drag: false,
                        stretch,
                    };
                }
            }
            Cmd::Rotate | Cmd::Flip | Cmd::VFlip => {
                let about: fn(Pt) -> Xform = match cmd {
                    Cmd::Rotate => Xform::rotate,
                    Cmd::Flip => Xform::flip_x,
                    _ => Xform::flip_y,
                };
                match &mut self.tool {
                    Tool::Move { from, lin, .. } => *lin = lin.then(about(*from)),
                    Tool::Place { at, lin, .. } => *lin = lin.then(about(*at)),
                    _ => {
                        let pivot = self.selection_pivot();
                        let ops =
                            self.with_doc(h, |doc, sel| edit::transform(doc, sel, about(pivot)));
                        self.commit(reqs, h, cmd.name(), ops);
                    }
                }
            }
            Cmd::RotateInPlace | Cmd::FlipInPlace | Cmd::VFlipInPlace => {
                let about: fn(Pt) -> Xform = match cmd {
                    Cmd::RotateInPlace => Xform::rotate,
                    Cmd::FlipInPlace => Xform::flip_x,
                    _ => Xform::flip_y,
                };
                let ops = self.with_doc(h, |doc, sel| edit::transform_each(doc, sel, about));
                self.commit(reqs, h, cmd.name(), ops);
            }
            Cmd::ConstrainH => self.opts.constrain = if self.opts.constrain == 1 { 0 } else { 1 },
            Cmd::ConstrainV => self.opts.constrain = if self.opts.constrain == 2 { 0 } else { 2 },
            Cmd::Unconstrained => self.opts.constrain = 0,
            Cmd::Descend | Cmd::DescendSymbol | Cmd::OpenSubTab | Cmd::OpenSymbolTab => {
                self.descend(h, cmd);
            }
            Cmd::Ascend => {
                self.tool = Tool::Idle;
                if !self.session.pop_level() {
                    self.session.status = "already at the top level".into();
                }
                self.scene_dirty = true;
            }
            Cmd::NewSchematic | Cmd::TabNew | Cmd::NewWindowTab | Cmd::NewSymbol => {
                let (kind, v) = if cmd == Cmd::NewSymbol {
                    (FileKind::Shprim, View::Symbol)
                } else {
                    (FileKind::Shcomp, View::Schematic)
                };
                match self.session.create(kind) {
                    Ok(d) => self.session.open_tab(d, v),
                    Err(e) => self.session.status = e,
                }
                self.scene_dirty = true;
            }
            Cmd::Open => self.open_chooser(ChooseFor::Open),
            Cmd::ComponentBrowser | Cmd::InsertSymbol => self.open_chooser(ChooseFor::Insert),
            Cmd::Waves => self.open_chooser(ChooseFor::Waves),
            Cmd::Save => {
                let top = self.session.tabs[self.session.active].stack[0].doc;
                for d in [h, top] {
                    match self.session.save(d) {
                        Ok(true) => {
                            self.session.status = format!("saved {}", self.session.name(d));
                            let _ = self.host.push(
                                None,
                                &Event::Saved {
                                    doc: plugins::doc_id(d),
                                },
                            );
                        }
                        Ok(false) => {}
                        Err(e) => self.session.status = e,
                    }
                }
            }
            Cmd::Reload => self.reload(h),
            Cmd::Close => self.close_tab(),
            Cmd::Quit => {
                let dirty = self
                    .session
                    .project
                    .handles()
                    .any(|d| self.session.project.is_dirty(d));
                if dirty && self.confirm != Some(Cmd::Quit) {
                    self.confirm = Some(Cmd::Quit);
                    self.session.status =
                        "unsaved changes: Ctrl+Q again to quit without saving".into();
                } else {
                    self.quit = true;
                }
            }
            Cmd::Highlight | Cmd::Unhighlight | Cmd::UnhighlightSelected => {
                self.highlight_cmd(h, cmd);
            }
            Cmd::Netlist | Cmd::NetlistTop => self.netlist(cmd == Cmd::Netlist),
            Cmd::SetDialect => {
                let word = if arg.is_empty() {
                    node.rsplit('.').next().unwrap_or("")
                } else {
                    arg.trim()
                };
                match spines::dialect_of(word) {
                    Some(d) => {
                        self.dialect = d;
                        self.session.status = format!("netlist dialect: {word}");
                    }
                    None => self.session.status = format!("{word:?}: not a netlist dialect"),
                }
            }
            Cmd::ShowNetlist => self.show_netlist = !self.show_netlist,
            Cmd::Simulate | Cmd::Optimize => self.simulate(h, cmd == Cmd::Optimize, arg),
            Cmd::SimCancel => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.session.status = if self.sim.cancel() {
                        "simulation cancelled".into()
                    } else {
                        "nothing is running".into()
                    };
                }
            }
            Cmd::NewBench => self.new_bench(h),
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
            Cmd::ChecksPick => self.checks_pick(h, node),
            Cmd::SendToViewer | Cmd::SendHighlighted => self.send_to_viewer(),
            Cmd::ResultsPick | Cmd::ResultsWaves | Cmd::ResultsHistory => {
                self.results_cmd(cmd, node);
            }
            Cmd::OpAnnotate | Cmd::OpAnnotateGraphs => {
                self.op_on = !self.op_on;
                self.op_job = None;
                self.session.status =
                    format!("OP annotation {}", if self.op_on { "on" } else { "off" });
            }
            Cmd::AcceptStaged | Cmd::RejectStaged => {
                let Some(id) = self.session.project.stages(h).first().map(|st| st.id) else {
                    self.session.status = "no staged edit on this document".into();
                    return true;
                };
                let edit = if cmd == Cmd::AcceptStaged {
                    Edit::Accept(id)
                } else {
                    Edit::Reject(id)
                };
                push_req(reqs, h, edit);
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
                _ => self.session.status = format!("{arg:?}: not a positive number of units"),
            },
            Cmd::Props | Cmd::PropsEditor | Cmd::PropsView => self.open_props(h),
            Cmd::PropsField => self.props_field(node, arg),
            Cmd::PropsOk => self.props_ok(h, reqs),
            Cmd::DialogClose => self.close_dialog(),
            Cmd::ChooseFilter => self.choose_filter(arg),
            Cmd::ChoosePick => self.choose_pick(node, h),
            Cmd::EnterTextOk => self.enter_text_ok(arg, ptr, reqs),
            Cmd::PlaceLabel | Cmd::PlaceWireLabel => self.open_text(TextFor::Label, "Net label"),
            Cmd::PlaceInPort => self.open_text(TextFor::Port(false), "Input port"),
            Cmd::PlaceOutPort => self.open_text(TextFor::Port(true), "Output port"),
            Cmd::InsertText => self.open_text(TextFor::Text, "Text"),
            Cmd::PlaceSymPin => {
                if view == View::Symbol {
                    self.open_text(TextFor::SymPin, "Symbol pin name");
                } else {
                    self.session.status = "symbol pins go in the symbol view (i)".into();
                }
            }
            Cmd::InsertWire | Cmd::InsertSnapWire => {
                if view != View::Schematic {
                    self.session.status = "wires go in a schematic".into();
                } else {
                    let from = if cmd == Cmd::InsertSnapWire {
                        self.nearest_conn(ptr).unwrap_or(ptr)
                    } else {
                        ptr
                    };
                    self.tool = Tool::Wire {
                        from,
                        to: from,
                        drag: false,
                    };
                }
            }
            Cmd::InsertLine | Cmd::InsertRect => {
                // Symbol graphics in the symbol view, `shape` records in a schematic.
                self.tool = Tool::Shape {
                    form: if cmd == Cmd::InsertRect {
                        Form::Rect
                    } else {
                        Form::Line
                    },
                    pts: vec![ptr],
                    to: ptr,
                };
                let _ = view;
            }
            Cmd::CycleManhattan => match self.tool {
                Tool::Wire { .. } | Tool::Move { .. } | Tool::Shape { .. } => {
                    self.opts.manhattan = (self.opts.manhattan + 1) % 3;
                }
                _ => {
                    self.space_pan = true;
                    self.pan = Some(self.pointer);
                }
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
            Cmd::TabSelect => {
                let i = node
                    .rsplit('.')
                    .next()
                    .and_then(|n| n.parse::<usize>().ok());
                if let Some(i) = i.filter(|&i| i < self.session.tabs.len()) {
                    self.switch_tab(i);
                }
            }
            Cmd::TabNext | Cmd::TabPrev => {
                let n = self.session.tabs.len();
                let i = if cmd == Cmd::TabNext {
                    (self.session.active + 1) % n
                } else {
                    (self.session.active + n - 1) % n
                };
                self.switch_tab(i);
            }
            Cmd::DocsToggle => {
                let a = self.session.active;
                if let Some(t) = self.session.tabs.get_mut(a) {
                    t.docs = !t.docs;
                }
            }
            Cmd::DocsSelect => self.page.sel = docs::row_of(node),
            Cmd::DocsEdit => {
                let Some(row) = docs::row_of(node) else {
                    return false;
                };
                self.page.sel = Some(row);
                let text = self
                    .session
                    .project
                    .doc(h)
                    .filter(|d| docs::kind_of(d, row) == Some(RecordKind::Prose))
                    .map(|d| docs::prose_text(d, row));
                if let Some(text) = text {
                    self.page.editing = Some((row, text));
                    self.page.focused = false;
                    self.focus_next = Some(docs::editor_id(row));
                }
            }
            Cmd::DocsVarOk => {
                let Some(row) = docs::row_of(node) else {
                    return false;
                };
                self.page.sel = Some(row);
                let ops = self.with_doc(h, |d, _| docs::var_ops(d, row, arg));
                self.commit_docs(reqs, h, "edit variable", ops);
            }
            Cmd::DocsProseOk => {
                if let Some((row, typed)) = self.page.editing.take() {
                    let text = if arg.is_empty() {
                        typed
                    } else {
                        arg.to_owned()
                    };
                    let ops = self.with_doc(h, |d, _| docs::prose_ops(d, row, &text));
                    self.commit_docs(reqs, h, "edit text", ops);
                }
            }
            Cmd::DocsAddProse | Cmd::DocsAddVar => {
                let Some(doc) = self.session.project.doc(h) else {
                    return false;
                };
                let row = docs::insert_row(doc, self.page.sel);
                let (label, ops) = if cmd == Cmd::DocsAddProse {
                    ("add text", docs::add_prose(row))
                } else {
                    ("add variable", docs::add_var(doc, row))
                };
                self.commit_docs(reqs, h, label, ops);
                self.page.sel = Some(row);
                let tab = self.session.active;
                if let Some(t) = self.session.tabs.get_mut(tab) {
                    t.docs = true;
                }
                if cmd == Cmd::DocsAddVar {
                    self.focus_next = Some(format!("docs.b.{row}.expr"));
                }
            }
            Cmd::DocsUp | Cmd::DocsDown => {
                let Some(row) = self.page.sel else {
                    self.session.status = "select a docs block first".into();
                    return true;
                };
                let up = cmd == Cmd::DocsUp;
                let ops = self.with_doc(h, |d, _| docs::move_block(d, row, up));
                if !ops.is_empty() {
                    self.page.sel = Some(if up { row - 1 } else { row + 1 });
                }
                self.commit_docs(reqs, h, "move block", ops);
            }
            Cmd::DocsDelete => {
                let Some(row) = self.page.sel.take() else {
                    self.session.status = "select a docs block first".into();
                    return true;
                };
                self.page.editing = None;
                let ops = self.with_doc(h, |d, _| docs::delete_block(d, row));
                self.commit_docs(reqs, h, "delete block", ops);
            }
            Cmd::UnselectUnderMouse => {
                if let Some(o) = self.hit_at(self.pointer) {
                    let key = self.scene.objs.key[o as usize];
                    let sel: Vec<ObjKey> = self
                        .selection()
                        .iter()
                        .copied()
                        .filter(|&k| k != key)
                        .collect();
                    self.set_selection(sel);
                }
            }
            // Every command added for xschem parity (`parity.rs`).
            other => return self.parity(other, arg, node, reqs),
        }
        true
    }

    // ------------------------------------------------------------------ helpers

    fn with_doc<R>(&self, h: DocHandle, f: impl FnOnce(&Document, &[ObjKey]) -> R) -> R
    where
        R: Default,
    {
        let sel = self.selection();
        self.session
            .project
            .doc(h)
            .map_or_else(R::default, |d| f(d, sel))
    }

    /// Appends a commit transaction (skipped when empty).
    fn commit(&mut self, reqs: &mut Vec<EditRequest>, h: DocHandle, label: &str, ops: Vec<Op>) {
        if ops.is_empty() {
            return;
        }
        let tx = Transaction {
            label: label.to_owned(),
            mode: Mode::Commit,
            ops,
        };
        push_req(reqs, h, Edit::Tx(tx));
    }

    /// [`Editor::commit`] for a docs-page edit, remembered so a D20 cache write it causes
    /// lands in the same undo entry (`sync_cache`).
    fn commit_docs(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        label: &str,
        ops: Vec<Op>,
    ) {
        if ops.is_empty() {
            return;
        }
        self.amend = Some((h, label.to_owned(), ops.clone(), None));
        self.commit(reqs, h, label, ops);
    }

    fn set_selection(&mut self, mut sel: Vec<ObjKey>) {
        sel.sort_unstable();
        sel.dedup();
        if let Some(l) = self.session.level_mut()
            && l.sel != sel
        {
            l.sel = sel;
            self.sel_changed = true;
        }
    }

    fn pointer_world(&self) -> (f64, f64) {
        let v = self.canvas_rect();
        self.camera()
            .to_world(v, f64::from(self.pointer.0), f64::from(self.pointer.1))
    }

    fn pointer_world_snapped(&self) -> Pt {
        let (x, y) = self.pointer_world();
        (snap(x, self.opts.snap), snap(y, self.opts.snap))
    }

    fn world_at(&self, s: (f32, f32)) -> (f64, f64) {
        self.camera()
            .to_world(self.canvas_rect(), f64::from(s.0), f64::from(s.1))
    }

    fn snapped_at(&self, s: (f32, f32)) -> Pt {
        let (x, y) = self.world_at(s);
        (snap(x, self.opts.snap), snap(y, self.opts.snap))
    }

    fn hit_at(&self, s: (f32, f32)) -> Option<u32> {
        let tol = HIT_PX / self.camera().scale;
        scene::hit(&self.scene.objs, self.world_at(s), tol)
    }

    fn selection_bbox(&self) -> Option<[i32; 4]> {
        let mut b: Option<[i32; 4]> = None;
        for &k in self.selection() {
            if let Some(i) = self.scene.find(k) {
                let o = self.scene.objs.bbox(i as usize);
                b = Some(match b {
                    None => o,
                    Some(c) => [
                        c[0].min(o[0]),
                        c[1].min(o[1]),
                        c[2].max(o[2]),
                        c[3].max(o[3]),
                    ],
                });
            }
        }
        b
    }

    /// Centroid of the selected objects' anchors, snapped: the group rotate/flip pivot and
    /// the clipboard origin. A single object turns about its own anchor (instance origin,
    /// label point, first wire end), which keeps it on the grid; drawn extents (texts) would
    /// move the pivot with font metrics.
    fn selection_pivot(&self) -> Pt {
        let anchors: Vec<Pt> = self
            .active()
            .and_then(|(h, _)| self.session.project.doc(h))
            .map(|doc| {
                edit::selected_records(doc, self.selection())
                    .iter()
                    .filter_map(|(_, _, r)| crate::geometry::anchor(r))
                    .collect()
            })
            .unwrap_or_default();
        if anchors.is_empty() {
            return self.pointer_world_snapped();
        }
        let n = anchors.len() as f64;
        let c = anchors.iter().fold((0.0, 0.0), |a, p| {
            (a.0 + f64::from(p.0) / n, a.1 + f64::from(p.1) / n)
        });
        (snap(c.0, self.opts.snap), snap(c.1, self.opts.snap))
    }

    fn nearest_conn(&self, p: Pt) -> Option<Pt> {
        let d = |q: Pt| {
            (i64::from(q.0) - i64::from(p.0)).pow(2) + (i64::from(q.1) - i64::from(p.1)).pow(2)
        };
        self.scene.conn.iter().map(|c| c.0).min_by_key(|&q| d(q))
    }

    fn fit(&mut self, b: [i32; 4]) {
        let v = self.canvas_rect();
        if v.width() <= 0.0 || v.height() <= 0.0 {
            return;
        }
        let mut cam = self.camera();
        let pad = 20 * UNIT;
        cam.fit(v, [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad]);
        self.set_camera(cam);
    }

    fn switch_tab(&mut self, i: usize) {
        assert!(i < self.session.tabs.len());
        self.tool = Tool::Idle;
        self.session.active = i;
        self.scene_dirty = true;
    }

    fn refresh_palette(&mut self) {
        let (p, sel) = self.shell.palette(self.opts.light);
        self.palette = p;
        self.sel_color = sel;
        self.sel_changed = true;
    }

    /// The single selected instance (or the instance under the pointer).
    fn one_instance(&self) -> Option<ObjKey> {
        let sel: Vec<ObjKey> = self
            .selection()
            .iter()
            .copied()
            .filter(|k| k.table == TableId::Instances)
            .collect();
        match sel.as_slice() {
            [k] => Some(*k),
            [] => self
                .hit_at(self.pointer)
                .map(|o| self.scene.objs.key[o as usize])
                .filter(|k| k.table == TableId::Instances),
            _ => None,
        }
    }

    fn descend(&mut self, h: DocHandle, cmd: Cmd) {
        let Some(key) = self.one_instance() else {
            self.session.status = "select exactly one instance".into();
            return;
        };
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let Some(loc) = edit::loc_of(doc, key) else {
            return;
        };
        let row = loc.row as usize;
        let s = doc.strings();
        let cellref = s.get(doc.instances().cell[row]).to_owned();
        let name = s.get(doc.instances().name[row]).to_owned();
        let cell = match self.session.project.resolve(h, &cellref) {
            Ok(r) => r,
            Err(c) => {
                self.session.status = format!("{cellref}: {c} {}", c.meaning());
                return;
            }
        };
        let kind = self.session.project.cells().kind[cell];
        let symbol = matches!(cmd, Cmd::DescendSymbol | Cmd::OpenSymbolTab);
        if !symbol && kind != FileKind::Shcomp {
            self.session.status =
                format!("{cellref} is a primitive: no schematic (use i for its symbol)");
            return;
        }
        let child = match self.session.open_cell(cell) {
            Ok(c) => c,
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
        self.tool = Tool::Idle;
        if matches!(cmd, Cmd::OpenSubTab | Cmd::OpenSymbolTab) {
            self.session.open_tab(child, view);
        } else if let Err(e) = self.session.push_level(child, view, &name) {
            self.session.status = e;
        }
        self.scene_dirty = true;
    }

    fn close_tab(&mut self) {
        let Some(tab) = self.session.tabs.get(self.session.active) else {
            return;
        };
        let dirty = tab
            .stack
            .iter()
            .any(|l| self.session.project.is_dirty(l.doc));
        if dirty && self.confirm != Some(Cmd::Close) {
            self.confirm = Some(Cmd::Close);
            self.session.status =
                "unsaved changes: Ctrl+S to save, or close again to discard".into();
            return;
        }
        self.confirm = None;
        self.session.tabs.remove(self.session.active);
        if self.session.tabs.is_empty() {
            match self.session.create(FileKind::Shcomp) {
                Ok(d) => self.session.open_tab(d, View::Schematic),
                Err(e) => self.session.status = e,
            }
        }
        self.session.active = self.session.active.min(self.session.tabs.len() - 1);
        self.tool = Tool::Idle;
        self.scene_dirty = true;
    }

    fn reload(&mut self, h: DocHandle) {
        let Some(path) = self.session.path(h).map(Path::to_path_buf) else {
            return;
        };
        if self.session.project.is_dirty(h) {
            self.session.status = "unsaved changes: save or undo them before reloading".into();
            return;
        }
        match platform::read(&path) {
            Ok(bytes) => match self.session.project.external_change(h, &bytes) {
                Ok(External::Unchanged) => self.session.status = "file unchanged".into(),
                Ok(e) => {
                    self.session.status = format!("reloaded ({e:?})");
                    self.session.bump(h);
                    self.session.flush_journal(h);
                }
                Err(c) => self.session.status = format!("reload: {c}"),
            },
            Err(e) => self.session.status = e,
        }
    }

    // ------------------------------------------------------------------ dialogs

    fn show_dialog(&mut self, id: &str) {
        let sym = self.shell.sym(id);
        self.shell.ui.set_dialog(sym);
        self.shell.invalidate();
    }

    fn close_dialog(&mut self) {
        self.dialog = Dialog::None;
        self.shell.ui.set_dialog(schemify_ui::Sym::NONE);
        self.shell.invalidate();
    }

    fn open_text(&mut self, purpose: TextFor, title: &str) {
        self.tool = Tool::Idle;
        self.dialog = Dialog::Text(purpose, title.to_owned(), String::new());
        self.show_dialog("dialog.enter_text");
        self.focus_next = Some("dialog.enter_text.input".into());
    }

    fn enter_text_ok(&mut self, arg: &str, at: Pt, reqs: &mut Vec<EditRequest>) {
        let Dialog::Text(purpose, _, typed) = self.dialog.clone() else {
            self.close_dialog();
            return;
        };
        // Enter in the field submits its text as `arg`; the OK button submits none, and the
        // typing reached the dialog as `field.typed` before the click, so that copy is used.
        let text = if arg.is_empty() {
            typed
        } else {
            arg.to_owned()
        };
        self.close_dialog();
        let text = text.trim().to_owned();
        if text.is_empty() {
            return; // xschem: empty text places nothing
        }
        let what = match purpose {
            TextFor::Label => Placement::Label(text),
            TextFor::Port(out) => Placement::Port(text, out),
            TextFor::Text => Placement::Text(text),
            TextFor::SymPin => Placement::SymPin(text),
            other => {
                self.text_ok(other, &text, reqs);
                return;
            }
        };
        self.tool = Tool::Place {
            what,
            at,
            lin: Xform::IDENTITY,
        };
    }

    fn open_chooser(&mut self, purpose: ChooseFor) {
        self.tool = Tool::Idle;
        self.dialog = Dialog::Choose(Chooser {
            purpose,
            filter: String::new(),
            rows: Vec::new(),
            note: String::new(),
        });
        self.refill_chooser();
        self.show_dialog("dialog.choose");
        self.focus_next = Some("dialog.choose.filter".into());
    }

    /// Recomputes the chooser rows from the filter (substring, case-insensitive).
    fn refill_chooser(&mut self) {
        let Dialog::Choose(c) = &self.dialog else {
            return;
        };
        let (purpose, filter) = (c.purpose, c.filter.to_lowercase());
        let mut rows = Vec::new();
        match purpose {
            ChooseFor::Insert
            | ChooseFor::Open
            | ChooseFor::Merge
            | ChooseFor::Delete
            | ChooseFor::Compare => {
                let p = &self.session.project;
                let cells = p.cells();
                for row in 0..cells.lib.len() {
                    let kind = cells.kind[row];
                    let skip = match purpose {
                        ChooseFor::Insert => kind == FileKind::Shbench,
                        // Only files with a schematic merge or compare.
                        ChooseFor::Merge | ChooseFor::Compare => kind == FileKind::Shprim,
                        // Only files on disk delete (not the bundled core library).
                        ChooseFor::Delete => self.session.lib_dir(cells.lib[row]).is_none(),
                        _ => false,
                    };
                    if skip {
                        continue;
                    }
                    let lib = &p.libs().name[usize::from(cells.lib[row])];
                    let rel = crate::session::cell_relpath(p, row);
                    let label = if lib.is_empty() {
                        rel
                    } else {
                        format!("{lib}:{rel}")
                    };
                    if label.to_lowercase().contains(&filter) {
                        rows.push((label, Pick::Cell(row)));
                    }
                }
            }
            ChooseFor::Waves => {
                for f in platform::list_files(&self.session.root, &[]) {
                    if f.extension().is_some_and(|e| e == "raw") {
                        let label = f
                            .strip_prefix(&self.session.root)
                            .unwrap_or(&f)
                            .display()
                            .to_string();
                        if label.to_lowercase().contains(&filter) {
                            rows.push((label, Pick::File(f)));
                        }
                    }
                }
            }
        }
        rows.truncate(CHOOSER_ROWS);
        if let Dialog::Choose(c) = &mut self.dialog {
            c.rows = rows;
        }
    }

    fn choose_filter(&mut self, arg: &str) {
        let Dialog::Choose(c) = &mut self.dialog else {
            return;
        };
        c.filter = arg.to_owned();
        let purpose = c.purpose;
        self.refill_chooser();
        if purpose == ChooseFor::Waves && !arg.is_empty() && platform::exists(Path::new(arg)) {
            self.open_waves(PathBuf::from(arg));
        } else if let Dialog::Choose(c) = &self.dialog
            && c.rows.len() == 1
        {
            let node = "dialog.choose.row.0".to_owned();
            let h = self.active().map(|a| a.0);
            if let Some(h) = h {
                self.choose_pick(&node, h);
            }
        }
    }

    fn choose_pick(&mut self, node: &str, h: DocHandle) {
        let Dialog::Choose(c) = &self.dialog else {
            return;
        };
        let i = node
            .rsplit('.')
            .next()
            .and_then(|n| n.parse::<usize>().ok());
        let Some((_, pick)) = i.and_then(|i| c.rows.get(i)).cloned() else {
            return;
        };
        let purpose = c.purpose;
        self.close_dialog();
        match (purpose, pick) {
            (ChooseFor::Insert, Pick::Cell(row)) => {
                let Some(cellref) = self.cellref_for(h, row) else {
                    self.session.status =
                        "that cell is not referenceable from here (lib:cell or ./cell)".into();
                    return;
                };
                let at = self.pointer_world_snapped();
                self.tool = Tool::Place {
                    what: Placement::Inst(cellref),
                    at,
                    lin: Xform::IDENTITY,
                };
            }
            (ChooseFor::Open, Pick::Cell(row)) => match self.session.open_cell(row) {
                Ok(d) => {
                    let view = default_view(self.session.project.doc(d));
                    self.session.open_tab(d, view);
                    self.remember(d);
                    let _ = self.host.push(
                        None,
                        &Event::DocOpened {
                            doc: plugins::doc_id(d),
                        },
                    );
                    self.scene_dirty = true;
                }
                Err(e) => self.session.status = e,
            },
            (ChooseFor::Waves, Pick::File(f)) => self.open_waves(f),
            (ChooseFor::Merge, Pick::Cell(row)) => self.merge_cell(row),
            (ChooseFor::Delete, Pick::Cell(row)) => self.delete_cell(row),
            (ChooseFor::Compare, Pick::Cell(row)) => self.compare_with(row),
            _ => {}
        }
    }

    /// How document `h` references indexed cell `row`: `lib:cell` for a library root, or
    /// `./cell` for a project cell in `h`'s directory (netlist spec §1).
    fn cellref_for(&self, h: DocHandle, row: usize) -> Option<String> {
        let p = &self.session.project;
        let cells = p.cells();
        let lib = &p.libs().name[usize::from(cells.lib[row])];
        let s = p.strings();
        let stem = s.get(cells.stem[row]);
        let candidates = [format!("./{stem}"), format!("{lib}:{stem}")];
        candidates
            .into_iter()
            .find(|c| p.resolve(h, c).ok() == Some(row))
    }

    /// Opens the waves dock on `path`, or points an open dock at it keeping its traces and panes
    /// (spec waves §9).
    fn open_waves(&mut self, path: PathBuf) {
        self.close_dialog();
        let title = path.display().to_string();
        let bytes = match platform::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.session.status = e;
                return;
            }
        };
        match self.waves.as_mut() {
            Some(w) => match w.reload(&title, &bytes) {
                Ok(s) | Err(s) => self.session.status = s,
            },
            None => match WavesView::open(&title, &bytes) {
                Ok(w) => self.waves = Some(w),
                Err(e) => self.session.status = e,
            },
        }
    }

    fn open_props(&mut self, h: DocHandle) {
        let key = match self.selection() {
            [k] => Some(*k),
            [] => self
                .hit_at(self.pointer)
                .map(|o| self.scene.objs.key[o as usize]),
            _ => None,
        };
        let Some(key) = key else {
            self.session.status = "select one object to edit its properties".into();
            return;
        };
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let Some(loc) = edit::loc_of(doc, key) else {
            return;
        };
        let rec = doc.record(loc);
        let field = |key: &str, label: &str, value: &str, editable: bool| Field {
            key: key.to_owned(),
            label: label.to_owned(),
            value: value.to_owned(),
            original: value.to_owned(),
            editable,
        };
        let mut fields = Vec::new();
        let title;
        match rec.kind {
            RecordKind::Inst => {
                let name = rec
                    .find_attr("name")
                    .and_then(|a| a.values.first())
                    .cloned()
                    .unwrap_or_default();
                let cellref = rec.fields[1].clone();
                title = format!("Instance {name} ({cellref})");
                fields.push(field("name", "name", &name, true));
                // Schema rows (D40: an empty value means "use the default").
                let iface = self
                    .session
                    .project
                    .resolve(h, &cellref)
                    .ok()
                    .and_then(|r| self.session.project.interface(r));
                if let Some(iface) = iface {
                    let is = iface.strings();
                    let p = iface.params();
                    for r in 0..p.fields.len() {
                        let pname = is.get(p.fields[r][0]).to_owned();
                        let default = is.get(p.fields[r][1]).to_owned();
                        let computed = iface.pool().find_attr(is, p.attrs[r], "computed").is_some();
                        let over = rec
                            .find_attr(&pname)
                            .and_then(|a| a.values.first())
                            .cloned()
                            .unwrap_or_default();
                        let label = if computed {
                            format!("{pname} (computed)")
                        } else {
                            format!("{pname} (default {default})")
                        };
                        fields.push(field(&pname, &label, &over, !computed));
                    }
                }
                // Overrides the schema does not know (kept, editable).
                for a in &rec.attrs {
                    let known = fields.iter().any(|f| f.key == a.key);
                    if !known && schemify_doc::Instances::is_override(&a.key) {
                        let v = a.values.join(" ");
                        fields.push(field(
                            &a.key,
                            &format!("{} (not in schema)", a.key),
                            &v,
                            true,
                        ));
                    }
                }
            }
            RecordKind::Label | RecordKind::Port | RecordKind::Text => {
                let what = match rec.kind {
                    RecordKind::Label => "net",
                    RecordKind::Port => "pin",
                    _ => "text",
                };
                title = format!("{} {}", rec.kind.word(), rec.fields[4]);
                fields.push(field(what, what, &rec.fields[4], true));
            }
            RecordKind::Wire => {
                title = "Wire".to_owned();
                let bus = rec
                    .find_attr("bus")
                    .and_then(|a| a.values.first())
                    .cloned()
                    .unwrap_or_default();
                fields.push(field("bus", "bus (true/false)", &bus, true));
            }
            RecordKind::Pin => {
                title = format!("Pin {}", rec.fields[0]);
                fields.push(field("name", "name", &rec.fields[0], true));
                fields.push(field("dir", "dir (in/out/inout)", &rec.fields[1], true));
            }
            _ => {
                self.session.status = format!("{}: no editable properties yet", rec.kind.word());
                return;
            }
        }
        self.tool = Tool::Idle;
        let first = fields.iter().position(|f| f.editable);
        self.dialog = Dialog::Props(Props {
            of: PropsFor::Object(key),
            title,
            fields,
        });
        self.show_dialog("dialog.props");
        self.focus_next = first.map(|i| format!("dialog.props.val.{i}"));
    }

    fn props_field(&mut self, node: &str, arg: &str) {
        let Dialog::Props(p) = &mut self.dialog else {
            return;
        };
        let i = node
            .rsplit('.')
            .next()
            .and_then(|n| n.parse::<usize>().ok());
        if let Some(f) = i.and_then(|i| p.fields.get_mut(i)).filter(|f| f.editable) {
            f.value = arg.trim().to_owned();
        }
    }

    /// Commits the properties dialog as one transaction: `set_attr` for changed values,
    /// `remove_attr` for cleared overrides (D40: only overrides are stored).
    fn props_ok(&mut self, h: DocHandle, reqs: &mut Vec<EditRequest>) {
        let Dialog::Props(p) = std::mem::take(&mut self.dialog) else {
            self.close_dialog();
            return;
        };
        self.close_dialog();
        let key = match p.of {
            PropsFor::Object(key) => key,
            PropsFor::Header => return self.header_ok(h, &p.fields, reqs),
            PropsFor::Search => return self.search_ok(h, &p.fields, reqs),
        };
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let Some(loc) = edit::loc_of(doc, key) else {
            return;
        };
        let rec = doc.record(loc);
        let tkey = match rec.kind.key() {
            schemify_format::KeyRule::Position => loc.row.to_string(),
            _ => rec.fields[0].clone(),
        };
        let target = schemify_doc::Target::new(rec.kind, &tkey);
        let mut ops = Vec::new();
        for f in p
            .fields
            .iter()
            .filter(|f| f.editable && f.value != f.original)
        {
            let op = match (rec.kind, f.key.as_str()) {
                (RecordKind::Label | RecordKind::Port | RecordKind::Text, _) => Op::SetField {
                    target: target.clone(),
                    index: 4,
                    value: f.value.clone(),
                },
                (RecordKind::Pin, "name") => Op::SetField {
                    target: target.clone(),
                    index: 0,
                    value: f.value.clone(),
                },
                (RecordKind::Pin, _) => Op::SetField {
                    target: target.clone(),
                    index: 1,
                    value: f.value.clone(),
                },
                (_, _) if f.value.is_empty() => Op::RemoveAttr {
                    target: target.clone(),
                    key: f.key.clone(),
                    first: None,
                },
                _ => Op::SetAttr {
                    target: target.clone(),
                    key: f.key.clone(),
                    values: vec![f.value.clone()],
                },
            };
            ops.push(op);
        }
        self.commit(reqs, h, "edit properties", ops);
    }

    /// `field.typed` (a dialog field's `input` command, sent as the field's text changes): the
    /// text reaches the dialog state as typed, so OK, a click elsewhere or the chooser's
    /// filtering see it without Enter. Returns whether anything visible changed (the chooser
    /// rows). The docs page's editor is followed by `sync_page`.
    fn field_typed(&mut self, node: &str, text: &str) -> bool {
        match &mut self.dialog {
            Dialog::Props(p) => {
                let i = node
                    .strip_prefix("dialog.props.val.")
                    .and_then(|n| n.parse::<usize>().ok());
                if let Some(f) = i.and_then(|i| p.fields.get_mut(i)).filter(|f| f.editable) {
                    f.value = text.trim().to_owned();
                }
                false
            }
            Dialog::Choose(c) if node == "dialog.choose.filter" && c.filter != text => {
                c.filter = text.to_owned();
                self.refill_chooser();
                true
            }
            Dialog::Text(_, _, typed) if node == "dialog.enter_text.input" => {
                text.clone_into(typed);
                false
            }
            Dialog::File(typed) if node == "dialog.edit_file.text" => {
                text.clone_into(typed);
                false
            }
            _ => false,
        }
    }

    // ------------------------------------------------------------------ canvas gestures

    /// One canvas pointer event through the tool state machine (xschem §2.3/§2.4 with
    /// `intuitive_interface=1`, `infix_interface=1`). Returns whether anything changed.
    fn gesture(&mut self, ev: InputEvent, reqs: &mut Vec<EditRequest>, now: u64) -> bool {
        let Some((h, view)) = self.active() else {
            return false;
        };
        let shift = self.mods & Mods::SHIFT != 0;
        let alt = self.mods & Mods::ALT != 0;
        let ctrl = self.mods & Mods::CTRL != 0;
        match ev {
            InputEvent::PointerMove { x, y } => {
                let prev = self.pointer;
                self.pointer = (x, y);
                if let Some(p) = self.pan {
                    let mut cam = self.camera();
                    cam.pan(f64::from(x - p.0), f64::from(y - p.1));
                    self.set_camera(cam);
                    self.pan = Some((x, y));
                    return true;
                }
                let w = self.snapped_at((x, y));
                let raw = self.world_at((x, y));
                match &mut self.tool {
                    Tool::Press { at, world, hit } => {
                        let (dx, dy) = (x - at.0, y - at.1);
                        if dx * dx + dy * dy >= DRAG_PX * DRAG_PX {
                            let (world, hit, at) = (*world, *hit, *at);
                            // The press already selected what it hit (unless Alt), so a drag
                            // that starts on an object moves the selection (intuitive mode).
                            let keys = self.session.level().map_or(&[][..], |l| &l.sel);
                            let ends = self.partial_ends();
                            let on_selected = hit.is_some_and(|o| {
                                let k = self.scene.objs.key[o as usize];
                                keys.contains(&k) || ends.iter().any(|e| e.0 == k)
                            });
                            // xschem: Ctrl+drag stretches (swapped by `enable_stretch`),
                            // Ctrl+Shift+drag stretches with kissing wires, Shift+drag copies.
                            let stretch = match (ctrl, shift) {
                                (true, true) => 2,
                                (true, false) => u8::from(!self.opts.stretch),
                                (false, _) => u8::from(self.opts.stretch),
                            };
                            self.tool = if on_selected {
                                Tool::Move {
                                    from: world,
                                    to: w,
                                    lin: Xform::IDENTITY,
                                    copy: shift && !ctrl,
                                    drag: true,
                                    stretch: if shift && !ctrl { 0 } else { stretch },
                                }
                            } else {
                                Tool::Box {
                                    from: self.world_at(at),
                                    to: raw,
                                    additive: shift,
                                    unselect: alt,
                                    partial: !alt && ctrl != self.opts.stretch,
                                }
                            };
                        }
                    }
                    Tool::Box { to, .. } | Tool::Zoom { to, .. } => *to = raw,
                    Tool::Move { from, to, .. } => *to = constrain(*from, w, self.opts.constrain),
                    Tool::Wire { from, to, .. } => *to = constrain(*from, w, self.opts.constrain),
                    Tool::Shape { pts, to, .. } => {
                        let last = *pts.last().expect("a shape tool has its first point");
                        *to = constrain(last, w, self.opts.constrain);
                    }
                    Tool::Place { at, .. } | Tool::Vertex { to: at, .. } => *at = w,
                    Tool::Idle | Tool::ZoomArmed | Tool::UnselectArmed => {
                        // The status line shows the pointer coordinates.
                        return prev != (x, y);
                    }
                }
                true
            }
            InputEvent::PointerDown { x, y, button } => {
                self.pointer = (x, y);
                if self.space_pan {
                    self.space_pan = false;
                    self.pan = None;
                    return true;
                }
                match button {
                    Button::Middle => {
                        self.pan = Some((x, y));
                        return false;
                    }
                    Button::Right => {
                        self.rzoom = Some((x, y));
                        return false;
                    }
                    Button::Left => {}
                }
                let w = self.snapped_at((x, y));
                match std::mem::take(&mut self.tool) {
                    Tool::Move {
                        from,
                        to,
                        lin,
                        copy,
                        drag: false,
                        stretch,
                    } => {
                        self.finish_move(reqs, h, from, to, lin, copy, stretch);
                    }
                    Tool::Wire { from, to, .. } => {
                        let to = if shift {
                            self.nearest_conn(to).unwrap_or(to)
                        } else {
                            to
                        };
                        self.finish_wire(reqs, h, from, to);
                    }
                    Tool::Shape { form, pts, to } => {
                        self.shape_click(reqs, h, view, form, pts, to);
                    }
                    Tool::Place { what, lin, .. } => self.finish_place(reqs, h, view, what, w, lin),
                    Tool::ZoomArmed => {
                        let p = self.world_at((x, y));
                        self.tool = Tool::Zoom { from: p, to: p };
                    }
                    Tool::UnselectArmed => {
                        let p = self.world_at((x, y));
                        self.tool = Tool::Box {
                            from: p,
                            to: p,
                            additive: false,
                            unselect: true,
                            partial: false,
                        };
                    }
                    _ => {
                        // Double click: properties of the object under the pointer.
                        let double = self.last_click.is_some_and(|(t, px, py)| {
                            now.saturating_sub(t) <= DOUBLE_CLICK_MS
                                && (px - x).abs() < 4.0
                                && (py - y).abs() < 4.0
                        });
                        self.last_click = Some((now, x, y));
                        let hit = self.hit_at((x, y));
                        if self.waves.is_some() && view == View::Schematic && !alt && !ctrl {
                            self.cross_probe(h, hit, w);
                        }
                        if let Some(o) = hit.filter(|_| double) {
                            self.last_click = None;
                            let key = self.scene.objs.key[o as usize];
                            self.set_selection(vec![key]);
                            self.open_props(h);
                            return true;
                        }
                        // A vertex of the one selected wire, line, rect or polygon: drag it.
                        if !shift && !alt && !ctrl && self.vertex_press((x, y)) {
                            return true;
                        }
                        // Intuitive wire start: the snapped press equals a connection point of
                        // an unselected object (pin or wire end) exactly (xschem §2.5), within
                        // the hit tolerance on screen, no modifier.
                        if !shift && !alt && !ctrl && view == View::Schematic {
                            let tol = HIT_PX / self.camera().scale;
                            let raw = self.world_at((x, y));
                            let on_conn = self.scene.conn.iter().find(|(p, o)| {
                                *p == w
                                    && (f64::from(p.0) - raw.0).abs() <= tol
                                    && (f64::from(p.1) - raw.1).abs() <= tol
                                    && self.selected.get(*o as usize) != Some(&true)
                            });
                            if let Some(&(p, _)) = on_conn {
                                self.tool = Tool::Wire {
                                    from: p,
                                    to: p,
                                    drag: true,
                                };
                                return true;
                            }
                        }
                        let mut sel: Vec<ObjKey> = self.selection().to_vec();
                        let hit_key = hit.map(|o| self.scene.objs.key[o as usize]);
                        // A press on a partially selected wire keeps the selection, so a drag
                        // moves it like a press on a selected object; a new selection drops it.
                        let partial =
                            hit_key.is_some_and(|k| self.partial_ends().iter().any(|e| e.0 == k));
                        let mut fresh = false;
                        match hit_key {
                            Some(k) if alt => sel.retain(|&s| s != k),
                            Some(k) if shift => sel.push(k),
                            Some(k) if !sel.contains(&k) && !partial => {
                                sel = vec![k];
                                fresh = true;
                            }
                            Some(_) => {}
                            None if !shift => {
                                sel.clear();
                                fresh = true;
                            }
                            None => {}
                        }
                        self.set_selection(sel);
                        if fresh {
                            self.set_partial(Vec::new());
                        }
                        self.tool = Tool::Press {
                            at: (x, y),
                            world: w,
                            hit,
                        };
                    }
                }
                true
            }
            InputEvent::PointerUp { x, y, button } => {
                self.pointer = (x, y);
                match button {
                    Button::Middle => {
                        self.pan = None;
                        return false;
                    }
                    Button::Right => {
                        if let Some(a) = self.rzoom.take() {
                            let (dx, dy) = (x - a.0, y - a.1);
                            if dx * dx + dy * dy >= DRAG_PX * DRAG_PX {
                                let (p, q) = (self.world_at(a), self.world_at((x, y)));
                                self.fit_world(p, q);
                                return true;
                            }
                            // A click: modified actions, end the tool, or the context menu.
                            self.right_click(reqs, h, view, (x, y));
                            return true;
                        }
                        return false;
                    }
                    Button::Left => {}
                }
                match std::mem::take(&mut self.tool) {
                    Tool::Move {
                        from,
                        to,
                        lin,
                        copy,
                        drag: true,
                        stretch,
                    } => self.finish_move(reqs, h, from, to, lin, copy, stretch),
                    Tool::Wire {
                        from,
                        to,
                        drag: true,
                    } => self.finish_wire(reqs, h, from, to),
                    Tool::Vertex { key, index, to, .. } => {
                        self.finish_vertex(reqs, h, key, index, to);
                    }
                    Tool::Box {
                        from,
                        to,
                        additive,
                        unselect,
                        partial,
                    } => {
                        let touch = to.0 < from.0; // right-to-left drag selects touching
                        let hits = scene::box_select(
                            &self.scene.objs,
                            [from.0, from.1, to.0, to.1],
                            touch,
                        );
                        let keys: Vec<ObjKey> = hits
                            .iter()
                            .map(|&o| self.scene.objs.key[o as usize])
                            .collect();
                        let mut sel: Vec<ObjKey> = if additive || unselect {
                            self.selection().to_vec()
                        } else {
                            Vec::new()
                        };
                        if unselect {
                            sel.retain(|k| !keys.contains(k));
                        } else {
                            sel.extend(keys);
                        }
                        self.set_selection(sel);
                        self.box_partial(from, to, partial, additive, unselect);
                    }
                    Tool::Zoom { from, to } => {
                        if from != to {
                            self.fit_world(from, to);
                        }
                    }
                    Tool::Press { .. } => {}
                    other => self.tool = other,
                }
                true
            }
            InputEvent::Wheel { x, y, dy, .. } => {
                let v = self.canvas_rect();
                let mut cam = self.camera();
                if shift {
                    cam.pan(f64::from(dy) * 40.0, 0.0);
                } else if ctrl {
                    cam.pan(0.0, f64::from(dy) * 40.0);
                } else {
                    cam.zoom_at(v, f64::from(x), f64::from(y), ZOOM_STEP.powf(f64::from(dy)));
                }
                self.set_camera(cam);
                true
            }
            _ => false,
        }
    }

    fn fit_world(&mut self, p: (f64, f64), q: (f64, f64)) {
        let b = [
            p.0.min(q.0) as i32,
            p.1.min(q.1) as i32,
            p.0.max(q.0) as i32,
            p.1.max(q.1) as i32,
        ];
        let v = self.canvas_rect();
        let mut cam = self.camera();
        cam.fit(v, b);
        self.set_camera(cam);
    }

    fn manhattan_points(&self, from: Pt, to: Pt) -> Vec<Pt> {
        let mode = if self.opts.orthogonal {
            if (to.0 - from.0).abs() >= (to.1 - from.1).abs() {
                1
            } else {
                2
            }
        } else {
            self.opts.manhattan
        };
        match mode {
            1 => vec![from, (to.0, from.1), to],
            2 => vec![from, (from.0, to.1), to],
            _ => vec![from, to],
        }
    }

    fn finish_wire(&mut self, reqs: &mut Vec<EditRequest>, h: DocHandle, from: Pt, to: Pt) {
        let pts = self.manhattan_points(from, to);
        let (ops, _) = {
            let Some(doc) = self.session.project.doc(h) else {
                return;
            };
            let mut ids = Ids::new(&mut self.rng);
            edit::wires(doc, &pts, &mut ids)
        };
        self.opts.constrain = 0;
        self.commit(reqs, h, "draw wire", ops);
        if self.opts.auto_trim {
            self.trim_after = Some(h);
        }
        if self.opts.persistent && from != to {
            // Options → persistent wire: the next segment starts where this one ended.
            self.tool = Tool::Wire {
                from: to,
                to,
                drag: false,
            };
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_move(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        from: Pt,
        to: Pt,
        lin: Xform,
        copy: bool,
        stretch: u8,
    ) {
        let x = lin.then(Xform::translate(to.0 - from.0, to.1 - from.1));
        self.opts.constrain = 0;
        if x == Xform::IDENTITY && !copy {
            return;
        }
        if !copy && (stretch > 0 || !self.partial_ends().is_empty()) {
            self.finish_stretch(reqs, h, x, stretch);
            if self.opts.auto_trim {
                self.trim_after = Some(h);
            }
            return;
        }
        if copy {
            let (ops, keys) = {
                let Some(doc) = self.session.project.doc(h) else {
                    return;
                };
                let sel = self.session.level().map_or(&[][..], |l| &l.sel);
                let mut ids = Ids::new(&mut self.rng);
                edit::duplicate(doc, sel, x, &mut ids)
            };
            self.commit(reqs, h, "copy", ops);
            if !keys.is_empty() {
                self.set_selection(keys);
            }
        } else {
            let ops = self.with_doc(h, |doc, sel| edit::transform(doc, sel, x));
            self.commit(reqs, h, "move", ops);
        }
    }

    fn finish_place(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        view: View,
        what: Placement,
        at: Pt,
        lin: Xform,
    ) {
        let mut err = None;
        let (label, ops, keys) = {
            let p = &self.session.project;
            let Some(doc) = p.doc(h) else { return };
            let schematic = view == View::Schematic && doc.kind() != FileKind::Shprim;
            let mut ids = Ids::new(&mut self.rng);
            let mut one = |kind: RecordKind, table: TableId, o: Orient, text: &str| {
                let (id, raw) = ids.draw(doc);
                let rec = edit::placed(kind, at, o, text, &id);
                (
                    vec![Op::Insert { rec, at: None }],
                    vec![ObjKey { table, key: raw }],
                )
            };
            match what {
                Placement::Inst(cellref) if schematic => {
                    let prefix = p
                        .resolve(h, &cellref)
                        .ok()
                        .and_then(|r| p.interface(r))
                        .and_then(|i| i.header_value("prefix").map(str::to_owned))
                        .unwrap_or_else(|| "X".to_owned());
                    let name = edit::unique_name(doc, &prefix, &mut Default::default());
                    let (id, raw) = ids.draw(doc);
                    let rec = edit::instance(&cellref, at, lin.orient(Orient::R0), &name, &id);
                    let key = ObjKey {
                        table: TableId::Instances,
                        key: raw,
                    };
                    (
                        "place instance",
                        vec![Op::Insert { rec, at: None }],
                        vec![key],
                    )
                }
                Placement::Label(t) if schematic => {
                    let (o, k) = one(
                        RecordKind::Label,
                        TableId::Labels,
                        lin.orient(Orient::R0),
                        &t,
                    );
                    ("place label", o, k)
                }
                Placement::Port(t, out) if schematic => {
                    let base = if out { Orient::R180 } else { Orient::R0 };
                    let (o, k) = one(RecordKind::Port, TableId::Ports, lin.orient(base), &t);
                    ("place port", o, k)
                }
                Placement::Text(t) if schematic => {
                    let (o, k) = one(RecordKind::Text, TableId::Texts, lin.orient(Orient::R0), &t);
                    ("place text", o, k)
                }
                Placement::SymPin(name) if view == View::Symbol => {
                    let rec = edit::pin(&name, at);
                    ("place pin", vec![Op::Insert { rec, at: None }], Vec::new())
                }
                Placement::Paste(recs) => {
                    let x = lin.then(Xform::translate(at.0, at.1));
                    let (o, k) = edit::insert_copies(doc, recs, x, &mut ids);
                    ("paste", o, k)
                }
                _ => {
                    err = Some("that object does not belong in this view");
                    ("", Vec::new(), Vec::new())
                }
            }
        };
        if let Some(e) = err {
            self.session.status = e.into();
            return;
        }
        self.commit(reqs, h, label, ops);
        if !keys.is_empty() {
            self.set_selection(keys);
        }
    }

    // ------------------------------------------------------------------ spine steps

    /// `plugin::deliver`: snapshots, events, and the plugins' requests into `reqs`.
    fn deliver(&mut self, reqs: &mut Vec<EditRequest>) -> bool {
        if !self.host.has_work() {
            return false;
        }
        let active = self.active().map(|a| a.0);
        let sel: Vec<u32> = self
            .selection()
            .iter()
            .filter(|k| !matches!(k.table, TableId::Pins | TableId::Graphics))
            .map(|k| k.key)
            .collect();
        #[cfg(not(target_arch = "wasm32"))]
        let rows = self.sim.result_rows();
        #[cfg(target_arch = "wasm32")]
        let rows = Vec::new();
        let cell = self
            .cell
            .as_ref()
            .filter(|_| self.cell_key.is_some_and(|k| Some(k.0) == active));
        // What the document tables are filled from: unchanged on a click, so a selection
        // change does not copy every row again.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            let project = &self.session.project;
            for d in project.handles() {
                (d, project.is_dirty(d), self.session.epoch(d)).hash(&mut h);
            }
            (active, cell.map(|c| c.pass), &rows).hash(&mut h);
            h.finish()
        };
        self.host
            .snapshot(&self.session.project, active, &sel, cell, &rows, key);
        let root = self.session.root.clone();
        let d = self.host.deliver(&self.session.project, &root, self.now);
        if !d.commands.is_empty() {
            // `schemify.cmd` from the user config: dispatched like key presses.
            let mut cmds = CommandRequests::default();
            for (name, args) in &d.commands {
                cmds.command.push(self.shell.strings.intern(name));
                cmds.node.push(schemify_ui::Sym::NONE);
                cmds.args.push_str(args);
                cmds.arg_end.push(cmds.args.len() as u32);
            }
            self.dispatch_all(&cmds, reqs);
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.sim.enqueue(d.runs);
        for (doc, label, text, stage) in d.edits {
            let Some(kind) = self.session.project.doc(doc).map(Document::kind) else {
                continue;
            };
            match ops_from_text(&text, kind) {
                Ok(ops) => {
                    let mode = if stage { Mode::Stage } else { Mode::Commit };
                    push_req(reqs, doc, Edit::Tx(Transaction { label, mode, ops }));
                }
                Err(e) => {
                    self.session.status = format!("plugin edit rejected: {:?}", e.first());
                }
            }
        }
        if let Some(l) = d.logs.last() {
            self.session.status.clone_from(l);
        }
        if d.state_changed {
            self.invalidate_cells(); // plugin functions may answer differently now (D20)
        }
        true
    }

    /// `doc::apply_edits`, then per-document bookkeeping: edit epochs, journals, plugin
    /// `DocChanged` events, and the status line for refusals. Every transaction first drops
    /// the D20 caches of the expressions it changes (`docs::drop_stale_caches`).
    fn apply(&mut self, reqs: &mut Vec<EditRequest>) -> bool {
        assert!(reqs.len() <= REQUESTS_MAX);
        for r in reqs.iter_mut() {
            if let Edit::Tx(tx) = &mut r.edit
                && let Some(doc) = self.session.project.doc(r.doc)
            {
                docs::drop_stale_caches(doc, &mut tx.ops);
            }
        }
        let outcomes = apply_edits(&mut self.session.project, reqs);
        assert_eq!(outcomes.len(), reqs.len());
        let mut changed = false;
        for (r, o) in reqs.iter().zip(&outcomes) {
            match o {
                Outcome::Committed { .. }
                | Outcome::Undone { .. }
                | Outcome::Redone { .. }
                | Outcome::Staged { .. }
                | Outcome::Dropped => {
                    self.track_elab(r, o);
                    self.session.bump(r.doc);
                    // A docs-page commit's record waits for its cache ops (`sync_cache`).
                    if !self.amend.as_ref().is_some_and(|a| a.0 == r.doc) {
                        self.session.flush_journal(r.doc);
                    }
                    self.generation = self.generation.wrapping_add(1);
                    let ev = Event::DocChanged {
                        doc: plugins::doc_id(r.doc),
                        epoch: self.session.epoch(r.doc),
                    };
                    let _ = self.host.push(None, &ev);
                    changed = true;
                }
                Outcome::Nothing => self.session.status = "nothing to undo/redo".into(),
                Outcome::Rejected(errs) => {
                    let e = errs.first().map_or(String::new(), |e| {
                        format!("{} {}", e.code, e.code.meaning())
                    });
                    self.session.status = format!("edit rejected: {e}");
                }
                Outcome::Refused(c) => {
                    self.session.status = format!("edit refused: {c} {}", c.meaning());
                }
            }
        }
        reqs.clear();
        changed
    }

    /// Keeps the cell states in step with one applied edit (called before the epoch bump):
    /// the ops it applied are queued on the document's own state (active or parked) while
    /// that state is current, for the incremental recompute; a state that is not current
    /// rebuilds, and a state whose cell instantiates the edited one rebuilds (its interface
    /// may have changed).
    fn track_elab(&mut self, r: &EditRequest, o: &Outcome) {
        let design = matches!(
            o,
            Outcome::Committed { .. } | Outcome::Undone { .. } | Outcome::Redone { .. }
        );
        if !design {
            return;
        }
        match (&r.edit, o) {
            (Edit::Tx(tx), Outcome::Committed { seq }) => {
                if self.commits.len() == COMMITS_KEPT {
                    self.commits.pop_front();
                }
                self.commits.push_back((r.doc, *seq, tx.ops.clone()));
                if let Some((d, _, a_ops, done)) = self.amend.as_mut()
                    && *d == r.doc
                    && tx.ops.starts_with(a_ops)
                {
                    *done = Some(*seq);
                }
            }
            (Edit::Extend(_, ext), Outcome::Committed { seq }) => {
                let mut entry = self.commits.iter_mut().rev();
                if let Some(c) = entry.find(|c| c.0 == r.doc && c.1 == *seq) {
                    c.2.extend(ext.iter().cloned());
                }
            }
            _ => {}
        }
        let epoch = self.session.epoch(r.doc);
        // The ops this outcome applied: an undo or redo touches what the entry's ops touched,
        // the same records, so the same recompute (their direction does not matter to it).
        let applied: Option<&Vec<Op>> = match (&r.edit, o) {
            (Edit::Tx(tx), Outcome::Committed { .. }) => Some(&tx.ops),
            (Edit::Extend(_, ext), Outcome::Committed { .. }) => Some(ext),
            (_, Outcome::Undone { seq } | Outcome::Redone { seq }) => self
                .commits
                .iter()
                .rev()
                .find(|c| c.0 == r.doc && c.1 == *seq)
                .map(|c| &c.2),
            _ => None,
        };
        let project = &self.session.project;
        let mut row: Option<Option<u32>> = None;
        let mut reads_edited = |state: &CellState| {
            let row = *row.get_or_insert_with(|| row_of_doc(project, r.doc));
            row.is_some_and(|row| state.ifaces.iter().skip(1).any(|f| f.row == row))
        };
        // The active state.
        match (self.cell_key, applied) {
            (Some(k), Some(ops)) if k.0 == r.doc && k.1 == epoch => {
                self.cell_ops.extend(ops.iter().cloned());
                self.cell_key = Some((k.0, epoch.wrapping_add(1), k.2)); // the bump that follows
            }
            (Some(k), _) if k.0 != r.doc && !self.cell.as_ref().is_some_and(&mut reads_edited) => {}
            _ => {
                self.cell_key = None;
                self.cell_ops.clear();
            }
        }
        // Parked states, the same way.
        self.parked.retain_mut(|p| {
            if p.doc != r.doc {
                return !reads_edited(&p.state);
            }
            match applied {
                Some(ops) if p.key.0 == epoch => {
                    p.ops.extend(ops.iter().cloned());
                    p.key.0 = epoch.wrapping_add(1);
                    true
                }
                _ => false,
            }
        });
        self.undid |= matches!(o, Outcome::Undone { .. } | Outcome::Redone { .. });
        debug_assert!(self.cell_key.is_some() || self.cell_ops.is_empty());
        debug_assert!(self.parked.len() <= PARKED_MAX);
    }

    /// `elab::CellState::recompute` for the active document: incremental over the queued ops
    /// when `cell` is current, else a full build (a document shown for the first time or
    /// since dropped, an undo of a commit older than [`COMMITS_KEPT`], a plugin change).
    /// Switching documents parks the state of the one left and takes back the parked state
    /// of the one shown while its key holds (D19: values kept for every open document).
    /// Plugin functions are called through the host (D20). Returns whether values may have
    /// changed.
    fn recompute_elab(&mut self) -> bool {
        let Some((h, _)) = self.active() else {
            return false;
        };
        // Every shown document is elaborated: its values (the page, `W = W_in` labels), its
        // violations (the live checks, D13) and its connectivity (net highlight, probes).
        // ponytail: structural edits rebuild the whole state (elab's recompute contract), 136 ms
        // at 100k objects; make elab's instance insert/delete incremental if cells that large
        // get edited structurally.
        let epoch = self.session.epoch(h);
        let fe = self.host.plugins.composed_epoch();
        if let Some(k) = self.cell_key.filter(|k| k.0 != h) {
            if let Some(state) = self.cell.take() {
                let ops = std::mem::take(&mut self.cell_ops);
                self.park(Parked {
                    doc: k.0,
                    key: (k.1, k.2),
                    state,
                    ops,
                });
            }
            self.cell_key = None;
            self.cell_ops.clear();
        }
        if self.cell_key.is_none()
            && let Some(i) = self.parked.iter().position(|p| p.doc == h)
        {
            let p = self.parked.swap_remove(i);
            if p.key == (epoch, fe) {
                self.cell = Some(p.state);
                self.cell_key = Some((h, epoch, fe));
                self.cell_ops = p.ops;
            }
        }
        let current = self.cell.is_some() && self.cell_key == Some((h, epoch, fe));
        if current && self.cell_ops.is_empty() {
            return false;
        }
        let ops = std::mem::take(&mut self.cell_ops);
        let (project, host) = (&self.session.project, &mut self.host);
        let mut funcs = |name: &str, args: &[Value<'_>]| host.call_fn(name, args);
        let values = if current {
            let cell = self.cell.as_mut().expect("current state");
            let done = cell.recompute(project, h, &ops, &mut funcs);
            done.graph || done.evaluated > 0
        } else {
            self.cell = CellState::build(project, h, &mut funcs);
            true
        };
        self.cell_key = self.cell.as_ref().map(|_| (h, epoch, fe));
        // Symbol texts show values: a value change re-stamps the scene; a geometry edit is
        // drawn incrementally (`scene::update`).
        self.scene_dirty |= values;
        assert!(self.cell_ops.is_empty());
        true
    }

    /// Keeps `p` among the parked states (replacing an older one of its document), dropping
    /// states of closed documents and, past [`PARKED_MAX`], the oldest.
    fn park(&mut self, p: Parked) {
        let project = &self.session.project;
        self.parked
            .retain(|q| q.doc != p.doc && project.doc(q.doc).is_some());
        if self.parked.len() >= PARKED_MAX {
            self.parked.remove(0);
        }
        self.parked.push(p);
        assert!(self.parked.len() <= PARKED_MAX);
    }

    /// Drops every cell state (active and parked): plugin functions may answer differently,
    /// a document was recovered or the cell index changed.
    fn invalidate_cells(&mut self) {
        self.cell_key = None;
        self.cell_ops.clear();
        self.parked.clear();
    }

    /// D20 write-back: when a loaded plugin computed a param default or var value whose
    /// `cached <value> <hash>` differs, writes it. After a docs-page edit the write extends
    /// that edit's undo entry and journal record (`Edit::Extend`): one edit, one undo, no
    /// extra journal records. Otherwise it is its own "update plugin cache" commit, skipped
    /// in a frame that undid or redid (it would clear the redo stack). Returns whether
    /// anything was written.
    fn sync_cache(&mut self, reqs: &mut Vec<EditRequest>) -> bool {
        let Some((h, _)) = self.active() else {
            return false;
        };
        let (Some(cell), Some(doc)) = (self.cell.as_ref(), self.session.project.doc(h)) else {
            return false;
        };
        if self.cell_key.map(|k| k.0) != Some(h) || doc.read_only() {
            return false;
        }
        let host = &self.host;
        let ops = docs::cache_ops(doc, cell, |fns| host.fn_hash(fns));
        if ops.is_empty() {
            return false;
        }
        match self.amend.as_ref().and_then(|a| a.3.filter(|_| a.0 == h)) {
            Some(seq) => push_req(reqs, h, Edit::Extend(seq, ops)),
            None if self.undid => return false,
            None => self.commit(reqs, h, "update plugin cache", ops),
        }
        let changed = self.apply(reqs);
        self.recompute_elab();
        changed
    }

    /// Follows the docs page's text editor: takes what is typed from its `field.typed`
    /// requests; when focus leaves it, Escape cancels and anything else (a click elsewhere)
    /// applies the text. Ctrl+Enter submits through `docs.prose_ok` instead.
    fn sync_page(
        &mut self,
        events: &InputEvents,
        cmds: &CommandRequests,
        reqs: &mut Vec<EditRequest>,
    ) -> bool {
        let Some((row, _)) = self.page.editing else {
            return false;
        };
        let focus = self.shell.ui.focus();
        let id = docs::editor_id(row);
        // Typing reaches the page before the focus test: one batch may type and click away.
        let typed = Cmd::FieldTyped.name();
        for i in 0..cmds.len() {
            let s = &self.shell.strings;
            if s.get(cmds.command[i]) == typed
                && s.get(cmds.node[i]) == id
                && let Some(e) = self.page.editing.as_mut()
            {
                cmds.arg(i).clone_into(&mut e.1);
            }
        }
        if !focus.is_none() && self.shell.strings.get(focus) == id {
            self.page.focused = true;
            return false;
        }
        let submitted = (0..cmds.len())
            .any(|i| self.shell.strings.get(cmds.command[i]) == Cmd::DocsProseOk.name());
        if !self.page.focused || submitted {
            return false;
        }
        let escape = events.events.iter().any(
            |e| matches!(e, InputEvent::Key { chord } if chord.key() == schemify_ui::Key::ESCAPE),
        );
        let (row, text) = self.page.editing.take().expect("checked above");
        self.page.focused = false;
        if !escape && let Some((h, _)) = self.active() {
            let ops = self.with_doc(h, |d, _| docs::prose_ops(d, row, &text));
            self.commit_docs(reqs, h, "edit text", ops);
        }
        true
    }

    /// Opens every cell the active schematic instantiates (their symbols draw it) and
    /// refreshes the symbol cache. Skipped while nothing changed since the last call.
    fn load_symbols(&mut self) {
        let Some((h, View::Schematic)) = self.active() else {
            return;
        };
        let key = (h, self.generation);
        if self.symbols_key == Some(key) {
            return;
        }
        self.symbols_key = Some(key);
        let refs: Vec<String> = {
            let Some(doc) = self.session.project.doc(h) else {
                return;
            };
            let s = doc.strings();
            let mut cells: Vec<_> = doc.instances().cell.clone();
            cells.sort_unstable_by_key(|c| c.0);
            cells.dedup();
            cells.iter().map(|&c| s.get(c).to_owned()).collect()
        };
        for cellref in refs {
            let Ok(row) = self.session.project.resolve(h, &cellref) else {
                continue;
            };
            match self.session.open_cell(row) {
                Ok(d) => {
                    let epoch = self.session.epoch(d);
                    if let Some(doc) = self.session.project.doc(d) {
                        self.symbols.refresh(row, d, epoch, doc);
                    }
                }
                Err(e) => self.session.status = e,
            }
        }
    }

    /// `ui::compose` + `ui::layout`: plugin sets (when their composition changed), the
    /// state contribution, then layout. Returns whether the layout changed.
    fn compose_ui(&mut self, relayout: bool) -> bool {
        let mut restyle = false;
        if let Some(sets) = self.host.ui_sets() {
            self.shell.set_plugins(&sets);
            self.commands.set_plugin_rows(self.host.plugins.composed());
            restyle = true;
        }
        let mut relayout = relayout;
        if let Some((h, _)) = self.active() {
            self.page.follow(h);
            // Prose images of the page, read once per path; one that loads lays it out again.
            // ponytail: an image file changed on disk is not reloaded; watch it with the documents.
            if self.docs_shown()
                && let Some(doc) = self.session.project.doc(h)
            {
                let failed = &mut self.page.images_failed;
                let (loaded, err) =
                    docs::load_images(doc, &self.session.root, &mut self.shell.text, failed);
                relayout |= loaded;
                if let Some(e) = err {
                    self.session.status = e;
                }
            }
        }
        let (text, graphs) = self.state_contribution();
        self.graphs = graphs;
        self.shell.set_state(&text);
        let changed = self.shell.refresh(self.viewport, relayout);
        if restyle {
            self.refresh_palette();
        }
        changed
    }

    /// `scene::build` when the active document, view, edit epoch or symbols changed; then
    /// the selection bitmap and marks when the selection changed.
    fn build_scene(&mut self) -> bool {
        let Some((h, view)) = self.active() else {
            return false;
        };
        let epoch = self.session.epoch(h);
        let stale =
            self.scene_dirty || self.scene.built != Some((h, view, epoch, self.symbols.epoch));
        if stale {
            let fe = self.host.plugins.composed_epoch();
            let values = self
                .cell
                .as_ref()
                .filter(|_| self.cell_key == Some((h, epoch, fe)));
            let project = &self.session.project;
            if self.scene_dirty || view != View::Schematic {
                scene::build(
                    &mut self.scene,
                    project,
                    h,
                    view,
                    &self.symbols,
                    epoch,
                    values,
                );
            } else {
                // An edit: only the objects whose rows changed are re-stamped.
                scene::update(&mut self.scene, project, h, &self.symbols, epoch, values);
            }
            self.canvas_gen[0] += 1;
            self.scene_dirty = false;
            let sel: Vec<ObjKey> = self
                .selection()
                .iter()
                .copied()
                .filter(|&k| self.scene.find(k).is_some())
                .collect();
            self.set_selection(sel);
            self.sel_changed = true;
            let unfitted = self.session.level().is_some_and(|l| l.cam.is_none());
            if unfitted && self.canvas_rect().width() > 0.0 {
                let e = self.scene.extent;
                let empty = e == [0; 4];
                self.fit(if empty {
                    [-400 * UNIT, -300 * UNIT, 400 * UNIT, 300 * UNIT]
                } else {
                    e
                });
            }
        }
        if self.sel_changed {
            self.selected = vec![false; self.scene.objs.len()];
            for &k in self.session.level().map_or(&[][..], |l| &l.sel) {
                if let Some(i) = self.scene.find(k) {
                    self.selected[i as usize] = true;
                }
            }
            scene::marks(&self.scene, &self.selected, self.sel_color, &mut self.marks);
            self.canvas_gen[3] += 1;
            self.sel_changed = false;
            let _ = self.host.push(None, &Event::SelectionChanged);
        }
        stale
    }

    /// Rubber bands and previews of the gesture in progress (overlay primitives).
    fn build_overlay(&mut self) {
        let mut o = CanvasPrims::default();
        match &self.tool {
            Tool::Box { from, to, .. } | Tool::Zoom { from, to } => {
                o.rect(
                    from.0 as i32,
                    from.1 as i32,
                    to.0 as i32,
                    to.1 as i32,
                    LAYER_SEL,
                );
            }
            Tool::Move { from, to, lin, .. } => {
                let x = lin.then(Xform::translate(to.0 - from.0, to.1 - from.1));
                scene::push_moved(&self.scene, &self.selected, x, &mut o);
            }
            Tool::Wire { from, to, .. } => {
                let pts = self.manhattan_points(*from, *to);
                for w in pts.windows(2) {
                    o.line(w[0].0, w[0].1, w[1].0, w[1].1, 1);
                }
            }
            Tool::Shape { .. } => {} // `parity_overlay` previews shapes
            Tool::Place { what, at, lin } => {
                let p = &self.session.project;
                let row = match (what, self.active()) {
                    (Placement::Inst(c), Some((h, _))) => p.resolve(h, c).ok(),
                    _ => None,
                };
                match what {
                    Placement::Inst(c) => {
                        let o2 = lin.orient(Orient::R0);
                        let drawn =
                            row.is_some_and(|r| scene::stamp(&self.symbols, r, o2, *at, &mut o));
                        if !drawn {
                            let u = 10 * UNIT;
                            o.rect(at.0 - u, at.1 - u, at.0 + u, at.1 + u, LAYER_SEL);
                            o.text(at.0 - u, at.1 + u + UNIT, 8 * UNIT, LAYER_SEL, c);
                        }
                    }
                    Placement::Label(t)
                    | Placement::Port(t, _)
                    | Placement::Text(t)
                    | Placement::SymPin(t) => {
                        o.rect(
                            at.0 - 2500,
                            at.1 - 2500,
                            at.0 + 2500,
                            at.1 + 2500,
                            LAYER_SEL,
                        );
                        o.text(at.0 + 4 * UNIT, at.1 - 17 * UNIT, 16 * UNIT, LAYER_SEL, t);
                    }
                    Placement::Paste(recs) => {
                        let x = lin.then(Xform::translate(at.0, at.1));
                        scene::preview_records(recs, x, &mut o);
                    }
                }
            }
            Tool::Idle
            | Tool::Press { .. }
            | Tool::ZoomArmed
            | Tool::UnselectArmed
            | Tool::Vertex { .. } => {} // `gesture_overlay` previews vertex drags
        }
        self.parity_overlay(&mut o);
        // Selected texts have no stroke to recolor: outline them (in object order; a frame
        // looks at the selection, not at every object).
        let sel = self.session.level().map_or(&[][..], |l| &l.sel);
        let mut texts: Vec<u32> = sel
            .iter()
            .filter(|k| k.table == TableId::Texts)
            .filter_map(|&k| self.scene.find(k))
            .collect();
        texts.sort_unstable();
        texts.dedup();
        for i in texts {
            let b = self.scene.objs.bbox(i as usize);
            o.rect(b[0], b[1], b[2], b[3], LAYER_SEL);
        }
        self.overlay = o;
    }

    /// The state contribution: session state projected into plugin-declared nodes, and the
    /// docs page's filled plot slots `(graph node id, expression)` for the painter.
    fn state_contribution(&self) -> (String, Vec<(String, String)>) {
        use std::fmt::Write;
        let mut t = String::with_capacity(1024);
        let mut graphs = Vec::new();
        let s = &self.session;
        for (i, tab) in s.tabs.iter().enumerate() {
            let l = tab.stack.last().expect("tabs are never empty");
            let mut name = match &tab.text {
                Some(v) => v.title.clone(),
                None => s.name(l.doc),
            };
            if tab.text.is_some() {
                // A text tab is read-only: no symbol or dirty marks.
            } else if l.view == View::Symbol && tab.stack.len() > 1 {
                name.push_str(" (symbol)");
            }
            if tab.text.is_none() && s.project.is_dirty(l.doc) {
                name.push('*');
            }
            let checked = u8::from(i == s.active);
            if i == 0 {
                let _ = writeln!(t, "set tab.0 label {}", q(&name));
                let _ = writeln!(t, "set tab.0 checked {checked}");
                let _ = writeln!(t, "set tab.0 order -1");
            } else {
                let _ = writeln!(t, "node tab.{i} tab");
                let _ = writeln!(t, "  parent tabs");
                let _ = writeln!(t, "  label {}", q(&name));
                let _ = writeln!(t, "  command tab.select");
                let _ = writeln!(t, "  checked {checked}");
                let _ = writeln!(t, "  order -1");
            }
        }
        let units = |m: i32| {
            let mut v = String::new();
            schemify_format::format_coord(m, &mut v);
            v
        };
        let _ = writeln!(t, "set status.snap value {}", q(&units(self.opts.snap)));
        let _ = writeln!(t, "set status.grid value {}", q(&units(self.opts.grid)));
        let banner = match &self.tool {
            Tool::Wire { .. } => "DRAW WIRE! ",
            Tool::Shape {
                form: Form::Rect, ..
            } => "DRAW RECTANGLE! ",
            Tool::Shape {
                form: Form::Line, ..
            } => "DRAW LINE! ",
            Tool::Shape {
                form: Form::Poly, ..
            } => "DRAW POLYGON! ",
            Tool::Shape {
                form: Form::Arc, ..
            } => "DRAW ARC! ",
            Tool::Shape {
                form: Form::Circle, ..
            } => "DRAW CIRCLE! ",
            Tool::UnselectArmed => "UNSELECT AREA! ",
            Tool::Move { copy: true, .. } => "COPY! ",
            Tool::Move { .. } => "MOVE! ",
            Tool::Place { .. } => "PLACE! ",
            Tool::ZoomArmed | Tool::Zoom { .. } => "ZOOM BOX! ",
            _ => "",
        };
        let (mx, my) = self.pointer_world_snapped();
        let msg = format!(
            "{banner}mouse = {} {}  selected: {}  path: {}  {}",
            units(mx),
            units(my),
            self.selection().len(),
            s.hier_path(),
            s.status
        );
        let _ = writeln!(t, "set status.msg label {}", q(&msg));
        let flags = [
            ("options.draw_grid", self.opts.show_grid),
            ("options.enable_orthogonal_wiring", self.opts.orthogonal),
            ("edit.unconstrained_move", self.opts.constrain == 0),
            ("edit.constrained_horizontal_move", self.opts.constrain == 1),
            ("edit.constrained_vertical_move", self.opts.constrain == 2),
        ];
        for (id, on) in flags {
            let _ = writeln!(t, "set {id} checked {}", u8::from(on));
        }
        // The docs page (D19): blocks under `docs.page`, or the pane removed while hidden.
        match self.active().and_then(|(h, _)| self.session.project.doc(h)) {
            Some(doc) if self.docs_shown() => {
                let h = self.active().expect("doc implies a level").0;
                let cell = self
                    .cell
                    .as_ref()
                    .filter(|_| self.cell_key.is_some_and(|k| k.0 == h));
                #[cfg(not(target_arch = "wasm32"))]
                let plots: Vec<(String, Option<String>)> = self
                    .sim
                    .plots
                    .iter()
                    .map(|p| (p.expr.clone(), p.waves.as_ref().err().cloned()))
                    .collect();
                #[cfg(target_arch = "wasm32")]
                let plots: Vec<(String, Option<String>)> = Vec::new();
                let extras = docs::Extras {
                    plots: &plots,
                    staged: self.staged.as_ref().map(|(l, d)| (l.as_str(), d)),
                };
                docs::contribute(&mut t, doc, cell, &self.page, extras, &mut graphs);
            }
            _ => t.push_str("remove docs.pane\n"),
        }
        match &self.dialog {
            Dialog::None => {}
            Dialog::Text(_, title, typed) => {
                let _ = writeln!(t, "set dialog.enter_text.title label {}", q(title));
                let _ = writeln!(t, "set dialog.enter_text.input value {}", q(typed));
                let _ = writeln!(t, "set dialog.enter_text.input input field.typed");
            }
            Dialog::Ctx(_) | Dialog::Layers | Dialog::File(_) | Dialog::Recover(..) => {}
            Dialog::Choose(c) => {
                let title = match c.purpose {
                    ChooseFor::Insert => "Insert symbol (type to filter, click to place)".into(),
                    ChooseFor::Open => "Open cell".into(),
                    ChooseFor::Waves if !c.note.is_empty() => format!(
                        "Open waveform file for {} (.raw; or type a path + Enter)",
                        c.note
                    ),
                    ChooseFor::Waves => {
                        "Open waveform file (.raw; or type a path + Enter)".to_owned()
                    }
                    ChooseFor::Merge => "Merge: insert a cell's schematic at the pointer".into(),
                    ChooseFor::Delete => "Delete a cell file (pick twice to confirm)".into(),
                    ChooseFor::Compare => "Compare the active schematic with".into(),
                };
                let _ = writeln!(t, "set dialog.choose.title label {}", q(&title));
                let _ = writeln!(t, "set dialog.choose.filter value {}", q(&c.filter));
                let _ = writeln!(t, "set dialog.choose.filter input field.typed");
                for (i, (label, _)) in c.rows.iter().enumerate() {
                    let _ = writeln!(t, "node dialog.choose.row.{i} item");
                    let _ = writeln!(t, "  parent dialog.choose.list");
                    let _ = writeln!(t, "  label {}", q(label));
                    let _ = writeln!(t, "  command choose.pick");
                    let _ = writeln!(t, "  style list-row");
                }
            }
            Dialog::Props(p) => {
                let _ = writeln!(t, "set dialog.props.title label {}", q(&p.title));
                for (i, f) in p.fields.iter().enumerate() {
                    let _ = writeln!(t, "node dialog.props.row.{i} panel");
                    let _ = writeln!(t, "  parent dialog.props.fields");
                    let _ = writeln!(t, "  style dialog-row");
                    let _ = writeln!(t, "node dialog.props.key.{i} label");
                    let _ = writeln!(t, "  parent dialog.props.row.{i}");
                    let _ = writeln!(t, "  label {}", q(&f.label));
                    let _ = writeln!(t, "  style dialog-key");
                    let kind = if f.editable { "text_input" } else { "label" };
                    let _ = writeln!(t, "node dialog.props.val.{i} {kind}");
                    let _ = writeln!(t, "  parent dialog.props.row.{i}");
                    if f.editable {
                        let _ = writeln!(t, "  value {}", q(&f.value));
                        let _ = writeln!(t, "  command props.field");
                        let _ = writeln!(t, "  input field.typed");
                        let _ = writeln!(t, "  style dialog-input");
                    } else {
                        let _ = writeln!(t, "  label {}", q("(computed)"));
                    }
                }
            }
        }
        self.contribute_integration(&mut t);
        self.contribute_parity(&mut t);
        (t, graphs)
    }
}

/// Quotes a record value (format spec §2 escapes), replacing control characters.
fn q(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The window shell's view of an [`Editor`] (`render::run` drives it).
pub struct Window {
    ed: Editor,
    #[cfg(not(target_arch = "wasm32"))]
    start: std::time::Instant,
}

impl Window {
    /// Wraps an editor.
    pub fn new(ed: Editor) -> Window {
        Window {
            ed,
            #[cfg(not(target_arch = "wasm32"))]
            start: std::time::Instant::now(),
        }
    }

    /// The wrapped editor: the `--rpc` server (`crate::rpc`) queues commands into it
    /// between frames.
    pub fn editor(&mut self) -> &mut Editor {
        &mut self.ed
    }

    fn now_ms(&self) -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.start.elapsed().as_millis() as u64
        }
        #[cfg(target_arch = "wasm32")]
        {
            0
        }
    }
}

impl App for Window {
    fn update(&mut self, events: &InputEvents, viewport: (f32, f32)) -> bool {
        let now = self.now_ms();
        self.ed.update(events, viewport, now)
    }

    fn exit_requested(&self) -> bool {
        // Journals are flushed after every edit, so nothing is lost by leaving now.
        self.ed.wants_quit()
    }

    fn paint<P: Painter>(&mut self, p: &mut P, _viewport: (f32, f32)) {
        self.ed.paint(p);
    }

    fn wake_ms(&self) -> Option<u32> {
        self.ed.wake_ms()
    }

    fn ime_area(&self) -> Option<(f32, f32, f32, f32)> {
        let s = &self.ed.shell;
        s.ui.ime_area(&s.tables, &s.layout)
            .map(|r| (r.x, r.y, r.w, r.h))
    }

    fn access(&mut self) -> schemify_render::accesskit::TreeUpdate {
        let s = &self.ed.shell;
        schemify_ui::access::access_tree(&s.tables, &s.layout, &s.strings, &s.ui)
    }
}

/// Opens the editor window and runs until it closes (native: blocks).
///
/// # Errors
///
/// The project or file cannot be opened, or no window can be created.
pub fn run(mut opts: Options) -> Result<(), String> {
    if opts.state_dir.is_none() {
        opts.state_dir = platform::state_dir();
    }
    opts.system_clipboard = true;
    let ed = Editor::new(opts)?;
    schemify_render::run("Schemify", Window::new(ed)).map_err(|e| e.to_string())
}

/// Snaps `to` onto the constraint axis through `from`.
fn constrain(from: Pt, to: Pt, c: u8) -> Pt {
    match c {
        1 => (to.0, from.1),
        2 => (from.0, to.1),
        _ => to,
    }
}

fn push_req(reqs: &mut Vec<EditRequest>, doc: DocHandle, edit: Edit) {
    assert!(reqs.len() < REQUESTS_MAX, "edit requests per frame");
    reqs.push(EditRequest {
        doc,
        edit,
        unix_ms: platform::unix_ms(),
    });
}

fn push_note(status: &mut String, note: &str) {
    if !status.is_empty() {
        status.push_str("; ");
    }
    status.push_str(note);
}

/// A `.shprim` has only a symbol; everything else opens on its schematic.
fn default_view(doc: Option<&Document>) -> View {
    match doc.map(Document::kind) {
        Some(FileKind::Shprim) => View::Symbol,
        _ => View::Schematic,
    }
}

/// The cell row whose open document is `h` (a scan of the cell index); `None` for a
/// document outside it (a new, unsaved file).
fn row_of_doc(project: &schemify_doc::Project, h: DocHandle) -> Option<u32> {
    let doc = project.doc(h)?;
    let cells = project.cells();
    (0..cells.lib.len())
        .find(|&r| project.interface(r).is_some_and(|d| std::ptr::eq(d, doc)))
        .map(|r| r as u32)
}

/// The project directory of a file: the nearest ancestor holding `schemify.project`, else
/// the file's directory.
fn project_root(file: &Path) -> PathBuf {
    let abs = if file.is_absolute() {
        file.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| file.to_path_buf(), |d| d.join(file))
    };
    let mut dir = abs.parent().map(Path::to_path_buf);
    // Bounded by the path depth.
    while let Some(d) = dir {
        if platform::exists(&d.join("schemify.project")) {
            return d;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    abs.parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}
