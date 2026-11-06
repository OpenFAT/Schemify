// The merge oracle, `include!`d by the `merge3` fuzz target and by the merge-tier corpus
// replay (`tests/fuzz_corpus.rs`), so both run the same checks.

/// Merges three NUL-separated inputs: never panics, a side equal to the base yields the
/// other side's canonical text, and the conflict count matches the markers (asserted
/// inside `merge3`).
fn merge3_oracle(data: &[u8]) {
    let mut parts = data.splitn(3, |&b| b == 0);
    let (Some(o), Some(a), Some(b)) = (parts.next(), parts.next(), parts.next()) else {
        return;
    };
    let parsed = (
        schemify_format::parse(o),
        schemify_format::parse(a),
        schemify_format::parse(b),
    );
    let (Ok(o), Ok(a), Ok(b)) = parsed else {
        return;
    };
    let _ = schemify_format::merge3(&o, &a, &b);
    let (wa, wb) = (schemify_format::write(&a), schemify_format::write(&b));
    assert_eq!(schemify_format::merge3(&o, &a, &o).text, wa);
    assert_eq!(schemify_format::merge3(&o, &o, &b).text, wb);
    assert!(schemify_format::diff(&a, &a).changes.is_empty());
    let _ = schemify_format::diff(&o, &b);
}
