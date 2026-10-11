//! The M2 integration through the frame spine: the live checks overlay, net highlight,
//! netlisting (`n`) against the CLI's bytes, the non-blocking run with a fake simulator
//! (results table, plot slots, OP back-annotation, cross-probe), the optimizer's staged edit,
//! plugin run requests, and the idle check (no ticks when nothing runs, D39).
//!
//! The fake simulator is `sh` (on Windows the one Git for Windows puts on `PATH`); the
//! real-ngspice test runs when `ngspice` is on `PATH`.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Session, U, fixture};
use schemify_app::checks::{LAYER_ERROR, LAYER_INFO, LAYER_WARN, SOURCE_CHECKS, SOURCE_OP};
use schemify_app::editor::{Editor, Options, Panel};
use schemify_app::scene::ObjKey;
use schemify_doc::{ObjId, TableId};
use schemify_run::{SimCommand, State};
use schemify_ui::{InputEvent, InputEvents, Kind};

/// Reads `.param R=` from the netlist, sleeps, then writes an ASCII raw file with an operating
/// point (v(in) = 1, v(out) = 0.5) and a transient where v(out) rises linearly from 0 to R
/// over 1 s.
const FAKE: &str = r#"R=$(sed -n 's/^\.param R=//p' "$1")
sleep 0.3
printf 'Title: fake\nPlotname: Operating Point\nFlags: real\nNo. Variables: 2\nNo. Points: 1\nVariables:\n 0 v(in) voltage\n 1 v(out) voltage\nValues:\n0 1\n 0.5\nTitle: fake\nPlotname: Transient Analysis\nFlags: real\nNo. Variables: 2\nNo. Points: 2\nVariables:\n 0 time time\n 1 v(out) voltage\nValues:\n0 0\n 0\n1 1\n %s\n' "$R" > "$2"
"#;

fn sh(script: &str) -> SimCommand {
    SimCommand {
        path: "sh".into(),
        args: ["-c", script, "fake", "{netlist}", "{raw}"]
            .map(String::from)
            .to_vec(),
    }
}

/// Opens `file` of fixture `name` with the default UI and the simulator `sim`.
fn open(name: &str, file: &str, tag: &str, sim: Option<SimCommand>) -> Session {
    let dir = fixture(name, tag);
    let ed = Editor::new(Options {
        path: Some(dir.join(file)),
        seed: 7,
        sim,
        ..Options::default()
    })
    .expect("fixture opens");
    let mut s = Session { ed, now: 1, dir };
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    s
}

/// Runs the command bound to UI node `id` (AccessKit activation: no menu needs to be open).
fn activate(s: &mut Session, id: &str) {
    let sym =
        s.ed.shell
            .strings
            .find(id)
            .unwrap_or_else(|| panic!("{id} interned"));
    let mut ev = InputEvents::default();
    ev.push(InputEvent::Activate { node: sym });
    s.frame(ev);
}

/// Ticks with no input until the run is idle (the window's timer path), bounded.
fn run_to_idle(s: &mut Session) -> u32 {
    let t = Instant::now();
    let mut ticks = 0;
    while s.ed.sim().busy() {
        assert!(t.elapsed() < Duration::from_secs(60), "run did not finish");
        assert!(s.ed.wake_ms().is_some(), "a busy run asks for ticks");
        std::thread::sleep(Duration::from_millis(10));
        s.frame(InputEvents::default());
        ticks += 1;
    }
    s.frame(InputEvents::default());
    ticks
}

fn node_kind(s: &Session, id: &str) -> Option<Kind> {
    let sh = &s.ed.shell;
    let sym = sh.strings.find(id)?;
    let n = sh.tables.nodes.find(sym)?;
    Some(sh.tables.nodes.kind()[n as usize])
}

fn marker_rects(s: &Session) -> usize {
    let layers = [LAYER_ERROR, LAYER_WARN, LAYER_INFO];
    s.ed.decor()
        .rects
        .layer
        .iter()
        .filter(|l| layers.contains(l))
        .count()
}

