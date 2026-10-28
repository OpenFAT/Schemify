//! One cell's evaluated graph, documents and waivers in, its value and instance findings
//! out (netlist spec §8): D13 gating on evaluated values with waivers (`E005`, `E006`,
//! `E032`, `E038`, `I005`), expression errors (`E008`), override checks (`E001`, `W007`,
//! `E030`) and stale Virtuoso-derived values (`E039`, D20 as amended 2026-10-15).
//! Connectivity findings come from `connect`; hierarchy findings (`E013`, `E014`, `E031`,
//! `E033`–`E036`, `W037`) from `elaborate`.
//!
//! Contents:
//! - `pub(crate) struct CellCtx`
//! - `pub(crate) const INST_CODES`
//! - `pub(crate) fn waivers(doc: &Document, names: &Names) -> Vec<(ObjId, Name)>`
//! - `pub(crate) fn gate(v, bounds, choices, names, out)`
//! - `pub(crate) fn passes(x: f64, b: [Option<f64>; 4]) -> bool`
//! - `pub(crate) fn check_overrides(cx, unresolved, names, out)`
//! - `pub(crate) fn inst_overrides(cx, i, unresolved, names, out)`
//! - `pub(crate) fn check_values(cx, vals, names, funcs, ws, out)`
//! - `pub(crate) fn value_errors(cx, vals, nodes, out)`
//! - `pub(crate) fn inst_rules(cx, i, vals, names, funcs, ws, out)`
//! - `pub(crate) fn inputs_digest(c: &Iface, vals: &[Val], names: &Names) -> Option<String>`

use crate::eval::{BoundOf, Work};
use crate::graph::{self, Graph, OWN};
use crate::iface::{BOUNDS, GRID, Iface, MAX, MIN, STEP, VALID};
use crate::names::{self, Name, Names};
use crate::values::{self, Code as PCode, Funcs, NONE, Val};
use crate::violations::{Code, Violation};
use schemify_doc::{Document, Instances, ObjId};
use std::cell::OnceCell;
use std::mem;

/// What one evaluation context of a cell is checked against.
pub(crate) struct CellCtx<'cell> {
    pub(crate) doc: &'cell Document,
    pub(crate) ifaces: &'cell [Iface],
    pub(crate) own: u32,
    pub(crate) inst_iface: &'cell [u32],
    pub(crate) graph: &'cell Graph,
    pub(crate) waivers: &'cell [(ObjId, Name)],
}

/// The codes a cell state's per-instance checks write ([`inst_overrides`],
/// [`value_errors`], [`inst_rules`]), each once: in a cell state's table every row of these
/// codes comes from them, keyed by its instance (own-scope `E008` rows by no object).
pub(crate) const INST_CODES: [Code; 10] = [
    Code::E001,
    Code::W007,
    Code::E030,
    Code::E008,
    Code::E005,
    Code::E006,
    Code::E032,
    Code::E038,
    Code::E039,
    Code::I005,
];

/// Waivers of one document: `(instance id, param)` pairs, sorted. A waiver naming a param
/// no interface has (its name not in `names`) can match no finding (only gating codes on
/// interface params are waivable) and is left out.
pub(crate) fn waivers(doc: &Document, names: &Names) -> Vec<(ObjId, Name)> {
    let s = doc.strings();
    let w = doc.waivers();
    let mut out: Vec<(ObjId, Name)> = (0..w.kind.len())
        .filter_map(|r| {
            let inst = ObjId::find(s.get(w.fields[r][1]), s)?;
            Some((inst, names.find(s.get(w.fields[r][2]))?))
        })
        .collect();
    out.sort_unstable();
    out
}

