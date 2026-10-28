//! Journal recovery (edits spec §3), DST-style: a seeded session of transactions,
//! undos, redos and saves runs while a simulated disk keeps the file and the journal
//! sidecar exactly as the app would write them. A crash after every request is then
//! simulated by opening the disk state of that moment: recovery must reproduce the
//! pre-crash document, undo/redo depths, undo labels and dirtiness, and undo-all must
//! still reach the original.

mod common;

use common::disk::*;
use common::*;
use schemify_doc::{Edit, Op, Outcome, Project, RecordBuf, Recovery, Target};
use schemify_format::RecordKind;

// ---- private ----

#[test]
fn replay_after_a_crash_at_every_request_equals_the_pre_crash_state() {
    let original = {
        let (p, h) = open(SEED_DOC, "cell.shcomp");
        canon(&p, h)
    };
    let (mut offers, mut saved_files, mut redo_states, mut unhashed_crashes) = (0, 0, 0, 0);
    for seed in 0..24 {
        let snaps = session(seed, 70, true);
        for (k, s) in snaps.iter().enumerate() {
            let ctx = format!("seed {seed} crash after request {k}");
            let (mut p, h, recovery) = reopen(&s.disk);
            // A crash before the idle steps took the disk hash leaves a `base` that
            // matches no file: the tail is replayed onto the (same) file instead.
            let unhashed = String::from_utf8_lossy(&s.disk.journal).contains("base xxxx");
            let diverged = matches!(recovery, Recovery::Diverged { .. });
            assert_eq!(diverged, unhashed, "{ctx}");
            unhashed_crashes += u32::from(unhashed);
            offers += u32::from(matches!(recovery, Recovery::Offer { .. }));
            saved_files += u32::from(s.disk.file != SEED_DOC.as_bytes());
            redo_states += u32::from(s.history.1 > 0);
            assert_eq!(canon(&p, h), s.state, "{ctx}");
            // That replay rewrites the journal, which keeps no redo history.
            match unhashed {
                false => assert_eq!(p.history_len(h), s.history, "{ctx}"),
                true => assert_eq!(p.history_len(h), (s.history.0, 0), "{ctx}"),
            }
            let labels: Vec<String> = p.undo_labels(h).iter().map(|l| l.to_string()).collect();
            assert_eq!(labels, s.labels, "{ctx}");
            assert_eq!(p.is_dirty(h), s.dirty, "{ctx}");
            while matches!(run(&mut p, h, Edit::Undo), Outcome::Undone { .. }) {}
            assert_eq!(canon(&p, h), original, "undo-all after recovery, {ctx}");
        }
    }
    // The sessions must exercise recovery, saves and redo history, or this proves nothing.
    assert!(
        offers > 200 && saved_files > 200 && redo_states > 100 && unhashed_crashes > 10,
        "{offers} {saved_files} {redo_states} {unhashed_crashes}"
    );
}

#[test]
fn a_torn_journal_tail_recovers_a_prefix_of_the_session() {
    for seed in 100..108 {
        let snaps = session(seed, 50, false);
        let full = &snaps.last().unwrap().disk.journal;
        let mut rng = Mix::new(seed);
        for _ in 0..120 {
            let cut = rng.below(full.len() as u64 + 1) as usize;
            let k = snaps
                .iter()
                .rposition(|s| s.disk.journal.len() <= cut)
                .unwrap();
            let disk = Disk {
                file: SEED_DOC.as_bytes().to_vec(),
                journal: full[..cut].to_vec(),
            };
            let (p, h, _) = reopen(&disk);
            let got = canon(&p, h);
            let ok = got == snaps[k].state || snaps.get(k + 1).is_some_and(|s| s.state == got);
            assert!(
                ok,
                "seed {seed} cut {cut}/{}: recovered state is not step {k} or {}",
                full.len(),
                k + 1
            );
        }
    }
}

