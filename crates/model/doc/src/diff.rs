//! Two documents of one file kind → the ops that turn the first into the second.
//! Cites: edits spec §4 (external changes rebased as ops).
//!
//! Contents:
//! - `pub(crate) fn diff_ops(from: &Document, to: &Document) -> Vec<Op>`

use crate::document::{Document, Loc, TableId};
use crate::ops::{Op, Target};
use crate::record::RecordBuf;
use std::collections::BTreeMap;

/// Returns ops that turn `from` into `to` (same file kind; canonical text equal after
/// applying). The header is one `put`; an ordered table that differs is rewritten whole
/// (deletes by descending row, inserts by ascending row); unordered tables go by key
/// (delete, put, insert). All deletes come first, then puts, then inserts, so a key may
/// move between kinds. Cost: O(n log n) in records, allocating one `RecordBuf` each.
pub(crate) fn diff_ops(from: &Document, to: &Document) -> Vec<Op> {
    assert_eq!(from.kind(), to.kind(), "diff across file kinds");
    let (mut deletes, mut puts, mut inserts) = (Vec::new(), Vec::new(), Vec::new());
    let header = to.header_record();
    if from.header_record() != header {
        puts.push(Op::Put {
            target: Target::header(),
            rec: header,
        });
    }
    for t in TableId::ALL {
        let rows = |d: &Document| -> Vec<(RecordBuf, Target)> {
            (0..d.table_len(t) as u32)
                .map(|row| {
                    let loc = Loc { table: t, row };
                    (d.record(loc), Target::at(d, loc))
                })
                .collect()
        };
        let (a, b) = (rows(from), rows(to));
        if t.ordered() {
            if a.iter().map(|x| &x.0).eq(b.iter().map(|x| &x.0)) {
                continue;
            }
            deletes.extend(a.into_iter().rev().map(|(_, tg)| Op::Delete(tg)));
            for (row, (rec, _)) in b.into_iter().enumerate() {
                inserts.push(Op::Insert {
                    rec,
                    at: Some(row as u32),
                });
            }
            continue;
        }
        let key = |(r, tg): (RecordBuf, Target)| ((r.kind, tg.key), r);
        let a: BTreeMap<_, _> = a.into_iter().map(key).collect();
        let mut b: BTreeMap<_, _> = b.into_iter().map(key).collect();
        for (k, rec) in a {
            let new = b.remove(&k);
            let target = Target {
                kind: k.0,
                key: k.1,
            };
            match new {
                None => deletes.push(Op::Delete(target)),
                Some(new) if new != rec => puts.push(Op::Put { target, rec: new }),
                Some(_) => {}
            }
        }
        inserts.extend(b.into_values().map(|rec| Op::Insert { rec, at: None }));
    }
    let rows: usize = TableId::ALL
        .iter()
        .map(|t| from.table_len(*t) + to.table_len(*t))
        .sum();
    deletes.extend(puts);
    deletes.extend(inserts);
    assert!(
        deletes.len() <= rows + 1,
        "at most one op per record plus the header"
    );
    deletes
}
