//! Contribution composition (§1, D8): `plugin₁ ⊕ … ⊕ pluginₙ` per (kind, scope), ordered,
//! last wins, every override recorded.

use std::collections::BTreeMap;

use schemify_abi::Kind;

use crate::PluginId;
use crate::intake::{Contributions, function_row, lex, set_records};

/// One record that survived composition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposedRecord {
    /// Contribution kind.
    pub kind: Kind,
    /// Annotation scope; empty for other kinds.
    pub scope: Box<str>,
    /// Composition key: record word and first value (`node tools.x`, `command x.run`;
    /// `set` adds the property; `remove X` shares `node X`'s key).
    pub key: Box<str>,
    /// The plugin whose record won.
    pub owner: PluginId,
    /// The record's full line-record text (record line plus indented lines).
    pub text: Box<str>,
}

/// A record replaced by a later plugin's record with the same key (health report, D8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Override {
    /// Contribution kind.
    pub kind: Kind,
    /// Annotation scope.
    pub scope: Box<str>,
    /// Shared key.
    pub key: Box<str>,
    /// Plugin whose record was replaced.
    pub loser: PluginId,
    /// Plugin whose record replaced it (equal to `loser` for a duplicate within one set).
    pub winner: PluginId,
}

/// Expression-function registry (D20) for `expr`: one row per composed `function` record,
/// sorted by name. Resolve `ns.f(args)` by binary search, then call
/// [`crate::Plugins::call`]`(plugin[i], fn_id[i], …)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Functions {
    /// Namespaced name (`gmid.width`).
    pub name: Vec<Box<str>>,
    /// Owning plugin.
    pub plugin: Vec<PluginId>,
    /// Id passed to `schemify_call`.
    pub fn_id: Vec<u32>,
    /// Argument count.
    pub arity: Vec<u8>,
}

impl Functions {
    /// Returns the row of `name`, if registered.
    pub fn find(&self, name: &str) -> Option<usize> {
        self.name.binary_search_by(|n| (**n).cmp(name)).ok()
    }
}

/// The composition: surviving records grouped by kind (in [`Kind::ALL`] order), then
/// scope (sorted), then first appearance; plus the function registry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composed {
    /// Surviving records.
    pub records: Vec<ComposedRecord>,
    /// Registry derived from the [`Kind::Functions`] records.
    pub functions: Functions,
}

impl Composed {
    /// Returns the records of `kind` (all scopes).
    pub fn of(&self, kind: Kind) -> &[ComposedRecord] {
        let start = self.records.partition_point(|r| r.kind < kind);
        let end = self.records.partition_point(|r| r.kind <= kind);
        &self.records[start..end]
    }

    /// Returns the surviving record of (`kind`, `key`) in any scope, e.g. the owner of a
    /// command for routing `Event::Command`.
    pub fn find(&self, kind: Kind, key: &str) -> Option<&ComposedRecord> {
        self.of(kind).iter().find(|r| &*r.key == key)
    }
}

/// Composes every set of the plugins in `order` (plugins absent from `order` are ignored).
/// For each key the record of the last plugin wins; it keeps the position where the key
/// first appeared. A `node X` or `remove X` record (a full redefinition) also drops every
/// `set X …` record composed before it, as the UI composition does (a redefined node starts
/// with no properties); a later `set X …` appears again, at its new position. Every replaced
/// or dropped record is one [`Override`]. Deterministic: same inputs, same output.
///
/// # Panics
///
/// Panics if a set fails validation, which [`crate::intake`] already guarantees.
///
/// # Time complexity
///
/// O(kinds × scopes × |order| × rows) lookups plus O(records · log records) keying.
// ponytail: linear row scans per (kind, scope, plugin); index rows by plugin if a profile
// shows recomposition on the frame path.
pub fn compose(contributions: &Contributions, order: &[PluginId]) -> (Composed, Vec<Override>) {
    debug_assert!(
        order
            .iter()
            .enumerate()
            .all(|(i, p)| !order[..i].contains(p)),
        "duplicate id"
    );
    let mut composed = Composed::default();
    let mut overrides = Vec::new();
    for &kind in Kind::ALL {
        let mut scopes: Vec<&str> = contributions
            .rows()
            .filter(|r| r.1 == kind)
            .map(|r| r.2)
            .collect();
        scopes.sort_unstable();
        scopes.dedup();
        for scope in scopes {
            let mut index: BTreeMap<String, usize> = BTreeMap::new();
            let first = composed.records.len();
            // Records of this group dropped by a redefinition (existence: filtered below).
            let mut dropped: Vec<bool> = Vec::new();
            for &plugin in order {
                let Some(body) = contributions.get(plugin, kind, scope) else {
                    continue;
                };
                for rec in set_records(kind, body).expect("validated at intake") {
                    if let Some(node) = rec.key.strip_prefix("node ") {
                        let prefix = format!("set {node} ");
                        let stale: Vec<String> = index
                            .range(prefix.clone()..)
                            .take_while(|(k, _)| k.starts_with(&prefix))
                            .map(|(k, _)| k.clone())
                            .collect();
                        for key in stale {
                            let i = index.remove(&key).expect("collected from the index");
                            dropped[i - first] = true;
                            overrides.push(Override {
                                kind,
                                scope: scope.into(),
                                key: key.into(),
                                loser: composed.records[i].owner,
                                winner: plugin,
                            });
                        }
                    }
                    match index.get(&rec.key) {
                        Some(&i) => {
                            let won = &mut composed.records[i];
                            overrides.push(Override {
                                kind,
                                scope: scope.into(),
                                key: rec.key.as_str().into(),
                                loser: won.owner,
                                winner: plugin,
                            });
                            won.owner = plugin;
                            won.text = rec.text.into();
                        }
                        None => {
                            index.insert(rec.key.clone(), composed.records.len());
                            dropped.push(false);
                            composed.records.push(ComposedRecord {
                                kind,
                                scope: scope.into(),
                                key: rec.key.into(),
                                owner: plugin,
                                text: rec.text.into(),
                            });
                        }
                    }
                }
            }
            assert_eq!(composed.records.len() - first, dropped.len());
            let group = composed.records.split_off(first);
            composed.records.extend(
                group
                    .into_iter()
                    .zip(dropped)
                    .filter_map(|(r, gone)| (!gone).then_some(r)),
            );
            debug_assert_eq!(composed.records.len() - first, index.len());
        }
    }
    composed.functions = functions(composed.of(Kind::Functions));
    debug_assert!(composed.records.windows(2).all(|w| w[0].kind <= w[1].kind));
    (composed, overrides)
}

fn functions(records: &[ComposedRecord]) -> Functions {
    let mut rows: Vec<(Box<str>, PluginId, u32, u8)> = records
        .iter()
        .map(|r| {
            let row = lex(&r.text, |d| function_row(d, 1));
            let Ok(Ok((name, fn_id, arity))) = row else {
                panic!("validated at intake: {}", r.text)
            };
            (name, r.owner, fn_id, arity)
        })
        .collect();
    rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut f = Functions::default();
    for (name, plugin, fn_id, arity) in rows {
        f.name.push(name);
        f.plugin.push(plugin);
        f.fn_id.push(fn_id);
        f.arity.push(arity);
    }
    debug_assert!(
        f.name.windows(2).all(|w| w[0] < w[1]),
        "keys are unique per name"
    );
    f
}
