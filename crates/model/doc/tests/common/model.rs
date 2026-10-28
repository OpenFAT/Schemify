//! The naive model the stateful property tests compare against: records as owned text
//! per table, linear searches, no indexes, rejections as the code the first failing
//! check would report.
//!
//! Contents:
//! - `pub struct Model`: `from_doc`, `canonical`, `apply_op`, `apply_tx`
//! - `pub type Reject`

use schemify_doc::{Attr, Document, Op, RecordBuf, TableId, Target};
use schemify_format::{Document as FormatDoc, FileKind, KeyRule, RecordKind, write};
use std::collections::BTreeMap;

/// The naive model: records as owned text per table, linear searches, no indexes.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub kind: FileKind,
    pub header: Vec<Attr>,
    pub tables: BTreeMap<TableId, Vec<RecordBuf>>,
}

/// The model's rejection: the code the first failing check would report.
pub type Reject = &'static str;

impl Model {
    pub fn from_doc(doc: &Document) -> Model {
        let text = doc.canonical();
        let f = schemify_format::parse(text.as_bytes()).unwrap();
        let mut m = Model {
            kind: f.kind(),
            header: RecordBuf::from_format(&f, 0).attrs,
            tables: BTreeMap::new(),
        };
        for r in 1..f.record_count() {
            let rec = RecordBuf::from_format(&f, r);
            m.tables
                .entry(TableId::of(rec.kind).unwrap())
                .or_default()
                .push(rec);
        }
        m
    }

    pub fn canonical(&self) -> String {
        let mut d = FormatDoc::new(self.kind);
        for a in &self.header {
            d.push_attr(&a.key);
            for v in &a.values {
                d.push_value(v);
            }
        }
        for recs in self.tables.values() {
            for r in recs {
                d.begin_record(r.kind);
                for c in &r.comments {
                    d.push_comment(c);
                }
                for f in &r.fields {
                    d.push_field(f);
                }
                for a in &r.attrs {
                    d.push_attr(&a.key);
                    for v in &a.values {
                        d.push_value(v);
                    }
                }
                for v in &r.verbatim {
                    d.push_verbatim(v);
                }
            }
        }
        write(&d)
    }

    /// Applies one op; on `Err` the model may be partially changed (callers clone).
    pub fn apply_op(&mut self, op: &Op) -> Result<(), Reject> {
        match op {
            Op::Insert { rec, at } => {
                let table = TableId::of(rec.kind).ok_or("E027")?;
                match rec.kind.key() {
                    KeyRule::Id if self.id_taken(&rec.fields[0]) => return Err("E041"),
                    KeyRule::Name if self.name_taken(rec.kind, &rec.fields[0], None) => {
                        return Err("E024");
                    }
                    _ => {}
                }
                let recs = self.tables.entry(table).or_default();
                let pos = match at {
                    Some(_) if !table.ordered() => return Err("E043"),
                    Some(p) if *p as usize > recs.len() => return Err("E043"),
                    Some(p) => *p as usize,
                    None => recs
                        .iter()
                        .rposition(|r| r.kind == rec.kind)
                        .map_or(recs.len(), |i| i + 1),
                };
                let mut rec = rec.clone();
                normalize(&mut rec);
                recs.insert(pos, rec);
                Ok(())
            }
            Op::Delete(t) => {
                let (table, row) = self.find(t).ok_or("E040")?;
                self.tables.get_mut(&table).unwrap().remove(row);
                Ok(())
            }
            Op::SetField {
                target,
                index,
                value,
            } => {
                let (table, row) = self.find(target).ok_or("E040")?;
                let mut rec = self.tables[&table][row].clone();
                let i = *index as usize;
                if i >= rec.fields.len() || (i == 0 && rec.kind.key() == KeyRule::Id) {
                    return Err("E043");
                }
                rec.fields[i] = value.clone();
                self.put(table, row, rec)
            }
            Op::SetAttr {
                target,
                key,
                values,
            } => {
                let set = |attrs: &mut Vec<Attr>| match attrs.iter().position(|a| a.key == *key) {
                    Some(i) => attrs[i].values = values.clone(),
                    None => attrs.push(Attr {
                        key: key.clone(),
                        values: values.clone(),
                    }),
                };
                if target.kind == RecordKind::Header {
                    set(&mut self.header);
                    return Ok(());
                }
                let (table, row) = self.find(target).ok_or("E040")?;
                let mut rec = self.tables[&table][row].clone();
                set(&mut rec.attrs);
                self.put(table, row, rec)
            }
            Op::RemoveAttr { target, key, .. } => {
                let (table, row) = self.find(target).ok_or("E040")?;
                let mut rec = self.tables[&table][row].clone();
                let before = rec.attrs.len();
                rec.attrs.retain(|a| a.key != *key);
                if rec.attrs.len() == before {
                    return Err("E040");
                }
                self.put(table, row, rec)
            }
            Op::Reorder { target, to } => {
                let (table, row) = self.find(target).ok_or("E040")?;
                let recs = self.tables.get_mut(&table).unwrap();
                if !table.ordered() || *to as usize >= recs.len() {
                    return Err("E043");
                }
                let r = recs.remove(row);
                recs.insert(*to as usize, r);
                Ok(())
            }
            Op::Put { .. } => unreachable!("the generator makes no puts"),
        }
    }

