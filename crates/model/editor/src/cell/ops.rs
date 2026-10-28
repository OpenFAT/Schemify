//! The `doc` ops each submitted field or button makes: header, pin, param and impl fields,
//! bodies, port sync, added, deleted and moved rows. One transaction per field.
//!
//! Contents:
//! - `pub(super) fn attr_ops`
//! - `pub(super) fn one_of`
//! - `pub(super) fn word`
//! - `pub fn set_ops`
//! - `pub(super) fn set_field`
//! - `pub(super) fn body_ops`
//! - `pub fn impl_ports`
//! - `pub fn sync_ops`
//! - `pub fn add_param_ops`
//! - `pub fn add_impl_ops`
//! - `pub fn delete_param_ops`
//! - `pub fn delete_impl_ops`
//! - `pub fn move_pin_ops`

use schemify_doc::{Document, Loc, Op, PinDir, RecordBuf, RecordKind, TableId, Target};
use schemify_format::FileKind;

use crate::scene::UNIT;

use super::bench::bench_ops;
use super::{
    ADD_LANGS, DIALECTS, Field, PARAM_ATTRS, PARAM_KINDS, PIN_DIRS, PIN_DOMAINS, SyncPlan, attr_of,
    body_row, body_text, names, record,
};

/// Set `key` to `value` on `target` (`values` split for multi-value attributes); an empty
/// value removes the line when present. No op when nothing changes.
pub(super) fn attr_ops(
    target: Target,
    current: Option<&str>,
    key: &str,
    value: &str,
    values: Vec<String>,
) -> Vec<Op> {
    match (current, value.is_empty()) {
        (Some(c), _) if c == value => Vec::new(),
        (None, true) => Vec::new(),
        (Some(_), true) => vec![Op::RemoveAttr {
            target,
            key: key.to_owned(),
            first: None,
        }],
        (_, false) => vec![Op::SetAttr {
            target,
            key: key.to_owned(),
            values,
        }],
    }
}

pub(super) fn one_of(value: &str, allowed: &[&str], what: &str) -> Result<(), String> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!("{what} must be one of {}", allowed.join(", ")))
    }
}

pub(super) fn word(value: &str, what: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        Err(format!("{what} must be one word"))
    } else {
        Ok(())
    }
}

