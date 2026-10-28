//! The single writer of [`UiTables`]: contributions in, composed tables out (spine line
//! `ui::apply_ui_edits`). Last contribution wins (D8); every override is a health row.
//!
//! Contents:
//! - `pub fn compose(strings: &Strings, base: &Contribution, plugins: &[(Sym, &Contribution)])
//!   -> UiTables`
//! - `pub fn patch_last(tables: &mut UiTables, old: &Contribution, new: &Contribution) -> bool`

use std::collections::HashMap;

use crate::keys::Chord;
use crate::kinds::{Kind, Prop, StyleProp};
use crate::strings::{Strings, Sym};
use crate::tables::{
    Contribution, Edit, Health, HealthKind, Keymap, NO_NODE, NO_STYLE, NodeIx, UiNodes, UiStyles,
    UiTables,
};

/// A node as the edits leave it, before the tree is built.
struct Row {
    id: Sym,
    kind: Kind,
    parent: Sym,
    order: i32,
    /// Declaration order (kept by a re-declaration): the sibling tie-break.
    seq: u32,
    owner: u8,
    live: bool,
    props: [Sym; Prop::COUNT],
}

/// One style selector as the edits leave it.
struct Selector {
    name: Sym,
    value: [u32; StyleProp::COUNT],
    /// Bit `p` set when prop `p` is set.
    set: u32,
    /// Contribution that set each prop last.
    owner: [u8; StyleProp::COUNT],
}

/// Composes `base ⊕ plugin₁ ⊕ … ⊕ pluginₙ` (last wins, D8) into fresh tables.
///
/// Every cross-contribution override, every edit of an unknown id, and every unreachable node is
/// recorded in [`UiTables::health`]; nothing is rejected silently and nothing panics on bad
/// contributions.
///
/// # Panics
///
/// Panics on more than 254 plugins or more than [`UiNodes::CAPACITY`] composed nodes.
///
/// # Time complexity
///
/// O(E + N log N) for E edits and N nodes.
pub fn compose(
    strings: &Strings,
    base: &Contribution,
    plugins: &[(Sym, &Contribution)],
) -> UiTables {
    assert!(plugins.len() < u8::MAX as usize, "too many plugins");
    let mut health = Health::default();
    let mut rows: Vec<Row> = Vec::with_capacity(base.edits.len() / 2);
    let mut by_id: HashMap<Sym, u32> = HashMap::new();
    let mut sel: Vec<Selector> = Vec::new();
    let mut keys: HashMap<Chord, (Sym, u8)> = HashMap::new();

    let all = std::iter::once((Sym::NONE, base)).chain(plugins.iter().copied());
    for (owner, (_, contribution)) in all.enumerate() {
        let owner = owner as u8;
        assert!(
            contribution.edits.len() <= Contribution::CAPACITY,
            "contribution too large"
        );
        for &edit in &contribution.edits {
            match edit {
                Edit::Node { id, kind } => {
                    declare(&mut rows, &mut by_id, &mut health, id, kind, owner)
                }
                Edit::Set { id, .. } | Edit::Parent { id, .. } | Edit::Order { id, .. } => {
                    let Some(row) = target(&mut rows, &by_id, &mut health, id, owner) else {
                        continue;
                    };
                    if row.owner != owner {
                        health.push(HealthKind::PropOverridden, id, owner, row.owner);
                    }
                    match edit {
                        Edit::Set { prop, value, .. } => row.props[prop as usize] = value,
                        Edit::Parent { parent, .. } => row.parent = parent,
                        Edit::Order { order, .. } => row.order = order,
                        _ => unreachable!("matched above"),
                    }
                }
                Edit::Remove { id } => {
                    let Some(row) = target(&mut rows, &by_id, &mut health, id, owner) else {
                        continue;
                    };
                    if row.owner != owner {
                        health.push(HealthKind::NodeRemoved, id, owner, row.owner);
                    }
                    row.live = false;
                }
                Edit::Style {
                    selector,
                    prop,
                    value,
                } => set_style(&mut sel, &mut health, selector, prop, value, owner),
                Edit::Key { chord, command } => {
                    if let Some(&(old, old_owner)) = keys.get(&chord)
                        && old_owner != owner
                    {
                        health.push(HealthKind::KeyOverridden, old, owner, old_owner);
                    }
                    keys.insert(chord, (command, owner));
                }
            }
        }
    }

    let styles = build_styles(strings, sel);
    let nodes = build_tree(&rows, &by_id, &styles, &mut health);
    let mut bindings: Vec<(Chord, Sym)> = keys.into_iter().map(|(c, (s, _))| (c, s)).collect();
    bindings.sort_unstable_by_key(|b| b.0);
    let keymap = Keymap {
        chord: bindings.iter().map(|b| b.0).collect(),
        command: bindings.iter().map(|b| b.1).collect(),
    };
    debug_assert!(keymap.chord.windows(2).all(|w| w[0] < w[1]));
    let mut owners = vec![Sym::NONE];
    owners.extend(plugins.iter().map(|p| p.0));
    UiTables {
        nodes,
        styles,
        keymap,
        health,
        owners,
    }
}

