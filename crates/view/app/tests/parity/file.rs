//! File menu parity: save as, recent files, merge, delete, image and project exports.

use schemify_editor::editor::{Editor, Options, Tool};
use schemify_ui::InputEvents;

use crate::common::{self, Session, U, Ui};

#[test]
fn save_as_moves_editing_to_the_new_file() {
    let mut s = Session::open("inv", "top.shcomp", "saveas", Ui::Default);
    let original = std::fs::read_to_string(s.dir.join("top.shcomp")).unwrap();
    s.click((150 * U, 0)); // x2
    s.key("Del");
    let edited = s.text();
    // The dialog opens prefilled with the current name; typing replaces it.
    s.key("Ctrl+Shift+S");
    assert_eq!(super::dialog(&s), "dialog.enter_text");
    for _ in 0.."top.shcomp".len() {
        s.key("BackSpace");
    }
    s.type_text("top2");
    s.key("Enter");
    let new = s.dir.join("top2.shcomp");
    assert_eq!(super::active_path(&s), new, "{}", s.status());
    assert_eq!(std::fs::read_to_string(&new).unwrap(), edited);
    assert_eq!(s.text(), edited);
    let (h, _) = s.ed.active().unwrap();
    assert!(!s.ed.session().project.is_dirty(h), "the new file is saved");
    assert_eq!(
        std::fs::read_to_string(s.dir.join("top.shcomp")).unwrap(),
        original,
        "the old file is untouched"
    );
    assert!(
        !s.dir.join(".schemify/journal/top.shcomp.journal").exists(),
        "the old file's unsaved edits are not offered for recovery"
    );
    // Negative space: an existing name is refused, a wrong extension too.
    s.run("file.save_as", "top2");
    assert!(s.status().contains("same file") || s.status().contains("exists"));
    s.run("file.save_as", "inv.shcomp");
    assert!(s.status().contains("exists"), "{}", s.status());
    s.run("file.save_as", "x.shprim");
    assert!(s.status().contains("keeps its extension"), "{}", s.status());
    // The new cell is indexed: the open chooser lists it.
    s.key("Ctrl+O");
    s.click_node("dialog.choose.filter");
    s.type_text("top2");
    assert!(super::composed(&s, "dialog.choose.row.0"));
    s.key("Esc");
    s.key("Esc");
    // Save as symbol shows the symbol side of the new file.
    s.run("file.save_as_symbol", "top3");
    let (_, view) = s.ed.active().unwrap();
    assert_eq!(view, schemify_editor::session::View::Symbol);
    assert!(s.dir.join("top3.shcomp").exists());
}

