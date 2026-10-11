//! Stateful model-based tests (TESTING.md `doc` row): random sequences of transactions,
//! undo, redo, stage, accept and reject against the naive model in `common`; undo-all
//! equals the original; redo-all equals the forward state; load(save(x)) == x along
//! the way.

mod common;

use common::*;
use proptest::prelude::*;
use schemify_doc::{
    Code, Edit, Mode, Op, Outcome, Project, Transaction, apply_edits, apply_edits_report,
};

fn stage_tx(ops: Vec<Op>) -> Edit {
    Edit::Tx(Transaction {
        label: "staged".into(),
        mode: Mode::Stage,
        ops,
    })
}

const REJECT_CODES: [Code; 6] = [
    Code::E024,
    Code::E026,
    Code::E029,
    Code::E040,
    Code::E041,
    Code::E043,
];

fn check_rejection(out: &Outcome, accepted: bool, ctx: &str) {
    match out {
        Outcome::Committed { .. } | Outcome::Staged { .. } => {
            assert!(accepted, "doc accepted, model rejected: {ctx}")
        }
        Outcome::Rejected(errs) => {
            assert!(!accepted, "doc rejected {errs:?}, model accepted: {ctx}");
            assert!(!errs.is_empty());
            for e in errs {
                assert!(REJECT_CODES.contains(&e.code), "unexpected {e:?}: {ctx}");
            }
        }
        other => panic!("unexpected outcome {other:?}: {ctx}"),
    }
}

fn reload_equal(p: &Project, h: schemify_doc::DocHandle, ctx: &str) {
    let text = canon(p, h);
    let (q, g) = open(&text, "cell.shcomp");
    assert_eq!(canon(&q, g), text, "load(save(x)) != x: {ctx}");
}