/// Brings tables that [`compose`] built with `old` as the **last** contribution up to `new`
/// without composing again, when the two differ only in the values of `Set` edits (the same
/// edits in the same order otherwise, `style` excepted: a node's class is derived from it).
/// Returns false, changing nothing, when they differ in any other way; the caller composes.
///
/// The result equals `compose` with `new` last: a prop's value is the last `Set` of it, and the
/// health rows are those of the same edits (oracle: `ui` tests, `patch_last_equals_compose`).
/// Cost: O(edits × changed props); no allocation when nothing changed.
pub fn patch_last(tables: &mut UiTables, old: &Contribution, new: &Contribution) -> bool {
    if old.edits.len() != new.edits.len() {
        return false;
    }
    let mut changed: Vec<(Sym, Prop)> = Vec::new();
    for (a, b) in old.edits.iter().zip(&new.edits) {
        if a == b {
            continue;
        }
        match (*a, *b) {
            (
                Edit::Set {
                    id: ia, prop: pa, ..
                },
                Edit::Set {
                    id: ib, prop: pb, ..
                },
            ) if ia == ib && pa == pb && pa != Prop::Style => changed.push((ia, pa)),
            _ => return false,
        }
    }
    // Each changed prop's new value: the last edit of `new` that decides it. A node declared
    // or removed after that edit would reset it: compose instead.
    let mut values: Vec<(NodeIx, Prop, Sym)> = Vec::with_capacity(changed.len());
    for &(id, prop) in &changed {
        let mut value = None;
        for e in new.edits.iter().rev() {
            match *e {
                Edit::Set {
                    id: i,
                    prop: p,
                    value: v,
                } if i == id && p == prop => {
                    value = Some(v);
                    break;
                }
                Edit::Node { id: i, .. } | Edit::Remove { id: i } if i == id => return false,
                _ => {}
            }
        }
        let value = value.expect("a changed set is in `new`");
        // A target that did not compose (unknown, removed or orphaned) shows nothing either way.
        if let Some(ix) = tables.nodes.find(id) {
            values.push((ix, prop, value));
        }
    }
    for (ix, prop, value) in values {
        tables.nodes.props[prop as usize][ix as usize] = value;
    }
    assert_eq!(tables.nodes.props[0].len(), tables.nodes.len());
    true
}

// ---- private ----

/// `node` edit: a fresh row, or a reset of the existing one (keeping its declaration order).
fn declare(
    rows: &mut Vec<Row>,
    by_id: &mut HashMap<Sym, u32>,
    health: &mut Health,
    id: Sym,
    kind: Kind,
    owner: u8,
) {
    let fresh = Row {
        id,
        kind,
        parent: Sym::NONE,
        order: 0,
        seq: rows.len() as u32,
        owner,
        live: true,
        props: [Sym::NONE; Prop::COUNT],
    };
    match by_id.get(&id) {
        Some(&r) => {
            let old = &mut rows[r as usize];
            if old.live && old.owner != owner {
                health.push(HealthKind::NodeReplaced, id, owner, old.owner);
            }
            *old = Row {
                seq: old.seq,
                ..fresh
            };
        }
        None => {
            by_id.insert(id, rows.len() as u32);
            rows.push(fresh);
        }
    }
    debug_assert_eq!(rows.len(), by_id.len());
}

/// The live row an edit of `id` targets; a missing or removed one is a health row.
fn target<'a>(
    rows: &'a mut [Row],
    by_id: &HashMap<Sym, u32>,
    health: &mut Health,
    id: Sym,
    owner: u8,
) -> Option<&'a mut Row> {
    let r = by_id.get(&id).copied().filter(|&r| rows[r as usize].live);
    if r.is_none() {
        health.push(HealthKind::UnknownTarget, id, owner, owner);
    }
    Some(&mut rows[r? as usize])
}

/// `style` edit: sets one prop of `selector`, creating the selector on first sight.
fn set_style(
    sel: &mut Vec<Selector>,
    health: &mut Health,
    selector: Sym,
    prop: StyleProp,
    value: u32,
    owner: u8,
) {
    let p = prop as usize;
    let i = match sel.iter().position(|s| s.name == selector) {
        Some(i) => i,
        None => {
            sel.push(Selector {
                name: selector,
                value: [0; StyleProp::COUNT],
                set: 0,
                owner: [0; StyleProp::COUNT],
            });
            sel.len() - 1
        }
    };
    let s = &mut sel[i];
    if s.set & (1 << p) != 0 && s.owner[p] != owner {
        health.push(HealthKind::StyleOverridden, selector, owner, s.owner[p]);
    }
    s.value[p] = value;
    s.set |= 1 << p;
    s.owner[p] = owner;
}

