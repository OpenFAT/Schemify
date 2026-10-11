//! Record-level three-way merge and two-way diff (D34, research §3.6, spec §8).
//!
//! Both key records by identity: (kind, word, id-or-name, occurrence). Merge spine:
//! `reid_collisions` → `join` (identity table over O, A, B) → `decide` per identity
//! (with `merge_record` per attribute line and `diff3` per verbatim line) → per rank:
//! positional block or `merge_seq` ordering → the kind-rule check (a clean result that
//! breaks them is assembled again with the symbol interface as one conflict) →
//! `write_spans` with conflict markers.

use crate::diag::Code;
use crate::document::{Document, NONE, record_eq};
use crate::id::{FNV_OFFSET, IdRng, fnv1a64, fresh_id, id_sort_key};
use crate::reader::check_kind_rules;
use crate::schema::{KeyRule, RANK_MAX, RANK_POSITIONAL, RecordKind, Ty};
use crate::writer::{ConflictSpan, write_spans};
use std::ops::Range;

const O: usize = 0;
const A: usize = 1;
const B: usize = 2;

/// Result of [`merge3`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merged {
    /// The merged file in canonical form. With conflicts it contains git-style
    /// `<<<<<<< ours` / `=======` / `>>>>>>> theirs` blocks and does not parse.
    pub text: String,
    /// Number of conflict blocks in `text`; 0 means a clean merge.
    pub conflicts: u32,
}

