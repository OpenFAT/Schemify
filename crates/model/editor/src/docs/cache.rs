//! The D20 cache write-back: plugin-computed param and var values copied into `cached`
//! attributes, and the rule that drops a cache whose expression changed.

use super::var_value;
use schemify_doc::{Document, Op, RecordKind, Records, Strings, Sym, Target};
use schemify_elab::{CellState, Val};
use schemify_expr::{Mode, Program, Template};

/// Ops writing `cached <value> <hash>` on every own param (its default, or its `computed`
/// expression) and own var whose expression calls plugin functions and whose current value a
/// loaded plugin computed (not unverified, not an error, not complex), when the stored pair
/// differs. `<value>` is [`schemify_elab::cached_text`] (a string quoted). `hash` maps the
/// expression's sorted, distinct `ns.f` names to the content hash of the plugins serving them
/// (`None`: one is missing).
///
/// Cost: one compile per param and var expression.
pub fn cache_ops(
    doc: &Document,
    cell: &CellState,
    hash: impl Fn(&[String]) -> Option<String>,
) -> Vec<Op> {
    let s = doc.strings();
    let mut ops = Vec::new();
    let p = doc.params();
    for r in 0..p.kind.len() {
        let name = s.get(p.fields[r][0]);
        let computed = doc.pool().find_attr(s, p.attrs[r], "computed");
        let expr = computed
            .and_then(|v| v.first())
            .map_or(p.fields[r][1], |v| *v);
        let stored = doc.pool().find_attr(s, p.attrs[r], "cached");
        let now = param_value(cell, name, r);
        cache_op(
            &mut ops,
            (RecordKind::Param, name),
            s.get(expr),
            now,
            stored,
            s,
            &hash,
        );
    }
    let b = doc.doc_blocks();
    let mut v = 0usize;
    for r in 0..b.kind.len() {
        if b.kind[r] != RecordKind::Var {
            continue;
        }
        let name = s.get(b.fields[r][0]);
        let stored = doc.pool().find_attr(s, b.attrs[r], "cached");
        let now = var_value(cell, name, v);
        cache_op(
            &mut ops,
            (RecordKind::Var, name),
            s.get(b.fields[r][1]),
            now,
            stored,
            s,
            &hash,
        );
        v += 1;
    }
    debug_assert!(ops.len() <= p.kind.len() + v);
    ops
}

/// The `cached` write of one record (see [`cache_ops`]), when it is due.
pub(super) fn cache_op(
    ops: &mut Vec<Op>,
    (kind, name): (RecordKind, &str),
    expr: &str,
    now: Option<(&Val, bool)>,
    stored: Option<&[Sym]>,
    s: &Strings,
    hash: &impl Fn(&[String]) -> Option<String>,
) {
    let fns = plugin_fns(expr);
    let Some((val, unverified)) = now.filter(|_| !fns.is_empty()) else {
        return;
    };
    let Some(h) = hash(&fns) else { return };
    if unverified || val.is_err() || matches!(val, Val::Cnum(..)) {
        return;
    }
    let mut text = String::new();
    schemify_elab::cached_text(val, &mut text);
    let same = stored.is_some_and(|c| c.len() == 2 && s.get(c[0]) == text && s.get(c[1]) == h);
    if !same {
        ops.push(Op::SetAttr {
            target: Target::new(kind, name),
            key: "cached".into(),
            values: vec![text, h],
        });
    }
}

/// Sorted, distinct `ns.f` plugin functions an expression (or a string param's template)
/// calls; empty when it calls none or does not compile.
pub(super) fn plugin_fns(text: &str) -> Vec<String> {
    let names = |p: &Program| -> Vec<String> {
        let mut v: Vec<String> = p.plugin_fns().map(|(n, f)| format!("{n}.{f}")).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    match Program::compile(text, Mode::Scalar) {
        Ok(p) => names(&p),
        Err(_) => Template::parse(text).map_or_else(|_| Vec::new(), |t| names(t.program())),
    }
}

/// Value and D20 flag of own param row `r` (`name` guards against a stale state): own params
/// are the graph's first nodes, in row order.
pub fn param_value<'c>(cell: &'c CellState, name: &str, r: usize) -> Option<(&'c Val, bool)> {
    let n = *cell.ifaces.first()?.param_name.get(r)?;
    if cell.names.get(n) != name {
        return None;
    }
    let g = &cell.graph;
    debug_assert!(r < g.params as usize);
    Some((g.vals.get(r)?, *g.unverified.get(r)?))
}

