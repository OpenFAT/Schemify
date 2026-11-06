//! Reader fuzz target. The oracle (`../oracles/parse.rs`) is shared with the merge-tier
//! replay of the checked-in corpus (`crates/protocol/format/tests/fuzz_corpus.rs`).

#![no_main]

use libfuzzer_sys::fuzz_target;

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/oracles/parse.rs"));

fuzz_target!(|data: &[u8]| parse_oracle(data));
