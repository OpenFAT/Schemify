//! The viewer tables' writer (D23, spec waves §1–§6): one [`Request`] in, the tables changed (or,
//! refused, unchanged) and an [`Applied`] out. The spine's `wave::apply` line.
//!
//! Contents:
//! - `Waves::apply(&mut self, r: &Request) -> Result<Applied, ViewError>`

use core::ops::Range;

use crate::axis::{self, Axis, UNIT};
use crate::decimate::{self, MinMax};
use crate::pane::{MAX_PANES, MAX_TRACES, PaneMode, PaneProps, TRACE_COLORS};
use crate::request::{Applied, Request, ViewError};
use crate::trace::{self, Evaluated, TraceY};
use crate::waves::Waves;

impl Waves {
    /// Applies one request. The spine's `wave::apply`: the writer of these tables.
    ///
    /// # Errors
    ///
    /// See [`ViewError`]; a refused request leaves every table unchanged.
    ///
    /// # Panics
    ///
    /// Never on a request: every field is checked and refused with a [`ViewError`]. The asserts
    /// after the write (the invariants listed on [`Waves`]) fire only on a bug in this crate.
    pub fn apply(&mut self, r: &Request) -> Result<Applied, ViewError> {
        let out = match r {
            Request::AddTrace { src, dataset, pane } => self.add_trace(src, *dataset, *pane)?,
            Request::RemoveTrace { trace } => {
                let t = self.trace_index(*trace)?;
                let p = self.traces.pane[t];
                self.remove_traces(|i, _| i == t);
                self.autoscale_y(p);
                Applied::Done
            }
            Request::MoveTrace { trace, pane } => self.move_trace(*trace, *pane)?,
            Request::SetStyle { trace, style } => {
                let i = self.trace_index(*trace)?;
                self.traces.style[i] = *style;
                Applied::Done
            }
            Request::SetColor { trace, color } => {
                let i = self.trace_index(*trace)?;
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
                let p = self.pane_index(*pane)?;
                if self.panes.len() > 1 {
                    self.remove_traces(|_, tp| tp == *pane);
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
                let p = self.pane_index(*pane)?;
                self.map_x(p, |a| axis::zoom(a, *at, *factor));
                Applied::Done
            }
            Request::ZoomY { pane, at, factor } => {
                let p = self.pane_index(*pane)?;
                self.panes[p] = axis::zoom(self.panes[p], *at, *factor);
                Applied::Done
            }
            Request::PanX { pane, by } => {
                let p = self.pane_index(*pane)?;
                self.map_x(p, |a| axis::window(a, *by, 1.0 + *by));
                Applied::Done
            }
            Request::PanY { pane, by } => {
                let p = self.pane_index(*pane)?;
                self.panes[p] = axis::window(self.panes[p], *by, 1.0 + *by);
                Applied::Done
            }
            Request::BoxX { pane, from, to } => {
                let p = self.pane_index(*pane)?;
                self.map_x(p, |a| axis::window(a, from.min(*to), from.max(*to)));
                Applied::Done
            }
            Request::BoxY { pane, from, to } => {
                let p = self.pane_index(*pane)?;
                self.panes[p] = axis::window(self.panes[p], from.min(*to), from.max(*to));
                Applied::Done
            }
            Request::SetX { pane, lo, hi } => {
                let p = self.pane_index(*pane)?;
                let a = typed_axis(*lo, *hi, self.x_of(p).log)?;
                self.map_x(p, |_| a);
                Applied::Done
            }
            Request::SetY { pane, lo, hi } => {
                let p = self.pane_index(*pane)?;
                self.panes[p] = typed_axis(*lo, *hi, self.panes[p].log)?;
                Applied::Done
            }
            Request::Autoscale => {
                self.autoscale_x();
                for p in 0..self.panes.len() {
                    if self.props[p].own_x() {
                        self.autoscale_own_x(p);
                    }
                }
                self.autoscale_every_y();
                Applied::Done
            }
            Request::AutoscaleY { pane } => {
                self.pane_index(*pane)?;
                self.autoscale_y(*pane);
                Applied::Done
            }
            Request::LogX { pane, on } => self.log_x(*pane, *on)?,
            Request::LogY { pane, on } => {
                let p = self.pane_index(*pane)?;
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
                let p = self.pane_index(*pane)?;
                self.hcursors[p][*which as usize] = at.filter(|v| v.is_finite());
                Applied::Done
            }
            Request::SwapCursors => {
                self.cursors.swap(0, 1);
                Applied::Done
            }
        };
        self.check();
        if let Applied::Trace(t) = out {
            assert_eq!(
                usize::from(t) + 1,
                self.trace_count(),
                "a new trace is the last row"
            );
        }
        Ok(out)
    }
}

// ---- private ----

/// The axis a typed range names, keeping `log`; refused when not [`Axis::valid`].
fn typed_axis(lo: f64, hi: f64, log: bool) -> Result<Axis, ViewError> {
    let a = Axis { lo, hi, log };
    if !a.valid() {
        return Err(ViewError::Range(lo, hi));
    }
    assert!(a.lo < a.hi, "a valid axis is ordered: {a:?}");
    Ok(a)
}

/// Smallest positive y over the samples with x in `[x_lo, x_hi]` (log-y autoscale).
// ponytail: scalar scan, only on log-y panes whose data crosses zero.
fn min_positive(x: &[f64], y: &[f64], steps: &[Range<u32>], x_lo: f64, x_hi: f64) -> f64 {
    assert_eq!(x.len(), y.len(), "x and y columns differ in length");
    let mut m = f64::INFINITY;
    for s in steps {
        assert!(s.start <= s.end, "step {s:?} is reversed");
        assert!(
            s.end as usize <= x.len(),
            "step {s:?} ends past the columns"
        );
        for k in s.start as usize..s.end as usize {
            if x[k] >= x_lo && x[k] <= x_hi && y[k] > 0.0 && y[k] < m {
                m = y[k];
            }
        }
    }
    assert!(m > 0.0, "positive, or +inf when no sample is");
    m
}

impl Waves {
    /// Row `t` when it is a trace, refused otherwise.
    fn trace_index(&self, t: u8) -> Result<usize, ViewError> {
        let i = usize::from(t);
        if i >= self.trace_count() {
            return Err(ViewError::NoTrace(t));
        }
        assert!(i < MAX_TRACES, "trace rows stay under the capacity");
        Ok(i)
    }

