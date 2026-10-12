//! The waveform viewer's tables (D23, D26, spec `docs/spec/waves.md`): datasets, traces, panes,
//! the shared x view and the cursors, written only by [`Waves::apply`] from a [`Request`] (and by
//! [`Waves::reload`] when new data replaces the datasets).
//!
//! # Data design
//!
//! 1. **In → out.** Datasets ([`RawPlot`]s) and requests (from the UI, the command line, plugins)
//!    in; trace columns, axis ranges, pane properties and cursor positions out, read by `ui`
//!    (rows, input, dialog) and `render` (panes, readouts).
//! 2. **How many.** A viewer shows a handful of panes and tens of traces over a few datasets;
//!    each trace may hold millions of points. Capacities: [`MAX_PANES`], [`MAX_TRACES`],
//!    [`MAX_DATASETS`], [`MAX_BINS`] per histogram, [`MAX_BUS_BITS`] per bus.
//! 3. **How wide.** Pane and dataset indices `u8`, colors RGBA8 `u32`, ranges `f64` (data units),
//!    points and steps `u32` (as in [`RawPlot`]), bus widths `u8` (≤ 32), bins `u16` (≤ 1024).
//! 4. **Access.** Drawing walks traces in order reading pane, color, style and the column
//!    together, so trace rows are SoA columns read in one pass; the column itself is the dataset's
//!    own storage for a plain signal ([`Waves::xy`]), evaluated values only for expressions. A
//!    pane's properties are read together (drawing one pane, the dialog), so they are one
//!    [`PaneProps`] row beside the y-axis column.
//! 5. **Lifetime.** Session. Evaluated columns live with their trace; datasets until replaced.
//! 6. **Parallel.** Traces decimate independently; nothing here needs it yet.

use core::ops::Range;

use schemify_expr::{CalcResult, ColKind, Mode, Program, Scratch, Value};

use crate::calc;
use crate::data::RawPlot;
use crate::decimate::{Scale, bin_counts, extent};
use crate::si::format_si;

/// Most stacked panes in one viewer.
pub const MAX_PANES: usize = 16;
/// Most traces in one viewer.
pub const MAX_TRACES: usize = 64;
/// Most datasets in one viewer (indices are `u8`).
pub const MAX_DATASETS: usize = 256;
/// Most histogram bins in one pane.
pub const MAX_BINS: u16 = 1024;
/// Most bits in one bus trace, so a bus value is an exact `u32`.
pub const MAX_BUS_BITS: usize = 32;

/// Trace colors, assigned in order to new traces (RGBA8, readable on the black plot background).
pub const TRACE_COLORS: [u32; 8] = [
    0xffd8_3cff, // yellow
    0x3ce6_ffff, // cyan
    0xff50_50ff, // red
    0x6cff_5cff, // green
    0xff9f_1aff, // orange
    0xc08c_ffff, // violet
    0x4d94_ffff, // blue
    0xff70_c8ff, // pink
];

/// The colour picker's swatches (spec waves §7): [`TRACE_COLORS`], then eight more.
pub const PALETTE: [u32; 16] = [
    0xffd8_3cff,
    0x3ce6_ffff,
    0xff50_50ff,
    0x6cff_5cff,
    0xff9f_1aff,
    0xc08c_ffff,
    0x4d94_ffff,
    0xff70_c8ff,
    0xffff_ffff, // white
    0xa0a0_a0ff, // grey
    0x00b0_90ff, // teal
    0xb8e0_40ff, // lime
    0xd8b0_80ff, // tan
    0x9090_ffff, // periwinkle
    0xff40_80ff, // rose
    0x40c0_ffff, // sky
];

/// One axis' visible range in data units; on a log axis both ends are positive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    /// Low end (left, bottom).
    pub lo: f64,
    /// High end.
    pub hi: f64,
    /// Logarithmic (decades).
    pub log: bool,
}

impl Axis {
    /// The axis mapped onto pixels `p0` (at `lo`) to `p1` (at `hi`).
    ///
    /// # Panics
    ///
    /// Panics if `p0 == p1` (the range itself is valid by construction).
    pub fn scale(&self, p0: f32, p1: f32) -> Scale {
        Scale::new(self.lo, self.hi, self.log, p0, p1)
    }

    fn t(&self, v: f64) -> f64 {
        if self.log { v.log10() } else { v }
    }

    fn from_t(t0: f64, t1: f64, log: bool) -> Option<Axis> {
        let f = |t: f64| if log { 10f64.powf(t) } else { t };
        let a = Axis {
            lo: f(t0),
            hi: f(t1),
            log,
        };
        a.valid().then_some(a)
    }

    /// Finite, ordered, positive on a log axis, and not narrower than f64 can resolve. Every
    /// axis the viewer holds satisfies this; requests that would break it are refused.
    pub fn valid(&self) -> bool {
        let (t0, t1) = (self.t(self.lo), self.t(self.hi));
        self.lo.is_finite()
            && self.hi.is_finite()
            && (!self.log || self.lo > 0.0)
            && t0 < t1
            && (t1 - t0) > 1e-12 * t0.abs().max(t1.abs()).max(1e-300)
    }

    /// The data value at fraction `f` of the span (0 = `lo`, 1 = `hi`), in transformed units.
    pub fn at(&self, f: f64) -> f64 {
        let (t0, t1) = (self.t(self.lo), self.t(self.hi));
        let t = t0 + f * (t1 - t0);
        if self.log { 10f64.powf(t) } else { t }
    }
}

/// How a trace is stroked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LineStyle {
    /// 1 px.
    Thin = 0,
    /// 2 px (xschem's highlighted wave).
    Bold = 1,
}

impl LineStyle {
    /// Stroke width in logical pixels.
    pub fn width(self) -> f64 {
        match self {
            LineStyle::Thin => 1.0,
            LineStyle::Bold => 2.0,
        }
    }
}

/// A pair of cursors: the vertical ones (x, shared by locked panes) or one pane's horizontal
/// ones (y).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Cursor {
    /// Cursor A (xschem key `a`; `Shift+A` for the horizontal one).
    A = 0,
    /// Cursor B (xschem key `b`; `Shift+B` for the horizontal one).
    B = 1,
}