#[test]
fn recent_files_persist_and_reopen() {
    let state = std::env::temp_dir().join(format!("schemify-state-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    let dir = common::fixture("inv", "recent");
    let opts = |p: &str| Options {
        path: Some(dir.join(p)),
        seed: 7,
        state_dir: Some(state.clone()),
        ..Default::default()
    };
    {
        let mut ed = Editor::new(opts("top.shcomp")).unwrap();
        ed.update(&InputEvents::default(), common::VIEW, 1);
        ed.command("file.open_most_recent", "");
        ed.update(&InputEvents::default(), common::VIEW, 2);
        assert_eq!(ed.recent().len(), 1);
    }
    let mut ed = Editor::new(opts("inv.shcomp")).unwrap();
    ed.update(&InputEvents::default(), common::VIEW, 1);
    let names: Vec<String> = ed
        .recent()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["inv.shcomp", "top.shcomp"],
        "most recent first, persisted"
    );
    let text = std::fs::read_to_string(state.join("recent")).unwrap();
    assert!(text.starts_with("schemify 1 config\n"), "{text}");
    // The File menu lists them; entry 1 opens top.shcomp in a tab.
    ed.update(&InputEvents::default(), common::VIEW, 3);
    assert!(
        ed.shell()
            .strings()
            .find("file.open_recent.1")
            .is_some_and(|s| ed.shell().tables().nodes.find(s).is_some())
    );
    ed.command("file.open_recent.1", "");
    ed.update(&InputEvents::default(), common::VIEW, 4);
    let (h, _) = ed.active().unwrap();
    assert!(ed.session().path(h).unwrap().ends_with("top.shcomp"));
    assert_eq!(ed.session().tabs().len(), 2);
    // Negative space: an index past the list is a status message, not a crash.
    ed.command("file.open_recent.9", "");
    ed.update(&InputEvents::default(), common::VIEW, 5);
    assert!(ed.session().status().contains("no recent file"));
    let _ = std::fs::remove_dir_all(&state);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merge_inserts_a_cell_schematic_at_the_pointer() {
    let mut s = Session::open("inv", "top.shcomp", "merge", Ui::Default);
    let before = s.text();
    s.move_to((400 * U, 0));
    s.key("b");
    assert_eq!(super::dialog(&s), "dialog.choose");
    s.click_node("dialog.choose.filter");
    s.type_text("inv.sh");
    assert!(
        !super::composed(&s, "dialog.choose.row.1"),
        "one cell matches"
    );
    s.click_node("dialog.choose.row.0");
    assert!(
        matches!(s.ed.tool(), Tool::Place { .. }),
        "{:?}",
        s.ed.tool()
    );
    s.click((400 * U, 0));
    let t = s.text();
    // inv's two transistors, eight wires, two labels, four ports and a text arrive.
    assert_eq!(common::lines(&t, "inst ").len(), 2 + 2, "{t}");
    assert_eq!(common::lines(&t, "wire ").len(), 9 + 8);
    assert!(t.contains("dev:pmos") && t.contains("name M1"));
    s.key("u");
    assert_eq!(s.text(), before, "one undo");
}

#[test]
fn delete_files_needs_a_second_pick() {
    let mut s = Session::open("inv", "top.shcomp", "delete", Ui::Default);
    s.run("file.save_as", "spare");
    let spare = s.dir.join("spare.shcomp");
    assert!(spare.exists());
    // Back to top so spare is not open... it is open (save-as switched to it): refused.
    s.key("Ctrl+Shift+O");
    s.run("file.delete_files", "");
    s.click_node("dialog.choose.filter");
    s.type_text("spare");
    if super::dialog(&s) == "dialog.choose" {
        s.click_node("dialog.choose.row.0");
    }
    assert!(s.status().contains("open in the editor"), "{}", s.status());
    assert!(spare.exists());
    // Write another cell by hand (not open): the first pick only asks.
    std::fs::write(
        s.dir.join("gone.shcomp"),
        "schemify 1 shcomp\n\nimpl sch schematic\n",
    )
    .unwrap();
    s.run("file.save_as", "spare2"); // re-index picks gone.shcomp up
    s.run("file.delete_files", "");
    s.click_node("dialog.choose.filter");
    s.type_text("gone");
    if super::dialog(&s) == "dialog.choose" {
        s.click_node("dialog.choose.row.0");
    }
    assert!(
        s.dir.join("gone.shcomp").exists(),
        "one pick does not delete"
    );
    assert!(s.status().contains("again"), "{}", s.status());
    s.click_node("dialog.choose.filter");
    s.type_text("gone");
    if super::dialog(&s) == "dialog.choose" {
        s.click_node("dialog.choose.row.0");
    }
    assert!(!s.dir.join("gone.shcomp").exists(), "{}", s.status());
}

#[test]
fn image_exports_png_svg_pdf_eps() {
    let mut s = Session::open("inv", "top.shcomp", "export", Ui::Default);
    let png = s.dir.join("out.png");
    s.run("file.image_export.png_export", png.to_str().unwrap());
    let bytes = std::fs::read(&png).unwrap_or_else(|_| panic!("{}", s.status()));
    let px = schemify_render::Pixmap::from_png(std::io::Cursor::new(bytes)).expect("a PNG");
    common::check_golden("export_png", px);
    // SVG: one element per scene primitive (parsed back by element kind).
    let svg = s.dir.join("out.svg");
    s.run("file.image_export.svg_export", svg.to_str().unwrap());
    let text = std::fs::read_to_string(&svg).unwrap();
    let p = &s.ed.scene().prims;
    let counts = schemify_editor::export::svg_counts(&text);
    assert_eq!(counts[0], p.lines.x0.len());
    assert_eq!(counts[1], p.rects.x0.len());
    assert_eq!(counts[2], p.arcs.cx.len());
    assert_eq!(counts[3], p.polylines.end.len() + p.polygons.end.len());
    assert_eq!(counts[4], p.texts.x.len());
    // PDF: the drawing on one page; the hierarchy: top and inv (two instances of one cell).
    let pdf = s.dir.join("out.pdf");
    s.run(
        "file.image_export.pdf_ps_export_full",
        pdf.to_str().unwrap(),
    );
    let t = String::from_utf8(std::fs::read(&pdf).unwrap()).unwrap();
    assert!(t.starts_with("%PDF-1.4") && t.ends_with("%%EOF\n"));
    assert_eq!(t.matches("/Type /Page ").count(), 1);
    s.run(
        "file.image_export.hierarchical_pdf_ps_export",
        pdf.to_str().unwrap(),
    );
    let t = String::from_utf8(std::fs::read(&pdf).unwrap()).unwrap();
    assert_eq!(t.matches("/Type /Page ").count(), 2, "top and inv");
    // EPS of the selection.
    s.click((150 * U, 0));
    let eps = s.dir.join("sel.eps");
    s.run(
        "file.image_export.eps_selection_export",
        eps.to_str().unwrap(),
    );
    let t = std::fs::read_to_string(&eps).unwrap();
    assert!(t.starts_with("%!PS-Adobe-3.0 EPSF-3.0\n"));
    // Default location: .schemify/export/<cell>.<ext>; black on white without color.
    s.run("options.color_postscript_svg", "");
    s.run("file.image_export.svg_export", "");
    let t = std::fs::read_to_string(s.dir.join(".schemify/export/top.svg")).unwrap();
    assert!(
        t.contains("fill=\"#ffffff\"") && !t.contains("#00ccff"),
        "mono"
    );
}

/// File → Export project (web spec §3): the archive holds exactly the listed project files
/// under `<name>/`, byte for byte (oracle: the files on disk, read back through the zip
/// reader that CPython's archives check), hidden entries (journals, earlier exports) left
/// out, and the same bytes again on a second export. Import is web-only: natively it only
/// reports.
#[test]
fn export_project_zips_the_listed_files() {
    let mut s = Session::open("inv", "top.shcomp", "export_project", Ui::Default);
    s.run("file.export_project", "");
    let name = s.dir.file_name().unwrap().to_str().unwrap().to_owned();
    let zip = s.dir.join(".schemify/export").join(format!("{name}.zip"));
    let bytes = std::fs::read(&zip)
        .unwrap_or_else(|e| panic!("{}: {e} (status: {})", zip.display(), s.status()));
    let files = schemify_cli::zip::read(&bytes, usize::MAX).expect("the archive reads");
    let want = schemify_editor::platform::list_files(&s.dir, &[]);
    assert!(!want.is_empty());
    assert_eq!(files.len(), want.len());
    for ((n, b), p) in files.iter().zip(&want) {
        let rel = schemify_editor::libs::rel_path(&s.dir, p).unwrap();
        assert_eq!(n, &format!("{name}/{rel}"));
        assert_eq!(b, &std::fs::read(p).unwrap(), "{n}");
    }
    assert!(
        files.iter().all(|(n, _)| !n.contains("/.")),
        "hidden left out"
    );
    s.run("file.export_project", "");
    assert_eq!(
        std::fs::read(&zip).unwrap(),
        bytes,
        "same project, same bytes"
    );
    s.run("file.import_project", "");
    assert!(s.status().contains("directory"), "{}", s.status());
}
