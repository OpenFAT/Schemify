//! Checks (netlist spec §8): the code table, the [`Violations`] table, D13 gating on
//! evaluated values with waivers, and the per-cell checks over instances and the
//! expression graph. Connectivity findings come from `connect`; hierarchy findings
//! (`E013`, `E014`, `E031`, `E033`–`E035`) from `elaborate`.

use crate::graph::{Graph, Work};
use crate::iface::{GRID, Iface, MAX, MIN, ParamKind, STEP};
use crate::names::{Name, Names};
use crate::values::{Code as PCode, Funcs, NONE, Val};
use schemify_doc::{Document, Instances, ObjId};
use std::cmp::Ordering;

/// Check codes (netlist spec §8; `E034`, `E035` are elab amendments).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(missing_docs)] // meanings: `Code::meaning`
pub enum Code {
    E001,
    E002,
    E003,
    W004,
    E005,
    E006,
    W007,
    E008,
    E009,
    W010,
    E011,
    E012,
    E013,
    E014,
    W016,
    E030,
    E031,
    E032,
    E033,
    E034,
    E035,
    I005,
}

/// How much a violation matters (spec §8): `Error` blocks Run and netlisting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Severity {
    /// Blocks.
    Error,
    /// Reported.
    Warning,
    /// Informational (waived violations).
    Info,
}

impl Code {
    /// Returns the code as written in reports (`"E005"`).
    pub fn as_str(self) -> &'static str {
        const S: [&str; 22] = [
            "E001", "E002", "E003", "W004", "E005", "E006", "W007", "E008", "E009", "W010", "E011",
            "E012", "E013", "E014", "W016", "E030", "E031", "E032", "E033", "E034", "E035", "I005",
        ];
        S[self as usize]
    }

    /// Returns the severity, from the code's letter.
    pub fn severity(self) -> Severity {
        match self.as_str().as_bytes()[0] {
            b'E' => Severity::Error,
            b'W' => Severity::Warning,
            _ => Severity::Info,
        }
    }

    /// Returns whether a `waive` record can downgrade it to `I005` (D13).
    pub fn waivable(self) -> bool {
        matches!(self, Code::E005 | Code::E006 | Code::E032)
    }

    /// Returns the spec meaning.
    pub fn meaning(self) -> &'static str {
        match self {
            Code::E001 => "missing lib/cell, or bench instantiated",
            Code::E002 => "impl pins differ from symbol pins",
            Code::E003 => "shorted names on one net",
            Code::W004 => "instance pin unconnected",
            Code::E005 => "param outside min/max",
            Code::E006 => "param not in choices",
            Code::W007 => "override of unknown param",
            Code::E008 => "expression error",
            Code::E009 => "duplicate instance name in a cell",
            Code::W010 => "real-domain pin on an analog/digital net",
            Code::E011 => "bus width mismatch",
            Code::E012 => "port without interface pin, or interface pin without port",
            Code::E013 => "dialect unsupported by selected impl",
            Code::E014 => "hierarchy recursion",
            Code::W016 => "net with a single connection",
            Code::E030 => "override of computed param",
            Code::E031 => "config names an impl the cell lacks",
            Code::E032 => "param off grid/step",
            Code::E033 => ".control in a code block for xyce",
            Code::E034 => "inherited pins of one name with different default nets",
            Code::E035 => "unknown corner or config",
            Code::I005 => "waived violation",
        }
    }
}

/// One finding. Keyed by (code, cell, object, path, param).
#[derive(Clone, Debug, PartialEq)]
pub struct Violation {
    /// What.
    pub code: Code,
    /// Display name of the cell whose document holds the object (`lib:cell`).
    pub cell: Name,
    /// The object id in that document, or [`ObjId::NONE`].
    pub object: ObjId,
    /// Hierarchical instance path (`/X1/X3`), or [`Name::NONE`] when the finding holds
    /// for every path through the object.
    pub path: Name,
    /// Param, pin, net or impl name, or [`Name::NONE`].
    pub param: Name,
    /// Human-readable detail (cold).
    pub detail: String,
}

/// Most violations one table holds (ARCHITECTURE capacity).
pub const VIOLATIONS_MAX: usize = 1 << 20;

/// Violations, sorted by key and unique per key.
#[derive(Clone, Debug, Default)]
pub struct Violations {
    /// Rows, sorted by (code text, cell text, object, path text, param text).
    pub rows: Vec<Violation>,
}