/// Ops setting `field` of `doc` to `value` (trimmed, except a body). `Ok(empty)` when
/// nothing changes; `Err` names what is wrong with the value (the document is untouched).
pub fn set_ops(doc: &Document, field: Field<'_>, value: &str) -> Result<Vec<Op>, String> {
    let v = if matches!(field, Field::Body(_)) {
        value.trim_end()
    } else {
        value.trim()
    };
    let ops = match field {
        Field::Header(key) => {
            let allowed: &[&str] = match doc.kind() {
                FileKind::Shprim | FileKind::Shcomp => &["doc", "category", "prefix", "default"],
                FileKind::Shbench => &["doc", "category", "corner", "config"],
                _ => &[],
            };
            one_of(key, allowed, "header attribute")?;
            if key == "default" && !v.is_empty() {
                let impls = names(doc, TableId::Impls, doc.impls().kind.len());
                if !impls.iter().any(|n| n == v) {
                    return Err(format!("no impl named {v}"));
                }
            }
            if key == "prefix" && v.chars().count() > 1 {
                return Err("prefix is one letter".into());
            }
            attr_ops(
                Target::header(),
                doc.header_value(key),
                key,
                v,
                vec![v.to_owned()],
            )
        }
        Field::Pin(row, a) => {
            let rec = record(doc, TableId::Pins, row).ok_or("no such pin")?;
            let target = Target::new(RecordKind::Pin, &rec.fields[0]);
            match a {
                "name" => {
                    word(v, "a pin name")?;
                    set_field(target, &rec, 0, v)
                }
                "dir" => {
                    one_of(v, &PIN_DIRS, "dir")?;
                    set_field(target, &rec, 1, v)
                }
                "domain" => {
                    one_of(v, &PIN_DOMAINS, "domain")?;
                    // `analog` is the default: written as no attribute.
                    let v = if v == "analog" { "" } else { v };
                    attr_ops(
                        target,
                        attr_of(&rec, "domain"),
                        "domain",
                        v,
                        vec![v.to_owned()],
                    )
                }
                _ => return Err(format!("pin has no field {a}")),
            }
        }
        Field::Param(row, a) => {
            let rec = record(doc, TableId::Params, row).ok_or("no such parameter")?;
            let target = Target::new(RecordKind::Param, &rec.fields[0]);
            match a {
                "name" => {
                    word(v, "a parameter name")?;
                    set_field(target, &rec, 0, v)
                }
                "default" => {
                    if v.is_empty() {
                        return Err("a parameter needs a default".into());
                    }
                    set_field(target, &rec, 1, v)
                }
                "kind" => {
                    one_of(v, &PARAM_KINDS, "kind")?;
                    let v = if v == "number" { "" } else { v };
                    attr_ops(target, attr_of(&rec, "kind"), "kind", v, vec![v.to_owned()])
                }
                "choices" => {
                    let cur = rec.find_attr("choices").map(|at| at.values.join(" "));
                    let vals = v.split_whitespace().map(str::to_owned).collect();
                    attr_ops(target, cur.as_deref(), "choices", v, vals)
                }
                a if PARAM_ATTRS.contains(&a) => {
                    attr_ops(target, attr_of(&rec, a), a, v, vec![v.to_owned()])
                }
                _ => return Err(format!("parameter has no field {a}")),
            }
        }
        Field::Impl(row, a) => {
            let rec = record(doc, TableId::Impls, row).ok_or("no such impl")?;
            one_of(a, &["model", "file"], "impl field")?;
            let target = Target::new(RecordKind::Impl, &rec.fields[0]);
            attr_ops(target, attr_of(&rec, a), a, v, vec![v.to_owned()])
        }
        Field::Format(row, d) => {
            let rec = record(doc, TableId::Impls, row).ok_or("no such impl")?;
            one_of(d, &DIALECTS, "dialect")?;
            let target = Target::new(RecordKind::Impl, &rec.fields[0]);
            let cur = rec
                .attrs
                .iter()
                .find(|at| at.key == "format" && at.values.first().is_some_and(|f| f == d))
                .and_then(|at| at.values.get(1))
                .map(String::as_str);
            match (cur, v.is_empty()) {
                (Some(c), _) if c == v => Vec::new(),
                (None, true) => Vec::new(),
                (Some(_), true) => vec![Op::RemoveAttr {
                    target,
                    key: "format".into(),
                    first: Some(d.to_owned()),
                }],
                (_, false) => vec![Op::SetAttr {
                    target,
                    key: "format".into(),
                    values: vec![d.to_owned(), v.to_owned()],
                }],
            }
        }
        Field::Body(row) => {
            let rec = record(doc, TableId::Impls, row).ok_or("no such impl")?;
            let name = &rec.fields[0];
            if rec.fields.get(1).is_some_and(|l| l == "schematic") {
                return Err("a schematic impl has no body".into());
            }
            body_ops(doc, name, v)
        }
        Field::Bench(b, row, a) => {
            let rec = record(doc, b.table(), row).ok_or_else(|| format!("no such {}", b.word()))?;
            return bench_ops(b, &rec, a, v);
        }
    };
    debug_assert!(ops.len() <= 2);
    Ok(ops)
}

pub(super) fn set_field(target: Target, rec: &RecordBuf, index: u32, v: &str) -> Vec<Op> {
    if rec.fields.get(index as usize).is_some_and(|f| f == v) {
        return Vec::new();
    }
    vec![Op::SetField {
        target,
        index,
        value: v.to_owned(),
    }]
}

/// Ops replacing impl `name`'s body with `text` (blank deletes the body record).
pub(super) fn body_ops(doc: &Document, name: &str, text: &str) -> Vec<Op> {
    let row = body_row(doc, name);
    if row.is_some_and(|_| body_text(doc, name) == text)
        || (row.is_none() && text.trim().is_empty())
    {
        return Vec::new();
    }
    let target = Target::new(RecordKind::Body, name);
    if text.trim().is_empty() {
        return vec![Op::Delete(target)];
    }
    let rec = text.split('\n').fold(
        RecordBuf::new(RecordKind::Body).field(name),
        RecordBuf::verbatim,
    );
    match row {
        Some(_) => vec![Op::Put { target, rec }],
        None => vec![Op::Insert { rec, at: None }],
    }
}

