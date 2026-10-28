//! The waveform viewer's tables (D23, D26, spec `docs/spec/waves.md`): datasets, traces, panes,
//! the shared x view and the cursors, written only by `Waves::apply` from a `Request` (and by
//! [`Waves::reload`] when new data replaces the datasets). This file holds the tables, their
//! readers and the reload; `apply.rs` holds the request writer.
//!
//! # Data design
//!
//! 1. **In → out.** Datasets ([`RawPlot`]s) and requests (from the UI, the command line, plugins)
//!    in; trace columns, axis ranges, pane properties and cursor positions out, read by `ui`
//!    (rows, input, dialog) and `render` (panes, readouts).
//! 2. **How many.** A viewer shows a handful of panes and tens of traces over a few datasets;
//!    each trace may hold millions of points. Capacities: [`MAX_PANES`], [`MAX_TRACES`],
//!    [`MAX_DATASETS`], [`MAX_BINS`](crate::MAX_BINS) per histogram,
//!    [`MAX_BUS_BITS`](crate::MAX_BUS_BITS) per bus.
//! 3. **How wide.** Pane and dataset indices `u8`, colors RGBA8 `u32`, ranges `f64` (data units),
//!    points and steps `u32` (as in [`RawPlot`]), bus widths `u8` (≤ 32), bins `u16` (≤ 1024).
//! 4. **Access.** Drawing walks traces in order reading pane, color, style and the column
//!    together, so trace rows are SoA columns read in one pass; the column itself is the
//!    dataset's own storage for a plain signal ([`Waves::xy`]), evaluated values only for
//!    expressions. A pane's properties are read together (drawing one pane, the dialog), so they
//!    are one [`PaneProps`] row beside the y-axis column.
//! 5. **Lifetime.** Session. Evaluated columns live with their trace; datasets until replaced.
//! 6. **Parallel.** Traces decimate independently; nothing here needs it yet.
//!
//! Contents:
//! - `pub struct Waves`: `new`, `datasets`, `trace_count`, `src`, `pane_of`, `color`, `style`,
//!   `is_magnitude`, `bus_bits`, `has_own_axis`, `xy`, `steps`, `shown_steps`, `panes`, `props`,
//!   `x_of`, `hcursors`, `x`, `cursors`, `value_at`, `value_at_step`, `logic_at`,
//!   `closest_step`, `histogram`, `reload`

use core::ops::Range;

use crate::axis::{Axis, UNIT, fit_range, span};
use crate::digital::bin_counts;
use crate::pane::{LineStyle, MAX_DATASETS, MAX_PANES, MAX_TRACES, PaneMode, PaneProps};
use crate::plot::RawPlot;
use crate::request::ViewError;
use crate::trace::{Evaluated, TraceY, Traces, evaluate};

/// The viewer tables. See the module docs for the data design.
///
/// Accessors that take a trace row `i` panic when `i >= trace_count()`. Rows shift up when an
/// earlier trace is removed, so a row index is only meaningful until the next `Waves::apply`
/// or [`Waves::reload`]. Invariants (asserted after every write): every row's pane and dataset
/// exist, columns are as long as their axis, every axis and property row is valid, and no
/// locked pane holds a trace on its own axis.
#[derive(Clone, Debug)]
pub struct Waves {
    pub(crate) datasets: Vec<RawPlot>,
    pub(crate) traces: Traces,
    /// Pane rows (SoA, at most [`MAX_PANES`], at least one): y axis, own x axis (used when
    /// [`PaneProps::own_x`]), properties, horizontal cursors.
    pub(crate) panes: Vec<Axis>,
    pub(crate) pane_x: Vec<Axis>,
    pub(crate) props: Vec<PaneProps>,
    pub(crate) hcursors: Vec<[Option<f64>; 2]>,
    /// The shared x axis and its vertical cursors.
    pub(crate) x: Axis,
    pub(crate) cursors: [Option<f64>; 2],
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

    /// The datasets traces are evaluated over.
    pub fn datasets(&self) -> &[RawPlot] {
        &self.datasets
    }

    /// Number of trace rows.
    pub fn trace_count(&self) -> usize {
        self.traces.len()
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
        self.traces.eval[i].complex
    }

    /// Width of bus trace `i` in bits; 0 when it is a single signal (spec waves §4).
    pub fn bus_bits(&self, i: usize) -> u8 {
        self.traces.eval[i].bits
    }

    /// True when trace `i` is on its own axis (`fft` bins, per-step results) rather than its
    /// dataset's scale.
    pub fn has_own_axis(&self, i: usize) -> bool {
        self.traces.eval[i].axis.is_some()
    }

    /// The x and y columns trace `i` draws (equal length). For a plain signal both borrow the
    /// dataset; valid until the next `Waves::apply` or [`Waves::reload`].
    ///
    /// # Panics
    ///
    /// Panics when `i >= trace_count()`.
    pub fn xy(&self, i: usize) -> (&[f64], &[f64]) {
        let d = &self.datasets[usize::from(self.traces.dataset[i])];
        let e = &self.traces.eval[i];
        let x = e.axis.as_ref().map_or(d.scale(), |a| &a.x[..]);
        let y = match &e.y {
            TraceY::Raw(v) => d.col(*v as usize),
            TraceY::Owned(y) => &y[..],
        };
        (x, y)
    }