    /// Row `p` when it is a pane, refused otherwise.
    fn pane_index(&self, p: u8) -> Result<usize, ViewError> {
        let i = usize::from(p);
        if i >= self.panes.len() {
            return Err(ViewError::NoPane(p));
        }
        assert!(i < MAX_PANES, "pane rows stay under the capacity");
        Ok(i)
    }

    /// Drops every trace row for which `gone(row, pane)` holds, keeping order.
    fn remove_traces(&mut self, gone: impl Fn(usize, u8) -> bool) {
        let before = self.trace_count();
        let keep: Vec<bool> = (0..before).map(|i| !gone(i, self.traces.pane[i])).collect();
        self.traces.retain(&keep);
        assert!(self.trace_count() <= before, "removing never adds a row");
    }

    /// Appends a pane row with default properties, its own x starting at the shared view.
    fn push_pane(&mut self) {
        assert!(self.panes.len() < MAX_PANES, "caller checks the capacity");
        self.panes.push(UNIT);
        self.pane_x.push(self.x);
        self.props.push(PaneProps::default());
        self.hcursors.push([None; 2]);
        assert_eq!(self.pane_x.len(), self.panes.len(), "pane columns in step");
        assert_eq!(
            self.hcursors.len(),
            self.panes.len(),
            "pane columns in step"
        );
    }

    /// Applies `f` to the x axis pane `p` shows.
    fn map_x(&mut self, p: usize, f: impl FnOnce(Axis) -> Axis) {
        assert!(p < self.panes.len(), "no pane {p}");
        let own = self.props[p].own_x();
        let shown = if own {
            &mut self.pane_x[p]
        } else {
            &mut self.x
        };
        assert!(shown.valid(), "pane {p}: the viewer holds only valid axes");
        *shown = f(*shown);
        assert!(
            shown.valid(),
            "pane {p}: `f` maps a valid axis to a valid one, got {shown:?}"
        );
    }

