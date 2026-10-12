//! Device limits for D13 gating (D14): `min`/`max` on `L`/`W` from model-card bins,
//! from recognised `drc=` procs, and `grid` from the tech file's manufacturing grid.
//!
//! Units follow the symbol's own convention, read from the parameter's default: a
//! default with an SI suffix or below 1e-3 is in metres (gf180 `0.28u`, IHP `0.13u`);
//! a plain default of 1e-3 or more is in micrometres (sky130 `L=0.15` with
//! `.option scale=1e-6`). Model bins are metres and are converted accordingly.

use crate::spice::{self, ModelScan};
use crate::tcl::{Bound, DrcProc, regex_match};
use crate::template::spice_number;

/// What one symbol's limits come from.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Inputs<'a> {
    /// `(param name, default as written)` of the symbol template.
    pub params: &'a [(String, String)],
    /// The SPICE `format` (to find the model name: `sky130_fd_pr__@model`).
    pub format: Option<&'a str>,
    /// The `drc=` attribute, if any.
    pub drc: Option<&'a str>,
    /// The symbol file name (`nfet_01v8.sym`), for procs that test `$symbol`.
    pub file_name: &'a str,
    /// Model scans to look bins up in.
    pub models: &'a [ModelScan],
    /// Recognised drc procs.
    pub procs: &'a [DrcProc],
    /// Manufacturing grid in picometres.
    pub grid_pm: Option<u32>,
}

/// One limit attribute for a parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Limit {
    /// Parameter name.
    pub param: String,
    /// `min`, `max` or `grid`.
    pub attr: &'static str,
    /// Expression text in the parameter's units.
    pub expr: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Metre,
    Micro,
}

fn unit_of(default: &str) -> Unit {
    let t = default.trim();
    let suffixed = t.ends_with(|c: char| c.is_ascii_alphabetic());
    match spice::parse_number(t) {
        Some(v) if !suffixed && v.abs() >= 1e-3 => Unit::Micro,
        _ => Unit::Metre,
    }
}

/// Shortest text for `v` after rounding to 12 significant digits (drops float noise
/// such as `0.15000000000000002`).
fn num(v: f64) -> String {
    let r: f64 = format!("{v:.11e}").parse().expect("formatted float parses");
    let s = format!("{r}");
    assert!(spice_number(&s).is_some(), "{s} is a literal");
    s
}

/// `v` metres with an SI suffix (`0.28u`, `5n`), as PDK symbols write lengths.
fn si(v: f64) -> String {
    let a = v.abs();
    if a >= 1e-3 || a == 0.0 {
        return num(v);
    }
    let (scale, suffix) = [(1e-6, "u"), (1e-9, "n")]
        .into_iter()
        .find(|(s, _)| a >= s * 0.1)
        .unwrap_or((1e-12, "p"));
    format!("{}{suffix}", num(v / scale))
}

/// Derives limit attributes; returns them with review notes (unrecognised drc parts).
pub(crate) fn derive(inp: &Inputs) -> (Vec<Limit>, Vec<String>) {
    let mut out: Vec<Limit> = Vec::new();
    let mut review = Vec::new();
    let find = |want: &str| {
        inp.params
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(want))
            .map(|(n, d)| (n.clone(), unit_of(d)))
    };
    let (l, w) = (find("l"), find("w"));
    // Model bins.
    if let (Some(fmt), Some((_, model))) =
        (inp.format, inp.params.iter().find(|(n, _)| n == "model"))
    {
        let spice_name = fmt
            .split_whitespace()
            .find(|word| word.contains("@model"))
            .map(|word| word.replace("@model", model.trim()));
        if let Some(name) = spice_name.filter(|n| !n.contains('@')) {
            let bins: Vec<spice::Bin> = inp.models.iter().flat_map(|s| s.bins_of(&name)).collect();
            let (lr, wr) = spice::union(&bins);
            for (p, ranges) in [(&l, &lr), (&w, &wr)] {
                let (Some((param, unit)), Some(first), Some(last)) =
                    (p, ranges.first(), ranges.last())
                else {
                    continue;
                };
                let text = |v: f64| {
                    if *unit == Unit::Micro {
                        num(v * 1e6)
                    } else {
                        si(v)
                    }
                };
                if first[0] > 0.0 {
                    out.push(Limit {
                        param: param.clone(),
                        attr: "min",
                        expr: text(first[0]),
                    });
                }
                if last[1].is_finite() {
                    out.push(Limit {
                        param: param.clone(),
                        attr: "max",
                        expr: text(last[1]),
                    });
                }
            }
        }
    }
    // drc procs.
    if let Some(call) = inp.drc {
        drc_limits(call, inp, &mut out, &mut review);
    }
    // Manufacturing grid.
    if let Some(pm) = inp.grid_pm.filter(|&g| g > 0) {
        for (param, unit) in [&l, &w].into_iter().flatten() {
            let expr = match unit {
                Unit::Micro => num(f64::from(pm) * 1e-6),
                Unit::Metre if pm % 1000 == 0 => format!("{}n", pm / 1000),
                Unit::Metre => format!("{pm}p"),
            };
            out.push(Limit {
                param: param.clone(),
                attr: "grid",
                expr,
            });
        }
    }
    (merge(out), review)
}

