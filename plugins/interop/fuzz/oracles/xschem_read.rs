// The xschem reader oracle, `include!`d by the `xschem_read` fuzz target and by the
// merge-tier corpus replay (`tests/fuzz_corpus.rs`), so both run the same checks.

/// Runs the xschem reader, symbol extraction and the attribute layer on one input. The
/// oracle is "no panic": the reader asserts record conservation and arena bounds on exit,
/// so an assertion failure is a reader bug.
fn xschem_read_oracle(data: &[u8]) {
    use interop::attr::{Attrs, at_tokens, classify};
    use interop::{symbol, xschem};
    let text = String::from_utf8_lossy(data);
    let Ok(file) = xschem::read(&text) else {
        return;
    };
    let interface = symbol::extract(&file);
    assert!(interface.pins.name.len() <= file.boxes.layer.len());
    let mut attrs = Attrs::default();
    let props = file
        .lines
        .props
        .iter()
        .chain(&file.boxes.props)
        .chain(&file.instances.props)
        .chain(&file.globals.text);
    for span in props {
        attrs.parse_into(file.str(*span));
        for i in 0..attrs.len() {
            let _ = at_tokens(attrs.value(i)).count();
        }
    }
    for span in &file.texts.text {
        let _ = classify(file.str(*span));
    }
}
