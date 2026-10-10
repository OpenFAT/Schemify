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
//! elab::recompute(doc, dirty)                          → (integration point: not yet)
//! ui::compose(base, plugin sets, state) + ui::layout   → ui_nodes, ui_layout
//! scene::build(doc, symbols)                           → canvas prims, objects (on change)
//! render::draw_ui / draw_canvas                        → scene (in `paint`)
//! ```
//!
//! Event-driven: `update` runs only when the window delivers input, and does nothing that
//! is not caused by it (D39 idle CPU 0%).
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
use schemify_format::{FileKind, IdRng, Orient, RecordKind};
use schemify_plugin::abi::Event;
use schemify_render::canvas::{self, Camera, CanvasPrims, CanvasScratch, Marks, Palette};
use schemify_render::vello::kurbo::Rect as KRect;
use schemify_render::widgets::{UiScratch, draw_ui};
use schemify_render::{App, Painter};
use schemify_ui::{
    Button, CommandRequests, InputEvent, InputEvents, Layer, Mods, Routed, route_input,
};

use crate::commands::{Cmd, Commands, Target};
use crate::edit::{self, Ids};
use crate::geometry::{Pt, Xform, snap};
use crate::platform;
use crate::plugins::{self, Host};
use crate::scene::{self, LAYER_SEL, ObjKey, Scene, Symbols, UNIT};
use crate::session::{Session, View};
use crate::shell::Shell;
use crate::waves::WavesView;

/// Pointer travel (logical px) before a press becomes a drag (xschem: 8 px).
const DRAG_PX: f32 = 8.0;
/// Hit-test tolerance in logical px.
const HIT_PX: f64 = 5.0;
/// Double-click window.
const DOUBLE_CLICK_MS: u64 = 400;
/// Zoom factor per wheel notch and per zoom key (xschem: 1.2).
const ZOOM_STEP: f64 = 1.2;
/// Chooser rows shown at once (the list widget does not scroll yet).
const CHOOSER_ROWS: usize = 24;
/// Edit requests one frame may carry.
const REQUESTS_MAX: usize = 1 << 10;

/// Startup options.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Run without the bundled default UI plugin (proves the core stands alone, D6).
    pub no_default_ui: bool,
    /// A project directory or a cell file to open.
    pub path: Option<PathBuf>,
    /// Id-generation seed (tests pin it; the binary uses OS entropy).
    pub seed: u64,
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
    /// Drawing a symbol line or rectangle.
    Shape {
        /// Rectangle instead of line.
        rect: bool,
        /// First corner.
        from: Pt,
        /// Current corner.
        to: Pt,
    },
    /// An object attached to the pointer; a click drops it.
    Place {
        /// What.
        what: Placement,
        /// Current position.
        at: Pt,
        /// Rotation/flip made in flight.
        lin: Xform,
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
}

#[derive(Clone, Debug)]
struct Chooser {
    purpose: ChooseFor,
    filter: String,
    rows: Vec<(String, Pick)>,
}

#[derive(Clone, Debug)]
struct Field {
    key: String,
    label: String,
    value: String,
    original: String,
    editable: bool,
}

#[derive(Clone, Debug)]
struct Props {
    key: ObjKey,
    title: String,
    fields: Vec<Field>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextFor {
    Label,
    Port(bool),
    Text,
    SymPin,
}

#[derive(Clone, Debug, Default)]
enum Dialog {
    #[default]
    None,
    Choose(Chooser),
    Props(Props),
    /// Purpose, title, typed text (synced every frame).
    Text(TextFor, String, String),
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
    canvas_scratch: CanvasScratch,
    overlay_scratch: CanvasScratch,
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
        let (host, notes) = Host::new(&session.project, &root, opts.no_default_ui);
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
            opts: EditOpts::default(),
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
            overlay_scratch: CanvasScratch::default(),
            ui_scratch: UiScratch::default(),
            waves: None,
            confirm: None,
            quit: false,
            scene_dirty: true,
            sel_changed: false,
            generation: 0,
            symbols_key: None,
            focus_next: None,
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
            }
            None => {
                let h = ed.session.create(FileKind::Shcomp)?;
                ed.session.open_tab(h, View::Schematic);
            }
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

