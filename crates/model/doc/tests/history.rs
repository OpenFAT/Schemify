//! Undo history tests (docs/dev/testing.md `doc` row): what `applied` reports, undo-all
//! and redo-all against the recorded states, redo cleared by a new commit, history per
//! document, the undo cap and the saved depth, and the garbage replaced rows leave.

mod common;

use common::{Mix, SEED_DOC};
use schemify_doc::{Edit, Op, Outcome, Project, RecordKind, Target, UNDO_MAX};

// ---- private ----

/// `Project::applied` negative space: an undone entry's ops are gone once a later commit
/// clears redo, and a stale handle reports nothing.
#[test]
fn applied_forgets_cleared_entries_and_stale_handles() {
    let (mut p, h) = common::open(SEED_DOC, "cell.shcomp");
    let mut rng = Mix::new(7);
    let committed = loop {
        let op = common::gen_op(&mut rng, p.doc(h).unwrap());
        let o = common::run(&mut p, h, common::tx(vec![op]));
        if matches!(o, Outcome::Committed { .. }) {
            break o;
        }
    };
    let undone = common::run(&mut p, h, Edit::Undo);
    assert!(p.applied(h, &undone).is_some_and(|ops| !ops.is_empty()));
    assert!(
        p.applied(h, &committed).is_none(),
        "undone: off the undo stack"
    );
    loop {
        let op = common::gen_op(&mut rng, p.doc(h).unwrap());
        let o = common::run(&mut p, h, common::tx(vec![op]));
        if matches!(o, Outcome::Committed { .. }) {
            assert!(p.applied(h, &o).is_some());
            break;
        }
    }
    assert!(
        p.applied(h, &undone).is_none(),
        "redo cleared by the commit"
    );
    assert!(p.applied(h, &Outcome::Nothing).is_none());
    assert!(p.close(h).is_ok());
    assert!(p.applied(h, &Outcome::Committed { seq: 0 }).is_none());
}

#[test]
fn undo_all_returns_to_the_original_and_redo_all_to_the_forward_state() {
    for seed in 0..16 {
        let mut rng = Mix::new(seed);
        let (mut p, h) = common::open(SEED_DOC, "cell.shcomp");
        let original = common::canon(&p, h);
        let mut commits = 0;
        for _ in 0..60 {
            let ops = vec![common::gen_op(&mut rng, p.doc(h).unwrap())];
            if matches!(
                common::run(&mut p, h, common::tx(ops)),
                Outcome::Committed { .. }
            ) {
                commits += 1;
            }
        }
        let forward = common::canon(&p, h);
        for _ in 0..commits {
            assert!(matches!(
                common::run(&mut p, h, Edit::Undo),
                Outcome::Undone { .. }
            ));
        }
        assert_eq!(common::run(&mut p, h, Edit::Undo), Outcome::Nothing);
        assert_eq!(common::canon(&p, h), original, "seed {seed}");
        assert!(!p.is_dirty(h));
        for _ in 0..commits {
            assert!(matches!(
                common::run(&mut p, h, Edit::Redo),
                Outcome::Redone { .. }
            ));
        }
        assert_eq!(common::canon(&p, h), forward, "seed {seed}");
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
        common::tx(vec![Op::Insert {
            rec: wire(id),
            at: None,
        }])
    };
    assert!(matches!(
        common::run(&mut p, a, ins("aaaaaa")),
        Outcome::Committed { .. }
    ));
    assert!(matches!(
        common::run(&mut p, b, ins("bbbbbb")),
        Outcome::Committed { .. }
    ));
    assert!(matches!(
        common::run(&mut p, a, Edit::Undo),
        Outcome::Undone { .. }
    ));
    assert_eq!(p.history_len(a), (0, 1));
    assert_eq!(
        p.history_len(b),
        (1, 0),
        "b's history is untouched by a's undo"
    );
    assert!(matches!(
        common::run(&mut p, a, ins("cccccc")),
        Outcome::Committed { .. }
    ));
    assert_eq!(p.history_len(a), (1, 0), "a commit clears redo");
    assert_eq!(common::run(&mut p, a, Edit::Redo), Outcome::Nothing);
}

/// Undo cap oracle: past `UNDO_MAX` commits the oldest entry drops and the saved depth
/// follows it down, so undoing back to the saved state (and only there) is clean.
#[test]
fn the_undo_cap_drops_the_oldest_entry_and_keeps_the_saved_depth() {
    const SAVED: usize = 10;
    let (mut p, h) = common::open("schemify 1 shbench\n\nwire 0d2k1m 0 0 10 0\n", "b.shbench");
    let mut x2 = 10u32;
    let mut edit = |p: &mut Project| {
        x2 = if x2 == 10 { 20 } else { 10 };
        let op = Op::SetField {
            target: Target::new(RecordKind::Wire, "0d2k1m"),
            index: 3,
            value: x2.to_string(),
        };
        assert!(matches!(
            common::run(p, h, common::tx(vec![op])),
            Outcome::Committed { .. }
        ));
    };
    for _ in 0..SAVED {
        edit(&mut p);
    }
    let bytes = p.save(h).unwrap().expect("dirty").bytes;
    p.saved(h, bytes).unwrap();
    for _ in 0..UNDO_MAX - SAVED + 1 {
        edit(&mut p);
    }
    assert_eq!(p.history_len(h), (UNDO_MAX, 0), "one entry dropped");
    // The saved state is now at depth SAVED - 1.
    for _ in 0..UNDO_MAX - SAVED + 1 {
        assert!(p.is_dirty(h));
        assert!(matches!(
            common::run(&mut p, h, Edit::Undo),
            Outcome::Undone { .. }
        ));
    }
    assert!(!p.is_dirty(h), "saved depth followed the drop");
    assert!(matches!(
        common::run(&mut p, h, Edit::Undo),
        Outcome::Undone { .. }
    ));
    assert!(p.is_dirty(h));
}

/// Latent bug 1: a replaced or removed graphic row leaves its coordinates in the pool, and
/// they count as garbage like its attribute lines, so compaction is due when it should be.
#[test]
fn replaced_shape_coordinates_count_as_garbage() {
    let text = format!("{}shape 0abcde line 0 0 10 10\n", common::SEED_DOC);
    let (mut p, h) = common::open(&text, "a.shcomp");
    assert_eq!(p.doc(h).unwrap().pool().dead, 0);
    for k in 1..=20u32 {
        let op = Op::SetField {
            target: Target::new(RecordKind::Shape, "0abcde"),
            index: 4,
            value: format!("{}", 10 + k),
        };
        assert!(matches!(
            common::run(&mut p, h, common::tx(vec![op])),
            Outcome::Committed { .. }
        ));
        // Four coordinates per replaced row (no attribute lines or comments on it).
        assert_eq!(p.doc(h).unwrap().pool().dead, 4 * k, "edit {k}");
    }
    let del = Op::Delete(Target::new(RecordKind::Shape, "0abcde"));
    assert!(matches!(
        common::run(&mut p, h, common::tx(vec![del])),
        Outcome::Committed { .. }
    ));
    assert_eq!(p.doc(h).unwrap().pool().dead, 4 * 21);
    assert_eq!(p.doc(h).unwrap().shapes().id.len(), 0);
}
