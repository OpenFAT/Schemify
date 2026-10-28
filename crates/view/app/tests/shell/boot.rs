//! Booting without the default UI plugin (D6), the plugins that own commands, manifest
//! plugins, the help menu, and `cmd` running a plugin command headless.

use schemify_ui::Kind;

use super::node_prop;
use crate::common::{self, Session, U, Ui, VIEW};

#[test]
fn boots_without_the_default_ui_plugin() {
    let mut s = Session::open("inv", "top.shcomp", "bare", Ui::Bare);
    let n = &s.ed.shell().tables().nodes;
    let kinds: Vec<Kind> = (0..n.len()).map(|i| n.kind()[i]).collect();
    // The core base also holds the docs pane (D21: docs render without plugins); a cell
    // without doc blocks starts with it removed.
    assert_eq!(
        kinds,
        [Kind::Window, Kind::Split, Kind::Canvas],
        "blank UI: window + work split + canvas only"
    );
    assert!(
        s.ed.shell().tables().keymap.chords().is_empty(),
        "no plugin, no keymap"
    );
    let r = s.ed.canvas_rect();
    assert_eq!(
        (r.width() as f32, r.height() as f32),
        VIEW,
        "the canvas fills the window"
    );
    assert!(!s.ed.scene().objs.is_empty(), "the schematic still draws");
    // Keys are unbound, the mouse still edits: drag x2 down.
    let before = s.text();
    s.key("w");
    assert_eq!(s.text(), before);
    s.drag((130 * U, 10 * U), (130 * U, 60 * U), 0);
    assert!(
        s.text().contains("  at 140 50 R0\n  name x2\n"),
        "{}",
        s.text()
    );
    let px = s.render();
    assert_eq!((px.width(), px.height()), (VIEW.0 as u16, VIEW.1 as u16));
}

/// A plugin-owned command (the default UI's `help.keys`, bound to `/`) re-contributes UI.
#[test]
fn default_ui_plugin_owns_its_commands() {
    let mut s = Session::open("inv", "top.shcomp", "helpkeys", Ui::Default);
    let panel = |s: &Session| {
        let id = s.ed.shell().strings().find("keys.panel");
        id.and_then(|id| s.ed.shell().tables().nodes.find(id))
            .is_some()
    };
    assert!(!panel(&s));
    s.key("/");
    assert!(
        panel(&s),
        "the plugin answered its command with a new ui set"
    );
    s.key("/");
    assert!(!panel(&s));
}

/// A manifest plugin loads after the default UI (D8 order), adds a menu item, and its
/// command is delivered to it; its log line reaches the status bar.
#[test]
fn manifest_plugins_load_in_order_and_receive_commands() {
    let dir = common::fixture("inv", "hello");
    let wasm = concat!(
        env!("SCHEMIFY_ROOT"),
        "/crates/plugins/plugin/tests/fixtures/hello.wasm"
    );
    std::fs::copy(wasm, dir.join("hello.wasm")).unwrap();
    let manifest = std::fs::read_to_string(dir.join("schemify.project")).unwrap();
    std::fs::write(
        dir.join("schemify.project"),
        format!("{manifest}\nplugin hello path hello.wasm\n"),
    )
    .unwrap();
    let ed = schemify_editor::editor::Editor::new(schemify_editor::editor::Options {
        no_default_ui: false,
        path: Some(dir.join("top.shcomp")),
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
    s.frame(Default::default());
    assert_eq!(
        s.ed.shell().tables().owners.len(),
        4,
        "base, default-ui, hello, state"
    );
    s.click_node("menu.tools");
    s.click_node("tools.count_nets");
    assert!(
        s.status().contains("single-connection nets"),
        "{}",
        s.status()
    );
}

/// Help has one key-bindings item (bound to `/`) and "About Schemify" (#49); nothing names
/// xschem.
#[test]
fn help_menu_is_schemify_branded_without_duplicates() {
    use schemify_ui::Prop;
    let s = Session::open("inv", "top.shcomp", "help-menu", Ui::Default);
    let sh = &s.ed.shell();
    assert!(
        sh.strings()
            .find("help.show_keybindings")
            .and_then(|i| sh.tables().nodes.find(i))
            .is_none()
    );
    assert_eq!(node_prop(&s, "help.keys", Prop::Accel), "/");
    assert_eq!(node_prop(&s, "help.about", Prop::Label), "About Schemify");
    let labels = sh.tables().nodes.prop(Prop::Label);
    assert!(
        labels
            .iter()
            .all(|&l| !sh.strings().get(l).to_lowercase().contains("xschem")),
        "no menu label names xschem"
    );
}

/// `schemify-gui cmd <path> <command>` (`schemify cmd`, D27) runs a plugin command without a
/// window: the manifest plugin loads, its log reaches stdout and the exit status is 0; a
/// command no one serves is a usage error (2); a plugin call that aborts (out of fuel) is
/// exit 1 with the fault on stdout.
#[test]
fn cmd_runs_a_plugin_command_headless() {
    let dir = common::fixture("inv", "cmd");
    let guests = concat!(
        env!("SCHEMIFY_ROOT"),
        "/crates/plugins/plugin/tests/fixtures"
    );
    for w in ["hello.wasm", "bad_actor.wasm"] {
        std::fs::copy(format!("{guests}/{w}"), dir.join(w)).unwrap();
    }
    let manifest = std::fs::read_to_string(dir.join("schemify.project")).unwrap();
    std::fs::write(
        dir.join("schemify.project"),
        format!("{manifest}\nplugin hello path hello.wasm\nplugin bad path bad_actor.wasm\n"),
    )
    .unwrap();
    // A run that does not finish in 120 s is killed and fails (`None`), never hangs the suite.
    let run = |args: &[&str]| {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_schemify-gui"))
            .arg("--clean")
            .arg("cmd")
            .arg(dir.join("top.shcomp"))
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let t0 = std::time::Instant::now();
        while child.try_wait().unwrap().is_none() {
            if t0.elapsed().as_secs() > 120 {
                let _ = child.kill();
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let text = format!("{stdout}{stderr}");
        (out.status.code(), text)
    };
    let (code, text) = run(&["count_nets.run"]);
    assert_eq!(code, Some(0), "{text}");
    assert!(text.contains("single-connection nets"), "{text}");
    let (code, text) = run(&["no_such.command"]);
    assert_eq!(code, Some(2), "{text}");
    let (code, text) = run(&["bad.spin"]);
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("out of fuel"), "{text}");

    // Bug fix: a plugin with raised fuel runs on a worker; taking its outputs in refreshed the
    // palette, which re-sent `SelectionChanged` to it, every frame, so it never went idle and
    // `cmd` never returned. Bounded by the test's own clock.
    std::fs::write(
        dir.join("schemify.project"),
        format!("{manifest}\nplugin hello path hello.wasm\n  fuel 1000000000\n"),
    )
    .unwrap();
    let (code, text) = run(&["count_nets.run"]);
    assert_eq!(code, Some(0), "{text}");
    assert!(text.contains("single-connection nets"), "{text}");
}
