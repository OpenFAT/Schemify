//! The waveform window (D23, D26, spec `docs/spec/waves.md`), built from UI tables: a static
//! skeleton ([`WAVES_UI`]), the rows that follow the viewer tables ([`WaveUi::contribute`]),
//! pointer input over the graph panes turned into viewer [`Request`]s ([`WaveUi::route`]), and
//! the graph properties dialog ([`PROPS_DIALOG`]). The same pane input drives an embedded graph
//! (D23: one pane, no browser) given just that pane's rectangle.
//!
//! One frame of the window, in spine order (the caller owns the loop):
//!
//! ```text
//! route_input(tables, layout, events)        -> commands          (buttons, keys, list rows)
//! WaveUi::route(waves, geometry, events)     -> requests          (wheel, drags, box zoom)
//! WaveUi::command(waves, command, …)         -> requests          (and the dialog to show)
//! Waves::apply(request); WaveUi::note(…)     -> viewer tables, status text
//! WaveUi::contribute(waves)                  -> ui contribution   (only when `note` says so)
//! ```
//!
//! [`WaveUi`] holds only transient interaction state (drag, hover, selection, filter text, the
//!
//! Contents:
//! - `pub const WAVES_UI`, `pub const PROPS_DIALOG`, `pub const ROW_H`, `pub const DOUBLE_CLICK_MS`
//! - `pub enum Drag`, `pub struct Geometry`, `pub enum DialogFx`, `pub struct Effect`,
//!   `pub struct WaveUi`
//! - `pub fn plot_area(pane) -> Rect`, `Geometry::of(tables, layout, strings) -> Geometry`
//! - `WaveUi::{drag, active_pane, selected_trace, status, dataset, measure, pointer,
//!   set_capacity, rows_fit}`; `contribute` in `rows`, `route` in `pointer`, `command` and
//!   `note` in `command`, the dialog in `dialog`
//! - re-exported from `readout`: `READOUT_HEADINGS`, `READOUT_NAME_CHARS`, `ReadoutHead`,
//!   `ReadoutRow`, `LANE_NAME_CHARS`, `lane_name`, `legend_label`, `readout_head`,
//!   `readout_rows`, `measure_at`, `measure_label`

use schemify_wave::{Cursor, PaneProps, Waves};

use crate::keys::Mods;
use crate::layout::{Rect, UiLayout};
use crate::strings::Strings;
use crate::tables::UiTables;

mod command;
mod dialog;
mod pointer;
mod readout;
mod rows;

pub use readout::{
    LANE_NAME_CHARS, READOUT_HEADINGS, READOUT_NAME_CHARS, ReadoutHead, ReadoutRow, lane_name,
    legend_label, measure_at, measure_label, readout_head, readout_rows,
};

/// The window's static skeleton: toolbar, browser, pane column, trace panel, readout, status,
/// the graph properties dialog, theme and keys. Compose it as the base contribution, then
/// [`WaveUi::contribute`] on top.
pub const WAVES_UI: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/waves-ui.records"));

/// Node id of the graph properties dialog; show it when an [`Effect`] says [`DialogFx::Open`].
pub const PROPS_DIALOG: &str = "waves.props";

/// Height of one signal or trace row in logical pixels.
pub const ROW_H: f32 = 18.0;

/// Gap between two clicks of a double-click.
pub const DOUBLE_CLICK_MS: u64 = 400;

/// A pointer gesture in progress.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Drag {
    /// Nothing held.
    #[default]
    None,
    /// Left drag in a plot: pan this pane's x (the shared one when locked) and y. Holds the last
    /// pointer position.
    Pan {
        /// Pane.
        pane: u8,
        /// Last x.
        x: f32,
        /// Last y.
        y: f32,
    },
    /// Left drag on the y-axis strip: pan this pane's y only (xschem).
    PanY {
        /// Pane.
        pane: u8,
        /// Last y.
        y: f32,
    },
    /// Left drag that started within 6 px of a vertical cursor in a locked pane.
    Cursor {
        /// Pane the drag started in (its plot maps the pointer).
        pane: u8,
        /// Which cursor.
        which: Cursor,
    },
    /// Left drag that started within 6 px of one of a pane's horizontal cursors.
    HCursor {
        /// Pane.
        pane: u8,
        /// Which cursor.
        which: Cursor,
    },
    /// Right drag: box zoom from the first corner to the current pointer.
    Box {
        /// Pane.
        pane: u8,
        /// Press x.
        x0: f32,
        /// Press y.
        y0: f32,
        /// Current x.
        x1: f32,
        /// Current y.
        y1: f32,
    },
}

/// Screen rectangles [`WaveUi::route`] hit-tests, read from one layout by [`Geometry::of`] (or
/// built by hand for an embedded graph: one pane, the rest empty).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geometry {
    /// Each pane's rectangle, top to bottom.
    pub panes: Vec<Rect>,
    /// The signal list.
    pub signals: Rect,
    /// The trace list.
    pub traces: Rect,
    /// The cursor readout panel.
    pub readout: Rect,
}

/// What the caller must do with the properties dialog ([`PROPS_DIALOG`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DialogFx {
    /// Leave it as it is.
    #[default]
    Keep,
    /// Show it (modal); its fields are in the next contribution.
    Open,
    /// Close it.
    Close,
}

/// What the caller must do after routing or a command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Effect {
    /// Paint again (drag overlay, hover, tooltip).
    pub redraw: bool,
    /// Rebuild the contribution and relayout (rows changed).
    pub recompose: bool,
    /// Show or close the properties dialog.
    pub dialog: DialogFx,
}

