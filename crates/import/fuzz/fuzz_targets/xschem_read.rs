//! Fuzz target: the xschem reader, symbol extraction and the attribute layer. The oracle
//! (`../oracles/xschem_read.rs`) is shared with the merge-tier replay of the checked-in
//! corpus (`crates/import/tests/fuzz_corpus.rs`).
//!
//! Seed the corpus with real `.sym`/`.sch` files:
//! `cargo +nightly fuzz run xschem_read corpus/xschem_read $SCHEMIFY_CORPUS`.
#![no_main]

use libfuzzer_sys::fuzz_target;

include!("../oracles/xschem_read.rs");

fuzz_target!(|data: &[u8]| xschem_read_oracle(data));
