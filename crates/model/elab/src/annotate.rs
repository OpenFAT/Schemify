//! Annotation requests in, the overlay out (ARCHITECTURE § Unifying abstractions): one
//! table of marks keyed by hierarchical path or object id, fed by `annotation_requests`
//! from checks, plugins (`contribute annotations`), OP back-annotation and LVS
//! cross-probing. No spec clause: the overlay is ARCHITECTURE's.
//!
//! Immediate mode, like plugin contributions: a request carries a source's complete
//! set, which replaces everything that source contributed before; an empty set
//! removes the source. A source that knows exactly what changed (the editor's live
//! checks, from `CellState::changes`) patches its marks instead ([`Annotations::patch`]).
//!
//! The table is an ordered map (target, source, severity, text) → count, so a patch of a few
//! marks costs O(changes · log rows) on an overlay of 10^5 marks (a 100k-object cell with
//! floating pins), and every reader sees the same deterministic order.
//!
//! Contents:
//! - `pub const ANNOTATIONS_MAX`
//! - `pub struct Mark`, `pub struct AnnotationRequest`
//! - `pub struct Annotations`: `replace`, `patch`, `iter`, `on`, `on_by`, `has_source`,
//!   `count_of`, `len`, `is_empty`
//! - `pub fn violation_marks(v: &Violations, names: &Names, ids) -> AnnotationRequest`

use crate::names::{Name, Names};
use crate::violations::{Severity, Violations};
use schemify_doc::ObjId;
use std::collections::BTreeMap;

/// Most marks the overlay holds (ARCHITECTURE capacity).
pub const ANNOTATIONS_MAX: usize = 1 << 20;

/// One mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    /// `/X1/M3` (a hierarchical path, starts with `/`) or an object id (`0gsrp0`).
    pub target: String,
    /// How it is drawn.
    pub severity: Severity,
    /// Text shown beside the target.
    pub text: String,
}

/// A source's complete set of marks (one `annotation_requests` row).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationRequest {
    /// Who contributes: `checks`, `op`, `plugin:<name>:<scope>`, …
    pub source: String,
    /// The full set; replaces the source's previous marks.
    pub marks: Vec<Mark>,
}

/// The overlay: every mark with its source, sorted by (target, source, severity, text) so
/// render and plugins see a deterministic order. Equal marks of one source are kept as a
/// count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Annotations {
    rows: BTreeMap<Row, u32>,
    /// Marks per source.
    sources: BTreeMap<String, usize>,
    /// Marks in all (counts included).
    len: usize,
}

/// One row of the overlay, ordered by (target, source, severity, text).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    mark: Mark,
    source: String,
}

impl Annotations {
    /// Replaces the marks of `req.source` by `req.marks`, moved in (the editor posts tens
    /// of thousands of check marks on a large cell). Marks past [`ANNOTATIONS_MAX`] are
    /// dropped.
    ///
    /// Cost: O(rows) to drop the source's old marks when it has some, plus O(new marks ·
    /// log rows).
    ///
    /// # Panics
    ///
    /// Panics only on a broken internal invariant: the source's count out of step with its
    /// rows.
    pub fn replace(&mut self, req: AnnotationRequest) {
        self.remove_source(&req.source);
        let wanted = req.marks.len();
        for m in req.marks {
            self.add(&req.source, m);
        }
        assert!(
            self.count_of(&req.source) <= wanted,
            "a source holds only its own set"
        );
        assert!(self.len <= ANNOTATIONS_MAX);
    }

    /// Removes the marks `gone` of `source` and adds `new` ones: the incremental form of
    /// [`Annotations::replace`] when a source changed a few marks (`gone` must be marks the
    /// source has; a missing one is ignored). Marks past [`ANNOTATIONS_MAX`] are dropped.
    ///
    /// Cost: O(changes · log rows); the other rows are not touched.
    ///
    /// # Panics
    ///
    /// Panics only on a broken internal invariant: a row whose source has no count, or a
    /// mark count outside `before - gone ..= before + new`.
    pub fn patch(&mut self, source: &str, gone: Vec<Mark>, new: Vec<Mark>) {
        let before = self.len;
        let (g, n) = (gone.len(), new.len());
        for mark in gone {
            let row = Row {
                mark,
                source: source.to_owned(),
            };
            let Some(c) = self.rows.get_mut(&row) else {
                continue;
            };
            *c -= 1;
            if *c == 0 {
                self.rows.remove(&row);
            }
            self.len -= 1;
            let left = self
                .sources
                .get_mut(source)
                .expect("a row's source is counted");
            *left -= 1;
            if *left == 0 {
                self.sources.remove(source);
            }
        }
        for m in new {
            self.add(source, m);
        }
        assert!(
            self.len + g >= before,
            "removed more marks than `gone` lists"
        );
        assert!(self.len <= before + n, "added more marks than `new` lists");
        assert!(self.len <= ANNOTATIONS_MAX);
    }