/// The checks overlay mirrors elab's violations of the active level: one mark per violation
/// (source `checks`), one marker per violation whose object is drawn; an edit that adds
/// violations adds marks, undo removes them.
#[test]
fn violation_overlay_follows_edits() {
    let mut s = open("inv", "top.shcomp", "checks", None);
    let check = |s: &Session| {
        let cell = s.ed.cell().expect("every shown cell is elaborated");
        let ann = s.ed.annotations();
        let marks = ann.source.iter().filter(|x| *x == SOURCE_CHECKS).count();
        assert_eq!(marks, cell.violations.rows.len());
        let drawn = cell
            .violations
            .rows
            .iter()
            .filter(|v| v.object != ObjId::NONE)
            .count();
        assert_eq!(marker_rects(s), drawn);
        cell.violations.rows.len()
    };
    let n0 = check(&s);
    // Move x2 far away (a plain drag; Ctrl+drag would stretch its wires along): its four
    // pins dangle (W004).
    s.drag((130 * U, 10 * U), (130 * U, 10 * U), 0);
    s.drag((130 * U, 10 * U), (130 * U, 300 * U), 0);
    let n1 = check(&s);
    assert!(n1 >= n0 + 4, "{n0} -> {n1}");
    let w004 =
        s.ed.cell()
            .unwrap()
            .violations
            .rows
            .iter()
            .filter(|v| v.code.as_str() == "W004")
            .count();
    assert!(w004 >= 4);
    s.key("u");
    assert_eq!(check(&s), n0, "undo removes the copy's violations");
    // The checks panel lists them; a row click selects its object.
    activate(&mut s, "view.checks");
    assert_eq!(s.ed.panel(), Panel::Checks);
    assert_eq!(node_kind(&s, "checks.pane"), Some(Kind::Panel));
    activate(&mut s, "view.checks");
    assert_eq!(s.ed.panel(), Panel::None);
    assert_eq!(node_kind(&s, "checks.pane"), None);
}

/// `k` highlights exactly elab's net (wires, labels, ports with that component, instance
/// pins on it); descending into a sub-cell keeps it highlighted through the port; `Shift+K`
/// clears.
#[test]
fn net_highlight_covers_exactly_the_net_across_the_hierarchy() {
    let mut s = open("inv", "top.shcomp", "hilite", None);
    // w00006 runs 0 -70 → 140 -70 on VDD; click its left part (the label sits at 70).
    s.click((20 * U, -70 * U));
    assert_eq!(s.ed.selection().len(), 1);
    s.key("k");
    let oracle = |s: &Session, comp_of: &dyn Fn(&schemify_elab::CellState) -> u32| {
        let cell = s.ed.cell().unwrap();
        let (h, _) = s.ed.active().unwrap();
        let doc = s.ed.session.project.doc(h).unwrap();
        // Elab's net: every component carrying the clicked component's bit (components
        // with the same name are one net, merged by name).
        let n = &cell.nets;
        let bit = n.bits_of(comp_of(cell) as usize)[0];
        let on = |c: u32| n.bits_of(c as usize).contains(&bit);
        let mut keys = Vec::new();
        for (ids, nets, table) in [
            (&doc.wires().id, &n.wire_net, TableId::Wires),
            (&doc.labels().id, &n.label_net, TableId::Labels),
            (&doc.ports().id, &n.port_net, TableId::Ports),
        ] {
            for (id, &k) in ids.iter().zip(nets) {
                if on(k) {
                    keys.push(ObjKey { table, key: id.0 });
                }
            }
        }
        keys.sort_unstable();
        let pins: usize = (0..doc.instances().id.len())
            .map(|i| n.pins(i).iter().filter(|&&k| on(k)).count())
            .sum();
        (keys, pins)
    };
    let top_comp = |cell: &schemify_elab::CellState| {
        let doc_row = 5; // w00006 is the sixth wire
        cell.nets.wire_net[doc_row]
    };
    let (keys, pins) = oracle(&s, &top_comp);
    let hl = s.ed.highlighted();
    assert!(!keys.is_empty());
    assert_eq!(hl.keys, keys, "{}", s.ed.session.status);
    assert_eq!(hl.pins.len(), pins);
    assert_eq!(pins, 2, "x1.VDD and x2.VDD");
    assert_eq!(s.ed.highlight_keys(), ["vdd"]);
    // Descend into x1: the child's VDD (port + label + wires) is the same net.
    s.click((-10 * U, 0));
    s.key("e");
    assert_eq!(s.ed.session.hier_path(), ".x1.");
    let child_comp = |cell: &schemify_elab::CellState| {
        let row = 0; // label n00001 VDD
        cell.nets.label_net[row]
    };
    let (keys, pins) = oracle(&s, &child_comp);
    assert_eq!(s.ed.highlighted().keys, keys);
    assert_eq!(s.ed.highlighted().pins.len(), pins);
    assert!(!keys.is_empty());
    s.key("Ctrl+E");
    s.key("Shift+K");
    assert!(s.ed.highlight_keys().is_empty());
    assert!(s.ed.highlighted().keys.is_empty());
}