/// How a pane draws its traces (spec waves §4, §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PaneMode {
    /// Polylines over x (the default).
    Analog = 0,
    /// One lane per trace with logic levels; buses show their values.
    Digital = 1,
    /// Each trace's distribution of values over the pane's own x axis.
    Histogram = 2,
}

/// How bus values and digital readouts are printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Radix {
    /// `0x1f`.
    Hex = 0,
    /// `31`.
    Dec = 1,
}

impl Radix {
    /// The text of bus value `v` in this radix.
    pub fn format(self, v: u32) -> String {
        match self {
            Radix::Hex => format!("0x{v:x}"),
            Radix::Dec => v.to_string(),
        }
    }
}

/// One pane's properties (spec waves §3), set together by [`Request::SetPane`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneProps {
    /// Drawing mode.
    pub mode: PaneMode,
    /// Logic threshold in y data units (digital lanes, bus bits): `y >= threshold` is 1. Finite.
    pub threshold: f64,
    /// Bus value radix.
    pub radix: Radix,
    /// Histogram bins, `2..=MAX_BINS`.
    pub bins: u16,
    /// Shows the shared x axis (false: its own; a histogram always uses its own).
    pub locked: bool,
    /// The one sweep step shown (dataset tracking); `None` shows every step.
    pub step: Option<u32>,
}

impl Default for PaneProps {
    fn default() -> Self {
        PaneProps {
            mode: PaneMode::Analog,
            threshold: 0.5,
            radix: Radix::Hex,
            bins: 32,
            locked: true,
            step: None,
        }
    }
}

impl PaneProps {
    /// Inside the domains of spec waves §3.
    pub fn valid(&self) -> bool {
        self.threshold.is_finite() && (2..=MAX_BINS).contains(&self.bins)
    }

    /// Uses the pane's own x axis rather than the shared one.
    pub fn own_x(&self) -> bool {
        !self.locked || self.mode == PaneMode::Histogram
    }
}

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

impl std::error::Error for ViewError {}

/// Where a trace's displayed y values live.
#[derive(Clone, Debug)]
enum TraceY {
    /// A real raw column, borrowed from the dataset (plain `v(x)` / `i(x)` traces).
    Raw(u32),
    /// Evaluated values (the magnitude for a complex result, 0/1 for a comparison, the integer
    /// value of a bus).
    Owned(Vec<f64>),
}

/// A trace on its own axis (`fft`, per-step results), not the dataset's scale.
#[derive(Clone, Debug)]
struct OwnAxis {
    x: Vec<f64>,
    steps: Vec<Range<u32>>,
}

/// Trace rows (SoA), in legend order.
#[derive(Clone, Debug, Default)]
struct Traces {
    src: Vec<String>,
    dataset: Vec<u8>,
    pane: Vec<u8>,
    color: Vec<u32>,
    style: Vec<LineStyle>,
    complex: Vec<bool>,
    /// Bus width; 0 for a single-signal trace.
    bits: Vec<u8>,
    y: Vec<TraceY>,
    axis: Vec<Option<OwnAxis>>,
}

/// The viewer tables. See the module docs for the data design.
///
/// Accessors that take a trace row `i` panic when `i >= trace_count()`. Rows shift up when an
/// earlier trace is removed, so a row index is only meaningful until the next [`Waves::apply`]
/// or [`Waves::reload`].
#[derive(Clone, Debug)]
pub struct Waves {
    datasets: Vec<RawPlot>,
    traces: Traces,
    /// Pane rows (SoA): y axis, own x axis (used when [`PaneProps::own_x`]), properties,
    /// horizontal cursors.
    panes: Vec<Axis>,
    pane_x: Vec<Axis>,
    props: Vec<PaneProps>,
    hcursors: Vec<[Option<f64>; 2]>,
    x: Axis,
    cursors: [Option<f64>; 2],
}

/// One evaluated trace, before it gets a row.
struct Eval {
    y: TraceY,
    axis: Option<OwnAxis>,
    complex: bool,
    bits: u8,
}

/// What a source evaluates to.
enum Evaluated {
    Trace(Eval),
    /// Measurement text (`src = value`).
    Value(String),
}

impl Waves {
    /// A viewer over `datasets` with one empty pane; x spans the first dataset's scale.
    ///
    /// # Panics
    ///
    /// Panics on more than [`MAX_DATASETS`] datasets.
    pub fn new(datasets: Vec<RawPlot>) -> Waves {
        assert!(datasets.len() <= MAX_DATASETS, "too many datasets");
        let x = datasets
            .first()
            .and_then(|d| {
                let (lo, hi) = span(d.scale(), false);
                fit_range(lo, hi, false, 0.0)
            })
            .unwrap_or(UNIT);
        let w = Waves {
            datasets,
            traces: Traces::default(),
            panes: vec![UNIT],
            pane_x: vec![x],
            props: vec![PaneProps::default()],
            hcursors: vec![[None; 2]],
            x,
            cursors: [None; 2],
        };
        w.check();
        w
    }

    /// Asserts the cross-table invariants (every row's pane and dataset exist, columns are as
    /// long as their axis, ranges and properties are valid, locked panes hold no trace on its
    /// own axis).
    fn check(&self) {
        let t = &self.traces;
        let n = t.src.len();
        let np = self.panes.len();
        assert!(n <= MAX_TRACES && np > 0 && np <= MAX_PANES);
        assert!(
            [t.dataset.len(), t.pane.len(), t.color.len(), t.style.len()]
                .iter()
                .chain(&[t.complex.len(), t.bits.len(), t.y.len(), t.axis.len()])
                .all(|&l| l == n),
            "trace columns differ in length"
        );
        assert!(
            self.pane_x.len() == np && self.props.len() == np && self.hcursors.len() == np,
            "pane columns differ in length"
        );
        assert!(t.pane.iter().all(|&p| usize::from(p) < np));
        assert!(
            t.dataset
                .iter()
                .all(|&d| usize::from(d) < self.datasets.len())
        );
        assert!(t.bits.iter().all(|&b| usize::from(b) <= MAX_BUS_BITS));
        assert!(self.x.valid() && self.panes.iter().all(Axis::valid));
        assert!(self.pane_x.iter().all(Axis::valid) && self.props.iter().all(PaneProps::valid));
        assert!(
            (0..n).all(|i| t.axis[i].is_none() || self.props[usize::from(t.pane[i])].own_x()),
            "a trace on its own axis in a locked pane"
        );
        debug_assert!((0..n).all(|i| {
            let (x, y) = self.xy(i);
            x.len() == y.len()
        }));
    }

