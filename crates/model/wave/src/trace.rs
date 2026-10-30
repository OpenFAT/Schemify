//! Trace rows and their evaluation (spec waves §1, §2, §4): source text in, the column a trace
//! draws out (a borrowed raw column, evaluated values, a bus's integer values, or values on the
//! trace's own axis), or a measurement's text.
//!
//! Contents:
//! - `pub(crate) struct Traces`: `len`, `push`, `retain`
//! - `pub(crate) struct Eval`, `pub(crate) enum TraceY`, `pub(crate) struct OwnAxis`
//! - `pub(crate) enum Evaluated`
//! - `pub(crate) fn evaluate(d: &RawPlot, src: &str, threshold: f64) -> Result<Evaluated, ViewError>`
//! - `pub(crate) fn bus_values(d: &RawPlot, src: &str, threshold: f64) -> Result<Vec<f64>, ViewError>`

use core::ops::Range;

use schemify_expr::{CalcResult, ColumnKind, Mode, Program, Scratch, Value};

use crate::calc;
use crate::pane::{LineStyle, MAX_BUS_BITS, MAX_TRACES};
use crate::plot::RawPlot;
use crate::request::ViewError;
use crate::si;

/// Trace rows (SoA), in legend order; at most [`MAX_TRACES`]. Every column has one entry per
/// row; `retain` and `push` are the only resizes, so they stay equal.
#[derive(Clone, Debug, Default)]
pub(crate) struct Traces {
    /// Expression text as typed.
    pub(crate) src: Vec<String>,
    /// Dataset the source evaluates over.
    pub(crate) dataset: Vec<u8>,
    /// Pane the trace is drawn in.
    pub(crate) pane: Vec<u8>,
    /// RGBA8.
    pub(crate) color: Vec<u32>,
    /// Stroke.
    pub(crate) style: Vec<LineStyle>,
    /// What the source evaluated to; replaced whole on reload, its y alone on a bus threshold.
    pub(crate) eval: Vec<Eval>,
}

/// One trace's evaluation over its dataset.
#[derive(Clone, Debug)]
pub(crate) struct Eval {
    /// The y column.
    pub(crate) y: TraceY,
    /// The trace's own x axis (`fft` bins, per-step results); `None` on the dataset's scale.
    pub(crate) axis: Option<OwnAxis>,
    /// The magnitude of a complex result.
    pub(crate) complex: bool,
    /// Bus width, `1..=MAX_BUS_BITS`; 0 for a single-signal trace.
    pub(crate) bits: u8,
}

/// Where a trace's displayed y values live.
#[derive(Clone, Debug)]
pub(crate) enum TraceY {
    /// A real raw column, borrowed from the dataset (plain `v(x)` / `i(x)` traces).
    Raw(u32),
    /// Evaluated values (the magnitude for a complex result, 0/1 for a comparison, the integer
    /// value of a bus).
    Owned(Vec<f64>),
}

/// A trace's own x axis (`fft`, per-step results), not the dataset's scale.
#[derive(Clone, Debug)]
pub(crate) struct OwnAxis {
    /// One x per y value.
    pub(crate) x: Vec<f64>,
    /// Steps partitioning `x`.
    pub(crate) steps: Vec<Range<u32>>,
}

/// What a source evaluates to.
pub(crate) enum Evaluated {
    /// A column to draw.
    Trace(Eval),
    /// Measurement text (`src = value`).
    Value(String),
}

/// A bus's value column and its width in bits, `1..=MAX_BUS_BITS`.
struct Bus {
    values: Vec<f64>,
    bits: u8,
}

