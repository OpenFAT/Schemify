//! Record and attribute identity: documents → identity tables joined across sides,
//! identity orders → one merged order, and same-id adds → re-ided theirs (spec §8
//! identity, order, adds).
//!
//! Contents:
//! - `pub(crate) fn join(docs: &[&Document<'_>]) -> Vec<[u32; 3]>`
//! - `pub(crate) fn join_attrs(docs: &[&Document<'_>], recs: [u32; 3]) -> Vec<[u32; 3]>`
//! - `pub(crate) fn merge_seq(seqs: [&[u32]; 3], flags: &[u8]) -> Option<Vec<u32>>`
//! - `pub(crate) fn reid_collisions<'s>(o, a, b: &Document<'s>) -> Option<Document<'s>>`

use crate::document::{Document, NONE, record_eq};
use crate::id::{FNV_OFFSET, IdRng, fnv1a64, fresh_id, id_sort_key};
use crate::merge::{A, B, KEEP, O};
use crate::schema::{KeyRule, RecordKind, Ty};

/// Flags of an identity present on all sides and kept.
const COMMON: u8 = 1 | 2 | 4 | KEEP;

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
            rows.push(((kind, d.record_word(r), k0, k1), side as u8, r));
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
    assert!(docs.len() <= 3 && recs[docs.len()..].iter().all(|&r| r == NONE));
    let mut rows = Vec::new();
    for (side, d) in docs.iter().enumerate() {
        if recs[side] == NONE {
            continue;
        }
        let schema = d.record_kind(recs[side]).schema();
        for a in d.attrs(recs[side]) {
            let key = d.attr_key(a);
            let repeat = schema.attr(key).is_some_and(|(_, s)| s.repeat);
            let disc = if repeat {
                d.attr_value(a, 0).unwrap_or("")
            } else {
                ""
            };
            rows.push(((key, disc), side as u8, a));
        }
    }
    group(rows)
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
            .all(|s| s.iter().all(|&x| (x as usize) < flags.len()))
    );
    let common = |x: u32| flags[x as usize] & COMMON == COMMON;
    let ord = seqs.map(|s| {
        s.iter()
            .copied()
            .filter(|&x| common(x))
            .collect::<Vec<u32>>()
    });
    let base = if ord[A] == ord[O] {
        &ord[B]
    } else if ord[B] == ord[O] || ord[B] == ord[A] {
        &ord[A]
    } else {
        return None;
    };
    let mut position: Vec<(u32, u32)> = base
        .iter()
        .enumerate()
        .map(|(p, &x)| (x, p as u32 + 1))
        .collect();
    position.sort_unstable();
    let anchor_of = |x: u32| {
        position[position
            .binary_search_by_key(&x, |e| e.0)
            .expect("common identity in base")]
        .1
    };

    let mut entries: Vec<(u32, u32, u32, u32)> = Vec::new(); // (anchor, run first, offset, identity)
    for side in [A, B] {
        let (mut anchor, mut first, mut offset) = (0, NONE, 0);
        for &x in seqs[side] {
            if common(x) {
                (anchor, first) = (anchor_of(x), NONE);
            } else if flags[x as usize] & KEEP != 0 {
                (first, offset) = if first == NONE {
                    (x, 0)
                } else {
                    (first, offset + 1)
                };
                entries.push((anchor, first, offset, x));
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
        while e < entries.len() && entries[e].0 == anchor as u32 {
            let x = entries[e].3;
            if !std::mem::replace(&mut emitted[x as usize], true) {
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
pub(crate) fn reid_collisions<'s>(
    o: &Document<'_>,
    a: &Document<'_>,
    b: &Document<'s>,
) -> Option<Document<'s>> {
    let ids = |d: &Document<'_>| -> Vec<(u128, u32)> {
        let mut v: Vec<(u128, u32)> = (1..d.record_count())
            .filter(|&r| {
                d.record_kind(r).key() == KeyRule::Id && d.record_kind(r) != RecordKind::Unknown
            })
            .filter_map(|r| d.field(r, 0).and_then(id_sort_key).map(|k| (k, r)))
            .collect();
        v.sort_unstable();
        v
    };
    let (io, ia, ib) = (ids(o), ids(a), ids(b));
    let find =
        |v: &[(u128, u32)], k: u128| v.binary_search_by_key(&k, |e| e.0).ok().map(|i| v[i].1);
    let mut colliding: Vec<(u128, u32)> = ib
        .iter()
        .filter(|&&(k, rb)| {
            find(&io, k).is_none() && find(&ia, k).is_some_and(|ra| !record_eq(a, ra, b, rb))
        })
        .copied()
        .collect();
    // One rename per id: `rename_id` rewrites every record of `b` with it, so a duplicate
    // id in `b` (W001) is renamed with its first occurrence (fuzz crash ded040dc).
    colliding.dedup_by_key(|e| e.0);
    let colliding: Vec<u32> = colliding.into_iter().map(|(_, rb)| rb).collect();
    if colliding.is_empty() {
        return None;
    }
    assert!(colliding.iter().all(|&rb| rb < b.record_count()));
    let mut nb = b.clone();
    let mut assigned: Vec<u128> = Vec::new();
    for rb in colliding {
        let old = b.field(rb, 0).expect("id-keyed record has field 0");
        let mut rng = IdRng::new(fnv1a64(FNV_OFFSET, old.as_bytes()));
        let new = fresh_id(&mut rng, |c| {
            let k = id_sort_key(c).expect("generated ids are legal");
            find(&io, k).is_some()
                || find(&ia, k).is_some()
                || find(&ib, k).is_some()
                || assigned.contains(&k)
        });
        assigned.push(id_sort_key(new.as_str()).expect("generated ids are legal"));
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
/// ([`NONE`] when absent). Rows: (key, side, index).
fn group<K: Ord + Copy>(mut rows: Vec<(K, u8, u32)>) -> Vec<[u32; 3]> {
    rows.sort_unstable_by_key(|&(k, side, idx)| (side, k, idx));
    let mut keyed: Vec<(K, u32, u8, u32)> = Vec::with_capacity(rows.len());
    for &(k, side, idx) in &rows {
        let ord = match keyed.last() {
            Some(&(pk, pord, pside, _)) if pk == k && pside == side => pord + 1,
            _ => 0,
        };
        keyed.push((k, ord, side, idx));
    }
    keyed.sort_unstable_by_key(|&(k, ord, side, _)| (k, ord, side));
    assert_eq!(keyed.len(), rows.len());
    let mut out: Vec<[u32; 3]> = Vec::new();
    for (i, &(k, ord, side, idx)) in keyed.iter().enumerate() {
        if i == 0 || (keyed[i - 1].0, keyed[i - 1].1) != (k, ord) {
            out.push([NONE; 3]);
        }
        let slot = &mut out.last_mut().expect("pushed above")[side as usize];
        assert_eq!(*slot, NONE, "one record per side per identity");
        *slot = idx;
    }
    out
}

/// Replaces every `Id`/`IdRef` field and `IdRef` attribute value equal to `old`.
fn rename_id(doc: &mut Document<'_>, old: &str, new: &str) {
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