    /// The datasets traces are evaluated over.
    pub fn datasets(&self) -> &[RawPlot] {
        &self.datasets
    }

    /// Number of trace rows.
    pub fn trace_count(&self) -> usize {
        self.traces.src.len()
    }

    /// Expression text of trace `i`.
    pub fn src(&self, i: usize) -> &str {
        &self.traces.src[i]
    }

    /// Pane of trace `i`.
    pub fn pane_of(&self, i: usize) -> u8 {
        self.traces.pane[i]
    }

    /// RGBA8 color of trace `i`.
    pub fn color(&self, i: usize) -> u32 {
        self.traces.color[i]
    }

    /// Stroke of trace `i`.
    pub fn style(&self, i: usize) -> LineStyle {
        self.traces.style[i]
    }

    /// True when trace `i` shows the magnitude of a complex result.
    pub fn is_magnitude(&self, i: usize) -> bool {
        self.traces.complex[i]
    }

    /// Width of bus trace `i` in bits; 0 when it is a single signal (spec waves §4).
    pub fn bus_bits(&self, i: usize) -> u8 {
        self.traces.bits[i]
    }

    /// True when trace `i` is on its own axis (`fft` bins, per-step results) rather than its
    /// dataset's scale.
    pub fn has_own_axis(&self, i: usize) -> bool {
        self.traces.axis[i].is_some()
    }

    /// The x and y columns trace `i` draws (equal length). For a plain signal both borrow the
    /// dataset; valid until the next [`Waves::apply`] or [`Waves::reload`].
    ///
    /// # Panics
    ///
    /// Panics when `i >= trace_count()`.
    pub fn xy(&self, i: usize) -> (&[f64], &[f64]) {
        let d = &self.datasets[usize::from(self.traces.dataset[i])];
        let x = match &self.traces.axis[i] {
            Some(a) => &a.x[..],
            None => d.scale(),
        };
        let y = match &self.traces.y[i] {
            TraceY::Raw(v) => d.col(*v as usize),
            TraceY::Owned(y) => &y[..],
        };
        (x, y)
    }

    /// Sweep steps of trace `i`, partitioning its columns.
    pub fn steps(&self, i: usize) -> &[Range<u32>] {
        match &self.traces.axis[i] {
            Some(a) => &a.steps,
            None => self.datasets[usize::from(self.traces.dataset[i])].steps(),
        }
    }

    /// Indices into [`Waves::steps`] that trace `i`'s pane shows: all of them, the tracked one,
    /// or none when the trace has no such step.
    pub fn shown_steps(&self, i: usize) -> Range<usize> {
        let n = self.steps(i).len();
        match self.props[usize::from(self.traces.pane[i])].step {
            None => 0..n,
            Some(s) if (s as usize) < n => s as usize..s as usize + 1,
            Some(_) => n..n,
        }
    }

    /// Y axis of every pane, top to bottom.
    pub fn panes(&self) -> &[Axis] {
        &self.panes
    }

    /// Properties of pane `p`.
    ///
    /// # Panics
    ///
    /// Panics when `p` is not a pane.
    pub fn props(&self, p: usize) -> PaneProps {
        self.props[p]
    }

    /// The x axis pane `p` shows: its own when [`PaneProps::own_x`], else the shared one.
    ///
    /// # Panics
    ///
    /// Panics when `p` is not a pane.
    pub fn x_of(&self, p: usize) -> Axis {
        if self.props[p].own_x() {
            self.pane_x[p]
        } else {
            self.x
        }
    }

    /// Horizontal cursor positions of pane `p` (y data units), indexed by [`Cursor`].
    ///
    /// # Panics
    ///
    /// Panics when `p` is not a pane.
    pub fn hcursors(&self, p: usize) -> [Option<f64>; 2] {
        self.hcursors[p]
    }

    /// The shared x axis (shown by every locked pane).
    pub fn x(&self) -> Axis {
        self.x
    }

    /// Vertical cursor positions (shared-x data units), indexed by [`Cursor`].
    pub fn cursors(&self) -> [Option<f64>; 2] {
        self.cursors
    }

    /// Trace `i`'s value at `x` in the first shown step whose (ascending) x range contains it;
    /// `None` when no shown step does. Interpolated linearly between samples, held from the
    /// last sample for a bus.
    ///
    /// # Time complexity
    ///
    /// O(steps + log points).
    pub fn value_at(&self, i: usize, x: f64) -> Option<f64> {
        let hold = self.traces.bits[i] > 0;
        self.shown_steps(i)
            .find_map(|s| self.sample_at(i, s, x, hold))
    }

    /// Trace `i`'s value at `x` in sweep step `s` (an index into [`Waves::steps`]), as
    /// [`Waves::value_at`] reads one step; `None` when `x` is outside the step or the step's x
    /// is not ascending.
    ///
    /// # Panics
    ///
    /// Panics when `s` is not a step of trace `i`.
    pub fn value_at_step(&self, i: usize, s: usize, x: f64) -> Option<f64> {
        self.sample_at(i, s, x, self.traces.bits[i] > 0)
    }

    /// Trace `i`'s logic value at `x` (spec waves §4): a bus's value, or 1 when the held sample
    /// is at or above its pane's threshold; `None` outside the shown steps.
    pub fn logic_at(&self, i: usize, x: f64) -> Option<u32> {
        let thr = self.props[usize::from(self.traces.pane[i])].threshold;
        let bus = self.traces.bits[i] > 0;
        let y = self
            .shown_steps(i)
            .find_map(|s| self.sample_at(i, s, x, true))?;
        Some(if bus { y as u32 } else { u32::from(y >= thr) })
    }