#[test]
fn discarding_recovery_keeps_history_and_rewrites_the_journal() {
    let (mut s, sh) = open(SEED_DOC, "cell.shcomp");
    let mut disk = Disk {
        file: SEED_DOC.as_bytes().to_vec(),
        journal: Vec::new(),
    };
    let wire = |id: &str| {
        RecordBuf::new(RecordKind::Wire)
            .field(id)
            .field("0")
            .field("0")
            .field("10")
            .field("0")
    };
    let ins = |id| {
        tx(vec![Op::Insert {
            rec: wire(id),
            at: None,
        }])
    };
    assert!(matches!(
        run(&mut s, sh, ins("aaaaaa")),
        Outcome::Committed { .. }
    ));
    let req = s.save(sh).unwrap().unwrap();
    disk.file = req.bytes.clone().into_bytes();
    s.saved(sh, req.bytes.clone()).unwrap();
    assert!(matches!(
        run(&mut s, sh, ins("bbbbbb")),
        Outcome::Committed { .. }
    ));
    disk.sync(&mut s, sh);

    let mut p = Project::new(None, None, 1).unwrap();
    let opened = p
        .open("", "cell.shcomp", &disk.file, Some(&disk.journal))
        .unwrap();
    let h = opened.handle;
    assert_eq!(opened.recovery, Recovery::Offer { txs: 1 });
    assert_eq!(
        p.history_len(h),
        (1, 0),
        "history before the save is undoable"
    );
    assert_eq!(
        p.save(h),
        Err(schemify_doc::Code::E044),
        "saving waits for the decision"
    );
    p.discard_recovery(h).unwrap();
    assert_eq!(canon(&p, h), req.bytes);
    assert!(!p.is_dirty(h));
    let w = p.take_journal(h).unwrap();
    assert!(w.rewrite);
    let again = p
        .open("", "again.shcomp", &disk.file, Some(w.text.as_bytes()))
        .unwrap();
    assert_eq!(again.recovery, Recovery::None);
    assert_eq!(p.history_len(again.handle), (1, 0));
    assert!(matches!(
        run(&mut p, again.handle, Edit::Undo),
        Outcome::Undone { .. }
    ));
    assert_eq!(
        canon(&p, again.handle),
        SEED_DOC,
        "undo after restart reaches the original"
    );
}

#[test]
fn edits_between_save_and_saved_stay_dirty_and_recoverable() {
    for undo_below in [false, true] {
        let (mut p, h) = open(SEED_DOC, "cell.shcomp");
        let mut disk = Disk {
            file: SEED_DOC.as_bytes().to_vec(),
            journal: Vec::new(),
        };
        let wire = |id: &str| {
            RecordBuf::new(RecordKind::Wire)
                .field(id)
                .field("0")
                .field("0")
                .field("10")
                .field("0")
        };
        let ins = |id| {
            tx(vec![Op::Insert {
                rec: wire(id),
                at: None,
            }])
        };
        assert!(matches!(
            run(&mut p, h, ins("aaaaaa")),
            Outcome::Committed { .. }
        ));
        let req = p.save(h).unwrap().unwrap();
        if undo_below {
            assert!(matches!(run(&mut p, h, Edit::Undo), Outcome::Undone { .. }));
        }
        assert!(matches!(
            run(&mut p, h, ins("bbbbbb")),
            Outcome::Committed { .. }
        ));
        // The write of the older bytes finishes only now.
        disk.file = req.bytes.clone().into_bytes();
        p.saved(h, req.bytes.clone()).unwrap();
        disk.sync(&mut p, h);
        assert!(p.is_dirty(h), "undo_below {undo_below}");
        let (q, g, recovery) = reopen(&disk);
        assert!(
            matches!(recovery, Recovery::Offer { .. }),
            "undo_below {undo_below}: {recovery:?}"
        );
        assert_eq!(canon(&q, g), canon(&p, h), "undo_below {undo_below}");
        // Undoing back to the saved state is clean again: the undo stack still reaches it.
        while p.is_dirty(h) {
            assert!(
                matches!(run(&mut p, h, Edit::Undo), Outcome::Undone { .. }),
                "undo_below {undo_below}"
            );
        }
        assert_eq!(canon(&p, h), req.bytes, "undo_below {undo_below}");
    }
}

#[test]
fn a_file_changed_outside_replays_the_tail_onto_it() {
    let (mut p, h) = open(SEED_DOC, "cell.shcomp");
    let mut disk = Disk {
        file: SEED_DOC.as_bytes().to_vec(),
        journal: Vec::new(),
    };
    let wire = |id: &str, y: &str| {
        RecordBuf::new(RecordKind::Wire)
            .field(id)
            .field("0")
            .field(y)
            .field("10")
            .field(y)
    };
    let ins = |rec| tx(vec![Op::Insert { rec, at: None }]);
    assert!(matches!(
        run(&mut p, h, ins(wire("aaaaaa", "0"))),
        Outcome::Committed { .. }
    ));
    assert!(matches!(
        run(&mut p, h, ins(wire("bbbbbb", "5"))),
        Outcome::Committed { .. }
    ));
    disk.sync(&mut p, h);
    // Meanwhile someone else wrote the file: it now already has a wire `bbbbbb`.
    let external = format!("{SEED_DOC}\nwire bbbbbb 0 9 10 9\n");
    let mut q = Project::new(None, None, 1).unwrap();
    let opened = q
        .open("", "cell.shcomp", external.as_bytes(), Some(&disk.journal))
        .unwrap();
    assert_eq!(opened.recovery, Recovery::Diverged { txs: 2 });
    let qh = opened.handle;
    let del = Op::Delete(Target::new(RecordKind::Wire, "bbbbbb"));
    assert_eq!(
        run(&mut q, qh, tx(vec![del])),
        Outcome::Refused(schemify_doc::Code::E044),
        "edits wait for the decision"
    );
    assert_eq!(
        q.recover(qh),
        Ok((1, 1)),
        "the second insert collides and is dropped"
    );
    let d = q.doc(qh).unwrap();
    assert_eq!(d.wires().id.len(), 2);
    assert!(q.is_dirty(qh));
    let w = q.take_journal(qh).unwrap();
    assert!(
        w.rewrite,
        "a diverged journal is rewritten against the new file"
    );
    let r = q
        .open(
            "",
            "re.shcomp",
            external.as_bytes(),
            Some(w.text.as_bytes()),
        )
        .unwrap();
    assert_eq!(r.recovery, Recovery::Offer { txs: 1 });
}

