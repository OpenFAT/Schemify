//! Journal work left to idle frames (edits spec §3, D39): the disk hash a journal's
//! `base` names is taken in idle steps, not in a commit's frame; a save or quit with
//! idle work pending still writes the right bytes; a first journal's history is encoded
//! ahead in idle steps; `open_indexed` reuses the index digest; a commit below the first
//! save starts no journal.

mod common;

use common::disk;
use common::disk::Disk;
use common::{OTA, SEED_DOC};
use schemify_doc::{Edit, Op, Outcome, Project, RecordBuf, Recovery, Target};
use schemify_format::RecordKind;

// ---- private ----

/// The disk hash a journal's `base` names is taken in idle steps (`Project::idle_step`),
/// never in a commit's frame (D39; HANDOFF latent bug 5): a commit before the steps end
/// leaves the hash pending and writes a placeholder `base`, and once the steps end the
/// journal is sent again whole, byte-identical to the journal of a document hashed before
/// its commit. A crash in between recovers the edits as a diverged tail. Oracle: the
/// journal of the hashed-first document, and the file's sha256.
#[test]
fn the_disk_hash_is_taken_in_idle_steps_not_in_the_commit() {
    let op = |v: &str| Op::Insert {
        rec: RecordBuf::new(RecordKind::Var).field(v).field("1"),
        at: None,
    };
    let committed = |o: Outcome| matches!(o, Outcome::Committed { .. });
    let (mut b, hb) = common::open(SEED_DOC, "cell.shcomp");
    assert!(disk::settle(&mut b, hb, 7) > 3, "the hash takes steps");
    assert!(!b.hash_pending(hb) && !b.idle_pending(Some(hb)));
    assert!(committed(common::run(
        &mut b,
        hb,
        common::tx(vec![op("q")])
    )));
    assert!(committed(common::run(
        &mut b,
        hb,
        common::tx(vec![op("r")])
    )));
    let want = b.take_journal(hb).unwrap();
    let sha = schemify_doc::hex(&schemify_doc::sha256(SEED_DOC.as_bytes()));
    assert!(want.rewrite && want.text.contains(&format!("base {sha}")));
    // Committed half-hashed, the journal taken (written) between the commits.
    let (mut c, hc) = common::open(SEED_DOC, "cell.shcomp");
    c.idle_step(Some(hc), 7);
    assert!(committed(common::run(
        &mut c,
        hc,
        common::tx(vec![op("q")])
    )));
    assert!(c.hash_pending(hc), "the commit did not hash the file");
    let mut disk = Disk {
        file: SEED_DOC.as_bytes().to_vec(),
        journal: Vec::new(),
    };
    disk.sync(&mut c, hc);
    assert!(!String::from_utf8_lossy(&disk.journal).contains(&sha));
    assert!(committed(common::run(
        &mut c,
        hc,
        common::tx(vec![op("r")])
    )));
    disk.sync(&mut c, hc);
    let (rp, rh, recovery) = disk::reopen(&disk);
    assert_eq!(
        recovery,
        Recovery::Diverged { txs: 2 },
        "a crash before the hash"
    );
    assert_eq!(common::canon(&rp, rh), common::canon(&c, hc));
    assert_eq!(rp.history_len(rh), c.history_len(hc));
    disk::settle(&mut c, hc, 7);
    let got = c
        .take_journal(hc)
        .expect("the journal again, with its hash");
    assert!(got.rewrite);
    assert_eq!(got.text, want.text, "the same journal");
    disk.journal = got.text.into_bytes();
    assert_eq!(disk::reopen(&disk).2, Recovery::Offer { txs: 2 });
    // Negative space: nothing to hash for a document without disk bytes.
    let mut p = Project::new(None, None, 7).unwrap();
    let new = p
        .create("", "fresh.shcomp", schemify_format::FileKind::Shcomp)
        .unwrap();
    assert!(!p.hash_pending(new) && !p.idle_pending(Some(new)));
}

