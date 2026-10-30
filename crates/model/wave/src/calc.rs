//! The calculator over raw data (D12, D23): a [`Mode::Calc`](schemify_expr::Mode::Calc)
//! program's `v()`/`i()` signals bound to [`RawPlot`] columns, evaluated by `schemify_expr`.
//!
//! Binding copies nothing: every [`Column`] borrows the plot's column-major storage, the sweep
//! axis is the plot's scale column and the sweep steps are the plot's steps.
//!
//! Contents:
//! - `pub fn eval(program, plot, ops, lookup, scratch) -> Result<CalcResult, Error>`
//! - `pub fn op_dataset(plots: &[RawPlot]) -> Option<&RawPlot>`
//! - `pub fn op_value(op: &RawPlot, vector: &str) -> Option<f64>`
//! - `pub fn signal_expr(plot: &RawPlot, v: usize) -> String`
//! - `pub(crate) fn bind(program, plot, ops, cols) -> Result<(), Error>`

use schemify_expr::{CalcResult, Code, Column, Error, Plot, Program, Scratch, SignalKind, Value};

use crate::plot::{RawPlot, VarKind};

/// Evaluates `program` over `plot`. Its `v()`/`i()` signals read `plot`'s columns by
/// [`RawPlot::find_signal`] without copying; `ops[k]` is the value of the `k`-th `op(…)` signal
/// (a run's job reads them from its operating point, run spec §4), `None` where there is none.
/// Identifiers (bench vars) resolve through `lookup`; the result's strings borrow `scratch`.
///
/// # Errors
///
/// `X004` naming the first signal the plot does not hold or an unknown identifier; `X006` for an
/// `op(…)` signal when `ops` is `None`; then every error of [`Program::eval_calc_plain`]
/// (`X003`, `X010`, `X011`, …).
///
/// # Panics
///
/// Panics if `program` was not compiled in calculator mode, or if `ops` does not hold one value
/// per `op(…)` signal.
pub fn eval<'scratch, 'value>(
    program: &Program,
    plot: &RawPlot,
    ops: Option<&[f64]>,
    lookup: impl FnMut(&str) -> Option<Value<'value>>,
    scratch: &'scratch mut Scratch,
) -> Result<CalcResult<'scratch>, Error> {
    let mut cols = Vec::with_capacity(program.signals().len());
    bind(program, plot, ops, &mut cols)?;
    let mut inputs = Vec::with_capacity(program.idents().len());
    program.bind(&mut inputs, lookup)?;
    assert_eq!(
        inputs.len(),
        program.idents().len(),
        "one input per identifier"
    );
    let p = Plot {
        x: plot.scale(),
        steps: plot.steps(),
        signals: &cols,
    };
    program.eval_calc_plain(&inputs, &p, scratch)
}

/// Fills `cols` (cleared first) with one borrowed column per [`Program::signals`] entry, in that
/// order, resolving names with [`RawPlot::find_signal`]. `ops[k]` is the value of the `k`-th
/// [`SignalKind::Op`] signal (in signal order): a run's job reads them from its operating point
/// (run spec §4); `None` where there is none.
///
/// # Errors
///
/// `X004` naming the first signal the plot does not hold; `X006` for an `op(…)` signal when
/// `ops` is `None`.
///
/// # Panics
///
/// Panics if `ops` does not hold one value per `op(…)` signal.
pub(crate) fn bind<'plot>(
    program: &Program,
    plot: &'plot RawPlot,
    ops: Option<&'plot [f64]>,
    cols: &mut Vec<Column<'plot>>,
) -> Result<(), Error> {
    cols.clear();
    let mut op_index = 0;
    for (k, (kind, name)) in program.signals().enumerate() {
        if kind == SignalKind::Op {
            let Some(x) = ops.and_then(|o| o.get(op_index)) else {
                return Err(Error::new(
                    Code::X006,
                    program.signal_span(k),
                    format!(
                        "op({name}) needs a simulation job's operating point: use it in a bench `output`"
                    ),
                ));
            };
            op_index += 1;
            cols.push(Column {
                re: core::slice::from_ref(x),
                im: &[],
            });
            continue;
        }
        let Some(v) = plot.find_signal(kind, name) else {
            let f = if kind == SignalKind::V { "v" } else { "i" };
            return Err(Error::new(
                Code::X004,
                program.signal_span(k),
                format!("no signal `{f}({name})` in `{}`", plot.name()),
            ));
        };
        cols.push(Column {
            re: plot.col(v),
            im: plot.col_im(v),
        });
    }
    assert_eq!(
        cols.len(),
        program.signals().len(),
        "one column per signal, in order"
    );
    assert!(
        ops.is_none_or(|o| o.len() == op_index),
        "one value per op() signal"
    );
    Ok(())
}

/// The operating-point dataset of a raw file's plots: the one whose plot name says
/// "operating point" (ngspice, ESPice), else a one-point `dc` plot (Xyce writes `.op` that way).
#[must_use]
pub fn op_dataset(plots: &[RawPlot]) -> Option<&RawPlot> {
    let named = |p: &&RawPlot, s: &str| p.name().to_ascii_lowercase().contains(s);
    plots
        .iter()
        .find(|p| named(p, "operating point"))
        .or_else(|| plots.iter().find(|p| named(p, "dc") && p.len() == 1))
}

