//! Characterization files (spec `interop.md` §8.3): what a PDK device's CDF callbacks derive
//! from the parameters a user sets, measured in Virtuoso by `SfyCharacterize`. Callbacks are
//! SKILL and never cross to Schemify; this file is the measured behaviour the PDK mirror's
//! rules are built and verified against. It names a PDK's parameters and values, so it stays
//! on the machine it was made on.

use schemify_format::{Document, RecordKind, parse, write};

use crate::design::{copy_attr, copy_header, push_attr};
use crate::sfyx::{self, Record, Value};

/// Most points one device may have (`SfyCharacterize` writes at most 4024).
pub const POINTS_MAX: usize = 10_000;

/// One characterized device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    /// Library and cell.
    pub lib: String,
    /// See [`Device::lib`].
    pub cell: String,
    /// The inputs the sweep set, in the order it set them (`w`, `l`, then the finger count).
    pub inputs: Vec<String>,
    /// The netlisted parameters (spectre `instParameters`), in CDF order.
    pub outputs: Vec<String>,
    /// The points, in file order.
    pub points: Vec<Point>,
}

/// One measured point.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    /// The value set per input (`""`: not set at this point), parallel to [`Device::inputs`].
    pub set: Vec<String>,
    /// Whether every callback ran; else the reason, as SfySetParams gave it.
    pub status: Result<(), String>,
    /// The value read back per netlisted parameter, parallel to [`Device::outputs`].
    pub values: Vec<String>,
}

/// How each netlisted parameter behaved over a device's successful points.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Points whose callbacks all ran.
    pub ok: usize,
    /// Points where a callback failed.
    pub failed: usize,
    /// Parameters that are inputs themselves (they may still be rewritten by a callback).
    pub inputs: Vec<String>,
    /// Parameters that change with the inputs: the callback-derived ones.
    pub derived: Vec<String>,
    /// Parameters that kept one value.
    pub constant: Vec<String>,
}

fn strs(r: &Record, v: &[Value], what: &str) -> Result<Vec<String>, String> {
    v.iter()
        .map(|x| match x {
            Value::Str(s) => Ok(s.clone()),
            _ => Err(format!("line {}: {what}: not a string", r.line)),
        })
        .collect()
}

/// Reads a characterization file.
///
/// # Errors
///
/// A record that is not `char`/`pt` between the `sfyx` header and `end`, a `pt` before any
/// `char`, a `pt` whose lists do not match its `char`'s, or more than [`POINTS_MAX`]
/// points for one device.
pub fn read(text: &str) -> Result<Vec<Device>, String> {
    let recs = sfyx::read(text).map_err(|e| format!("line {}: {}", e.line, e.msg))?;
    let mut out: Vec<Device> = Vec::new();
    for r in &recs {
        match r.tag.as_str() {
            "sfyx" | "end" => {}
            "char" => {
                r.arity(4).map_err(|e| e.msg)?;
                out.push(Device {
                    lib: r.str(0).map_err(|e| e.msg)?.to_owned(),
                    cell: r.str(1).map_err(|e| e.msg)?.to_owned(),
                    inputs: strs(r, r.list(2).map_err(|e| e.msg)?, "inputs")?,
                    outputs: strs(r, r.list(3).map_err(|e| e.msg)?, "netlisted")?,
                    points: Vec::new(),
                });
            }
            "pt" => {
                r.arity(3).map_err(|e| e.msg)?;
                let d = out
                    .last_mut()
                    .ok_or_else(|| format!("line {}: a point before any `char`", r.line))?;
                let set = strs(r, r.list(0).map_err(|e| e.msg)?, "set")?;
                let values = strs(r, r.list(2).map_err(|e| e.msg)?, "values")?;
                if set.len() != d.inputs.len() || values.len() != d.outputs.len() {
                    return Err(format!(
                        "line {}: {} set and {} values for {} inputs and {} parameters",
                        r.line,
                        set.len(),
                        values.len(),
                        d.inputs.len(),
                        d.outputs.len()
                    ));
                }
                if d.points.len() == POINTS_MAX {
                    return Err(format!("line {}: more than {POINTS_MAX} points", r.line));
                }
                let status = match r.str(1).map_err(|e| e.msg)? {
                    "ok" => Ok(()),
                    why => Err(why.to_owned()),
                };
                d.points.push(Point {
                    set,
                    status,
                    values,
                });
            }
            t => return Err(format!("line {}: unknown record `{t}`", r.line)),
        }
    }
    debug_assert!(out.iter().all(|d| d.points.len() <= POINTS_MAX));
    Ok(out)
}

