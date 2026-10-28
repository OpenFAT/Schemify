//! The M2 integration through the frame spine: the live checks overlay, net highlight,
//! netlisting (`n`) against the CLI's bytes, the non-blocking run with a fake simulator
//! (results table, plot slots, OP back-annotation, cross-probe), the optimizer's staged edit,
//! plugin run requests, and the idle check (no ticks when nothing runs, D39).
//!
//! The fake simulator is `sh` (on Windows the one Git for Windows puts on `PATH`); the
//! real-ngspice test runs when `ngspice` is on `PATH`.

#[path = "../common/mod.rs"]
mod common;
mod engines;
mod netlist;
mod runs;
mod staged;

use std::time::{Duration, Instant};

use common::{Session, fixture};
use schemify_editor::checks::{LAYER_ERROR, LAYER_INFO, LAYER_WARN};
use schemify_editor::editor::{Editor, Options};
use schemify_run::{SimCommand, Tools};
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
        env: Vec::new(),
    }
}

/// The fake simulator `script` with the default compilers.
fn tools(script: &str) -> Tools {
    let mut t = Tools::default();
    t.set_sims(&sh(script));
    t
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
        std::fs::write(dir.join(f), text).expect("the test's temp dir is writable");
    }
    // These tests run ngspice decks (a fake ngspice), so ngspice is pinned: the default
    // (espice, or whichever simulator this machine has) must not decide the dialect.
    let init = dir.join("init.lua");
    std::fs::write(&init, "schemify.opt.simulator = \"ngspice\"\n")
        .expect("the test's temp dir is writable");
    let ed = Editor::new(Options {
        path: Some(dir.join(file)),
        seed: 7,
        sim,
        config: schemify_config::Startup::File(init),
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
    s.send([InputEvent::Activate { node: sym }]);
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
    s.ed.markers()
        .rects
        .layer
        .iter()
        .filter(|l| layers.contains(l))
        .count()
}