/// Three-way merges `a` (ours) and `b` (theirs) against their common ancestor `o`.
///
/// Records are matched by identity: (kind, id) for id-keyed kinds, (kind, name) for
/// named kinds, the header by itself; the occurrence number separates duplicates.
/// Inside a record both sides merge per component: the record line, each attribute
/// line (by key, plus first value for repeatable attributes), the verbatim block and
/// the comment block. A component changed by one side takes that side; changed
/// identically takes either; changed differently is a conflict. Adds and deletes merge;
/// delete against modify is a conflict. Symbol graphics (rank 2) merge as one block.
/// Order inside file-order ranks merges too (both sides reordering differently is a
/// conflict over the whole rank).
///
/// Verbatim lines merge line by line (diff3): every run between lines that all three
/// sides keep takes the side that changed it; both changing one run differently is a
/// conflict.
///
/// When both sides add different records with the same id, `b`'s record gets a new id
/// drawn deterministically from the old one, and `b`'s id references to it are
/// rewritten. A conflicting record is written as ours, then theirs, between markers.
///
/// A clean result that breaks the kind rules both sides keep (spec §4, `E025`: both add
/// a schematic impl, or each deletes the other's last pin) does not count as clean: the
/// symbol interface (rank 1) becomes one conflict block of ours' and theirs' records.
///
/// Properties (tested): `merge3(o, a, o)` and `merge3(o, o, a)` equal `write(a)`;
/// clean merges are commutative in `a` and `b`; output is canonical.
///
/// Cost: O(n log n) in records.
///
/// # Examples
///
/// ```
/// use schemify_format::{merge3, parse};
/// let o = parse(b"schemify 1 shbench\ninst 0abc ./ota\n  at 0 0 R0\n  name X1\n  W 1\n")?;
/// let a = parse(b"schemify 1 shbench\ninst 0abc ./ota\n  at 10 0 R0\n  name X1\n  W 1\n")?;
/// let b = parse(b"schemify 1 shbench\ninst 0abc ./ota\n  at 0 0 R0\n  name X1\n  W 2\n")?;
/// let m = merge3(&o, &a, &b);
/// assert_eq!(m.conflicts, 0);
/// assert_eq!(m.text, "schemify 1 shbench\n\ninst 0abc ./ota\n  at 10 0 R0\n  name X1\n  W 2\n");
/// # Ok::<(), schemify_format::ParseError>(())
/// ```
pub fn merge3(o: &Document<'_>, a: &Document<'_>, b: &Document<'_>) -> Merged {
    for d in [o, a, b] {
        assert!(
            d.record_count() >= 1 && d.record_kind(0) == RecordKind::Header,
            "merge3 input lacks a header"
        );
    }
    let reided = reid_collisions(o, a, b);
    let b = reided.as_ref().unwrap_or(b);
    let docs = [o, a, b];

    let idents = join(&docs);
    let mut ident_of: [Vec<u32>; 3] = docs.map(|d| vec![NONE; d.record_count() as usize]);
    let mut rank_of = vec![0u8; idents.len()];
    let doc_ranks = docs.map(|d| d.ranks());
    for (i, recs) in idents.iter().enumerate() {
        for s in 0..3 {
            if recs[s] != NONE {
                ident_of[s][recs[s] as usize] = i as u32;
            }
        }
        // One rank per identity. Sides can disagree only for `Unknown` records, whose rank
        // is inherited from their position: the side that moved the record wins, like any
        // other component (fuzz crash 13af0e2c: `merge3(o, o, b)` must keep b's position).
        let rank = |s: usize| (recs[s] != NONE).then(|| doc_ranks[s][recs[s] as usize]);
        let (ro, ra, rb) = (rank(O), rank(A), rank(B));
        rank_of[i] = match (ra, rb) {
            (Some(a), Some(b)) if a != b && ro == Some(a) => b,
            (Some(a), _) => a,
            (None, Some(b)) => b,
            (None, None) => ro.expect("identity has a record"),
        };
    }

    let mut picks = Picks::default();
    let header = decide(&docs, [0, 0, 0], &mut picks);
    let outcomes: Vec<Outcome> = idents
        .iter()
        .map(|recs| decide(&docs, *recs, &mut picks))
        .collect();
    let mut flags = vec![0u8; idents.len()];
    for (i, recs) in idents.iter().enumerate() {
        let presence = (0..3)
            .filter(|&s| recs[s] != NONE)
            .fold(0, |f, s| f | 1 << s);
        flags[i] = presence
            | if matches!(outcomes[i], Outcome::Drop) {
                0
            } else {
                KEEP
            };
    }

    let head_side = match header {
        Outcome::Take(s) => s,
        Outcome::Merged(m) => m.fields,
        Outcome::Conflict { .. } | Outcome::Drop => A as u8,
    };
    let head = docs[head_side as usize];

    // by_rank[s][rank]: side s's records of that rank, in document order.
    let by_rank: [Vec<Vec<u32>>; 3] = [O, A, B].map(|s| {
        let mut buckets = vec![Vec::new(); RANK_MAX as usize + 1];
        for r in 1..docs[s].record_count() {
            buckets[rank_of[ident_of[s][r as usize] as usize] as usize].push(r);
        }
        buckets
    });
    // Writes the merged records rank by rank; rank `conflict_rank` (`NO_RANK`: none) is
    // written as one conflict block of ours' and theirs' records.
    let assemble = |conflict_rank: u8| -> (Document<'static>, Vec<ConflictSpan>) {
        let mut out = Document::bare("", head.version(), head.kind());
        let mut spans: Vec<ConflictSpan> = Vec::new();
        emit(&mut out, &mut spans, &docs, [0, 0, 0], header, &picks);
        for rank in 0..=RANK_MAX {
            let blocks = [O, A, B].map(|s| &by_rank[s][rank as usize]);
            if rank == conflict_rank {
                push_conflict(&mut out, &mut spans, &docs, blocks[A], blocks[B]);
                continue;
            }
            if rank == RANK_POSITIONAL {
                let same = |x: usize, y: usize| {
                    blocks[x].len() == blocks[y].len()
                        && blocks[x]
                            .iter()
                            .zip(blocks[y])
                            .all(|(&p, &q)| record_eq(docs[x], p, docs[y], q))
                };
                match pick3(same) {
                    Some(s) => {
                        for &r in blocks[s as usize] {
                            out.copy_record_any(docs[s as usize], r);
                        }
                    }
                    None => push_conflict(&mut out, &mut spans, &docs, blocks[A], blocks[B]),
                }
                continue;
            }
            // In sorted ranks the writer orders known kinds itself, so only `Unknown`
            // records keep a file order worth merging; known ones go first by identity.
            let sorted = out.kind().rank_sorted(rank);
            let seqs = [O, A, B].map(|s| {
                let ident = |r: &u32| ident_of[s][*r as usize];
                let unknown = |r: &&u32| docs[s].record_kind(**r) == RecordKind::Unknown;
                if !sorted {
                    return blocks[s].iter().map(ident).collect::<Vec<u32>>();
                }
                let mut seq: Vec<u32> = blocks[s]
                    .iter()
                    .filter(|r| !unknown(r))
                    .map(ident)
                    .collect();
                seq.sort_unstable();
                seq.extend(blocks[s].iter().filter(unknown).map(ident));
                seq
            });
            match merge_seq([&seqs[O], &seqs[A], &seqs[B]], &flags) {
                Some(order) => {
                    for i in order {
                        emit(
                            &mut out,
                            &mut spans,
                            &docs,
                            idents[i as usize],
                            outcomes[i as usize],
                            &picks,
                        );
                    }
                }
                None => push_conflict(&mut out, &mut spans, &docs, blocks[A], blocks[B]),
            }
        }
        (out, spans)
    };
    let (mut out, mut spans) = assemble(NO_RANK);
    // The kind rules count pins and impls (rank 1). Both sides keeping them makes a broken
    // result the merge's doing, and guarantees that both sides' rank-1 blocks are non-empty.
    if spans.is_empty() && kind_rules_hold(a) && kind_rules_hold(b) && !kind_rules_hold(&out) {
        (out, spans) = assemble(RecordKind::Impl.rank());
        assert!(!spans.is_empty(), "the interface conflict is a span");
    }

    let eof = [o, a, b].map(|d| d.eof_comments().collect::<Vec<_>>());
    let eof_split = match pick3(|x, y| eof[x] == eof[y]) {
        Some(s) => {
            eof[s as usize].iter().for_each(|c| out.push_eof_comment(c));
            None
        }
        None => {
            eof[A]
                .iter()
                .chain(&eof[B])
                .for_each(|c| out.push_eof_comment(c));
            Some(eof[A].len() as u32)
        }
    };
    let mut text = String::new();
    write_spans(&out, &spans, eof_split, &mut text);
    let conflicts = spans.len() as u32 + u32::from(eof_split.is_some());
    // Markers are the only lines that can start with `=` (records start with a word).
    assert_eq!(conflicts > 0, text.contains("\n=======\n"));
    Merged { text, conflicts }
}

