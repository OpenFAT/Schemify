//! `raw_plots`: one [`RawPlot`] per analysis block, columns stored column-major.
//!
//! All samples of a block live in one flat `Vec<f64>` per part (real, imaginary): variable `v`
//! occupies `re[v*n .. (v+1)*n]`. A column is therefore a plain contiguous slice, which is what
//! the calculator ([`schemify_expr::Column`]) and the decimator read without copying. Sweep steps
//! are index ranges into the columns.

use core::ops::Range;

use schemify_expr::SignalKind;

use crate::raw::RawError;

/// Variable classification, from the type field of a `Variables:` header line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VarKind {
    /// Transient time axis.
    Time = 0,
    /// AC / noise frequency axis.
    Frequency,
    /// Node voltage (also Xyce's DC `sweep` axis).
    Voltage,
    /// Branch or device current.
    Current,
    /// Anything else (`notype`, user vectors).
    Other,
}

impl VarKind {
    pub(crate) fn from_type_str(s: &str) -> Self {
        let s = s.trim();
        if s.eq_ignore_ascii_case("time") {
            VarKind::Time
        } else if s.eq_ignore_ascii_case("frequency") {
            VarKind::Frequency
        } else if s.eq_ignore_ascii_case("voltage") {
            VarKind::Voltage
        } else if s.eq_ignore_ascii_case("current")
            || s.eq_ignore_ascii_case("device_current")
            || s.eq_ignore_ascii_case("subckt_current")
        {
            VarKind::Current
        } else {
            VarKind::Other
        }
    }

    /// Display unit (`"V"`, `"A"`, `"s"`, `"Hz"`, or `""`).
    pub fn unit(self) -> &'static str {
        match self {
            VarKind::Time => "s",
            VarKind::Frequency => "Hz",
            VarKind::Voltage => "V",
            VarKind::Current => "A",
            VarKind::Other => "",
        }
    }
}

/// One `Variables:` entry. Cold data: read when listing signals and resolving names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    /// Name exactly as the simulator wrote it (`v(out)`, `OUT`, `V1#branch`, `i(@m1[id])`).
    pub name: String,
    /// Classification from the header's type field.
    pub kind: VarKind,
}

/// One analysis block of a `.raw` file (or several blocks merged by [`group_datasets`]).
///
/// Invariants (checked at construction): at least one variable and one point; `re` holds
/// `vars × points` samples, `im` the same when complex and nothing otherwise; `steps` partitions
/// `0..points` into non-empty ranges. Column 0 is the scale (sweep axis).
#[derive(Debug, Clone, PartialEq)]
pub struct RawPlot {
    pub(crate) name: String,
    pub(crate) complex: bool,
    pub(crate) vars: Vec<Variable>,
    pub(crate) n: u32,
    pub(crate) re: Vec<f64>,
    pub(crate) im: Vec<f64>,
    pub(crate) steps: Vec<Range<u32>>,
}

impl RawPlot {
    /// Builds a block from column-major samples and detects its sweep steps (a step ends where
    /// the scale column reverses direction). Used by the parser and by callers that synthesize
    /// data (tests, generated signals).
    ///
    /// # Errors
    ///
    /// [`RawError::Header`] when there are no variables or no points, or when `re`/`im` do not
    /// hold `vars.len()` columns of equal length (`im` empty for real data).
    pub fn from_columns(
        name: &str,
        vars: Vec<Variable>,
        re: Vec<f64>,
        im: Vec<f64>,
    ) -> Result<RawPlot, RawError> {
        let nv = vars.len();
        if nv == 0 || re.is_empty() || !re.len().is_multiple_of(nv) {
            return Err(RawError::Header(format!(
                "{} samples do not form {nv} non-empty columns",
                re.len()
            )));
        }
        let n = re.len() / nv;
        if n > u32::MAX as usize {
            return Err(RawError::Header(format!("{n} points exceed 2^32")));
        }
        let complex = !im.is_empty();
        if complex && im.len() != re.len() {
            return Err(RawError::Header(
                "imaginary part has a different length".into(),
            ));
        }
        let steps = detect_steps(&re[..n]);
        let plot = RawPlot {
            name: name.to_owned(),
            complex,
            vars,
            n: n as u32,
            re,
            im,
            steps,
        };
        plot.check();
        Ok(plot)
    }

    /// Asserts the struct invariants (TigerStyle exit check of every constructor).
    pub(crate) fn check(&self) {
        let n = self.n as usize;
        assert!(n > 0 && !self.vars.is_empty(), "empty plot");
        assert_eq!(self.re.len(), n * self.vars.len(), "column store size");
        assert_eq!(self.im.len(), if self.complex { self.re.len() } else { 0 });
        let mut at = 0;
        for s in &self.steps {
            assert!(
                s.start == at && s.end > s.start,
                "steps do not partition the points"
            );
            at = s.end;
        }
        assert_eq!(at, self.n, "steps do not cover the points");
    }

    /// The `Plotname:` header value (for merged datasets, the first block's).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// True for AC-style data: every column has an imaginary part.
    pub fn is_complex(&self) -> bool {
        self.complex
    }

