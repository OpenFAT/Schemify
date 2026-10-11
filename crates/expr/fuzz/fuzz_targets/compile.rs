//! Compiler fuzz target. The oracle (`../oracles/compile.rs`) is shared with the
//! merge-tier replay of the checked-in corpus (`crates/expr/tests/fuzz_corpus.rs`).
//!
//! Run (nightly): `cargo fuzz run compile` from `crates/expr`.

#![no_main]

use libfuzzer_sys::fuzz_target;

include!("../oracles/compile.rs");

fuzz_target!(|data: &[u8]| compile_oracle(data));
