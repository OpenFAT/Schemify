//! Bench records' ops: their fields (goals and line lists included), added and deleted
//! analysis, sweep, corner and optimize records.
//!
//! Contents:
//! - `pub(super) fn bench_ops`
//! - `pub fn add_bench_ops`
//! - `pub fn delete_bench_ops`

use schemify_doc::{Document, Op, RecordBuf, RecordKind, TableId, Target};
use schemify_format::FileKind;

use super::ops::{attr_ops, one_of, set_field, word};
use super::{ALGOS, Bench, attr_of, names, record, record_count};

pub(super) fn bench_ops(b: Bench, rec: &RecordBuf, a: &str, v: &str) -> Result<Vec<Op>, String> {
    let target = Target::new(b.kind(), &rec.fields[0]);
    let words = |v: &str| {
        v.split_whitespace()
            .map(|w| vec![w.to_owned()])
            .collect::<Vec<_>>()
    };
    let ops = match (b, a) {
        (Bench::Output, "name") => {
            word(v, "an output name")?;
            set_field(target, rec, 0, v)
        }
        (Bench::Output, "expr") => {
            if v.is_empty() {
                return Err("an output needs an expression".into());
            }
            set_field(target, rec, 1, v)
        }
        (Bench::Output, "unit") | (Bench::Spec, "min" | "max") => {
            attr_ops(target, attr_of(rec, a), a, v, vec![v.to_owned()])
        }
        (Bench::Optimize, "algo") => {
            one_of(v, &ALGOS, "algo")?;
            let v = if v == "nelder-mead" { "" } else { v };
            attr_ops(target, attr_of(rec, "algo"), "algo", v, vec![v.to_owned()])
        }
        (Bench::Optimize, "budget" | "seed") => {
            if !v.is_empty() && v.parse::<u64>().is_err() {
                return Err(format!("{a} must be a whole number"));
            }
            attr_ops(target, attr_of(rec, a), a, v, vec![v.to_owned()])
        }
        (Bench::Optimize, "goals") => lines_ops(&target, rec, "goal", parse_goals(v)?),
        (Bench::Optimize, "vary") => lines_ops(&target, rec, "vary", words(v)),
        (Bench::Optimize, "corners") => lines_ops(&target, rec, "corner", words(v)),
        (Bench::Optimize, "configs") => lines_ops(&target, rec, "config", words(v)),
        _ => return Err(format!("{} has no field {a}", b.word())),
    };
    Ok(ops)
}

/// Ops adding a bench record: an output `out<k>` = `0`, a spec for the first output without
/// one, or an optimizer `opt<k>` (defaults: nelder-mead, budget 100).
pub fn add_bench_ops(doc: &Document, b: Bench) -> Result<Vec<Op>, String> {
    if doc.kind() != FileKind::Shbench {
        return Err("only a testbench has outputs, specs and optimizers".into());
    }
    let n = record_count(doc, b.table());
    let have = names(doc, b.table(), n);
    let free = |base: &str| {
        (1..=n + 1)
            .map(|k| format!("{base}{k}"))
            .find(|c| !have.contains(c))
            .expect("one of n + 1 names is free")
    };
    let rec = match b {
        Bench::Output => RecordBuf::new(RecordKind::Output)
            .field(&free("out"))
            .field("0"),
        Bench::Spec => {
            let outs = names(doc, TableId::Outputs, record_count(doc, TableId::Outputs));
            let out = outs
                .into_iter()
                .find(|o| !have.contains(o))
                .ok_or("every output already has a spec (add an output first)")?;
            RecordBuf::new(RecordKind::Spec).field(&out)
        }
        Bench::Optimize => RecordBuf::new(RecordKind::Optimize).field(&free("opt")),
    };
    Ok(vec![Op::Insert { rec, at: None }])
}

/// Ops deleting bench record `row` of table `b`.
pub fn delete_bench_ops(doc: &Document, b: Bench, row: u32) -> Vec<Op> {
    record(doc, b.table(), row).map_or_else(Vec::new, |rec| {
        vec![Op::Delete(Target::new(b.kind(), &rec.fields[0]))]
    })
}

// ---- private ----

/// Ops replacing every `key` line of `target` by one line per item of `items`.
fn lines_ops(target: &Target, rec: &RecordBuf, key: &str, items: Vec<Vec<String>>) -> Vec<Op> {
    let current: Vec<Vec<String>> = rec
        .attrs
        .iter()
        .filter(|a| a.key == key)
        .map(|a| a.values.clone())
        .collect();
    if current == items {
        return Vec::new();
    }
    let mut ops = Vec::with_capacity(items.len() + 1);
    if !current.is_empty() {
        ops.push(Op::RemoveAttr {
            target: target.clone(),
            key: key.to_owned(),
            first: None,
        });
    }
    // ponytail: SetAttr replaces a repeatable line by its first value, so items sharing a
    // first value (two goals on one output) keep only the last; split the record if needed.
    for values in items {
        ops.push(Op::SetAttr {
            target: target.clone(),
            key: key.to_owned(),
            values,
        });
    }
    ops
}

/// Parses an optimizer goal list `<output> min|max|target <expr> [weight <w>]; …`.
fn parse_goals(v: &str) -> Result<Vec<Vec<String>>, String> {
    let mut out = Vec::new();
    for g in v.split(';').map(str::trim).filter(|g| !g.is_empty()) {
        let w: Vec<String> = g.split_whitespace().map(str::to_owned).collect();
        let ok = match w.get(1).map(String::as_str) {
            Some("min" | "max") => w.len() == 2 || (w.len() == 4 && w[2] == "weight"),
            Some("target") => w.len() == 3 || (w.len() == 5 && w[3] == "weight"),
            _ => false,
        };
        if !ok {
            return Err(format!(
                "goal `{g}`: expected `<output> min|max|target <expr> [weight <w>]`"
            ));
        }
        out.push(w);
    }
    Ok(out)
}