    /// Whether the user asked to quit (the window shell exits the process).
    pub fn wants_quit(&self) -> bool {
        self.quit
    }

    // ------------------------------------------------------------------ the frame spine

    /// One frame: input in, tables updated. Returns whether the picture changed. `now_ms`
    /// is a monotonic clock (double-click detection, waves UI).
    pub fn update(&mut self, events: &InputEvents, viewport: (f32, f32), now_ms: u64) -> bool {
        assert!(viewport.0 >= 0.0 && viewport.1 >= 0.0, "negative viewport");
        if let Some(w) = self.waves.as_mut() {
            let (redraw, close) = w.update(events, viewport, now_ms);
            if close {
                self.waves = None;
                self.shell.invalidate();
            } else {
                return redraw;
            }
        }
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
        let synced = self.sync_fields();
        let mut changed = resized || routed.redraw || routed.relayout || synced;
        let mut reqs: Vec<EditRequest> = Vec::new();
        // 2. dispatch → edit_requests, plugin_events, ui state
        changed |= self.dispatch_all(&routed.commands, &mut reqs);
        for &ev in &routed.canvas {
            changed |= self.gesture(ev, &mut reqs, now_ms);
        }
        // 3–4. plugin::deliver → request_queues; doc::apply_edits (twice at most, so plugins
        // see this frame's DocChanged and their answers land in the same frame).
        for _ in 0..2 {
            changed |= self.deliver(&mut reqs);
            if reqs.is_empty() {
                break;
            }
            changed |= self.apply(&mut reqs);
        }
        // 5. symbols of the active document (I/O) — elab::recompute hooks in here later.
        self.load_symbols();
        // 6. ui compose + layout
        changed |= self.compose_ui(routed.relayout);
        if let Some(id) = self.focus_next.take() {
            let s = &mut self.shell;
            let sym = s.strings.intern(&id);
            s.ui.focus_text(&s.tables, &s.layout, &s.strings, &mut s.text, sym);
        }
        // 7. scene, overlay, marks
        changed |= self.build_scene();
        self.build_overlay();
        self.routed = routed;
        changed
    }

