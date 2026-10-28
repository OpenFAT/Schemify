//! The non-blocking run with the fake simulator: benches, polling to results, waves and OP,
//! plot slots, cancel, two benches at once, the results table and pane, and the idle check.

use std::time::{Duration, Instant};

use schemify_editor::checks::SOURCE_OP;
use schemify_editor::editor::{Editor, Options, Panel};
use schemify_run::State;
use schemify_ui::{InputEvents, Kind};

use super::{FAKE, activate, node_kind, open, open_with, run_to_idle, sh, tools};
use crate::common::{self, Session, U, fixture};

/// `s` on a component explains D11b; "New bench from this component" writes a `.shbench`
/// instantiating it with a label per pin, opened in a new tab.
#[test]
fn simulate_needs_a_bench_and_new_bench_makes_one() {
    let mut s = open("inv", "inv.shcomp", "newbench", None);
    s.key("s");
    assert!(s.status().contains(".shbench"), "{}", s.status());
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
    assert!(s.ed.sim().busy(), "{}", s.status());
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
            .iter()
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
    assert!(!s.ed.annotations().has_source(SOURCE_OP));
    activate(&mut s, "waves.op_annotate");
    assert!(s.ed.annotations().has_source(SOURCE_OP));
    let px = s.render();
    assert_eq!(px.width(), common::VIEW.0 as u16);
    // Waves on the job, then cross-probe a click on the `out` wire.
    activate(&mut s, "results.waves");
    assert!(s.ed.waves_open(), "{}", s.status());
    s.move_screen(640.0, 60.0);
    s.key("f");
    s.click((0, 45 * U));
    assert_eq!(s.ed.waves_traces(), ["v(out)"], "{}", s.status());
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
    // Reopened with the same pinned simulator (the fixture's init.lua): a run is stale for
    // another dialect.
    let ed = Editor::new(Options {
        path: Some(s.dir.join("tb.shbench")),
        seed: 7,
        config: schemify_config::Startup::File(s.dir.join("init.lua")),
        ..Options::default()
    })
    .unwrap();
    // Both sessions remove the directory when dropped; the second removal finds nothing.
    let mut s2 = Session {
        caches: Default::default(),
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
    assert!(s2.ed.sim().stale(), "{}", s2.status());
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
    let mut s = Session {
        ed,
        now: 1,
        dir,
        caches: Default::default(),
    };
    s.frame(InputEvents::default());
    s.key("s");
    run_to_idle(&mut s);
    let (_, jobs, _) = s.ed.sim().tables().unwrap();
    assert_eq!(jobs.len(), 3, "{}", s.status());
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
    let mut s = Session {
        ed,
        now: 1,
        dir,
        caches: Default::default(),
    };
    s.frame(InputEvents::default());
    s.key("s");
    run_to_idle(&mut s);
    let (_, jobs, _) = s.ed.sim().tables().expect("a run");
    assert_eq!(jobs.state(0), State::Done, "{}", s.status());
    let show = |s: &mut Session, file: &str| {
        s.run("file.open", file);
        assert!(s.status().contains("opened"), "{}", s.status());
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

/// Two benches simulate at once; a Simulate of a bench whose run is in flight waits for it
/// (a queue per bench) and starts when it is idle.
#[test]
fn two_benches_run_at_once_and_each_bench_queues_its_own() {
    use schemify_editor::session::Session as Project;
    use schemify_editor::sim::Sim;
    let dir = fixture("rc", "two-runs");
    let tb = std::fs::read_to_string(dir.join("tb.shbench")).unwrap();
    std::fs::write(dir.join("tb2.shbench"), &tb).unwrap();
    let mut session = Project::load(&dir, 1, None).unwrap();
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
    let mismatched = tb.replace("  max 10k\n", "  max 10k\n  dist gauss 50\n");
    let many = format!(
        "{mismatched}\nsweep rsw\n  var R\n  lin 1k 10k 100\n\nmontecarlo mc\n  samples 2\n  seed 3\n"
    );
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
    assert!(log.out.len() <= schemify_editor::sim::LOG_LINES);
}
