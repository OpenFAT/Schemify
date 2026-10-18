//! `project.reload` (pdk spec §9.2): a manifest change takes effect without a restart. A new
//! `lib` and a `lib tech map` resolve after the command, the open document (with an unsaved
//! edit) stays open, and a manifest that drops an open document's library is refused.

mod common;

use std::path::Path;

use common::{Session, copy_dir};
use schemify_app::editor::{Editor, Options};

fn session(tag: &str) -> Session {
    let dir =
        std::env::temp_dir().join(format!("schemify-editor-pdk-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cli = Path::new(env!("CARGO_MANIFEST_DIR")).join("../cli/tests/fixtures/tech");
    // The fixture carries the PDK manager's maps in `maps/`.
    copy_dir(&cli, &dir);
    std::fs::write(
        dir.join("schemify.project"),
        "schemify 1 project\nlib sky130_fd_pr path pdk/sky130/sky130_fd_pr\n",
    )
    .unwrap();
    let ed = Editor::new(Options {
        no_default_ui: true,
        path: Some(dir.join("inv.shcomp")),
        seed: 7,
        ..Default::default()
    })
    .expect("opens");
    let mut s = Session { ed, now: 1, dir };
    s.frame(Default::default());
    s
}

#[test]
fn reload_picks_up_a_manifest_change() {
    let mut s = session("change");
    let h = s.ed.session.project.handles().next().expect("inv is open");
    assert!(
        s.ed.session.project.resolve(h, "tech:nmos").is_err(),
        "no technology yet"
    );
    std::fs::create_dir_all(s.dir.join("extra")).unwrap();
    std::fs::write(
        s.dir.join("extra/buf.shprim"),
        "schemify 1 shprim\n\npin a in 0 0\n\nimpl spice spice\n",
    )
    .unwrap();
    std::fs::write(
        s.dir.join("schemify.project"),
        "schemify 1 project\nlib sky130_fd_pr path pdk/sky130/sky130_fd_pr\nlib extra path extra\nlib tech map maps/sky130.map\n",
    )
    .unwrap();
    s.ed.command("project.reload", "");
    s.frame(Default::default());
    let p = &s.ed.session.project;
    assert!(p.doc(h).is_some(), "the open document stays open");
    assert!(
        p.resolve(h, "extra:buf").is_ok(),
        "the new library resolves"
    );
    let row = p
        .resolve(h, "tech:nmos")
        .expect("tech:nmos resolves through sky130's map");
    assert!(
        p.interface(row)
            .unwrap()
            .canonical()
            .contains("sky130_fd_pr__{model}")
    );
    assert!(
        s.ed.session.status.contains("reloaded"),
        "{}",
        s.ed.session.status
    );
    // Switching technology is one manifest line.
    std::fs::write(
        s.dir.join("schemify.project"),
        "schemify 1 project\nlib sky130_fd_pr path pdk/sky130/sky130_fd_pr\nlib gf180mcu_fd_pr path pdk/gf180mcu/gf180mcu_fd_pr\nlib tech map maps/gf180mcu.map\n",
    )
    .unwrap();
    s.ed.command("project.reload", "");
    s.frame(Default::default());
    let p = &s.ed.session.project;
    let row = p.resolve(h, "tech:pmos").expect("gf180 pmos");
    assert!(p.interface(row).unwrap().canonical().contains("pfet_03v3"));
    assert!(
        p.resolve(h, "extra:buf").is_err(),
        "the dropped library is gone"
    );
}

#[test]
fn reload_refuses_to_orphan_an_open_document() {
    let mut s = session("orphan");
    let lib = s.dir.join("pdk/sky130/sky130_fd_pr/nfet_01v8.shprim");
    let h = s.ed.session.open_path(&lib).expect("opens the PDK cell");
    std::fs::write(s.dir.join("schemify.project"), "schemify 1 project\n").unwrap();
    s.ed.command("project.reload", "");
    s.frame(Default::default());
    assert!(
        s.ed.session
            .status
            .contains("close the documents of library `sky130_fd_pr`"),
        "{}",
        s.ed.session.status
    );
    let p = &s.ed.session.project;
    assert!(p.doc(h).is_some());
    assert!(
        p.resolve(h, "./nfet_01v8").is_ok(),
        "the old manifest still holds"
    );
}
