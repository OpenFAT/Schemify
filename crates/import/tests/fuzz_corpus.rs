//! Merge-tier replay of the checked-in fuzz corpus (TESTING.md tiers): every input of
//! `fuzz/corpus/xschem_read/` through the fuzz target's own oracle.

include!("../../../tests/fuzz/replay.rs");
include!("../fuzz/oracles/xschem_read.rs");

#[test]
fn xschem_read_corpus_passes_the_reader_oracle() {
    replay("xschem_read", xschem_read_oracle);
}
