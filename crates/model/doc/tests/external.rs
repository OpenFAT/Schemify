//! External changes (edits spec §4): silent reload, clean merge with local edits kept
//! dirty and undo rebased onto the new bytes, conflicts with per-block choices and
//! blocked saves, unreadable bytes, and the journal after a merge.

mod common;

use common::*;
use schemify_doc::{
    Choice, Code, Edit, External, Op, Outcome, Project, RecordBuf, Recovery, Target,
};
use schemify_format::{RecordKind, merge3, parse};

fn set_attr(id: &str, key: &str, values: &[&str]) -> Op {
    Op::SetAttr {
        target: Target::new(RecordKind::Inst, id),
        key: key.into(),
        values: values.iter().map(|v| v.to_string()).collect(),
    }
}

/// OTA with M2's `W` changed on disk by someone else.
fn theirs_w(value: &str) -> String {
    let t = OTA.replacen(
        "  name M2\n  W Wp\n",
        &format!("  name M2\n  W {value}\n"),
        1,
    );
    assert_ne!(t, OTA);
    t
}

fn undo_all(p: &mut Project, h: schemify_doc::DocHandle) {
    while matches!(run(p, h, Edit::Undo), Outcome::Undone { .. }) {}
}

#[test]
fn no_local_edits_reload_silently() {
    let (mut p, h) = open(OTA, "ota5t.shcomp");
    assert_eq!(
        p.external_change(h, OTA.as_bytes()),
        Ok(External::Unchanged)
    );
    let theirs = theirs_w("7");
    // An edit that was undone leaves nothing unsaved.
    assert!(matches!(
        run(
            &mut p,
            h,
            tx(vec![set_attr("0gsrp0", "at", &["0", "0", "R0"])])
        ),
        Outcome::Committed { .. }
    ));
    assert!(matches!(run(&mut p, h, Edit::Undo), Outcome::Undone { .. }));
    assert!(matches!(
        p.external_change(h, theirs.as_bytes()),
        Ok(External::Reloaded { .. })
    ));
    assert_eq!(canon(&p, h), theirs);
    assert_eq!(
        p.history_len(h),
        (0, 0),
        "history of the old bytes is dropped"
    );
    assert!(!p.is_dirty(h));
}

#[test]
fn clean_merge_keeps_local_edits_dirty_and_rebases_undo() {
    let (mut p, h) = open(OTA, "ota5t.shcomp");
    assert!(matches!(
        run(
            &mut p,
            h,
            tx(vec![set_attr("0gsrp0", "at", &["-70", "-300", "R0"])])
        ),
        Outcome::Committed { .. }
    ));
    assert!(matches!(
        run(&mut p, h, tx(vec![set_attr("4szjc8", "W", &["8"])])),
        Outcome::Committed { .. }
    ));
    let ours = canon(&p, h);
    let theirs = theirs_w("7");
    let want = merge3(
        &parse(OTA.as_bytes()).unwrap(),
        &parse(ours.as_bytes()).unwrap(),
        &parse(theirs.as_bytes()).unwrap(),
    );
    assert_eq!(want.conflicts, 0);
    assert_eq!(
        p.external_change(h, theirs.as_bytes()),
        Ok(External::Merged { dropped: 0 })
    );
    assert_eq!(canon(&p, h), want.text);
    assert!(p.is_dirty(h));
    assert_eq!(p.history_len(h), (2, 0), "both local edits stay undoable");
    assert!(matches!(run(&mut p, h, Edit::Undo), Outcome::Undone { .. }));
    undo_all(&mut p, h);
    assert_eq!(
        canon(&p, h),
        theirs,
        "undo-all after a rebase reaches the external version"
    );
    assert!(!p.is_dirty(h));
    while matches!(run(&mut p, h, Edit::Redo), Outcome::Redone { .. }) {}
    assert_eq!(canon(&p, h), want.text);
    // Saving now writes the merge, and the new bytes are the next merge base.
    let req = p.save(h).unwrap().unwrap();
    assert_eq!(req.bytes, want.text);
    p.saved(h, req.bytes.clone()).unwrap();
    assert!(!p.is_dirty(h));
}