/// Gating (D13) of one evaluated param value. Appends `(code, detail)` for each
/// violated rule; `bounds` are the evaluated `min`, `max`, `step`, `grid`.
pub(crate) fn gate(
    v: &Val,
    bounds: &[Option<Val>; BOUNDS],
    choices: &[Name],
    names: &Names,
    out: &mut Vec<(Code, String)>,
) {
    let before = out.len();
    // The value's text goes only into a finding or a choice compare: written on first use
    // (a flat 1M-instance netlist gates 10^6 passing values).
    let cell = OnceCell::new();
    let text = || {
        cell.get_or_init(|| {
            let mut t = String::new();
            if !v.is_err() {
                v.write_text(&mut t);
            }
            t
        })
        .as_str()
    };
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
            out.push((
                Code::E005,
                format!("{} is below min {}", text(), num_text(m)),
            ));
        }
        if let Some(m) = above {
            out.push((
                Code::E005,
                format!("{} is above max {}", text(), num_text(m)),
            ));
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
                out.push((
                    Code::E032,
                    format!("{} is off grid {}", text(), num_text(g)),
                ));
            }
        } else if let Some(st) = step {
            let base = bounds[MIN].as_ref().and_then(Val::as_f64).unwrap_or(0.0);
            if !on_grid(x, base, st) {
                out.push((
                    Code::E032,
                    format!(
                        "{} is off step {} from {}",
                        text(),
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
            c == text()
                || matches!((schemify_expr::parse_num(c), v.as_f64()), (Ok(a), Some(b)) if a == b)
        });
        if !hit {
            out.push((Code::E006, format!("{} is not one of the choices", text())));
        }
    }
    // The param's legality rule (`valid`, D13 as amended 2026-10-15): a rule that does not
    // hold, or that is not a bool, is a finding; one that could not be evaluated is already
    // an E008.
    match &bounds[VALID] {
        None | Some(Val::Bool(true)) => {}
        Some(Val::Bool(false)) => out.push((
            Code::E038,
            format!("{} is not a legal value (its valid rule is false)", text()),
        )),
        Some(other) => {
            let mut t = String::new();
            other.write_text(&mut t);
            out.push((Code::E038, format!("its valid rule gives {t}, not a bool")));
        }
    }
    // min, max, one of grid/step, choices, valid: at most five findings.
    assert!(
        out.len() - before <= 5,
        "gate appended {} findings",
        out.len() - before
    );
    debug_assert!(
        out[before..]
            .iter()
            .all(|(c, _)| matches!(c, Code::E005 | Code::E006 | Code::E032 | Code::E038)),
        "gate writes gating codes only"
    );
}

/// Whether [`gate`] finds nothing for a number `x` against numeric bounds `b` (`MIN`,
/// `MAX`, `STEP`, `GRID`; `None` absent) and no choices: the per-instance fast path, with
/// no bound copied and no text written.
pub(crate) fn passes(x: f64, b: [Option<f64>; 4]) -> bool {
    debug_assert!(x.is_finite(), "a gated number is finite");
    let grid = b[GRID].filter(|g| *g != 0.0);
    let step = b[STEP].filter(|s| *s != 0.0);
    let base = b[MIN].unwrap_or(0.0);
    b[MIN].is_none_or(|m| x >= m)
        && b[MAX].is_none_or(|m| x <= m)
        && match (grid, step) {
            (Some(g), _) => on_grid(x, 0.0, g),
            (None, Some(st)) => on_grid(x, base, st),
            (None, None) => true,
        }
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
    assert_eq!(unresolved.len(), cx.doc.instances().id.len());
    assert_eq!(
        cx.inst_iface.len(),
        unresolved.len(),
        "one interface slot per instance"
    );
    for (i, why) in unresolved.iter().enumerate() {
        inst_overrides(cx, i, why.as_deref(), names, out);
    }
}

/// [`check_overrides`] of instance row `i` alone (`unresolved`: why its cell did not
/// resolve). Its rows carry the instance's id, so this is what a patch of one instance's
/// findings recomputes.
pub(crate) fn inst_overrides(
    cx: &CellCtx<'_>,
    i: usize,
    unresolved: Option<&str>,
    names: &mut Names,
    out: &mut Vec<Violation>,
) {
    let doc = cx.doc;
    let s = doc.strings();
    let inst = doc.instances();
    let cell = cx.ifaces[cx.own as usize].cell;
    assert!(i < inst.id.len(), "instance row {i} in range");
    if let Some(why) = unresolved {
        assert!(
            cx.inst_iface[i] == NONE,
            "an unresolved instance has no interface"
        );
        out.push(Violation::new(
            Code::E001,
            cell,
            inst.id[i],
            Name::NONE,
            why.to_owned(),
        ));
        return;
    }
    let Some(c) = cx.ifaces.get(cx.inst_iface[i] as usize) else {
        return;
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
        out.push(Violation::new(code, cell, inst.id[i], name, detail));
    }
}

/// Value-dependent checks of every node: `E008` (evaluation errors) and gating (`E005`
/// min/max, `E006` choices, `E032` step/grid, D13), from the evaluated `vals` (indexed
/// like `cx.graph`). Waived gating findings become `I005`.
pub(crate) fn check_values<F: Funcs>(
    cx: &CellCtx<'_>,
    vals: &[Val],
    names: &Names,
    funcs: &mut F,
    ws: &mut Work,
    out: &mut Vec<Violation>,
) {
    let g = cx.graph;
    assert_eq!(vals.len(), g.len());
    assert_eq!(cx.inst_iface.len(), cx.doc.instances().id.len());
    value_errors(cx, vals, 0..g.len() as u32, out);
    for i in 0..cx.doc.instances().id.len() {
        inst_rules(cx, i, vals, names, funcs, ws, out);
    }
}

/// The `E008` rows of `nodes` (of `cx.graph`, ascending: rows of one key keep the full
/// pass's order).
pub(crate) fn value_errors(
    cx: &CellCtx<'_>,
    vals: &[Val],
    nodes: impl Iterator<Item = u32>,
    out: &mut Vec<Violation>,
) {
    let g = cx.graph;
    let me = &cx.ifaces[cx.own as usize];
    let inst = cx.doc.instances();
    assert_eq!(vals.len(), g.len());
    let before = out.len();
    for n in nodes {
        let Val::Err(e) = &vals[n as usize] else {
            continue;
        };
        let owner = g.owner[n as usize];
        let child_scope = owner != NONE && g.table[n as usize] != OWN;
        if child_scope && matches!(g.code(n, cx.ifaces), PCode::Bad(_)) {
            continue;
        }
        let param = g.node_param(n, cx.ifaces);
        let object = if owner == NONE {
            ObjId::NONE
        } else {
            inst.id[owner as usize]
        };
        let detail = format!("{}: {}", e.code.as_str(), e.msg);
        out.push(Violation::new(Code::E008, me.cell, object, param, detail));
    }
    debug_assert!(out[before..].iter().all(|v| v.code == Code::E008));
}

/// Gating (`E005`, `E006`, `E032`, `E038`, waived `I005`) and stale derived values (`E039`)
/// of instance row `i`: a function of that instance's nodes, its attributes and the
/// waivers only.
pub(crate) fn inst_rules<F: Funcs>(
    cx: &CellCtx<'_>,
    i: usize,
    vals: &[Val],
    names: &Names,
    funcs: &mut F,
    ws: &mut Work,
    out: &mut Vec<Violation>,
) {
    let g = cx.graph;
    let me = &cx.ifaces[cx.own as usize];
    let inst = cx.doc.instances();
    assert!(i < inst.id.len(), "instance row {i} in range");
    assert_eq!(vals.len(), g.len(), "values indexed like the graph");
    let Some(c) = cx.ifaces.get(cx.inst_iface[i] as usize) else {
        return;
    };
    let first = g.inst_first[i];
    assert!(first != NONE, "a resolved instance has a node block");
    if !c.vinputs.is_empty() && c.param_vderived.iter().any(|d| *d) {
        stale_derived(cx, c, i, &vals[first as usize..], names, out);
    }
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
        // Fast path: constant bounds and no choices (PDK `min`/`max`/`grid`), a value
        // that passes them has nothing to report.
        let konst = |w| c.bound_konst(k, w);
        let fast = c.choice_start[k] == c.choice_start[k + 1]
            && (0..BOUNDS).all(|w| c.param_bound[k][w] == NONE || konst(w).is_some())
            && konst(VALID).is_none_or(|v| matches!(v, Val::Bool(true)));
        if fast {
            let b = |w: usize| konst(w).and_then(Val::as_f64);
            if v.as_f64()
                .is_none_or(|x| passes(x, [b(0), b(1), b(2), b(3)]))
            {
                continue;
            }
        }
        let mut bounds: [Option<Val>; BOUNDS] = Default::default();
        for (which, b) in bounds.iter_mut().enumerate() {
            let at = BoundOf {
                inst: i,
                param: k,
                which,
            };
            *b = g.bound(at, vals, cx.ifaces, funcs, ws);
        }
        let mut found = mem::take(&mut ws.gated);
        found.clear();
        gate(v, &bounds, c.choices_of(k), names, &mut found);
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
            out.push(Violation::new(
                code,
                me.cell,
                inst.id[i],
                c.param_name[k],
                detail,
            ));
        }
        ws.gated = found;
    }
}