    fn sample_at(&self, i: usize, s: usize, x: f64, hold: bool) -> Option<f64> {
        let (xs, ys) = self.xy(i);
        let st = &self.steps(i)[s];
        let (sx, sy) = (
            &xs[st.start as usize..st.end as usize],
            &ys[st.start as usize..st.end as usize],
        );
        let (first, last) = (sx[0], sx[sx.len() - 1]);
        if !(first <= x && x <= last) {
            return None;
        }
        if hold {
            // Last sample at or before x: k >= 1 because first <= x.
            let k = sx.partition_point(|&v| v <= x);
            return Some(sy[k - 1]);
        }
        let k = sx.partition_point(|&v| v < x);
        if k == 0 || sx[k] == x {
            return Some(sy[k]);
        }
        let (x0, x1, y0, y1) = (sx[k - 1], sx[k], sy[k - 1], sy[k]);
        Some(y0 + (y1 - y0) * (x - x0) / (x1 - x0))
    }

    /// The step of pane `pane`'s traces whose value at `x` is closest to `y` (in transformed
    /// units on a log y axis): dataset tracking, xschem `t`. `None` for a non-analog pane or
    /// when no step covers `x`.
    ///
    /// # Time complexity
    ///
    /// O(traces in the pane · steps · log points).
    pub fn closest_step(&self, pane: u8, x: f64, y: f64) -> Option<u32> {
        let p = usize::from(pane);
        if p >= self.panes.len() || self.props[p].mode != PaneMode::Analog {
            return None;
        }
        let ya = self.panes[p];
        let mut best: Option<(f64, u32)> = None;
        for i in (0..self.trace_count()).filter(|&i| self.traces.pane[i] == pane) {
            for s in 0..self.steps(i).len() {
                let Some(v) = self.value_at_step(i, s, x) else {
                    continue;
                };
                let d = (ya.t(v) - ya.t(y)).abs();
                if d.is_finite() && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, s as u32));
                }
            }
        }
        best.map(|(_, s)| s)
    }

    /// Fills `out` with trace `i`'s histogram (spec waves §5): its pane's `bins` counts of the
    /// shown steps' samples over the pane's x axis.
    ///
    /// # Time complexity
    ///
    /// O(points of the shown steps).
    pub fn histogram(&self, i: usize, out: &mut Vec<u32>) {
        let p = usize::from(self.traces.pane[i]);
        let ax = self.x_of(p);
        out.clear();
        out.resize(usize::from(self.props[p].bins), 0);
        let (_, y) = self.xy(i);
        let (t0, t1) = (ax.t(ax.lo), ax.t(ax.hi));
        for s in &self.steps(i)[self.shown_steps(i)] {
            bin_counts(&y[s.start as usize..s.end as usize], t0, t1, ax.log, out);
        }
        debug_assert_eq!(out.len(), usize::from(self.props[p].bins));
    }

    /// Replaces the datasets (new results) and re-evaluates every trace over them, keeping panes,
    /// properties, ranges and cursors (spec waves §9). Returns `src: reason` for each trace that
    /// no longer evaluates (its dataset is gone, a signal is missing, it became a value); those
    /// are removed.
    ///
    /// # Panics
    ///
    /// Panics on more than [`MAX_DATASETS`] datasets.
    pub fn reload(&mut self, datasets: Vec<RawPlot>) -> Vec<String> {
        assert!(datasets.len() <= MAX_DATASETS, "too many datasets");
        self.datasets = datasets;
        let n = self.trace_count();
        let mut keep = vec![true; n];
        let mut dropped = Vec::new();
        for (i, kept) in keep.iter_mut().enumerate() {
            let p = usize::from(self.traces.pane[i]);
            let ds = self.traces.dataset[i];
            let res = match self.datasets.get(usize::from(ds)) {
                None => Err(ViewError::NoDataset(ds)),
                Some(d) => evaluate(d, &self.traces.src[i], self.props[p].threshold),
            };
            let why = match res {
                Ok(Evaluated::Trace(e)) if e.axis.is_none() || self.props[p].own_x() => {
                    let t = &mut self.traces;
                    t.y[i] = e.y;
                    t.axis[i] = e.axis;
                    t.complex[i] = e.complex;
                    t.bits[i] = e.bits;
                    continue;
                }
                Ok(Evaluated::Trace(_)) => ViewError::OwnAxis.to_string(),
                Ok(Evaluated::Value(v)) => format!("now a value ({v})"),
                Err(e) => e.to_string(),
            };
            *kept = false;
            dropped.push(format!("{}: {why}", self.traces.src[i]));
        }
        self.remove_rows(|i, _| !keep[i]);
        self.check();
        debug_assert_eq!(self.trace_count() + dropped.len(), n);
        dropped
    }

    /// Applies one request. The spine's `wave::apply`: the writer of these tables.
    ///
    /// # Errors
    ///
    /// See [`ViewError`]; a refused request leaves every table unchanged.
    pub fn apply(&mut self, r: &Request) -> Result<Applied, ViewError> {
        let out = match r {
            Request::AddTrace { src, dataset, pane } => self.add_trace(src, *dataset, *pane)?,
            Request::RemoveTrace { trace } => {
                self.trace_ix(*trace)?;
                let p = self.traces.pane[usize::from(*trace)];
                self.remove_rows(|i, _| i == usize::from(*trace));
                self.autoscale_y(p);
                Applied::Done
            }
            Request::MoveTrace { trace, pane } => {
                let i = self.trace_ix(*trace)?;
                let to = self.pane_ix(*pane)?;
                if self.traces.axis[i].is_some() && !self.props[to].own_x() {
                    return Err(ViewError::OwnAxis);
                }
                let from = self.traces.pane[i];
                let thr = self.props[to].threshold;
                if self.traces.bits[i] > 0 && thr != self.props[usize::from(from)].threshold {
                    let y = self.bus_column(i, thr)?;
                    self.traces.y[i] = TraceY::Owned(y);
                }
                let was_empty = !self.traces.pane.contains(pane);
                self.traces.pane[i] = *pane;
                if was_empty && self.props[to].own_x() {
                    self.autoscale_own_x(to);
                }
                self.autoscale_y(from);
                self.autoscale_y(*pane);
                Applied::Done
            }
            Request::SetStyle { trace, style } => {
                let i = self.trace_ix(*trace)?;
                self.traces.style[i] = *style;
                Applied::Done
            }
            Request::SetColor { trace, color } => {
                let i = self.trace_ix(*trace)?;
                self.traces.color[i] = *color;
                Applied::Done
            }
            Request::AddPane => {
                if self.panes.len() >= MAX_PANES {
                    return Err(ViewError::Full("panes"));
                }
                self.push_pane();
                Applied::Done
            }
            Request::RemovePane { pane } => {
                let p = self.pane_ix(*pane)?;
                if self.panes.len() > 1 {
                    self.remove_rows(|_, tp| tp == *pane);
                    for tp in &mut self.traces.pane {
                        *tp -= u8::from(*tp > *pane);
                    }
                    self.panes.remove(p);
                    self.pane_x.remove(p);
                    self.props.remove(p);
                    self.hcursors.remove(p);
                }
                Applied::Done
            }
            Request::SetPane { pane, props } => self.set_pane(*pane, *props)?,
            Request::ZoomX { pane, at, factor } => {
                let p = self.pane_ix(*pane)?;
                self.map_x(p, |a| zoom(a, *at, *factor));
                Applied::Done
            }
            Request::ZoomY { pane, at, factor } => {
                let p = self.pane_ix(*pane)?;
                self.panes[p] = zoom(self.panes[p], *at, *factor);
                Applied::Done
            }
            Request::PanX { pane, by } => {
                let p = self.pane_ix(*pane)?;
                self.map_x(p, |a| window(a, *by, 1.0 + *by));
                Applied::Done
            }
            Request::PanY { pane, by } => {
                let p = self.pane_ix(*pane)?;
                self.panes[p] = window(self.panes[p], *by, 1.0 + *by);
                Applied::Done
            }
            Request::BoxX { pane, from, to } => {
                let p = self.pane_ix(*pane)?;
                self.map_x(p, |a| window(a, from.min(*to), from.max(*to)));
                Applied::Done
            }
            Request::BoxY { pane, from, to } => {
                let p = self.pane_ix(*pane)?;
                self.panes[p] = window(self.panes[p], from.min(*to), from.max(*to));
                Applied::Done
            }
            Request::SetX { pane, lo, hi } => {
                let p = self.pane_ix(*pane)?;
                let a = Axis {
                    lo: *lo,
                    hi: *hi,
                    log: self.x_of(p).log,
                };
                if !a.valid() {
                    return Err(ViewError::Range(*lo, *hi));
                }
                self.map_x(p, |_| a);
                Applied::Done
            }
            Request::SetY { pane, lo, hi } => {
                let p = self.pane_ix(*pane)?;
                let a = Axis {
                    lo: *lo,
                    hi: *hi,
                    log: self.panes[p].log,
                };
                if !a.valid() {
                    return Err(ViewError::Range(*lo, *hi));
                }
                self.panes[p] = a;
                Applied::Done
            }
            Request::Autoscale => {
                self.autoscale_x();
                for p in 0..self.panes.len() {
                    if self.props[p].own_x() {
                        self.autoscale_own_x(p);
                    }
                }
                for p in 0..self.panes.len() {
                    self.autoscale_y(p as u8);
                }
                Applied::Done
            }
            Request::AutoscaleY { pane } => {
                self.pane_ix(*pane)?;
                self.autoscale_y(*pane);
                Applied::Done
            }
            Request::LogX { pane, on } => {
                let p = self.pane_ix(*pane)?;
                if self.props[p].own_x() {
                    if *on != self.pane_x[p].log {
                        let keep = self.pane_x[p];
                        self.pane_x[p].log = *on;
                        if !self.pane_x[p].valid() && !self.autoscale_own_x(p) {
                            self.pane_x[p] = keep;
                            return Err(ViewError::NotPositive);
                        }
                        self.autoscale_y(*pane);
                    }
                } else if *on != self.x.log {
                    let keep = self.x;
                    self.x.log = *on;
                    if !self.x.valid() && !self.autoscale_x() {
                        self.x = keep;
                        return Err(ViewError::NotPositive);
                    }
                    for p in 0..self.panes.len() {
                        self.autoscale_y(p as u8);
                    }
                }
                Applied::Done
            }
            Request::LogY { pane, on } => {
                let p = self.pane_ix(*pane)?;
                if *on != self.panes[p].log {
                    let keep = self.panes[p];
                    self.panes[p].log = *on;
                    if !self.autoscale_y(*pane) && !self.panes[p].valid() {
                        self.panes[p] = keep;
                        return Err(ViewError::NotPositive);
                    }
                }
                Applied::Done
            }
            Request::Cursor { which, at } => {
                self.cursors[*which as usize] = at.filter(|v| v.is_finite());
                Applied::Done
            }
            Request::HCursor { pane, which, at } => {
                let p = self.pane_ix(*pane)?;
                self.hcursors[p][*which as usize] = at.filter(|v| v.is_finite());
                Applied::Done
            }
            Request::SwapCursors => {
                self.cursors.swap(0, 1);
                Applied::Done
            }
        };
        self.check();
        Ok(out)
    }

    fn trace_ix(&self, t: u8) -> Result<usize, ViewError> {
        let i = usize::from(t);
        (i < self.trace_count())
            .then_some(i)
            .ok_or(ViewError::NoTrace(t))
    }

    fn pane_ix(&self, p: u8) -> Result<usize, ViewError> {
        let i = usize::from(p);
        (i < self.panes.len())
            .then_some(i)
            .ok_or(ViewError::NoPane(p))
    }

    /// Appends a pane row with default properties, its own x starting at the shared view.
    fn push_pane(&mut self) {
        assert!(self.panes.len() < MAX_PANES, "caller checks the capacity");
        self.panes.push(UNIT);
        self.pane_x.push(self.x);
        self.props.push(PaneProps::default());
        self.hcursors.push([None; 2]);
    }

    /// Applies `f` to the x axis pane `p` shows.
    fn map_x(&mut self, p: usize, f: impl FnOnce(Axis) -> Axis) {
        if self.props[p].own_x() {
            self.pane_x[p] = f(self.pane_x[p]);
        } else {
            self.x = f(self.x);
        }
    }

    fn add_trace(&mut self, src: &str, dataset: u8, pane: u8) -> Result<Applied, ViewError> {
        if self.trace_count() >= MAX_TRACES {
            return Err(ViewError::Full("traces"));
        }
        let p = self.pane_ix(pane)?;
        let d = self
            .datasets
            .get(usize::from(dataset))
            .ok_or(ViewError::NoDataset(dataset))?;
        let e = match evaluate(d, src, self.props[p].threshold)? {
            Evaluated::Value(v) => return Ok(Applied::Value(v)),
            Evaluated::Trace(e) => e,
        };
        let pane = self.admit(pane, e.axis.is_some())?;
        let p = usize::from(pane);
        let own = self.props[p].own_x();
        let first = if own {
            !self.traces.pane.contains(&pane)
        } else {
            (0..self.trace_count()).all(|i| self.props[usize::from(self.traces.pane[i])].own_x())
        };
        let used = |c: &u32| self.traces.color.contains(c);
        let color = TRACE_COLORS
            .iter()
            .copied()
            .find(|c| !used(c))
            .unwrap_or(TRACE_COLORS[self.trace_count() % TRACE_COLORS.len()]);
        let t = &mut self.traces;
        t.src.push(src.to_owned());
        t.dataset.push(dataset);
        t.pane.push(pane);
        t.color.push(color);
        t.style.push(LineStyle::Thin);
        t.complex.push(e.complex);
        t.bits.push(e.bits);
        t.y.push(e.y);
        t.axis.push(e.axis);
        if first && own {
            self.autoscale_own_x(p);
        } else if first {
            self.autoscale_x();
        }
        self.autoscale_y(pane);
        Ok(Applied::Trace((self.trace_count() - 1) as u8))
    }

    /// The pane a new trace lands in (spec waves §2): `pane` itself, unless the trace is on its
    /// own axis and `pane` shows the shared one; then `pane` is unlocked when it is empty, else
    /// a new unlocked pane is appended.
    fn admit(&mut self, pane: u8, own_axis: bool) -> Result<u8, ViewError> {
        let p = usize::from(pane);
        if !own_axis || self.props[p].own_x() {
            return Ok(pane);
        }
        if !self.traces.pane.contains(&pane) {
            self.props[p].locked = false;
            self.pane_x[p] = self.x;
            return Ok(pane);
        }
        if self.panes.len() >= MAX_PANES {
            return Err(ViewError::Full("panes"));
        }
        self.push_pane();
        let k = self.panes.len() - 1;
        self.props[k].locked = false;
        debug_assert!(self.props[k].own_x());
        Ok(k as u8)
    }

    /// Validates and applies [`Request::SetPane`]: buses of the pane are re-evaluated when the
    /// threshold changes, and the pane is refitted when its mode or shown step changes.
    fn set_pane(&mut self, pane: u8, props: PaneProps) -> Result<Applied, ViewError> {
        let p = self.pane_ix(pane)?;
        if !props.valid() {
            return Err(ViewError::Props);
        }
        let mine: Vec<usize> = (0..self.trace_count())
            .filter(|&i| self.traces.pane[i] == pane)
            .collect();
        if !props.own_x() && mine.iter().any(|&i| self.traces.axis[i].is_some()) {
            return Err(ViewError::OwnAxis);
        }
        let old = self.props[p];
        // Evaluate first, commit after: a refused request changes nothing.
        let mut buses = Vec::new();
        if props.threshold != old.threshold {
            for &i in mine.iter().filter(|&&i| self.traces.bits[i] > 0) {
                buses.push((i, self.bus_column(i, props.threshold)?));
            }
        }
        for (i, y) in buses {
            self.traces.y[i] = TraceY::Owned(y);
        }
        if props.own_x() && !old.own_x() {
            self.pane_x[p] = self.x;
        }
        self.props[p] = props;
        if props.mode != old.mode || props.step != old.step {
            if props.own_x() {
                self.autoscale_own_x(p);
            }
            self.autoscale_y(pane);
        }
        Ok(Applied::Done)
    }

    /// Bus trace `i` re-evaluated with logic threshold `threshold`.
    fn bus_column(&self, i: usize, threshold: f64) -> Result<Vec<f64>, ViewError> {
        let body = bus_body(&self.traces.src[i]).expect("a bus trace has a bus source");
        let d = &self.datasets[usize::from(self.traces.dataset[i])];
        eval_bus(d, body, threshold).map(|(y, _)| y)
    }

    /// Drops every trace row for which `gone(row, pane)` holds, keeping order.
    fn remove_rows(&mut self, gone: impl Fn(usize, u8) -> bool) {
        let keep: Vec<bool> = (0..self.trace_count())
            .map(|i| !gone(i, self.traces.pane[i]))
            .collect();
        let t = &mut self.traces;
        retain_by(&mut t.src, &keep);
        retain_by(&mut t.dataset, &keep);
        retain_by(&mut t.pane, &keep);
        retain_by(&mut t.color, &keep);
        retain_by(&mut t.style, &keep);
        retain_by(&mut t.complex, &keep);
        retain_by(&mut t.bits, &keep);
        retain_by(&mut t.y, &keep);
        retain_by(&mut t.axis, &keep);
    }

    /// Full extent of the traces in panes that show the shared x axis (the first dataset's
    /// scale when there are none); returns false (and changes nothing) when no data fits the
    /// axis (no positive x on a log axis).
    fn autoscale_x(&mut self) -> bool {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        let mut any = false;
        for i in 0..self.trace_count() {
            if self.props[usize::from(self.traces.pane[i])].own_x() {
                continue;
            }
            any = true;
            let (x, _) = self.xy(i);
            let (a, b) = span(x, self.x.log);
            lo = lo.min(a);
            hi = hi.max(b);
        }
        if !any && let Some(d) = self.datasets.first() {
            (lo, hi) = span(d.scale(), self.x.log);
        }
        match fit_range(lo, hi, self.x.log, 0.0) {
            Some(a) => {
                self.x = a;
                true
            }
            None => false,
        }
    }

    /// Fits pane `p`'s own x axis to its traces: their x extent, or their value extent in
    /// histogram mode; returns false (and changes nothing) when nothing fits.
    fn autoscale_own_x(&mut self, p: usize) -> bool {
        let log = self.pane_x[p].log;
        let histogram = self.props[p].mode == PaneMode::Histogram;
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in (0..self.trace_count()).filter(|&i| usize::from(self.traces.pane[i]) == p) {
            let (x, y) = self.xy(i);
            let col = if histogram { y } else { x };
            for s in &self.steps(i)[self.shown_steps(i)] {
                let (a, b) = span(&col[s.start as usize..s.end as usize], log);
                lo = lo.min(a);
                hi = hi.max(b);
            }
        }
        match fit_range(lo, hi, log, 0.0) {
            Some(a) => {
                self.pane_x[p] = a;
                true
            }
            None => false,
        }
    }

    /// Y range of `pane`'s traces over its visible x, padded 5% (the counts in histogram mode);
    /// returns false (and changes nothing) when the pane shows no fitting sample.
    fn autoscale_y(&mut self, pane: u8) -> bool {
        let p = usize::from(pane);
        let log = self.panes[p].log;
        if self.props[p].mode == PaneMode::Histogram {
            let mut counts = Vec::new();
            let mut most = 1u32;
            for i in (0..self.trace_count()).filter(|&i| self.traces.pane[i] == pane) {
                self.histogram(i, &mut counts);
                most = most.max(counts.iter().copied().max().unwrap_or(0));
            }
            self.panes[p] = Axis {
                lo: if log { 0.5 } else { 0.0 },
                hi: f64::from(most) * 1.05,
                log,
            };
            debug_assert!(self.panes[p].valid());
            return true;
        }
        let xa = self.x_of(p);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in (0..self.trace_count()).filter(|&i| self.traces.pane[i] == pane) {
            let (x, y) = self.xy(i);
            let steps = &self.steps(i)[self.shown_steps(i)];
            let (a, b) = extent(x, y, steps, xa.lo, xa.hi);
            let a = if log && a <= 0.0 {
                min_positive(x, y, steps, xa.lo, xa.hi)
            } else {
                a
            };
            lo = lo.min(a);
            hi = hi.max(b);
        }
        match fit_range(lo, hi, log, 0.05) {
            Some(a) => {
                self.panes[p] = a;
                true
            }
            None => false,
        }
    }
}