#[test]
fn fresh_ids_and_snapped_values_are_journaled_as_applied() {
    let (mut p, h) = open(SEED_DOC, "cell.shcomp");
    let wire = RecordBuf::new(RecordKind::Wire)
        .field(schemify_doc::NEW_ID)
        .field("0")
        .field("0")
        .field("10")
        .field("0");
    assert!(matches!(
        run(
            &mut p,
            h,
            tx(vec![Op::Insert {
                rec: wire,
                at: None
            }])
        ),
        Outcome::Committed { .. }
    ));
    let id = p.doc(h).unwrap().wires().id[0].text(p.doc(h).unwrap().strings());
    assert_ne!(id, schemify_doc::NEW_ID);
    let journal = p.take_journal(h).unwrap().text;
    assert!(
        journal.contains(&format!("insert wire {id} 0 0 10 0")),
        "{journal}"
    );
    let (q, g, _) = reopen(&Disk {
        file: SEED_DOC.as_bytes().to_vec(),
        journal: journal.into_bytes(),
    });
    assert_eq!(canon(&q, g), canon(&p, h));
}

/// 2^16 + 1 commits, then a save: the journal is compacted to `base` plus the last 2^12
/// transactions as history, and a restart from it can still undo them.
#[test]
fn a_long_journal_is_compacted_on_save() {
    let (mut p, h) = open(SEED_DOC, "cell.shcomp");
    let mut disk = Disk {
        file: SEED_DOC.as_bytes().to_vec(),
        journal: Vec::new(),
    };
    let label = |i: u32| Op::SetAttr {
        target: Target::header(),
        key: "doc".into(),
        values: vec![i.to_string()],
    };
    let n = (1 << 16) + 1;
    for i in 0..n {
        assert!(matches!(
            run(&mut p, h, tx(vec![label(i)])),
            Outcome::Committed { .. }
        ));
        if i % 4096 == 0 {
            disk.sync(&mut p, h);
        }
    }
    disk.sync(&mut p, h);
    assert_eq!(
        p.history_len(h).0,
        1 << 16,
        "the undo stack drops its oldest entries"
    );
    let before = disk.journal.len();
    let req = p.save(h).unwrap().unwrap();
    disk.file = req.bytes.clone().into_bytes();
    p.saved(h, req.bytes).unwrap();
    disk.sync(&mut p, h);
    assert!(
        disk.journal.len() < before / 8,
        "{} -> {}",
        before,
        disk.journal.len()
    );
    assert_eq!(p.history_len(h), (1 << 12, 0));
    let mut q = Project::new(None, None, 1).unwrap();
    let opened = q
        .open("", "cell.shcomp", &disk.file, Some(&disk.journal))
        .unwrap();
    assert_eq!(opened.recovery, Recovery::None);
    assert_eq!(q.history_len(opened.handle), (1 << 12, 0));
    assert!(matches!(
        run(&mut q, opened.handle, Edit::Undo),
        Outcome::Undone { .. }
    ));
    let want = (n - 2).to_string();
    assert_eq!(
        q.doc(opened.handle).unwrap().header_value("doc"),
        Some(want.as_str())
    );
}