    /// Variables in column order; entry 0 is the scale.
    pub fn vars(&self) -> &[Variable] {
        &self.vars
    }

    /// Points per column (all steps together).
    pub fn len(&self) -> usize {
        self.n as usize
    }

    /// Always false: a plot has at least one point (kept for clippy's `len` convention).
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Real part of column `v`.
    ///
    /// # Panics
    ///
    /// Panics when `v >= vars().len()`.
    #[inline]
    pub fn col(&self, v: usize) -> &[f64] {
        assert!(v < self.vars.len(), "column {v} out of range");
        let n = self.n as usize;
        &self.re[v * n..(v + 1) * n]
    }

    /// Imaginary part of column `v`; empty for real data.
    ///
    /// # Panics
    ///
    /// Panics when `v >= vars().len()`.
    #[inline]
    pub fn col_im(&self, v: usize) -> &[f64] {
        assert!(v < self.vars.len(), "column {v} out of range");
        if !self.complex {
            return &[];
        }
        let n = self.n as usize;
        &self.im[v * n..(v + 1) * n]
    }

    /// The scale (sweep axis) column, real part. AC frequencies are real in every simulator.
    #[inline]
    pub fn scale(&self) -> &[f64] {
        self.col(0)
    }

    /// Sweep steps: contiguous non-empty ranges partitioning `0..len()`.
    pub fn steps(&self) -> &[Range<u32>] {
        &self.steps
    }

    /// Column of the variable named `name`, compared case-insensitively (simulators disagree on
    /// case: ngspice writes `v(out)`, Xyce `OUT`).
    pub fn find_var(&self, name: &str) -> Option<usize> {
        self.vars
            .iter()
            .position(|v| v.name.eq_ignore_ascii_case(name))
    }

    /// Column a calculator signal names (spec expr §4): `v(net)` is the vector `v(net)`, else
    /// `net` (Xyce, `notype` vectors); `i(dev)` is `i(dev)`, else `dev#branch` (Xyce, old
    /// ngspice), else `dev`. All comparisons ignore case. Never column 0 unless the name says so.
    pub fn find_signal(&self, kind: SignalKind, name: &str) -> Option<usize> {
        let f = match kind {
            SignalKind::V => "v",
            SignalKind::I => "i",
            // A device's operating-point value is no column of a plot (run spec §4).
            SignalKind::Op => return None,
        };
        let mut buf = String::with_capacity(name.len() + 8);
        buf.push_str(f);
        buf.push('(');
        buf.push_str(name);
        buf.push(')');
        if let Some(v) = self.find_var(&buf) {
            return Some(v);
        }
        if kind == SignalKind::I {
            buf.clear();
            buf.push_str(name);
            buf.push_str("#branch");
            if let Some(v) = self.find_var(&buf) {
                return Some(v);
            }
        }
        self.find_var(name)
    }
}

/// Splits `x` into sweep steps: a new step starts where `x` reverses direction (a nested sweep
/// restarting, a `.step` run appended in place). Equal neighbours never split, so a descending
/// sweep stays one step. O(n).
pub(crate) fn detect_steps(x: &[f64]) -> Vec<Range<u32>> {
    assert!(!x.is_empty() && x.len() <= u32::MAX as usize);
    let mut steps = Vec::new();
    let mut start = 0usize;
    let mut dir = 0i8;
    for i in 1..x.len() {
        let d = x[i] - x[i - 1];
        let s = if d > 0.0 {
            1
        } else if d < 0.0 {
            -1
        } else {
            0
        };
        if s == 0 {
            continue;
        }
        if dir == 0 {
            dir = s;
        } else if s != dir {
            steps.push(start as u32..i as u32);
            start = i;
            dir = 0;
        }
    }
    steps.push(start as u32..x.len() as u32);
    debug_assert_eq!(steps.last().map(|s| s.end as usize), Some(x.len()));
    steps
}

/// Merges runs of consecutive blocks that hold the same variables (names ignoring case, kinds,
/// real/complex) into one block whose steps are the blocks' steps: Xyce writes each `.step` as
/// its own block, ngspice `set appendwrite` appends one block per run. Blocks that differ start a
/// new dataset. Cost: one copy of every merged block's samples.
///
/// # Panics
///
/// Panics if a merged dataset would exceed 2^32 points.
pub fn group_datasets(plots: Vec<RawPlot>) -> Vec<RawPlot> {
    let mut out: Vec<RawPlot> = Vec::with_capacity(plots.len());
    let mut run: Vec<RawPlot> = Vec::new();
    for p in plots {
        if let Some(first) = run.first()
            && !same_vars(first, &p)
        {
            out.push(merge(std::mem::take(&mut run)));
        }
        run.push(p);
    }
    if !run.is_empty() {
        out.push(merge(run));
    }
    debug_assert!(out.iter().all(|p| !p.steps.is_empty()));
    out
}

