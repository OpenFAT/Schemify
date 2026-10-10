//! The waveform viewer's tables (D23, D26): datasets, traces, panes, the shared x view and the
//! A/B cursors, written only by [`Waves::apply`] from a [`Request`].
//!
//! # Data design
//!
//! 1. **In → out.** Datasets ([`RawPlot`]s) and requests (from the UI, the command line, plugins)
//!    in; trace columns, axis ranges and cursor positions out, read by `ui` (rows, input) and
//!    `render` (panes, readouts).
//! 2. **How many.** A viewer shows a handful of panes and tens of traces over a few datasets;
//!    each trace may hold millions of points. Capacities: [`MAX_PANES`], [`MAX_TRACES`],
//!    [`MAX_DATASETS`].
//! 3. **How wide.** Pane and dataset indices `u8`, colors RGBA8 `u32`, ranges `f64` (data units),
//!    points `u32` (as in [`RawPlot`]).
//! 4. **Access.** Drawing walks traces in order reading pane, color, style and the column
//!    together, so trace rows are SoA columns read in one pass; the column itself is the dataset's
//!    own storage for a plain signal ([`Waves::xy`]), evaluated values only for expressions.
//! 5. **Lifetime.** Session. Evaluated columns live with their trace; datasets until replaced.
//! 6. **Parallel.** Traces decimate independently; nothing here needs it yet.

use core::ops::Range;

use schemify_expr::{CalcResult, ColKind, Mode, Program, Scratch, Value};
use thiserror::Error;

use crate::calc;
use crate::data::RawPlot;
use crate::decimate::{Scale, extent};
use crate::si::format_si;

/// Most stacked panes in one viewer.
pub const MAX_PANES: usize = 16;
/// Most traces in one viewer.
pub const MAX_TRACES: usize = 64;
/// Most datasets in one viewer (indices are `u8`).
pub const MAX_DATASETS: usize = 256;

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

    /// Finite, ordered, positive on a log axis, and not narrower than f64 can resolve.
    fn valid(&self) -> bool {
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

/// The two vertical cursors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Cursor {
    /// Cursor A (xschem key `a`).
    A = 0,
    /// Cursor B (xschem key `b`).
    B = 1,
}