/// Adds to `ops` a `remove_attr <kind> <name> cached` for every var or param whose expression
/// they change (a var's expression, a param's default or `computed`) while leaving its D20
/// cache alone: a cache belongs to the expression it was computed from (docs-page §6). A
/// `put` of such a record has the cache stripped from the replacement instead.
///
/// Cost: O(ops · touched records).
pub fn drop_stale_caches(doc: &Document, ops: &mut Vec<Op>) {
    let s = doc.strings();
    let n = ops.len();
    let touches_cache = |ops: &[Op], t: &Target| {
        ops.iter().any(|op| match op {
            Op::SetAttr { target, key, .. } | Op::RemoveAttr { target, key, .. } => {
                key == "cached" && target == t
            }
            _ => false,
        })
    };
    let mut stale: Vec<Target> = Vec::new();
    for op in ops.iter_mut().take(n) {
        let changes_expr = match op {
            Op::SetField { target, index, .. } => *index == 1 && cacheable(target.kind),
            Op::SetAttr { target, key, .. } | Op::RemoveAttr { target, key, .. } => {
                target.kind == RecordKind::Param && key == "computed"
            }
            Op::Put { target, rec } if cacheable(target.kind) => {
                let Some(cur) = named(doc, target.kind, &target.key) else {
                    continue;
                };
                let (t, r) = cur;
                let before = (s.get(t.fields[r][1]), first_attr(doc, t, r, "computed"));
                let computed = rec.attrs.iter().find(|a| a.key == "computed");
                let after = (
                    rec.fields.get(1).map(String::as_str),
                    computed.and_then(|a| a.values.first()).map(String::as_str),
                );
                if after != (Some(before.0), before.1) {
                    rec.attrs.retain(|a| a.key != "cached");
                }
                false
            }
            _ => false,
        };
        let target = match op {
            Op::SetField { target, .. }
            | Op::SetAttr { target, .. }
            | Op::RemoveAttr { target, .. } => target,
            _ => continue,
        };
        if changes_expr && !stale.contains(target) {
            stale.push(target.clone());
        }
    }
    for t in stale {
        let has = named(doc, t.kind, &t.key)
            .is_some_and(|(tab, r)| doc.pool().find_attr(s, tab.attrs[r], "cached").is_some());
        if has && !touches_cache(&ops[..], &t) {
            ops.push(Op::RemoveAttr {
                target: t,
                key: "cached".into(),
                first: None,
            });
        }
    }
    debug_assert!(ops.len() >= n);
}

pub(super) fn cacheable(kind: RecordKind) -> bool {
    matches!(kind, RecordKind::Var | RecordKind::Param)
}

/// The table and row of the var or param named `name`.
pub(super) fn named<'d>(
    doc: &'d Document,
    kind: RecordKind,
    name: &str,
) -> Option<(&'d Records, usize)> {
    let t = match kind {
        RecordKind::Param => doc.params(),
        RecordKind::Var => doc.doc_blocks(),
        _ => return None,
    };
    let s = doc.strings();
    (0..t.kind.len())
        .find(|&r| t.kind[r] == kind && s.get(t.fields[r][0]) == name)
        .map(|r| (t, r))
}

pub(super) fn first_attr<'d>(
    doc: &'d Document,
    t: &Records,
    r: usize,
    key: &str,
) -> Option<&'d str> {
    let s = doc.strings();
    let v = doc.pool().find_attr(s, t.attrs[r], key)?;
    v.first().map(|v| s.get(*v))
}