/// `n` writes `.schemify/netlist/top.spice` with the same bytes `schemify netlist top.shcomp`
/// prints, and opens it in a read-only text tab; an expression error blocks it (D13) and
/// lists the error in the checks panel.
#[test]
fn netlist_command_writes_the_cli_bytes() {
    let mut s = open("inv", "top.shcomp", "netlist", None);
    s.key("n");
    let file = s.dir.join(".schemify/netlist/top.spice");
    let bytes = std::fs::read(&file).unwrap_or_else(|e| panic!("{e}: {}", s.ed.session.status));
    // The CLI's own load + netlist spine (`schemify netlist top.shcomp`; the binary lives in
    // schemify-cli since the D39 split, so its spine is called directly).
    let (mut o, _) = schemify_cli::spines::open_file(&s.dir.join("top.shcomp"), 1).unwrap();
    let env = schemify_cli::spines::env_pairs();
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let req = schemify_elab::Request {
        config: None,
        corner: None,
        overrides: &[],
        env: &env,
        dialect: schemify_elab::Dialect::Ngspice,
    };
    let top = o.top;
    let e = schemify_cli::spines::elaborate_all(&mut o, top, &req, &mut schemify_elab::no_funcs)
        .unwrap();
    let cli = schemify_netlist::emit(&e, schemify_elab::Dialect::Ngspice).text;
    assert_eq!(bytes, cli.as_bytes(), "editor and CLI netlists differ");
    let view =
        s.ed.session
            .text_view()
            .expect("a text tab shows the netlist");
    assert_eq!(view.body, String::from_utf8(bytes).unwrap().trim_end());
    assert_eq!(node_kind(&s, "textview.body"), Some(Kind::Prose));
    assert_eq!(
        node_kind(&s, "canvas"),
        None,
        "the text tab replaces the canvas"
    );
    // Editing keys do nothing on a text tab.
    let before = s.text();
    s.key("Del");
    assert_eq!(s.text(), before);
    // Another dialect, another file.
    activate(&mut s, "tab.0");
    activate(&mut s, "sim.dialect.cdl");
    s.key("n");
    assert!(s.dir.join(".schemify/netlist/top.cdl").exists());
    // Gating: an expression error in an override blocks the netlist.
    let dir = fixture("inv", "netlist-gate");
    let top = std::fs::read_to_string(dir.join("top.shcomp")).unwrap();
    std::fs::write(
        dir.join("top.shcomp"),
        top.replace("  wp 4", "  wp \"1 +\""),
    )
    .unwrap();
    let ed = Editor::new(Options {
        path: Some(dir.join("top.shcomp")),
        seed: 7,
        ..Options::default()
    })
    .unwrap();
    let mut s = Session { ed, now: 1, dir };
    s.frame(InputEvents::default());
    s.key("n");
    assert!(
        s.ed.session.status.contains("blocked"),
        "{}",
        s.ed.session.status
    );
    assert!(!s.dir.join(".schemify/netlist/top.spice").exists());
    assert_eq!(s.ed.panel(), Panel::Checks);
    assert_eq!(node_kind(&s, "checks.blocked.0"), Some(Kind::Label));
}

/// `s` on a component explains D11b; "New bench from this component" writes a `.shbench`
/// instantiating it with a label per pin, opened in a new tab.
#[test]
fn simulate_needs_a_bench_and_new_bench_makes_one() {
    let mut s = open("inv", "inv.shcomp", "newbench", None);
    s.key("s");
    assert!(
        s.ed.session.status.contains(".shbench"),
        "{}",
        s.ed.session.status
    );
    activate(&mut s, "sim.new_bench");
    let text = std::fs::read_to_string(s.dir.join("inv_tb.shbench")).unwrap();
    assert!(text.starts_with("schemify 1 shbench\n"), "{text}");
    assert!(text.contains(" ./inv\n"), "{text}");
    for pin in ["A", "Y", "VDD", "VSS"] {
        assert!(text.contains(&format!(" R0 {pin}\n")), "{pin}: {text}");
    }
    let (h, _) = s.ed.active().unwrap();
    let doc = s.ed.session.project.doc(h).unwrap();
    assert_eq!(doc.kind(), schemify_doc::FileKind::Shbench);
    let cell = s.ed.cell().unwrap();
    assert!(
        cell.violations
            .rows
            .iter()
            .all(|v| v.code.as_str() != "E001"),
        "the bench resolves its instance"
    );
}

