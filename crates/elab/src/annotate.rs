//! The annotation overlay (ARCHITECTURE § Unifying abstractions): one table of marks
//! keyed by hierarchical path or object id, fed by `annotation_requests` from checks,
//! plugins (`contribute annotations`), OP back-annotation and LVS cross-probing.
//!
//! Immediate mode, like plugin contributions: a request carries a source's complete
//! set, which replaces everything that source contributed before; an empty set
//! removes the source. A source that knows exactly what changed (the editor's live
//! checks, from `CellState::changes`) patches its marks instead ([`Annotations::patch`]).

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
    /// Cost: O(rows + new marks) per request plus one sort; every mark is cloned (see
    /// [`Annotations::replace`] for a request the caller gives up).
    pub fn apply(&mut self, requests: &[AnnotationRequest]) {
        for req in requests {
            self.remove_source(&req.source);
            for m in &req.marks {
                if self.marks.len() >= ANNOTATIONS_MAX {
                    break;
                }
                self.source.push(req.source.clone());
                self.marks.push(m.clone());
            }
        }
        self.sort();
    }

    /// [`Annotations::apply`] of one request, moving its marks in instead of cloning them
    /// (the editor posts tens of thousands of check marks on a large cell).
    ///
    /// Cost: O(rows + new marks) plus one sort.
    pub fn replace(&mut self, req: AnnotationRequest) {
        self.remove_source(&req.source);
        let room = ANNOTATIONS_MAX.saturating_sub(self.marks.len());
        let n = req.marks.len().min(room);
        self.source.extend(std::iter::repeat_n(req.source, n));
        self.marks.extend(req.marks.into_iter().take(n));
        self.sort();
    }

    /// Removes the marks `gone` of `source` and adds `new` ones, keeping the overlay sorted:
    /// the incremental form of [`Annotations::replace`] when a source changed a few marks
    /// (`gone` must be marks the source has; a missing one is ignored). Marks past
    /// [`ANNOTATIONS_MAX`] are dropped.
    ///
    /// Cost: O(changes · log rows) comparisons to place the changes, then one pass moving
    /// the existing rows (no clone, no compare, no sort of them).
    pub fn patch(&mut self, source: &str, gone: Vec<Mark>, mut new: Vec<Mark>) {
        let order = |a: &Mark, b: &Mark| {
            (&a.target, a.severity, &a.text).cmp(&(&b.target, b.severity, &b.text))
        };
        new.sort_by(order);
        let (before, added) = (self.marks.len(), new.len());
        // Row `i` sorts before change `x` (of `source`).
        let below = |this: &Annotations, i: usize, x: &Mark| {
            let m = &this.marks[i];
            (&m.target, this.source[i].as_str(), m.severity, &m.text)
                < (&x.target, source, x.severity, &x.text)
        };
        let first = |this: &Annotations, x: &Mark| {
            let (mut lo, mut hi) = (0, this.marks.len());
            while lo < hi {
                let mid = (lo + hi) / 2;
                if below(this, mid, x) {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        // Rows to drop: the first equal, not yet dropped row of each gone mark.
        let mut drop: Vec<usize> = Vec::with_capacity(gone.len());
        for x in &gone {
            let mut i = first(self, x);
            while i < self.marks.len()
                && (self.source[i] != source || self.marks[i] != *x || drop.contains(&i))
                && self.marks[i].target == x.target
            {
                i += 1;
            }
            if i < self.marks.len() && self.source[i] == source && self.marks[i] == *x {
                drop.push(i);
            }
        }
        drop.sort_unstable();
        // Where each new mark goes: before the first row it does not follow.
        let at: Vec<usize> = new.iter().map(|x| first(self, x)).collect();
        debug_assert!(at.windows(2).all(|w| w[0] <= w[1]));
        let removed = drop.len();
        let size = before - removed + added;
        let rows = std::mem::replace(&mut self.source, Vec::with_capacity(size))
            .into_iter()
            .zip(std::mem::replace(&mut self.marks, Vec::with_capacity(size)));
        let (mut d, mut n) = (
            drop.into_iter().peekable(),
            at.into_iter().zip(new).peekable(),
        );
        for (i, (s, m)) in rows.enumerate() {
            while let Some((_, x)) = n.next_if(|(a, _)| *a <= i) {
                self.source.push(source.to_owned());
                self.marks.push(x);
            }
            if d.next_if_eq(&i).is_some() {
                continue;
            }
            self.source.push(s);
            self.marks.push(m);
        }
        for (_, x) in n {
            self.source.push(source.to_owned());
            self.marks.push(x);
        }
        self.source.truncate(ANNOTATIONS_MAX);
        self.marks.truncate(ANNOTATIONS_MAX);
        assert_eq!(self.source.len(), self.marks.len());
        assert_eq!(
            self.marks.len(),
            (before - removed + added).min(ANNOTATIONS_MAX)
        );
        debug_assert!(
            self.marks
                .windows(2)
                .zip(self.source.windows(2))
                .all(|(m, s)| {
                    (&m[0].target, &s[0], m[0].severity, &m[0].text)
                        <= (&m[1].target, &s[1], m[1].severity, &m[1].text)
                })
        );
    }

    fn remove_source(&mut self, source: &str) {
        let mut keep = self.source.iter().map(|s| s != source);
        self.marks
            .retain(|_| keep.next().expect("parallel columns"));
        self.source.retain(|s| s != source);
        assert_eq!(self.source.len(), self.marks.len());
    }

    fn sort(&mut self) {
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
        // `replace` moves the request in and agrees with `apply`.
        let mut r = a.clone();
        a.apply(&[req("op", vec![mark("/X1", "1.2V"), mark("/X0", "0V")])]);
        r.replace(req("op", vec![mark("/X1", "1.2V"), mark("/X0", "0V")]));
        assert_eq!((&a.source, &a.marks), (&r.source, &r.marks));
        r.replace(req("checks", vec![]));
        assert_eq!(r.source, ["op", "op"]);
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
            a.apply(&[
                AnnotationRequest {
                    source: "checks".into(),
                    marks: checks.clone(),
                },
                AnnotationRequest {
                    source: "op".into(),
                    marks: op,
                },
            ]);
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
            assert_eq!((a.source, a.marks), (b.source, b.marks));
        }
    }
}
