// Temporary profiling scaffold (deleted before commit).
#[test]
fn empty() {
    let text = std::env::var("N2S_TEXT").unwrap_or_else(|_| "* nothing\n.op\n".into());
    let text = if let Some(p) = text.strip_prefix('@') { std::fs::read_to_string(p).unwrap() } else { text };
    let _ = std::hint::black_box(net2schem::convert("n2s/x.sp", &text));
    eprintln!("seg_ok {:?}", net2schem::route::C);
}