    /// Every mark with its source, in (target, source, severity, text) order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Mark)> + '_ {
        self.rows
            .iter()
            .flat_map(|(r, &c)| std::iter::repeat_n((r.source.as_str(), &r.mark), c as usize))
    }

    /// The marks on `target`, every source's, in order.
    ///
    /// Cost: O(log rows + marks on it).
    pub fn on<'a>(&'a self, target: &str) -> impl Iterator<Item = &'a Mark> + 'a {
        self.range_of(target, None)
    }

    /// The marks `source` has on `target`, in order.
    ///
    /// Cost: O(log rows + marks on the target).
    pub fn on_by<'a>(&'a self, target: &str, source: &str) -> impl Iterator<Item = &'a Mark> + 'a {
        self.range_of(target, Some(source))
    }

    /// Whether `source` has a mark.
    #[must_use]
    pub fn has_source(&self, source: &str) -> bool {
        let count = self.sources.get(source);
        debug_assert!(count.is_none_or(|&n| n > 0), "a counted source has a mark");
        count.is_some()
    }

    /// How many marks `source` has.
    #[must_use]
    pub fn count_of(&self, source: &str) -> usize {
        let count = self.sources.get(source).copied().unwrap_or(0);
        debug_assert!(count <= self.len, "a source's marks are among all marks");
        count
    }

    /// Number of marks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether there is no mark.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        debug_assert_eq!(self.len == 0, self.rows.is_empty(), "rows hold every mark");
        self.len == 0
    }
}

impl Ord for Row {
    fn cmp(&self, o: &Row) -> std::cmp::Ordering {
        let (a, b) = (&self.mark, &o.mark);
        (&a.target, &self.source, a.severity, &a.text)
            .cmp(&(&b.target, &o.source, b.severity, &b.text))
    }
}

impl PartialOrd for Row {
    fn partial_cmp(&self, o: &Row) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

/// Turns violations into the `checks` source's request: one mark per violation on its
/// path when it has one, else on its object id; findings about neither (a net name,
/// a request problem) target the cell name.
pub fn violation_marks(
    v: &Violations,
    names: &Names,
    ids: impl Fn(u32) -> String,
) -> AnnotationRequest {
    let marks = v
        .rows
        .iter()
        .map(|r| {
            let target = if r.path != Name::NONE {
                names.get(r.path).to_owned()
            } else if r.object != ObjId::NONE {
                ids(r.object.0)
            } else {
                names.get(r.cell).to_owned()
            };
            Mark {
                target,
                severity: r.code.severity(),
                text: format!("{} {}", r.code.as_str(), r.detail),
            }
        })
        .collect::<Vec<Mark>>();
    AnnotationRequest {
        source: "checks".to_owned(),
        marks,
    }
}

// ---- private ----

impl Annotations {
    fn add(&mut self, source: &str, mark: Mark) {
        if self.len >= ANNOTATIONS_MAX {
            return;
        }
        let row = Row {
            mark,
            source: source.to_owned(),
        };
        *self.rows.entry(row).or_insert(0) += 1;
        let count = self.sources.entry(source.to_owned()).or_insert(0);
        *count += 1;
        assert!(
            *count <= ANNOTATIONS_MAX,
            "a source's marks are capped with the table"
        );
        self.len += 1;
        assert!(self.len <= ANNOTATIONS_MAX);
    }

    fn remove_source(&mut self, source: &str) {
        let Some(n) = self.sources.remove(source) else {
            return;
        };
        assert!(n > 0, "a counted source has a mark");
        assert!(n <= self.len, "a source's marks are among all marks");
        self.rows.retain(|r, _| r.source != source);
        self.len -= n;
        debug_assert_eq!(
            self.len,
            self.rows.values().map(|&c| c as usize).sum::<usize>()
        );
    }

