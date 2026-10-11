//! `clip`: clipboard fragments as line-record text — a complete Schemify file (`schemify 1
//! shcomp` header, the one schematic `impl` a `.shcomp` needs, then the records in rank order,
//! canonical form), so a fragment pasted into a text editor, another editor process or a chat
//! is readable, diffable, parses back with `format::parse` and could be saved as a cell. The
//! system clipboard itself is `platform::clipboard_set/get`.
//!
//! Data design: in, the copied records (coordinates relative to the copy pivot); out, text.
//! A fragment holds what one selection holds (≤ 2^22 records); nothing is kept.

use schemify_doc::RecordBuf;
use schemify_format::{Document as FormatDoc, FileKind, RecordKind, parse, write};

/// Whether a record kind is a drawable fragment record: a symbol pin, a symbol graphic or a
/// schematic object (no params, impls, docs or bench records).
fn drawable(kind: RecordKind) -> bool {
    kind == RecordKind::Pin || matches!(kind.rank(), 2 | 3)
}

/// Writes `recs` as a fragment file. Records that are not drawable (`drawable`) are
/// skipped; the rest are written in rank order, canonical, after `impl clipboard schematic`.
pub fn encode(recs: &[RecordBuf]) -> String {
    let mut order: Vec<&RecordBuf> = recs
        .iter()
        .filter(|r| FileKind::Shcomp.records().contains(&r.kind) && drawable(r.kind))
        .collect();
    order.sort_by_key(|r| r.kind.rank());
    let mut d = FormatDoc::new(FileKind::Shcomp);
    let pins = order
        .iter()
        .take_while(|r| r.kind == RecordKind::Pin)
        .count();
    let impl_at = pins; // pins first (rank 1, file order), then the impl, then the rest
    let mut wrote_impl = false;
    for (i, r) in order.iter().enumerate() {
        if i == impl_at {
            d.begin_record(RecordKind::Impl);
            d.push_field("clipboard");
            d.push_field("schematic");
            wrote_impl = true;
        }
        write_rec(&mut d, r);
    }
    if !wrote_impl {
        d.begin_record(RecordKind::Impl);
        d.push_field("clipboard");
        d.push_field("schematic");
    }
    let text = write(&d);
    assert!(text.starts_with("schemify "));
    text
}

fn write_rec(d: &mut FormatDoc<'_>, r: &RecordBuf) {
    d.begin_record(r.kind);
    for f in &r.fields {
        d.push_field(f);
    }
    for a in &r.attrs {
        d.push_attr(&a.key);
        for v in &a.values {
            d.push_value(v);
        }
    }
}

/// Reads a fragment back: every record of ranks 1–3 (pins, symbol graphics, schematic
/// objects), in file order.
///
/// # Errors
///
/// The text is not a Schemify file (the first diagnostic), or holds no drawable record.
pub fn decode(text: &str) -> Result<Vec<RecordBuf>, String> {
    let d = parse(text.as_bytes()).map_err(|e| {
        e.diagnostics.first().map_or_else(
            || "not a Schemify fragment".to_owned(),
            |d| format!("{}:{}: {}", d.line, d.col, d.detail),
        )
    })?;
    let recs: Vec<RecordBuf> = (1..d.record_count())
        .filter(|&r| {
            let k = d.record_kind(r);
            k != RecordKind::Unknown && drawable(k)
        })
        .map(|r| RecordBuf::from_format(&d, r))
        .collect();
    if recs.is_empty() {
        return Err("the clipboard holds no schematic objects".into());
    }
    Ok(recs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_round_trip_in_rank_order() {
        let wire = RecordBuf::new(RecordKind::Wire)
            .field("0abcde")
            .field("0")
            .field("0")
            .field("10")
            .field("0");
        let pin = RecordBuf::new(RecordKind::Pin)
            .field("A")
            .field("in")
            .field("0")
            .field("0");
        let inst = RecordBuf::new(RecordKind::Inst)
            .field("1abcde")
            .field("core:res")
            .attr("at", &["0", "0", "R0"])
            .attr("name", &["R1"])
            .attr("R", &["1k"]);
        let text = encode(&[wire.clone(), pin.clone(), inst.clone()]);
        assert!(text.starts_with("schemify 1 shcomp\n"), "{text}");
        let back = decode(&text).unwrap();
        assert_eq!(
            back,
            [pin, inst, wire],
            "rank order, then the writer's kind order"
        );
        assert!(decode("hello").is_err());
        assert!(decode("schemify 1 shcomp\n").is_err());
    }
}
