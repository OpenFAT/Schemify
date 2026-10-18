//! The user config in the editor (config spec §3, §5, §6), through the real frame spine:
//! options seed the edit options, a Lua keymap runs a command, the user's keymaps win over
//! the default UI's (D8), hooks see editor events, and a lazy package is not instantiated
//! until its command runs.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use schemify_app::editor::{Editor, Options};
use schemify_app::plugins::Host;
use schemify_config::Startup;
use schemify_ui::InputEvents;

fn write_init(dir: &Path, src: &str) -> PathBuf {
    let init = dir.join("init.lua");
    std::fs::write(&init, src).unwrap();
    init
}

fn open_with(dir: &Path, init: &Path) -> Session {
    let ed = Editor::new(Options {
        path: Some(dir.join("top.shcomp")),
        seed: 7,
        config: Startup::File(init.to_path_buf()),
        ..Default::default()
    })
    .expect("fixture opens");
    let mut s = Session {
        ed,
        now: 1,
        dir: dir.to_path_buf(),
    };
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    s
}

#[test]
fn options_keymaps_and_hooks_reach_the_editor() {
    let dir = fixture("inv", "userconfig");
    let init = write_init(
        &dir,
        r#"
schemify.opt.snap = 5
schemify.opt.light = true
-- `%` toggles the grid in the default keymap; the user's binding wins (D8, spec §6).
schemify.keymap.set("%", "view.redraw")
schemify.keymap.set("F7", function() schemify.cmd("options.draw_grid") end)
schemify.on("load", function() schemify.notify("hello from init.lua") end)
"#,
    );
    let mut s = open_with(&dir, &init);
    assert_eq!(s.ed.opts().snap, 5 * U);
    assert!(s.ed.opts().light);
    assert!(s.ed.opts().show_grid);
    assert!(
        s.ed.session().status.contains("hello from init.lua"),
        "{}",
        s.ed.session().status
    );
    s.key("%");
    assert!(
        s.ed.opts().show_grid,
        "the user's `%` replaced the default UI's grid toggle"
    );
    // The Lua function queues `options.draw_grid`; it runs at the next delivery.
    s.key("F7");
    s.frame(InputEvents::default());
    assert!(!s.ed.opts().show_grid, "F7 ran the Lua keymap");
    s.key("F7");
    s.frame(InputEvents::default());
    assert!(s.ed.opts().show_grid);
}

#[test]
fn clean_startup_and_config_errors() {
    let dir = fixture("inv", "userconfig-err");
    let init = write_init(&dir, "schemify.opt.snap = 5\nschemify.opt.nope = 1\n");
    let s = open_with(&dir, &init);
    assert_eq!(s.ed.opts().snap, 5 * U, "set before the error");
    assert!(
        s.ed.session()
            .status
            .contains("init.lua:2: unknown option `nope`"),
        "{}",
        s.ed.session().status
    );
    // The default (tests, `--clean`) never runs a user config.
    let clean = Session::open("inv", "top.shcomp", "userconfig-clean", true);
    assert_eq!(clean.ed.opts().snap, 10 * U);
}

/// A lazy user package (config spec §5): absent from the plugin table until its command runs,
/// then loaded and handed that command.
#[test]
fn a_lazy_package_loads_on_its_first_command() {
    let dir = fixture("inv", "userconfig-lazy");
    let pkg = dir.join("hello-pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    let wasm = Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugin/tests/fixtures/hello.wasm");
    std::fs::copy(&wasm, pkg.join("hello.wasm")).unwrap();
    let init = write_init(
        &dir,
        &format!(
            "schemify.pack.add {{ {{ path = [==[{}]==], name = 'hello', lazy = {{ cmd = 'count_nets.run' }} }} }}",
            pkg.display()
        ),
    );
    let project = schemify_doc::Project::new(None, None, 1).unwrap();
    let (mut host, notes) = Host::new(&project, &dir, true, &Startup::File(init));
    assert!(notes.is_empty(), "{notes:?}");
    assert!(host.is_lazy("hello"));
    assert!(
        host.plugins.ids().is_empty(),
        "nothing instantiated before the command"
    );
    let d = host.deliver(&project, &dir, 0);
    assert!(d.logs.is_empty() && host.plugins.ids().is_empty());
    assert!(!host.user_command("nobody.owns_this", ""));
    assert!(host.user_command("count_nets.run", ""));
    assert!(!host.is_lazy("hello"));
    assert_eq!(host.plugins.ids().len(), 1);
    let d = host.deliver(&project, &dir, 0);
    assert!(
        d.logs.iter().any(|l| l.contains("single-connection nets")),
        "the command reached the plugin: {:?}",
        d.logs
    );
}