#[test]
fn local_edits_that_no_longer_apply_are_dropped_and_the_merge_still_wins() {
    let (mut p, h) = open(OTA, "ota5t.shcomp");
    let wire = |y: &str| {
        RecordBuf::new(RecordKind::Wire)
            .field("aaaaaa")
            .field("0")
            .field(y)
            .field("10")
            .field(y)
    };
    assert!(matches!(
        run(
            &mut p,
            h,
            tx(vec![Op::Insert {
                rec: wire("0"),
                at: None
            }])
        ),
        Outcome::Committed { .. }
    ));
    // They added a different wire with the same id: merge3 re-ids theirs, so the merge
    // is clean, but replaying our insert onto their bytes collides.
    let theirs = OTA.replacen("label 2ma7xe", "wire aaaaaa 0 5 10 5\n\nlabel 2ma7xe", 1);
    let theirs = schemify_format::write(&parse(theirs.as_bytes()).unwrap());
    let ours = canon(&p, h);
    let want = merge3(
        &parse(OTA.as_bytes()).unwrap(),
        &parse(ours.as_bytes()).unwrap(),
        &parse(theirs.as_bytes()).unwrap(),
    );
    assert_eq!(want.conflicts, 0);
    assert_eq!(
        p.external_change(h, theirs.as_bytes()),
        Ok(External::Merged { dropped: 1 })
    );
    assert_eq!(canon(&p, h), want.text);
    undo_all(&mut p, h);
    assert_eq!(canon(&p, h), theirs);
}

#[test]
fn edits_below_the_save_point_still_rebase_exactly() {
    let (mut p, h) = open(OTA, "ota5t.shcomp");
    assert!(matches!(
        run(&mut p, h, tx(vec![set_attr("0gsrp0", "L", &["2"])])),
        Outcome::Committed { .. }
    ));
    let req = p.save(h).unwrap().unwrap();
    p.saved(h, req.bytes.clone()).unwrap();
    assert!(matches!(run(&mut p, h, Edit::Undo), Outcome::Undone { .. }));
    assert!(matches!(
        run(&mut p, h, tx(vec![set_attr("4tyd1p", "L", &["3"])])),
        Outcome::Committed { .. }
    ));
    let ours = canon(&p, h);
    let theirs = req
        .bytes
        .replacen("  name M2\n  W Wp\n", "  name M2\n  W 9\n", 1);
    let want = merge3(
        &parse(req.bytes.as_bytes()).unwrap(),
        &parse(ours.as_bytes()).unwrap(),
        &parse(theirs.as_bytes()).unwrap(),
    );
    assert_eq!(want.conflicts, 0);
    assert!(matches!(
        p.external_change(h, theirs.as_bytes()),
        Ok(External::Merged { .. })
    ));
    assert_eq!(canon(&p, h), want.text);
    undo_all(&mut p, h);
    assert_eq!(canon(&p, h), theirs);
}