/// What happens to one identity.
#[derive(Clone, Copy, Debug)]
enum Outcome {
    Drop,
    Take(u8),
    Merged(MergedRec),
    /// `ours`/`theirs` are record indices in A/B, [`NONE`] for a deleted side.
    Conflict {
        ours: u32,
        theirs: u32,
    },
}

/// A record assembled from components of different sides.
#[derive(Clone, Copy, Debug)]
struct MergedRec {
    fields: u8,
    comments: u8,
    /// Range into [`Picks::attrs`], in merged order.
    attrs: (u32, u32),
    /// Range into [`Picks::lines`], in merged order.
    lines: (u32, u32),
}

/// Components that [`merge_record`] took from different sides, shared by every merged
/// record: `(side, attribute index)` and `(side, verbatim line index)` in that side's
/// document. Grows by at most the attributes and verbatim lines of the three inputs.
#[derive(Default)]
struct Picks {
    attrs: Vec<(u8, u32)>,
    lines: Vec<(u8, u32)>,
}

/// Presence bits (`1 << side`) plus this one: the identity survives the merge.
const KEEP: u8 = 8;
const COMMON: u8 = 1 | 2 | 4 | KEEP;
/// No rank is forced into one conflict block.
const NO_RANK: u8 = u8::MAX;
/// Largest LCS table [`diff3`] aligns (cells = (base lines + 1) × (side lines + 1) after
/// the common prefix and suffix): 8 MiB of `u16`.
const ALIGN_CELLS_MAX: usize = 1 << 22;