    /// Paints one frame: UI main layer, canvas, UI overlay (menus, dialogs).
    pub fn paint<P: Painter>(&mut self, p: &mut P) {
        if let Some(w) = self.waves.as_mut() {
            w.paint(p);
            return;
        }
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
        let view = self.canvas_rect();
        let cam = self.camera();
        let grid = if self.opts.show_grid {
            self.opts.grid
        } else {
            i32::MAX / 4
        };
        // `draw_canvas` step by step: the overlay gets its own scratch, because the shaped-label
        // cache is keyed by one text table, and sharing it would reshape every scene label
        // each frame (measured: 130 ms at 50k labels).
        p.push_clip(view);
        p.fill_rect(view, schemify_render::rgba(self.palette.background));
        canvas::draw_grid(p, view, &cam, grid, &self.palette, &mut self.canvas_scratch);
        let (prims, palette, text) = (&self.scene.prims, &self.palette, &mut self.shell.text);
        canvas::draw_prims(
            p,
            view,
            &cam,
            prims,
            palette,
            text,
            &mut self.canvas_scratch,
        );
        canvas::draw_prims(
            p,
            view,
            &cam,
            &self.overlay,
            palette,
            text,
            &mut self.overlay_scratch,
        );
        canvas::draw_marks(p, view, &cam, prims, &self.marks, &mut self.canvas_scratch);
        p.pop_clip();
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
            let arg = cmds.arg(i).to_owned();
            changed |= match self.commands.resolve(&name) {
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
                Target::Unknown => {
                    self.session.status = format!("{name}: not available yet");
                    true
                }
            };
        }
        changed
    }

    /// Runs one core command. Returns whether anything visible changed.
    #[allow(clippy::too_many_lines)]
    fn run(&mut self, cmd: Cmd, arg: &str, node: &str, reqs: &mut Vec<EditRequest>) -> bool {
        let modal = !matches!(self.dialog, Dialog::None) || !self.shell.ui.dialog().is_none();
        let dialog_cmd = matches!(
            cmd,
            Cmd::DialogClose
                | Cmd::ChooseFilter
                | Cmd::ChoosePick
                | Cmd::EnterTextOk
                | Cmd::PropsOk
                | Cmd::PropsField
        );
        if modal && !dialog_cmd {
            return false; // xschem: editing keys are no-ops while a dialog is open
        }
        if cmd != Cmd::Close && cmd != Cmd::Quit {
            self.confirm = None;
        }
        let Some((h, view)) = self.active() else {
            return false;
        };
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
                if cmd == Cmd::Cut {
                    let ops = self.with_doc(h, edit::delete);
                    self.commit(reqs, h, "cut", ops);
                    self.set_selection(Vec::new());
                }
            }
            Cmd::Paste => {
                if !self.clipboard.is_empty() {
                    self.tool = Tool::Place {
                        what: Placement::Paste(self.clipboard.clone()),
                        at: ptr,
                        lin: Xform::IDENTITY,
                    };
                }
            }
            Cmd::Duplicate | Cmd::Move | Cmd::MoveKiss | Cmd::MoveStretch => {
                if !self.selection().is_empty() {
                    self.tool = Tool::Move {
                        from: ptr,
                        to: ptr,
                        lin: Xform::IDENTITY,
                        copy: cmd == Cmd::Duplicate,
                        drag: false,
                    };
                }
                if matches!(cmd, Cmd::MoveKiss | Cmd::MoveStretch) {
                    self.session.status =
                        "stretch/kissing moves are not implemented: plain move".into();
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
            Cmd::NewSchematic | Cmd::TabNew | Cmd::NewSymbol => {
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
            Cmd::Highlight | Cmd::Unhighlight => {
                self.session.status =
                    "net highlight: not available yet (elab integration point)".into();
            }
            Cmd::Netlist | Cmd::NetlistTop => {
                self.session.status =
                    "netlist: not available yet (elab/netlist integration point)".into();
            }
            Cmd::Simulate => {
                let bench = self
                    .session
                    .project
                    .doc(h)
                    .is_some_and(|d| d.kind() == FileKind::Shbench);
                self.session.status = if bench {
                    "simulate: not available yet (run integration point)".into()
                } else {
                    "simulate: only a .shbench runs (D11b)".into()
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
                _ => self.session.status = format!("{arg:?}: not a positive number of units"),
            },
            Cmd::Props | Cmd::PropsEditor | Cmd::PropsView => self.open_props(h),
            Cmd::PropsField => self.props_field(node, arg),
            Cmd::PropsOk => self.props_ok(h, reqs),
            Cmd::DialogClose => self.close_dialog(),
            Cmd::ChooseFilter => self.choose_filter(arg),
            Cmd::ChoosePick => self.choose_pick(node, h),
            Cmd::EnterTextOk => self.enter_text_ok(arg, ptr),
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
                if view == View::Symbol {
                    self.tool = Tool::Shape {
                        rect: cmd == Cmd::InsertRect,
                        from: ptr,
                        to: ptr,
                    };
                } else {
                    self.session.status =
                        "lines and rectangles are symbol graphics: use the symbol view (i)".into();
                }
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

    fn enter_text_ok(&mut self, arg: &str, at: Pt) {
        let Dialog::Text(purpose, _, typed) = self.dialog.clone() else {
            self.close_dialog();
            return;
        };
        // Enter in the field submits its text as `arg`; the OK button submits none, and the
        // field lost focus before the click, so the synced copy is used.
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
            ChooseFor::Insert | ChooseFor::Open => {
                let p = &self.session.project;
                let cells = p.cells();
                for row in 0..cells.lib.len() {
                    let kind = cells.kind[row];
                    if purpose == ChooseFor::Insert && kind == FileKind::Shbench {
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

    fn open_waves(&mut self, path: PathBuf) {
        self.close_dialog();
        match platform::read(&path).and_then(|b| WavesView::open(&path.display().to_string(), &b)) {
            Ok(w) => self.waves = Some(w),
            Err(e) => self.session.status = e,
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
        self.dialog = Dialog::Props(Props { key, title, fields });
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
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let Some(loc) = edit::loc_of(doc, p.key) else {
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

    /// Copies the focused dialog field's text into the dialog state every frame (the field
    /// commits only on Enter; clicking OK first must not lose the typing). Returns whether
    /// the chooser filter changed.
    fn sync_fields(&mut self) -> bool {
        let focus = self.shell.ui.focus();
        let Some(ed) = self.shell.ui.editor() else {
            return false;
        };
        if focus.is_none() {
            return false;
        }
        let id = self.shell.strings.get(focus).to_owned();
        let text = ed.raw_text().to_owned();
        match &mut self.dialog {
            Dialog::Props(p) => {
                let i = id
                    .strip_prefix("dialog.props.val.")
                    .and_then(|n| n.parse::<usize>().ok());
                if let Some(f) = i.and_then(|i| p.fields.get_mut(i)).filter(|f| f.editable) {
                    f.value = text.trim().to_owned();
                }
                false
            }
            Dialog::Choose(c) if id == "dialog.choose.filter" && c.filter != text => {
                c.filter = text;
                self.refill_chooser();
                true
            }
            Dialog::Text(_, _, typed) if id == "dialog.enter_text.input" => {
                *typed = text;
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
                            let on_selected = hit
                                .is_some_and(|o| keys.contains(&self.scene.objs.key[o as usize]));
                            self.tool = if on_selected {
                                Tool::Move {
                                    from: world,
                                    to: w,
                                    lin: Xform::IDENTITY,
                                    copy: shift,
                                    drag: true,
                                }
                            } else {
                                Tool::Box {
                                    from: self.world_at(at),
                                    to: raw,
                                    additive: shift,
                                    unselect: alt,
                                }
                            };
                        }
                    }
                    Tool::Box { to, .. } | Tool::Zoom { to, .. } => *to = raw,
                    Tool::Move { from, to, .. } => *to = constrain(*from, w, self.opts.constrain),
                    Tool::Wire { from, to, .. } | Tool::Shape { from, to, .. } => {
                        *to = constrain(*from, w, self.opts.constrain)
                    }
                    Tool::Place { at, .. } => *at = w,
                    Tool::Idle | Tool::ZoomArmed => {
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
                    } => {
                        self.finish_move(reqs, h, from, to, lin, copy);
                    }
                    Tool::Wire { from, to, .. } => {
                        let to = if shift {
                            self.nearest_conn(to).unwrap_or(to)
                        } else {
                            to
                        };
                        self.finish_wire(reqs, h, from, to);
                    }
                    Tool::Shape { rect, from, to } => {
                        if from != to {
                            let rec = edit::shape(rect, from, to);
                            self.commit(
                                reqs,
                                h,
                                if rect { "draw rect" } else { "draw line" },
                                vec![Op::Insert { rec, at: None }],
                            );
                        }
                    }
                    Tool::Place { what, lin, .. } => self.finish_place(reqs, h, view, what, w, lin),
                    Tool::ZoomArmed => {
                        let p = self.world_at((x, y));
                        self.tool = Tool::Zoom { from: p, to: p };
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
                        if let Some(o) = hit.filter(|_| double) {
                            self.last_click = None;
                            let key = self.scene.objs.key[o as usize];
                            self.set_selection(vec![key]);
                            self.open_props(h);
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
                        match hit.map(|o| self.scene.objs.key[o as usize]) {
                            Some(k) if alt => sel.retain(|&s| s != k),
                            Some(k) if shift => sel.push(k),
                            Some(k) if !sel.contains(&k) => sel = vec![k],
                            Some(_) => {}
                            None if !shift => sel.clear(),
                            None => {}
                        }
                        self.set_selection(sel);
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
                            // ponytail: right click opens xschem's context menu; no popup
                            // menus outside the menubar yet.
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
                    } => self.finish_move(reqs, h, from, to, lin, copy),
                    Tool::Wire {
                        from,
                        to,
                        drag: true,
                    } => self.finish_wire(reqs, h, from, to),
                    Tool::Box {
                        from,
                        to,
                        additive,
                        unselect,
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
    }

    fn finish_move(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        from: Pt,
        to: Pt,
        lin: Xform,
        copy: bool,
    ) {
        let x = lin.then(Xform::translate(to.0 - from.0, to.1 - from.1));
        self.opts.constrain = 0;
        if x == Xform::IDENTITY && !copy {
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
        self.host.snapshot(&self.session.project, active, &sel);
        let root = self.session.root.clone();
        let d = self.host.deliver(&self.session.project, &root);
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
        true
    }

    /// `doc::apply_edits`, then per-document bookkeeping: edit epochs, journals, plugin
    /// `DocChanged` events, and the status line for refusals.
    fn apply(&mut self, reqs: &mut Vec<EditRequest>) -> bool {
        assert!(reqs.len() <= REQUESTS_MAX);
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
                    self.session.bump(r.doc);
                    self.session.flush_journal(r.doc);
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
        let text = self.state_contribution();
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
            scene::build(
                &mut self.scene,
                &self.session.project,
                h,
                view,
                &self.symbols,
                epoch,
            );
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
            Tool::Shape { rect, from, to } => {
                if *rect {
                    o.rect(from.0, from.1, to.0, to.1, 4);
                } else {
                    o.line(from.0, from.1, to.0, to.1, 4);
                }
            }
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
            Tool::Idle | Tool::Press { .. } | Tool::ZoomArmed => {}
        }
        // Selected texts have no stroke to recolor: outline them.
        for (i, &on) in self.selected.iter().enumerate() {
            if on && self.scene.objs.key[i].table == TableId::Texts {
                let b = self.scene.objs.bbox(i);
                o.rect(b[0], b[1], b[2], b[3], LAYER_SEL);
            }
        }
        self.overlay = o;
    }

    /// The state contribution: session state projected into plugin-declared nodes.
    fn state_contribution(&self) -> String {
        use std::fmt::Write;
        let mut t = String::with_capacity(1024);
        let s = &self.session;
        for (i, tab) in s.tabs.iter().enumerate() {
            let l = tab.stack.last().expect("tabs are never empty");
            let mut name = s.name(l.doc);
            if l.view == View::Symbol && tab.stack.len() > 1 {
                name.push_str(" (symbol)");
            }
            if s.project.is_dirty(l.doc) {
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
            Tool::Shape { rect: true, .. } => "DRAW RECTANGLE! ",
            Tool::Shape { rect: false, .. } => "DRAW LINE! ",
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
        match &self.dialog {
            Dialog::None => {}
            Dialog::Text(_, title, _) => {
                let _ = writeln!(t, "set dialog.enter_text.title label {}", q(title));
            }
            Dialog::Choose(c) => {
                let title = match c.purpose {
                    ChooseFor::Insert => "Insert symbol (type to filter, click to place)",
                    ChooseFor::Open => "Open cell",
                    ChooseFor::Waves => "Open waveform file (.raw; or type a path + Enter)",
                };
                let _ = writeln!(t, "set dialog.choose.title label {}", q(title));
                let _ = writeln!(t, "set dialog.choose.filter value {}", q(&c.filter));
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
                        let _ = writeln!(t, "  style dialog-input");
                    } else {
                        let _ = writeln!(t, "  label {}", q("(computed)"));
                    }
                }
            }
        }
        t
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
        let changed = self.ed.update(events, viewport, now);
        if self.ed.wants_quit() {
            // Journals are flushed after every edit, so nothing is lost by leaving now.
            std::process::exit(0);
        }
        changed
    }

    fn paint<P: Painter>(&mut self, p: &mut P, _viewport: (f32, f32)) {
        self.ed.paint(p);
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
pub fn run(opts: Options) -> Result<(), String> {
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
