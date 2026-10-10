//! The annotation overlay (ARCHITECTURE § Unifying abstractions): one table of marks
//! keyed by hierarchical path or object id, fed by `annotation_requests` from checks,
//! plugins (`contribute annotations`), OP back-annotation and LVS cross-probing.
//!
//! Immediate mode, like plugin contributions: a request carries a source's complete
//! set, which replaces everything that source contributed before; an empty set
//! removes the source. Nobody diffs.

use crate::check::{Severity, Violations};
use crate::names::Names;

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

/// The overlay, sorted by (target, source, severity, text) so render and plugins see a
/// deterministic order.
#[derive(Clone, Debug, Default)]
pub struct Annotations {
    /// Source of each row.
    pub source: Vec<String>,
    /// The marks, parallel to `source`.
    pub marks: Vec<Mark>,
}

impl Annotations {
    /// Applies requests in order (the frame spine's `elab::apply_annotations`): each
    /// replaces its source's marks. Marks past [`ANNOTATIONS_MAX`] are dropped.
    ///
    /// Cost: O(rows + new marks) per request plus one sort.
    pub fn apply(&mut self, requests: &[AnnotationRequest]) {
        for req in requests {
            let mut keep = self.source.iter().map(|s| *s != req.source);
            self.marks
                .retain(|_| keep.next().expect("parallel columns"));
            self.source.retain(|s| *s != req.source);
            for m in &req.marks {
                if self.marks.len() >= ANNOTATIONS_MAX {
                    break;
                }
                self.source.push(req.source.clone());
                self.marks.push(m.clone());
            }
        }
        let mut rows: Vec<(String, Mark)> =
            self.source.drain(..).zip(self.marks.drain(..)).collect();
        rows.sort_by(|a, b| {
            (&a.1.target, &a.0, a.1.severity, &a.1.text).cmp(&(
                &b.1.target,
                &b.0,
                b.1.severity,
                &b.1.text,
            ))
        });
        (self.source, self.marks) = rows.into_iter().unzip();
        assert_eq!(self.source.len(), self.marks.len());
    }

    /// Returns the marks on `target`.
    pub fn on<'a>(&'a self, target: &'a str) -> impl Iterator<Item = &'a Mark> + 'a {
        self.marks.iter().filter(move |m| m.target == target)
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
            let target = if r.path != crate::names::Name::NONE {
                names.get(r.path).to_owned()
            } else if r.object != schemify_doc::ObjId::NONE {
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
        .collect();
    AnnotationRequest {
        source: "checks".to_owned(),
        marks,
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
        a.apply(&[
            req("checks", vec![mark("/X1", "a"), mark("0abcde", "b")]),
            req("op", vec![mark("/X1", "1.2V")]),
        ]);
        assert_eq!(a.marks.len(), 3);
        assert_eq!(a.on("/X1").count(), 2);
        a.apply(&[req("checks", vec![mark("/X2", "c")])]);
        assert_eq!(a.marks.len(), 2);
        assert_eq!(
            a.on("/X1").map(|m| m.text.as_str()).collect::<Vec<_>>(),
            ["1.2V"]
        );
        a.apply(&[req("op", vec![])]);
        assert_eq!(a.source, ["checks"]);
        // Order does not depend on request order.
        let mut b = Annotations::default();
        b.apply(&[
            req("z", vec![mark("/A", "1")]),
            req("a", vec![mark("/A", "1")]),
        ]);
        let mut c = Annotations::default();
        c.apply(&[
            req("a", vec![mark("/A", "1")]),
            req("z", vec![mark("/A", "1")]),
        ]);
        assert_eq!((b.source, b.marks), (c.source, c.marks));
    }
}