/// The digest of an instance's `vinputs` values (D20 as amended 2026-10-15): FNV-1a 64 of
/// one `<name>=<cached text>` line per input, in `vinputs` order, as 16 lowercase hex
/// digits; `None` when an input is not a param of the cell or has no value.
pub(crate) fn inputs_digest(c: &Iface, vals: &[Val], names: &Names) -> Option<String> {
    assert!(
        vals.len() >= c.param_name.len(),
        "one value per param of the cell"
    );
    let mut text = String::new();
    for &n in &c.vinputs {
        let v = &vals[c.param(n)?];
        if v.is_err() || matches!(v, Val::Cnum(..)) {
            return None;
        }
        text.push_str(names.get(n));
        text.push('=');
        values::cached_text(v, &mut text);
        text.push('\n');
    }
    let digest = format!("{:016x}", names::fnv1a64(text.as_bytes()));
    debug_assert_eq!(digest.len(), 16, "a digest is 16 hex digits");
    Some(digest)
}

// ---- private ----

/// Whether `x` is `base + k·step` for an integer `k`, to a relative 1e-9 (values are
/// f64 from decimal text, so `0.3` on a `0.1` grid must pass).
fn on_grid(x: f64, base: f64, step: f64) -> bool {
    assert!(step != 0.0, "callers drop a zero grid or step");
    debug_assert!(
        step.is_finite() && base.is_finite(),
        "bounds are finite numbers"
    );
    let r = (x - base) / step;
    (r - r.round()).abs() <= 1e-9 * r.abs().max(1.0)
}