impl Device {
    /// Each netlisted parameter classified over the points whose callbacks ran.
    pub fn summary(&self) -> Summary {
        let ok: Vec<&Point> = self.points.iter().filter(|p| p.status.is_ok()).collect();
        let mut s = Summary {
            ok: ok.len(),
            failed: self.points.len() - ok.len(),
            ..Summary::default()
        };
        for (k, name) in self.outputs.iter().enumerate() {
            let first = ok.first().map(|p| &p.values[k]);
            if ok.iter().all(|p| Some(&p.values[k]) == first) {
                s.constant.push(name.clone());
            } else if self.inputs.contains(name) {
                s.inputs.push(name.clone());
            } else {
                s.derived.push(name.clone());
            }
        }
        debug_assert_eq!(
            s.inputs.len() + s.derived.len() + s.constant.len(),
            self.outputs.len()
        );
        s
    }
}

/// A CDF number as Virtuoso prints it (`6.72f`, `80n`, `1.2u`, `2`): AEL scale factors (`M`
/// is mega), `None` for anything else (`iPar("m")`, `""`).
pub fn ael_num(s: &str) -> Option<f64> {
    let s = s.trim();
    let (body, scale) = match s.chars().last()? {
        c if c.is_ascii_digit() || c == '.' => (s, 1.0),
        c => (
            &s[..s.len() - c.len_utf8()],
            match c {
                'T' => 1e12,
                'G' => 1e9,
                'M' => 1e6,
                'K' | 'k' => 1e3,
                'm' => 1e-3,
                'u' => 1e-6,
                'n' => 1e-9,
                'p' => 1e-12,
                'f' => 1e-15,
                'a' => 1e-18,
                _ => return None,
            },
        ),
    };
    let x: f64 = body.parse().ok()?;
    (x * scale).is_finite().then_some(x * scale)
}

/// Relative agreement a rule must reach at every point: the callbacks print about four
/// significant digits.
pub const REL_TOL: f64 = 2e-3;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= REL_TOL * a.abs().max(b.abs()) + 1e-30
}

/// The rules measured for one device: Schemify expressions over its own params (`w`, `l`,
/// the finger count) that reproduce what the callbacks derive, each checked at every point.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rules {
    /// (param, expression) for each derived netlisted param a rule explains.
    pub computed: Vec<(String, String)>,
    /// Derived params no rule explains: they keep Virtuoso's values (or go stale, §8.3).
    pub unexplained: Vec<String>,
    /// The lengths the callbacks keep as set (l grid points not rewritten), as
    /// `(first, last)` runs of the length table, for the legality check.
    pub legal_l: Vec<(f64, f64)>,
    /// The length table's step.
    pub l_step: Option<f64>,
    /// Points every rule was checked at.
    pub points: usize,
}

/// A point's netlisted (w, l, finger count), `None` when one is not a number.
type Geo<'a> = dyn Fn(&Point) -> Option<(f64, f64, f64)> + 'a;

/// One rule shape. `n` is the finger count, `wf = w/n`, `e` a length table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// Depends on l only: the table itself.
    LOnly,
    /// `(floor(n/2)+1)·wf·e` (source area of a multi-finger MOSFET).
    AreaS,
    /// `ceil(n/2)·wf·e` (drain area).
    AreaD,
    /// `(floor(n/2)+1)·2·(wf+e)` (source perimeter).
    PerimS,
    /// `ceil(n/2)·2·(wf+e)` (drain perimeter).
    PerimD,
}

const SHAPES: [Shape; 5] = [
    Shape::LOnly,
    Shape::AreaS,
    Shape::AreaD,
    Shape::PerimS,
    Shape::PerimD,
];

impl Shape {
    fn count(self, n: f64) -> f64 {
        match self {
            Shape::AreaS | Shape::PerimS => (n / 2.0).floor() + 1.0,
            Shape::AreaD | Shape::PerimD => (n / 2.0).ceil(),
            Shape::LOnly => 1.0,
        }
    }

