//! Fuzz target: the `.sfyx` codec and the whole Virtuoso import. The oracle
//! (`../oracles/sfyx_import.rs`) is shared with the merge-tier replay of the checked-in
//! corpus (`plugins/interop/tests/fuzz_corpus.rs`).
//!
//! Seed the corpus with the dump fixtures:
//! `cargo +nightly fuzz run sfyx_import corpus/sfyx_import`.
#![no_main]

use libfuzzer_sys::fuzz_target;

include!("../oracles/sfyx_import.rs");

fuzz_target!(|data: &[u8]| sfyx_import_oracle(data));