fn num_text(x: f64) -> String {
    let mut s = String::new();
    schemify_expr::write_num(&mut s, x);
    debug_assert!(!s.is_empty());
    s
}

/// `E039` for each `vderived` param of instance row `i` (its param values `vals`, in the
/// cell's param order) whose `cached` value was derived from other input values than the
/// instance has now, or that has no cached value while an input is overridden; `I005`
/// when waived. A param the instance overrides itself is the user's value: never stale.
fn stale_derived(
    cx: &CellCtx<'_>,
    c: &Iface,
    i: usize,
    vals: &[Val],
    names: &Names,
    out: &mut Vec<Violation>,
) {
    assert!(i < cx.doc.instances().id.len(), "instance row {i} in range");
    assert_eq!(
        c.param_vderived.len(),
        c.param_name.len(),
        "param columns in step"
    );
    let Some(now) = inputs_digest(c, vals, names) else {
        return; // an input failed: its own E008 says so
    };
    let doc = cx.doc;
    let set = c
        .vinputs
        .iter()
        .any(|&n| graph::cached_of(doc, i, names.get(n)).own_override);
    let derived = (0..c.param_name.len()).filter(|&k| c.param_vderived[k]);
    for k in derived {
        let found = graph::cached_of(doc, i, names.get(c.param_name[k]));
        let detail = match found.digest {
            _ if found.own_override => continue,
            Some(digest) if digest == now => continue,
            Some(_) => "its inputs changed since Virtuoso derived it",
            None if set => "an input is set, and Virtuoso never derived it for these values",
            None => continue,
        };
        let id = doc.instances().id[i];
        let waived = cx.waivers.binary_search(&(id, c.param_name[k])).is_ok();
        let (code, detail) = if waived {
            (Code::I005, format!("waived E039: {detail}"))
        } else {
            (Code::E039, detail.to_owned())
        };
        out.push(Violation::new(
            code,
            cx.ifaces[cx.own as usize].cell,
            id,
            c.param_name[k],
            detail,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(v: Val, b: [Option<f64>; 4], choices: &[&str]) -> Vec<Code> {
        let mut names = Names::default();
        let choices: Vec<Name> = choices.iter().map(|c| names.intern(c)).collect();
        let [b0, b1, b2, b3] = b.map(|x| x.map(Val::Num));
        let bounds = [b0, b1, b2, b3, None];
        let mut out = Vec::new();
        gate(&v, &bounds, &choices, &names, &mut out);
        // The fast path agrees: it passes exactly when gate finds nothing.
        if let (true, Some(x)) = (choices.is_empty(), v.as_f64()) {
            assert_eq!(passes(x, b), out.is_empty(), "{v:?} {b:?}");
        }
        out.into_iter().map(|(c, _)| c).collect()
    }

    /// Exhaustive small domain: every value on a 0.05 lattice against min/max/grid/step.
    #[test]
    fn gating_matches_the_rules_over_a_small_domain() {
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
            let got = run(Val::Num(x), [Some(0.15), Some(1.5), None, Some(0.1)], &[]);
            assert_eq!(got, want, "x = {x}");
            // step 0.25 from min 0.25: on-step iff (x - 0.25) / 0.25 is an integer.
            let on_step = (i - 5) % 5 == 0;
            let got = run(Val::Num(x), [Some(0.25), None, Some(0.25), None], &[]);
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
        assert!(run(Val::Num(0.3), [None, None, None, Some(0.1)], &[]).is_empty());
        assert!(run(Val::Num(1.15e-6), [None, None, None, Some(5e-9)], &[]).is_empty());
        assert_eq!(
            run(Val::Num(1.151e-6), [None, None, None, Some(5e-9)], &[]),
            [Code::E032]
        );
        // Ints compare like nums; strings and bools ignore numeric rules.
        assert_eq!(
            run(Val::Int(0), [Some(1.0), None, None, None], &[]),
            [Code::E005]
        );
        assert!(
            run(
                Val::Str("x".into()),
                [Some(1.0), Some(2.0), None, Some(0.5)],
                &[]
            )
            .is_empty()
        );
    }

    #[test]
    fn choices_compare_text_or_number() {
        assert!(run(Val::Str("nfet".into()), [None; 4], &["nfet", "pfet"]).is_empty());
        assert_eq!(
            run(Val::Str("lvt".into()), [None; 4], &["nfet", "pfet"]),
            [Code::E006]
        );
        assert!(run(Val::Num(2.0), [None; 4], &["1", "2"]).is_empty());
        assert!(run(Val::Int(2), [None; 4], &["2.0"]).is_empty());
        assert_eq!(run(Val::Int(3), [None; 4], &["1", "2"]), [Code::E006]);
    }
}
