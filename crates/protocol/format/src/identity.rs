//! Record and attribute identity: documents → identity tables joined across sides,
//! identity orders → one merged order, and same-id adds → re-ided theirs (spec §8
//! identity, order, adds).
//!
//! Contents:
//! - `pub(crate) fn join(docs: &[&Document<'_>]) -> Vec<[u32; 3]>`
//! - `pub(crate) fn join_attrs(docs: &[&Document<'_>], recs: [u32; 3]) -> Vec<[u32; 3]>`
//! - `pub(crate) fn merge_seq(seqs: [&[u32]; 3], flags: &[u8]) -> Option<Vec<u32>>`
//! - `pub(crate) fn reid_collisions<'src>(o, a, b: &Document<'src>) -> Option<Document<'src>>`

use crate::document;
use crate::document::{Document, NONE};
use crate::id;
use crate::id::{FNV_OFFSET, IdRng, IdRow};
use crate::merge::{A, B, KEEP, O};
use crate::schema::{KeyRule, RecordKind, Ty};
use std::mem;

/// Flags of an identity present on all sides and kept.
const COMMON: u8 = 1 | 2 | 4 | KEEP;

/// One record or attribute line of one side, keyed for [`group`].
struct Row<K> {
    key: K,
    /// Occurrence number of `key` on its side, assigned by [`group`].
    ordinal: u32,
    side: u8,
    /// Record or attribute index in that side's document.
    index: u32,
}

/// A kept identity outside the base order, placed after its anchor. Field order is the
/// sort order: anchor, then run, then position in the run (symmetric in A and B).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Anchored {
    /// 1 + the base position of the nearest common identity before it; 0 for none.
    anchor: u32,
    /// First identity of its run at that anchor.
    run_first: u32,
    offset: u32,
    identity: u32,
}

