//! Staged edits: the optimizer's best point, plugin run requests, and reviewing staged
//! inserts and deletes one stage at a time.

use std::time::{Duration, Instant};

use schemify_editor::checks::LAYER_ERROR;
use schemify_ui::{InputEvents, Kind};

use super::{FAKE, activate, node_kind, open, run_to_idle, sh, tools};
use crate::common::{Session, U, fixture};

/// Optimize runs the bench's `optimize` record one candidate at a time through the same
/// non-blocking runner and stages the best point (D18); Accept commits it, Reject drops it.
#[test]
fn optimizer_stages_the_best_point_accept_and_reject() {
    for accept in [true, false] {
        let tag = if accept { "opt-accept" } else { "opt-reject" };
        let mut s = open(
            "rc",
            "tb.shbench",
            tag,
            Some(sh(&FAKE.replace("sleep 0.3", ""))),
        );
        let before = s.text();
        activate(&mut s, "sim.optimize");
        assert!(s.ed.sim().busy(), "{}", s.status());
        run_to_idle(&mut s);
        let (h, _) = s.ed.active().unwrap();
        let stages = s.ed.session().project.stages(h);
        assert_eq!(stages.len(), 1, "{}", s.status());
        assert_eq!(s.text(), before, "staged, not applied");
        assert_eq!(node_kind(&s, "docs.staged"), Some(Kind::Panel));
        // The best candidate: the jobs' R with the lowest |tau - 1m|.
        let (plan, jobs, res) = s.ed.sim().tables().unwrap();
        let tau = plan.output("tau").unwrap();
        let best = (0..jobs.len())
            .filter_map(|j| Some((j, (res.value(j, tau)? - 1e-3).abs())))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap()
            .0;
        let best_r = jobs.assigned(best, "R").unwrap().to_owned();
        if accept {
            activate(&mut s, "docs.staged.accept");
            let text = s.text();
            assert!(
                text.contains(&format!("var R {best_r}\n")),
                "{best_r}\n{text}"
            );
            assert!(s.ed.session().project.stages(h).is_empty());
        } else {
            activate(&mut s, "edit.reject_staged");
            assert_eq!(s.text(), before);
            assert!(s.ed.session().project.stages(h).is_empty());
            assert_eq!(node_kind(&s, "docs.staged"), None);
        }
    }
}

