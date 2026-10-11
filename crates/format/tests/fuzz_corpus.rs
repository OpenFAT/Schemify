//! Merge-tier replay of the checked-in fuzz corpora (TESTING.md tiers): every input of
//! `fuzz/corpus/{parse,merge3}/` through the fuzz targets' own oracles.

include!("../../../tests/fuzz/replay.rs");
include!("../fuzz/oracles/parse.rs");
include!("../fuzz/oracles/merge3.rs");

#[test]
fn parse_corpus_passes_the_reader_oracle() {
    replay("parse", parse_oracle);
}

#[test]
fn merge3_corpus_passes_the_merge_oracle() {
    replay("merge3", merge3_oracle);
}