fn check_model(seed: u64, steps: usize) {
    let mut rng = Mix::new(seed);
    let (mut p, h) = open(SEED_DOC, "cell.shcomp");
    let original = canon(&p, h);
    let mut model = Model::from_doc(p.doc(h).unwrap());
    let (mut undo, mut redo): (Vec<Model>, Vec<Model>) = (Vec::new(), Vec::new());
    let mut stages: Vec<(u16, Vec<Op>)> = Vec::new();
    let (mut commits, mut rejects) = (0u32, 0u32);
    for step in 0..steps {
        let ctx = format!("seed {seed} step {step}");
        match rng.below(12) {
            0..=6 => {
                let ops: Vec<Op> = (0..1 + rng.below(3))
                    .map(|_| gen_op(&mut rng, p.doc(h).unwrap()))
                    .collect();
                let before = model.clone();
                let accepted = model.apply_tx(&ops);
                let out = run(&mut p, h, tx(ops.clone()));
                check_rejection(&out, accepted, &format!("{ctx} {ops:?}"));
                commits += u32::from(accepted);
                rejects += u32::from(!accepted);
                if accepted {
                    undo.push(before);
                    redo.clear();
                }
            }
            7 => match (run(&mut p, h, Edit::Undo), undo.pop()) {
                (Outcome::Undone { .. }, Some(m)) => redo.push(std::mem::replace(&mut model, m)),
                (Outcome::Nothing, None) => {}
                (o, m) => panic!("undo {o:?} vs model {}: {ctx}", m.is_some()),
            },
            8 => match (run(&mut p, h, Edit::Redo), redo.pop()) {
                (Outcome::Redone { .. }, Some(m)) => undo.push(std::mem::replace(&mut model, m)),
                (Outcome::Nothing, None) => {}
                (o, m) => panic!("redo {o:?} vs model {}: {ctx}", m.is_some()),
            },
            9 => {
                let ops: Vec<Op> = (0..1 + rng.below(3))
                    .map(|_| gen_op(&mut rng, p.doc(h).unwrap()))
                    .collect();
                let accepted = model.clone().apply_tx(&ops);
                let out = run(&mut p, h, stage_tx(ops.clone()));
                check_rejection(&out, accepted, &ctx);
                if let Outcome::Staged { stage } = out {
                    assert_eq!(p.preview(h, stage).unwrap().canonical(), {
                        let mut m = model.clone();
                        assert!(m.apply_tx(&ops));
                        m.canonical()
                    });
                    stages.push((stage, ops));
                }
            }
            _ if stages.is_empty() => {}
            10 => {
                let (id, ops) = stages.remove(rng.below(stages.len() as u64) as usize);
                let before = model.clone();
                let accepted = model.apply_tx(&ops);
                let out = run(&mut p, h, Edit::Accept(id));
                check_rejection(&out, accepted, &ctx);
                if accepted {
                    undo.push(before);
                    redo.clear();
                } else {
                    stages.push((id, ops)); // a rejected accept keeps the stage
                }
            }
            _ => {
                let (id, _) = stages.remove(rng.below(stages.len() as u64) as usize);
                assert_eq!(run(&mut p, h, Edit::Reject(id)), Outcome::Dropped, "{ctx}");
            }
        }
        assert_eq!(canon(&p, h), model.canonical(), "{ctx}");
        assert_eq!(p.history_len(h), (undo.len(), redo.len()), "{ctx}");
        assert_eq!(p.stages(h).len(), stages.len(), "{ctx}");
        if step % 16 == 0 {
            reload_equal(&p, h, &ctx);
        }
    }
    // The generator must exercise both paths, or the comparison proves nothing.
    assert!(
        commits > steps as u32 / 8 && rejects > 0,
        "seed {seed}: {commits} commits, {rejects} rejects"
    );
    let forward = canon(&p, h);
    let depth = undo.len();
    for _ in 0..depth {
        assert!(matches!(run(&mut p, h, Edit::Undo), Outcome::Undone { .. }));
    }
    // The bottom of the undo stack is always the original: the stack only grows from
    // the state it had when empty.
    assert_eq!(canon(&p, h), original, "undo-all, seed {seed}");
    assert_eq!(run(&mut p, h, Edit::Undo), Outcome::Nothing);
    for _ in 0..depth {
        assert!(matches!(run(&mut p, h, Edit::Redo), Outcome::Redone { .. }));
    }
    assert_eq!(canon(&p, h), forward, "redo-all, seed {seed}");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn edits_match_the_naive_model(seed: u64) {
        check_model(seed, 150);
    }
}

/// `apply_edits_report` oracle: the reported ops, committed as one transaction to a
/// mirror document that saw every earlier report, reproduce the edited document after
/// every request (commits, undo, redo, stage, accept, reject, rejections); requests
/// that change nothing report no ops.
#[test]
fn reported_ops_replay_to_the_same_document() {
    for seed in 0..24 {
        let mut rng = Mix::new(seed);
        let (mut p, h) = open(SEED_DOC, "cell.shcomp");
        let (mut mirror, m) = open(SEED_DOC, "cell.shcomp");
        let mut stages: Vec<u16> = Vec::new();
        let (mut changed, mut unchanged) = (0u32, 0u32);
        for step in 0..120 {
            let ctx = format!("seed {seed} step {step}");
            let edit = match rng.below(10) {
                0..=4 => tx((0..1 + rng.below(3))
                    .map(|_| gen_op(&mut rng, p.doc(h).unwrap()))
                    .collect()),
                5 => Edit::Undo,
                6 => Edit::Redo,
                7 => stage_tx(vec![gen_op(&mut rng, p.doc(h).unwrap())]),
                8 if !stages.is_empty() => Edit::Accept(stages.remove(0)),
                _ => Edit::Reject(stages.pop().unwrap_or(u16::MAX)),
            };
            let before = canon(&p, h);
            let applied = apply_edits_report(&mut p, &[req(h, edit)]).remove(0);
            if let Outcome::Staged { stage } = applied.outcome {
                stages.push(stage);
            }
            let after = canon(&p, h);
            if applied.ops.is_empty() {
                assert_eq!(before, after, "no ops, no change: {ctx} {applied:?}");
                unchanged += 1;
                continue;
            }
            assert!(
                matches!(
                    applied.outcome,
                    Outcome::Committed { .. } | Outcome::Undone { .. } | Outcome::Redone { .. }
                ),
                "{ctx} {applied:?}"
            );
            let out = run(&mut mirror, m, tx(applied.ops.clone()));
            assert!(matches!(out, Outcome::Committed { .. }), "{ctx} {out:?}");
            assert_eq!(canon(&mirror, m), after, "{ctx} {applied:?}");
            changed += 1;
        }
        assert!(
            changed > 20 && unchanged > 0,
            "seed {seed}: {changed}/{unchanged}"
        );
    }
    // The plain entry point reports the same outcomes.
    let (mut p, h) = open(SEED_DOC, "cell.shcomp");
    let (mut q, g) = open(SEED_DOC, "cell.shcomp");
    let mut rng = Mix::new(99);
    for _ in 0..40 {
        let ops = vec![gen_op(&mut rng, p.doc(h).unwrap())];
        let a = apply_edits(&mut p, &[req(h, tx(ops.clone()))]).remove(0);
        let b = apply_edits_report(&mut q, &[req(g, tx(ops))]).remove(0);
        assert_eq!(a, b.outcome);
    }
    assert_eq!(canon(&p, h), canon(&q, g));
}