const UNIT: Axis = Axis {
    lo: 0.0,
    hi: 1.0,
    log: false,
};

/// Evaluates trace source `src` over `d` (spec waves §2, §4): a bus, a borrowed raw column, an
/// evaluated column, or measurement text. `threshold` is the logic threshold of a bus's bits.
fn evaluate(d: &RawPlot, src: &str, threshold: f64) -> Result<Evaluated, ViewError> {
    if let Some(body) = bus_body(src) {
        let (y, bits) = eval_bus(d, body, threshold)?;
        return Ok(Evaluated::Trace(Eval {
            y: TraceY::Owned(y),
            axis: None,
            complex: false,
            bits,
        }));
    }
    let prog = Program::compile(src, Mode::Calc).map_err(ViewError::Expr)?;
    let mut cols = Vec::new();
    calc::bind(&prog, d, &mut cols).map_err(ViewError::Expr)?;
    if bare_signal(src) && cols.len() == 1 && !d.is_complex() {
        let (k, n) = prog.signals().next().expect("one signal");
        let v = d.find_signal(k, n).expect("bound above");
        return Ok(Evaluated::Trace(Eval {
            y: TraceY::Raw(v as u32),
            axis: None,
            complex: false,
            bits: 0,
        }));
    }
    let mut scratch = Scratch::default();
    let r = calc::eval(&prog, d, |_| None, &mut scratch).map_err(ViewError::Expr)?;
    let c = match r {
        CalcResult::Scalar(v) => {
            let text = match v {
                Value::Int(i) => i.to_string(),
                Value::Num(x) => format_si(x, 6),
                Value::Bool(b) => b.to_string(),
                Value::Cnum { re, im } => {
                    let sign = if im < 0.0 { '-' } else { '+' };
                    format!("{} {sign} {}j", format_si(re, 6), format_si(im.abs(), 6))
                }
                Value::Str(s) => format!("\"{s}\""),
            };
            return Ok(Evaluated::Value(format!("{src} = {text}")));
        }
        CalcResult::Column(c) => c,
    };
    let complex = c.kind == ColKind::Complex;
    let y: Vec<f64> = if complex {
        c.re.iter().zip(&c.im).map(|(r, i)| r.hypot(*i)).collect()
    } else {
        c.re
    };
    if c.axis.is_some() && y.len() == 1 {
        return Ok(Evaluated::Value(format!("{src} = {}", format_si(y[0], 4))));
    }
    let axis = c.axis.map(|a| {
        debug_assert_eq!(a.x.len(), y.len());
        OwnAxis {
            x: a.x,
            steps: a.steps,
        }
    });
    Ok(Evaluated::Trace(Eval {
        y: TraceY::Owned(y),
        axis,
        complex,
        bits: 0,
    }))
}