/// The ports impl `row`'s body declares (`schemify_elab::impl_ports`). `Err` in words when
/// the row is no impl, the impl is a schematic or has no body, or the body does not parse
/// (with its line).
pub fn impl_ports(doc: &Document, row: u32) -> Result<Vec<(String, PinDir)>, String> {
    let rec = record(doc, TableId::Impls, row).ok_or("no such impl")?;
    let (name, lang) = (&rec.fields[0], rec.fields.get(1).map_or("", String::as_str));
    if lang == "schematic" {
        return Err("a schematic impl's ports are its schematic's".into());
    }
    if body_row(doc, name).is_none() {
        return Err(format!("impl {name} has no body to read ports from"));
    }
    let ports = schemify_elab::impl_ports(lang, &body_text(doc, name))
        .map_err(|e| format!("impl {name} body {e}"))?;
    debug_assert!(ports.len() <= schemify_elab::PORTS_MAX);
    Ok(ports)
}

/// What syncing `doc`'s pins to `ports` (unique names, port order) changes, and the ops
/// that do it in one transaction: delete pins with no port, set dirs, add a pin per new
/// port, reorder to port order. Existing pins keep their coordinates and attributes; new
/// pins stack below the lowest pin on the 10-unit grid, inputs in the leftmost pin column,
/// the rest in the rightmost. No ops when the pins already match.
pub fn sync_ops(doc: &Document, ports: &[(String, PinDir)]) -> (SyncPlan, Vec<Op>) {
    assert!(ports.len() <= schemify_elab::PORTS_MAX);
    let (s, pins) = (doc.strings(), doc.pins());
    let have: Vec<&str> = pins.name.iter().map(|&n| s.get(n)).collect();
    let mut plan = SyncPlan::default();
    let mut ops = Vec::new();
    for name in &have {
        if !ports.iter().any(|p| p.0 == *name) {
            plan.remove.push((*name).to_owned());
            ops.push(Op::Delete(Target::new(RecordKind::Pin, name)));
        }
    }
    for (name, dir) in ports {
        match have.iter().position(|h| h == name) {
            Some(i) if pins.dir[i] != *dir => {
                plan.redir.push((name.clone(), pins.dir[i], *dir));
                ops.push(Op::SetField {
                    target: Target::new(RecordKind::Pin, name),
                    index: 1,
                    value: dir.as_str().to_owned(),
                });
            }
            Some(_) => {}
            None => plan.add.push((name.clone(), *dir)),
        }
    }
    // ponytail: new pins go below every pin, not outside the drawn body's bbox; a symbol
    // whose graphics reach below its pins may overlap them. "Symbol from ports" redraws
    // cleanly; read the graphics' extent here if that bites.
    let grid = 10 * UNIT;
    let (left, right, bottom) = match (pins.x.iter().min(), pins.x.iter().max()) {
        (Some(&l), Some(&r)) => {
            let r = if r == l { l + 4 * grid } else { r };
            (l, r, pins.y.iter().copied().max().unwrap_or(0))
        }
        _ => (-2 * grid, 2 * grid, 0),
    };
    let c = crate::geometry::coord;
    for (k, (name, dir)) in plan.add.iter().enumerate() {
        let x = if *dir == PinDir::In { left } else { right };
        let y = bottom + (k as i32 + 1) * grid;
        let rec = RecordBuf::new(RecordKind::Pin)
            .field(name)
            .field(dir.as_str())
            .field(&c(x))
            .field(&c(y));
        ops.push(Op::Insert { rec, at: None });
    }
    // Pin order after the deletes and appends, then moved into port order one by one.
    let mut order: Vec<&str> = have
        .iter()
        .copied()
        .filter(|h| ports.iter().any(|p| p.0 == *h))
        .chain(plan.add.iter().map(|a| a.0.as_str()))
        .collect();
    assert_eq!(order.len(), ports.len());
    for (k, (name, _)) in ports.iter().enumerate() {
        if order[k] != name {
            let from = order[k..]
                .iter()
                .position(|o| o == name)
                .expect("every port has a pin")
                + k;
            let moved = order.remove(from);
            order.insert(k, moved);
            plan.reorder = true;
            ops.push(Op::Reorder {
                target: Target::new(RecordKind::Pin, name),
                to: k as u32,
            });
        }
    }
    debug_assert!(order.iter().zip(ports).all(|(o, p)| *o == p.0));
    debug_assert_eq!(plan.is_empty(), ops.is_empty());
    (plan, ops)
}