#[test]
fn undo_all_returns_to_the_original_and_redo_all_to_the_forward_state() {
    for seed in 0..16 {
        let mut rng = Mix::new(seed);
        let (mut p, h) = open(SEED_DOC, "cell.shcomp");
        let original = canon(&p, h);
        let mut commits = 0;
        for _ in 0..60 {
            let ops = vec![gen_op(&mut rng, p.doc(h).unwrap())];
            if matches!(run(&mut p, h, tx(ops)), Outcome::Committed { .. }) {
                commits += 1;
            }
        }
        let forward = canon(&p, h);
        for _ in 0..commits {
            assert!(matches!(run(&mut p, h, Edit::Undo), Outcome::Undone { .. }));
        }
        assert_eq!(run(&mut p, h, Edit::Undo), Outcome::Nothing);
        assert_eq!(canon(&p, h), original, "seed {seed}");
        assert!(!p.is_dirty(h));
        for _ in 0..commits {
            assert!(matches!(run(&mut p, h, Edit::Redo), Outcome::Redone { .. }));
        }
        assert_eq!(canon(&p, h), forward, "seed {seed}");
    }
}

#[test]
fn a_new_commit_clears_redo_and_history_is_per_document() {
    let mut p = Project::new(None, None, 1).unwrap();
    let a = p
        .open("", "a.shcomp", SEED_DOC.as_bytes(), None)
        .unwrap()
        .handle;
    let b = p
        .open("", "b.shcomp", SEED_DOC.as_bytes(), None)
        .unwrap()
        .handle;
    let wire = |id: &str| {
        schemify_doc::RecordBuf::new(schemify_format::RecordKind::Wire)
            .field(id)
            .field("0")
            .field("0")
            .field("10")
            .field("0")
    };
    let ins = |id: &str| {
        tx(vec![Op::Insert {
            rec: wire(id),
            at: None,
        }])
    };
    assert!(matches!(
        run(&mut p, a, ins("aaaaaa")),
        Outcome::Committed { .. }
    ));
    assert!(matches!(
        run(&mut p, b, ins("bbbbbb")),
        Outcome::Committed { .. }
    ));
    assert!(matches!(run(&mut p, a, Edit::Undo), Outcome::Undone { .. }));
    assert_eq!(p.history_len(a), (0, 1));
    assert_eq!(
        p.history_len(b),
        (1, 0),
        "b's history is untouched by a's undo"
    );
    assert!(matches!(
        run(&mut p, a, ins("cccccc")),
        Outcome::Committed { .. }
    ));
    assert_eq!(p.history_len(a), (1, 0), "a commit clears redo");
    assert_eq!(run(&mut p, a, Edit::Redo), Outcome::Nothing);
}
