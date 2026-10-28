//! The viewer tables' writer (D23, spec waves §1–§6): one [`Request`] in, the tables changed (or,
//! refused, unchanged) and an [`Applied`] out. The spine's `wave::apply` line.
//!
//! Contents:
//! - `Waves::apply(&mut self, r: &Request) -> Result<Applied, ViewError>`

use core::ops::Range;

use crate::axis::{Axis, UNIT, fit_range, span, window, zoom};
use crate::decimate::extent;
use crate::pane::{MAX_PANES, MAX_TRACES, PaneMode, PaneProps, TRACE_COLORS};
use crate::request::{Applied, Request, ViewError};
use crate::trace::{Evaluated, TraceY, bus_values, evaluate};
use crate::waves::Waves;

impl Waves {
    /// Applies one request. The spine's `wave::apply`: the writer of these tables.
    ///
    /// # Errors
    ///
    /// See [`ViewError`]; a refused request leaves every table unchanged.
    pub fn apply(&mut self, r: &Request) -> Result<Applied, ViewError> {
        let out = match r {
            Request::AddTrace { src, dataset, pane } => self.add_trace(src, *dataset, *pane)?,
            Request::RemoveTrace { trace } => {
                let t = self.trace_ix(*trace)?;
                let p = self.traces.pane[t];
                self.remove_traces(|i, _| i == t);
                self.autoscale_y(p);
                Applied::Done
            }
            Request::MoveTrace { trace, pane } => self.move_trace(*trace, *pane)?,
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
                let a = typed_axis(*lo, *hi, self.x_of(p).log)?;
                self.map_x(p, |_| a);
                Applied::Done
            }
            Request::SetY { pane, lo, hi } => {
                let p = self.pane_ix(*pane)?;
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
                self.pane_ix(*pane)?;
                self.autoscale_y(*pane);
                Applied::Done
            }
            Request::LogX { pane, on } => self.log_x(*pane, *on)?,
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
}

// ---- private ----

/// The axis a typed range names, keeping `log`; refused when not [`Axis::valid`].
fn typed_axis(lo: f64, hi: f64, log: bool) -> Result<Axis, ViewError> {
    let a = Axis { lo, hi, log };
    a.valid().then_some(a).ok_or(ViewError::Range(lo, hi))
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

impl Waves {
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

    /// Drops every trace row for which `gone(row, pane)` holds, keeping order.
    fn remove_traces(&mut self, gone: impl Fn(usize, u8) -> bool) {
        let keep: Vec<bool> = (0..self.trace_count())
            .map(|i| !gone(i, self.traces.pane[i]))
            .collect();
        self.traces.retain(&keep);
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
        if self.traces_in(pane).next().is_none() {
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
        assert!(self.props[k].own_x(), "an unlocked pane has its own x");
        Ok(k as u8)
    }

    /// Moves trace `trace` to `pane`, re-evaluating a bus at the new pane's threshold, and
    /// refits both panes' y (and the new pane's own x when the trace is its first).
    fn move_trace(&mut self, trace: u8, pane: u8) -> Result<Applied, ViewError> {
        let i = self.trace_ix(trace)?;
        let to = self.pane_ix(pane)?;
        if self.has_own_axis(i) && !self.props[to].own_x() {
            return Err(ViewError::OwnAxis);
        }
        let from = self.traces.pane[i];
        let thr = self.props[to].threshold;
        if self.bus_bits(i) > 0 && thr != self.props[usize::from(from)].threshold {
            let d = &self.datasets[usize::from(self.traces.dataset[i])];
            let y = bus_values(d, &self.traces.src[i], thr)?;
            self.traces.eval[i].y = TraceY::Owned(y);
        }
        let was_empty = self.traces_in(pane).next().is_none();
        self.traces.pane[i] = pane;
        if was_empty && self.props[to].own_x() {
            self.autoscale_own_x(to);
        }
        self.autoscale_y(from);
        self.autoscale_y(pane);
        Ok(Applied::Done)
    }

    /// Validates and applies [`Request::SetPane`]: buses of the pane are re-evaluated when the
    /// threshold changes, and the pane is refitted when its mode or shown step changes.
    fn set_pane(&mut self, pane: u8, props: PaneProps) -> Result<Applied, ViewError> {
        let p = self.pane_ix(pane)?;
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
            for i in self.traces_in(pane).filter(|&i| self.bus_bits(i) > 0) {
                let d = &self.datasets[usize::from(self.traces.dataset[i])];
                buses.push((i, bus_values(d, &self.traces.src[i], props.threshold)?));
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
        Ok(Applied::Done)
    }

    /// Switches the x axis `pane` shows to log or linear, refitting it when its range does not
    /// suit the new scale; refused when no data does.
    fn log_x(&mut self, pane: u8, on: bool) -> Result<Applied, ViewError> {
        let p = self.pane_ix(pane)?;
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
            let (a, b) = span(self.xy(i).0, self.x.log);
            lo = lo.min(a);
            hi = hi.max(b);
        }
        if !any && let Some(d) = self.datasets.first() {
            (lo, hi) = span(d.scale(), self.x.log);
        }
        let Some(a) = fit_range(lo, hi, self.x.log, 0.0) else {
            return false;
        };
        self.x = a;
        true
    }

    /// Fits pane `p`'s own x axis to its traces: their x extent, or their value extent in
    /// histogram mode; returns false (and changes nothing) when nothing fits.
    fn autoscale_own_x(&mut self, p: usize) -> bool {
        let log = self.pane_x[p].log;
        let histogram = self.props[p].mode == PaneMode::Histogram;
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in self.traces_in(p as u8) {
            let (x, y) = self.xy(i);
            let col = if histogram { y } else { x };
            for s in &self.steps(i)[self.shown_steps(i)] {
                let (a, b) = span(&col[s.start as usize..s.end as usize], log);
                lo = lo.min(a);
                hi = hi.max(b);
            }
        }
        let Some(a) = fit_range(lo, hi, log, 0.0) else {
            return false;
        };
        self.pane_x[p] = a;
        true
    }

    fn autoscale_every_y(&mut self) {
        for p in 0..self.panes.len() {
            self.autoscale_y(p as u8);
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
        let xa = self.x_of(p);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in self.traces_in(pane) {
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
        let Some(a) = fit_range(lo, hi, log, 0.05) else {
            return false;
        };
        self.panes[p] = a;
        true
    }
}