    fn add_trace(&mut self, src: &str, dataset: u8, pane: u8) -> Result<Applied, ViewError> {
        if self.trace_count() >= MAX_TRACES {
            return Err(ViewError::Full("traces"));
        }
        let p = self.pane_index(pane)?;
        let d = self
            .datasets
            .get(usize::from(dataset))
            .ok_or(ViewError::NoDataset(dataset))?;
        let e = match trace::evaluate(d, src, self.props[p].threshold)? {
            Evaluated::Value(v) => return Ok(Applied::Value(v)),
            Evaluated::Trace(e) => e,
        };
        let pane = self.admit(pane, e.axis.is_some())?;
        let p = usize::from(pane);
        assert!(
            e.axis.is_none() || self.props[p].own_x(),
            "admit picks a pane that can hold it"
        );
        let own = self.props[p].own_x();
        // The first trace on the axis it lands on fits that axis.
        let first = if own {
            self.traces_in(pane).next().is_none()
        } else {
            (0..self.trace_count()).all(|i| self.props[usize::from(self.traces.pane[i])].own_x())
        };
        let used = |c: &u32| self.traces.color.contains(c);
        let color = TRACE_COLORS
            .iter()
            .copied()
            .find(|c| !used(c))
            .unwrap_or(TRACE_COLORS[self.trace_count() % TRACE_COLORS.len()]);
        self.traces.push(src, dataset, pane, color, e);
        let row = self.trace_count() - 1;
        assert_eq!(
            self.traces.pane[row], pane,
            "the new row is in the pane it was admitted to"
        );
        if first && own {
            self.autoscale_own_x(p);
        } else if first {
            self.autoscale_x();
        }
        self.autoscale_y(pane);
        Ok(Applied::Trace(row as u8))
    }

    /// The pane a new trace lands in (spec waves §2): `pane` itself, unless the trace is on its
    /// own axis and `pane` shows the shared one; then `pane` is unlocked when it is empty, else
    /// a new unlocked pane is appended.
    fn admit(&mut self, pane: u8, own_axis: bool) -> Result<u8, ViewError> {
        let p = usize::from(pane);
        if !own_axis || self.props[p].own_x() {
            return Ok(pane);
        }
        if self.traces_in(pane).next().is_none() {
            self.props[p].locked = false;
            self.pane_x[p] = self.x;
            assert!(self.props[p].own_x(), "an unlocked pane has its own x");
            return Ok(pane);
        }
        if self.panes.len() >= MAX_PANES {
            return Err(ViewError::Full("panes"));
        }
        self.push_pane();
        let k = self.panes.len() - 1;
        assert!(k < MAX_PANES, "the new pane fits a u8 index");
        self.props[k].locked = false;
        assert!(self.props[k].own_x(), "an unlocked pane has its own x");
        Ok(k as u8)
    }

    /// Moves trace `trace` to `pane`, re-evaluating a bus at the new pane's threshold, and
    /// refits both panes' y (and the new pane's own x when the trace is its first).
    fn move_trace(&mut self, trace: u8, pane: u8) -> Result<Applied, ViewError> {
        let i = self.trace_index(trace)?;
        let to = self.pane_index(pane)?;
        if self.has_own_axis(i) && !self.props[to].own_x() {
            return Err(ViewError::OwnAxis);
        }
        let from = self.traces.pane[i];
        let threshold = self.props[to].threshold;
        if self.bus_bits(i) > 0 && threshold != self.props[usize::from(from)].threshold {
            let d = &self.datasets[usize::from(self.traces.dataset[i])];
            let y = trace::bus_values(d, &self.traces.src[i], threshold)?;
            self.traces.eval[i].y = TraceY::Owned(y);
        }
        let was_empty = self.traces_in(pane).next().is_none();
        self.traces.pane[i] = pane;
        if was_empty && self.props[to].own_x() {
            self.autoscale_own_x(to);
        }
        self.autoscale_y(from);
        self.autoscale_y(pane);
        assert_eq!(self.traces.pane[i], pane, "the row moved, not another");
        if self.has_own_axis(i) {
            assert!(
                self.props[to].own_x(),
                "a trace on its own axis in a locked pane"
            );
        }
        Ok(Applied::Done)
    }

