//! The plugin snapshot follows random edits, equals a reopened file's, and refills on the
//! next tick after an edit.

use schemify_canvas::LAYER_DEAD;

use super::{Rng, markers_match};

use crate::common::{self, Session, U};

/// Oracle for the patched plugin snapshot (`Host::snapshot`, whose debug build compares every
/// fill with a full refill on documents this small): random edits of a schematic of floating
/// wires and instances (hundreds of violations, several gone and added per pass, not side by
/// side), undo and redo, each followed by the ticks the editor asks for, so every fill follows
/// the change log and the previous check pass. A scene and markers check rides along.
#[test]
fn the_plugin_snapshot_follows_random_edits() {
    use schemify_doc::{Edit, EditRequest, Mode, Transaction, ops_from_text};
    use schemify_format::FileKind;
    let mut top = String::from("schemify 1 shcomp\n\nimpl sch schematic\n\n");
    for i in 0..40 {
        let (x, y) = ((i % 8) * 200, (i / 8) * 200);
        top.push_str(&format!(
            "inst m{i:05} dev:nmos\n  at {x} {y} R0\n  name M{i}\n"
        ));
        for k in 0..4 {
            let yy = y - 30 - k * 10;
            top.push_str(&format!(
                "wire w{i:03}{k:02} {} {yy} {} {yy}\n",
                x + 20,
                x + 120
            ));
        }
        top.push_str(&format!("label n{i:05} {} {} R0 n{i}\n", x + 120, y - 30));
    }
    top.push_str("\nvar wv 3\n");
    let dir = common::fixture("inv", "snapshot-walk");
    std::fs::write(dir.join("walk.shcomp"), top).unwrap();
    let ed = schemify_editor::editor::Editor::new(schemify_editor::editor::Options {
        no_default_ui: false,
        path: Some(dir.join("walk.shcomp")),
        seed: 3,
        ..Default::default()
    })
    .unwrap();
    let mut s = Session {
        ed,
        now: 1,
        dir,
        caches: Default::default(),
    };
    s.frame(Default::default());
    let mut rng = Rng(11);
    let (h, _) = s.ed.active().unwrap();
    let mut next = 0;
    for step in 0..120 {
        let doc = s.ed.session().project.doc(h).unwrap();
        let (w, inst) = (doc.wires(), doc.instances());
        let pick = |rng: &mut Rng, n: usize| rng.below(n.max(1) as u64) as usize;
        let text = match rng.below(10) {
            7 if !inst.id.is_empty() => {
                // A rename: unique, a duplicate (E009), a bus (more elements), back.
                let r = pick(&mut rng, inst.id.len());
                let name = ["MX", "M3", "MB[1:0]", "M7"][pick(&mut rng, 4)];
                format!(
                    "set_attr inst {} name {name}\n",
                    inst.id[r].text(doc.strings())
                )
            }
            8 if !inst.id.is_empty() => {
                // A param shown on the symbol: a value, a var, dropped.
                let r = pick(&mut rng, inst.id.len());
                let id = inst.id[r].text(doc.strings());
                match pick(&mut rng, 4) {
                    0 => format!("remove_attr inst {id} W\n"),
                    1 => format!("set_attr inst {id} W wv*2\n"),
                    _ => format!("set_attr inst {id} W {}\n", 1 + pick(&mut rng, 9)),
                }
            }
            9 => format!("set_field var wv 1 {}\n", 1 + pick(&mut rng, 9)),
            0 | 1 if !w.id.is_empty() => {
                // Two wires far apart in the violation order.
                let (a, b) = (pick(&mut rng, w.id.len()), pick(&mut rng, w.id.len()));
                let ids: Vec<String> = [a, b]
                    .iter()
                    .map(|&r| w.id[r].text(doc.strings()))
                    .collect();
                if ids[0] == ids[1] {
                    format!("delete wire {}\n", ids[0])
                } else {
                    format!("delete wire {}\ndelete wire {}\n", ids[0], ids[1])
                }
            }
            2 if !inst.id.is_empty() => {
                let r = pick(&mut rng, inst.id.len());
                format!("delete inst {}\n", inst.id[r].text(doc.strings()))
            }
            3 => {
                next += 1;
                let (x, y) = (pick(&mut rng, 1600) as i64, pick(&mut rng, 1000) as i64);
                format!("insert wire x{next:05} {x} {y} {} {y}\n", x + 60)
            }
            4 if !w.id.is_empty() => {
                let r = pick(&mut rng, w.id.len());
                let id = w.id[r].text(doc.strings());
                let (x, y) = (pick(&mut rng, 1600) as i64, pick(&mut rng, 1000) as i64);
                format!("put wire {id} {id} {x} {y} {} {y}\n", x + 40)
            }
            5 => "undo".into(),
            _ => "redo".into(),
        };
        let edit = match text.as_str() {
            "undo" => Edit::Undo,
            "redo" => Edit::Redo,
            t => Edit::Tx(Transaction {
                label: "walk".into(),
                mode: Mode::Commit,
                ops: ops_from_text(t, FileKind::Shcomp).unwrap_or_else(|e| panic!("{t}: {e:?}")),
            }),
        };
        s.ed.request_edits([EditRequest {
            doc: h,
            edit,
            unix_ms: 0,
        }]);
        s.frame(Default::default());
        for _ in 0..4 {
            if s.ed.wake_ms() == Some(0) {
                s.frame(Default::default());
            }
        }
        markers_match(&s, &format!("step {step} ({text:?})"));
        if step % 10 == 9 {
            same_as_reopened(&mut s, &format!("step {step} ({text:?})"));
        }
    }
}