/// The run spine without blocking: `s` returns at once, ticks poll the fake simulator, the
/// results table fills, the docs page's plot slot gets its graph, the OP overlay marks the
/// nets, Waves opens the job, a click on a net cross-probes `v(out)`, and once idle no tick
/// is asked for. The results are saved and reload in a fresh editor (D18).
#[test]
fn simulate_polls_a_fake_simulator_to_results_waves_and_op() {
    let mut s = open("rc", "tb.shbench", "simulate", Some(sh(FAKE)));
    assert_eq!(s.ed.wake_ms(), None, "idle before the run");
    let t = Instant::now();
    s.key("s");
    let started = t.elapsed();
    assert!(
        started < Duration::from_millis(250),
        "starting a run must not wait for the simulator: {started:?}"
    );
    assert!(s.ed.sim().busy(), "{}", s.ed.session.status);
    assert_eq!(s.ed.panel(), Panel::Results);
    let ticks = run_to_idle(&mut s);
    assert!(
        ticks >= 2,
        "the run finished through polling ({ticks} ticks)"
    );
    assert_eq!(s.ed.wake_ms(), None, "no tick once idle (D39)");
    let (plan, jobs, res) = s.ed.sim().tables().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(
        jobs.state(0),
        State::Done,
        "{:?}",
        res.errors().collect::<Vec<_>>()
    );
    let tau = res.value(0, plan.output("tau").unwrap()).unwrap();
    assert!(
        (tau - 0.632_120_558_828_557_7 / 3300.0).abs() < 1e-9,
        "{tau}"
    );
    // Results table, plot slot, OP marks.
    assert_eq!(node_kind(&s, "results.r.0"), Some(Kind::Panel));
    assert_eq!(node_kind(&s, "results.r.0.s0"), Some(Kind::Label));
    let graphs: Vec<String> = (0..s.ed.shell.tables.nodes.len())
        .filter(|&i| s.ed.shell.tables.nodes.kind()[i] == Kind::Graph)
        .map(|i| {
            s.ed.shell
                .strings
                .get(s.ed.shell.tables.nodes.id()[i])
                .to_owned()
        })
        .collect();
    assert_eq!(graphs.len(), 1, "the plot slot became a graph");
    assert!(s.ed.sim().plots[0].waves.is_ok());
    let op: Vec<(&str, &str)> =
        s.ed.annotations()
            .source
            .iter()
            .zip(&s.ed.annotations().marks)
            .filter(|(src, _)| *src == SOURCE_OP)
            .map(|(_, m)| (m.target.as_str(), m.text.as_str()))
            .collect();
    assert!(op.contains(&("out", "500mV")), "{op:?}");
    assert!(op.contains(&("in", "1V")), "{op:?}");
    assert!(
        s.ed.decor().texts.text.contains("500mV"),
        "drawn on the net"
    );
    activate(&mut s, "waves.op_annotate");
    assert!(!s.ed.annotations().source.iter().any(|x| x == SOURCE_OP));
    activate(&mut s, "waves.op_annotate");
    assert!(s.ed.annotations().source.iter().any(|x| x == SOURCE_OP));
    let px = s.render();
    assert_eq!(px.width(), common::VIEW.0 as u16);
    // Waves on the job, then cross-probe a click on the `out` wire.
    activate(&mut s, "results.waves");
    assert!(s.ed.waves_open(), "{}", s.ed.session.status);
    s.move_screen(640.0, 60.0);
    s.key("f");
    s.click((0, 45 * U));
    assert_eq!(s.ed.waves_traces(), ["v(out)"], "{}", s.ed.session.status);
    let px = s.render();
    assert_eq!(px.height(), common::VIEW.1 as u16);
    // Saved per D18 and reloaded by a fresh editor on the same project.
    let runs = s.dir.join(".schemify/runs");
    let dir = std::fs::read_dir(&runs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(std::fs::read_to_string(dir.join("bench")).unwrap(), "tb\n");
    let ed = Editor::new(Options {
        path: Some(s.dir.join("tb.shbench")),
        seed: 7,
        ..Options::default()
    })
    .unwrap();
    // Both sessions remove the directory when dropped; the second removal finds nothing.
    let mut s2 = Session {
        ed,
        now: 1,
        dir: s.dir.clone(),
    };
    s2.frame(InputEvents::default());
    let (_, jobs, res2) = s2.ed.sim().tables().expect("results reloaded");
    assert_eq!(jobs.state(0), State::Done);
    assert_eq!(res2.value(0, 0), Some(tau));
    assert!(!s2.ed.sim().stale);
    // An edit to the bench makes them stale.
    activate(&mut s2, "view.results");
    s2.click_node("docs.b.1.expr");
    s2.key("Ctrl+A");
    s2.type_text("4.7k");
    s2.key("Enter");
    assert!(s2.ed.sim().stale, "{}", s2.ed.session.status);
}

/// A swept bench overlays its plot slot: one trace per finished job (each sweep point), the
/// selected job's first; the fake's `v(out)` ends at R, so the traces are the jobs' own.
#[test]
fn plot_slots_overlay_every_sweep_point() {
    let dir = fixture("rc", "overlay");
    let tb = dir.join("tb.shbench");
    let text = std::fs::read_to_string(&tb).unwrap();
    let swept = text.replace(
        "optimize fit\n",
        "sweep rs\n  var R\n  values 1k 2.2k 3.3k\n\noptimize fit\n",
    );
    std::fs::write(&tb, swept).unwrap();
    let ed = Editor::new(Options {
        path: Some(tb),
        seed: 7,
        sim: Some(sh(FAKE)),
        ..Options::default()
    })
    .expect("fixture opens");
    let mut s = Session { ed, now: 1, dir };
    s.frame(InputEvents::default());
    s.key("s");
    run_to_idle(&mut s);
    let (_, jobs, _) = s.ed.sim().tables().unwrap();
    assert_eq!(jobs.len(), 3, "{}", s.ed.session.status);
    assert!((0..3).all(|j| jobs.state(j) == State::Done));
    let w = s.ed.sim().plots[0].waves.as_ref().expect("a graph");
    assert_eq!(w.trace_count(), 3, "one trace per sweep point");
}

/// Bench → mid → leaf: a docs page two levels below the bench shows its run (a run opens
/// every cell it reaches); a cell outside the hierarchy does not.
#[test]
fn plot_slots_reach_cells_below_the_bench() {
    const PAGE: &str = "prose 1aaaae\n  | ```plot\n  | v(out)\n  | ```\n";
    let leaf = format!(
        "schemify 1 shcomp\n  prefix x\n\npin a inout 0 0\npin b inout 0 60\n\nimpl sch schematic\n\ninst 1aaaaa ./res\n  at 0 30 R0\n  name R1\n  R 1k\n\nport 1aaaab 0 0 R0 a\nport 1aaaac 0 60 R0 b\n\n{PAGE}"
    );
    let mid = "schemify 1 shcomp\n  prefix x\n\npin a inout 0 0\npin b inout 0 60\n\nimpl sch schematic\n\ninst 2aaaaa ./leaf\n  at 0 0 R0\n  name X1\n\nport 2aaaab 0 0 R0 a\nport 2aaaac 0 60 R0 b\n";
    let bench = "schemify 1 shbench\n\ninst 3aaaaa ./mid\n  at 0 0 R0\n  name X1\n\nlabel 3aaaab 0 0 R0 in\nlabel 3aaaac 0 60 R0 out\n\nvar R 3.3k\n\ncode tran spice\n  | V1 in 0 1\n  | .tran 1u 5m\n";
    let dir = fixture("rc", "deep");
    std::fs::write(dir.join("leaf.shcomp"), &leaf).unwrap();
    std::fs::write(dir.join("other.shcomp"), leaf.replace("1aaaa", "4aaaa")).unwrap();
    std::fs::write(dir.join("mid.shcomp"), mid).unwrap();
    std::fs::write(dir.join("deep.shbench"), bench).unwrap();
    let ed = Editor::new(Options {
        path: Some(dir.join("deep.shbench")),
        seed: 7,
        sim: Some(sh(FAKE)),
        ..Options::default()
    })
    .expect("fixture opens");
    let mut s = Session { ed, now: 1, dir };
    s.frame(InputEvents::default());
    s.key("s");
    run_to_idle(&mut s);
    let (_, jobs, _) = s.ed.sim().tables().expect("a run");
    assert_eq!(jobs.state(0), State::Done, "{}", s.ed.session.status);
    let show = |s: &mut Session, file: &str| {
        let d = s.ed.session.open_path(&s.dir.join(file)).unwrap();
        s.ed.session
            .open_tab(d, schemify_app::session::View::Schematic);
        s.frame(InputEvents::default());
    };
    show(&mut s, "leaf.shcomp");
    let plot = s.ed.sim().plots.first().expect("the leaf's slot is filled");
    assert!(plot.waves.is_ok());
    show(&mut s, "other.shcomp");
    assert!(s.ed.sim().plots.is_empty(), "outside the bench's hierarchy");
}

/// Cancel kills the simulator; the run is idle at once and asks for no more ticks.
#[test]
fn cancel_stops_the_run() {
    let slow = FAKE.replace("sleep 0.3", "sleep 30");
    let mut s = open("rc", "tb.shbench", "cancel", Some(sh(&slow)));
    s.key("s");
    s.frame(InputEvents::default());
    assert!(s.ed.sim().busy());
    activate(&mut s, "sim.cancel");
    assert!(!s.ed.sim().busy());
    assert_eq!(s.ed.sim().tables().unwrap().1.state(0), State::Cancelled);
    s.frame(InputEvents::default());
    assert_eq!(s.ed.wake_ms(), None);
}

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
        assert!(s.ed.sim().busy(), "{}", s.ed.session.status);
        run_to_idle(&mut s);
        let (h, _) = s.ed.active().unwrap();
        let stages = s.ed.session.project.stages(h);
        assert_eq!(stages.len(), 1, "{}", s.ed.session.status);
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
            assert!(s.ed.session.project.stages(h).is_empty());
        } else {
            activate(&mut s, "edit.reject_staged");
            assert_eq!(s.text(), before);
            assert!(s.ed.session.project.stages(h).is_empty());
            assert_eq!(node_kind(&s, "docs.staged"), None);
        }
    }
}

