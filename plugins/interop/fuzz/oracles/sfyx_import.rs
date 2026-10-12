// The `.sfyx` oracle, `include!`d by the `sfyx_import` fuzz target and by the merge-tier
// corpus replay (`tests/fuzz_corpus.rs`), so both run the same checks.

/// Runs the exchange-file codec and the whole Virtuoso import on one input. Oracles: the
/// codec is canonical (`write(read(x))` reads back to the same records and writes the same
/// bytes); the import of anything — a valid dump or garbage — returns files or declared
/// problems and never panics (its count-conservation and table asserts are the checks).
fn sfyx_import_oracle(data: &[u8]) {
    use interop::design::Files;
    use interop::map::Mapping;
    use interop::sfyx;
    use interop::virtuoso::{self, ImportOptions};
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(recs) = sfyx::read(text) {
        let once = sfyx::write(&recs);
        let again = sfyx::read(&once).expect("the writer's output reads");
        assert_eq!(again.len(), recs.len());
        for (a, b) in recs.iter().zip(&again) {
            assert_eq!((&a.tag, &a.fields), (&b.tag, &b.fields));
        }
        assert_eq!(sfyx::write(&again), once, "write is canonical");
    }
    let _ = virtuoso::import_targets(text);
    let _ = virtuoso::import(text, &Files::new(), &Mapping::default(), &ImportOptions::default());
}