impl Violations {
    /// Sorts `rows` by key and keeps the first row of each key. Beyond
    /// [`VIOLATIONS_MAX`] rows the tail is dropped (the count stays visible as the cap).
    pub(crate) fn from_rows(mut rows: Vec<Violation>, names: &Names) -> Violations {
        rows.sort_by(|a, b| cmp_key(a, b, names));
        rows.dedup_by(|b, a| cmp_key(a, b, names) == Ordering::Equal);
        rows.truncate(VIOLATIONS_MAX);
        debug_assert!(
            rows.windows(2)
                .all(|w| cmp_key(&w[0], &w[1], names) == Ordering::Less)
        );
        Violations { rows }
    }

    /// Returns whether any row blocks (severity `Error`).
    pub fn blocking(&self) -> bool {
        self.rows
            .iter()
            .any(|v| v.code.severity() == Severity::Error)
    }

    /// Returns the rows with `code`.
    pub fn with(&self, code: Code) -> impl Iterator<Item = &Violation> + '_ {
        self.rows.iter().filter(move |v| v.code == code)
    }
}

fn cmp_key(a: &Violation, b: &Violation, names: &Names) -> Ordering {
    a.code
        .as_str()
        .cmp(b.code.as_str())
        .then_with(|| names.cmp(a.cell, b.cell))
        .then(a.object.cmp(&b.object))
        .then_with(|| names.cmp(a.path, b.path))
        .then_with(|| names.cmp(a.param, b.param))
}

/// Waivers of one document: `(instance id, param)` pairs, sorted.
pub(crate) fn waivers(doc: &Document, names: &mut Names) -> Vec<(ObjId, Name)> {
    let s = doc.strings();
    let w = doc.waivers();
    let mut out: Vec<(ObjId, Name)> = (0..w.kind.len())
        .filter_map(|r| {
            let inst = ObjId::find(s.get(w.fields[r][1]), s)?;
            Some((inst, names.intern(s.get(w.fields[r][2]))))
        })
        .collect();
    out.sort_unstable();
    out
}

/// Gating (D13) of one evaluated param value. Appends `(code, detail)` for each
/// violated rule; `bounds` are the evaluated `min`, `max`, `step`, `grid`.
pub(crate) fn gate(
    v: &Val,
    kind: ParamKind,
    bounds: &[Option<Val>; 4],
    choices: &[Name],
    names: &Names,
    out: &mut Vec<(Code, String)>,
) {
    let mut text = String::new();
    if !v.is_err() {
        v.write_text(&mut text);
    }
    if let Some(x) = v.as_f64() {
        let below = bounds[MIN]
            .as_ref()
            .and_then(Val::as_f64)
            .filter(|m| x < *m);
        let above = bounds[MAX]
            .as_ref()
            .and_then(Val::as_f64)
            .filter(|m| x > *m);
        if let Some(m) = below {
            out.push((Code::E005, format!("{text} is below min {}", num_text(m))));
        }
        if let Some(m) = above {
            out.push((Code::E005, format!("{text} is above max {}", num_text(m))));
        }
        let grid = bounds[GRID]
            .as_ref()
            .and_then(Val::as_f64)
            .filter(|g| *g != 0.0);
        let step = bounds[STEP]
            .as_ref()
            .and_then(Val::as_f64)
            .filter(|s| *s != 0.0);
        if let Some(g) = grid {
            if !on_grid(x, 0.0, g) {
                out.push((Code::E032, format!("{text} is off grid {}", num_text(g))));
            }
        } else if let Some(st) = step {
            let base = bounds[MIN].as_ref().and_then(Val::as_f64).unwrap_or(0.0);
            if !on_grid(x, base, st) {
                out.push((
                    Code::E032,
                    format!(
                        "{text} is off step {} from {}",
                        num_text(st),
                        num_text(base)
                    ),
                ));
            }
        }
    }
    if !choices.is_empty() && !v.is_err() {
        let hit = choices.iter().any(|c| {
            let c = names.get(*c);
            c == text
                || matches!((schemify_expr::parse_num(c), v.as_f64()), (Ok(a), Some(b)) if a == b)
        });
        if !hit || (kind == ParamKind::Choice && v.as_f64().is_some() && !hit) {
            out.push((Code::E006, format!("{text} is not one of the choices")));
        }
    }
}

/// Whether `x` is `base + k·step` for an integer `k`, to a relative 1e-9 (values are
/// f64 from decimal text, so `0.3` on a `0.1` grid must pass).
fn on_grid(x: f64, base: f64, step: f64) -> bool {
    let r = (x - base) / step;
    (r - r.round()).abs() <= 1e-9 * r.abs().max(1.0)
}

fn num_text(x: f64) -> String {
    let mut s = String::new();
    schemify_expr::write_num(&mut s, x);
    s
}

