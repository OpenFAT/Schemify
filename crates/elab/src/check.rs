//! Checks (netlist spec §8): the code table, the [`Violations`] table, D13 gating on
//! evaluated values with waivers, and the per-cell checks over instances and the
//! expression graph. Connectivity findings come from `connect`; hierarchy findings
//! (`E013`, `E014`, `E031`, `E033`–`E036`, `W037`) from `elaborate`.

use crate::graph::{Graph, Work};
use crate::iface::{GRID, Iface, MAX, MIN, STEP};
use crate::names::{Name, Names};
use crate::values::{Code as PCode, Funcs, NONE, Val};
use schemify_doc::{Document, Instances, ObjId};
use std::cmp::Ordering;

/// Check codes (netlist spec §8; `E035`, `E036`, `W037` are elab amendments).
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
    E036,
    W037,
    E038,
    E039,
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
        const S: [&str; 26] = [
            "E001", "E002", "E003", "W004", "E005", "E006", "W007", "E008", "E009", "W010", "E011",
            "E012", "E013", "E014", "W016", "E030", "E031", "E032", "E033", "E034", "E035", "E036",
            "W037", "E038", "E039", "I005",
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
        matches!(
            self,
            Code::E005 | Code::E006 | Code::E032 | Code::E038 | Code::E039
        )
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
            Code::E034 => "undefined ${NAME} in code, body or corner path",
            Code::E035 => "unknown corner or config",
            Code::E036 => "inherited pins of one name with different default nets",
            Code::W037 => "config inst rule names a path no instance has",
            Code::E038 => "param value fails its valid rule",
            Code::E039 => "Virtuoso-derived value is stale",
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

impl Violation {
    /// The order [`Violations::rows`] are sorted in (unique per table): code text, cell,
    /// object, path, param (texts through `names`, the arena both rows' names are from).
    pub fn key_cmp(&self, other: &Violation, names: &Names) -> Ordering {
        cmp_key(self, other, names)
    }
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
        // Room for a patch's insertions: the first one after a build would otherwise
        // reallocate the whole table in an edit frame.
        rows.reserve(rows.len() / 8 + 64);
        debug_assert!(
            rows.windows(2)
                .all(|w| cmp_key(&w[0], &w[1], names) == Ordering::Less)
        );
        Violations { rows }
    }

    /// Replaces the value-dependent rows (`E008`, `E005`, `E006`, `E032`, `I005`) with
    /// `fresh` (sorted, value codes only), keeping order, and returns the replaced rows
    /// (sorted). Cost: O(rows), no re-sort.
    pub(crate) fn replace_values(&mut self, fresh: Violations, names: &Names) -> Violations {
        assert!(
            fresh.rows.iter().all(|v| value_code(v.code)),
            "only value rows are replaced"
        );
        let (replaced, old): (Vec<Violation>, Vec<Violation>) = std::mem::take(&mut self.rows)
            .into_iter()
            .partition(|v| value_code(v.code));
        let mut out = Vec::with_capacity(old.len() + fresh.rows.len());
        let (mut a, mut b) = (
            old.into_iter().peekable(),
            fresh.rows.into_iter().peekable(),
        );
        while let (Some(x), Some(y)) = (a.peek(), b.peek()) {
            let next = if cmp_key(x, y, names) == Ordering::Greater {
                b.next()
            } else {
                a.next()
            };
            out.extend(next);
        }
        out.extend(a);
        out.extend(b);
        out.truncate(VIOLATIONS_MAX);
        debug_assert!(
            out.windows(2)
                .all(|w| cmp_key(&w[0], &w[1], names) == Ordering::Less)
        );
        self.rows = out;
        Violations { rows: replaced }
    }

    /// What changed from `old` to `self` (both from the same names arena): `(gone, new)`,
    /// the rows only `old` has and the rows only `self` has, each in key order. A row whose
    /// key stayed but whose detail changed is in both.
    ///
    /// Cost: one merge pass, O(rows) row compares; key comparisons (string compares through
    /// `names`) only where the two lists differ.
    pub fn diff<'a>(
        &'a self,
        old: &'a Violations,
        names: &Names,
    ) -> (Vec<&'a Violation>, Vec<&'a Violation>) {
        let (mut gone, mut new) = (Vec::new(), Vec::new());
        let (a, b) = (&old.rows, &self.rows);
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            if a[i] == b[j] {
                (i, j) = (i + 1, j + 1);
                continue;
            }
            match cmp_key(&a[i], &b[j], names) {
                Ordering::Less => {
                    gone.push(&a[i]);
                    i += 1;
                }
                Ordering::Greater => {
                    new.push(&b[j]);
                    j += 1;
                }
                Ordering::Equal => {
                    gone.push(&a[i]);
                    new.push(&b[j]);
                    (i, j) = (i + 1, j + 1);
                }
            }
        }
        gone.extend(&a[i..]);
        new.extend(&b[j..]);
        assert!(gone.len() <= a.len() && new.len() <= b.len());
        (gone, new)
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