/// Transient state of one waveform window (or embedded graph).
#[derive(Clone, Debug, Default)]
pub struct WaveUi {
    dataset: u8,
    filter: String,
    expr: String,
    scroll: u32,
    signal_rows: u16,
    trace_rows: u16,
    selected_signal: Option<u32>,
    selected_trace: Option<u8>,
    active_pane: u8,
    status: String,
    drag: Drag,
    mods: Mods,
    pointer: Option<(f32, f32)>,
    last_click: Option<(u32, u64)>,
    last_down: Option<(f32, f32, u64)>,
    cursors_on: [bool; 2],
    measure: bool,
    dialog: Option<Dialog>,
}

/// Text fields of the properties dialog (`waves.props.f.<k>`), in order.
const FIELDS: [&str; 11] = [
    "threshold",
    "bins",
    "step",
    "x min",
    "x max",
    "y min",
    "y max",
    "cursor a",
    "cursor b",
    "cursor A",
    "cursor B",
];

/// The properties dialog's working copy (spec waves §8): toggles edit `props` in place, text
/// fields are parsed on OK and compared with the values shown when it opened.
#[derive(Clone, Debug)]
struct Dialog {
    pane: u8,
    props: PaneProps,
    text: [String; FIELDS.len()],
    shown: [Option<f64>; FIELDS.len()],
}

/// Plot area of a pane: the pane minus room for y tick labels (left), the legend (top) and x
/// tick labels (bottom). [`WaveUi::route`] and the renderer both use it, so a click lands where
/// the trace is drawn.
pub fn plot_area(pane: Rect) -> Rect {
    let (l, t, r, b) = (58.0, 18.0, 10.0, 18.0);
    Rect {
        x: pane.x + l,
        y: pane.y + t,
        w: (pane.w - l - r).max(1.0),
        h: (pane.h - t - b).max(1.0),
    }
}

impl Geometry {
    /// Reads the waves window's rectangles from `layout`. A node not laid out gives an empty
    /// rectangle; panes stop at the first missing one.
    pub fn of(tables: &UiTables, layout: &UiLayout, strings: &Strings) -> Geometry {
        let rect = |id: &str| {
            strings
                .find(id)
                .and_then(|s| tables.nodes.find(s))
                .and_then(|n| layout.row_of(n))
                .map(|r| layout.rect()[r])
        };
        let mut panes = Vec::new();
        for k in 0..schemify_wave::MAX_PANES {
            match rect(&format!("waves.pane.{k}")) {
                Some(r) => panes.push(r),
                None => break,
            }
        }
        let g = Geometry {
            panes,
            signals: rect("waves.signals").unwrap_or_default(),
            traces: rect("waves.traces").unwrap_or_default(),
            readout: rect("waves.readout").unwrap_or_default(),
        };
        debug_assert!(g.panes.len() <= schemify_wave::MAX_PANES);
        g
    }

    fn pane_at(&self, x: f32, y: f32) -> Option<u8> {
        self.panes
            .iter()
            .position(|r| r.contains(x, y))
            .map(|k| k as u8)
    }
}

impl WaveUi {
    /// The gesture in progress (the renderer draws the box-zoom rubber band from it).
    pub fn drag(&self) -> Drag {
        self.drag
    }

    /// The pane commands act on (the last one hovered or clicked).
    pub fn active_pane(&self) -> u8 {
        self.active_pane
    }

    /// The selected trace row, if any.
    pub fn selected_trace(&self) -> Option<u8> {
        self.selected_trace
    }

    /// Status-line text (last error, measurement or confirmation).
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Dataset the browser lists and new traces use.
    pub fn dataset(&self) -> u8 {
        self.dataset
    }

    /// True while the measurement tooltip is on (xschem `m`): the renderer shows `y=`/`x=` at
    /// [`WaveUi::pointer`].
    pub fn measure(&self) -> bool {
        self.measure
    }

    /// Last pointer position over the window, `None` after it left.
    pub fn pointer(&self) -> Option<(f32, f32)> {
        self.pointer
    }

    /// Sets how many signal and trace rows fit, measured from a layout made with fewer rows
    /// (see [`WaveUi::rows_fit`]); returns true when that changes the contribution.
    pub fn set_capacity(&mut self, signal_rows: u16, trace_rows: u16) -> bool {
        let changed = (signal_rows, trace_rows) != (self.signal_rows, self.trace_rows);
        self.signal_rows = signal_rows;
        self.trace_rows = trace_rows;
        changed
    }

    /// Rows of [`ROW_H`] that fit in `r` (a list's rectangle).
    pub fn rows_fit(r: Rect) -> u16 {
        ((r.h - 2.0) / ROW_H).floor().clamp(0.0, 512.0) as u16
    }
}

// ---- private ----

impl Effect {
    fn or(&mut self, o: Effect) {
        self.redraw |= o.redraw;
        self.recompose |= o.recompose;
        if o.dialog != DialogFx::Keep {
            self.dialog = o.dialog;
        }
    }
}

impl WaveUi {
    /// Variables of the current dataset that pass the filter (case-insensitive substring),
    /// skipping the scale column.
    fn visible_signals(&self, waves: &Waves) -> Vec<u32> {
        let Some(d) = waves.datasets().get(usize::from(self.dataset)) else {
            return Vec::new();
        };
        let f = self.filter.to_ascii_lowercase();
        (1..d.vars().len())
            .filter(|&v| f.is_empty() || d.vars()[v].name.to_ascii_lowercase().contains(&f))
            .map(|v| v as u32)
            .collect()
    }
}