fn same_vars(a: &RawPlot, b: &RawPlot) -> bool {
    a.complex == b.complex
        && a.vars.len() == b.vars.len()
        && a.vars
            .iter()
            .zip(&b.vars)
            .all(|(x, y)| x.kind == y.kind && x.name.eq_ignore_ascii_case(&y.name))
}

fn merge(mut run: Vec<RawPlot>) -> RawPlot {
    assert!(!run.is_empty());
    if run.len() == 1 {
        return run.pop().expect("one block");
    }
    let total: usize = run.iter().map(RawPlot::len).sum();
    assert!(
        total <= u32::MAX as usize,
        "merged dataset exceeds 2^32 points"
    );
    let nv = run[0].vars.len();
    let complex = run[0].complex;
    let mut re = Vec::with_capacity(total * nv);
    let mut im = Vec::with_capacity(if complex { total * nv } else { 0 });
    for v in 0..nv {
        for p in &run {
            re.extend_from_slice(p.col(v));
            im.extend_from_slice(p.col_im(v));
        }
    }
    let mut steps = Vec::new();
    let mut base = 0u32;
    for p in &run {
        steps.extend(p.steps.iter().map(|s| s.start + base..s.end + base));
        base += p.n;
    }
    let first = run.swap_remove(0);
    let plot = RawPlot {
        name: first.name,
        complex,
        vars: first.vars,
        n: total as u32,
        re,
        im,
        steps,
    };
    plot.check();
    plot
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(name: &str, kind: VarKind) -> Variable {
        Variable {
            name: name.into(),
            kind,
        }
    }

    #[test]
    fn steps_split_on_reversal_only() {
        assert_eq!(
            detect_steps(&[0.0, 1.0, 2.0, 0.0, 1.0, 2.0]),
            vec![0..3, 3..6]
        );
        assert_eq!(detect_steps(&[2.0, 1.0, 0.0]), vec![0..3]); // descending sweep
        assert_eq!(detect_steps(&[0.0, 0.0, 1.0, 1.0, 2.0]), vec![0..5]); // repeated time points
        assert_eq!(detect_steps(&[5.0]), vec![0..1]);
        assert_eq!(detect_steps(&[2.0, 0.0, 0.0, 2.0, 0.0]), vec![0..3, 3..5]);
    }

    #[test]
    fn signal_lookup_follows_simulator_naming() {
        let vars = vec![
            var("time", VarKind::Time),
            var("v(out)", VarKind::Voltage),
            var("OUT2", VarKind::Voltage),
            var("V1#branch", VarKind::Current),
            var("i(l1)", VarKind::Current),
            var("pow", VarKind::Other),
        ];
        let p = RawPlot::from_columns("t", vars, vec![0.0; 6], vec![]).unwrap();
        assert_eq!(p.find_signal(SignalKind::V, "OUT"), Some(1));
        assert_eq!(p.find_signal(SignalKind::V, "out2"), Some(2));
        assert_eq!(p.find_signal(SignalKind::I, "v1"), Some(3));
        assert_eq!(p.find_signal(SignalKind::I, "L1"), Some(4));
        assert_eq!(p.find_signal(SignalKind::V, "pow"), Some(5));
        assert_eq!(p.find_signal(SignalKind::V, "l1"), None);
        assert_eq!(p.find_signal(SignalKind::I, "out"), None);
    }

    #[test]
    fn from_columns_rejects_ragged_and_empty() {
        let v = || vec![var("t", VarKind::Time), var("a", VarKind::Voltage)];
        assert!(RawPlot::from_columns("x", v(), vec![0.0; 3], vec![]).is_err());
        assert!(RawPlot::from_columns("x", v(), vec![], vec![]).is_err());
        assert!(RawPlot::from_columns("x", vec![], vec![1.0], vec![]).is_err());
        assert!(RawPlot::from_columns("x", v(), vec![0.0; 4], vec![0.0; 2]).is_err());
        let ok = RawPlot::from_columns("x", v(), vec![0.0, 1.0, 5.0, 6.0], vec![]).unwrap();
        assert_eq!((ok.len(), ok.col(1)), (2, &[5.0, 6.0][..]));
        assert!(ok.col_im(1).is_empty());
    }

    #[test]
    fn grouping_merges_only_compatible_neighbours() {
        let v = |n: &str| vec![var("time", VarKind::Time), var(n, VarKind::Voltage)];
        let a = RawPlot::from_columns("a", v("x"), vec![0.0, 1.0, 10.0, 11.0], vec![]).unwrap();
        let b = RawPlot::from_columns("b", v("X"), vec![0.0, 1.0, 2.0, 20.0, 21.0, 22.0], vec![])
            .unwrap();
        let c = RawPlot::from_columns("c", v("y"), vec![0.0, 1.0, 30.0, 31.0], vec![]).unwrap();
        let g = group_datasets(vec![a, b, c]);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].steps(), &[0..2, 2..5]);
        assert_eq!(g[0].col(1), &[10.0, 11.0, 20.0, 21.0, 22.0]);
        assert_eq!(g[0].scale(), &[0.0, 1.0, 0.0, 1.0, 2.0]);
        assert_eq!(g[1].name(), "c");
    }
}
