//! Merge fuzz target. The oracle (`../oracles/merge3.rs`) is shared with the merge-tier
//! replay of the checked-in corpus (`crates/protocol/format/tests/fuzz_corpus.rs`).

#![no_main]

use libfuzzer_sys::fuzz_target;

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/oracles/merge3.rs"));

fuzz_target!(|data: &[u8]| merge3_oracle(data));
