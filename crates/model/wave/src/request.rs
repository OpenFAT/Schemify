//! The viewer's request table and its answers (D23, spec waves §1): every change to the viewer
//! tables is one [`Request`], applied by `Waves::apply`, answered by [`Applied`] or refused with
//! a [`ViewError`] that leaves every table unchanged.
//!
//! Contents:
//! - `pub enum Request`
//! - `pub enum Applied`
//! - `pub enum ViewError`

use crate::pane::{Cursor, LineStyle, PaneProps};

/// One change to the viewer tables. Positions are fractions of the visible span (0 = low end,
/// 1 = high end), so a request means the same at any size and on log axes, unless a field says
/// it is in data units. An x request names a pane: it moves the x axis that pane shows (the
/// shared one when the pane is locked, every locked pane following).
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Compiles and evaluates `src` (calculator mode, or a `{b1, …, bn}` bus) over `dataset`
    /// into a new trace on `pane`; a trace on its own axis may land in a new pane (spec §2).
    AddTrace {
        /// Expression text.
        src: String,
        /// Dataset index.
        dataset: u8,
        /// Pane index.
        pane: u8,
    },
    /// Deletes a trace (later traces move up one row).
    RemoveTrace {
        /// Trace row.
        trace: u8,
    },
    /// Moves a trace to another pane and rescales both panes.
    MoveTrace {
        /// Trace row.
        trace: u8,
        /// Destination pane.
        pane: u8,
    },
    /// Sets a trace's stroke.
    SetStyle {
        /// Trace row.
        trace: u8,
        /// New style.
        style: LineStyle,
    },
    /// Sets a trace's color (RGBA8, the colour picker).
    SetColor {
        /// Trace row.
        trace: u8,
        /// New color.
        color: u32,
    },
    /// Appends an empty pane.
    AddPane,
    /// Deletes a pane and its traces; the last pane stays.
    RemovePane {
        /// Pane index.
        pane: u8,
    },
    /// Replaces a pane's properties (the graph properties dialog).
    SetPane {
        /// Pane index.
        pane: u8,
        /// New properties.
        props: PaneProps,
    },
    /// Zooms the x axis `pane` shows by `factor` (< 1 zooms in) about fraction `at`.
    ZoomX {
        /// Pane index.
        pane: u8,
        /// Fixed point.
        at: f64,
        /// Span multiplier.
        factor: f64,
    },
    /// Zooms one pane's y axis.
    ZoomY {
        /// Pane index.
        pane: u8,
        /// Fixed point.
        at: f64,
        /// Span multiplier.
        factor: f64,
    },
    /// Shifts the x view of `pane` by `by` spans (positive moves the view right).
    PanX {
        /// Pane index.
        pane: u8,
        /// Shift in spans.
        by: f64,
    },
    /// Shifts one pane's y view by `by` spans (positive moves the view up).
    PanY {
        /// Pane index.
        pane: u8,
        /// Shift in spans.
        by: f64,
    },
    /// Zooms the x axis `pane` shows to the span between two fractions (box zoom).
    BoxX {
        /// Pane index.
        pane: u8,
        /// One edge.
        from: f64,
        /// Other edge.
        to: f64,
    },
    /// Zooms one pane's y to the span between two fractions.
    BoxY {
        /// Pane index.
        pane: u8,
        /// One edge.
        from: f64,
        /// Other edge.
        to: f64,
    },
    /// Sets the x axis `pane` shows to `lo .. hi` in data units (keeping its log flag).
    SetX {
        /// Pane index.
        pane: u8,
        /// Low end.
        lo: f64,
        /// High end.
        hi: f64,
    },
    /// Sets one pane's y axis to `lo .. hi` in data units (keeping its log flag).
    SetY {
        /// Pane index.
        pane: u8,
        /// Low end.
        lo: f64,
        /// High end.
        hi: f64,
    },
    /// Fits every x axis to its traces, then every pane's y (xschem `f`, spec §2).
    Autoscale,
    /// One pane's y over its visible x range.
    AutoscaleY {
        /// Pane index.
        pane: u8,
    },
    /// Sets the x axis `pane` shows to log or linear.
    LogX {
        /// Pane index.
        pane: u8,
        /// Log when true.
        on: bool,
    },
    /// Sets one pane's y axis to log or linear.
    LogY {
        /// Pane index.
        pane: u8,
        /// Log when true.
        on: bool,
    },
    /// Shows a vertical cursor at shared-x data value `at`, or hides it (`None`).
    Cursor {
        /// Which one.
        which: Cursor,
        /// Position in x data units.
        at: Option<f64>,
    },
    /// Shows one of `pane`'s horizontal cursors at y data value `at`, or hides it (`None`).
    HCursor {
        /// Pane index.
        pane: u8,
        /// Which one.
        which: Cursor,
        /// Position in y data units.
        at: Option<f64>,
    },
    /// Swaps cursors A and B (xschem `s`).
    SwapCursors,
}

/// What [`Waves::apply`] did.
#[derive(Clone, Debug, PartialEq)]
pub enum Applied {
    /// Tables changed (or the request was a no-op, like zooming past f64 resolution).
    Done,
    /// A trace was added at this row.
    Trace(u8),
    /// The expression evaluated to a measurement (a scalar, or the one value of a per-step
    /// result over a one-step dataset), shown as text instead of a trace.
    Value(String),
}

/// Why a request was refused. Refused requests change nothing.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewError {
    /// The expression failed to compile or evaluate.
    Expr(schemify_expr::Error),
    /// A table is at capacity.
    Full(&'static str),
    /// No such trace row.
    NoTrace(u8),
    /// No such pane.
    NoPane(u8),
    /// No such dataset.
    NoDataset(u8),
    /// A log axis was requested over data with no positive value.
    NotPositive,
    /// An axis range that is empty, reversed, not finite, or not positive on a log axis.
    Range(f64, f64),
    /// Pane properties outside spec waves §3.
    Props,
    /// A trace on its own x axis (`fft`, per-step results) would share a locked pane's axis.
    OwnAxis,
    /// A malformed bus (`{b1, …, bn}`).
    Bus(String),
}

impl std::error::Error for ViewError {}

impl std::fmt::Display for ViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ViewError::Expr(e) => write!(f, "{e}"),
            ViewError::Full(what) => write!(f, "too many {what}"),
            ViewError::NoTrace(t) => write!(f, "no trace {t}"),
            ViewError::NoPane(p) => write!(f, "no pane {p}"),
            ViewError::NoDataset(d) => write!(f, "no dataset {d}"),
            ViewError::NotPositive => f.write_str("no positive values for a log axis"),
            ViewError::Range(lo, hi) => write!(f, "invalid range {lo} .. {hi}"),
            ViewError::Props => {
                f.write_str("invalid pane properties (threshold must be finite, bins 2..=1024)")
            }
            ViewError::OwnAxis => {
                f.write_str("a trace on its own x axis cannot be in a locked pane")
            }
            ViewError::Bus(m) => write!(f, "bus: {m}"),
        }
    }
}
