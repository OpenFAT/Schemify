//! Record-level three-way merge: base O, ours A, theirs B → canonical text with conflict
//! blocks (D34, D28, research §3.6, spec §8).
//!
//! Spine: `reid_collisions` → `Plan::new` (identity table over O, A, B; `decide` per
//! identity, with `merge_record` per attribute line and `diff3` per verbatim line) →
//! `assemble` per rank (positional block or `merge_seq` ordering) → the kind-rule check
//! (a clean result that breaks them is assembled again with the symbol interface as one
//! conflict) → end-of-file comments → `write_spans` with conflict markers.
//!
//! Contents:
//! - `pub struct Merged`
//! - `pub fn merge3(o: &Document<'_>, a: &Document<'_>, b: &Document<'_>) -> Merged`
//! - `pub(crate) const O, A, B: usize`, `pub(crate) const KEEP: u8`

use crate::diag::Code;
use crate::document::{Document, NONE, record_eq};
use crate::identity::{join, join_attrs, merge_seq, reid_collisions};
use crate::lines::diff3;
use crate::reader::check_kind_rules;
use crate::schema::{RANK_MAX, RANK_POSITIONAL, RecordKind};
use crate::writer::{ConflictSpan, write_spans};

/// Result of [`merge3`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merged {
    /// The merged file in canonical form. With conflicts it contains git-style
    /// `<<<<<<< ours` / `=======` / `>>>>>>> theirs` blocks and does not parse.
    pub text: String,
    /// Number of conflict blocks in `text`; 0 means a clean merge.
    pub conflicts: u32,
}

/// Side indices into `[o, a, b]`.
pub(crate) const O: usize = 0;
/// Ours.
pub(crate) const A: usize = 1;
/// Theirs.
pub(crate) const B: usize = 2;
/// Presence bits (`1 << side`) plus this one: the identity survives the merge.
pub(crate) const KEEP: u8 = 8;

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

/// Every decision the merge makes before writing, shared by both `assemble` passes.
struct Plan {
    /// Per identity: its record on each side, or [`NONE`].
    idents: Vec<[u32; 3]>,
    outcomes: Vec<Outcome>,
    /// Per identity: presence bits and [`KEEP`].
    flags: Vec<u8>,
    /// `ident_of[s][r]`: the identity of side `s`'s record `r`.
    ident_of: [Vec<u32>; 3],
    /// `by_rank[s][rank]`: side `s`'s records of that rank, in document order.
    by_rank: [Vec<Vec<u32>>; 3],
    header: Outcome,
    picks: Picks,
}

/// The merged document under construction and its conflict spans.
type Assembly = (Document<'static>, Vec<ConflictSpan>);

/// No rank is forced into one conflict block.
const NO_RANK: u8 = u8::MAX;

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
/// # Panics
///
/// Panics if an input lacks its header record (never true for a parsed or built one).
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
    let plan = Plan::new(&docs);
    let (mut out, mut spans) = assemble(&docs, &plan, NO_RANK);
    // The kind rules count pins and impls (rank 1). Both sides keeping them makes a broken
    // result the merge's doing, and guarantees that both sides' rank-1 blocks are non-empty.
    if spans.is_empty() && kind_rules_hold(a) && kind_rules_hold(b) && !kind_rules_hold(&out) {
        (out, spans) = assemble(&docs, &plan, RecordKind::Impl.rank());
        assert!(!spans.is_empty(), "the interface conflict is a span");
    }
    let eof_split = merge_eof(&docs, &mut out);
    let mut text = String::new();
    write_spans(&out, &spans, eof_split, &mut text);
    let conflicts = spans.len() as u32 + u32::from(eof_split.is_some());
    // Markers are the only lines that can start with `=` (records start with a word).
    assert_eq!(conflicts > 0, text.contains("\n=======\n"));
    Merged { text, conflicts }
}

// ---- private ----

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