/// The scene's texts (symbol texts show evaluated params) and the violations of an editor
/// that followed edits equal those of an editor that opens the same text afresh.
fn same_as_reopened(s: &mut Session, ctx: &str) {
    let dir = common::fixture("inv", "snapshot-walk-reopened");
    let path = dir.join("walk.shcomp");
    std::fs::write(&path, s.text()).expect("the test's temp dir is writable");
    let ed = schemify_editor::editor::Editor::new(schemify_editor::editor::Options {
        no_default_ui: false,
        path: Some(path),
        seed: 3,
        ..Default::default()
    })
    .expect("the fixture project opens");
    let mut fresh = Session {
        ed,
        now: 1,
        dir,
        caches: Default::default(),
    };
    for _ in 0..8 {
        fresh.frame(Default::default());
    }
    let texts = |s: &Session| {
        let t = &s.ed.scene().prims.texts;
        let mut rows: Vec<String> = Vec::new();
        let mut start = 0;
        for i in 0..t.x.len() {
            let end = t.end[i] as usize;
            if t.layer[i] != LAYER_DEAD {
                rows.push(format!("{} {} {}", t.x[i], t.y[i], &t.text[start..end]));
            }
            start = end;
        }
        rows.sort();
        rows
    };
    let violations = |s: &Session| {
        let st = s.ed.cell().expect("elaborated");
        let n = |x: schemify_elab::Name| {
            if x == schemify_elab::Name::NONE {
                String::new()
            } else {
                st.names.get(x).to_owned()
            }
        };
        let rows: Vec<String> = st
            .violations
            .rows
            .iter()
            .map(|v| {
                format!(
                    "{} {} {} {}",
                    v.code.as_str(),
                    v.object.0,
                    n(v.param),
                    v.detail
                )
            })
            .collect();
        rows
    };
    assert_eq!(texts(s), texts(&fresh), "{ctx}: scene texts");
    assert_eq!(violations(s), violations(&fresh), "{ctx}: violations");
}

/// An edit's check pass comes after the frame delivered `DocChanged` to the plugins, so their
/// snapshot lags it: the editor asks for one tick at once, which refills it, and is idle
/// after it (D39). Without plugins there is no snapshot to refill.
#[test]
fn an_edit_refills_the_plugin_snapshot_on_the_next_tick() {
    for plugins in [true, false] {
        let mut s = Session::open("inv", "top.shcomp", &format!("snap-{plugins}"), plugins);
        s.frame(Default::default());
        assert_eq!(s.ed.wake_ms(), None, "idle after opening");
        let before = s.text();
        s.click((10 * U, 0));
        assert_eq!(s.ed.selection().len(), 1, "instance x1");
        s.ed.command("edit.delete", "");
        s.frame(Default::default());
        assert_ne!(s.text(), before, "deleted");
        if plugins {
            assert_eq!(s.ed.wake_ms(), Some(0), "the snapshot is due");
        }
        // The due tick, and the user config's copy of `DocChanged` (delivered on the next).
        for _ in 0..2 {
            if s.ed.wake_ms().is_some() {
                s.frame(Default::default());
            }
        }
        assert_eq!(
            s.ed.wake_ms(),
            None,
            "idle after the edit (plugins: {plugins})"
        );
    }
}
