//! docs/dev/testing.md `format` oracles: `format::parse(format::write(d)) == d`, `format::write(format::parse(s)) == s` for
//! canonical `s`, and any reorder of rank-3 records parses to the same canonical file.

mod common;

use common::Rng;
use common::docgen;
use proptest::prelude::*;
use schemify_format as format;
use schemify_format::{Document, FileKind, RecordKind, Shape, Ty};

const RANK3: &[&str] = &[
    "inst",
    "wire",
    "label",
    "port",
    "noconn",
    "text",
    "constraint",
    "waive",
];

// ---- private ----

fn kind_of(seed: u64) -> FileKind {
    if seed.is_multiple_of(2) {
        FileKind::Shcomp
    } else {
        FileKind::Shbench
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn parse_of_write_is_identity(seed in any::<u64>()) {
        let doc = docgen::gen_doc(&mut Rng::new(seed), kind_of(seed));
        let text = format::write(&doc);
        let parsed = format::parse(text.as_bytes()).unwrap_or_else(|e| panic!("{e}\n---\n{text}"));
        prop_assert_eq!(&parsed, &doc, "\n{}", text);
        prop_assert!(parsed.warnings().is_empty());
        // Same model, same bytes: the parsed copy borrows text, the original owns it.
        prop_assert_eq!(format::write(&parsed), text);
    }

    #[test]
    fn rank3_permutation_is_invisible(seed in any::<u64>()) {
        let mut rng = Rng::new(seed);
        let text = format::write(&docgen::gen_doc(&mut rng, kind_of(seed)));
        let parts = common::chunks(&text);
        let is3 = |c: &Vec<&str>| RANK3.contains(&common::chunk_word(c));
        let first = parts.iter().position(is3).unwrap_or(parts.len());
        let end = first + parts[first..].iter().take_while(|c| is3(c)).count();
        let mut middle: Vec<Vec<&str>> = parts[first..end].to_vec();
        rng.shuffle(&mut middle);
        let lines: Vec<&str> = parts[..first].iter().chain(&middle).chain(&parts[end..]).flatten().copied().collect();
        let shuffled = format!("{}\n", lines.join("\n"));
        let doc = format::parse(shuffled.as_bytes()).unwrap_or_else(|e| panic!("{e}\n---\n{shuffled}"));
        prop_assert_eq!(format::write(&doc), text);
    }

    #[test]
    fn noncanonical_whitespace_canonicalizes(seed in any::<u64>()) {
        let mut rng = Rng::new(seed);
        let text = format::write(&docgen::gen_doc(&mut rng, kind_of(seed)));
        let noisy = common::messy(&mut rng, &text);
        let doc = format::parse(noisy.as_bytes()).unwrap_or_else(|e| panic!("{e}\n---\n{noisy:?}"));
        prop_assert_eq!(format::write(&doc), text);
    }

    /// Oracle: `parse_coord` of the slot's text. The reader's coordinate column holds every
    /// `Coord` slot's milli-units and 0 elsewhere, on canonical and on messy input.
    #[test]
    fn coordinate_column_equals_the_parsed_text(seed in any::<u64>()) {
        let mut rng = Rng::new(seed);
        let text = format::write(&docgen::gen_doc(&mut rng, kind_of(seed)));
        let noisy = common::messy(&mut rng, &text);
        for t in [&text, &noisy] {
            let doc = format::parse(t.as_bytes()).unwrap_or_else(|e| panic!("{e}\n---\n{t:?}"));
            coords_match(&doc);
        }
    }

    /// The reader never panics and only reports declared codes, on arbitrary bytes and
    /// on mutated canonical files (the fuzz target's oracle, run on every test pass).
    #[test]
    fn reader_is_total(seed in any::<u64>(), junk in proptest::collection::vec(any::<u8>(), 0..64)) {
        let mut rng = Rng::new(seed);
        let mut bytes = format::write(&docgen::gen_doc(&mut rng, kind_of(seed))).into_bytes();
        let at = rng.below(bytes.len() as u64 + 1) as usize;
        let cut = (at + rng.below(8) as usize).min(bytes.len());
        bytes.splice(at..cut, junk.iter().copied());
        if let Ok(doc) = format::parse(&bytes) {
            let again = format::write(&doc);
            let reparsed = format::parse(again.as_bytes()).expect("canonical output parses");
            prop_assert_eq!(format::write(&reparsed), again);
        }
        let _ = format::parse(&junk);
    }
}

#[test]
fn unknown_records_survive_in_newer_versions() {
    let text = "schemify 2 shbench\n\nfuture 1 2\n  knob 3\n  | raw\n\ninst 0abc ./x\n  at 0 0 R0\n  name X1\n\nhologram zz\n\nwire 1abc 0 0 1 0\n";
    let doc = format::parse(text.as_bytes()).unwrap();
    assert_eq!(doc.record_word(1), "future");
    // `future` follows the header (rank 0); `hologram` sorts after the known rank-3 kinds.
    let expected = "schemify 2 shbench\n\nfuture 1 2\n  knob 3\n  | raw\n\ninst 0abc ./x\n  at 0 0 R0\n  name X1\n\nwire 1abc 0 0 1 0\n\nhologram zz\n";
    assert_eq!(format::write(&doc), expected);
    assert_eq!(
        format::write(&format::parse(expected.as_bytes()).unwrap()),
        expected
    );
}

#[test]
fn attributes_keep_order_with_unknown_ones_last() {
    let text = "schemify 1 shprim\n  pdk sky130\n  doc d\n\npin A in 0 0\n  zz 1\n  domain real\nimpl sp spice\n";
    let doc = format::parse(text.as_bytes()).unwrap();
    assert_eq!(
        format::write(&doc),
        "schemify 1 shprim\n  doc d\n  pdk sky130\n\npin A in 0 0\n  domain real\n  zz 1\n\nimpl sp spice\n"
    );
}

#[test]
#[ignore = "prints sample generated documents for eyeballing the generator"]
fn show_sample() {
    for seed in [3u64, 4] {
        println!(
            "{}",
            format::write(&docgen::gen_doc(&mut Rng::new(seed), kind_of(seed)))
        );
    }
}

#[test]
fn values_borrow_unless_escaped() {
    let text = "schemify 1 config\n\nset a \"x y\"\nset b \"q\\\"q\"\n";
    let doc = format::parse(text.as_bytes()).unwrap();
    assert_eq!(doc.field(1, 1), Some("x y"));
    assert_eq!(doc.field(2, 1), Some("q\"q"));
    assert_eq!(format::write(&doc), text);
}

/// Asserts the coordinate column of every known record against its texts.
fn coords_match(doc: &Document<'_>) {
    let check = |shape: &Shape, texts: Vec<&str>, milli: &[i32]| {
        assert_eq!(texts.len(), milli.len());
        for (i, (t, m)) in texts.iter().zip(milli).enumerate() {
            let want = match shape.ty(i) {
                Some(Ty::Coord) => format::parse_coord(t).expect("a parsed Coord slot is valid"),
                _ => 0,
            };
            assert_eq!(*m, want, "slot {i} {t:?}");
        }
    };
    for r in 1..doc.record_count() {
        let kind = doc.record_kind(r);
        if kind == RecordKind::Unknown {
            continue;
        }
        let schema = kind.schema();
        check(&schema.fields, doc.fields(r).collect(), doc.field_coords(r));
        for a in doc.attrs(r) {
            if let Some((_, attr)) = schema.attr(doc.attr_key(a)) {
                check(
                    &attr.shape,
                    doc.attr_values(a).collect(),
                    doc.value_coords(a),
                );
            }
        }
    }
}
