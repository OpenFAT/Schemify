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
//!   `closest_step`, `histogram`, `reload`; `pub(crate)` `traces_in`, `check`

use core::ops::Range;

use crate::axis::{self, Axis, UNIT};
use crate::digital;
use crate::pane::{LineStyle, MAX_BINS, MAX_DATASETS, MAX_PANES, MAX_TRACES, PaneMode, PaneProps};
use crate::plot::RawPlot;
use crate::request::ViewError;
use crate::trace::{self, Evaluated, TraceY, Traces};

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

/// How [`Waves`] reads a trace between two samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sampling {
    /// Linear interpolation (an analog readout).
    Interpolate,
    /// The last sample at or before `x` (a bus, a logic level).
    Hold,
}

impl Waves {
    /// A viewer over `datasets` with one empty pane; x spans the first dataset's scale.
    ///
    /// # Panics
    ///
    /// Panics on more than [`MAX_DATASETS`] datasets.
    #[must_use]
    pub fn new(datasets: Vec<RawPlot>) -> Waves {
        assert!(
            datasets.len() <= MAX_DATASETS,
            "{} datasets, at most {MAX_DATASETS} (indices are u8)",
            datasets.len()
        );
        let x = datasets
            .first()
            .and_then(|d| {
                let s = axis::span(d.scale(), /* log */ false);
                axis::fit_range(s.min, s.max, /* log */ false, 0.0)
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
        assert_eq!(w.trace_count(), 0, "a new viewer has no trace");
        w
    }

    /// The datasets traces are evaluated over.
    #[must_use]
    pub fn datasets(&self) -> &[RawPlot] {
        debug_assert!(
            self.datasets.len() <= MAX_DATASETS,
            "dataset indices are u8"
        );
        &self.datasets
    }

    /// Number of trace rows.
    #[must_use]
    pub fn trace_count(&self) -> usize {
        let n = self.traces.len();
        debug_assert!(n <= MAX_TRACES, "{n} traces, at most {MAX_TRACES}");
        n
    }

    /// Expression text of trace `i`.
    #[must_use]
    pub fn src(&self, i: usize) -> &str {
        self.assert_row(i);
        &self.traces.src[i]
    }

    /// Pane of trace `i`.
    #[must_use]
    pub fn pane_of(&self, i: usize) -> u8 {
        self.assert_row(i);
        self.traces.pane[i]
    }

    /// RGBA8 color of trace `i`.
    #[must_use]
    pub fn color(&self, i: usize) -> u32 {
        self.assert_row(i);
        self.traces.color[i]
    }

    /// Stroke of trace `i`.
    #[must_use]
    pub fn style(&self, i: usize) -> LineStyle {
        self.assert_row(i);
        self.traces.style[i]
    }

    /// True when trace `i` shows the magnitude of a complex result.
    #[must_use]
    pub fn is_magnitude(&self, i: usize) -> bool {
        self.assert_row(i);
        self.traces.eval[i].complex
    }

    /// Width of bus trace `i` in bits; 0 when it is a single signal (spec waves §4).
    #[must_use]
    pub fn bus_bits(&self, i: usize) -> u8 {
        self.assert_row(i);
        self.traces.eval[i].bits
    }

    /// True when trace `i` is on its own axis (`fft` bins, per-step results) rather than its
    /// dataset's scale.
    #[must_use]
    pub fn has_own_axis(&self, i: usize) -> bool {
        self.assert_row(i);
        self.traces.eval[i].axis.is_some()
    }

    /// The x and y columns trace `i` draws (equal length). For a plain signal both borrow the
    /// dataset; valid until the next `Waves::apply` or [`Waves::reload`].
    ///
    /// # Panics
    ///
    /// Panics when `i >= trace_count()`.
    #[must_use]
    pub fn xy(&self, i: usize) -> (&[f64], &[f64]) {
        self.assert_row(i);
        let d = &self.datasets[usize::from(self.traces.dataset[i])];
        let e = &self.traces.eval[i];
        let x = e.axis.as_ref().map_or(d.scale(), |a| &a.x[..]);
        let y = match &e.y {
            TraceY::Raw(v) => d.col(*v as usize),
            TraceY::Owned(y) => &y[..],
        };
        assert_eq!(x.len(), y.len(), "trace {i}: one x per y value");
        (x, y)
    }

    /// Sweep steps of trace `i`, partitioning its columns.
    #[must_use]
    pub fn steps(&self, i: usize) -> &[Range<u32>] {
        self.assert_row(i);
        let steps = match &self.traces.eval[i].axis {
            Some(a) => &a.steps[..],
            None => self.datasets[usize::from(self.traces.dataset[i])].steps(),
        };
        debug_assert!(!steps.is_empty(), "trace {i}: a column has a step");
        steps
    }

    /// Indices into [`Waves::steps`] that trace `i`'s pane shows: all of them, the tracked one,
    /// or none when the trace has no such step.
    #[must_use]
    pub fn shown_steps(&self, i: usize) -> Range<usize> {
        let n = self.steps(i).len();
        let shown = match self.props[usize::from(self.traces.pane[i])].step {
            None => 0..n,
            Some(s) if (s as usize) < n => s as usize..s as usize + 1,
            Some(_) => n..n,
        };
        debug_assert!(shown.start <= shown.end, "a forward range");
        debug_assert!(shown.end <= n, "only steps the trace has");
        shown
    }

    /// Y axis of every pane, top to bottom.
    #[must_use]
    pub fn panes(&self) -> &[Axis] {
        &self.panes
    }

    /// Properties of pane `p`.
    ///
    /// # Panics
    ///
    /// Panics when `p` is not a pane.
    #[must_use]
    pub fn props(&self, p: usize) -> PaneProps {
        self.assert_pane(p);
        self.props[p]
    }

    /// The x axis pane `p` shows: its own when [`PaneProps::own_x`], else the shared one.
    ///
    /// # Panics
    ///
    /// Panics when `p` is not a pane.
    #[must_use]
    pub fn x_of(&self, p: usize) -> Axis {
        self.assert_pane(p);
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
    #[must_use]
    pub fn hcursors(&self, p: usize) -> [Option<f64>; 2] {
        self.assert_pane(p);
        debug_assert!(
            self.hcursors[p].iter().flatten().all(|y| y.is_finite()),
            "a cursor is finite or absent"
        );
        self.hcursors[p]
    }

    /// The shared x axis (shown by every locked pane).
    #[must_use]
    pub fn x(&self) -> Axis {
        debug_assert!(self.x.valid(), "the viewer holds only valid axes");
        self.x
    }

    /// Vertical cursor positions (shared-x data units), indexed by [`Cursor`](crate::Cursor).
    #[must_use]
    pub fn cursors(&self) -> [Option<f64>; 2] {
        debug_assert!(
            self.cursors.iter().flatten().all(|x| x.is_finite()),
            "a cursor is finite or absent"
        );
        self.cursors
    }

    /// Trace `i`'s value at `x` in the first shown step whose (ascending) x range contains it;
    /// `None` when no shown step does. Interpolated linearly between samples, held from the
    /// last sample for a bus.
    ///
    /// # Panics
    ///
    /// Panics when `i >= trace_count()`.
    ///
    /// # Time complexity
    ///
    /// O(steps + log points).
    #[must_use]
    pub fn value_at(&self, i: usize, x: f64) -> Option<f64> {
        let how = self.sampling(i);
        self.shown_steps(i)
            .find_map(|s| self.sample_at(i, s, x, how))
    }

    /// Trace `i`'s value at `x` in sweep step `s` (an index into [`Waves::steps`]), as
    /// [`Waves::value_at`] reads one step; `None` when `x` is outside the step or the step's x
    /// is not ascending.
    ///
    /// # Panics
    ///
    /// Panics when `i >= trace_count()` or `s` is not a step of trace `i`.
    #[must_use]
    pub fn value_at_step(&self, i: usize, s: usize, x: f64) -> Option<f64> {
        let steps = self.steps(i).len();
        assert!(s < steps, "trace {i} has {steps} steps, not step {s}");
        self.sample_at(i, s, x, self.sampling(i))
    }

    /// Trace `i`'s logic value at `x` (spec waves §4): a bus's value, or 1 when the held sample
    /// is at or above its pane's threshold; `None` outside the shown steps.
    ///
    /// # Panics
    ///
    /// Panics when `i >= trace_count()`.
    #[must_use]
    pub fn logic_at(&self, i: usize, x: f64) -> Option<u32> {
        let threshold = self.props[usize::from(self.pane_of(i))].threshold;
        let y = self
            .shown_steps(i)
            .find_map(|s| self.sample_at(i, s, x, Sampling::Hold))?;
        let bus = self.bus_bits(i) > 0;
        let level = if bus {
            y as u32
        } else {
            u32::from(y >= threshold)
        };
        debug_assert!(bus || level <= 1, "a single signal reads 0 or 1");
        Some(level)
    }

    /// The step of pane `pane`'s traces whose value at `x` is closest to `y` (in transformed
    /// units on a log y axis): dataset tracking, xschem `t`. `None` for a non-analog pane, a
    /// pane that does not exist, or when no step covers `x`.
    ///
    /// # Time complexity
    ///
    /// O(traces in the pane · steps · log points).
    #[must_use]
    pub fn closest_step(&self, pane: u8, x: f64, y: f64) -> Option<u32> {
        let p = usize::from(pane);
        if p >= self.panes.len() || self.props[p].mode != PaneMode::Analog {
            return None;
        }
        let y_axis = self.panes[p];
        let mut best: Option<(f64, u32)> = None;
        for i in self.traces_in(pane) {
            for s in 0..self.steps(i).len() {
                let Some(v) = self.value_at_step(i, s, x) else {
                    continue;
                };
                let d = (y_axis.t(v) - y_axis.t(y)).abs();
                if d.is_finite() && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, s as u32));
                }
            }
        }
        if let Some((d, _)) = best {
            debug_assert!(d.is_finite(), "only finite distances are kept");
            debug_assert!(d >= 0.0, "a distance is an absolute value");
        }
        best.map(|(_, s)| s)
    }