/// A save, or a quit, while the idle work is pending writes the right bytes: the file is
/// the canonical form whatever the steps did (compaction keeps rows and ids), and the
/// journal names the hash of the bytes it started from (`saved` and
/// `Project::settle_journal` take the rest of the hash in their frame). Oracle: the same
/// session settled before the save.
#[test]
fn a_save_or_quit_with_idle_work_pending_writes_the_right_bytes() {
    let op = |i: usize| Op::SetAttr {
        target: Target::new(RecordKind::Inst, "0gsrp0"),
        key: "W".into(),
        values: vec![format!("{}", i % 7)],
    };
    let session = |idle: bool| {
        let (mut p, h) = common::open(OTA, "ota5t.shcomp");
        let mut i = 0;
        while p.doc(h).unwrap().pool().dead < 1 << 17 {
            let o = common::run(&mut p, h, common::tx(vec![op(i)]));
            assert!(matches!(o, Outcome::Committed { .. }));
            i += 1;
            assert!(i < 1 << 16, "garbage grows");
        }
        assert!(p.hash_pending(h), "a hash waits");
        assert!(
            p.doc(h).unwrap().pool().dead > 1 << 16,
            "a compaction waits"
        );
        if idle {
            disk::settle(&mut p, h, 1 << 17);
            assert!(p.doc(h).unwrap().pool().dead == 0, "compacted");
        }
        (p, h)
    };
    let ((mut lazy, h), (mut busy, _)) = (session(true), session(false));
    let (a, b) = (
        lazy.save(h).unwrap().unwrap(),
        busy.save(h).unwrap().unwrap(),
    );
    assert_eq!(a.bytes, b.bytes, "the saved file");
    let (mut quit, _) = session(false);
    quit.settle_journal(h);
    let want = lazy.take_journal(h).unwrap().text;
    assert_eq!(
        quit.take_journal(h).unwrap().text,
        want,
        "the journal at a quit"
    );
    lazy.saved(h, a.bytes).unwrap();
    busy.saved(h, b.bytes).unwrap();
    let (ja, jb) = (lazy.take_journal(h).unwrap(), busy.take_journal(h).unwrap());
    assert!(
        !ja.rewrite && jb.rewrite,
        "the busy journal is sent again, settled"
    );
    assert_eq!(
        jb.text,
        format!("{want}{}", ja.text),
        "the journal after the save"
    );
}

/// A document's first journal (the first commit after its first save) holds its whole
/// undo history as `hist` records: idle steps encode them ahead (`Project::idle_step`), so
/// that commit's frame does not, to the same journal. Oracle: the journal of a session
/// without idle steps, and a reopen that undoes the history.
#[test]
fn a_first_journal_encodes_its_history_in_idle_steps() {
    let var = |i: usize| Op::Insert {
        rec: RecordBuf::new(RecordKind::Var)
            .field(&format!("v{i}"))
            .field("1"),
        at: None,
    };
    let session = |idle: bool| {
        let mut p = Project::new(None, None, 7).unwrap();
        let h = p
            .create("", "fresh.shcomp", schemify_format::FileKind::Shcomp)
            .unwrap();
        let imp = Op::Insert {
            rec: RecordBuf::new(RecordKind::Impl)
                .field("sch")
                .field("schematic"),
            at: None,
        };
        let committed = |o: Outcome| matches!(o, Outcome::Committed { .. });
        assert!(committed(common::run(&mut p, h, common::tx(vec![imp]))));
        for i in 0..300 {
            assert!(committed(common::run(&mut p, h, common::tx(vec![var(i)]))));
        }
        let req = p.save(h).unwrap().unwrap();
        let file = req.bytes.clone();
        p.saved(h, req.bytes).unwrap();
        if idle {
            assert!(p.idle_pending(Some(h)), "a hash and the history wait");
            // Small steps: the history is encoded across many of them.
            assert!(disk::settle(&mut p, h, 512) > 20);
        }
        assert!(committed(common::run(
            &mut p,
            h,
            common::tx(vec![var(300)])
        )));
        if !idle {
            p.settle_journal(h);
        }
        (file, p.take_journal(h).unwrap())
    };
    let ((file, lazy), (_, busy)) = (session(true), session(false));
    assert!(lazy.rewrite && busy.rewrite);
    assert_eq!(lazy.text, busy.text, "the same journal");
    let mut q = Project::new(None, None, 7).unwrap();
    let opened = q
        .open(
            "",
            "fresh.shcomp",
            file.as_bytes(),
            Some(lazy.text.as_bytes()),
        )
        .unwrap();
    assert_eq!(opened.recovery, Recovery::Offer { txs: 1 });
    let h = opened.handle;
    assert_eq!(q.history_len(h), (301, 0), "the history before the save");
    q.recover(h).unwrap();
    while matches!(common::run(&mut q, h, Edit::Undo), Outcome::Undone { .. }) {}
    assert!(
        !common::canon(&q, h).contains("var"),
        "undo-all reaches the empty cell"
    );
}

