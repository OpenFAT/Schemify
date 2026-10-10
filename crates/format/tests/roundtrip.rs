//! TESTING.md `format` oracles: `parse(write(d)) == d`, `write(parse(s)) == s` for
//! canonical `s`, and any reorder of rank-3 records parses to the same canonical file.

mod common;

use common::{Rng, chunk_word, chunks, gen_doc, messy};
use proptest::prelude::*;
use schemify_format::{FileKind, parse, write};

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
        let doc = gen_doc(&mut Rng::new(seed), kind_of(seed));
        let text = write(&doc);
        let parsed = parse(text.as_bytes()).unwrap_or_else(|e| panic!("{e}\n---\n{text}"));
        prop_assert_eq!(&parsed, &doc, "\n{}", text);
        prop_assert!(parsed.warnings().is_empty());
        // Same model, same bytes: the parsed copy borrows text, the original owns it.
        prop_assert_eq!(write(&parsed), text);
    }

    #[test]
    fn rank3_permutation_is_invisible(seed in any::<u64>()) {
        let mut rng = Rng::new(seed);
        let text = write(&gen_doc(&mut rng, kind_of(seed)));
        let parts = chunks(&text);
        let is3 = |c: &Vec<&str>| RANK3.contains(&chunk_word(c));
        let first = parts.iter().position(is3).unwrap_or(parts.len());
        let end = first + parts[first..].iter().take_while(|c| is3(c)).count();
        let mut middle: Vec<Vec<&str>> = parts[first..end].to_vec();
        rng.shuffle(&mut middle);
        let lines: Vec<&str> = parts[..first].iter().chain(&middle).chain(&parts[end..]).flatten().copied().collect();
        let shuffled = lines.join("\n") + "\n";
        let doc = parse(shuffled.as_bytes()).unwrap_or_else(|e| panic!("{e}\n---\n{shuffled}"));
        prop_assert_eq!(write(&doc), text);
    }

    #[test]
    fn noncanonical_whitespace_canonicalizes(seed in any::<u64>()) {
        let mut rng = Rng::new(seed);
        let text = write(&gen_doc(&mut rng, kind_of(seed)));
        let noisy = messy(&mut rng, &text);
        let doc = parse(noisy.as_bytes()).unwrap_or_else(|e| panic!("{e}\n---\n{noisy:?}"));
        prop_assert_eq!(write(&doc), text);
    }

    /// The reader never panics and only reports declared codes, on arbitrary bytes and
    /// on mutated canonical files (the fuzz target's oracle, run on every test pass).
    #[test]
    fn reader_is_total(seed in any::<u64>(), junk in proptest::collection::vec(any::<u8>(), 0..64)) {
        let mut rng = Rng::new(seed);
        let mut bytes = write(&gen_doc(&mut rng, kind_of(seed))).into_bytes();
        let at = rng.below(bytes.len() as u64 + 1) as usize;
        let cut = (at + rng.below(8) as usize).min(bytes.len());
        bytes.splice(at..cut, junk.iter().copied());
        if let Ok(doc) = parse(&bytes) {
            let again = write(&doc);
            let reparsed = parse(again.as_bytes()).expect("canonical output parses");
            prop_assert_eq!(write(&reparsed), again);
        }
        let _ = parse(&junk);
    }
}

#[test]
fn unknown_records_survive_in_newer_versions() {
    let text = "schemify 2 shbench\n\nfuture 1 2\n  knob 3\n  | raw\n\ninst 0abc ./x\n  at 0 0 R0\n  name X1\n\nhologram zz\n\nwire 1abc 0 0 1 0\n";
    let doc = parse(text.as_bytes()).unwrap();
    assert_eq!(doc.record_word(1), "future");
    // `future` follows the header (rank 0); `hologram` sorts after the known rank-3 kinds.
    let expected = "schemify 2 shbench\n\nfuture 1 2\n  knob 3\n  | raw\n\ninst 0abc ./x\n  at 0 0 R0\n  name X1\n\nwire 1abc 0 0 1 0\n\nhologram zz\n";
    assert_eq!(write(&doc), expected);
    assert_eq!(write(&parse(expected.as_bytes()).unwrap()), expected);
}

#[test]
fn attributes_keep_order_with_unknown_ones_last() {
    let text = "schemify 1 shprim\n  pdk sky130\n  doc d\n\npin A in 0 0\n  zz 1\n  domain real\nimpl sp spice\n";
    let doc = parse(text.as_bytes()).unwrap();
    assert_eq!(
        write(&doc),
        "schemify 1 shprim\n  doc d\n  pdk sky130\n\npin A in 0 0\n  domain real\n  zz 1\n\nimpl sp spice\n"
    );
}

#[test]
#[ignore = "prints sample generated documents for eyeballing the generator"]
fn show_sample() {
    for seed in [3u64, 4] {
        println!("{}", write(&gen_doc(&mut Rng::new(seed), kind_of(seed))));
    }
}

#[test]
fn values_borrow_unless_escaped() {
    let text = "schemify 1 config\n\nset a \"x y\"\nset b \"q\\\"q\"\n";
    let doc = parse(text.as_bytes()).unwrap();
    assert_eq!(doc.field(1, 1), Some("x y"));
    assert_eq!(doc.field(2, 1), Some("q\"q"));
    assert_eq!(write(&doc), text);
}