/// Ops adding a parameter `p<k>` (the first free `k`) with default `0`.
pub fn add_param_ops(doc: &Document) -> Vec<Op> {
    let names = names(doc, TableId::Params, doc.params().kind.len());
    let name = (1..=names.len() + 1)
        .map(|k| format!("p{k}"))
        .find(|n| !names.contains(n))
        .expect("one of n + 1 names is free");
    let rec = RecordBuf::new(RecordKind::Param).field(&name).field("0");
    vec![Op::Insert { rec, at: None }]
}

/// Ops adding an impl of `lang` (named per [`ADD_LANGS`], uniquified). A second impl also
/// sets the header `default` to the first (E025: more than one impl needs a default). `Err`
/// when the file kind does not allow it: a `.shprim` has no schematic impl, a `.shcomp`
/// exactly one, a `.shbench` none.
pub fn add_impl_ops(doc: &Document, lang: &str) -> Result<Vec<Op>, String> {
    let (_, base) = ADD_LANGS
        .iter()
        .find(|(l, _)| *l == lang)
        .ok_or_else(|| format!("unknown impl language {lang}"))?;
    let n = doc.impls().kind.len();
    let langs: Vec<String> = (0..n as u32)
        .map(|row| {
            let r = doc.record(Loc {
                table: TableId::Impls,
                row,
            });
            r.fields.get(1).cloned().unwrap_or_default()
        })
        .collect();
    let has_sch = langs.iter().any(|l| l == "schematic");
    match (doc.kind(), lang) {
        (FileKind::Shprim, "schematic") => return Err("a primitive has no schematic".into()),
        (FileKind::Shcomp, "schematic") if has_sch => {
            return Err("a component has exactly one schematic".into());
        }
        (FileKind::Shprim | FileKind::Shcomp, _) => {}
        _ => return Err("this file kind has no impls".into()),
    }
    let names = names(doc, TableId::Impls, n);
    let name = std::iter::once(base.to_string())
        .chain((2..=n + 2).map(|k| format!("{base}{k}")))
        .find(|c| !names.contains(c))
        .expect("one of n + 2 names is free");
    let mut ops = vec![Op::Insert {
        rec: RecordBuf::new(RecordKind::Impl).field(&name).field(lang),
        at: None,
    }];
    if n == 1 && doc.header_value("default").is_none() {
        ops.push(Op::SetAttr {
            target: Target::header(),
            key: "default".into(),
            values: vec![names[0].clone()],
        });
    }
    Ok(ops)
}

/// Ops deleting param `row`.
pub fn delete_param_ops(doc: &Document, row: u32) -> Vec<Op> {
    record(doc, TableId::Params, row).map_or_else(Vec::new, |rec| {
        vec![Op::Delete(Target::new(RecordKind::Param, &rec.fields[0]))]
    })
}

/// Ops deleting impl `row` with its body; a header `default` naming it moves to the first
/// remaining impl, or goes when at most one impl remains.
pub fn delete_impl_ops(doc: &Document, row: u32) -> Vec<Op> {
    let Some(rec) = record(doc, TableId::Impls, row) else {
        return Vec::new();
    };
    let name = rec.fields[0].clone();
    let mut ops = vec![Op::Delete(Target::new(RecordKind::Impl, &name))];
    if body_row(doc, &name).is_some() {
        ops.push(Op::Delete(Target::new(RecordKind::Body, &name)));
    }
    let rest: Vec<String> = names(doc, TableId::Impls, doc.impls().kind.len())
        .into_iter()
        .filter(|n| *n != name)
        .collect();
    let default = doc.header_value("default");
    if default.is_some() && (rest.len() <= 1 || default == Some(name.as_str())) {
        if rest.len() > 1 {
            ops.push(Op::SetAttr {
                target: Target::header(),
                key: "default".into(),
                values: vec![rest[0].clone()],
            });
        } else {
            ops.push(Op::RemoveAttr {
                target: Target::header(),
                key: "default".into(),
                first: None,
            });
        }
    }
    ops
}

/// Ops moving pin `row` one place earlier (`up`) or later in port order; none at an end.
pub fn move_pin_ops(doc: &Document, row: u32, up: bool) -> Vec<Op> {
    let n = doc.pins().name.len() as u32;
    let to = match (up, row) {
        (true, 0) => return Vec::new(),
        (true, r) => r - 1,
        (false, r) if r + 1 >= n => return Vec::new(),
        (false, r) => r + 1,
    };
    let Some(rec) = record(doc, TableId::Pins, row) else {
        return Vec::new();
    };
    vec![Op::Reorder {
        target: Target::new(RecordKind::Pin, &rec.fields[0]),
        to,
    }]
}
