//! Merge-tier replay of the checked-in fuzz corpus (TESTING.md tiers): every input of
//! `fuzz/corpus/compile/` through the fuzz target's own oracle.

include!("../../../tests/fuzz/replay.rs");
include!("../fuzz/oracles/compile.rs");

#[test]
fn compile_corpus_passes_the_compiler_oracle() {
    replay("compile", compile_oracle);
}