/// The value of ngspice device vector `vector` (`@m.x1.mm2[gm]`) at the operating point `op`:
/// the vector by that name, or as ngspice writes saved device vectors (`i(@…)`, `v(@…)`),
/// compared case-insensitively; `None` when `op` lacks it.
///
/// # Panics
///
/// Panics if `vector` does not start with `@` (it names no device vector).
#[must_use]
pub fn op_value(op: &RawPlot, vector: &str) -> Option<f64> {
    assert!(vector.starts_with('@'), "a device vector: {vector}");
    let v = op
        .find_var(vector)
        .or_else(|| op.find_var(&format!("i({vector})")))
        .or_else(|| op.find_var(&format!("v({vector})")))?;
    op.col(v).first().copied()
}

/// The calculator expression that names column `v` of `plot`: `v(out)`, `i(v1)`, or the quoted
/// form `i("@m1[id]")` when the name is not an identifier. Xyce's bare `OUT` becomes `v(OUT)` and
/// `V1#branch` becomes `i(V1)`. Compiling the result and binding it resolves back to `v` (the
/// browser relies on this).
///
/// # Panics
///
/// Panics when `v >= plot.vars().len()`.
#[must_use]
pub fn signal_expr(plot: &RawPlot, v: usize) -> String {
    let columns = plot.vars().len();
    assert!(v < columns, "column {v} out of range ({columns} columns)");
    let var = &plot.vars()[v];
    let name = var.name.as_str();
    let inner = |p: char| {
        let b = name.as_bytes();
        (b.len() > 3
            && b[0].eq_ignore_ascii_case(&(p as u8))
            && b[1] == b'('
            && name.ends_with(')'))
        .then(|| &name[2..name.len() - 1])
    };
    let (f, arg) = if let Some(a) = inner('v') {
        ('v', a)
    } else if let Some(a) = inner('i') {
        ('i', a)
    } else if let Some(a) = name
        .len()
        .checked_sub(7)
        .filter(|&k| name.is_char_boundary(k) && name[k..].eq_ignore_ascii_case("#branch"))
        .map(|k| &name[..k])
    {
        ('i', a)
    } else if var.kind == VarKind::Current {
        ('i', name)
    } else {
        ('v', name)
    };
    let ident = arg
        .bytes()
        .enumerate()
        .all(|(k, c)| c == b'_' || c.is_ascii_alphabetic() || (k > 0 && c.is_ascii_digit()));
    let out = if ident && !arg.is_empty() {
        format!("{f}({arg})")
    } else {
        let quoted = arg.replace('\\', "\\\\").replace('"', "\\\"");
        format!("{f}(\"{quoted}\")")
    };
    debug_assert!(out.starts_with(f), "a `{f}(…)` call");
    debug_assert!(out.ends_with(')'), "a `{f}(…)` call");
    out
}

#[cfg(test)]
mod tests {
    use schemify_expr::{ColumnKind, Mode};

    use super::*;
    use crate::plot::Variable;

    fn plot() -> RawPlot {
        let vars: Vec<Variable> = [
            "time",
            "v(out)",
            "IN",
            "V1#branch",
            "i(@m1[id])",
            "v(x1.a)",
            "pow",
        ]
        .iter()
        .zip([
            VarKind::Time,
            VarKind::Voltage,
            VarKind::Voltage,
            VarKind::Current,
            VarKind::Current,
            VarKind::Voltage,
            VarKind::Other,
        ])
        .map(|(n, kind)| Variable {
            name: (*n).into(),
            kind,
        })
        .collect();
        let n = 4;
        let re: Vec<f64> = (0..vars.len() * n).map(|k| k as f64).collect();
        RawPlot::from_columns("t", vars, re, vec![]).unwrap()
    }

    #[test]
    fn signal_expr_round_trips_through_bind() {
        let p = plot();
        let want = [
            "v(time)",
            "v(out)",
            "v(IN)",
            "i(V1)",
            "i(\"@m1[id]\")",
            "v(\"x1.a\")",
            "v(pow)",
        ];
        let mut cols = Vec::new();
        for (v, w) in want.iter().enumerate() {
            let src = signal_expr(&p, v);
            assert_eq!(&src, w);
            let program = Program::compile(&src, Mode::Calc).unwrap();
            bind(&program, &p, None, &mut cols).unwrap();
            assert_eq!(cols[0].re, p.col(v), "{src}");
        }
    }

    #[test]
    fn eval_borrows_columns_and_reports_unknown_signals() {
        let p = plot();
        let program = Program::compile("v(out) * 2 + i(v1)", Mode::Calc).unwrap();
        let mut s = Scratch::default();
        let CalcResult::Column(c) = eval(&program, &p, None, |_| None, &mut s).unwrap() else {
            panic!("column expected")
        };
        assert_eq!(c.kind, ColumnKind::Real);
        let want: Vec<f64> = (0..4).map(|k| p.col(1)[k] * 2.0 + p.col(3)[k]).collect();
        assert_eq!(c.re, want);
        let bad = Program::compile("v(nope)", Mode::Calc).unwrap();
        assert_eq!(
            eval(&bad, &p, None, |_| None, &mut s).unwrap_err().code,
            Code::X004
        );
        let ident = Program::compile("v(out) * k", Mode::Calc).unwrap();
        assert_eq!(
            eval(&ident, &p, None, |_| None, &mut s).unwrap_err().code,
            Code::X004
        );
        let k = eval(
            &ident,
            &p,
            None,
            |n| (n == "k").then_some(Value::Num(3.0)),
            &mut s,
        )
        .unwrap();
        assert!(matches!(k, CalcResult::Column(c) if c.re[1] == p.col(1)[1] * 3.0));
    }
}