/// The inside of a bus source `{b1, …, bn}`, or `None` when `src` is not one.
fn bus_body(src: &str) -> Option<&str> {
    src.trim().strip_prefix('{')?.strip_suffix('}')
}

/// A bus's value column (spec waves §4): each bit a real column on `d`'s axis, thresholded at
/// `threshold`, the first bit the most significant. Returns the values and the width.
fn eval_bus(d: &RawPlot, body: &str, threshold: f64) -> Result<(Vec<f64>, u8), ViewError> {
    let parts = split_top(body);
    if parts.len() > MAX_BUS_BITS || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(ViewError::Bus(format!(
            "1 to {MAX_BUS_BITS} comma-separated bits, none empty"
        )));
    }
    let n = d.len();
    let mut acc = vec![0u32; n];
    let mut scratch = Scratch::default();
    for part in &parts {
        let e = part.trim();
        let prog = Program::compile(e, Mode::Calc).map_err(ViewError::Expr)?;
        let r = calc::eval(&prog, d, |_| None, &mut scratch).map_err(ViewError::Expr)?;
        let CalcResult::Column(c) = r else {
            return Err(ViewError::Bus(format!("`{e}` is a value, not a signal")));
        };
        if c.axis.is_some() || c.kind == ColKind::Complex || c.re.len() != n {
            return Err(ViewError::Bus(format!(
                "`{e}` is not a real column on the dataset's axis"
            )));
        }
        for (a, &v) in acc.iter_mut().zip(&c.re) {
            *a = (*a << 1) | u32::from(v >= threshold);
        }
    }
    debug_assert!(!parts.is_empty() && parts.len() <= MAX_BUS_BITS);
    Ok((acc.into_iter().map(f64::from).collect(), parts.len() as u8))
}

