// The reader oracle, `include!`d by the `parse` fuzz target and by the merge-tier corpus
// replay (`tests/fuzz_corpus.rs`), so both run the same checks.

/// Runs the reader on one input: never panics, reports only declared codes, and anything
/// it accepts writes to a canonical text that re-parses and is a fixed point of
/// parse → write. The crate's internal asserts are the rest of the oracle.
fn parse_oracle(data: &[u8]) {
    match schemify_format::parse(data) {
        Ok(doc) => {
            let text = schemify_format::write(&doc);
            let again =
                schemify_format::parse(text.as_bytes()).expect("canonical output parses");
            assert_eq!(schemify_format::write(&again), text, "write is not idempotent");
        }
        Err(e) => {
            assert!(!e.diagnostics.is_empty() && e.diagnostics.len() <= 64);
            assert!(e.diagnostics.iter().all(|d| !d.code.is_warning()));
        }
    }
    let _ = schemify_format::parse_prefix(data, 1);
}
