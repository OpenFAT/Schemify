//! Merge-tier replay of the checked-in fuzz corpus (docs/dev/testing.md tiers): every input of
//! `fuzz/corpus/compile/` through the fuzz target's own oracle.

include!(concat!(env!("SCHEMIFY_ROOT"), "/tests/fuzz/replay.rs"));
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fuzz/oracles/compile.rs"
));

// ---- private ----

#[test]
fn compile_corpus_passes_the_compiler_oracle() {
    replay("compile", compile_oracle);
}
