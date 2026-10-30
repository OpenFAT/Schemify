//! Two-way record diff: documents a, b → the records added, removed and changed, with
//! the components and attribute keys that changed (spec §8 "Diff").
//!
//! Contents:
//! - `pub enum ChangeKind`, `pub struct Parts`, `Parts::{FIELDS, ATTRS, VERBATIM, COMMENTS}`,
//!   `Parts::contains`, `pub struct Change`,
//!   `pub struct Diff<'doc>`
//! - `pub fn diff<'doc>(a: &'doc Document<'_>, b: &'doc Document<'_>) -> Diff<'doc>`

use crate::document::{Document, NONE};
use crate::identity;
use std::ops::Range;

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
pub struct Diff<'doc> {
    /// One entry per differing record.
    pub changes: Vec<Change>,
    /// Changed attribute words, shared by all changes; borrowed from the documents.
    pub keys: Vec<&'doc str>,
}

/// Lists the records added, removed and changed from `a` to `b`, matched by identity
/// as in [`merge3`](crate::merge3). Symbol graphics (no identity) are matched by occurrence within
/// their kind, so inserting one shows the later ones as changed. Record order changes
/// alone are not reported.
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
/// use schemify_format as format;
/// use schemify_format::{ChangeKind, Parts};
/// let a = format::parse(b"schemify 1 shbench\nlabel 0abc 0 0 R0 vin\nwire 1abc 0 0 10 0\n")?;
/// let b = format::parse(b"schemify 1 shbench\nlabel 0abc 0 0 R0 vbias\n")?;
/// let d = format::diff(&a, &b);
/// assert_eq!(d.changes.len(), 2);
/// assert_eq!(d.changes[0].kind, ChangeKind::Removed); // wire: kinds come in listed order
/// assert_eq!(d.changes[1].kind, ChangeKind::Changed); // label text
/// assert!(d.changes[1].parts.contains(Parts::FIELDS));
/// # Ok::<(), schemify_format::ParseError>(())
/// ```
#[must_use]
pub fn diff<'doc>(a: &'doc Document<'_>, b: &'doc Document<'_>) -> Diff<'doc> {
    assert!(a.record_count() >= 1, "diff input lacks a header");
    assert!(b.record_count() >= 1, "diff input lacks a header");
    let docs = [a, b];
    let mut idents = vec![[0, 0, NONE]];
    idents.extend(identity::join(&docs));
    let mut out = Diff {
        changes: Vec::new(),
        keys: Vec::new(),
    };
    for recs in idents {
        let (ra, rb) = (recs[0], recs[1]);
        assert_eq!(recs[2], NONE, "a two-way join fills two sides");
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
            for ids in identity::join_attrs(&docs, [ra, rb, NONE]) {
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
            .all(|c| (c.kind == ChangeKind::Changed) == (c.parts.0 != 0)),
        "parts are set exactly on changed records"
    );
    assert!(
        out.changes
            .iter()
            .all(|c| c.keys.end as usize <= out.keys.len())
    );
    out
}

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
    #[must_use]
    pub fn contains(self, other: Parts) -> bool {
        self.0 & other.0 == other.0
    }
}