/// Identity table over the non-header records of up to three documents.
pub(crate) fn join(docs: &[&Document<'_>]) -> Vec<[u32; 3]> {
    assert!(docs.len() <= 3);
    let mut rows = Vec::new();
    for (side, d) in docs.iter().enumerate() {
        for r in 1..d.record_count() {
            let kind = d.record_kind(r);
            assert!(
                kind != RecordKind::Header,
                "second header in an input document"
            );
            let (k0, k1) = d.record_key(r);
            rows.push(Row {
                key: (kind, d.record_word(r), k0, k1),
                ordinal: 0,
                side: side as u8,
                index: r,
            });
        }
    }
    let rows_len = rows.len();
    let idents = group(rows);
    assert!(idents.len() <= rows_len);
    idents
}

/// Identity table over the attribute lines of `recs[s]` on each side. A repeatable
/// known attribute is identified by key and first value, anything else by key.
pub(crate) fn join_attrs(docs: &[&Document<'_>], recs: [u32; 3]) -> Vec<[u32; 3]> {
    assert!(docs.len() <= 3);
    assert!(
        recs[docs.len()..].iter().all(|&r| r == NONE),
        "no record on a side without a document"
    );
    let mut rows = Vec::new();
    for (side, d) in docs.iter().enumerate() {
        if recs[side] == NONE {
            continue;
        }
        let schema = d.record_kind(recs[side]).schema();
        for a in d.attrs(recs[side]) {
            let key = d.attr_key(a);
            let repeat = schema.attr(key).is_some_and(|(_, s)| s.repeat);
            let discriminator = if repeat {
                d.attr_value(a, 0).unwrap_or("")
            } else {
                ""
            };
            rows.push(Row {
                key: (key, discriminator),
                ordinal: 0,
                side: side as u8,
                index: a,
            });
        }
    }
    let rows_len = rows.len();
    let idents = group(rows);
    assert!(idents.len() <= rows_len);
    idents
}

/// Merges three orderings of identities.
///
/// `flags[i]` holds presence bits (O=1, A=2, B=4) and [`KEEP`]. Identities kept and present
/// on all sides form the base order: B's order when A kept O's, else A's. Every other
/// kept identity is anchored after the nearest common identity before it on its own
/// side; runs at one anchor are ordered by (first identity, offset, identity), which
/// is symmetric in A and B. Returns `None` when A and B both reordered the common
/// identities, differently.
pub(crate) fn merge_seq(seqs: [&[u32]; 3], flags: &[u8]) -> Option<Vec<u32>> {
    assert!(
        seqs.iter()
            .all(|s| s.iter().all(|&x| (x as usize) < flags.len())),
        "every identity has its flags"
    );
    let common = |x: u32| flags[x as usize] & COMMON == COMMON;
    let ord: [Vec<u32>; 3] = seqs.map(|s| s.iter().copied().filter(|&x| common(x)).collect());
    let base = if ord[A] == ord[O] {
        &ord[B]
    } else if ord[B] == ord[O] || ord[B] == ord[A] {
        &ord[A]
    } else {
        return None;
    };
    // anchor_of[x]: 1 + the base position of common identity `x`.
    let mut anchor_of = vec![NONE; flags.len()];
    for (p, &x) in base.iter().enumerate() {
        anchor_of[x as usize] = p as u32 + 1;
    }

    let mut entries: Vec<Anchored> = Vec::new();
    for side in [A, B] {
        let (mut anchor, mut first, mut offset) = (0, NONE, 0);
        for &x in seqs[side] {
            if common(x) {
                anchor = anchor_of[x as usize];
                assert_ne!(anchor, NONE, "a common identity is in the base order");
                first = NONE;
            } else if flags[x as usize] & KEEP != 0 {
                (first, offset) = if first == NONE {
                    (x, 0)
                } else {
                    (first, offset + 1)
                };
                entries.push(Anchored {
                    anchor,
                    run_first: first,
                    offset,
                    identity: x,
                });
            }
        }
    }
    entries.sort_unstable();
    let mut emitted = vec![false; flags.len()];
    let mut out = Vec::with_capacity(base.len() + entries.len());
    let mut e = 0;
    for anchor in 0..=base.len() {
        if anchor > 0 {
            out.push(base[anchor - 1]);
        }
        while e < entries.len() && entries[e].anchor == anchor as u32 {
            let x = entries[e].identity;
            if !mem::replace(&mut emitted[x as usize], true) {
                out.push(x);
            }
            e += 1;
        }
    }
    assert_eq!(e, entries.len(), "every entry has an anchor");
    assert!(
        out.iter().all(|&x| flags[x as usize] & KEEP != 0),
        "only kept identities are ordered"
    );
    Some(out)
}

/// When A and B both added a record with the same id but different content, returns a
/// copy of `b` where that record has a fresh id (seeded from the old id, so the result
/// is deterministic) and every id field or id reference to it is rewritten.
pub(crate) fn reid_collisions<'src>(
    o: &Document<'_>,
    a: &Document<'_>,
    b: &Document<'src>,
) -> Option<Document<'src>> {
    let ids = |d: &Document<'_>| -> Vec<IdRow> {
        let mut rows: Vec<IdRow> = (1..d.record_count())
            .filter(|&r| {
                d.record_kind(r).key() == KeyRule::Id && d.record_kind(r) != RecordKind::Unknown
            })
            .filter_map(|r| {
                let key = d.field(r, 0).and_then(id::id_sort_key)?;
                Some(IdRow { key, record: r })
            })
            .collect();
        rows.sort_unstable();
        rows
    };
    let (io, ia, ib) = (ids(o), ids(a), ids(b));
    let find = |rows: &[IdRow], key: u128| {
        let i = rows.binary_search_by_key(&key, |row| row.key).ok()?;
        Some(rows[i].record)
    };
    let mut colliding: Vec<IdRow> = ib
        .iter()
        .filter(|row| {
            find(&io, row.key).is_none()
                && find(&ia, row.key).is_some_and(|ra| !document::record_eq(a, ra, b, row.record))
        })
        .copied()
        .collect();
    // One rename per id: `rename_id` rewrites every record of `b` with it, so a duplicate
    // id in `b` (W001) is renamed with its first occurrence (fuzz crash ded040dc).
    colliding.dedup_by_key(|row| row.key);
    if colliding.is_empty() {
        return None;
    }
    assert!(colliding.iter().all(|row| row.record < b.record_count()));
    let mut nb = b.clone();
    let mut assigned: Vec<u128> = Vec::new();
    for row in colliding {
        let old = b
            .field(row.record, 0)
            .expect("an id-keyed record has field 0");
        let mut rng = IdRng::new(id::fnv1a64(FNV_OFFSET, old.as_bytes()));
        let new = id::fresh_id(&mut rng, |c| {
            let k = id::id_sort_key(c).expect("generated ids are legal");
            find(&io, k).is_some()
                || find(&ia, k).is_some()
                || find(&ib, k).is_some()
                || assigned.contains(&k)
        });
        assigned.push(id::id_sort_key(new.as_str()).expect("generated ids are legal"));
        rename_id(&mut nb, old, new.as_str());
    }
    assert_eq!(
        nb.record_count(),
        b.record_count(),
        "re-id renames, never adds"
    );
    Some(nb)
}