/// Whether rows of `code` depend on evaluated values (what a value-only recheck replaces).
pub(crate) fn value_code(code: Code) -> bool {
    matches!(
        code,
        Code::E008 | Code::E005 | Code::E006 | Code::E032 | Code::E038 | Code::E039 | Code::I005
    )
}

/// Orders violations by their key (code text, cell, object, path, param).
pub(crate) fn cmp_key(a: &Violation, b: &Violation, names: &Names) -> Ordering {
    let code = if a.code == b.code {
        Ordering::Equal
    } else {
        a.code.as_str().cmp(b.code.as_str())
    };
    code.then_with(|| names.cmp(a.cell, b.cell))
        .then(a.object.cmp(&b.object))
        .then_with(|| names.cmp(a.path, b.path))
        .then_with(|| names.cmp(a.param, b.param))
}

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
    bounds: &[Option<Val>; crate::iface::BOUNDS],
    choices: &[Name],
    names: &Names,
    out: &mut Vec<(Code, String)>,
) {
    // The value's text goes only into a finding or a choice compare: written on first use
    // (a flat 1M-instance netlist gates 10^6 passing values).
    let cell = std::cell::OnceCell::new();
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
    match &bounds[crate::iface::VALID] {
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
}

/// Whether [`gate`] finds nothing for a number `x` against numeric bounds `b` (`MIN`,
/// `MAX`, `STEP`, `GRID`; `None` absent) and no choices: the per-instance fast path, with
/// no bound copied and no text written.
pub(crate) fn passes(x: f64, b: [Option<f64>; 4]) -> bool {
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
            let konst = &c.bound_const[k];
            let fast = c.choice_start[k] == c.choice_start[k + 1]
                && (0..crate::iface::BOUNDS)
                    .all(|w| c.param_bound[k][w] == NONE || konst[w].is_some())
                && konst[crate::iface::VALID]
                    .as_ref()
                    .is_none_or(|v| matches!(v, Val::Bool(true)));
            if fast {
                let b = |w: usize| konst[w].as_ref().and_then(Val::as_f64);
                if v.as_f64()
                    .is_none_or(|x| passes(x, [b(0), b(1), b(2), b(3)]))
                {
                    continue;
                }
            }
            let mut bounds: [Option<Val>; crate::iface::BOUNDS] = Default::default();
            for (which, b) in bounds.iter_mut().enumerate() {
                *b = g.bound(i, k, which, vals, cx.ifaces, funcs, ws);
            }
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

/// The digest of an instance's `vinputs` values (D20 as amended 2026-10-15): FNV-1a 64 of
/// one `<name>=<cached text>` line per input, in `vinputs` order, as 16 lowercase hex
/// digits; `None` when an input is not a param of the cell or has no value.
pub(crate) fn inputs_digest(c: &Iface, vals: &[Val], names: &Names) -> Option<String> {
    let mut text = String::new();
    for &n in &c.vinputs {
        let v = &vals[c.param(n)?];
        if v.is_err() || matches!(v, Val::Cnum(..)) {
            return None;
        }
        text.push_str(names.get(n));
        text.push('=');
        crate::values::cached_text(v, &mut text);
        text.push('\n');
    }
    Some(format!("{:016x}", crate::names::fnv1a64(text.as_bytes())))
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
    let Some(now) = inputs_digest(c, vals, names) else {
        return; // an input failed: its own E008 says so
    };
    let doc = cx.doc;
    let set = c
        .vinputs
        .iter()
        .any(|&n| crate::graph::cached_of(doc, i, names.get(n)).1);
    for k in (0..c.param_name.len()).filter(|&k| c.param_vderived[k]) {
        let (cached, own) = crate::graph::cached_of(doc, i, names.get(c.param_name[k]));
        let detail = match cached {
            _ if own => continue,
            Some((_, d)) if d == now => continue,
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
        out.push(Violation {
            code,
            cell: cx.ifaces[cx.own as usize].cell,
            object: id,
            path: Name::NONE,
            param: c.param_name[k],
            detail,
        });
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
        assert!(Code::E039.waivable() && Code::E038.waivable());
        assert_eq!(Code::I005.as_str(), "I005");
        assert_eq!(Code::E035.as_str(), "E035");
    }
}
