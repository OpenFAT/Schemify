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
use schemify_run::{SimCommand, State, Tools};
use schemify_ui::{InputEvent, InputEvents, Kind, Prop};

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
        env: Vec::new(),
    }
}

/// The fake simulator `script` with the default compilers.
fn tools(script: &str) -> Tools {
    Tools {
        sim: sh(script),
        ..Tools::default()
    }
}

/// Opens `file` of fixture `name` with the default UI and the simulator `sim`.
fn open(name: &str, file: &str, tag: &str, sim: Option<SimCommand>) -> Session {
    open_with(name, &[], file, tag, sim)
}

/// [`open`] after writing `files` (name, text) into the fixture's copy.
fn open_with(
    name: &str,
    files: &[(&str, &str)],
    file: &str,
    tag: &str,
    sim: Option<SimCommand>,
) -> Session {
    let dir = fixture(name, tag);
    for (f, text) in files {
        std::fs::write(dir.join(f), text).unwrap();
    }
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
        s.ed.shell()
            .strings()
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
    let sh = &s.ed.shell();
    let sym = sh.strings().find(id)?;
    let n = sh.tables().nodes.find(sym)?;
    Some(sh.tables().nodes.kind()[n as usize])
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
        let doc = s.ed.session().project.doc(h).unwrap();
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
    assert_eq!(hl.keys, keys, "{}", s.ed.session().status);
    assert_eq!(hl.pins.len(), pins);
    assert_eq!(pins, 2, "x1.VDD and x2.VDD");
    assert_eq!(s.ed.highlight_keys(), ["vdd"]);
    // Descend into x1: the child's VDD (port + label + wires) is the same net.
    s.click((-10 * U, 0));
    s.key("e");
    assert_eq!(s.ed.session().hier_path(), ".x1.");
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
    let bytes = std::fs::read(&file).unwrap_or_else(|e| panic!("{e}: {}", s.ed.session().status));
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
        s.ed.session()
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
        s.ed.session().status.contains("blocked"),
        "{}",
        s.ed.session().status
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
        s.ed.session().status.contains(".shbench"),
        "{}",
        s.ed.session().status
    );
    activate(&mut s, "sim.new_bench");
    let text = std::fs::read_to_string(s.dir.join("inv_tb.shbench")).unwrap();
    assert!(text.starts_with("schemify 1 shbench\n"), "{text}");
    assert!(text.contains(" ./inv\n"), "{text}");
    for pin in ["A", "Y", "VDD", "VSS"] {
        assert!(text.contains(&format!(" R0 {pin}\n")), "{pin}: {text}");
    }
    let (h, _) = s.ed.active().unwrap();
    let doc = s.ed.session().project.doc(h).unwrap();
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
    assert!(s.ed.sim().busy(), "{}", s.ed.session().status);
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
    // OP annotation is on: the job's netlist saves currents and every vector (run §7a).
    let job0 = s.ed.sim().dir().unwrap().join("0").join("netlist.cir");
    let netlist = std::fs::read_to_string(job0).unwrap();
    assert!(
        netlist.ends_with(".save all\n.options savecurrents\n.end\n"),
        "{netlist}"
    );
    // Results table, plot slot, OP marks.
    assert_eq!(node_kind(&s, "results.r.0"), Some(Kind::Panel));
    assert_eq!(node_kind(&s, "results.r.0.s0"), Some(Kind::Label));
    let graphs: Vec<String> = (0..s.ed.shell().tables().nodes.len())
        .filter(|&i| s.ed.shell().tables().nodes.kind()[i] == Kind::Graph)
        .map(|i| {
            s.ed.shell()
                .strings()
                .get(s.ed.shell().tables().nodes.id()[i])
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
    assert!(s.ed.waves_open(), "{}", s.ed.session().status);
    s.move_screen(640.0, 60.0);
    s.key("f");
    s.click((0, 45 * U));
    assert_eq!(s.ed.waves_traces(), ["v(out)"], "{}", s.ed.session().status);
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
    assert!(!s2.ed.sim().stale());
    // An edit to the bench makes them stale.
    activate(&mut s2, "view.results");
    s2.click_node("docs.b.1.expr");
    s2.key("Ctrl+A");
    s2.type_text("4.7k");
    s2.key("Enter");
    assert!(s2.ed.sim().stale(), "{}", s2.ed.session().status);
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
    assert_eq!(jobs.len(), 3, "{}", s.ed.session().status);
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
    assert_eq!(jobs.state(0), State::Done, "{}", s.ed.session().status);
    let show = |s: &mut Session, file: &str| {
        s.ed.command("file.open", file);
        s.frame(InputEvents::default());
        assert!(
            s.ed.session().status.contains("opened"),
            "{}",
            s.ed.session().status
        );
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
        assert!(s.ed.sim().busy(), "{}", s.ed.session().status);
        run_to_idle(&mut s);
        let (h, _) = s.ed.active().unwrap();
        let stages = s.ed.session().project.stages(h);
        assert_eq!(stages.len(), 1, "{}", s.ed.session().status);
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
    use schemify_app::session::Session as Project;
    use schemify_app::sim::Sim;
    use schemify_plugin::{PluginId, RunRequest};
    let dir = fixture("rc", "plugin-run");
    let mut session = Project::load(&dir, 1).unwrap();
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

/// Two benches simulate at once; a Simulate of a bench whose run is in flight waits for it
/// (a queue per bench) and starts when it is idle.
#[test]
fn two_benches_run_at_once_and_each_bench_queues_its_own() {
    use schemify_app::session::Session as Project;
    use schemify_app::sim::Sim;
    let dir = fixture("rc", "two-runs");
    let tb = std::fs::read_to_string(dir.join("tb.shbench")).unwrap();
    std::fs::write(dir.join("tb2.shbench"), &tb).unwrap();
    let mut session = Project::load(&dir, 1).unwrap();
    let a = session.open_path(&dir.join("tb.shbench")).unwrap();
    let b = session.open_path(&dir.join("tb2.shbench")).unwrap();
    let mut sim = Sim::new(tools(&FAKE.replace("sleep 0.3", "sleep 0.5")));
    let env: Vec<(&str, &str)> = Vec::new();
    let funcs = &mut schemify_elab::no_funcs;
    sim.start(&mut session, a, None, "", &env, funcs).unwrap();
    sim.start(&mut session, b, None, "", &env, funcs).unwrap();
    let msg = sim.start(&mut session, a, None, "", &env, funcs).unwrap();
    assert!(msg.contains("queued"), "{msg}");
    assert_eq!(sim.queued(), 1);
    let (mut both, mut a_done, mut a_again) = (false, false, false);
    let t = Instant::now();
    while sim.busy() {
        assert!(t.elapsed() < Duration::from_secs(60), "{}", sim.status);
        sim.serve_queue(&mut session, &env, funcs);
        sim.poll(&session.project, &env, funcs);
        let state = |h| sim.tables_of(h).map(|(_, j, _)| j.state(0));
        both |= state(a) == Some(State::Running) && state(b) == Some(State::Running);
        match state(a) {
            Some(s) if s.finished() => a_done = true,
            Some(_) if a_done => a_again = true,
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(both, "the two benches' simulators ran at the same time");
    assert!(a_again, "the queued Simulate of tb ran after its first run");
    assert_eq!(sim.queued(), 0);
    for h in [a, b] {
        let (plan, jobs, res) = sim.tables_of(h).unwrap();
        let errs: Vec<_> = res.errors().collect();
        assert_eq!(jobs.state(0), State::Done, "{errs:?}");
        let tau = res.value(0, plan.output("tau").unwrap()).unwrap();
        assert!(
            (tau - 0.632_120_558_828_557_7 / 3300.0).abs() < 1e-9,
            "{tau}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The results table over 200 jobs (100 sweep points × 2 Monte Carlo samples): a window of
/// rows around the scroll position with spacers for the rest, header buttons that sort
/// (again: reversed), a filter, and the Monte Carlo yield as a table row.
#[test]
fn results_table_sorts_filters_and_scrolls_any_number_of_jobs() {
    let src = fixture("rc", "table-src");
    let tb = std::fs::read_to_string(src.join("tb.shbench")).unwrap();
    let _ = std::fs::remove_dir_all(&src);
    let many = tb.replace("  max 10k\n", "  max 10k\n  dist gauss 50\n")
        + "\nsweep rsw\n  var R\n  lin 1k 10k 100\n\nmontecarlo mc\n  samples 2\n  seed 3\n";
    assert_ne!(many, tb);
    let fast = FAKE.replace("sleep 0.3", "");
    let mut s = open_with(
        "rc",
        &[("many.shbench", &many)],
        "many.shbench",
        "table",
        Some(sh(&fast)),
    );
    s.key("s");
    run_to_idle(&mut s);
    let (plan, jobs, res) = s.ed.sim().tables().unwrap();
    assert_eq!(jobs.len(), 200);
    assert!(jobs.states().iter().all(|&st| st == State::Done));
    let tau = plan.output("tau").unwrap();
    let values: Vec<f64> = (0..200).map(|j| res.value(j, tau).unwrap()).collect();
    // A window of rows, spacers for the rest; the Monte Carlo yield is a row.
    assert_eq!(node_kind(&s, "results.r.0"), Some(Kind::Panel));
    assert_eq!(node_kind(&s, "results.r.150"), None);
    assert_eq!(node_kind(&s, "results.gap-bottom.0"), Some(Kind::Panel));
    assert_eq!(node_kind(&s, "results.yield"), Some(Kind::Panel));
    assert_eq!(node_kind(&s, "results.yield.s0"), Some(Kind::Label));
    let list = s.ed.shell().strings().find("results.list").unwrap();
    s.ed.set_scroll(list, 152.0 * 19.0, 1e9);
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    assert_eq!(
        node_kind(&s, "results.r.150"),
        Some(Kind::Panel),
        "scrolled to it"
    );
    assert_eq!(node_kind(&s, "results.r.0"), None);
    assert_eq!(node_kind(&s, "results.gap-top.0"), Some(Kind::Panel));
    // Sort by tau (output 0: names sort `tau` < `v1m`), then reversed.
    s.ed.set_scroll(list, 0.0, 1e9);
    activate(&mut s, "results.head.o0");
    let rows = s.ed.sim().table.rows.clone();
    assert_eq!(rows.len(), 200);
    let ordered = |rows: &[u32], up: bool| {
        rows.windows(2).all(|w| {
            let (a, b) = (values[w[0] as usize], values[w[1] as usize]);
            if up { a <= b } else { a >= b }
        })
    };
    assert!(ordered(&rows, true), "ascending tau");
    assert_eq!(
        node_kind(&s, &format!("results.r.{}", rows[0])),
        Some(Kind::Panel)
    );
    activate(&mut s, "results.head.o0");
    let rows = s.ed.sim().table.rows.clone();
    assert!(ordered(&rows, false), "descending tau");
    // Filter: the last sweep point (R = 10000), both samples.
    s.click_node("results.filter");
    s.type_text("r=10000");
    s.key("Enter");
    let (_, jobs, _) = s.ed.sim().tables().unwrap();
    let want: Vec<u32> = (0..200)
        .filter(|&j| jobs.assignments(j).next() == Some(("R", "10000")))
        .collect();
    assert_eq!(want.len(), 2);
    let mut got = s.ed.sim().table.rows.clone();
    got.sort_unstable();
    assert_eq!(got, want, "filter {:?}", s.ed.sim().table.filter);
    assert_eq!(node_kind(&s, "results.filtered"), Some(Kind::Label));
}

/// Xyce writes `.op` as a one-point DC plot with bare upper-case node names: the OP marks
/// read it like ngspice's operating point.
#[test]
fn op_annotation_reads_xyce_naming() {
    let xyce = r#"printf 'Title: fake\nPlotname: DC transfer characteristic\nFlags: real\nNo. Variables: 4\nNo. Points: 1\nVariables:\n 0 sweep voltage\n 1 IN voltage\n 2 OUT voltage\n 3 V1#branch current\nValues:\n0 0\n 1\n 0.5\n -0.001\n' > "$2"
"#;
    let mut s = open("rc", "tb.shbench", "xyce-op", Some(sh(xyce)));
    s.key("s");
    run_to_idle(&mut s);
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
        !op.iter().any(|m| m.0 == "sweep"),
        "the sweep scale is no net"
    );
}

/// With ngspice on `PATH`: an NMOS (level 1, kp 100µ, vto 0.5, W/L 1) at Vgs 1 V, Vds 1.8 V
/// is in saturation, so gm = kp·(Vgs − vto) = 50 µS and id = kp/2·(Vgs − vto)² = 12.5 µA.
/// OP annotation saved `@m1[gm]` (a `.save` the editor added), and `.options
/// savecurrents` wrote the drain current under the name cross-probe asks for.
#[test]
fn real_ngspice_saves_mos_op_quantities_and_terminal_currents() {
    let found = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| Path::new(&d).join("ngspice").exists()));
    if !found {
        eprintln!("ngspice not on PATH: skipped");
        return;
    }
    let nmos = "schemify 1 shprim\n  prefix M\n\npin d inout 0 -30\npin g in -30 0\n\
                pin s inout 0 30\npin b inout 30 0\n\nimpl spice spice\n  \
                format spice \"M{name} {d} {g} {s} {b} nm W=1u L=1u\"\n";
    let bench = "schemify 1 shbench\n\ninst 0aaaaa ./nmos\n  at 0 0 R0\n  name 1\n\n\
                 label 0bbbba 0 -30 R0 d\nlabel 0bbbbb -30 0 R0 g\nlabel 0bbbbc 0 30 R0 0\n\
                 label 0bbbbd 30 0 R0 0\n\ncode op spice\n  | Vd d 0 1.8\n  | Vg g 0 1\n  \
                 | .model nm nmos level=1 vto=0.5 kp=100u\n  | .op\n";
    let mut s = open_with(
        "rc",
        &[("nmos.shprim", nmos), ("mos.shbench", bench)],
        "mos.shbench",
        "mos-op",
        None,
    );
    s.key("s");
    run_to_idle(&mut s);
    let (_, jobs, res) = s.ed.sim().tables().unwrap();
    let errs: Vec<_> = res.errors().collect();
    assert_eq!(jobs.state(0), State::Done, "{errs:?}");
    let marks: Vec<&str> =
        s.ed.annotations()
            .marks
            .iter()
            .filter(|m| m.target == "/1")
            .map(|m| m.text.as_str())
            .collect();
    let gm = format!("gm={}", schemify_wave::format_si(50e-6, 4));
    let id = format!("id={}", schemify_wave::format_si(12.5e-6, 4));
    assert!(marks.contains(&gm.as_str()), "{marks:?}");
    assert!(marks.contains(&id.as_str()), "{marks:?}");
    // The vector cross-probe asks for on the drain pin is the one ngspice wrote.
    let probe = schemify_app::waves::current_probe("M1", "d");
    assert_eq!(probe, "i(\"@m1[id]\")");
    // ngspice writes saved device vectors wrapped by kind; `i("@m1[id]")` binds `i(@m1[id])`.
    let raw = std::fs::read(s.ed.sim().job_raw(0).unwrap()).unwrap();
    let plots = schemify_wave::parse_raw(&raw).unwrap();
    assert!(
        plots
            .iter()
            .any(|p| p.vars().iter().any(|v| v.name == "i(@m1[id])")),
        "savecurrents wrote i(@m1[id])"
    );
}

/// A bus highlights by every bit: `k` on the single-bit wire `d[0]` lights the bus `d[1:0]`
/// that carries it (not `d[1]`, which only the bus shares); `k` on the bus lights both bit
/// wires. The oracle is elab's membership: components carrying any highlighted bit.
#[test]
fn bus_components_highlight_by_every_bit() {
    let bench = "schemify 1 shbench\n\nwire 0w0001 0 0 100 0\n  bus true\nwire 0w0002 0 100 100 100\n\
                 wire 0w0003 0 200 100 200\n\nlabel 0n0001 0 0 R0 d[1:0]\nlabel 0n0002 0 100 R0 d[0]\n\
                 label 0n0003 0 200 R0 d[1]\n";
    let mut s = open_with(
        "inv",
        &[("bus.shbench", bench)],
        "bus.shbench",
        "bus-hilite",
        None,
    );
    let keys = |s: &Session, rows: &[usize]| {
        let (h, _) = s.ed.active().unwrap();
        let doc = s.ed.session().project.doc(h).unwrap();
        let mut k: Vec<ObjKey> = rows
            .iter()
            .flat_map(|&r| {
                [
                    ObjKey {
                        table: TableId::Wires,
                        key: doc.wires().id[r].0,
                    },
                    ObjKey {
                        table: TableId::Labels,
                        key: doc.labels().id[r].0,
                    },
                ]
            })
            .collect();
        k.sort_unstable();
        k
    };
    // Rows follow the ids: 0 the bus, 1 `d[0]`, 2 `d[1]`.
    s.click((50 * U, 100 * U));
    assert_eq!(s.ed.selection().len(), 1);
    s.key("k");
    assert_eq!(
        s.ed.highlighted().keys,
        keys(&s, &[0, 1]),
        "{}",
        s.ed.session().status
    );
    s.key("Shift+K");
    assert!(s.ed.highlighted().keys.is_empty());
    s.click((50 * U, 0));
    s.key("k");
    assert_eq!(s.ed.highlight_keys().len(), 2, "one key per bit");
    assert_eq!(s.ed.highlighted().keys, keys(&s, &[0, 1, 2]));
}

/// Staged edits under review: inserted records are drawn where they will be, deleted objects
/// crossed out, changed ones outlined; with two stages the banner steps between them and
/// Accept / Reject act on the one under review.
#[test]
fn staged_edits_draw_inserts_and_deletes_and_review_one_stage_at_a_time() {
    use schemify_app::checks::LAYER_DIFF;
    use schemify_doc::{Edit, EditRequest, FileKind, Mode, Transaction, ops_from_text};
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

/// Find in the netlist tab: the numbered lines of the whole file that contain the text,
/// case-insensitively; an empty find shows the netlist again.
#[test]
fn netlist_tab_finds_lines_of_the_whole_file() {
    let mut s = open("inv", "top.shcomp", "find", None);
    s.key("n");
    let text = std::fs::read_to_string(s.dir.join(".schemify/netlist/top.spice")).unwrap();
    assert!(
        s.ed.session().text_view().is_some(),
        "{}",
        s.ed.session().status
    );
    let find = |s: &mut Session, q: &str| {
        s.click_node("textview.find");
        s.key("Ctrl+A");
        s.key("Del");
        if !q.is_empty() {
            s.type_text(q);
        }
        s.key("Enter");
    };
    find(&mut s, "SUBCKT");
    let want: Vec<String> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| l.to_ascii_lowercase().contains("subckt"))
        .map(|(i, l)| format!("{:>6}: {l}", i + 1))
        .collect();
    assert!(!want.is_empty(), "{text}");
    let (count, lines) = s.ed.session().text_view().unwrap().found.clone().unwrap();
    assert_eq!(count as usize, want.len());
    assert_eq!(lines.lines().collect::<Vec<_>>(), want);
    assert_eq!(node_kind(&s, "textview.found"), Some(Kind::Label));
    find(&mut s, "no such text anywhere");
    let (count, lines) = s.ed.session().text_view().unwrap().found.clone().unwrap();
    assert_eq!((count, lines.as_str()), (0, ""));
    find(&mut s, "");
    assert!(s.ed.session().text_view().unwrap().found.is_none());
    assert_eq!(node_kind(&s, "textview.found"), None);
}

/// The results pane shows the selected job's run log (#47): the simulator command line that
/// ran on the netlist, and the tail of what the simulator printed; a failing simulator's
/// stderr is visible without opening a file.
#[test]
fn results_pane_shows_the_run_log() {
    let mut s = open("rc", "tb.shbench", "runlog", Some(sh(FAKE)));
    s.key("s");
    run_to_idle(&mut s);
    let log = &s.ed.sim().log;
    assert!(log.command.contains("netlist.cir"), "{log:?}");
    assert!(
        node_kind(&s, "results.log.cmd").is_some(),
        "the command row is shown"
    );

    let failing = "echo starting; echo 'fatal: no such model nfet' >&2; exit 3";
    let mut f = open("rc", "tb.shbench", "runlog-fail", Some(sh(failing)));
    f.key("s");
    run_to_idle(&mut f);
    let log = &f.ed.sim().log;
    assert!(
        log.err.iter().any(|l| l.contains("no such model")),
        "{log:?}"
    );
    assert!(log.out.iter().any(|l| l == "starting"), "{log:?}");
    assert!(node_kind(&f, "results.log.err.0").is_some());
    assert!(log.out.len() <= schemify_app::sim::LOG_LINES);
}

/// The label of composed UI node `id`.
fn label(s: &Session, id: &str) -> String {
    let sh = &s.ed.shell();
    let n = sh
        .strings()
        .find(id)
        .and_then(|i| sh.tables().nodes.find(i))
        .unwrap_or_else(|| panic!("{id} composed"));
    sh.strings()
        .get(sh.tables().nodes.prop(Prop::Label)[n as usize])
        .to_owned()
}

/// Simulation → Netlist dialect → ESPice: the status shows it, and Simulate netlists the
/// bench for espice (the fake simulator refuses any other deck) and records its results.
/// Back on ngspice the next run is ngspice again (negative space).
#[test]
fn the_espice_dialect_simulates_an_espice_deck() {
    let only = |word: &str| {
        format!(
            "head -1 \"$1\" | grep -q '(schemify {word})' || {{ echo 'not {word}' >&2; exit 3; }}\n{FAKE}"
        )
    };
    let mut s = open("rc", "tb.shbench", "espice", Some(sh(&only("espice"))));
    assert_eq!(label(&s, "status.mode"), "ngspice");
    activate(&mut s, "sim.dialect.espice");
    assert_eq!(label(&s, "status.mode"), "espice");
    s.key("s");
    run_to_idle(&mut s);
    let (_, jobs, res) = s.ed.sim().tables().unwrap();
    assert_eq!(
        jobs.state(0),
        State::Done,
        "{:?}",
        res.errors().collect::<Vec<_>>()
    );
    activate(&mut s, "sim.dialect.ngspice");
    s.key("s");
    run_to_idle(&mut s);
    let (_, jobs, res) = s.ed.sim().tables().unwrap();
    assert_eq!(jobs.state(0), State::Failed, "an ngspice deck this time");
    assert!(res.errors().any(|e| e.2.contains("not espice")));
}

/// Children of `textview` and the source lines of its prose body.
fn text_tab_nodes(s: &Session) -> (usize, Vec<String>) {
    let sh = &s.ed.shell();
    let tv = sh
        .tables()
        .nodes
        .find(sh.strings().find("textview").unwrap());
    let kids = sh.tables().nodes.children(tv.unwrap()).count();
    let body = sh.strings().find("textview.body").unwrap();
    let body = sh.tables().nodes.find(body).unwrap();
    let src = sh
        .strings()
        .get(sh.tables().nodes.prop(Prop::Value)[body as usize]);
    (kids, src.lines().map(str::to_owned).collect())
}

fn open_text(s: &mut Session, body: String) {
    let path = s.dir.join("big.txt");
    s.ed.open_text_tab(schemify_app::session::TextView {
        title: "big".into(),
        path,
        body,
        find: String::new(),
        found: None,
    });
    s.frame(InputEvents::default());
}

/// A 16k-line text tab is windowed: the body node holds at most `TEXT_WINDOW` lines and the
/// tab a bounded number of nodes; its pixels equal those of a short (unwindowed) tab holding
/// the same lines from a little above the screen; a line that wraps stops the window from
/// starting below it; scrolled to the end it shows the last line.
#[test]
fn a_long_text_tab_lays_out_a_window_of_lines() {
    use schemify_app::session::TEXT_WINDOW;
    let mut s = Session::open("inv", "top.shcomp", "textwin", true);
    let mut body: Vec<String> = (0..16_384)
        .map(|i| format!("R{i} n{i} n{} {}k", i + 1, i % 97))
        .collect();
    body.push("… (truncated; the whole netlist is in big.txt)".into());
    let n = body.len();
    let first = |lines: &[String]| body.iter().position(|l| *l == lines[1]).unwrap();
    open_text(&mut s, body.join("\n"));
    let tv =
        s.ed.shell()
            .strings()
            .find("textview")
            .expect("the text tab composed");
    let at = 10_000.0 * 12.0;
    s.ed.set_scroll(tv, at, 1e9);
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    let (kids, lines) = text_tab_nodes(&s);
    let bound = TEXT_WINDOW as usize + 2;
    assert!(lines.len() <= bound, "{} lines in the body", lines.len());
    assert!(
        (9_900..10_000).contains(&first(&lines)),
        "{}",
        first(&lines)
    );
    // find + body + spacers of at most 2^14 px each over 16k rows of 12 px.
    assert!(kids <= 2 + 2 * (n * 12 / (1 << 14) + 1), "{kids} nodes");
    let windowed = s.render();
    // Oracle: the lines from 9960 on as a short tab, scrolled by as much less.
    open_text(&mut s, body[9_960..10_200].join("\n"));
    s.ed.set_scroll(tv, at - 9_960.0 * 12.0, 1e9);
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    let (kids, lines) = text_tab_nodes(&s);
    assert_eq!(
        (kids, lines.len()),
        (2, 242),
        "the oracle is one unwindowed node"
    );
    let oracle = s.render();
    assert!(
        oracle.data() == windowed.data(),
        "the window paints what the whole text does"
    );
    // Negative space: a line that wraps (here at 5000) is never above the window.
    let mut wraps = body.clone();
    wraps[5_000] = "word ".repeat(400);
    open_text(&mut s, wraps.join("\n"));
    s.ed.set_scroll(tv, at, 1e9);
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    let (_, lines) = text_tab_nodes(&s);
    assert_eq!(
        lines[1], wraps[5_000],
        "the window starts at the wrapping line"
    );
    assert!(lines.len() > 5_000, "and reaches the screen");
    open_text(&mut s, body.join("\n"));
    // The end: the last line is in the window.
    s.ed.set_scroll(tv, 1e9, 1e9);
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    let (_, lines) = text_tab_nodes(&s);
    assert!(lines.len() <= bound);
    assert_eq!(lines[lines.len() - 2], body[n - 1]);
    assert_eq!(node_kind(&s, "textview.gap-bottom.0"), None);
    assert_eq!(node_kind(&s, "textview.gap-top.0"), Some(Kind::Panel));
}

/// A short text tab is one body node with the whole text and no spacers, as before windowing.
#[test]
fn a_short_text_tab_is_one_node() {
    let mut s = Session::open("inv", "top.shcomp", "textshort", true);
    let body = "* netlist\nR1 a b 1k\n.end".to_owned();
    open_text(&mut s, body.clone());
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    let (kids, lines) = text_tab_nodes(&s);
    assert_eq!(lines.join("\n"), format!("```\n{body}\n```"));
    assert_eq!(kids, 2, "find and body");
    assert_eq!(node_kind(&s, "textview.gap-top.0"), None);
}