/// Edit garbage is compacted in idle steps (`Project::idle_step`), never inside a commit
/// while the steps keep up (a compaction is O(document): ~100 ms at 100k objects, HANDOFF
/// latent bug 5); a caller that never idles still compacts in its commits once garbage
/// is three times the live data. Neither changes the document or its undo history.
#[test]
fn edit_garbage_is_compacted_without_changing_the_document() {
    let (mut p, h) = open(OTA, "ota5t.shcomp");
    let (mut in_commits, mut in_steps) = (0, 0);
    const COMMITS: usize = 40_000;
    for i in 0..COMMITS {
        let op = Op::SetAttr {
            target: Target::new(RecordKind::Inst, "0gsrp0"),
            key: "W".into(),
            values: vec![format!("{}", i % 7)],
        };
        let (cursor, dead) = (p.doc(h).unwrap().cursor(), p.doc(h).unwrap().pool().dead);
        assert!(matches!(
            run(&mut p, h, tx(vec![op])),
            Outcome::Committed { .. }
        ));
        // A compaction renumbers the tables: the change log no longer reads through it.
        let d = p.doc(h).unwrap();
        let compacted = d.pool().dead < dead;
        assert_eq!(d.changes_since(cursor).is_none(), compacted, "commit {i}");
        in_commits += u32::from(compacted);
        // The first quarter idles after every commit; the rest never does.
        if i < COMMITS / 4 {
            assert!(!compacted, "commit {i} compacted in its frame");
            let cursor = p.doc(h).unwrap().cursor();
            let rebuilt = p.idle_step(Some(h), 1 << 17);
            let broke = p.doc(h).unwrap().changes_since(cursor).is_none();
            assert_eq!(rebuilt == Some(h), broke, "step after commit {i}");
            in_steps += u32::from(broke);
        }
        let d = p.doc(h).unwrap();
        assert!(
            d.pool().dead < 4 << 16,
            "garbage stays bounded: {}",
            d.pool().dead
        );
    }
    assert!(in_steps > 0 && in_commits > 0, "{in_steps} {in_commits}");
    let expected = OTA.replacen(
        "  name M1\n  W Wp\n",
        &format!("  name M1\n  W {}\n", (COMMITS - 1) % 7),
        1,
    );
    assert_eq!(canon(&p, h), expected);
    while matches!(run(&mut p, h, Edit::Undo), Outcome::Undone { .. }) {}
    assert_eq!(canon(&p, h), OTA);
}

/// `Edit::Extend` (edits spec §1): ops added to the last commit share its undo entry and its
/// journal record. While the record is pending it is rewritten in place (no record added);
/// once written out the journal is rewritten from base. A crash after either recovers the
/// extended state, one undo reverts all of it, and only the top entry can be extended.
#[test]
fn extend_joins_the_last_commit_and_its_journal_record() {
    let (mut p, h) = open(SEED_DOC, "cell.shcomp");
    let mut disk = Disk {
        file: SEED_DOC.as_bytes().to_vec(),
        journal: Vec::new(),
    };
    let original = canon(&p, h);
    let var = |name: &str, v: &str| Op::Insert {
        rec: RecordBuf::new(RecordKind::Var).field(name).field(v),
        at: None,
    };
    let Outcome::Committed { seq } = run(&mut p, h, tx(vec![var("a", "1")])) else {
        panic!("the commit applies");
    };
    let out = run(&mut p, h, Edit::Extend(seq, vec![var("b", "2")]));
    assert_eq!(out, Outcome::Committed { seq });
    disk.sync(&mut p, h);
    let journal = String::from_utf8(disk.journal.clone()).unwrap();
    let words: Vec<&str> = journal
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|w| ["tx", "undo", "redo"].contains(w))
        .collect();
    assert_eq!(words, ["tx"], "one record, rewritten in place: {journal}");
    assert_eq!(p.history_len(h), (1, 0), "one undo entry");
    let extended = canon(&p, h);
    assert!(extended.contains("var a 1") && extended.contains("var b 2"));
    let (rp, rh, _) = reopen(&disk);
    assert_eq!(
        canon(&rp, rh),
        extended,
        "recovery replays the extended record"
    );
    // The record is on disk now: the journal is rewritten from base instead.
    let out = run(&mut p, h, Edit::Extend(seq, vec![var("c", "3")]));
    assert_eq!(out, Outcome::Committed { seq });
    disk.sync(&mut p, h);
    let (rp, rh, _) = reopen(&disk);
    assert_eq!(canon(&rp, rh), canon(&p, h));
    assert!(matches!(run(&mut p, h, Edit::Undo), Outcome::Undone { .. }));
    assert_eq!(
        canon(&p, h),
        original,
        "one undo reverts the commit and both extensions"
    );
    // Negative space: with redo pending, or naming another entry, nothing changes.
    let refused = Outcome::Refused(schemify_doc::Code::E040);
    assert_eq!(
        run(&mut p, h, Edit::Extend(seq, vec![var("d", "4")])),
        refused
    );
    assert!(matches!(run(&mut p, h, Edit::Redo), Outcome::Redone { .. }));
    let other = Edit::Extend(seq + 1, vec![var("d", "4")]);
    assert_eq!(run(&mut p, h, other), refused);
    assert!(!canon(&p, h).contains("var d"));
}