impl Plan {
    /// Joins the three sides by identity and decides each identity's outcome.
    fn new(docs: &[&Document<'_>; 3]) -> Plan {
        let idents = join(docs);
        let mut ident_of: [Vec<u32>; 3] = docs.map(|d| vec![NONE; d.record_count() as usize]);
        let doc_ranks = docs.map(|d| d.ranks());
        let mut rank_of = vec![0u8; idents.len()];
        for (i, recs) in idents.iter().enumerate() {
            for s in [O, A, B] {
                if recs[s] != NONE {
                    ident_of[s][recs[s] as usize] = i as u32;
                }
            }
            // One rank per identity. Sides can disagree only for `Unknown` records, whose
            // rank is inherited from their position: the side that moved the record wins,
            // like any other component (fuzz crash 13af0e2c: `merge3(o, o, b)` must keep
            // b's position).
            let rank = |s: usize| (recs[s] != NONE).then(|| doc_ranks[s][recs[s] as usize]);
            rank_of[i] = match (rank(O), rank(A), rank(B)) {
                (ro, Some(a), Some(b)) if a != b && ro == Some(a) => b,
                (_, Some(a), _) => a,
                (_, None, Some(b)) => b,
                (ro, None, None) => ro.expect("identity has a record"),
            };
        }
        let mut picks = Picks::default();
        let header = decide(docs, [0, 0, 0], &mut picks);
        let outcomes: Vec<Outcome> = idents
            .iter()
            .map(|recs| decide(docs, *recs, &mut picks))
            .collect();
        let flags = idents
            .iter()
            .zip(&outcomes)
            .map(|(recs, outcome)| {
                let keep = if matches!(outcome, Outcome::Drop) {
                    0
                } else {
                    KEEP
                };
                presence(recs) | keep
            })
            .collect();
        let by_rank = [O, A, B].map(|s| {
            let mut buckets = vec![Vec::new(); RANK_MAX as usize + 1];
            for r in 1..docs[s].record_count() {
                buckets[rank_of[ident_of[s][r as usize] as usize] as usize].push(r);
            }
            buckets
        });
        Plan {
            idents,
            outcomes,
            flags,
            ident_of,
            by_rank,
            header,
            picks,
        }
    }

    /// Appends identity `i`'s merged form.
    fn emit(&self, out: &mut Assembly, docs: &[&Document<'_>; 3], i: u32) {
        let i = i as usize;
        emit(out, docs, self.idents[i], self.outcomes[i], &self.picks);
    }
}

/// Presence bits of an identity: `1 << side` for each side that has it.
fn presence(recs: &[u32; 3]) -> u8 {
    (0..3)
        .filter(|&s| recs[s] != NONE)
        .fold(0, |f, s| f | 1 << s)
}

/// Writes the merged records rank by rank; rank `conflict_rank` ([`NO_RANK`]: none) is
/// written as one conflict block of ours' and theirs' records.
fn assemble(docs: &[&Document<'_>; 3], plan: &Plan, conflict_rank: u8) -> Assembly {
    let head_side = match plan.header {
        Outcome::Take(s) => s,
        Outcome::Merged(m) => m.fields,
        Outcome::Conflict { .. } | Outcome::Drop => A as u8,
    };
    let head = docs[head_side as usize];
    let mut out: Assembly = (Document::bare("", head.version(), head.kind()), Vec::new());
    emit(&mut out, docs, [0, 0, 0], plan.header, &plan.picks);
    for rank in 0..=RANK_MAX {
        let blocks = [O, A, B].map(|s| plan.by_rank[s][rank as usize].as_slice());
        if rank == conflict_rank {
            push_conflict(&mut out, docs, blocks[A], blocks[B]);
        } else if rank == RANK_POSITIONAL {
            assemble_positional(&mut out, docs, blocks);
        } else {
            assemble_ordered(&mut out, docs, plan, blocks, rank);
        }
    }
    assert_eq!(out.0.record_kind(0), RecordKind::Header);
    out
}

/// Rank 2 (symbol graphics) merges as one block by the pick rule.
fn assemble_positional(out: &mut Assembly, docs: &[&Document<'_>; 3], blocks: [&[u32]; 3]) {
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
                out.0.copy_record_any(docs[s as usize], r);
            }
        }
        None => push_conflict(out, docs, blocks[A], blocks[B]),
    }
}

/// A file-order or sorted rank: merges the order of the identities, then emits each.
fn assemble_ordered(
    out: &mut Assembly,
    docs: &[&Document<'_>; 3],
    plan: &Plan,
    blocks: [&[u32]; 3],
    rank: u8,
) {
    // In sorted ranks the writer orders known kinds itself, so only `Unknown` records keep
    // a file order worth merging; known ones go first by identity.
    let sorted = out.0.kind().rank_sorted(rank);
    let unknown = |s: usize| move |r: &&u32| docs[s].record_kind(**r) == RecordKind::Unknown;
    let seqs = [O, A, B].map(|s| {
        let ident = |r: &u32| plan.ident_of[s][*r as usize];
        if !sorted {
            return blocks[s].iter().map(ident).collect::<Vec<u32>>();
        }
        let mut seq: Vec<u32> = blocks[s]
            .iter()
            .filter(|r| !unknown(s)(r))
            .map(ident)
            .collect();
        seq.sort_unstable();
        seq.extend(blocks[s].iter().filter(unknown(s)).map(ident));
        seq
    });
    match merge_seq([&seqs[O], &seqs[A], &seqs[B]], &plan.flags) {
        Some(order) => order.into_iter().for_each(|i| plan.emit(out, docs, i)),
        None if sorted => {
            // Only the `Unknown` records' file order can conflict here: known kinds merge
            // by identity as usual, and the unknown ones (written after them, in file
            // order) form the conflict block, so it stays contiguous in the writer's
            // canonical order (fuzz crash dceebe51).
            let mut known: Vec<u32> = seqs
                .iter()
                .flatten()
                .copied()
                .filter(|&i| {
                    let recs = plan.idents[i as usize];
                    let s = [A, B, O].into_iter().find(|&s| recs[s] != NONE);
                    let s = s.expect("identity has a record");
                    docs[s].record_kind(recs[s]) != RecordKind::Unknown
                })
                .filter(|&i| plan.flags[i as usize] & KEEP != 0)
                .collect();
            known.sort_unstable();
            known.dedup();
            known.into_iter().for_each(|i| plan.emit(out, docs, i));
            let side =
                |s: usize| -> Vec<u32> { blocks[s].iter().filter(unknown(s)).copied().collect() };
            push_conflict(out, docs, &side(A), &side(B));
        }
        None => push_conflict(out, docs, blocks[A], blocks[B]),
    }
}

/// Picks the end-of-file comments into `out`; returns the conflict split (ours' count)
/// when both sides changed them differently.
fn merge_eof(docs: &[&Document<'_>; 3], out: &mut Document<'static>) -> Option<u32> {
    let eof = docs.map(|d| d.eof_comments().collect::<Vec<_>>());
    match pick3(|x, y| eof[x] == eof[y]) {
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
    }
}

/// Returns whether `doc` keeps the spec §4 kind rules (`E025`).
fn kind_rules_hold(doc: &Document<'_>) -> bool {
    let mut diags = Vec::new();
    check_kind_rules(doc, &mut diags);
    assert!(diags.iter().all(|d| d.code == Code::E025));
    diags.is_empty()
}

fn decide(docs: &[&Document<'_>; 3], recs: [u32; 3], picks: &mut Picks) -> Outcome {
    let [ro, ra, rb] = recs;
    assert!(
        recs.iter()
            .zip(docs)
            .all(|(&r, d)| r == NONE || r < d.record_count())
    );
    let eq = |x: usize, y: usize| record_eq(docs[x], recs[x], docs[y], recs[y]);
    let conflict = |ours, theirs| Outcome::Conflict { ours, theirs };
    match (ro != NONE, ra != NONE, rb != NONE) {
        (true, true, true) => match pick3(eq) {
            Some(s) => Outcome::Take(s),
            None => merge_record(docs, recs, picks).map_or(conflict(ra, rb), Outcome::Merged),
        },
        (true, true, false) if eq(O, A) => Outcome::Drop,
        (true, true, false) => conflict(ra, NONE),
        (true, false, true) if eq(O, B) => Outcome::Drop,
        (true, false, true) => conflict(NONE, rb),
        (true, false, false) => Outcome::Drop,
        (false, true, false) => Outcome::Take(A as u8),
        (false, false, true) => Outcome::Take(B as u8),
        (false, true, true) if eq(A, B) => Outcome::Take(A as u8),
        (false, true, true) => conflict(ra, rb),
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
    let (d, r) = (docs, recs);
    let fields = pick3(|x, y| d[x].fields(r[x]).eq(d[y].fields(r[y])))?;
    let comments = pick3(|x, y| d[x].comments(r[x]).eq(d[y].comments(r[y])))?;
    let lines: Vec<(u8, u32)> = match pick3(|x, y| d[x].verbatim(r[x]).eq(d[y].verbatim(r[y]))) {
        Some(s) => (0..d[s as usize].verbatim(r[s as usize]).len() as u32)
            .map(|i| (s, i))
            .collect(),
        None => {
            let block = |s: usize| d[s].verbatim(r[s]).collect::<Vec<&str>>();
            diff3(&block(O), &block(A), &block(B))?
        }
    };
    let attrs = merge_attrs(docs, recs)?;
    let start = picks.attrs.len() as u32;
    picks.attrs.extend(attrs);
    // Lines are picked last, so a record that conflicts leaves no picks behind.
    let first_line = picks.lines.len() as u32;
    for (side, i) in lines {
        let base = d[side as usize].rec_verbatim_start[r[side as usize] as usize];
        picks.lines.push((side, base + i));
    }
    Some(MergedRec {
        fields,
        comments,
        attrs: (start, picks.attrs.len() as u32),
        lines: (first_line, picks.lines.len() as u32),
    })
}

/// Merges the attribute lines of one record: each line by the pick rule, adds and
/// deletes like records, then their order. Returns `(side, attribute index)` in merged
/// order, or `None` on a conflict.
fn merge_attrs(docs: &[&Document<'_>; 3], recs: [u32; 3]) -> Option<Vec<(u8, u32)>> {
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
        flags[i] = presence(ids) | if side.is_some() { KEEP } else { 0 };
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
    let picked: Vec<(u8, u32)> = order
        .into_iter()
        .map(|i| {
            let side = take[i as usize];
            (side, attr_ids[i as usize][side as usize])
        })
        .collect();
    assert!(picked.iter().all(|&(s, a)| a != NONE && s <= B as u8));
    Some(picked)
}

/// Appends the merged form of one identity to `out`.
fn emit(
    out: &mut Assembly,
    docs: &[&Document<'_>; 3],
    recs: [u32; 3],
    outcome: Outcome,
    picks: &Picks,
) {
    let before = out.0.record_count();
    assert!(out.1.iter().all(|s| s.first + s.ours + s.theirs <= before));
    let doc = &mut out.0;
    match outcome {
        Outcome::Drop => {}
        Outcome::Take(s) => {
            doc.copy_record_any(docs[s as usize], recs[s as usize]);
        }
        Outcome::Merged(m) => {
            let (fs, cs) = (m.fields as usize, m.comments as usize);
            doc.begin_like(docs[A], recs[A]);
            doc.copy_comments(docs[cs], recs[cs]);
            doc.copy_fields(docs[fs], recs[fs]);
            for &(s, attr) in &picks.attrs[m.attrs.0 as usize..m.attrs.1 as usize] {
                doc.copy_attr(docs[s as usize], attr);
            }
            for &(s, line) in &picks.lines[m.lines.0 as usize..m.lines.1 as usize] {
                let d = docs[s as usize];
                doc.push_verbatim(d.text(d.verbatim[line as usize]));
            }
        }
        Outcome::Conflict { ours, theirs } => {
            let side = |r: u32| -> Vec<u32> { (r != NONE).then_some(r).into_iter().collect() };
            push_conflict(out, docs, &side(ours), &side(theirs));
        }
    }
}

/// Copies A's `ours` records then B's `theirs` records and records the conflict span.
fn push_conflict(out: &mut Assembly, docs: &[&Document<'_>; 3], ours: &[u32], theirs: &[u32]) {
    assert!(
        !ours.is_empty() || !theirs.is_empty(),
        "conflict without records"
    );
    let first = out.0.record_count();
    for &r in ours {
        out.0.copy_record_any(docs[A], r);
    }
    for &r in theirs {
        out.0.copy_record_any(docs[B], r);
    }
    out.1.push(ConflictSpan {
        first,
        ours: ours.len() as u32,
        theirs: theirs.len() as u32,
    });
    assert_eq!(
        out.0.record_count(),
        first + ours.len() as u32 + theirs.len() as u32
    );
}