#[test]
fn conflicts_block_saves_and_edits_until_resolved_per_block() {
    for (choice, want_w) in [
        (Choice::Ours, "5"),
        (Choice::Theirs, "7"),
        (Choice::Text(String::new()), ""),
    ] {
        let (mut p, h) = open(OTA, "ota5t.shcomp");
        assert!(matches!(
            run(&mut p, h, tx(vec![set_attr("1j446r", "W", &["5"])])),
            Outcome::Committed { .. }
        ));
        assert!(matches!(
            run(&mut p, h, tx(vec![set_attr("0gsrp0", "nf", &["4"])])),
            Outcome::Committed { .. }
        ));
        let theirs = theirs_w("7");
        assert_eq!(
            p.external_change(h, theirs.as_bytes()),
            Ok(External::Conflict { blocks: 1 })
        );
        let blocks = p.conflicts(h).unwrap().to_vec();
        assert!(
            blocks[0].ours.contains("W 5") && blocks[0].theirs.contains("W 7"),
            "{blocks:?}"
        );
        assert_eq!(p.save(h), Err(Code::E044));
        let del = Op::Delete(Target::new(RecordKind::Wire, "0d2k1m"));
        assert_eq!(run(&mut p, h, tx(vec![del])), Outcome::Refused(Code::E044));
        assert_eq!(run(&mut p, h, Edit::Undo), Outcome::Refused(Code::E044));
        assert_eq!(p.external_change(h, OTA.as_bytes()), Err(Code::E044));
        assert!(p.resolve_conflicts(h, &[]).is_err(), "one choice per block");
        let text_choice = matches!(choice, Choice::Text(_));
        let choice = match choice {
            Choice::Text(_) => Choice::Text(blocks[0].theirs.replace("W 7", "W 6")),
            c => c,
        };
        let want_w = if text_choice { "6" } else { want_w };
        assert!(matches!(
            p.resolve_conflicts(h, &[choice]),
            Ok(External::Merged { .. })
        ));
        assert!(p.conflicts(h).is_none());
        let d = p.doc(h).unwrap();
        let s = d.strings();
        let m2 = (0..5)
            .find(|&i| s.get(d.instances().name[i]) == "M2")
            .unwrap();
        let w = d.pool().find_attr(s, d.instances().attrs[m2], "W").unwrap();
        assert_eq!(s.get(w[0]), want_w);
        let m1 = (0..5)
            .find(|&i| s.get(d.instances().name[i]) == "M1")
            .unwrap();
        let nf = d
            .pool()
            .find_attr(s, d.instances().attrs[m1], "nf")
            .unwrap();
        assert_eq!(s.get(nf[0]), "4", "the non-conflicting local edit survives");
        assert!(p.save(h).unwrap().is_some());
        undo_all(&mut p, h);
        assert_eq!(canon(&p, h), theirs);
    }
}

#[test]
fn unreadable_external_bytes_change_nothing() {
    let (mut p, h) = open(OTA, "ota5t.shcomp");
    assert!(matches!(
        run(&mut p, h, tx(vec![set_attr("1j446r", "W", &["5"])])),
        Outcome::Committed { .. }
    ));
    let before = canon(&p, h);
    let broken = format!("{OTA}<<<<<<< ours\n");
    assert!(matches!(
        p.external_change(h, broken.as_bytes()),
        Ok(External::Unreadable(_))
    ));
    assert_eq!(canon(&p, h), before);
    assert_eq!(p.history_len(h), (1, 0));
}

#[test]
fn the_journal_after_a_merge_recovers_the_merged_state() {
    let (mut p, h) = open(OTA, "ota5t.shcomp");
    assert!(matches!(
        run(
            &mut p,
            h,
            tx(vec![set_attr("0gsrp0", "at", &["-70", "-300", "R0"])])
        ),
        Outcome::Committed { .. }
    ));
    let theirs = theirs_w("7");
    assert!(matches!(
        p.external_change(h, theirs.as_bytes()),
        Ok(External::Merged { .. })
    ));
    let merged = canon(&p, h);
    let w = p.take_journal(h).unwrap();
    assert!(w.rewrite, "the journal is rewritten against the new base");
    let mut q = Project::new(None, None, 1).unwrap();
    let opened = q
        .open(
            "",
            "ota5t.shcomp",
            theirs.as_bytes(),
            Some(w.text.as_bytes()),
        )
        .unwrap();
    assert_eq!(opened.recovery, Recovery::Offer { txs: 1 });
    q.recover(opened.handle).unwrap();
    assert_eq!(canon(&q, opened.handle), merged);
}
