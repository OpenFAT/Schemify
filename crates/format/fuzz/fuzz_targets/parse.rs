//! Reader fuzz target. The oracle (`../oracles/parse.rs`) is shared with the merge-tier
//! replay of the checked-in corpus (`crates/format/tests/fuzz_corpus.rs`).

#![no_main]

use libfuzzer_sys::fuzz_target;

include!("../oracles/parse.rs");

fuzz_target!(|data: &[u8]| parse_oracle(data));