    /// The value at width `w`, finger count `n`, table value `e`.
    fn eval(self, w: f64, n: f64, e: f64) -> f64 {
        let (c, wf) = (self.count(n), w / n);
        match self {
            Shape::LOnly => e,
            Shape::AreaS | Shape::AreaD => c * wf * e,
            Shape::PerimS | Shape::PerimD => c * 2.0 * (wf + e),
        }
    }

    /// The table value that gives `v` at `w`, `n`.
    fn solve(self, v: f64, w: f64, n: f64) -> f64 {
        let (c, wf) = (self.count(n), w / n);
        match self {
            Shape::LOnly => v,
            Shape::AreaS | Shape::AreaD => v / (c * wf),
            Shape::PerimS | Shape::PerimD => v / (2.0 * c) - wf,
        }
    }

    /// The expression, `n`/`w` the finger count's and width's param names, `e` the table.
    fn expr(self, w: &str, n: &str, e: &str) -> String {
        match self {
            Shape::LOnly => e.to_owned(),
            Shape::AreaS => format!("(floor({n}/2)+1)*({w}/{n})*{e}"),
            Shape::AreaD => format!("ceil({n}/2)*({w}/{n})*{e}"),
            Shape::PerimS => format!("(floor({n}/2)+1)*2*({w}/{n}+{e})"),
            Shape::PerimD => format!("ceil({n}/2)*2*({w}/{n}+{e})"),
        }
    }
}

/// A length table: (requested l, value) runs merged into breakpoints.
#[derive(Clone, Debug, PartialEq)]
struct Table {
    /// (from l, value), increasing l; the last entry's l is the table's end.
    steps: Vec<(f64, f64)>,
    end: f64,
}

impl Table {
    fn from_points(pts: &[(f64, f64)]) -> Option<Table> {
        let (first, last) = (pts.first()?, pts.last()?);
        let mut steps: Vec<(f64, f64)> = vec![*first];
        for &(l, v) in &pts[1..] {
            if !close(steps.last().expect("one at least").1, v) {
                steps.push((l, v));
            }
        }
        (last.0 > first.0).then_some(Table { steps, end: last.0 })
    }

    fn at(&self, l: f64) -> Option<f64> {
        if l < self.steps[0].0 * (1.0 - 1e-9) || l > self.end * (1.0 + 1e-9) {
            return None;
        }
        self.steps
            .iter()
            .rev()
            .find(|(b, _)| *b <= l * (1.0 + 1e-9))
            .map(|s| s.1)
    }

    fn expr(&self, l: &str) -> String {
        let mut s = format!("steps({l}");
        for (b, v) in &self.steps {
            s.push_str(", ");
            schemify_expr::write_num(&mut s, *b);
            s.push_str(", ");
            schemify_expr::write_num(&mut s, *v);
        }
        s.push_str(", ");
        schemify_expr::write_num(&mut s, self.end);
        s.push(')');
        s
    }
}