/// Returns whether `doc` keeps the spec §4 kind rules (`E025`).
fn kind_rules_hold(doc: &Document<'_>) -> bool {
    let mut diags = Vec::new();
    check_kind_rules(doc, &mut diags);
    assert!(diags.iter().all(|d| d.code == Code::E025));
    diags.is_empty()
}

/// Three-way pick over sides O=0, A=1, B=2 given an equality test: the side that
/// changed wins, an identical change takes A; `None` when both changed differently.
fn pick3(eq: impl Fn(usize, usize) -> bool) -> Option<u8> {
    // Short-circuits: the common case (nothing changed) costs one comparison.
    let side = if eq(A, B) || eq(O, B) {
        Some(A as u8)
    } else if eq(O, A) {
        Some(B as u8)
    } else {
        None
    };
    assert!(
        side.is_some() || !eq(B, A),
        "a conflict means the sides differ"
    );
    side
}

fn decide(docs: &[&Document<'_>; 3], recs: [u32; 3], picks: &mut Picks) -> Outcome {
    let [ro, ra, rb] = recs;
    assert!(
        recs.iter()
            .zip(docs)
            .all(|(&r, d)| r == NONE || r < d.record_count())
    );
    let eq = |x: usize, y: usize| record_eq(docs[x], recs[x], docs[y], recs[y]);
    match (ro != NONE, ra != NONE, rb != NONE) {
        (true, true, true) => match pick3(eq) {
            Some(s) => Outcome::Take(s),
            None => merge_record(docs, recs, picks).map_or(
                Outcome::Conflict {
                    ours: ra,
                    theirs: rb,
                },
                Outcome::Merged,
            ),
        },
        (true, true, false) if eq(O, A) => Outcome::Drop,
        (true, true, false) => Outcome::Conflict {
            ours: ra,
            theirs: NONE,
        },
        (true, false, true) if eq(O, B) => Outcome::Drop,
        (true, false, true) => Outcome::Conflict {
            ours: NONE,
            theirs: rb,
        },
        (true, false, false) => Outcome::Drop,
        (false, true, false) => Outcome::Take(A as u8),
        (false, false, true) => Outcome::Take(B as u8),
        (false, true, true) if eq(A, B) => Outcome::Take(A as u8),
        (false, true, true) => Outcome::Conflict {
            ours: ra,
            theirs: rb,
        },
        (false, false, false) => unreachable!("identity without records"),
    }
}

/// Merges a record that all three sides have and both changed, component by component.
fn merge_record(docs: &[&Document<'_>; 3], recs: [u32; 3], picks: &mut Picks) -> Option<MergedRec> {
    assert!(
        recs.iter().all(|&r| r != NONE),
        "both-changed records exist on all sides"
    );
    assert!(
        !record_eq(docs[A], recs[A], docs[B], recs[B]),
        "identical sides take the fast path"
    );
    let d = |s: usize| (docs[s], recs[s]);
    let fields = pick3(|x, y| d(x).0.fields(d(x).1).eq(d(y).0.fields(d(y).1)))?;
    let comments = pick3(|x, y| d(x).0.comments(d(x).1).eq(d(y).0.comments(d(y).1)))?;
    let block = |s: usize| docs[s].verbatim(recs[s]).collect::<Vec<&str>>();
    let lines: Vec<(u8, u32)> =
        match pick3(|x, y| d(x).0.verbatim(d(x).1).eq(d(y).0.verbatim(d(y).1))) {
            Some(s) => (0..docs[s as usize].verbatim(recs[s as usize]).len() as u32)
                .map(|i| (s, i))
                .collect(),
            None => diff3(&block(O), &block(A), &block(B))?,
        };

    let attr_ids = join_attrs(docs, recs);
    let value_eq = |ids: &[u32; 3], x: usize, y: usize| {
        docs[x].attr_values(ids[x]).eq(docs[y].attr_values(ids[y]))
    };
    let mut take = vec![0u8; attr_ids.len()];
    let mut flags = vec![0u8; attr_ids.len()];
    for (i, ids) in attr_ids.iter().enumerate() {
        let side = match (ids[O] != NONE, ids[A] != NONE, ids[B] != NONE) {
            (true, true, true) => Some(pick3(|x, y| value_eq(ids, x, y))?),
            (true, true, false) if value_eq(ids, O, A) => None,
            (true, false, true) if value_eq(ids, O, B) => None,
            (true, false, false) => None,
            (false, true, false) => Some(A as u8),
            (false, false, true) => Some(B as u8),
            (false, true, true) if value_eq(ids, A, B) => Some(A as u8),
            _ => return None, // modify/delete or a different add on both sides
        };
        let presence = (0..3)
            .filter(|&s| ids[s] != NONE)
            .fold(0, |f, s| f | 1 << s);
        flags[i] = presence | if side.is_some() { KEEP } else { 0 };
        take[i] = side.unwrap_or(0);
    }
    let seqs = [O, A, B].map(|s| {
        let mut seq: Vec<(u32, u32)> = attr_ids
            .iter()
            .enumerate()
            .filter(|(_, ids)| ids[s] != NONE)
            .map(|(i, ids)| (ids[s], i as u32))
            .collect();
        seq.sort_unstable();
        seq.into_iter().map(|(_, i)| i).collect::<Vec<u32>>()
    });
    let order = merge_seq([&seqs[O], &seqs[A], &seqs[B]], &flags)?;
    let start = picks.attrs.len() as u32;
    for i in order {
        let side = take[i as usize];
        picks
            .attrs
            .push((side, attr_ids[i as usize][side as usize]));
    }
    // Lines are picked last, so a record that conflicts leaves no picks behind.
    let first_line = picks.lines.len() as u32;
    for (side, i) in lines {
        let base = docs[side as usize].rec_verbatim_start[recs[side as usize] as usize];
        picks.lines.push((side, base + i));
    }
    Some(MergedRec {
        fields,
        comments,
        attrs: (start, picks.attrs.len() as u32),
        lines: (first_line, picks.lines.len() as u32),
    })
}

/// Line-level three-way merge of one verbatim block (diff3). Lines of `o` that `a` and
/// `b` both keep, at matching positions, split the blocks into stable and changed runs;
/// a changed run takes the side that changed it (both identically: `a`'s). Returns the
/// merged lines as `(side, line index within that side's block)`, or `None` when both
/// sides changed one run differently or a block is too large to align
/// ([`ALIGN_CELLS_MAX`]).
///
/// Symmetric in `a` and `b`: the alignments `o`–`a` and `o`–`b` are computed alike and
/// independently. Cost: O(n) plus O(n·m) of the changed middles (bounded).
fn diff3(o: &[&str], a: &[&str], b: &[&str]) -> Option<Vec<(u8, u32)>> {
    let (ma, mb) = (align(o, a)?, align(o, b)?);
    assert!(ma.len() == o.len() && mb.len() == o.len());
    let mut out: Vec<(u8, u32)> = Vec::with_capacity(a.len().max(b.len()));
    let (mut lo, mut la, mut lb) = (0usize, 0usize, 0usize);
    loop {
        // Stable run: base lines both sides keep at the next positions.
        while lo < o.len() && ma[lo] == la as u32 && mb[lo] == lb as u32 {
            out.push((O as u8, lo as u32));
            (lo, la, lb) = (lo + 1, la + 1, lb + 1);
        }
        // The next base line both sides keep ends the changed run (or the blocks end).
        let sync = (lo..o.len()).find(|&j| ma[j] != NONE && mb[j] != NONE);
        let (eo, ea, eb) = match sync {
            Some(j) => (j, ma[j] as usize, mb[j] as usize),
            None => (o.len(), a.len(), b.len()),
        };
        assert!(eo >= lo && ea >= la && eb >= lb, "alignments are monotone");
        let (co, ca, cb) = (&o[lo..eo], &a[la..ea], &b[lb..eb]);
        let (side, from, to) = if ca == co {
            (B, lb, eb)
        } else if cb == co || ca == cb {
            (A, la, ea)
        } else {
            return None;
        };
        out.extend((from..to).map(|i| (side as u8, i as u32)));
        (lo, la, lb) = (eo, ea, eb);
        if sync.is_none() {
            break;
        }
    }
    assert!(
        lo == o.len() && la == a.len() && lb == b.len(),
        "every line consumed"
    );
    Some(out)
}

/// Aligns `x` to `o`: for each line of `o`, the index of the line of `x` it matches in
/// one longest common subsequence, or [`NONE`]. Matches are strictly increasing. The
/// common prefix and suffix match directly; the middle goes through an LCS table, and a
/// middle above [`ALIGN_CELLS_MAX`] cells returns `None` (the block merges as a unit).
// ponytail: O(n·m) table over the changed middle; Myers' O(ND) diff if large blocks
// with scattered edits need line merges.
fn align(o: &[&str], x: &[&str]) -> Option<Vec<u32>> {
    let prefix = o.iter().zip(x).take_while(|(p, q)| p == q).count();
    let rest = o.len().min(x.len()) - prefix;
    let suffix = o
        .iter()
        .rev()
        .zip(x.iter().rev())
        .take(rest)
        .take_while(|(p, q)| p == q)
        .count();
    let (mo, mx) = (&o[prefix..o.len() - suffix], &x[prefix..x.len() - suffix]);
    let (n, m) = (mo.len(), mx.len());
    if (n + 1) * (m + 1) > ALIGN_CELLS_MAX {
        return None;
    }
    // lcs[i * (m + 1) + j] = LCS length of mo[i..] and mx[j..]; it fits u16 because
    // min(n, m) < 2^11 under the cell cap.
    let mut lcs = vec![0u16; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * (m + 1) + j] = if mo[i] == mx[j] {
                lcs[(i + 1) * (m + 1) + j + 1] + 1
            } else {
                lcs[(i + 1) * (m + 1) + j].max(lcs[i * (m + 1) + j + 1])
            };
        }
    }
    let mut out = vec![NONE; o.len()];
    for (k, slot) in out[..prefix].iter_mut().enumerate() {
        *slot = k as u32;
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if mo[i] == mx[j] {
            out[prefix + i] = (prefix + j) as u32;
            (i, j) = (i + 1, j + 1);
        } else if lcs[(i + 1) * (m + 1) + j] >= lcs[i * (m + 1) + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    for k in 0..suffix {
        out[o.len() - suffix + k] = (x.len() - suffix + k) as u32;
    }
    assert!(
        out.iter()
            .filter(|&&v| v != NONE)
            .zip(out.iter().filter(|&&v| v != NONE).skip(1))
            .all(|(p, q)| p < q),
        "matches are strictly increasing"
    );
    Some(out)
}

/// Appends the merged form of one identity to `out`.
fn emit(
    out: &mut Document<'static>,
    spans: &mut Vec<ConflictSpan>,
    docs: &[&Document<'_>; 3],
    recs: [u32; 3],
    outcome: Outcome,
    picks: &Picks,
) {
    let before = out.record_count();
    assert!(spans.iter().all(|s| s.first + s.ours + s.theirs <= before));
    match outcome {
        Outcome::Drop => {}
        Outcome::Take(s) => {
            out.copy_record_any(docs[s as usize], recs[s as usize]);
        }
        Outcome::Merged(m) => {
            let (fs, cs) = (m.fields as usize, m.comments as usize);
            out.begin_like(docs[A], recs[A]);
            out.copy_comments(docs[cs], recs[cs]);
            out.copy_fields(docs[fs], recs[fs]);
            for &(s, attr) in &picks.attrs[m.attrs.0 as usize..m.attrs.1 as usize] {
                out.copy_attr(docs[s as usize], attr);
            }
            for &(s, line) in &picks.lines[m.lines.0 as usize..m.lines.1 as usize] {
                let d = docs[s as usize];
                out.push_verbatim(d.text(d.verbatim[line as usize]));
            }
        }
        Outcome::Conflict { ours, theirs } => {
            let ours: Vec<u32> = (ours != NONE).then_some(ours).into_iter().collect();
            let theirs: Vec<u32> = (theirs != NONE).then_some(theirs).into_iter().collect();
            push_conflict(out, spans, docs, &ours, &theirs);
        }
    }
}

/// Copies A's `ours` records then B's `theirs` records and records the conflict span.
fn push_conflict(
    out: &mut Document<'static>,
    spans: &mut Vec<ConflictSpan>,
    docs: &[&Document<'_>; 3],
    ours: &[u32],
    theirs: &[u32],
) {
    assert!(
        !ours.is_empty() || !theirs.is_empty(),
        "conflict without records"
    );
    let first = out.record_count();
    for &r in ours {
        out.copy_record_any(docs[A], r);
    }
    for &r in theirs {
        out.copy_record_any(docs[B], r);
    }
    spans.push(ConflictSpan {
        first,
        ours: ours.len() as u32,
        theirs: theirs.len() as u32,
    });
    assert_eq!(
        out.record_count(),
        first + ours.len() as u32 + theirs.len() as u32
    );
}

/// Merges three orderings of identities.
///
/// `flags[i]` holds presence bits (O=1, A=2, B=4) and [`KEEP`]. Identities kept and present
/// on all sides form the base order: B's order when A kept O's, else A's. Every other
/// kept identity is anchored after the nearest common identity before it on its own
/// side; runs at one anchor are ordered by (first identity, offset, identity), which
/// is symmetric in A and B. Returns `None` when A and B both reordered the common
/// identities, differently.
fn merge_seq(seqs: [&[u32]; 3], flags: &[u8]) -> Option<Vec<u32>> {
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

/// Identity table over the non-header records of up to three documents.
fn join(docs: &[&Document<'_>]) -> Vec<[u32; 3]> {
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
fn join_attrs(docs: &[&Document<'_>], recs: [u32; 3]) -> Vec<[u32; 3]> {
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

/// When A and B both added a record with the same id but different content, returns a
/// copy of `b` where that record has a fresh id (seeded from the old id, so the result
/// is deterministic) and every id field or id reference to it is rewritten.
fn reid_collisions<'s>(
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

/// Replaces every `Id`/`IdRef` field and `IdRef` attribute value equal to `old`.
fn rename_id(doc: &mut Document<'_>, old: &str, new: &str) {
    let replacement = doc.intern(new);
    let mut renamed = 0u32;
    for r in 0..doc.record_count() {
        let schema = doc.record_kind(r).schema();
        let fields = field_range(doc, r);
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
            let values = value_range(doc, a);
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

fn field_range(doc: &Document<'_>, r: u32) -> Range<usize> {
    let start = doc.rec_field_start[r as usize] as usize;
    let range = start..start + doc.fields(r).len();
    assert!(range.end <= doc.fields.len());
    range
}

fn value_range(doc: &Document<'_>, a: u32) -> Range<usize> {
    let start = doc.attr_value_start[a as usize] as usize;
    let range = start..start + doc.attr_values(a).len();
    assert!(range.end <= doc.attr_values.len());
    range
}

// ---------------------------------------------------------------- diff

/// How a record differs between the two sides of a [`diff`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// Only in `b`.
    Added,
    /// Only in `a`.
    Removed,
    /// In both, with different content.
    Changed,
}

/// Which components of a changed record differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Parts(pub u8);

impl Parts {
    /// The record line (positions, geometry, label text, cell reference).
    pub const FIELDS: Parts = Parts(1);
    /// At least one attribute line was added, removed or changed.
    pub const ATTRS: Parts = Parts(2);
    /// The verbatim block.
    pub const VERBATIM: Parts = Parts(4);
    /// The attached comments.
    pub const COMMENTS: Parts = Parts(8);

    /// Returns whether every bit of `other` is set.
    pub fn contains(self, other: Parts) -> bool {
        self.0 & other.0 == other.0
    }
}

/// One added, removed or changed record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// What happened.
    pub kind: ChangeKind,
    /// Record index in `a`, or [`NONE`] when added.
    pub a: u32,
    /// Record index in `b`, or [`NONE`] when removed.
    pub b: u32,
    /// Differing components; empty unless `Changed`.
    pub parts: Parts,
    /// Range into [`Diff::keys`]: attribute words that were added, removed or changed.
    pub keys: Range<u32>,
}

/// Record-level differences from `a` to `b`, in identity order (kind, then key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff<'d> {
    /// One entry per differing record.
    pub changes: Vec<Change>,
    /// Changed attribute words, shared by all changes; borrowed from the documents.
    pub keys: Vec<&'d str>,
}

/// Lists the records added, removed and changed from `a` to `b`, matched by identity
/// as in [`merge3`]. Symbol graphics (no identity) are matched by occurrence within
/// their kind, so inserting one shows the later ones as changed. Record order changes
/// alone are not reported.
///
/// Cost: O(n log n) in records.
///
/// # Examples
///
/// ```
/// use schemify_format::{diff, parse, ChangeKind, Parts};
/// let a = parse(b"schemify 1 shbench\nlabel 0abc 0 0 R0 vin\nwire 1abc 0 0 10 0\n")?;
/// let b = parse(b"schemify 1 shbench\nlabel 0abc 0 0 R0 vbias\n")?;
/// let d = diff(&a, &b);
/// assert_eq!(d.changes.len(), 2);
/// assert_eq!(d.changes[0].kind, ChangeKind::Removed); // wire: kinds come in listed order
/// assert_eq!(d.changes[1].kind, ChangeKind::Changed); // label text
/// assert!(d.changes[1].parts.contains(Parts::FIELDS));
/// # Ok::<(), schemify_format::ParseError>(())
/// ```
pub fn diff<'d>(a: &'d Document<'_>, b: &'d Document<'_>) -> Diff<'d> {
    assert!(
        a.record_count() >= 1 && b.record_count() >= 1,
        "diff input lacks a header"
    );
    let docs = [a, b];
    let mut idents = vec![[0, 0, NONE]];
    idents.extend(join(&docs));
    let mut out = Diff {
        changes: Vec::new(),
        keys: Vec::new(),
    };
    for recs in idents {
        let (ra, rb) = (recs[0], recs[1]);
        let kind = match (ra != NONE, rb != NONE) {
            (true, false) => ChangeKind::Removed,
            (false, true) => ChangeKind::Added,
            _ => ChangeKind::Changed,
        };
        let keys_start = out.keys.len() as u32;
        let mut parts = 0u8;
        if kind == ChangeKind::Changed {
            parts |= u8::from(!a.fields(ra).eq(b.fields(rb)));
            parts |= u8::from(!a.verbatim(ra).eq(b.verbatim(rb))) * Parts::VERBATIM.0;
            parts |= u8::from(!a.comments(ra).eq(b.comments(rb))) * Parts::COMMENTS.0;
            for ids in join_attrs(&docs, [ra, rb, NONE]) {
                let same = ids[0] != NONE
                    && ids[1] != NONE
                    && a.attr_values(ids[0]).eq(b.attr_values(ids[1]));
                if !same {
                    let key = if ids[0] != NONE {
                        a.attr_key(ids[0])
                    } else {
                        b.attr_key(ids[1])
                    };
                    if out.keys.last() != Some(&key) || out.keys.len() as u32 == keys_start {
                        out.keys.push(key);
                    }
                    parts |= Parts::ATTRS.0;
                }
            }
            if parts == 0 {
                continue;
            }
        }
        out.changes.push(Change {
            kind,
            a: ra,
            b: rb,
            parts: Parts(parts),
            keys: keys_start..out.keys.len() as u32,
        });
    }
    assert!(
        out.changes
            .iter()
            .all(|c| (c.kind == ChangeKind::Changed) == (c.parts.0 != 0))
    );
    out
}