/// Splits `s` at the commas outside parentheses, brackets and string quotes.
fn split_top(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut quote, mut escape, mut start) = (0i32, false, false, 0usize);
    for (k, b) in s.bytes().enumerate() {
        if quote {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                quote = false;
            }
            continue;
        }
        match b {
            b'"' => quote = true,
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&s[start..k]);
                start = k + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    debug_assert!(!out.is_empty());
    out
}

fn retain_by<T>(v: &mut Vec<T>, keep: &[bool]) {
    assert_eq!(v.len(), keep.len());
    let mut k = keep.iter();
    v.retain(|_| *k.next().expect("one flag per row"));
}

/// `v(x)` / `i(x)` with nothing around it: the trace can borrow the raw column.
fn bare_signal(src: &str) -> bool {
    let s = src.trim();
    let Some(rest) = s.strip_prefix(['v', 'V', 'i', 'I']) else {
        return false;
    };
    let rest = rest.trim_start();
    rest.len() >= 2
        && rest.starts_with('(')
        && rest.ends_with(')')
        && !rest[1..rest.len() - 1].contains(['(', ')'])
}

/// Min and max of `x`, with the min taken over positive values on a log axis.
fn span(x: &[f64], log: bool) -> (f64, f64) {
    let (lo, hi) = crate::decimate::minmax(x);
    if log && lo <= 0.0 {
        (
            x.iter()
                .copied()
                .filter(|&v| v > 0.0)
                .fold(f64::INFINITY, f64::min),
            hi,
        )
    } else {
        (lo, hi)
    }
}

