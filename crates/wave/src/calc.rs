//! The calculator over raw data (D12, D23): a [`Mode::Calc`](schemify_expr::Mode::Calc)
//! program's `v()`/`i()` signals bound to [`RawPlot`] columns, evaluated by `schemify_expr`.
//!
//! Binding copies nothing: every [`Column`] borrows the plot's column-major storage, the sweep
//! axis is the plot's scale column and the sweep steps are the plot's steps.

use schemify_expr::{
    CalcResult, Code, Column, Error, Plot, Program, Scratch, SignalKind, Span, Value,
};

use crate::data::{RawPlot, VarKind};

/// Fills `cols` (cleared first) with one borrowed column per [`Program::signals`] entry, in that
/// order, resolving names with [`RawPlot::find_signal`].
///
/// # Errors
///
/// `X004` naming the first signal the plot does not hold.
pub fn bind<'a>(
    prog: &Program,
    plot: &'a RawPlot,
    cols: &mut Vec<Column<'a>>,
) -> Result<(), Error> {
    cols.clear();
    for (kind, name) in prog.signals() {
        let Some(v) = plot.find_signal(kind, name) else {
            let f = if kind == SignalKind::V { "v" } else { "i" };
            return Err(Error::new(
                Code::X004,
                Span::default(),
                format!("no signal `{f}({name})` in `{}`", plot.name()),
            ));
        };
        cols.push(Column {
            re: plot.col(v),
            im: plot.col_im(v),
        });
    }
    debug_assert_eq!(cols.len(), prog.signals().len());
    Ok(())
}

/// Evaluates `prog` over `plot`. Identifiers (bench vars) resolve through `lookup`; the result's
/// strings borrow `scratch`.
///
/// # Errors
///
/// `X004` for an unknown signal or identifier, then every error of
/// [`Program::eval_calc_plain`] (`X003`, `X010`, `X011`, …).
///
/// # Panics
///
/// Panics if `prog` was not compiled in calculator mode.
pub fn eval<'s, 'v>(
    prog: &Program,
    plot: &RawPlot,
    lookup: impl FnMut(&str) -> Option<Value<'v>>,
    scratch: &'s mut Scratch,
) -> Result<CalcResult<'s>, Error> {
    let mut cols = Vec::with_capacity(prog.signals().len());
    bind(prog, plot, &mut cols)?;
    let mut inputs = Vec::with_capacity(prog.idents().len());
    prog.bind(&mut inputs, lookup)?;
    let p = Plot {
        x: plot.scale(),
        steps: plot.steps(),
        signals: &cols,
    };
    prog.eval_calc_plain(&inputs, &p, scratch)
}

/// The calculator expression that names column `v` of `plot`: `v(out)`, `i(v1)`, or the quoted
/// form `i("@m1[id]")` when the name is not an identifier. Xyce's bare `OUT` becomes `v(OUT)` and
/// `V1#branch` becomes `i(V1)`. Compiling the result and binding it resolves back to `v` (the
/// browser relies on this).
///
/// # Panics
///
/// Panics when `v >= plot.vars().len()`.
pub fn signal_expr(plot: &RawPlot, v: usize) -> String {
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
    debug_assert!(out.len() >= 4);
    out
}

#[cfg(test)]
mod tests {
    use schemify_expr::{ColKind, Mode};

    use super::*;
    use crate::data::Variable;

    fn plot() -> RawPlot {
        let vars = [
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
        .collect::<Vec<_>>();
        let n = 4;
        let re = (0..vars.len() * n).map(|k| k as f64).collect();
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
            let prog = Program::compile(&src, Mode::Calc).unwrap();
            bind(&prog, &p, &mut cols).unwrap();
            assert_eq!(cols[0].re, p.col(v), "{src}");
        }
    }

    #[test]
    fn eval_borrows_columns_and_reports_unknown_signals() {
        let p = plot();
        let prog = Program::compile("v(out) * 2 + i(v1)", Mode::Calc).unwrap();
        let mut s = Scratch::default();
        let CalcResult::Column(c) = eval(&prog, &p, |_| None, &mut s).unwrap() else {
            panic!("column expected")
        };
        assert_eq!(c.kind, ColKind::Real);
        let want: Vec<f64> = (0..4).map(|k| p.col(1)[k] * 2.0 + p.col(3)[k]).collect();
        assert_eq!(c.re, want);
        let bad = Program::compile("v(nope)", Mode::Calc).unwrap();
        assert_eq!(
            eval(&bad, &p, |_| None, &mut s).unwrap_err().code,
            Code::X004
        );
        let ident = Program::compile("v(out) * k", Mode::Calc).unwrap();
        assert_eq!(
            eval(&ident, &p, |_| None, &mut s).unwrap_err().code,
            Code::X004
        );
        let k = eval(
            &ident,
            &p,
            |n| (n == "k").then_some(Value::Num(3.0)),
            &mut s,
        )
        .unwrap();
        assert!(matches!(k, CalcResult::Column(c) if c.re[1] == p.col(1)[1] * 3.0));
    }
}