impl Device {
    /// The rules that reproduce this device's derived params (spec §8.3). Each rule is one
    /// shape (an l-only table, a multi-finger area or perimeter) over a length table solved from the length points, kept only when it
    /// reproduces the param at every successful point within [`REL_TOL`]; the length table
    /// candidate that explains the most params wins. Needs inputs named `w` and `l`.
    pub fn rules(&self) -> Rules {
        let s = self.summary();
        let idx = |n: &str| self.outputs.iter().position(|o| o == n);
        let fing = ["nf", "nfin", "fingers"]
            .into_iter()
            .find(|f| self.inputs.iter().any(|i| i == f));
        let (Some(iw), Some(il)) = (idx("w"), idx("l")) else {
            return Rules {
                unexplained: s.derived,
                ..Rules::default()
            };
        };
        let inf = fing.and_then(idx);
        let ok: Vec<&Point> = self.points.iter().filter(|p| p.status.is_ok()).collect();
        // Per point: netlisted w, l, finger count (read back: what the device netlists).
        let geo = |p: &Point| -> Option<(f64, f64, f64)> {
            Some((
                ael_num(&p.values[iw])?,
                ael_num(&p.values[il])?,
                inf.map_or(Some(1.0), |k| ael_num(&p.values[k]))?,
            ))
        };
        // The length table: points whose width and finger count are the first point's.
        let inl = self.inputs.iter().position(|i| i == "l");
        let lpts: Vec<&Point> = ok
            .iter()
            .copied()
            .filter(|p| {
                self.inputs
                    .iter()
                    .enumerate()
                    .all(|(k, n)| n == "l" || p.set[k] == ok[0].set[k])
            })
            .collect();
        let mut rules = Rules {
            points: ok.len(),
            ..Rules::default()
        };
        // Legal lengths: requested l the callbacks kept.
        if let Some(inl) = inl {
            let mut req: Vec<(f64, bool)> = lpts
                .iter()
                .filter_map(|p| {
                    let want = ael_num(&p.set[inl])?;
                    let got = ael_num(&p.values[il])?;
                    Some((want, close(want, got)))
                })
                .collect();
            req.sort_by(|a, b| a.0.total_cmp(&b.0));
            req.dedup_by(|a, b| close(a.0, b.0));
            rules.l_step = (req.len() > 1).then(|| req[1].0 - req[0].0);
            for (l, legal) in &req {
                match (legal, rules.legal_l.last_mut()) {
                    (true, Some(run)) if close(run.1 + rules.l_step.unwrap_or(0.0), *l) => {
                        run.1 = *l
                    }
                    (true, _) => rules.legal_l.push((*l, *l)),
                    (false, _) => {}
                }
            }
        }
        // Candidate tables: every derived param under every shape, solved on the length
        // points (keyed by the requested length, so a snapped l keeps its own entry).
        let mut best: Option<(Vec<(String, Shape)>, Table)> = None;
        for d in &s.derived {
            let Some(k) = idx(d) else { continue };
            for shape in SHAPES {
                let mut pts: Vec<(f64, f64)> = lpts
                    .iter()
                    .filter_map(|p| {
                        let (w, _, n) = geo(p)?;
                        let l = inl.and_then(|i| ael_num(&p.set[i]))?;
                        Some((l, shape.solve(ael_num(&p.values[k])?, w, n)))
                    })
                    .collect();
                pts.sort_by(|a, b| a.0.total_cmp(&b.0));
                let Some(table) = Table::from_points(&pts) else {
                    continue;
                };
                let explained = self.explained(&s.derived, &table, &ok, &geo, inl);
                if best.as_ref().is_none_or(|b| explained.len() > b.0.len()) {
                    best = Some((explained, table));
                }
            }
        }
        let (w, n) = ("w", fing.unwrap_or("1"));
        if let Some((explained, table)) = best {
            let e = table.expr("l");
            for (name, shape) in &explained {
                rules.computed.push((name.clone(), shape.expr(w, n, &e)));
            }
        }
        rules.unexplained = s
            .derived
            .iter()
            .filter(|d| !rules.computed.iter().any(|c| &c.0 == *d))
            .cloned()
            .collect();
        debug_assert!(rules.computed.len() + rules.unexplained.len() == s.derived.len());
        rules
    }

    /// The derived params `table` explains under some shape at every point, with the shape.
    fn explained(
        &self,
        derived: &[String],
        table: &Table,
        ok: &[&Point],
        geo: &Geo<'_>,
        inl: Option<usize>,
    ) -> Vec<(String, Shape)> {
        let mut out = Vec::new();
        for d in derived {
            let Some(k) = self.outputs.iter().position(|o| o == d) else {
                continue;
            };
            let fits = |shape: Shape| {
                ok.iter().all(|p| {
                    let (Some((w, _, n)), Some(v)) = (geo(p), ael_num(&p.values[k])) else {
                        return false;
                    };
                    // The table is keyed by the requested length.
                    let Some(l) = inl.and_then(|i| ael_num(&p.set[i])) else {
                        return false;
                    };
                    table.at(l).is_some_and(|e| close(shape.eval(w, n, e), v))
                })
            };
            if let Some(shape) = SHAPES.into_iter().find(|s| fits(*s)) {
                out.push((d.clone(), shape));
            }
        }
        out
    }
}

