//! D39 budget: open a 100k-object `.shcomp` in ≤ 200 ms (and save within the same
//! budget). Load + save measured at ~125 ms on the dev machine.
//! Run: `cargo test -p schemify-doc --release --test perf -- --ignored --nocapture`.

mod common;

use common::{Mix, coord};
use schemify_doc::{Edit, Op, Outcome, Project, Target, apply_edits};
use schemify_format::{Document as FormatDoc, FileKind, RecordKind, write};
use std::time::{Duration, Instant};

// ---- private ----

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
    for (key, d) in [("open", load), ("save", save), ("edit_undo", edit)] {
        eprintln!("PERF doc.{key}_100k_ms {:.3}", d.as_secs_f64() * 1e3);
    }
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

/// D39 (HANDOFF latent bug 5): at 100k objects a commit's frame does O(change) work; the
/// O(document) work commits once did inline runs in idle steps (`Project::idle_step`).
/// Measures, per case, the commit frame with the steps keeping up and the work the steps
/// took off it (which a commit did in its frame before): the disk hash a first commit's
/// journal names, a compaction once garbage outweighs live data, and the history a new
/// document's first journal encodes.
#[test]
#[ignore = "release-mode benchmark"]
fn commit_frames_leave_document_work_to_idle_steps_100k() {
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let timed = |f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        t.elapsed()
    };
    let text = design(100_000, 1);
    let mut p = Project::new(None, None, 1).unwrap();
    let h = p
        .open("", "big.shcomp", text.as_bytes(), None)
        .unwrap()
        .handle;
    let ids: Vec<String> = {
        let d = p.doc(h).unwrap();
        d.instances()
            .id
            .iter()
            .map(|i| i.text(d.strings()))
            .collect()
    };
    let set = |id: &str, w: usize| Op::SetAttr {
        target: Target::new(RecordKind::Inst, id),
        key: "W".into(),
        values: vec![format!("{w}u")],
    };
    let commit = |p: &mut Project, ops: Vec<Op>| {
        let t = Instant::now();
        let out = apply_edits(p, &[common::req(h, common::tx(ops))]);
        assert!(matches!(out[0], Outcome::Committed { .. }), "{out:?}");
        t.elapsed()
    };
    // 1. The first commit after the open, before any idle step hashed the file.
    let first = commit(&mut p, vec![set(&ids[7], 1)]);
    assert!(p.hash_pending(h), "the commit left the hash to the steps");
    let hash = timed(&mut || p.settle_journal(h));
    // 2. Commits of 1000 instance edits until garbage outweighs live data; the steps run
    //    between them.
    let (mut worst, mut commits, mut compact) = (Duration::ZERO, 0, Duration::ZERO);
    while compact.is_zero() {
        let ops = ids.iter().take(1000).map(|id| set(id, commits)).collect();
        worst = worst.max(commit(&mut p, ops));
        commits += 1;
        while p.idle_pending(Some(h)) {
            let t = Instant::now();
            if p.idle_step(Some(h), 128 << 10) == Some(h) {
                compact = t.elapsed();
            }
        }
        assert!(commits < 1 << 12, "garbage grows");
    }
    // 3. A new document of 100k wires, saved: its first journal holds that history.
    let wire = |i: u32| {
        schemify_doc::RecordBuf::new(RecordKind::Wire)
            .field(&format!("{:06}", i))
            .field(&coord(i as i32 * 10_000))
            .field("0")
            .field(&coord(i as i32 * 10_000 + 10_000))
            .field("0")
    };
    let mut q = Project::new(None, None, 1).unwrap();
    let mut history = [Duration::ZERO; 2];
    for (k, idle) in [false, true].into_iter().enumerate() {
        let n = q
            .create("", &format!("new{k}.shcomp"), FileKind::Shcomp)
            .unwrap();
        let imp = schemify_doc::RecordBuf::new(RecordKind::Impl)
            .field("sch")
            .field("schematic");
        let mut ops = vec![Op::Insert { rec: imp, at: None }];
        ops.extend((0..100_000).map(|i| Op::Insert {
            rec: wire(i),
            at: None,
        }));
        let out = apply_edits(&mut q, &[common::req(n, common::tx(ops))]);
        assert!(matches!(out[0], Outcome::Committed { .. }));
        let req = q.save(n).unwrap().unwrap();
        q.saved(n, req.bytes).unwrap();
        while idle && q.idle_pending(Some(n)) {
            q.idle_step(Some(n), 128 << 10);
        }
        let t = Instant::now();
        let out = apply_edits(
            &mut q,
            &[common::req(
                n,
                common::tx(vec![Op::Insert {
                    rec: wire(200_000),
                    at: None,
                }]),
            )],
        );
        assert!(matches!(out[0], Outcome::Committed { .. }));
        history[k] = t.elapsed();
    }
    eprintln!(
        "first commit {:.3} ms (+{:.3} ms of hash it no longer takes); {commits} commits of \
         1000 edits, worst {:.3} ms (+{:.3} ms compaction in an idle step); first journal \
         of 100k-wire history: commit {:.3} ms after idle steps, {:.3} ms without",
        ms(first),
        ms(hash),
        ms(worst),
        ms(compact),
        ms(history[1]),
        ms(history[0])
    );
    for (key, d) in [
        ("first_commit", first),
        ("hash_moved", hash),
        ("compaction_moved", compact),
        ("first_journal_commit", history[1]),
        ("first_journal_moved", history[0]),
    ] {
        eprintln!("PERF doc.{key}_100k_ms {:.3}", ms(d));
    }
}