/// Smallest positive y over the samples with x in `[x_lo, x_hi]` (log-y autoscale).
// ponytail: scalar scan, only on log-y panes whose data crosses zero.
fn min_positive(x: &[f64], y: &[f64], steps: &[Range<u32>], x_lo: f64, x_hi: f64) -> f64 {
    let mut m = f64::INFINITY;
    for s in steps {
        for k in s.start as usize..s.end as usize {
            if x[k] >= x_lo && x[k] <= x_hi && y[k] > 0.0 && y[k] < m {
                m = y[k];
            }
        }
    }
    m
}

/// An axis covering `[lo, hi]` padded by `pad` spans on both sides (in transformed units). A
/// single value widens to ±1 (or one decade each way on a log axis).
fn fit_range(lo: f64, hi: f64, log: bool, pad: f64) -> Option<Axis> {
    if !(lo.is_finite() && hi.is_finite()) || lo > hi || (log && lo <= 0.0) {
        return None;
    }
    let t = |v: f64| if log { v.log10() } else { v };
    let (t0, t1) = (t(lo), t(hi));
    let w = t1 - t0;
    let wide = w > 1e-12 * t0.abs().max(t1.abs()).max(1e-300);
    if wide && pad == 0.0 {
        // Exact data ends: a full zoom shows `0 .. 20u`, not a log10 round trip of them.
        let a = Axis { lo, hi, log };
        return a.valid().then_some(a);
    }
    let (t0, t1) = if wide {
        (t0 - pad * w, t1 + pad * w)
    } else {
        let d = if log { 1.0 } else { t0.abs().max(1.0) * 0.5 };
        (t0 - d, t1 + d)
    };
    Axis::from_t(t0, t1, log)
}

/// `a` zoomed by `factor` about fraction `at`; `a` unchanged when the result is not a valid
/// axis (zooming past f64 resolution, a non-finite factor).
fn zoom(a: Axis, at: f64, factor: f64) -> Axis {
    if !(factor.is_finite() && factor > 0.0 && at.is_finite()) {
        return a;
    }
    let (t0, t1) = (a.t(a.lo), a.t(a.hi));
    let c = t0 + at * (t1 - t0);
    Axis::from_t(c - (c - t0) * factor, c + (t1 - c) * factor, a.log).unwrap_or(a)
}

/// `a` narrowed (or moved) to the fractions `[f0, f1]` of its span; unchanged when invalid.
fn window(a: Axis, f0: f64, f1: f64) -> Axis {
    if !(f0.is_finite() && f1.is_finite()) || f1 - f0 < 1e-9 {
        return a;
    }
    let (t0, t1) = (a.t(a.lo), a.t(a.hi));
    let w = t1 - t0;
    Axis::from_t(t0 + f0 * w, t0 + f1 * w, a.log).unwrap_or(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_top_respects_nesting_and_quotes() {
        assert_eq!(split_top("v(a), v(b)"), vec!["v(a)", " v(b)"]);
        assert_eq!(
            split_top("max(v(a), 1), i(\"x,y\"), v(z[1,2])"),
            vec!["max(v(a), 1)", " i(\"x,y\")", " v(z[1,2])"]
        );
        assert_eq!(split_top(""), vec![""]);
        assert_eq!(split_top("\"a\\\",b\""), vec!["\"a\\\",b\""]);
    }

    #[test]
    fn bus_sources_are_braced() {
        assert_eq!(bus_body(" {v(a), v(b)} "), Some("v(a), v(b)"));
        assert_eq!(bus_body("v(a)"), None);
        assert_eq!(bus_body("{v(a)"), None);
    }
}