    /// Applies a transaction all-or-nothing; returns whether it was accepted.
    pub fn apply_tx(&mut self, ops: &[Op]) -> bool {
        let mut next = self.clone();
        for op in ops {
            if next.apply_op(op).is_err() {
                return false;
            }
        }
        let zero_wire = next
            .tables
            .get(&TableId::Wires)
            .into_iter()
            .flatten()
            .any(|w| w.fields[1] == w.fields[3] && w.fields[2] == w.fields[4]);
        if zero_wire {
            return false;
        }
        *self = next;
        true
    }
}

// ---- private ----

/// Wire fields are kept with `(x1, y1) < (x2, y2)` (format spec §4), so `set_field`
/// indices address the normalized endpoints.
fn normalize(rec: &mut RecordBuf) {
    if rec.kind != RecordKind::Wire {
        return;
    }
    let c = |i: usize| schemify_format::parse_coord(&rec.fields[i]).unwrap();
    if (c(3), c(4)) < (c(1), c(2)) {
        rec.fields.swap(1, 3);
        rec.fields.swap(2, 4);
    }
}

impl Model {
    fn find(&self, t: &Target) -> Option<(TableId, usize)> {
        let table = TableId::of(t.kind)?;
        let recs = self.tables.get(&table)?;
        let row = match t.kind.key() {
            KeyRule::Position => t.key.parse::<usize>().ok().filter(|&r| r < recs.len())?,
            _ => recs.iter().position(|r| r.fields[0] == t.key)?,
        };
        (recs[row].kind == t.kind).then_some((table, row))
    }

    fn id_taken(&self, id: &str) -> bool {
        self.tables
            .values()
            .flatten()
            .any(|r| r.kind.key() == KeyRule::Id && r.fields[0] == id)
    }

    fn name_taken(&self, kind: RecordKind, name: &str, except: Option<usize>) -> bool {
        let Some(recs) = self.tables.get(&TableId::of(kind).unwrap()) else {
            return false;
        };
        recs.iter()
            .enumerate()
            .any(|(i, r)| Some(i) != except && r.kind == kind && r.fields[0] == name)
    }

    fn put(&mut self, table: TableId, row: usize, mut rec: RecordBuf) -> Result<(), Reject> {
        if rec.kind.key() == KeyRule::Name && self.name_taken(rec.kind, &rec.fields[0], Some(row)) {
            return Err("E024");
        }
        if rec.kind == RecordKind::Inst
            && (rec.find_attr("at").is_none() || rec.find_attr("name").is_none())
        {
            return Err("E029");
        }
        normalize(&mut rec);
        self.tables.get_mut(&table).unwrap()[row] = rec;
        Ok(())
    }
}
