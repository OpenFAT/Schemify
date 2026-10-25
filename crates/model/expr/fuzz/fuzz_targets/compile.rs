//! Compiler fuzz target. The oracle (`../oracles/compile.rs`) is shared with the
//! merge-tier replay of the checked-in corpus (`crates/model/expr/tests/fuzz_corpus.rs`).
//!
//! Run (nightly): `cargo fuzz run compile` from `crates/model/expr`.

#![no_main]

use libfuzzer_sys::fuzz_target;

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/oracles/compile.rs"));

fuzz_target!(|data: &[u8]| compile_oracle(data));
