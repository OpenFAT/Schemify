//! `search`: the Search dialog (xschem `property_search`, Tools → Search) as two pure
//! transforms over one document — find the objects whose field matches, and replace the
//! matched text everywhere in one transaction.
//!
//! Fields: `name` (instance names), `cell` (instance cell references), `net` (label and
//! port texts), `text` (annotation texts), `*` (all of those), or any other word: that
//! instance attribute (a parameter override, `ignore`, …). Matching is exact or substring,
//! case-sensitive.
//!
//! Data design: in, one document's rank-3 tables (≤ 2^22 rows per kind) and the query;
//! out, object keys or ops. One linear pass per kind; nothing kept.

use schemify_doc::{Document, Op, TableId, Target};
use schemify_format::RecordKind;

use crate::scene::ObjKey;

/// A search: which field, the value to find, exact or substring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    /// `name`, `cell`, `net`, `text`, `*`, or an instance attribute key.
    pub field: String,
    /// Text to find (non-empty).
    pub value: String,
    /// Whole-value match instead of substring.
    pub exact: bool,
}

impl Query {
    fn hit(&self, text: &str) -> bool {
        if self.exact {
            text == self.value
        } else {
            text.contains(self.value.as_str())
        }
    }

    fn wants(&self, field: &str) -> bool {
        self.field == "*" || self.field == field
    }

    /// The replaced text: the whole value when exact, else every occurrence.
    fn replaced(&self, text: &str, with: &str) -> String {
        if self.exact {
            with.to_owned()
        } else {
            text.replace(self.value.as_str(), with)
        }
    }
}

/// One matched field: the object, how to write it back, and its current text.
struct Hit {
    key: ObjKey,
    kind: RecordKind,
    /// `None`: positional field `index`; `Some(attr)`: that attribute's first value.
    attr: Option<String>,
    index: u32,
    text: String,
}

fn hits(doc: &Document, q: &Query) -> Vec<Hit> {
    assert!(!q.value.is_empty(), "an empty search matches everything");
    let s = doc.strings();
    let mut out = Vec::new();
    let inst = doc.instances();
    for i in 0..inst.id.len() {
        let key = ObjKey {
            table: TableId::Instances,
            key: inst.id[i].0,
        };
        let name = s.get(inst.name[i]);
        if q.wants("name") && q.hit(name) {
            out.push(Hit {
                key,
                kind: RecordKind::Inst,
                attr: Some("name".into()),
                index: 0,
                text: name.to_owned(),
            });
        }
        let cell = s.get(inst.cell[i]);
        if q.wants("cell") && q.hit(cell) {
            out.push(Hit {
                key,
                kind: RecordKind::Inst,
                attr: None,
                index: 1,
                text: cell.to_owned(),
            });
        }
        for (k, v) in doc.pool().attrs(inst.attrs[i]) {
            let k = s.get(k);
            let Some(&v0) = v.first() else { continue };
            let v = s.get(v0);
            let named = q.field == k && !matches!(k, "name" | "at");
            let any = q.field == "*" && schemify_doc::Instances::is_override(k);
            if (named || any) && q.hit(v) {
                out.push(Hit {
                    key,
                    kind: RecordKind::Inst,
                    attr: Some(k.to_owned()),
                    index: 0,
                    text: v.to_owned(),
                });
            }
        }
    }
    let placed = [
        (TableId::Labels, RecordKind::Label, doc.labels(), "net"),
        (TableId::Ports, RecordKind::Port, doc.ports(), "net"),
        (TableId::Texts, RecordKind::Text, doc.texts(), "text"),
    ];
    for (table, kind, t, field) in placed {
        if !q.wants(field) {
            continue;
        }
        for i in 0..t.id.len() {
            let text = s.get(t.text[i]);
            if q.hit(text) {
                out.push(Hit {
                    key: ObjKey {
                        table,
                        key: t.id[i].0,
                    },
                    kind,
                    attr: None,
                    index: 4,
                    text: text.to_owned(),
                });
            }
        }
    }
    out
}

/// Keys of the objects with a matching field, sorted and distinct.
///
/// # Panics
///
/// Panics if `q.value` is empty.
pub fn find(doc: &Document, q: &Query) -> Vec<ObjKey> {
    let mut keys: Vec<ObjKey> = hits(doc, q).into_iter().map(|h| h.key).collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// Ops that replace every match of `q` with `with` (one transaction). A match whose
/// replacement equals its text writes nothing.
///
/// # Panics
///
/// Panics if `q.value` is empty.
pub fn replace(doc: &Document, q: &Query, with: &str) -> Vec<Op> {
    let s = doc.strings();
    let mut ops = Vec::new();
    for h in hits(doc, q) {
        let new = q.replaced(&h.text, with);
        if new == h.text {
            continue;
        }
        let Some(loc) = crate::edit::loc_of(doc, h.key) else {
            continue;
        };
        let id = match h.key.table {
            TableId::Instances => doc.instances().id[loc.row as usize],
            TableId::Labels => doc.labels().id[loc.row as usize],
            TableId::Ports => doc.ports().id[loc.row as usize],
            _ => doc.texts().id[loc.row as usize],
        };
        let target = Target::new(h.kind, &id.text(s));
        ops.push(match h.attr {
            Some(key) => Op::SetAttr {
                target,
                key,
                values: vec![new],
            },
            None => Op::SetField {
                target,
                index: h.index,
                value: new,
            },
        });
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_match_exact_or_substring() {
        let q = Query {
            field: "net".into(),
            value: "vin".into(),
            exact: false,
        };
        assert!(q.hit("vin_p") && q.wants("net") && !q.wants("name"));
        assert_eq!(q.replaced("vin_vin", "a"), "a_a");
        let e = Query { exact: true, ..q };
        assert!(!e.hit("vin_p") && e.hit("vin"));
        assert_eq!(e.replaced("vin", "x"), "x");
    }
}