/// One change to the viewer tables. Positions are fractions of the visible span (0 = low end,
/// 1 = high end), so a request means the same at any size and on log axes.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Compiles and evaluates `src` (calculator mode) over `dataset` into a new trace on `pane`.
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
    /// Appends an empty pane.
    AddPane,
    /// Deletes a pane and its traces; the last pane stays.
    RemovePane {
        /// Pane index.
        pane: u8,
    },
    /// Zooms the shared x axis by `factor` (< 1 zooms in) about fraction `at`.
    ZoomX {
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
    /// Shifts the x view by `by` spans (positive moves the view right).
    PanX {
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
    /// Zooms x to the span between two fractions (box zoom).
    BoxX {
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
    /// Full x range of all traces, then every pane's y over it (xschem `f`).
    Autoscale,
    /// One pane's y over the visible x range.
    AutoscaleY {
        /// Pane index.
        pane: u8,
    },
    /// Sets the x axis to log or linear.
    LogX {
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
    /// Shows a cursor at data x `at`, or hides it (`None`).
    Cursor {
        /// Which one.
        which: Cursor,
        /// Position in x data units.
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
    /// The expression evaluated to a measurement (a scalar or one value per sweep step), shown
    /// as text instead of a trace.
    Value(String),
}

/// Why a request was refused. Refused requests change nothing.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum ViewError {
    /// The expression failed to compile or evaluate.
    #[error("{0}")]
    Expr(schemify_expr::Error),
    /// A table is at capacity.
    #[error("too many {0}")]
    Full(&'static str),
    /// No such trace row.
    #[error("no trace {0}")]
    NoTrace(u8),
    /// No such pane.
    #[error("no pane {0}")]
    NoPane(u8),
    /// No such dataset.
    #[error("no dataset {0}")]
    NoDataset(u8),
    /// A log axis was requested over data with no positive value.
    #[error("no positive values for a log axis")]
    NotPositive,
}

/// Where a trace's displayed y values live.
#[derive(Clone, Debug)]
enum TraceY {
    /// A real raw column, borrowed from the dataset (plain `v(x)` / `i(x)` traces).
    Raw(u32),
    /// Evaluated values (the magnitude for a complex result, 0/1 for a comparison).
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
    prog: Vec<Program>,
    dataset: Vec<u8>,
    pane: Vec<u8>,
    color: Vec<u32>,
    style: Vec<LineStyle>,
    complex: Vec<bool>,
    y: Vec<TraceY>,
    axis: Vec<Option<OwnAxis>>,
}

/// The viewer tables. See the module docs for the data design.
#[derive(Clone, Debug)]
pub struct Waves {
    datasets: Vec<RawPlot>,
    traces: Traces,
    panes: Vec<Axis>,
    x: Axis,
    cursors: [Option<f64>; 2],
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
            x,
            cursors: [None; 2],
        };
        w.check();
        w
    }

    /// Asserts the cross-table invariants (every row's pane and dataset exist, columns are as
    /// long as their axis, ranges are valid).
    fn check(&self) {
        let t = &self.traces;
        let n = t.src.len();
        assert!(n <= MAX_TRACES && !self.panes.is_empty() && self.panes.len() <= MAX_PANES);
        assert!(
            [t.prog.len(), t.dataset.len(), t.pane.len(), t.color.len()]
                .iter()
                .chain(&[t.style.len(), t.complex.len(), t.y.len(), t.axis.len()])
                .all(|&l| l == n),
            "trace columns differ in length"
        );
        assert!(t.pane.iter().all(|&p| usize::from(p) < self.panes.len()));
        assert!(
            t.dataset
                .iter()
                .all(|&d| usize::from(d) < self.datasets.len())
        );
        assert!(self.x.valid() && self.panes.iter().all(Axis::valid));
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

    /// Compiled program of trace `i`.
    pub fn program(&self, i: usize) -> &Program {
        &self.traces.prog[i]
    }

    /// Pane of trace `i`.
    pub fn pane_of(&self, i: usize) -> u8 {
        self.traces.pane[i]
    }

    /// Dataset of trace `i`.
    pub fn dataset_of(&self, i: usize) -> u8 {
        self.traces.dataset[i]
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

    /// The x and y columns trace `i` draws (equal length). For a plain signal both borrow the
    /// dataset; valid until the next [`Waves::apply`].
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

    /// Y axis of every pane, top to bottom.
    pub fn panes(&self) -> &[Axis] {
        &self.panes
    }

    /// The shared x axis.
    pub fn x(&self) -> Axis {
        self.x
    }

    /// Cursor positions (x data units), indexed by [`Cursor`].
    pub fn cursors(&self) -> [Option<f64>; 2] {
        self.cursors
    }

    /// Trace `i`'s value at `x`, linearly interpolated in the first sweep step whose (ascending)
    /// x range contains it; `None` when no step does.
    ///
    /// # Time complexity
    ///
    /// O(steps + log points).
    pub fn value_at(&self, i: usize, x: f64) -> Option<f64> {
        let (xs, ys) = self.xy(i);
        // ponytail: one value per trace (the first step that covers x). Per-step readouts belong
        // with dataset tracking (xschem `t`), which picks the step to read.
        for s in self.steps(i) {
            let (sx, sy) = (
                &xs[s.start as usize..s.end as usize],
                &ys[s.start as usize..s.end as usize],
            );
            let (first, last) = (sx[0], sx[sx.len() - 1]);
            if !(first <= x && x <= last) {
                continue;
            }
            let k = sx.partition_point(|&v| v < x);
            if k == 0 || sx[k] == x {
                return Some(sy[k]);
            }
            let (x0, x1, y0, y1) = (sx[k - 1], sx[k], sy[k - 1], sy[k]);
            return Some(y0 + (y1 - y0) * (x - x0) / (x1 - x0));
        }
        None
    }

    /// Applies one request. The spine's `wave::apply`: the only writer of these tables.
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
                self.pane_ix(*pane)?;
                let from = self.traces.pane[i];
                self.traces.pane[i] = *pane;
                self.autoscale_y(from);
                self.autoscale_y(*pane);
                Applied::Done
            }
            Request::SetStyle { trace, style } => {
                let i = self.trace_ix(*trace)?;
                self.traces.style[i] = *style;
                Applied::Done
            }
            Request::AddPane => {
                if self.panes.len() >= MAX_PANES {
                    return Err(ViewError::Full("panes"));
                }
                self.panes.push(UNIT);
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
                }
                Applied::Done
            }
            Request::ZoomX { at, factor } => {
                self.x = zoom(self.x, *at, *factor);
                Applied::Done
            }
            Request::ZoomY { pane, at, factor } => {
                let p = self.pane_ix(*pane)?;
                self.panes[p] = zoom(self.panes[p], *at, *factor);
                Applied::Done
            }
            Request::PanX { by } => {
                self.x = window(self.x, *by, 1.0 + *by);
                Applied::Done
            }
            Request::PanY { pane, by } => {
                let p = self.pane_ix(*pane)?;
                self.panes[p] = window(self.panes[p], *by, 1.0 + *by);
                Applied::Done
            }
            Request::BoxX { from, to } => {
                self.x = window(self.x, from.min(*to), from.max(*to));
                Applied::Done
            }
            Request::BoxY { pane, from, to } => {
                let p = self.pane_ix(*pane)?;
                self.panes[p] = window(self.panes[p], from.min(*to), from.max(*to));
                Applied::Done
            }
            Request::Autoscale => {
                self.autoscale_x();
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
            Request::LogX { on } => {
                if *on != self.x.log {
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

    fn add_trace(&mut self, src: &str, dataset: u8, pane: u8) -> Result<Applied, ViewError> {
        if self.trace_count() >= MAX_TRACES {
            return Err(ViewError::Full("traces"));
        }
        self.pane_ix(pane)?;
        let d = self
            .datasets
            .get(usize::from(dataset))
            .ok_or(ViewError::NoDataset(dataset))?;
        let prog = Program::compile(src, Mode::Calc).map_err(ViewError::Expr)?;
        let mut cols = Vec::new();
        calc::bind(&prog, d, &mut cols).map_err(ViewError::Expr)?;
        let (y, axis, complex) = if bare_signal(src) && cols.len() == 1 && !d.is_complex() {
            let (k, n) = prog.signals().next().expect("one signal");
            let v = d.find_signal(k, n).expect("bound above");
            (TraceY::Raw(v as u32), None, false)
        } else {
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
                    return Ok(Applied::Value(format!("{src} = {text}")));
                }
                CalcResult::Column(c) => c,
            };
            let complex = c.kind == ColKind::Complex;
            let y: Vec<f64> = if complex {
                c.re.iter().zip(&c.im).map(|(r, i)| r.hypot(*i)).collect()
            } else {
                c.re
            };
            if let Some(a) = &c.axis
                && y.len() <= d.steps().len()
            {
                let vals: Vec<String> = y.iter().map(|v| format_si(*v, 4)).collect();
                debug_assert_eq!(a.x.len(), y.len());
                return Ok(Applied::Value(format!("{src} = {}", vals.join(", "))));
            }
            let axis = c.axis.map(|a| OwnAxis {
                x: a.x,
                steps: a.steps,
            });
            (TraceY::Owned(y), axis, complex)
        };
        let first = self.trace_count() == 0;
        let used = |c: &u32| self.traces.color.contains(c);
        let color = TRACE_COLORS
            .iter()
            .copied()
            .find(|c| !used(c))
            .unwrap_or(TRACE_COLORS[self.trace_count() % TRACE_COLORS.len()]);
        let t = &mut self.traces;
        t.src.push(src.to_owned());
        t.prog.push(prog);
        t.dataset.push(dataset);
        t.pane.push(pane);
        t.color.push(color);
        t.style.push(LineStyle::Thin);
        t.complex.push(complex);
        t.y.push(y);
        t.axis.push(axis);
        if first {
            self.autoscale_x();
        }
        self.autoscale_y(pane);
        Ok(Applied::Trace((self.trace_count() - 1) as u8))
    }

    /// Drops every trace row for which `gone(row, pane)` holds, keeping order.
    fn remove_rows(&mut self, gone: impl Fn(usize, u8) -> bool) {
        let keep: Vec<bool> = (0..self.trace_count())
            .map(|i| !gone(i, self.traces.pane[i]))
            .collect();
        let t = &mut self.traces;
        retain_by(&mut t.src, &keep);
        retain_by(&mut t.prog, &keep);
        retain_by(&mut t.dataset, &keep);
        retain_by(&mut t.pane, &keep);
        retain_by(&mut t.color, &keep);
        retain_by(&mut t.style, &keep);
        retain_by(&mut t.complex, &keep);
        retain_by(&mut t.y, &keep);
        retain_by(&mut t.axis, &keep);
    }

    /// Full x extent of every trace; returns false (and changes nothing) when there is no data
    /// that fits the axis (no trace, or no positive x on a log axis).
    fn autoscale_x(&mut self) -> bool {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in 0..self.trace_count() {
            let (x, _) = self.xy(i);
            let (a, b) = span(x, self.x.log);
            lo = lo.min(a);
            hi = hi.max(b);
        }
        if self.trace_count() == 0
            && let Some(d) = self.datasets.first()
        {
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

    /// Y range of `pane`'s traces over the visible x, padded 5%; returns false (and changes
    /// nothing) when the pane shows no fitting sample.
    fn autoscale_y(&mut self, pane: u8) -> bool {
        let p = usize::from(pane);
        let log = self.panes[p].log;
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in (0..self.trace_count()).filter(|&i| self.traces.pane[i] == pane) {
            let (x, y) = self.xy(i);
            let (a, b) = extent(x, y, self.steps(i), self.x.lo, self.x.hi);
            let a = if log && a <= 0.0 {
                min_positive(x, y, self.steps(i), self.x.lo, self.x.hi)
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
