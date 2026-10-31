//! D39 budget check: a whole pack of the 100k fixture's canvas rows.
//!
//! `cargo test --release -p schemify-render --test perf -- --ignored --nocapture`

use std::time::Instant;

use schemify_canvas::Palette;
use schemify_render::gpu::{Change, InstanceRows};

mod common;
use common::hundred_k;

// ---- private ----

/// D39: a whole pack of the 100k fixture (a document opened, a palette changed, a diff that
/// gave up) must stay under the 1 ms interactive budget's order: median of 20 packs.
#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn pack_100k_whole() {
    let prims = hundred_k();
    let palette = Palette::xschem_dark();
    let mut rows = InstanceRows::default();
    let mut ms = Vec::new();
    for _ in 0..20 {
        let t0 = Instant::now();
        rows.pack(0, &prims, &palette, Change::All);
        ms.push(t0.elapsed().as_secs_f64() * 1e3);
        std::hint::black_box(&rows);
    }
    ms.sort_by(f64::total_cmp);
    let bytes: usize = (0..schemify_render::gpu::KINDS)
        .map(|k| rows.bytes(0, k).len())
        .sum();
    eprintln!(
        "pack 100k whole: median {:.3} ms, max {:.3} ms, {} KiB of rows",
        ms[10],
        ms[19],
        bytes / 1024
    );
    eprintln!("PERF render.pack_100k_whole_ms {:.3}", ms[10]);
}