    /// The marks on `target`: `source`'s only, or every source's for `None`.
    fn range_of<'a>(
        &'a self,
        target: &str,
        source: Option<&str>,
    ) -> impl Iterator<Item = &'a Mark> + 'a {
        let from = Row {
            mark: Mark {
                target: target.to_owned(),
                severity: Severity::Error, // the least severity: the range starts at the first
                text: String::new(),
            },
            // "" sorts first: every source's rows on the target follow it.
            source: source.unwrap_or("").to_owned(),
        };
        let (t, src) = (target.to_owned(), source.map(str::to_owned));
        self.rows
            .range(from..)
            .take_while(move |(r, _)| {
                r.mark.target == t && src.as_ref().is_none_or(|s| r.source == *s)
            })
            .flat_map(|(r, &c)| std::iter::repeat_n(&r.mark, c as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(t: &str, text: &str) -> Mark {
        Mark {
            target: t.to_owned(),
            severity: Severity::Warning,
            text: text.to_owned(),
        }
    }

    #[test]
    fn requests_replace_their_source_only() {
        let mut a = Annotations::default();
        let req = |s: &str, m: Vec<Mark>| AnnotationRequest {
            source: s.to_owned(),
            marks: m,
        };
        a.replace(req("checks", vec![mark("/X1", "a"), mark("0abcde", "b")]));
        a.replace(req("op", vec![mark("/X1", "1.2V")]));
        assert_eq!(a.len(), 3);
        assert_eq!(a.on("/X1").count(), 2);
        a.replace(req("checks", vec![mark("/X2", "c")]));
        assert_eq!(a.len(), 2);
        assert_eq!(
            a.on("/X1").map(|m| m.text.as_str()).collect::<Vec<_>>(),
            ["1.2V"]
        );
        a.replace(req("op", vec![]));
        assert!(a.has_source("checks") && !a.has_source("op"));
        let mut r = a.clone();
        r.replace(req("op", vec![mark("/X1", "1.2V"), mark("/X0", "0V")]));
        r.replace(req("checks", vec![]));
        assert_eq!(r.iter().map(|m| m.0).collect::<Vec<_>>(), ["op", "op"]);
        // Order does not depend on request order.
        let mut b = Annotations::default();
        b.replace(req("z", vec![mark("/A", "1")]));
        b.replace(req("a", vec![mark("/A", "1")]));
        let mut c = Annotations::default();
        c.replace(req("a", vec![mark("/A", "1")]));
        c.replace(req("z", vec![mark("/A", "1")]));
        assert_eq!(b, c);
    }

    /// `patch` with a source's removed and added marks equals `replace` with its full new set,
    /// for random overlays (several sources, duplicate targets) and random changes.
    #[test]
    fn patch_equals_replace() {
        let mut seed = 0x2545_f491_u64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..2000 {
            let m = |rnd: &mut dyn FnMut(u64) -> u64| {
                let sev = [Severity::Error, Severity::Warning, Severity::Info][rnd(3) as usize];
                Mark {
                    target: format!("t{}", rnd(6)),
                    severity: sev,
                    text: format!("x{}", rnd(4)),
                }
            };
            let checks: Vec<Mark> = (0..rnd(12)).map(|_| m(&mut rnd)).collect();
            let op: Vec<Mark> = (0..rnd(6)).map(|_| m(&mut rnd)).collect();
            let mut a = Annotations::default();
            a.replace(AnnotationRequest {
                source: "checks".into(),
                marks: checks.clone(),
            });
            a.replace(AnnotationRequest {
                source: "op".into(),
                marks: op,
            });
            // Drop some marks, add some new ones.
            let mut kept = checks.clone();
            let mut gone = Vec::new();
            for _ in 0..rnd(4) {
                if !kept.is_empty() {
                    gone.push(kept.remove(rnd(kept.len() as u64) as usize));
                }
            }
            let new: Vec<Mark> = (0..rnd(4)).map(|_| m(&mut rnd)).collect();
            kept.extend(new.iter().cloned());
            let mut b = a.clone();
            a.patch("checks", gone, new);
            b.replace(AnnotationRequest {
                source: "checks".into(),
                marks: kept,
            });
            assert_eq!(a, b);
            // The range lookups equal filters of the whole overlay.
            for t in (0..6).map(|k| format!("t{k}")) {
                for src in ["checks", "op"] {
                    let by: Vec<&Mark> = a.on_by(&t, src).collect();
                    let scan: Vec<&Mark> = a
                        .iter()
                        .filter(|(s, m)| *s == src && m.target == t)
                        .map(|x| x.1)
                        .collect();
                    assert_eq!(by, scan);
                }
                assert_eq!(
                    a.on(&t).count(),
                    a.iter().filter(|x| x.1.target == t).count()
                );
            }
            let checks = a.iter().filter(|x| x.0 == "checks").count();
            assert_eq!(a.count_of("checks"), checks);
            assert_eq!(a.has_source("checks"), checks > 0);
        }
    }
}