/// Combines several bounds on one (param, attr): the tighter wins (`max(…)` of minima,
/// `min(…)` of maxima). Order: first appearance.
fn merge(limits: Vec<Limit>) -> Vec<Limit> {
    let mut out: Vec<Limit> = Vec::new();
    for l in limits {
        match out
            .iter_mut()
            .find(|o| o.param == l.param && o.attr == l.attr)
        {
            Some(o) if o.expr != l.expr => {
                let f = if l.attr == "max" { "min" } else { "max" };
                o.expr = format!("{f}({}, {})", o.expr, l.expr);
            }
            Some(_) => {}
            None => out.push(l),
        }
    }
    out
}

fn drc_limits(call: &str, inp: &Inputs, out: &mut Vec<Limit>, review: &mut Vec<String>) {
    let mut words = call.split_whitespace();
    let Some(proc_name) = words.next() else {
        return;
    };
    let actual: Vec<String> = words
        .map(|w| {
            w.trim_matches(|c| c == '{' || c == '}' || c == '\\')
                .to_owned()
        })
        .collect();
    let Some(p) = inp.procs.iter().find(|p| p.name == proc_name) else {
        review.push(format!(
            "drc proc {proc_name} not found among the xschemrc procs: {call}"
        ));
        return;
    };
    if p.arms.is_empty() {
        let first = p.unknown.first().map_or("", String::as_str);
        review.push(format!(
            "{proc_name}: no `switch -regexp $model` rule set (first line `{first}`)"
        ));
        return;
    }
    for u in &p.unknown {
        review.push(format!("{proc_name}: unrecognised line `{u}`"));
    }
    let param_of = |formal: &str| -> Option<String> {
        let k = p.args.iter().position(|a| a == formal)?;
        actual.get(k)?.strip_prefix('@').map(str::to_owned)
    };
    let default_of = |param: &str| {
        inp.params
            .iter()
            .find(|(n, _)| n == param)
            .map(|(_, d)| d.trim().to_owned())
    };
    let Some(model) = param_of("model").and_then(|m| default_of(&m)) else {
        review.push(format!(
            "{proc_name}: no model argument to select a rule set: {call}"
        ));
        return;
    };
    let Some(arm) = p.arms.iter().find(|a| regex_match(&a.pattern, &model)) else {
        return; // the proc has no rules for this model
    };
    for u in &arm.unknown {
        review.push(format!(
            "{proc_name} {{{}}}: unrecognised rule `{u}`",
            arm.pattern
        ));
    }
    for r in &arm.rules {
        let Some(param) = param_of(&r.var) else {
            review.push(format!(
                "{proc_name}: rule on `{}` is not bound to a parameter",
                r.var
            ));
            continue;
        };
        let Some(default) = default_of(&param) else {
            review.push(format!(
                "{proc_name}: parameter {param} not in the template"
            ));
            continue;
        };
        let metre = unit_of(&default) == Unit::Metre;
        let micro = p.micro.contains(&r.var) && r.value >= 1e-3 && metre;
        let mut expr = match (micro, metre) {
            (true, _) => format!("{}u", num(r.value)),
            (false, true) => si(r.value),
            (false, false) => num(r.value),
        };
        for d in p.divisions.iter().filter(|d| d.var == r.var) {
            let applies = d
                .unless_symbol
                .as_ref()
                .is_none_or(|re| !regex_match(re, inp.file_name));
            if !applies {
                continue;
            }
            match param_of(&d.by) {
                Some(by) => expr = format!("{expr}*{by}"),
                None => review.push(format!(
                    "{proc_name}: divisor `{}` is not a parameter",
                    d.by
                )),
            }
        }
        let attr = match r.bound {
            Bound::Min => "min",
            Bound::Max => "max",
        };
        out.push(Limit { param, attr, expr });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tcl::drc_procs;

    fn s(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn drc_rules_bins_and_grid() {
        let procs = drc_procs(
            "proc fet_drc {instance symbol model w l ng } {\n  regsub {u$} $w {} w\n  set w [expr { double($w) / double($ng)}]\n  switch -regexp $model {\n    {sg13_lv_nmos$} {\n      if { $w < 0.13 } {\n      }\n      if { $l > 10.0 } {\n      }\n    }\n  }\n}\n",
        );
        let params = s(&[
            ("w", "0.15u"),
            ("l", "0.13u"),
            ("ng", "1"),
            ("model", "sg13_lv_nmos"),
        ]);
        let inp = Inputs {
            params: &params,
            format: Some("@spiceprefix@name @pinlist @model w=@w"),
            drc: Some("fet_drc @name @symname @model @w @l @ng"),
            file_name: "sg13_lv_nmos.sym",
            models: &[],
            procs: &procs,
            grid_pm: Some(5000),
        };
        let (lim, review) = derive(&inp);
        assert!(review.is_empty(), "{review:?}");
        let got: Vec<_> = lim
            .iter()
            .map(|l| (l.param.as_str(), l.attr, l.expr.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("w", "min", "0.13u*ng"),
                ("l", "max", "10"),
                ("l", "grid", "5n"),
                ("w", "grid", "5n")
            ]
        );
        // Micrometre convention (sky130): thresholds and grid in µm.
        let params = s(&[("W", "1"), ("L", "0.15"), ("model", "nfet_01v8")]);
        let inp = Inputs {
            params: &params,
            drc: None,
            ..inp
        };
        let (lim, _) = derive(&inp);
        assert_eq!(lim[0].expr, "0.005");
        assert_eq!(
            merge(vec![
                Limit {
                    param: "w".into(),
                    attr: "min",
                    expr: "1".into()
                },
                Limit {
                    param: "w".into(),
                    attr: "min",
                    expr: "2".into()
                },
            ])[0]
                .expr,
            "max(1, 2)"
        );
    }
}
