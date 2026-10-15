// Merge-tier replay of a checked-in fuzz corpus (docs/dev/testing.md tiers), `include!`d by each
// fuzzed crate's `tests/fuzz_corpus.rs` next to the target oracles in `fuzz/oracles/`.
// The corpora are minimized (`cargo fuzz cmin`) by the manual `fuzz-corpus` job of
// `.github/workflows/nightly.yml` and committed under `<crate>/fuzz/corpus/<target>/`.

/// Runs `oracle` on every input of the including crate's `fuzz/corpus/<target>/`, in
/// file-name order, and names the input that fails it.
///
/// # Panics
///
/// When the corpus directory is missing or empty, or an input fails the oracle.
fn replay(target: &str, oracle: fn(&[u8])) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz/corpus")
        .join(target);
    let mut inputs: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("corpus entry").path())
        .collect();
    inputs.sort();
    assert!(!inputs.is_empty(), "{}: empty corpus", dir.display());
    for input in &inputs {
        let data = std::fs::read(input).expect("corpus input");
        let run = std::panic::catch_unwind(|| oracle(&data));
        assert!(run.is_ok(), "{} fails the {target} oracle", input.display());
    }
}