/// A plugin `run` request becomes a job of the same queue and its `RunDone` goes back to the
/// requesting plugin.
#[test]
fn plugin_run_requests_join_the_queue_and_report_done() {
    use schemify_editor::session::Session as Project;
    use schemify_editor::sim::Sim;
    use schemify_plugin::{PluginId, RunRequest};
    let dir = fixture("rc", "plugin-run");
    let mut session = Project::load(&dir, 1, None).unwrap();
    let mut sim = Sim::new(tools(&FAKE.replace("sleep 0.3", "")));
    sim.enqueue(vec![RunRequest {
        plugin: PluginId(7),
        bench: "tb".into(),
        config: String::new(),
        corner: String::new(),
        vary: vec![("R".into(), "1k".into())],
    }]);
    let env: Vec<(&str, &str)> = Vec::new();
    sim.serve_queue(&mut session, &env, &mut schemify_elab::no_funcs);
    let mut done = Vec::new();
    let t = Instant::now();
    while done.is_empty() {
        assert!(t.elapsed() < Duration::from_secs(30), "{}", sim.status);
        let p = sim.poll(&session.project, &env, &mut schemify_elab::no_funcs);
        done.extend(p.run_done);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(done, [(PluginId(7), 0)]);
    let (_, jobs, res) = sim.tables().unwrap();
    assert_eq!(jobs.assigned(0, "R"), Some("1k"));
    assert_eq!(res.var_value(0, 0), Some(1000.0));
    // The `Results` snapshot carries each output's spec verdict: v1m = R·1e-3 = 1 > 0.9.
    let rows = sim.result_rows();
    let verdict = |o: &str| rows.iter().find(|r| r.0 == 0 && r.1 == o).map(|r| r.3);
    assert_eq!(verdict("v1m"), Some(2), "{rows:?}");
    assert_eq!(verdict("tau"), Some(0), "no spec: unknown");
    assert!(!sim.busy());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Staged edits under review: inserted records are drawn where they will be, deleted objects
/// crossed out, changed ones outlined; with two stages the banner steps between them and
/// Accept / Reject act on the one under review.
#[test]
fn staged_edits_draw_inserts_and_deletes_and_review_one_stage_at_a_time() {
    use schemify_doc::{Edit, EditRequest, FileKind, Mode, Transaction, ops_from_text};
    use schemify_editor::checks::LAYER_DIFF;
    let mut s = open("rc", "tb.shbench", "stages", None);
    let before = s.text();
    let (h, _) = s.ed.active().unwrap();
    for (label, ops) in [
        ("s1", "insert wire - 0 200 50 200\ndelete label 0bbbbd\n"),
        ("s2", "set_field label 0bbbba 4 vin\n"),
    ] {
        let ops = ops_from_text(ops, FileKind::Shbench).unwrap();
        let tx = Transaction {
            label: label.into(),
            mode: Mode::Stage,
            ops,
        };
        let req = EditRequest {
            doc: h,
            edit: Edit::Tx(tx),
            unix_ms: 0,
        };
        s.ed.request_edits([req]);
        s.frame(InputEvents::default());
        assert_eq!(
            s.ed.session()
                .project
                .stages(h)
                .last()
                .map(|st| st.label.as_str()),
            Some(label)
        );
    }
    if !s.ed.docs_shown() {
        activate(&mut s, "view.docs");
    }
    s.frame(InputEvents::default());
    assert_eq!(s.ed.session().project.stages(h).len(), 2);
    let lines_on = |s: &Session, layer: u8| -> Vec<[i32; 4]> {
        let l = &s.ed.decor().lines;
        (0..l.layer.len())
            .filter(|&i| l.layer[i] == layer)
            .map(|i| [l.x0[i], l.y0[i], l.x1[i], l.y1[i]])
            .collect()
    };
    let rects_on = |s: &Session, layer: u8| {
        s.ed.decor()
            .rects
            .layer
            .iter()
            .filter(|&&l| l == layer)
            .count()
    };
    // Stage 1 under review: the new wire drawn, the deleted label crossed out.
    assert!(
        lines_on(&s, LAYER_DIFF).contains(&[0, 200 * U, 50 * U, 200 * U]),
        "{:?}",
        lines_on(&s, LAYER_DIFF)
    );
    assert_eq!(lines_on(&s, LAYER_ERROR).len(), 2, "one cross");
    assert_eq!(node_kind(&s, "docs.staged.next"), Some(Kind::Button));
    // Stage 2: only the renamed label, outlined.
    activate(&mut s, "docs.staged.next");
    assert!(lines_on(&s, LAYER_DIFF).is_empty());
    assert!(lines_on(&s, LAYER_ERROR).is_empty());
    assert_eq!(rects_on(&s, LAYER_DIFF), 1);
    // Accept acts on the stage under review; the other stays, and is reviewed next.
    activate(&mut s, "docs.staged.accept");
    assert!(
        s.text().contains("label 0bbbba 0 -30 R0 vin\n"),
        "{}",
        s.text()
    );
    assert_eq!(s.ed.session().project.stages(h).len(), 1);
    assert_eq!(
        lines_on(&s, LAYER_ERROR).len(),
        2,
        "stage 1 under review again"
    );
    assert_eq!(
        node_kind(&s, "docs.staged.next"),
        None,
        "one stage: no stepping"
    );
    activate(&mut s, "edit.reject_staged");
    assert!(s.ed.session().project.stages(h).is_empty());
    assert_eq!(
        s.text(),
        before.replace("label 0bbbba 0 -30 R0 in\n", "label 0bbbba 0 -30 R0 vin\n"),
        "only the accepted stage changed the file"
    );
}