/// Evaluates trace source `src` over `d` (spec waves §2, §4): a bus, a borrowed raw column, an
/// evaluated column, or measurement text. `threshold` is the logic threshold of a bus's bits.
///
/// # Errors
///
/// [`ViewError::Expr`] when the source (or a bit) does not compile or evaluate over `d`;
/// [`ViewError::Bus`] for a malformed bus.
///
/// # Panics
///
/// Panics if `threshold` is not finite (pane properties are valid).
pub(crate) fn evaluate(d: &RawPlot, src: &str, threshold: f64) -> Result<Evaluated, ViewError> {
    assert!(
        threshold.is_finite(),
        "logic threshold {threshold}: pane properties are valid"
    );
    if let Some(body) = bus_body(src) {
        let bus = eval_bus(d, body, threshold)?;
        return Ok(Evaluated::Trace(Eval {
            y: TraceY::Owned(bus.values),
            axis: None,
            complex: false,
            bits: bus.bits,
        }));
    }
    let program = Program::compile(src, Mode::Calc).map_err(ViewError::Expr)?;
    let mut cols = Vec::new();
    calc::bind(&program, d, None, &mut cols).map_err(ViewError::Expr)?;
    if bare_signal(src) && cols.len() == 1 && !d.is_complex() {
        let (k, n) = program.signals().next().expect("one signal");
        let v = d.find_signal(k, n).expect("bound above");
        return Ok(Evaluated::Trace(Eval {
            y: TraceY::Raw(v as u32),
            axis: None,
            complex: false,
            bits: 0,
        }));
    }
    let mut scratch = Scratch::default();
    let c = match calc::eval(&program, d, None, |_| None, &mut scratch).map_err(ViewError::Expr)? {
        CalcResult::Scalar(v) => return Ok(Evaluated::Value(format!("{src} = {}", text(v)))),
        CalcResult::Column(c) => c,
    };
    let complex = c.kind == ColumnKind::Complex;
    let y: Vec<f64> = if complex {
        c.re.iter().zip(&c.im).map(|(r, i)| r.hypot(*i)).collect()
    } else {
        c.re
    };
    if c.axis.is_some() && y.len() == 1 {
        return Ok(Evaluated::Value(format!(
            "{src} = {}",
            si::format_si(y[0], 4)
        )));
    }
    if c.axis.is_none() {
        // The pair of `Waves::xy`'s check: x is then the dataset's scale.
        assert_eq!(
            y.len(),
            d.len(),
            "`{src}`: a column without its own axis is on the scale"
        );
    }
    let axis = c.axis.map(|a| {
        assert_eq!(a.x.len(), y.len(), "one x per value");
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

/// Bus source `src` re-evaluated over `d` with logic threshold `threshold`.
///
/// # Errors
///
/// As [`evaluate`] for a bus.
///
/// # Panics
///
/// Panics if `src` is not a bus source (`{b1, …, bn}`) or `threshold` is not finite.
pub(crate) fn bus_values(d: &RawPlot, src: &str, threshold: f64) -> Result<Vec<f64>, ViewError> {
    assert!(
        threshold.is_finite(),
        "logic threshold {threshold}: pane properties are valid"
    );
    let body = bus_body(src).expect("a bus trace has a bus source");
    let bus = eval_bus(d, body, threshold)?;
    assert_eq!(bus.values.len(), d.len(), "a bus is on its dataset's scale");
    Ok(bus.values)
}

impl Traces {
    /// Number of rows.
    pub(crate) fn len(&self) -> usize {
        debug_assert_eq!(self.eval.len(), self.src.len(), "trace columns in step");
        self.src.len()
    }

    /// Appends a row with a thin stroke.
    ///
    /// # Panics
    ///
    /// Panics at [`MAX_TRACES`] rows (the caller refuses first).
    pub(crate) fn push(&mut self, src: &str, dataset: u8, pane: u8, color: u32, eval: Eval) {
        assert!(self.len() < MAX_TRACES, "caller checks the capacity");
        assert!(usize::from(eval.bits) <= MAX_BUS_BITS, "bus width");
        self.src.push(src.to_owned());
        self.dataset.push(dataset);
        self.pane.push(pane);
        self.color.push(color);
        self.style.push(LineStyle::Thin);
        self.eval.push(eval);
        assert_eq!(self.eval.len(), self.len(), "trace columns in step");
    }

    /// Keeps the rows whose `keep` flag is set, in order.
    ///
    /// # Panics
    ///
    /// Panics unless `keep` has one flag per row.
    pub(crate) fn retain(&mut self, keep: &[bool]) {
        assert_eq!(keep.len(), self.len(), "one flag per row");
        retain_by(&mut self.src, keep);
        retain_by(&mut self.dataset, keep);
        retain_by(&mut self.pane, keep);
        retain_by(&mut self.color, keep);
        retain_by(&mut self.style, keep);
        retain_by(&mut self.eval, keep);
        debug_assert_eq!(self.len(), keep.iter().filter(|&&k| k).count());
    }
}

// ---- private ----

/// The inside of a bus source `{b1, …, bn}`, or `None` when `src` is not one.
fn bus_body(src: &str) -> Option<&str> {
    let body = src.trim().strip_prefix('{')?.strip_suffix('}')?;
    debug_assert!(body.len() + 2 <= src.len(), "the braces are stripped");
    Some(body)
}

/// A bus's value column (spec waves §4): each bit a real column on `d`'s axis, thresholded at
/// `threshold`, the first bit the most significant.
fn eval_bus(d: &RawPlot, body: &str, threshold: f64) -> Result<Bus, ViewError> {
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
        let program = Program::compile(e, Mode::Calc).map_err(ViewError::Expr)?;
        let r = calc::eval(&program, d, None, |_| None, &mut scratch).map_err(ViewError::Expr)?;
        let CalcResult::Column(c) = r else {
            return Err(ViewError::Bus(format!("`{e}` is a value, not a signal")));
        };
        if c.axis.is_some() || c.kind == ColumnKind::Complex || c.re.len() != n {
            return Err(ViewError::Bus(format!(
                "`{e}` is not a real column on the dataset's axis"
            )));
        }
        for (a, &v) in acc.iter_mut().zip(&c.re) {
            *a = (*a << 1) | u32::from(v >= threshold);
        }
    }
    assert!((1..=MAX_BUS_BITS).contains(&parts.len()), "bus width");
    let values: Vec<f64> = acc.into_iter().map(f64::from).collect();
    assert_eq!(values.len(), n, "one value per point");
    Ok(Bus {
        values,
        bits: parts.len() as u8,
    })
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
    assert!(!out.is_empty(), "the text after the last comma is a part");
    debug_assert_eq!(
        out.iter().map(|p| p.len()).sum::<usize>() + out.len() - 1,
        s.len(),
        "the parts and the commas between them cover the text"
    );
    out
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

/// A measurement's value as readout text: engineering units for numbers, `re ± imj` for
/// complex values.
fn text(v: Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Num(x) => si::format_si(x, 6),
        Value::Bool(b) => b.to_string(),
        Value::Cnum { re, im } => {
            let sign = if im < 0.0 { '-' } else { '+' };
            format!(
                "{} {sign} {}j",
                si::format_si(re, 6),
                si::format_si(im.abs(), 6)
            )
        }
        Value::Str(s) => format!("\"{s}\""),
    }
}

/// Keeps the entries of `v` whose `keep` flag is set, in order.
fn retain_by<T>(v: &mut Vec<T>, keep: &[bool]) {
    assert_eq!(v.len(), keep.len(), "one flag per row");
    let mut k = keep.iter();
    v.retain(|_| *k.next().expect("one flag per row"));
    assert!(k.next().is_none(), "every flag was read");
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

    #[test]
    fn only_a_lone_signal_call_is_bare() {
        for (src, bare) in [
            ("v(out)", true),
            (" I (v1) ", true),
            ("v(\"x1.a\")", true),
            ("v(out) * 2", false),
            ("db(v(out))", false),
            ("vout", false),
            ("x(out)", false),
            ("v()", true),
        ] {
            assert_eq!(bare_signal(src), bare, "{src}");
        }
    }
}