/// `l`'s legality as an expression (spec §8.3): true on the lengths the callbacks kept, on
/// the measured grid. `None` without a length table.
pub fn legal_expr(rules: &Rules, l: &str) -> Option<String> {
    let step = rules.l_step?;
    let tol = step * 1e-3;
    let num = |x: f64| {
        let mut s = String::new();
        schemify_expr::write_num(&mut s, x);
        s
    };
    let on_grid = format!("abs({l}/{} - round({l}/{})) <= 0.001", num(step), num(step));
    let terms: Vec<String> = rules
        .legal_l
        .iter()
        .map(|&(a, b)| {
            if close(a, b) {
                format!("abs({l} - {}) <= {}", num(a), num(tol))
            } else {
                format!("({l} >= {} && {l} <= {})", num(a - tol), num(b + tol))
            }
        })
        .collect();
    (!terms.is_empty()).then(|| format!("({}) && {on_grid}", terms.join(" || ")))
}

/// Appends record `r` of `src` to `out` without the attributes `skip` names.
fn copy_without(out: &mut Document<'static>, src: &Document<'_>, r: u32, skip: &[&str]) {
    out.begin_record(src.record_kind(r));
    for f in src.fields(r) {
        out.push_field(f);
    }
    for a in src.attrs(r) {
        if !skip.contains(&src.attr_key(a)) {
            copy_attr(out, src, a);
        }
    }
    for v in src.verbatim(r) {
        out.push_verbatim(v);
    }
}

/// A mirror cell file with `rules` applied (spec §8.3): each explained param `computed
/// <expr>`, each unexplained derived param `vderived true` (its value is Virtuoso's and goes
/// stale when the inputs change), `l` `valid <expr>` (the lengths the callbacks keep).
///
/// # Errors
///
/// The text is not a Schemify file.
pub fn apply_rules(text: &[u8], rules: &Rules) -> Result<String, String> {
    let src = parse(text).map_err(|e| e.to_string())?;
    let mut out = Document::new(src.kind());
    copy_header(&mut out, &src, &[]);
    let legal = legal_expr(rules, "l");
    for r in 1..src.record_count() {
        let name = src.field(r, 0).unwrap_or("");
        if src.record_kind(r) != RecordKind::Param {
            out.copy_record(&src, r);
        } else if let Some((_, e)) = rules.computed.iter().find(|c| c.0 == name) {
            copy_without(&mut out, &src, r, &["computed"]);
            push_attr(&mut out, "computed", &[e]);
        } else if rules.unexplained.iter().any(|u| u == name) {
            copy_without(&mut out, &src, r, &["vderived"]);
            push_attr(&mut out, "vderived", &["true"]);
        } else if let (Some(v), "l") = (&legal, name) {
            copy_without(&mut out, &src, r, &["valid"]);
            push_attr(&mut out, "valid", &[v]);
        } else {
            out.copy_record(&src, r);
        }
    }
    debug_assert_eq!(out.record_count(), src.record_count());
    Ok(write(&out))
}

