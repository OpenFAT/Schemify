//! Stateful model-based tests (docs/dev/testing.md `doc` row): random sequences of transactions,
//! undo, redo, stage, accept and reject against the naive model in `common`; reported
//! ops replay to the same document; the change log names every row write; load(save(x))
//! == x along the way. Undo history on its own: `history.rs`.

mod common;

use common::model::Model;
use common::{Mix, SEED_DOC};
use schemify_doc::{Code, Edit, Mode, Op, Outcome, Project, Transaction};

const REJECT_CODES: [Code; 6] = [
    Code::E024,
    Code::E026,
    Code::E029,
    Code::E040,
    Code::E041,
    Code::E043,
];

// ---- private ----

fn stage_tx(ops: Vec<Op>) -> Edit {
    Edit::Tx(Transaction {
        label: "staged".into(),
        mode: Mode::Stage,
        ops,
    })
}

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
    let text = common::canon(p, h);
    let (q, g) = common::open(&text, "cell.shcomp");
    assert_eq!(common::canon(&q, g), text, "load(save(x)) != x: {ctx}");
}

fn check_model(seed: u64, steps: usize) {
    let mut rng = Mix::new(seed);
    let (mut p, h) = common::open(SEED_DOC, "cell.shcomp");
    let original = common::canon(&p, h);
    let mut model = Model::from_doc(common::doc(&p, h));
    let (mut undo, mut redo): (Vec<Model>, Vec<Model>) = (Vec::new(), Vec::new());
    let mut stages: Vec<(u16, Vec<Op>)> = Vec::new();
    let (mut commits, mut rejects) = (0u32, 0u32);
    for step in 0..steps {
        let ctx = format!("seed {seed} step {step}");
        match rng.below(12) {
            0..=6 => {
                let ops: Vec<Op> = (0..1 + rng.below(3))
                    .map(|_| common::gen_op(&mut rng, common::doc(&p, h)))
                    .collect();
                let before = model.clone();
                let accepted = model.apply_tx(&ops);
                let out = common::run(&mut p, h, common::tx(ops.clone()));
                check_rejection(&out, accepted, &format!("{ctx} {ops:?}"));
                commits += u32::from(accepted);
                rejects += u32::from(!accepted);
                if accepted {
                    undo.push(before);
                    redo.clear();
                }
            }
            7 => match (common::run(&mut p, h, Edit::Undo), undo.pop()) {
                (Outcome::Undone { .. }, Some(m)) => redo.push(std::mem::replace(&mut model, m)),
                (Outcome::Nothing, None) => {}
                (o, m) => panic!("undo {o:?} vs model {}: {ctx}", m.is_some()),
            },
            8 => match (common::run(&mut p, h, Edit::Redo), redo.pop()) {
                (Outcome::Redone { .. }, Some(m)) => undo.push(std::mem::replace(&mut model, m)),
                (Outcome::Nothing, None) => {}
                (o, m) => panic!("redo {o:?} vs model {}: {ctx}", m.is_some()),
            },
            9 => {
                let ops: Vec<Op> = (0..1 + rng.below(3))
                    .map(|_| common::gen_op(&mut rng, common::doc(&p, h)))
                    .collect();
                let accepted = model.clone().apply_tx(&ops);
                let out = common::run(&mut p, h, stage_tx(ops.clone()));
                check_rejection(&out, accepted, &ctx);
                if let Outcome::Staged { stage } = out {
                    assert_eq!(
                        p.preview(h, stage)
                            .expect("a stage the edit just returned has a preview")
                            .canonical(),
                        {
                            let mut m = model.clone();
                            assert!(m.apply_tx(&ops));
                            m.canonical()
                        }
                    );
                    stages.push((stage, ops));
                }
            }
            _ if stages.is_empty() => {}
            10 => {
                let (id, ops) = stages.remove(rng.below(stages.len() as u64) as usize);
                let before = model.clone();
                let accepted = model.apply_tx(&ops);
                let out = common::run(&mut p, h, Edit::Accept(id));
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
                assert_eq!(
                    common::run(&mut p, h, Edit::Reject(id)),
                    Outcome::Dropped,
                    "{ctx}"
                );
            }
        }
        assert_eq!(common::canon(&p, h), model.canonical(), "{ctx}");
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
    let forward = common::canon(&p, h);
    let depth = undo.len();
    for _ in 0..depth {
        assert!(matches!(
            common::run(&mut p, h, Edit::Undo),
            Outcome::Undone { .. }
        ));
    }
    // The bottom of the undo stack is always the original: the stack only grows from
    // the state it had when empty.
    assert_eq!(common::canon(&p, h), original, "undo-all, seed {seed}");
    assert_eq!(common::run(&mut p, h, Edit::Undo), Outcome::Nothing);
    for _ in 0..depth {
        assert!(matches!(
            common::run(&mut p, h, Edit::Redo),
            Outcome::Redone { .. }
        ));
    }
    assert_eq!(common::canon(&p, h), forward, "redo-all, seed {seed}");
}

/// Property over generated inputs: 48 seeds drawn from one fixed stream, so a failure
/// replays from the printed seed.
#[test]
fn edits_match_the_naive_model() {
    let mut seeds = Mix::new(0x5eed);
    for _ in 0..48 {
        let seed = seeds.next();
        check_model(seed, 150);
    }
}

/// `Project::applied` oracle: the reported ops, committed as one transaction to a
/// mirror document that saw every earlier report, reproduce the edited document after
/// every request (commits, undo, redo, stage, accept, reject, rejections); requests
/// that change nothing report no ops.
#[test]
fn reported_ops_replay_to_the_same_document() {
    for seed in 0..24 {
        let mut rng = Mix::new(seed);
        let (mut p, h) = common::open(SEED_DOC, "cell.shcomp");
        let (mut mirror, m) = common::open(SEED_DOC, "cell.shcomp");
        let mut stages: Vec<u16> = Vec::new();
        let (mut changed, mut unchanged) = (0u32, 0u32);
        for step in 0..120 {
            let ctx = format!("seed {seed} step {step}");
            let edit = match rng.below(10) {
                0..=4 => common::tx(
                    (0..1 + rng.below(3))
                        .map(|_| common::gen_op(&mut rng, p.doc(h).unwrap()))
                        .collect(),
                ),
                5 => Edit::Undo,
                6 => Edit::Redo,
                7 => stage_tx(vec![common::gen_op(&mut rng, p.doc(h).unwrap())]),
                8 if !stages.is_empty() => Edit::Accept(stages.remove(0)),
                _ => Edit::Reject(stages.pop().unwrap_or(u16::MAX)),
            };
            let before = common::canon(&p, h);
            let outcome = schemify_doc::apply_edits(&mut p, &[common::req(h, edit)]).remove(0);
            if let Outcome::Staged { stage } = outcome {
                stages.push(stage);
            }
            let after = common::canon(&p, h);
            let design = matches!(
                outcome,
                Outcome::Committed { .. } | Outcome::Undone { .. } | Outcome::Redone { .. }
            );
            let Some(ops) = p.applied(h, &outcome) else {
                assert!(
                    !design,
                    "a design change reports its ops: {ctx} {outcome:?}"
                );
                assert_eq!(before, after, "no ops, no change: {ctx} {outcome:?}");
                unchanged += 1;
                continue;
            };
            assert!(design, "{ctx} {outcome:?}");
            let out = common::run(&mut mirror, m, common::tx(ops.to_vec()));
            assert!(matches!(out, Outcome::Committed { .. }), "{ctx} {out:?}");
            assert_eq!(common::canon(&mirror, m), after, "{ctx} {outcome:?}");
            changed += 1;
        }
        assert!(
            changed > 20 && unchanged > 0,
            "seed {seed}: {changed}/{unchanged}"
        );
    }
}

/// Change log oracle (`Document::changes_since`): a mirror of every table's rows that
/// re-reads only the rows the log names (and drops rows past each table's length) equals a
/// fresh read of every row after every request (commits, undo, redo, stage, accept,
/// reject, rejections); the ids it marks deleted are exactly the ids gone from the
/// document, and a cursor of another document, or of a copy, reads nothing.
#[test]
fn the_change_log_names_every_row_write() {
    use schemify_doc::{Document, Loc, ObjId, TableId, Write};
    use std::collections::BTreeSet;
    let rows_of = |d: &Document, t: TableId| -> Vec<String> {
        (0..d.table_len(t) as u32)
            .map(|row| format!("{:?}", d.record(Loc { table: t, row })))
            .collect()
    };
    let ids_of = |d: &Document| -> BTreeSet<u32> {
        TableId::ALL
            .iter()
            .flat_map(|&t| (0..d.table_len(t) as u32).map(move |row| (t, row)))
            .filter_map(|(t, row)| {
                let r = d.record(Loc { table: t, row });
                let f = r.fields.first()?;
                ObjId::find(f, d.strings()).filter(|_| r.kind.key() == schemify_format::KeyRule::Id)
            })
            .map(|id| id.0)
            .collect()
    };
    let (mut followed, mut deleted) = (0u32, 0u32);
    for seed in 0..16 {
        let mut rng = Mix::new(seed);
        let (mut p, h) = common::open(SEED_DOC, "cell.shcomp");
        let doc = p.doc(h).unwrap();
        let mut mirror: Vec<Vec<String>> = TableId::ALL.iter().map(|&t| rows_of(doc, t)).collect();
        let mut cursor = doc.cursor();
        let mut stages: Vec<u16> = Vec::new();
        for step in 0..120 {
            let ctx = format!("seed {seed} step {step}");
            let edit = match rng.below(10) {
                0..=4 => common::tx(
                    (0..1 + rng.below(3))
                        .map(|_| common::gen_op(&mut rng, p.doc(h).unwrap()))
                        .collect(),
                ),
                5 => Edit::Undo,
                6 => Edit::Redo,
                7 => stage_tx(vec![common::gen_op(&mut rng, p.doc(h).unwrap())]),
                8 if !stages.is_empty() => Edit::Accept(stages.remove(0)),
                _ => Edit::Reject(stages.pop().unwrap_or(u16::MAX)),
            };
            let ids_before = ids_of(p.doc(h).unwrap());
            if let Outcome::Staged { stage } =
                schemify_doc::apply_edits(&mut p, &[common::req(h, edit)]).remove(0)
            {
                stages.push(stage);
            }
            let d = p.doc(h).unwrap();
            let changes = d
                .changes_since(cursor)
                .expect("the log reaches the last frame");
            let mut gone = BTreeSet::new();
            for c in changes {
                let ti = TableId::ALL.iter().position(|&t| t == c.table).unwrap();
                let len = d.table_len(c.table);
                let m = &mut mirror[ti];
                m.resize(m.len().max(len), String::new());
                if (c.row as usize) < len {
                    m[c.row as usize] = format!(
                        "{:?}",
                        d.record(Loc {
                            table: c.table,
                            row: c.row
                        })
                    );
                }
                match c.what {
                    Write::Deleted => {
                        gone.insert(c.id.0);
                    }
                    Write::Content => {
                        gone.remove(&c.id.0);
                    }
                    Write::Moved => {}
                }
            }
            for (ti, &t) in TableId::ALL.iter().enumerate() {
                mirror[ti].truncate(d.table_len(t));
                assert_eq!(mirror[ti], rows_of(d, t), "{ctx}: {t:?} rows");
            }
            let ids_after = ids_of(d);
            let left: BTreeSet<u32> = ids_before.difference(&ids_after).copied().collect();
            assert!(left.is_subset(&gone), "{ctx}: every deleted id is logged");
            assert!(
                gone.is_disjoint(&ids_after),
                "{ctx}: a logged deletion is gone"
            );
            followed += u32::from(!changes.is_empty());
            deleted += left.len() as u32;
            cursor = d.cursor();
            assert_eq!(d.changes_since(cursor).map(<[_]>::len), Some(0));
        }
        // Negative space: another document's cursor, and a copy's, read nothing.
        let (q, g) = common::open(SEED_DOC, "cell.shcomp");
        assert!(
            p.doc(h)
                .unwrap()
                .changes_since(q.doc(g).unwrap().cursor())
                .is_none()
        );
        let copy = p.doc(h).unwrap().clone();
        assert!(
            copy.changes_since(cursor).is_none(),
            "a copy is another log"
        );
    }
    assert!(
        followed > 400 && deleted > 50,
        "{followed} followed, {deleted} deleted"
    );
}
