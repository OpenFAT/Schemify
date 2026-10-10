//! D39 budget: open 100k objects ≤ 200 ms end to end; the format's share is held to
//! < 100 ms for the parse. Run: `cargo test -p schemify-format --release --test perf -- --ignored`.

mod common;

use common::{Rng, new_id};
use schemify_format::{Document, FileKind, RecordKind, merge3, parse, write};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// The research §1 synthetic mix: 35% instances (name + 5 params), 45% wires,
/// 12% labels, 8% texts, on a grid of √n·150 units.
fn design(n: usize, seed: u64) -> String {
    let mut rng = Rng::new(seed);
    let mut ids = BTreeSet::new();
    let side = ((n as f64).sqrt() * 15.0) as u64 + 1;
    let grid = |rng: &mut Rng| (rng.below(side) as i32 - side as i32 / 2) * 10_000;
    let mut d = Document::new(FileKind::Shbench);
    for i in 0..n {
        let id = new_id(&mut rng, &mut ids);
        match i * 100 / n {
            0..35 => {
                d.begin_record(RecordKind::Inst);
                d.push_field(&id);
                d.push_field("sky130:nfet_01v8");
                d.push_attr("at");
                d.push_value_coord(grid(&mut rng));
                d.push_value_coord(grid(&mut rng));
                d.push_value("R90");
                d.push_attr("name");
                d.push_value(&format!("M{i}"));
                for (k, v) in [
                    ("W", "1u"),
                    ("L", "0.18u"),
                    ("nf", "2"),
                    ("m", "1"),
                    ("model", "nfet_01v8"),
                ] {
                    d.push_attr(k);
                    d.push_value(v);
                }
            }
            35..80 => {
                d.begin_record(RecordKind::Wire);
                d.push_field(&id);
                let (x, y) = (grid(&mut rng), grid(&mut rng));
                for c in [x, y, x + 100_000, y] {
                    d.push_field_coord(c);
                }
            }
            80..92 => {
                d.begin_record(RecordKind::Label);
                d.push_field(&id);
                d.push_field_coord(grid(&mut rng));
                d.push_field_coord(grid(&mut rng));
                d.push_field("R0");
                d.push_field(&format!("net{}", i % 997));
            }
            _ => {
                d.begin_record(RecordKind::Text);
                d.push_field(&id);
                d.push_field_coord(grid(&mut rng));
                d.push_field_coord(grid(&mut rng));
                d.push_field("R0");
                d.push_field(if i % 25 == 0 {
                    "see \"spec\" rev\nsecond line"
                } else {
                    "bias for stage"
                });
            }
        }
    }
    write(&d)
}

fn best_of<T>(runs: u32, mut f: impl FnMut() -> T) -> Duration {
    (0..runs)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(f());
            t.elapsed()
        })
        .min()
        .expect("runs > 0")
}

#[test]
#[ignore = "timing budget; meaningful only with --release"]
fn parse_100k_objects_within_budget() {
    let text = design(100_000, 1);
    let parse_time = best_of(5, || {
        parse(text.as_bytes())
            .expect("generated file parses")
            .record_count()
    });
    let doc = parse(text.as_bytes()).unwrap();
    let write_time = best_of(5, || write(&doc).len());
    let merge_time = best_of(3, || merge3(&doc, &doc, &doc).conflicts);
    println!(
        "100k objects, {} MB: parse {parse_time:?}, write {write_time:?}, merge3 {merge_time:?}",
        text.len() / 1_000_000
    );
    assert_eq!(doc.record_count(), 100_001);
    assert!(
        parse_time < Duration::from_millis(100),
        "parse took {parse_time:?}"
    );
}