/// What one evaluation context of a cell is checked against.
pub(crate) struct CellCtx<'a> {
    pub(crate) doc: &'a Document,
    pub(crate) ifaces: &'a [Iface],
    pub(crate) own: u32,
    pub(crate) inst_iface: &'a [u32],
    pub(crate) graph: &'a Graph,
    pub(crate) waivers: &'a [(ObjId, Name)],
    /// Instance path prefix for context-specific findings, or `NONE`.
    pub(crate) path: Name,
}

/// Instance-level checks that do not depend on values: `E001` (unresolved cell, given
/// as `unresolved[i]`), `W007` (override of an unknown param), `E030` (override of a
/// computed param).
pub(crate) fn check_overrides(
    cx: &CellCtx<'_>,
    unresolved: &[Option<String>],
    names: &mut Names,
    out: &mut Vec<Violation>,
) {
    let doc = cx.doc;
    let s = doc.strings();
    let inst = doc.instances();
    let cell = cx.ifaces[cx.own as usize].cell;
    assert_eq!(unresolved.len(), inst.id.len());
    for i in 0..inst.id.len() {
        if let Some(why) = &unresolved[i] {
            out.push(Violation {
                code: Code::E001,
                cell,
                object: inst.id[i],
                path: Name::NONE,
                param: Name::NONE,
                detail: why.clone(),
            });
            continue;
        }
        let Some(c) = cx.ifaces.get(cx.inst_iface[i] as usize) else {
            continue;
        };
        for (k, _) in doc.pool().attrs(inst.attrs[i]) {
            let key = s.get(k);
            if !Instances::is_override(key) {
                continue;
            }
            let name = names.intern(key);
            let (code, detail) = match c.param(name) {
                None => (
                    Code::W007,
                    format!("{} has no param `{key}`", names.get(c.cell)),
                ),
                Some(p) if c.param_computed[p] => (Code::E030, format!("`{key}` is computed")),
                Some(_) => continue,
            };
            out.push(Violation {
                code,
                cell,
                object: inst.id[i],
                path: Name::NONE,
                param: name,
                detail,
            });
        }
    }
}

