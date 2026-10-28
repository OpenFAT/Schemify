//! Merge-tier replay of the checked-in fuzz corpora (docs/dev/testing.md tiers): every input of
//! `fuzz/corpus/{parse,merge3}/` through the fuzz targets' own oracles.

include!(concat!(env!("SCHEMIFY_ROOT"), "/tests/fuzz/replay.rs"));
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fuzz/oracles/parse.rs"
));
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fuzz/oracles/merge3.rs"
));

// ---- private ----

#[test]
fn parse_corpus_passes_the_reader_oracle() {
    replay("parse", parse_oracle);
}

#[test]
fn merge3_corpus_passes_the_merge_oracle() {
    replay("merge3", merge3_oracle);
}
