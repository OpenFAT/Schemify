//! Verbatim line merge (diff3): base, ours and theirs line blocks → merged lines as
//! [`Pick`]s of (side, line index) (spec §8 "Verbatim lines merge line by line").
//!
//! Contents:
//! - `pub(crate) fn diff3(o: &[&str], a: &[&str], b: &[&str]) -> Option<Vec<Pick>>`

use crate::document::NONE;
use crate::merge::{A, B, O, Pick};

/// Largest LCS table [`diff3`] aligns (cells = (base lines + 1) × (side lines + 1) after
/// the common prefix and suffix): 8 MiB of `u16`.
const ALIGN_CELLS_MAX: usize = 1 << 22;

/// Line-level three-way merge of one verbatim block (diff3). Lines of `o` that `a` and
/// `b` both keep, at matching positions, split the blocks into stable and changed runs;
/// a changed run takes the side that changed it (both identically: `a`'s). Returns the
/// merged lines as picks of (side, line index within that side's block), or `None` when both
/// sides changed one run differently or a block is too large to align
/// ([`ALIGN_CELLS_MAX`]).
///
/// Symmetric in `a` and `b`: the alignments `o`–`a` and `o`–`b` are computed alike and
/// independently. Cost: O(n) plus O(n·m) of the changed middles (bounded).
pub(crate) fn diff3(o: &[&str], a: &[&str], b: &[&str]) -> Option<Vec<Pick>> {
    let (ma, mb) = (align(o, a)?, align(o, b)?);
    assert_eq!(ma.len(), o.len(), "one match slot per base line");
    assert_eq!(mb.len(), o.len(), "one match slot per base line");
    let mut out: Vec<Pick> = Vec::with_capacity(a.len().max(b.len()));
    let (mut lo, mut la, mut lb) = (0usize, 0usize, 0usize);
    loop {
        // Stable run: base lines both sides keep at the next positions.
        while lo < o.len() && ma[lo] == la as u32 && mb[lo] == lb as u32 {
            out.push(Pick {
                side: O as u8,
                index: lo as u32,
            });
            (lo, la, lb) = (lo + 1, la + 1, lb + 1);
        }
        // The next base line both sides keep ends the changed run (or the blocks end).
        let sync = (lo..o.len()).find(|&j| ma[j] != NONE && mb[j] != NONE);
        let (eo, ea, eb) = match sync {
            Some(j) => (j, ma[j] as usize, mb[j] as usize),
            None => (o.len(), a.len(), b.len()),
        };
        assert!(eo >= lo, "alignments are monotone");
        assert!(ea >= la, "alignments are monotone");
        assert!(eb >= lb, "alignments are monotone");
        let (co, ca, cb) = (&o[lo..eo], &a[la..ea], &b[lb..eb]);
        let (side, from, to) = if ca == co {
            (B, lb, eb)
        } else if cb == co || ca == cb {
            (A, la, ea)
        } else {
            return None;
        };
        let run = (from..to).map(|i| Pick {
            side: side as u8,
            index: i as u32,
        });
        out.extend(run);
        (lo, la, lb) = (eo, ea, eb);
        if sync.is_none() {
            break;
        }
    }
    assert_eq!(lo, o.len(), "every base line consumed");
    assert_eq!(la, a.len(), "every line of ours consumed");
    assert_eq!(lb, b.len(), "every line of theirs consumed");
    Some(out)
}

// ---- private ----

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
    assert!(
        prefix + suffix <= o.len().min(x.len()),
        "prefix and suffix do not overlap"
    );
    if (n + 1) * (m + 1) > ALIGN_CELLS_MAX {
        return None;
    }
    assert!(n.min(m) < 1 << 11, "LCS lengths fit u16 under the cell cap");
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