/// Value checks of one evaluation (`vals`): `E008` for every node whose own
/// expression failed (child-scope compile errors are the child cell's, reported
/// there), and D13 gating per instance param, downgraded to `I005` when waived.
pub(crate) fn check_values<F: Funcs>(
    cx: &CellCtx<'_>,
    vals: &[Val],
    names: &Names,
    funcs: &mut F,
    ws: &mut Work,
    out: &mut Vec<Violation>,
) {
    let g = cx.graph;
    let me = &cx.ifaces[cx.own as usize];
    let inst = cx.doc.instances();
    assert_eq!(vals.len(), g.len());
    for n in 0..g.len() as u32 {
        let Val::Err(e) = &vals[n as usize] else {
            continue;
        };
        let owner = g.owner[n as usize];
        let child_scope = owner != NONE && g.table[n as usize] != crate::graph::OWN;
        if child_scope && matches!(g.code(n, cx.ifaces), PCode::Bad(_)) {
            continue;
        }
        let slot = g.slot[n as usize];
        let param = if owner == NONE {
            if slot & crate::values::VAR == 0 {
                me.param_name[slot as usize]
            } else {
                g.var_name[(slot & !crate::values::VAR) as usize]
            }
        } else {
            let c = &cx.ifaces[g.inst_iface[owner as usize] as usize];
            if slot & crate::values::VAR == 0 {
                c.param_name[slot as usize]
            } else {
                c.var_name[(slot & !crate::values::VAR) as usize]
            }
        };
        out.push(Violation {
            code: Code::E008,
            cell: me.cell,
            object: if owner == NONE {
                ObjId::NONE
            } else {
                inst.id[owner as usize]
            },
            path: cx.path,
            param,
            detail: format!("{}: {}", e.code.as_str(), e.msg),
        });
    }
    let mut found = Vec::new();
    for i in 0..inst.id.len() {
        let Some(c) = cx.ifaces.get(cx.inst_iface[i] as usize) else {
            continue;
        };
        let first = g.inst_first[i];
        for k in 0..c.param_name.len() {
            let has_rules = c.param_bound[k].iter().any(|b| *b != NONE)
                || c.choice_start[k] != c.choice_start[k + 1];
            if !has_rules {
                continue;
            }
            let v = &vals[(first + k as u32) as usize];
            if v.is_err() {
                continue;
            }
            let mut bounds: [Option<Val>; 4] = Default::default();
            for (which, b) in bounds.iter_mut().enumerate() {
                *b = g.bound(i, k, which, vals, cx.ifaces, funcs, ws);
            }
            found.clear();
            gate(
                v,
                c.param_kind[k],
                &bounds,
                c.choices_of(k),
                names,
                &mut found,
            );
            for (code, detail) in found.drain(..) {
                let waived = cx
                    .waivers
                    .binary_search(&(inst.id[i], c.param_name[k]))
                    .is_ok();
                let (code, detail) = if waived && code.waivable() {
                    (Code::I005, format!("waived {}: {detail}", code.as_str()))
                } else {
                    (code, detail)
                };
                out.push(Violation {
                    code,
                    cell: me.cell,
                    object: inst.id[i],
                    path: Name::NONE,
                    param: c.param_name[k],
                    detail,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(v: Val, kind: ParamKind, b: [Option<f64>; 4], choices: &[&str]) -> Vec<Code> {
        let mut names = Names::default();
        let choices: Vec<Name> = choices.iter().map(|c| names.intern(c)).collect();
        let bounds = b.map(|x| x.map(Val::Num));
        let mut out = Vec::new();
        gate(&v, kind, &bounds, &choices, &names, &mut out);
        out.into_iter().map(|(c, _)| c).collect()
    }

    /// Exhaustive small domain: every value on a 0.05 lattice against min/max/grid/step.
    #[test]
    fn gating_matches_the_rules_over_a_small_domain() {
        use ParamKind::Number as N;
        for i in -10..=40 {
            let x = f64::from(i) * 0.05;
            let in_range = (0.15 - 1e-12..=1.5 + 1e-12).contains(&x);
            let on_grid = i % 2 == 0; // grid 0.1
            let mut want = Vec::new();
            if !in_range {
                want.push(Code::E005);
            }
            if !on_grid {
                want.push(Code::E032);
            }
            let got = run(
                Val::Num(x),
                N,
                [Some(0.15), Some(1.5), None, Some(0.1)],
                &[],
            );
            assert_eq!(got, want, "x = {x}");
            // step 0.25 from min 0.25: on-step iff (x - 0.25) / 0.25 is an integer.
            let on_step = (i - 5) % 5 == 0;
            let got = run(Val::Num(x), N, [Some(0.25), None, Some(0.25), None], &[]);
            let mut want = Vec::new();
            if x < 0.25 - 1e-12 {
                want.push(Code::E005);
            }
            if !on_step {
                want.push(Code::E032);
            }
            assert_eq!(got, want, "step x = {x}");
        }
        // Decimal values on a decimal grid pass despite binary rounding.
        assert!(run(Val::Num(0.3), N, [None, None, None, Some(0.1)], &[]).is_empty());
        assert!(run(Val::Num(1.15e-6), N, [None, None, None, Some(5e-9)], &[]).is_empty());
        assert_eq!(
            run(Val::Num(1.151e-6), N, [None, None, None, Some(5e-9)], &[]),
            [Code::E032]
        );
        // Ints compare like nums; strings and bools ignore numeric rules.
        assert_eq!(
            run(Val::Int(0), N, [Some(1.0), None, None, None], &[]),
            [Code::E005]
        );
        assert!(
            run(
                Val::Str("x".into()),
                N,
                [Some(1.0), Some(2.0), None, Some(0.5)],
                &[]
            )
            .is_empty()
        );
    }

    #[test]
    fn choices_compare_text_or_number() {
        let k = ParamKind::Choice;
        assert!(run(Val::Str("nfet".into()), k, [None; 4], &["nfet", "pfet"]).is_empty());
        assert_eq!(
            run(Val::Str("lvt".into()), k, [None; 4], &["nfet", "pfet"]),
            [Code::E006]
        );
        assert!(run(Val::Num(2.0), ParamKind::Number, [None; 4], &["1", "2"]).is_empty());
        assert!(run(Val::Int(2), ParamKind::Number, [None; 4], &["2.0"]).is_empty());
        assert_eq!(
            run(Val::Int(3), ParamKind::Number, [None; 4], &["1", "2"]),
            [Code::E006]
        );
    }

    #[test]
    fn codes_have_severity_by_letter_and_only_gating_is_waivable() {
        assert_eq!(Code::E005.severity(), Severity::Error);
        assert_eq!(Code::W016.severity(), Severity::Warning);
        assert_eq!(Code::I005.severity(), Severity::Info);
        let waivable: Vec<Code> = [Code::E001, Code::E005, Code::E006, Code::E032, Code::E030]
            .into_iter()
            .filter(|c| c.waivable())
            .collect();
        assert_eq!(waivable, [Code::E005, Code::E006, Code::E032]);
        assert_eq!(Code::I005.as_str(), "I005");
        assert_eq!(Code::E035.as_str(), "E035");
    }
}
