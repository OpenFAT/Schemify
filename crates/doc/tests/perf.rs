//! D39 budget: open a 100k-object `.shcomp` in ≤ 200 ms (and save within the same
//! budget). Load + save measured at ~125 ms on the dev machine.
//! Run: `cargo test -p schemify-doc --release --test perf -- --ignored --nocapture`.

mod common;

use common::{Mix, coord};
use schemify_doc::{Edit, Op, Outcome, Project, Target, apply_edits};
use schemify_format::{Document as FormatDoc, FileKind, RecordKind, write};
use std::time::{Duration, Instant};

/// The research §1 synthetic mix: 35% instances (name + 5 params), 45% wires,
/// 12% labels, 8% texts.
fn design(n: usize, seed: u64) -> String {
    let mut rng = Mix::new(seed);
    let side = ((n as f64).sqrt() * 15.0) as u64 + 1;
    let milli = |rng: &mut Mix| (rng.below(side) as i32 - side as i32 / 2) * 10_000;
    let grid = |rng: &mut Mix| coord(milli(rng));
    let mut d = FormatDoc::new(FileKind::Shcomp);
    d.begin_record(RecordKind::Impl);
    d.push_field("sch");
    d.push_field("schematic");
    for i in 0..n {
        let id = {
            // Multiplying by an odd constant is a bijection mod 2^30: unique, scattered ids.
            const A: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
            let v = (i as u64).wrapping_mul(0x9e37_79b1) & ((1 << 30) - 1);
            (0..6)
                .rev()
                .map(|k| A[((v >> (5 * k)) & 31) as usize] as char)
                .collect::<String>()
        };
        match i * 100 / n {
            0..35 => {
                d.begin_record(RecordKind::Inst);
                d.push_field(&id);
                d.push_field("sky130:nfet_01v8");
                d.push_attr("at");
                d.push_value(&grid(&mut rng));
                d.push_value(&grid(&mut rng));
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
                let (x, y) = (milli(&mut rng), milli(&mut rng));
                d.push_field_coord(x);
                d.push_field_coord(y);
                d.push_field_coord(x + 100_000);
                d.push_field_coord(y);
            }
            80..92 => {
                d.begin_record(RecordKind::Label);
                d.push_field(&id);
                d.push_field(&grid(&mut rng));
                d.push_field(&grid(&mut rng));
                d.push_field("R0");
                d.push_field(&format!("net{}", i % 997));
            }
            _ => {
                d.begin_record(RecordKind::Text);
                d.push_field(&id);
                d.push_field(&grid(&mut rng));
                d.push_field(&grid(&mut rng));
                d.push_field("R0");
                d.push_field("bias for stage");
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
        .unwrap()
}

#[test]
#[ignore = "release-mode benchmark"]
fn load_and_save_100k_objects_within_budget() {
    let text = design(100_000, 1);
    let mut p = Project::new(None, None, 1).unwrap();
    let h = p
        .open("", "big.shcomp", text.as_bytes(), None)
        .unwrap()
        .handle;
    assert!(
        p.doc(h).unwrap().canonical() == text,
        "the design round-trips"
    );
    p.close(h).unwrap();
    let parse = best_of(5, || {
        schemify_format::parse(text.as_bytes())
            .unwrap()
            .record_count()
    });
    let load = best_of(5, || {
        let h = p
            .open("", "big.shcomp", text.as_bytes(), None)
            .unwrap()
            .handle;
        p.close(h).unwrap()
    });
    let h = p
        .open("", "big.shcomp", text.as_bytes(), None)
        .unwrap()
        .handle;
    let save = best_of(5, || p.doc(h).unwrap().canonical().len());
    let edit = best_of(5, || {
        let mv = Op::SetAttr {
            target: Target::new(
                RecordKind::Inst,
                &p.doc(h).unwrap().instances().id[7].text(p.doc(h).unwrap().strings()),
            ),
            key: "at".into(),
            values: vec!["10".into(), "20".into(), "R0".into()],
        };
        let out = apply_edits(&mut p, &[common::req(h, common::tx(vec![mv]))]);
        assert!(matches!(out[0], Outcome::Committed { .. }));
        apply_edits(&mut p, &[common::req(h, Edit::Undo)])
    });
    let total = load + save;
    eprintln!(
        "100k objects, {} KiB: format parse {parse:?}; open (parse + tables) {load:?}; save (tables + write) {save:?}; total {total:?}; one edit + undo {edit:?}",
        text.len() / 1024
    );
    assert!(
        load < Duration::from_millis(200),
        "open took {load:?} (D39: ≤ 200 ms)"
    );
    assert!(
        save < Duration::from_millis(200),
        "save took {save:?} (D39: ≤ 200 ms)"
    );
}

/// One open + one save, for `valgrind --tool=callgrind` (instruction counts do not
/// depend on machine load).
#[test]
#[ignore = "profiling target"]
fn profile_once() {
    let text = design(20_000, 1);
    let mut p = Project::new(None, None, 1).unwrap();
    let h = p
        .open("", "big.shcomp", text.as_bytes(), None)
        .unwrap()
        .handle;
    assert!(p.doc(h).unwrap().canonical().len() == text.len());
}

#[test]
#[ignore = "phase breakdown"]
fn phases() {
    let text = design(100_000, 1);
    let model = schemify_format::parse(text.as_bytes()).unwrap();
    let sha = best_of(5, || schemify_doc::sha256(text.as_bytes()));
    let mut p = Project::new(None, None, 1).unwrap();
    let h = p
        .open("", "big.shcomp", text.as_bytes(), None)
        .unwrap()
        .handle;
    let d = p.doc(h).unwrap();
    let canon = best_of(5, || d.canonical().len());
    let wr = best_of(5, || schemify_format::write(&model).len());
    eprintln!("PHASES sha256 {sha:?}; canonical {canon:?}; format write of parsed model {wr:?}");
}
