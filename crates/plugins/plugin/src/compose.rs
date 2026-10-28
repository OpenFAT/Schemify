//! Contribution composition (§1, D8): every plugin's sets and the plugin order in →
//! `plugin₁ ⊕ … ⊕ pluginₙ` per (kind, scope), last wins, plus every override and the
//! expression-function registry (D20) out.
//!
//! Contents:
//! - `pub struct ComposedRecord`, `pub struct Override`
//! - `pub struct Functions`, `Functions::find(&self, name: &str) -> Option<usize>`
//! - `pub struct Composed`, `Composed::of(&self, kind: Kind) -> &[ComposedRecord]`,
//!   `Composed::find(&self, kind: Kind, key: &str) -> Option<&ComposedRecord>`
//! - `pub fn compose(contributions: &Contributions, order: &[PluginId]) -> (Composed, Vec<Override>)`

use std::collections::BTreeMap;
use std::mem;

use schemify_abi::Kind;

use crate::contributions::{self, FunctionRow};
use crate::{Contributions, PluginId};

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
/// sorted by name (unique). Resolve `ns.f(args)` by binary search, then call
/// [`crate::Plugins::call`]`(plugin[i], fn_id[i], …)`.
///
/// The columns are public for reading; a caller that pushes to them keeps them in step.
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

/// The composition: surviving records grouped by kind (in [`Kind::ALL`] order), then
/// scope (sorted), then first appearance; plus the function registry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composed {
    /// Surviving records.
    pub records: Vec<ComposedRecord>,
    /// Registry derived from the [`Kind::Functions`] records.
    pub functions: Functions,
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
/// Panics if a set fails validation, which [`crate::intake`] already guarantees, or (debug)
/// if `order` names a plugin twice.
///
/// # Time complexity
///
/// O(kinds × scopes × |order| × rows) lookups plus O(records · log records) keying.
// ponytail: linear row scans per (kind, scope, plugin); index rows by plugin if a profile
// shows recomposition on the frame path.
#[must_use]
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
            .filter(|row| row.kind == kind)
            .map(|row| row.scope)
            .collect();
        scopes.sort_unstable();
        scopes.dedup();
        for scope in scopes {
            // One (kind, scope) group: key → record index; records a redefinition dropped
            // are marked in `dropped` and filtered out at the group's end.
            let first = composed.records.len();
            let mut index: BTreeMap<String, usize> = BTreeMap::new();
            let mut dropped: Vec<bool> = Vec::new();
            for &plugin in order {
                let Some(body) = contributions.get(plugin, kind, scope) else {
                    continue;
                };
                let records = contributions::set_records(kind, body).expect("validated at intake");
                for record in records {
                    let replaced = |key: String, loser| Override {
                        kind,
                        scope: scope.into(),
                        key: key.into(),
                        loser,
                        winner: plugin,
                    };
                    if let Some(node) = record.key.strip_prefix("node ") {
                        let prefix = format!("set {node} ");
                        let stale: Vec<String> = index
                            .range(prefix.clone()..)
                            .take_while(|(k, _)| k.starts_with(&prefix))
                            .map(|(k, _)| k.clone())
                            .collect();
                        for key in stale {
                            let i = index.remove(&key).expect("collected from the index");
                            dropped[i - first] = true;
                            overrides.push(replaced(key, composed.records[i].owner));
                        }
                    }
                    match index.get(&record.key) {
                        Some(&i) => {
                            let won = &mut composed.records[i];
                            overrides.push(replaced(record.key, won.owner));
                            won.owner = plugin;
                            won.text = record.text.into();
                        }
                        None => {
                            index.insert(record.key.clone(), composed.records.len());
                            dropped.push(false);
                            composed.records.push(ComposedRecord {
                                kind,
                                scope: scope.into(),
                                key: record.key.into(),
                                owner: plugin,
                                text: record.text.into(),
                            });
                        }
                    }
                }
            }
            assert_eq!(composed.records.len() - first, dropped.len());
            let group = composed.records.split_off(first);
            let kept = group.into_iter().zip(dropped).filter(|(_, gone)| !gone);
            composed.records.extend(kept.map(|(record, _)| record));
            debug_assert_eq!(composed.records.len() - first, index.len());
        }
    }
    composed.functions = functions(composed.of(Kind::Functions));
    debug_assert!(composed.records.windows(2).all(|w| w[0].kind <= w[1].kind));
    (composed, overrides)
}

impl Functions {
    /// Returns the row of `name`, if registered. O(log rows).
    ///
    /// # Panics
    ///
    /// Panics (debug builds) if the columns are not in step.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<usize> {
        debug_assert_eq!(self.name.len(), self.plugin.len());
        debug_assert_eq!(self.name.len(), self.fn_id.len());
        debug_assert_eq!(self.name.len(), self.arity.len());
        self.name.binary_search_by(|row| (**row).cmp(name)).ok()
    }
}

impl Composed {
    /// Returns the records of `kind` (all scopes). O(log records).
    #[must_use]
    pub fn of(&self, kind: Kind) -> &[ComposedRecord] {
        let start = self.records.partition_point(|record| record.kind < kind);
        let end = self.records.partition_point(|record| record.kind <= kind);
        // A `start` past `end` means `records` is not grouped by kind.
        debug_assert!(start <= end, "records are grouped by kind");
        &self.records[start..end]
    }

    /// Returns the surviving record of (`kind`, `key`) in any scope, e.g. the owner of a
    /// command for routing `Event::Command`. O(records of `kind`).
    #[must_use]
    pub fn find(&self, kind: Kind, key: &str) -> Option<&ComposedRecord> {
        let found = self.of(kind).iter().find(|record| &*record.key == key);
        debug_assert!(found.is_none_or(|record| record.kind == kind));
        found
    }
}

// ---- private ----

/// The registry of the composed `function` records, sorted by name.
fn functions(records: &[ComposedRecord]) -> Functions {
    debug_assert!(records.iter().all(|record| record.kind == Kind::Functions));
    let mut rows: Vec<FunctionRow> = Vec::with_capacity(records.len());
    for record in records {
        let lexed = contributions::lex(&record.text, |document| {
            contributions::function_row(document, 1)
        });
        let Ok(Ok(row)) = lexed else {
            panic!("validated at intake: {}", record.text)
        };
        rows.push(row);
    }
    // Sort record indices by name, so each row keeps its record's owner.
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_unstable_by(|&a, &b| rows[a].name.cmp(&rows[b].name));
    let mut registry = Functions::default();
    for i in order {
        let row = &mut rows[i];
        registry.name.push(mem::take(&mut row.name));
        registry.plugin.push(records[i].owner);
        registry.fn_id.push(row.fn_id);
        registry.arity.push(row.arity);
    }
    debug_assert_eq!(registry.name.len(), records.len());
    debug_assert!(
        registry.name.windows(2).all(|pair| pair[0] < pair[1]),
        "keys are unique per name"
    );
    registry
}