    /// Fills `out` with trace `i`'s histogram (spec waves §5): its pane's `bins` counts of the
    /// shown steps' samples over the pane's x axis.
    ///
    /// # Panics
    ///
    /// Panics when `i >= trace_count()`.
    ///
    /// # Time complexity
    ///
    /// O(points of the shown steps).
    pub fn histogram(&self, i: usize, out: &mut Vec<u32>) {
        let p = usize::from(self.pane_of(i));
        let x_axis = self.x_of(p);
        let bins = usize::from(self.props[p].bins);
        assert!(
            bins >= 2,
            "pane {p} properties are valid: at least two bins"
        );
        assert!(
            bins <= usize::from(MAX_BINS),
            "pane {p} properties are valid: {bins} bins"
        );
        out.clear();
        out.resize(bins, 0);
        let (_, y) = self.xy(i);
        for s in &self.steps(i)[self.shown_steps(i)] {
            let y = &y[s.start as usize..s.end as usize];
            let (t0, t1) = (x_axis.t(x_axis.lo), x_axis.t(x_axis.hi));
            digital::bin_counts(y, t0, t1, x_axis.log, out);
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
        assert!(
            datasets.len() <= MAX_DATASETS,
            "{} datasets, at most {MAX_DATASETS} (indices are u8)",
            datasets.len()
        );
        self.datasets = datasets;
        let n = self.trace_count();
        let mut keep = vec![true; n];
        let mut dropped = Vec::new();
        for (i, kept) in keep.iter_mut().enumerate() {
            let p = usize::from(self.traces.pane[i]);
            let dataset = self.traces.dataset[i];
            let result = match self.datasets.get(usize::from(dataset)) {
                None => Err(ViewError::NoDataset(dataset)),
                Some(d) => trace::evaluate(d, &self.traces.src[i], self.props[p].threshold),
            };
            let why = match result {
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
        self.assert_pane(usize::from(pane));
        (0..self.trace_count()).filter(move |&i| self.traces.pane[i] == pane)
    }

    /// Asserts the cross-table invariants listed on [`Waves`].
    pub(crate) fn check(&self) {
        let t = &self.traces;
        let n = t.len();
        let np = self.panes.len();
        assert!(n <= MAX_TRACES, "{n} traces, at most {MAX_TRACES}");
        assert!(np > 0, "the last pane stays");
        assert!(np <= MAX_PANES, "{np} panes, at most {MAX_PANES}");
        assert_eq!(t.dataset.len(), n, "trace columns in step: dataset");
        assert_eq!(t.pane.len(), n, "trace columns in step: pane");
        assert_eq!(t.color.len(), n, "trace columns in step: color");
        assert_eq!(t.style.len(), n, "trace columns in step: style");
        assert_eq!(t.eval.len(), n, "trace columns in step: eval");
        assert_eq!(self.pane_x.len(), np, "pane columns in step: own x");
        assert_eq!(self.props.len(), np, "pane columns in step: properties");
        assert_eq!(
            self.hcursors.len(),
            np,
            "pane columns in step: horizontal cursors"
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
        assert!(self.x.valid(), "shared x axis {:?}", self.x);
        assert!(self.panes.iter().all(Axis::valid), "y axis");
        assert!(self.pane_x.iter().all(Axis::valid), "own x axis");
        assert!(self.props.iter().all(PaneProps::valid), "pane properties");
        assert!(
            self.cursors.iter().flatten().all(|x| x.is_finite()),
            "a vertical cursor is finite or absent"
        );
        assert!(
            self.hcursors
                .iter()
                .flatten()
                .flatten()
                .all(|y| y.is_finite()),
            "a horizontal cursor is finite or absent"
        );
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
    /// Asserts that `i` is a trace row: the precondition of every per-trace accessor.
    fn assert_row(&self, i: usize) {
        let n = self.trace_count();
        assert!(i < n, "no trace row {i} ({n} traces)");
    }

    /// Asserts that `p` is a pane: the precondition of every per-pane accessor.
    fn assert_pane(&self, p: usize) {
        let n = self.panes.len();
        assert!(p < n, "no pane {p} ({n} panes)");
    }

    /// How trace `i` reads between samples: held for a bus, else interpolated.
    fn sampling(&self, i: usize) -> Sampling {
        // A bus is never on its own axis: its bits are columns on the dataset's scale.
        debug_assert!(
            self.bus_bits(i) == 0 || !self.has_own_axis(i),
            "trace {i}: a bus on its own axis"
        );
        if self.bus_bits(i) > 0 {
            Sampling::Hold
        } else {
            Sampling::Interpolate
        }
    }

    /// Trace `i`'s value at `x` in step `s`: held from the last sample at or before `x`, or
    /// interpolated, as `how` says; `None` outside the step's x range.
    fn sample_at(&self, i: usize, s: usize, x: f64, how: Sampling) -> Option<f64> {
        let (xs, ys) = self.xy(i);
        let step = &self.steps(i)[s];
        assert!(
            step.end as usize <= xs.len(),
            "trace {i} step {s}: steps partition the columns"
        );
        let r = step.start as usize..step.end as usize;
        let (sx, sy) = (&xs[r.clone()], &ys[r]);
        assert!(!sx.is_empty(), "trace {i} step {s}: steps are non-empty");
        let (first, last) = (sx[0], sx[sx.len() - 1]);
        if !(first <= x && x <= last) {
            return None;
        }
        if how == Sampling::Hold {
            // Last sample at or before x: k >= 1 because first <= x.
            let k = sx.partition_point(|&v| v <= x);
            debug_assert!(k >= 1, "first <= x leaves a sample at or before x");
            return Some(sy[k - 1]);
        }
        let k = sx.partition_point(|&v| v < x);
        debug_assert!(k < sx.len(), "x <= last leaves a sample at or after x");
        if k == 0 || sx[k] == x {
            return Some(sy[k]);
        }
        let (x0, x1, y0, y1) = (sx[k - 1], sx[k], sy[k - 1], sy[k]);
        Some(y0 + (y1 - y0) * (x - x0) / (x1 - x0))
    }
}