    /// Validates and applies [`Request::SetPane`]: buses of the pane are re-evaluated when the
    /// threshold changes, and the pane is refitted when its mode or shown step changes.
    fn set_pane(&mut self, pane: u8, props: PaneProps) -> Result<Applied, ViewError> {
        let p = self.pane_index(pane)?;
        if !props.valid() {
            return Err(ViewError::Props);
        }
        if !props.own_x() && self.traces_in(pane).any(|i| self.has_own_axis(i)) {
            return Err(ViewError::OwnAxis);
        }
        let old = self.props[p];
        // Evaluate first, commit after: a refused request changes nothing.
        let mut buses = Vec::new();
        if props.threshold != old.threshold {
            let bus_rows = self.traces_in(pane).filter(|&i| self.bus_bits(i) > 0);
            for i in bus_rows {
                let d = &self.datasets[usize::from(self.traces.dataset[i])];
                buses.push((
                    i,
                    trace::bus_values(d, &self.traces.src[i], props.threshold)?,
                ));
            }
        }
        for (i, y) in buses {
            self.traces.eval[i].y = TraceY::Owned(y);
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
        assert_eq!(
            self.props[p], props,
            "the pane holds the requested properties"
        );
        assert!(self.x_of(p).valid(), "pane {p} shows a valid x axis");
        Ok(Applied::Done)
    }

    /// Switches the x axis `pane` shows to log or linear, refitting it when its range does not
    /// suit the new scale; refused when no data does.
    fn log_x(&mut self, pane: u8, on: bool) -> Result<Applied, ViewError> {
        let p = self.pane_index(pane)?;
        if self.props[p].own_x() {
            if on != self.pane_x[p].log {
                let keep = self.pane_x[p];
                self.pane_x[p].log = on;
                if !self.pane_x[p].valid() && !self.autoscale_own_x(p) {
                    self.pane_x[p] = keep;
                    return Err(ViewError::NotPositive);
                }
                self.autoscale_y(pane);
            }
        } else if on != self.x.log {
            let keep = self.x;
            self.x.log = on;
            if !self.x.valid() && !self.autoscale_x() {
                self.x = keep;
                return Err(ViewError::NotPositive);
            }
            self.autoscale_every_y();
        }
        assert_eq!(self.x_of(p).log, on, "pane {p} shows the requested scale");
        Ok(Applied::Done)
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
            let s = axis::span(self.xy(i).0, self.x.log);
            lo = lo.min(s.min);
            hi = hi.max(s.max);
        }
        if !any && let Some(d) = self.datasets.first() {
            let s = axis::span(d.scale(), self.x.log);
            (lo, hi) = (s.min, s.max);
        }
        let Some(a) = axis::fit_range(lo, hi, self.x.log, 0.0) else {
            return false;
        };
        assert_eq!(a.log, self.x.log, "fitting keeps the scale");
        self.x = a;
        true
    }

    /// Fits pane `p`'s own x axis to its traces: their x extent, or their value extent in
    /// histogram mode; returns false (and changes nothing) when nothing fits.
    fn autoscale_own_x(&mut self, p: usize) -> bool {
        assert!(
            self.props[p].own_x(),
            "pane {p} shows the shared x axis, not its own"
        );
        let log = self.pane_x[p].log;
        let histogram = self.props[p].mode == PaneMode::Histogram;
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in self.traces_in(p as u8) {
            let (x, y) = self.xy(i);
            let col = if histogram { y } else { x };
            for s in &self.steps(i)[self.shown_steps(i)] {
                let step = axis::span(&col[s.start as usize..s.end as usize], log);
                lo = lo.min(step.min);
                hi = hi.max(step.max);
            }
        }
        let Some(a) = axis::fit_range(lo, hi, log, 0.0) else {
            return false;
        };
        assert_eq!(a.log, log, "fitting keeps the scale");
        self.pane_x[p] = a;
        true
    }

    /// Refits the y axis of every pane.
    fn autoscale_every_y(&mut self) {
        // The `as u8` below is lossless only under the capacity.
        assert!(
            self.panes.len() <= MAX_PANES,
            "pane rows stay under the capacity"
        );
        for p in 0..self.panes.len() {
            self.autoscale_y(p as u8);
        }
    }

    /// Y range of `pane`'s traces over its visible x, padded 5% (the counts in histogram mode);
    /// returns false (and changes nothing) when the pane shows no fitting sample.
    fn autoscale_y(&mut self, pane: u8) -> bool {
        let p = usize::from(pane);
        assert!(p < self.panes.len(), "no pane {pane}");
        let log = self.panes[p].log;
        if self.props[p].mode == PaneMode::Histogram {
            let mut counts = Vec::new();
            let mut most = 1u32;
            for i in self.traces_in(pane) {
                self.histogram(i, &mut counts);
                most = most.max(counts.iter().copied().max().unwrap_or(0));
            }
            self.panes[p] = Axis {
                lo: if log { 0.5 } else { 0.0 },
                hi: f64::from(most) * 1.05,
                log,
            };
            assert!(self.panes[p].valid(), "a count axis is valid");
            return true;
        }
        let x_axis = self.x_of(p);
        let mut all = MinMax::EMPTY;
        for i in self.traces_in(pane) {
            let (x, y) = self.xy(i);
            let steps = &self.steps(i)[self.shown_steps(i)];
            let seen = decimate::extent(x, y, steps, x_axis.lo, x_axis.hi);
            let min = if log && seen.min <= 0.0 {
                min_positive(x, y, steps, x_axis.lo, x_axis.hi)
            } else {
                seen.min
            };
            all.min = all.min.min(min);
            all.max = all.max.max(seen.max);
        }
        let Some(a) = axis::fit_range(all.min, all.max, log, 0.05) else {
            return false;
        };
        assert_eq!(a.log, log, "fitting keeps the scale");
        self.panes[p] = a;
        true
    }
}