/// A plugin `run` request becomes a job of the same queue and its `RunDone` goes back to the
/// requesting plugin.
#[test]
fn plugin_run_requests_join_the_queue_and_report_done() {
    use schemify_app::session::Session as Project;
    use schemify_app::sim::Sim;
    use schemify_plugin::{PluginId, RunRequest};
    let dir = fixture("rc", "plugin-run");
    let mut session = Project::load(&dir, 1).unwrap();
    let mut sim = Sim::new(Some(sh(&FAKE.replace("sleep 0.3", ""))));
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
    assert!(!sim.busy());
    let _ = std::fs::remove_dir_all(&dir);
}

/// D39: nothing pending, no tick: a fresh editor and one after edits and a netlist ask the
/// window for none.
#[test]
fn idle_editor_schedules_no_ticks() {
    let mut s = open("inv", "top.shcomp", "idle", None);
    assert_eq!(s.ed.wake_ms(), None);
    s.key("n");
    s.drag((130 * U, 10 * U), (130 * U, 110 * U), 0);
    s.frame(InputEvents::default());
    assert_eq!(s.ed.wake_ms(), None);
}

/// With ngspice on `PATH`: the RC bench simulates for real in the editor; tau = RC within
/// 0.5%, and the operating point puts 1 V on both nets (the capacitor is open at DC).
#[test]
fn real_ngspice_simulates_the_rc_bench() {
    let found = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| Path::new(&d).join("ngspice").exists()));
    if !found {
        eprintln!("ngspice not on PATH: skipped");
        return;
    }
    let mut s = open("rc", "tb.shbench", "ngspice", None);
    s.key("s");
    run_to_idle(&mut s);
    let (plan, jobs, res) = s.ed.sim().tables().unwrap();
    assert_eq!(
        jobs.state(0),
        State::Done,
        "{:?}",
        res.errors().collect::<Vec<_>>()
    );
    let tau = res.value(0, plan.output("tau").unwrap()).unwrap();
    assert!((tau - 3.3e-3).abs() < 3.3e-3 * 5e-3, "{tau}");
    let op: Vec<&str> =
        s.ed.annotations()
            .marks
            .iter()
            .filter(|m| m.target == "out")
            .map(|m| m.text.as_str())
            .collect();
    assert_eq!(op, ["1V"]);
    if let Ok(path) = std::env::var("SCHEMIFY_SCREENSHOT") {
        activate(&mut s, "results.waves");
        s.move_screen(640.0, 60.0);
        s.key("f");
        s.click((0, 45 * U));
        let px = s.render();
        common::save_png(&px, Path::new(&path));
    }
}