    /// Sweep steps of trace `i`, partitioning its columns.
    pub fn steps(&self, i: usize) -> &[Range<u32>] {
        match &self.traces.eval[i].axis {
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

    /// Horizontal cursor positions of pane `p` (y data units), indexed by
    /// [`Cursor`](crate::Cursor).
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

    /// Vertical cursor positions (shared-x data units), indexed by [`Cursor`](crate::Cursor).
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
        let hold = self.bus_bits(i) > 0;
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
        self.sample_at(i, s, x, self.bus_bits(i) > 0)
    }

    /// Trace `i`'s logic value at `x` (spec waves §4): a bus's value, or 1 when the held sample
    /// is at or above its pane's threshold; `None` outside the shown steps.
    pub fn logic_at(&self, i: usize, x: f64) -> Option<u32> {
        let thr = self.props[usize::from(self.traces.pane[i])].threshold;
        let y = self
            .shown_steps(i)
            .find_map(|s| self.sample_at(i, s, x, true))?;
        Some(if self.bus_bits(i) > 0 {
            y as u32
        } else {
            u32::from(y >= thr)
        })
    }

    /// The step of pane `pane`'s traces whose value at `x` is closest to `y` (in transformed
    /// units on a log y axis): dataset tracking, xschem `t`. `None` for a non-analog pane, a
    /// pane that does not exist, or when no step covers `x`.
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
        for i in self.traces_in(pane) {
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
        let bins = usize::from(self.props[p].bins);
        out.clear();
        out.resize(bins, 0);
        let (_, y) = self.xy(i);
        for s in &self.steps(i)[self.shown_steps(i)] {
            let y = &y[s.start as usize..s.end as usize];
            bin_counts(y, ax.t(ax.lo), ax.t(ax.hi), ax.log, out);
        }
        assert_eq!(out.len(), bins, "one count per bin");
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
                    self.traces.eval[i] = e;
                    continue;
                }
                Ok(Evaluated::Trace(_)) => ViewError::OwnAxis.to_string(),
                Ok(Evaluated::Value(v)) => format!("now a value ({v})"),
                Err(e) => e.to_string(),
            };
            *kept = false;
            dropped.push(format!("{}: {why}", self.traces.src[i]));
        }
        self.traces.retain(&keep);
        self.check();
        assert_eq!(self.trace_count() + dropped.len(), n, "kept or named");
        dropped
    }

    /// The rows of the traces drawn in `pane`, in order.
    pub(crate) fn traces_in(&self, pane: u8) -> impl Iterator<Item = usize> + '_ {
        (0..self.trace_count()).filter(move |&i| self.traces.pane[i] == pane)
    }

    /// Asserts the cross-table invariants listed on [`Waves`].
    pub(crate) fn check(&self) {
        let t = &self.traces;
        let n = t.len();
        let np = self.panes.len();
        assert!(n <= MAX_TRACES, "trace capacity");
        assert!(np > 0 && np <= MAX_PANES, "pane capacity");
        assert!(
            [
                t.dataset.len(),
                t.pane.len(),
                t.color.len(),
                t.style.len(),
                t.eval.len()
            ]
            .iter()
            .all(|&l| l == n),
            "trace columns differ in length"
        );
        assert!(
            self.pane_x.len() == np && self.props.len() == np && self.hcursors.len() == np,
            "pane columns differ in length"
        );
        assert!(
            t.pane.iter().all(|&p| usize::from(p) < np),
            "trace in no pane"
        );
        assert!(
            t.dataset
                .iter()
                .all(|&d| usize::from(d) < self.datasets.len()),
            "trace over no dataset"
        );
        assert!(self.x.valid() && self.panes.iter().all(Axis::valid), "axis");
        assert!(self.pane_x.iter().all(Axis::valid), "own x axis");
        assert!(self.props.iter().all(PaneProps::valid), "pane properties");
        assert!(
            (0..n).all(|i| !self.has_own_axis(i) || self.props[usize::from(t.pane[i])].own_x()),
            "a trace on its own axis in a locked pane"
        );
        debug_assert!((0..n).all(|i| {
            let (x, y) = self.xy(i);
            x.len() == y.len()
        }));
    }
}

// ---- private ----

impl Waves {
    /// Trace `i`'s value at `x` in step `s`: held from the last sample at or before `x` when
    /// `hold`, else interpolated; `None` outside the step's x range.
    fn sample_at(&self, i: usize, s: usize, x: f64, hold: bool) -> Option<f64> {
        let (xs, ys) = self.xy(i);
        let st = &self.steps(i)[s];
        let r = st.start as usize..st.end as usize;
        let (sx, sy) = (&xs[r.clone()], &ys[r]);
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
}