/// Oracle: [`Project::open`], which hashes the bytes itself. `open_indexed` reuses the
/// index's digest instead, so it must match the same journal checkpoint (a wrong digest
/// turns an offer into `Diverged`) and leave nothing for `hash_step`.
#[test]
fn open_indexed_matches_the_journal_with_the_index_digest() {
    let mut offers = 0;
    for seed in 0..6 {
        for s in disk::session(seed, 40, true).iter().step_by(5) {
            let journal = (!s.disk.journal.is_empty()).then_some(s.disk.journal.as_slice());
            let mut p = Project::new(None, None, 99).unwrap();
            p.index_library("", ".", &[("cell.shcomp", &s.disk.file)])
                .unwrap();
            let opened = p
                .open_indexed("", "cell.shcomp", s.disk.file.clone(), journal)
                .unwrap();
            let h = opened.handle;
            assert!(
                !p.hash_pending(h),
                "seed {seed}: the index digest is reused"
            );
            let (_, _, want) = disk::reopen(&s.disk);
            assert_eq!(opened.recovery, want, "seed {seed}");
            offers += u32::from(matches!(want, Recovery::Offer { .. }));
        }
    }
    assert!(offers > 0, "the sweep reaches journals with unsaved edits");
    // A file the index does not list is hashed as `open` hashes it.
    let mut p = Project::new(None, None, 99).unwrap();
    let h = p
        .open_indexed("", "cell.shcomp", SEED_DOC.as_bytes().to_vec(), None)
        .unwrap()
        .handle;
    assert!(p.hash_pending(h));
}

/// A new document is edited, saved, undone below its save and edited again: the commit
/// clears the redo entry the saved state sat on, so no lineage reaches the disk bytes and
/// the journal waits for the next save (it once asserted `depth <= undo.len()` in
/// `rewrite_journal` and stopped the editor). Oracle: the spec (edits §3, rewrites happen
/// at a reachable disk state) and a reopen of the next save's journal.
#[test]
fn a_commit_below_the_first_save_starts_no_journal() {
    let mut p = Project::new(None, None, 7).unwrap();
    let h = p
        .create("", "fresh.shcomp", schemify_format::FileKind::Shcomp)
        .unwrap();
    let op = |v: &str| Op::Insert {
        rec: RecordBuf::new(RecordKind::Var).field(v).field("1"),
        at: None,
    };
    let imp = Op::Insert {
        rec: RecordBuf::new(RecordKind::Impl)
            .field("sch")
            .field("schematic"),
        at: None,
    };
    assert!(matches!(
        common::run(&mut p, h, common::tx(vec![imp])),
        Outcome::Committed { .. }
    ));
    assert!(matches!(
        common::run(&mut p, h, common::tx(vec![op("a")])),
        Outcome::Committed { .. }
    ));
    let req = p.save(h).unwrap().unwrap();
    p.saved(h, req.bytes).unwrap();
    assert!(matches!(
        common::run(&mut p, h, Edit::Undo),
        Outcome::Undone { .. }
    ));
    let o = common::run(&mut p, h, common::tx(vec![op("b")]));
    assert!(matches!(o, Outcome::Committed { .. }), "{o:?}");
    assert!(p.take_journal(h).is_none(), "no reachable disk state");
    assert!(p.is_dirty(h));
    let req = p.save(h).unwrap().unwrap();
    let file = req.bytes.clone();
    p.saved(h, req.bytes).unwrap();
    assert!(matches!(
        common::run(&mut p, h, common::tx(vec![op("c")])),
        Outcome::Committed { .. }
    ));
    p.settle_journal(h);
    let journal = p
        .take_journal(h)
        .expect("the journal starts at the saved state");
    assert!(journal.rewrite);
    let mut q = Project::new(None, None, 7).unwrap();
    let opened = q
        .open(
            "",
            "fresh.shcomp",
            file.as_bytes(),
            Some(journal.text.as_bytes()),
        )
        .unwrap();
    assert_eq!(opened.recovery, Recovery::Offer { txs: 1 });
    q.recover(opened.handle).unwrap();
    assert_eq!(common::canon(&q, opened.handle), common::canon(&p, h));
}