/// `text` (a design file) without the overrides of `params` on instances of `cellref`, and
/// how many it dropped; `None` when it drops none. Computed params take no override (E030),
/// and a value from Virtuoso's callbacks is the rule's value at those inputs.
pub fn strip_overrides(text: &[u8], cellref: &str, params: &[&str]) -> Option<(String, usize)> {
    let src = parse(text).ok()?;
    let mut out = Document::new(src.kind());
    copy_header(&mut out, &src, &[]);
    let mut dropped = 0;
    for r in 1..src.record_count() {
        if src.record_kind(r) == RecordKind::Inst && src.field(r, 1) == Some(cellref) {
            dropped += src
                .attrs(r)
                .filter(|&a| params.contains(&src.attr_key(a)))
                .count();
            copy_without(&mut out, &src, r, params);
        } else {
            out.copy_record(&src, r);
        }
    }
    (dropped > 0).then(|| (write(&out), dropped))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"("sfyx" 1 "schemify.il 1")
("char" "pdk" "nch" ("w" "nf") ("w" "nf" "ad" "m"))
("pt" ("1e-06" "1") "ok" ("1u" "1" "1f" "1"))
("pt" ("1e-06" "2") "ok" ("1u" "2" "0.5f" "1"))
("pt" ("4e-06" "1") "\"callback: w failed\"" ("4u" "1" "?" "1"))
("end")
"#;

    #[test]
    fn a_file_reads_and_classifies_its_parameters() {
        let d = &read(FILE).unwrap()[0];
        assert_eq!((d.lib.as_str(), d.cell.as_str()), ("pdk", "nch"));
        assert_eq!(d.points.len(), 3);
        assert!(d.points[2].status.is_err());
        let s = d.summary();
        assert_eq!((s.ok, s.failed), (2, 1));
        // The failed point's values are not evidence: w is constant over the ok points.
        assert_eq!(s.constant, ["w", "m"]);
        assert_eq!(s.inputs, ["nf"]);
        assert_eq!(s.derived, ["ad"]);
    }

    /// A made-up device that derives like a multi-finger MOSFET: an extension table over l
    /// that rises and falls, lengths between 10n and 18n snapped to even nanometres, and
    /// `as ad ps pd` from w, the finger count and the extension.
    fn synthetic() -> Device {
        let ext = |l: f64| match l {
            l if l < 14e-9 => 50e-9,
            l if l < 16e-9 => 44e-9,
            l if l < 20e-9 => 48e-9,
            _ => 60e-9,
        };
        let snap = |l: f64| {
            if l < 18e-9 {
                ((l * 1e9 / 2.0).round() * 2.0) * 1e-9
            } else {
                l
            }
        };
        let mut d = Device {
            lib: "pdk".into(),
            cell: "mos".into(),
            inputs: vec!["w".into(), "l".into(), "nf".into()],
            outputs: ["w", "l", "nf", "as", "ad", "ps", "pd", "lx", "m"]
                .map(String::from)
                .to_vec(),
            points: Vec::new(),
        };
        let mut pt = |w: f64, l: f64, n: f64| {
            let (ls, e) = (snap(l), ext(snap(l)));
            let (s, dr, wf) = ((n / 2.0).floor() + 1.0, (n / 2.0).ceil(), w / n);
            let f = |x: f64| format!("{x:e}");
            d.points.push(Point {
                set: vec![f(w), f(l), format!("{n}")],
                status: Ok(()),
                values: vec![
                    f(w),
                    f(ls),
                    format!("{n}"),
                    f(s * wf * e),
                    f(dr * wf * e),
                    f(s * 2.0 * (wf + e)),
                    f(dr * 2.0 * (wf + e)),
                    f(e),
                    "1".into(),
                ],
            });
        };
        for w in [100e-9, 400e-9, 1.6e-6] {
            for l in [10e-9, 20e-9] {
                for n in [1.0, 2.0, 3.0, 4.0] {
                    pt(w, l, n);
                }
            }
        }
        for k in 0..=30 {
            pt(100e-9, 10e-9 + f64::from(k) * 1e-9, 1.0);
        }
        d
    }

    #[test]
    fn rules_reproduce_every_point_through_the_expression_evaluator() {
        let d = synthetic();
        let r = d.rules();
        assert!(r.unexplained.is_empty(), "{:?}", r.unexplained);
        let names: Vec<&str> = r.computed.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(names, ["as", "ad", "ps", "pd", "lx"]);
        // Oracle: each expression, compiled and evaluated by schemify-expr at every point
        // (the device's own w, requested l, finger count), gives the measured value.
        for (name, src) in &r.computed {
            let k = d.outputs.iter().position(|o| o == name).unwrap();
            let prog = schemify_expr::Program::compile(src, schemify_expr::Mode::Scalar)
                .unwrap_or_else(|e| panic!("{name}: {src}: {e:?}"));
            for p in &d.points {
                let val = |i: usize| schemify_expr::Value::Num(ael_num(&p.set[i]).unwrap());
                let mut inputs = Vec::new();
                prog.bind(&mut inputs, |n| match n {
                    "w" => Some(val(0)),
                    "l" => Some(val(1)),
                    "nf" => Some(val(2)),
                    _ => None,
                })
                .unwrap();
                let mut scratch = schemify_expr::Scratch::default();
                let got = prog.eval(&inputs, &mut scratch).unwrap();
                let schemify_expr::Value::Num(got) = got else {
                    panic!("{name}: {got:?}")
                };
                let want = ael_num(&p.values[k]).unwrap();
                assert!(close(got, want), "{name} at {:?}: {got} vs {want}", p.set);
            }
        }
        // Lengths: 10n..18n only on even nanometres, then every nanometre to 40n.
        assert_eq!(r.l_step.map(|s| (s * 1e9).round()), Some(1.0));
        let runs: Vec<(i64, i64)> = r
            .legal_l
            .iter()
            .map(|(a, b)| ((a * 1e9).round() as i64, (b * 1e9).round() as i64))
            .collect();
        assert_eq!(runs, [(10, 10), (12, 12), (14, 14), (16, 16), (18, 40)]);
    }

    #[test]
    fn a_param_no_shape_explains_stays_unexplained() {
        let mut d = synthetic();
        for (k, p) in d.points.iter_mut().enumerate() {
            p.values[8] = format!("{}", k % 7); // m varies with no rule behind it
        }
        let r = d.rules();
        assert_eq!(r.unexplained, ["m"]);
        assert_eq!(r.computed.len(), 5);
    }

    #[test]
    fn rules_apply_to_a_mirror_cell_and_strip_design_overrides() {
        let r = synthetic().rules();
        let cell = b"schemify 1 shprim\n\npin d inout 0 0\n\nparam w 100n\nparam l 10n\nparam nf 1\nparam as 5f\n  unit m\nparam m 1\n\nimpl spice spice\n  format spice \"M{name} {d} x w={w} as={as}\"\n";
        let out = apply_rules(cell, &r).unwrap();
        assert!(
            out.contains("param as 5f\n  unit m\n  computed \"(floor(nf/2)+1)*(w/nf)*steps(l, "),
            "{out}"
        );
        assert!(out.contains("param l 10n\n  valid "), "{out}");
        // The legality expression accepts what the callbacks kept and nothing else.
        let v = legal_expr(&r, "l").unwrap();
        let prog = schemify_expr::Program::compile(&v, schemify_expr::Mode::Scalar).unwrap();
        for (nm, want) in [
            (10.0, true),
            (11.0, false),
            (12.0, true),
            (13.0, false),
            (18.0, true),
            (25.0, true),
            (40.0, true),
            (41.0, false),
            (25.5, false),
            (9.0, false),
        ] {
            let mut inputs = Vec::new();
            prog.bind(&mut inputs, |_| Some(schemify_expr::Value::Num(nm * 1e-9)))
                .unwrap();
            let mut scratch = schemify_expr::Scratch::default();
            let got = prog.eval(&inputs, &mut scratch).unwrap();
            assert_eq!(got, schemify_expr::Value::Bool(want), "l = {nm}n");
        }
        let design = b"schemify 1 shcomp\n\nimpl schematic schematic\n\ninst a00001 pdk:mos\n  at 0 0 R0\n  name M1\n  w 1u\n  as 3f\n  ad 2f\n\ninst a00002 pdk:other\n  at 10 0 R0\n  name M2\n  as 3f\n";
        let (text, n) = strip_overrides(design, "pdk:mos", &["as", "ad"]).unwrap();
        assert_eq!(n, 2);
        assert!(
            text.contains("  w 1u\n") && !text.contains("  ad 2f"),
            "{text}"
        );
        assert!(
            text.contains("name M2\n  as 3f"),
            "another cell keeps its overrides: {text}"
        );
        assert!(strip_overrides(design, "pdk:none", &["as"]).is_none());
    }

    #[test]
    fn malformed_files_are_refused() {
        let bad = [
            FILE.replace(
                "(\"char\" \"pdk\" \"nch\" (\"w\" \"nf\") (\"w\" \"nf\" \"ad\" \"m\"))\n",
                "",
            ),
            FILE.replace(
                "(\"1e-06\" \"1\") \"ok\" (\"1u\" \"1\" \"1f\" \"1\")",
                "(\"1e-06\") \"ok\" (\"1u\" \"1\" \"1f\" \"1\")",
            ),
            FILE.replace("(\"end\")", "(\"what\")\n(\"end\")"),
        ];
        for b in &bad {
            assert!(read(b).is_err(), "{b}");
        }
    }
}