/// The style table, with each kind's row and the `*` row resolved once.
fn build_styles(strings: &Strings, sel: Vec<Selector>) -> UiStyles {
    assert!(sel.len() < NO_STYLE as usize, "too many style selectors");
    let mut styles = UiStyles {
        selector: sel.iter().map(|s| s.name).collect(),
        value: sel.iter().map(|s| s.value).collect(),
        set: sel.iter().map(|s| s.set).collect(),
        ..UiStyles::default()
    };
    for &k in Kind::ALL {
        styles.kind_row[k as usize] = strings.find(k.name()).map_or(NO_STYLE, |s| styles.row(s));
    }
    styles.default_row = strings.find("*").map_or(NO_STYLE, |s| styles.row(s));
    styles
}

/// Lays the live rows out in pre-order from the roots, reporting unreachable ones as orphans.
fn build_tree(
    rows: &[Row],
    by_id: &HashMap<Sym, u32>,
    styles: &UiStyles,
    health: &mut Health,
) -> UiNodes {
    const ROOT: u32 = u32::MAX;
    // (parent row or ROOT, order, seq, row) sorted gives every sibling group contiguous and ordered.
    let mut edges: Vec<(u32, i32, u32, u32)> = Vec::with_capacity(rows.len());
    for (r, row) in rows.iter().enumerate().filter(|(_, row)| row.live) {
        let parent = if row.parent.is_none() {
            ROOT
        } else {
            match by_id.get(&row.parent) {
                Some(&p) if rows[p as usize].live => p,
                _ => continue, // reported as orphan below
            }
        };
        edges.push((parent, row.order, row.seq, r as u32));
    }
    edges.sort_unstable();
    let group = |parent: u32| {
        let lo = edges.partition_point(|e| e.0 < parent);
        let hi = edges.partition_point(|e| e.0 <= parent);
        &edges[lo..hi]
    };

    let mut nodes = UiNodes::default();
    let mut visited = vec![false; rows.len()];
    // Explicit stack: (row, parent index); children pushed in reverse to pop in order.
    let mut stack: Vec<(u32, NodeIx)> = group(ROOT).iter().rev().map(|e| (e.3, NO_NODE)).collect();
    while let Some((r, parent)) = stack.pop() {
        assert!(
            nodes.id.len() < UiNodes::CAPACITY,
            "ui node capacity exceeded"
        );
        let row = &rows[r as usize];
        let ix = nodes.id.len() as NodeIx;
        visited[r as usize] = true;
        nodes.id.push(row.id);
        nodes.kind.push(row.kind);
        nodes.parent.push(parent);
        nodes.end.push(ix + 1);
        nodes.owner.push(row.owner);
        let class = row.props[Prop::Style as usize];
        nodes.class.push(if class.is_none() {
            NO_STYLE
        } else {
            styles.row(class)
        });
        for (col, &v) in nodes.props.iter_mut().zip(&row.props) {
            col.push(v);
        }
        nodes.index.insert(row.id, ix);
        stack.extend(group(r).iter().rev().map(|e| (e.3, ix)));
    }
    // Pre-order: every descendant follows its parent, so one reverse pass propagates subtree ends.
    for i in (0..nodes.len()).rev() {
        let p = nodes.parent[i];
        if p != NO_NODE {
            nodes.end[p as usize] = nodes.end[p as usize].max(nodes.end[i]);
        }
    }
    // Unreached rows are orphans, except descendants of a removed node: those went with it.
    for (r, row) in rows.iter().enumerate() {
        if row.live && !visited[r] && !removed_ancestor(rows, by_id, row) {
            health.push(HealthKind::Orphaned, row.id, row.owner, row.owner);
        }
    }
    debug_assert!(
        nodes
            .parent
            .iter()
            .enumerate()
            .all(|(i, &p)| p == NO_NODE || (p as usize) < i)
    );
    nodes
}

/// True when some ancestor of `row` (by parent id) was removed. Bounded by the row count, so a
/// parent cycle ends.
fn removed_ancestor(rows: &[Row], by_id: &HashMap<Sym, u32>, row: &Row) -> bool {
    let mut p = row.parent;
    for _ in 0..rows.len() {
        match by_id.get(&p) {
            Some(&q) if !rows[q as usize].live => return true,
            Some(&q) => p = rows[q as usize].parent,
            None => return false,
        }
    }
    false
}