// ---- private ----

/// Assigns occurrence ordinals per side and groups rows with equal (key, ordinal)
/// across sides. Returns, in key order, each identity's index on every side
/// ([`NONE`] when absent).
fn group<K: Ord + Copy>(mut rows: Vec<Row<K>>) -> Vec<[u32; 3]> {
    assert!(rows.iter().all(|row| row.side < 3), "three sides at most");
    rows.sort_unstable_by_key(|row| (row.side, row.key, row.index));
    for i in 1..rows.len() {
        let (earlier, later) = (&rows[i - 1], &rows[i]);
        if earlier.key == later.key && earlier.side == later.side {
            rows[i].ordinal = rows[i - 1].ordinal + 1;
        }
    }
    rows.sort_unstable_by_key(|row| (row.key, row.ordinal, row.side));
    let mut out: Vec<[u32; 3]> = Vec::new();
    let mut identity = [NONE; 3];
    for (i, row) in rows.iter().enumerate() {
        if i > 0 && (rows[i - 1].key, rows[i - 1].ordinal) != (row.key, row.ordinal) {
            out.push(mem::replace(&mut identity, [NONE; 3]));
        }
        let slot = &mut identity[row.side as usize];
        assert_eq!(*slot, NONE, "one record per side per identity");
        *slot = row.index;
    }
    if !rows.is_empty() {
        out.push(identity);
    }
    assert!(out.len() <= rows.len());
    out
}

/// Replaces every `Id`/`IdRef` field and `IdRef` attribute value equal to `old`.
fn rename_id(doc: &mut Document<'_>, old: &str, new: &str) {
    assert!(id::is_id(new), "the new id is legal");
    assert_ne!(old, new, "a rename changes the id");
    let replacement = doc.intern(new);
    let mut renamed = 0u32;
    for r in 0..doc.record_count() {
        let schema = doc.record_kind(r).schema();
        let fields = doc.field_range(r);
        for f in fields.clone() {
            let is_ref = matches!(schema.fields.ty(f - fields.start), Some(Ty::Id | Ty::IdRef));
            if is_ref && doc.text(doc.fields[f]) == old {
                doc.fields[f] = replacement;
                renamed += 1;
            }
        }
        for a in doc.attrs(r) {
            let Some((_, attr)) = schema.attr(doc.attr_key(a)) else {
                continue;
            };
            let values = doc.value_range(a);
            for v in values.clone() {
                if attr.shape.ty(v - values.start) == Some(Ty::IdRef)
                    && doc.text(doc.attr_values[v]) == old
                {
                    doc.attr_values[v] = replacement;
                }
            }
        }
    }
    assert!(renamed >= 1, "renamed id must define a record");
}
