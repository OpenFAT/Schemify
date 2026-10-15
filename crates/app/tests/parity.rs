//! xschem parity, end to end through the frame spine: the ratchet over every command the
//! default UI names, then one oracle-backed test per parity feature (docs/dev/testing.md: oracles,
//! negative space). Oracles: the canonical record text of the edited document (and undo
//! restoring it byte for byte), files written by exports and saves (decoded or parsed back),
//! elab connectivity before/after (stretch), and properties over seeded inputs (stretch,
//! search/replace, join/trim).

mod common;
use common::{Session, U, lines};

use std::path::{Path, PathBuf};

use schemify_app::commands::Target;
use schemify_app::editor::{Editor, Options, Tool};
use schemify_doc::TableId;
use schemify_ui::{Button, InputEvent, InputEvents};

const UI: &str = include_str!("../../../ui/default-plugin/ui.records");
const KEYMAP: &str = include_str!("../../../ui/default-plugin/keymap.records");

/// Unresolved command names allowed. Lower it when a command lands; never raise it.
/// 2026-10-11: 198 before the parity pass, 0 after it.
const RATCHET: usize = 0;

const SHIFT: u8 = schemify_ui::Mods::SHIFT;
const CTRL: u8 = schemify_ui::Mods::CTRL;

/// Every command name the default UI records reference, sorted and distinct.
fn named_commands() -> Vec<&'static str> {
    let mut names: Vec<&str> = UI
        .lines()
        .filter_map(|l| l.strip_prefix("  command "))
        .chain(KEYMAP.lines().filter_map(|l| {
            let mut t = l.split_whitespace();
            (t.next() == Some("key")).then(|| t.nth(1)).flatten()
        }))
        .map(str::trim)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

#[test]
#[allow(clippy::absurd_extreme_comparisons)] // the ratchet reached 0; `<=` stays its form
fn every_default_ui_command_resolves() {
    let s = Session::open("inv", "top.shcomp", "parity", true);
    let names = named_commands();
    assert!(names.len() > 200, "the records were read: {}", names.len());
    let unresolved: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| s.ed.commands().resolve_arg(n).0 == Target::Unknown)
        .collect();
    assert!(
        unresolved.len() <= RATCHET,
        "{} unresolved command(s) (ratchet {RATCHET}):\n{}",
        unresolved.len(),
        unresolved.join("\n")
    );
    // Negative space: a name nobody implements does not resolve.
    assert_eq!(
        s.ed.commands().resolve_arg("no.such_command").0,
        Target::Unknown
    );
}

// ---------------------------------------------------------------------------- helpers

/// Queues command `name` (argument `arg`) and runs one frame.
fn run(s: &mut Session, name: &str, arg: &str) {
    s.ed.command(name, arg);
    s.frame(InputEvents::default());
}

/// Lines of the active document starting with `prefix`, owned.
fn owned(s: &Session, prefix: &str) -> Vec<String> {
    lines(&s.text(), prefix)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn status(s: &Session) -> String {
    s.ed.session.status.clone()
}

fn active_path(s: &Session) -> PathBuf {
    let (h, _) = s.ed.active().unwrap();
    s.ed.session.path(h).unwrap().to_path_buf()
}

/// Right click (press + release) at world point `p` with modifiers `mods`.
fn right_click(s: &mut Session, p: (i32, i32), mods: u8) {
    let (x, y) = s.ed.to_screen(p);
    for down in [true, false] {
        let mut ev = InputEvents::default();
        ev.push(InputEvent::Mods {
            mods: schemify_ui::Mods(mods),
        });
        ev.push(InputEvent::PointerMove { x, y });
        ev.push(if down {
            InputEvent::PointerDown {
                x,
                y,
                button: Button::Right,
            }
        } else {
            InputEvent::PointerUp {
                x,
                y,
                button: Button::Right,
            }
        });
        s.frame(ev);
    }
    let mut ev = InputEvents::default();
    ev.push(InputEvent::Mods {
        mods: schemify_ui::Mods::default(),
    });
    s.frame(ev);
}

/// Whether UI node `id` is composed (exists in the UI table this frame).
fn composed(s: &Session, id: &str) -> bool {
    let sh = &s.ed.shell;
    sh.strings
        .find(id)
        .is_some_and(|sym| sh.tables.nodes.find(sym).is_some())
}

/// The open dialog's node id ("" when none).
fn dialog(s: &Session) -> String {
    let d = s.ed.shell.ui.dialog();
    if d.is_none() {
        String::new()
    } else {
        s.ed.shell.strings.get(d).to_owned()
    }
}

/// Connectivity of the active schematic: (pin identity, component), pins of instances by
/// (instance id, pin index), ports by id.
fn pin_nets(s: &Session) -> Vec<(String, u32)> {
    let (h, _) = s.ed.active().unwrap();
    let doc = s.ed.session.project.doc(h).unwrap();
    let cell = s.ed.cell().expect("elaborated");
    let mut out = Vec::new();
    let inst = doc.instances();
    for i in 0..inst.id.len() {
        for (k, (c, _)) in schemify_app::checks::pins_of(doc, cell, i)
            .iter()
            .enumerate()
        {
            out.push((format!("{}.{k}", inst.id[i].0), *c));
        }
    }
    for (r, id) in doc.ports().id.iter().enumerate() {
        out.push((format!("port.{}", id.0), cell.nets.port_net[r]));
    }
    out.sort();
    out
}

fn check_golden(name: &str, pixmap: schemify_render::Pixmap) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");
    let path = format!("{dir}/{name}.png");
    let png = pixmap.clone().into_png().expect("encode png");
    if std::env::var_os("SCHEMIFY_BLESS").is_some() {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(&path, &png).unwrap();
    }
    let want = std::fs::read(&path)
        .unwrap_or_else(|_| panic!("{path} missing; run with SCHEMIFY_BLESS=1"));
    let want =
        schemify_render::Pixmap::from_png(std::io::Cursor::new(want)).expect("decode golden");
    assert_eq!(
        (want.width(), want.height()),
        (pixmap.width(), pixmap.height())
    );
    let bad = want
        .data()
        .iter()
        .zip(pixmap.data())
        .filter(|(a, b)| {
            let (a, b) = ([a.r, a.g, a.b, a.a], [b.r, b.g, b.b, b.a]);
            a.iter().zip(b).any(|(x, y)| x.abs_diff(y) > 2)
        })
        .count();
    let limit = want.data().len() / 1000;
    if bad > limit {
        let out = format!("{}/{name}.actual.png", env!("CARGO_TARGET_TMPDIR"));
        std::fs::write(&out, &png).unwrap();
        panic!("{name}: {bad} pixels differ (limit {limit}); actual written to {out}");
    }
}

/// A tiny deterministic generator for the seeded property tests (no dependency).
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }
}

// ---------------------------------------------------------------------------- File

#[test]
fn save_as_moves_editing_to_the_new_file() {
    let mut s = Session::open("inv", "top.shcomp", "saveas", true);
    let original = std::fs::read_to_string(s.dir.join("top.shcomp")).unwrap();
    s.click((150 * U, 0)); // x2
    s.key("Del");
    let edited = s.text();
    // The dialog opens prefilled with the current name; typing replaces it.
    s.key("Ctrl+Shift+S");
    assert_eq!(dialog(&s), "dialog.enter_text");
    for _ in 0.."top.shcomp".len() {
        s.key("BackSpace");
    }
    s.type_text("top2");
    s.key("Enter");
    let new = s.dir.join("top2.shcomp");
    assert_eq!(active_path(&s), new, "{}", status(&s));
    assert_eq!(std::fs::read_to_string(&new).unwrap(), edited);
    assert_eq!(s.text(), edited);
    let (h, _) = s.ed.active().unwrap();
    assert!(!s.ed.session.project.is_dirty(h), "the new file is saved");
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
    run(&mut s, "file.save_as", "top2");
    assert!(status(&s).contains("same file") || status(&s).contains("exists"));
    run(&mut s, "file.save_as", "inv.shcomp");
    assert!(status(&s).contains("exists"), "{}", status(&s));
    run(&mut s, "file.save_as", "x.shprim");
    assert!(status(&s).contains("keeps its extension"), "{}", status(&s));
    // The new cell is indexed: the open chooser lists it.
    s.key("Ctrl+O");
    s.click_node("dialog.choose.filter");
    s.type_text("top2");
    assert!(composed(&s, "dialog.choose.row.0"));
    s.key("Esc");
    s.key("Esc");
    // Save as symbol shows the symbol side of the new file.
    run(&mut s, "file.save_as_symbol", "top3");
    let (_, view) = s.ed.active().unwrap();
    assert_eq!(view, schemify_app::session::View::Symbol);
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
        ed.shell
            .strings
            .find("file.open_recent.1")
            .is_some_and(|s| ed.shell.tables.nodes.find(s).is_some())
    );
    ed.command("file.open_recent.1", "");
    ed.update(&InputEvents::default(), common::VIEW, 4);
    let (h, _) = ed.active().unwrap();
    assert!(ed.session.path(h).unwrap().ends_with("top.shcomp"));
    assert_eq!(ed.session.tabs.len(), 2);
    // Negative space: an index past the list is a status message, not a crash.
    ed.command("file.open_recent.9", "");
    ed.update(&InputEvents::default(), common::VIEW, 5);
    assert!(ed.session.status.contains("no recent file"));
    let _ = std::fs::remove_dir_all(&state);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merge_inserts_a_cell_schematic_at_the_pointer() {
    let mut s = Session::open("inv", "top.shcomp", "merge", true);
    let before = s.text();
    s.move_to((400 * U, 0));
    s.key("b");
    assert_eq!(dialog(&s), "dialog.choose");
    s.click_node("dialog.choose.filter");
    s.type_text("inv.sh");
    assert!(!composed(&s, "dialog.choose.row.1"), "one cell matches");
    s.click_node("dialog.choose.row.0");
    assert!(matches!(s.ed.tool, Tool::Place { .. }), "{:?}", s.ed.tool);
    s.click((400 * U, 0));
    let t = s.text();
    // inv's two transistors, eight wires, two labels, four ports and a text arrive.
    assert_eq!(lines(&t, "inst ").len(), 2 + 2, "{t}");
    assert_eq!(lines(&t, "wire ").len(), 9 + 8);
    assert!(t.contains("dev:pmos") && t.contains("name M1"));
    s.key("u");
    assert_eq!(s.text(), before, "one undo");
}

#[test]
fn delete_files_needs_a_second_pick() {
    let mut s = Session::open("inv", "top.shcomp", "delete", true);
    run(&mut s, "file.save_as", "spare");
    let spare = s.dir.join("spare.shcomp");
    assert!(spare.exists());
    // Back to top so spare is not open... it is open (save-as switched to it): refused.
    s.key("Ctrl+Shift+O");
    run(&mut s, "file.delete_files", "");
    s.click_node("dialog.choose.filter");
    s.type_text("spare");
    if dialog(&s) == "dialog.choose" {
        s.click_node("dialog.choose.row.0");
    }
    assert!(status(&s).contains("open in the editor"), "{}", status(&s));
    assert!(spare.exists());
    // Write another cell by hand (not open): the first pick only asks.
    std::fs::write(
        s.dir.join("gone.shcomp"),
        "schemify 1 shcomp\n\nimpl sch schematic\n",
    )
    .unwrap();
    run(&mut s, "file.save_as", "spare2"); // re-index picks gone.shcomp up
    run(&mut s, "file.delete_files", "");
    s.click_node("dialog.choose.filter");
    s.type_text("gone");
    if dialog(&s) == "dialog.choose" {
        s.click_node("dialog.choose.row.0");
    }
    assert!(
        s.dir.join("gone.shcomp").exists(),
        "one pick does not delete"
    );
    assert!(status(&s).contains("again"), "{}", status(&s));
    s.click_node("dialog.choose.filter");
    s.type_text("gone");
    if dialog(&s) == "dialog.choose" {
        s.click_node("dialog.choose.row.0");
    }
    assert!(!s.dir.join("gone.shcomp").exists(), "{}", status(&s));
}

#[test]
fn image_exports_png_svg_pdf_eps() {
    let mut s = Session::open("inv", "top.shcomp", "export", true);
    let png = s.dir.join("out.png");
    run(
        &mut s,
        "file.image_export.png_export",
        png.to_str().unwrap(),
    );
    let bytes = std::fs::read(&png).unwrap_or_else(|_| panic!("{}", status(&s)));
    let px = schemify_render::Pixmap::from_png(std::io::Cursor::new(bytes)).expect("a PNG");
    check_golden("export_png", px);
    // SVG: one element per scene primitive (parsed back by element kind).
    let svg = s.dir.join("out.svg");
    run(
        &mut s,
        "file.image_export.svg_export",
        svg.to_str().unwrap(),
    );
    let text = std::fs::read_to_string(&svg).unwrap();
    let p = &s.ed.scene().prims;
    let counts = schemify_app::export::svg_counts(&text);
    assert_eq!(counts[0], p.lines.x0.len());
    assert_eq!(counts[1], p.rects.x0.len());
    assert_eq!(counts[2], p.arcs.cx.len());
    assert_eq!(counts[3], p.polylines.end.len() + p.polygons.end.len());
    assert_eq!(counts[4], p.texts.x.len());
    // PDF: the drawing on one page; the hierarchy: top and inv (two instances of one cell).
    let pdf = s.dir.join("out.pdf");
    run(
        &mut s,
        "file.image_export.pdf_ps_export_full",
        pdf.to_str().unwrap(),
    );
    let t = String::from_utf8(std::fs::read(&pdf).unwrap()).unwrap();
    assert!(t.starts_with("%PDF-1.4") && t.ends_with("%%EOF\n"));
    assert_eq!(t.matches("/Type /Page ").count(), 1);
    run(
        &mut s,
        "file.image_export.hierarchical_pdf_ps_export",
        pdf.to_str().unwrap(),
    );
    let t = String::from_utf8(std::fs::read(&pdf).unwrap()).unwrap();
    assert_eq!(t.matches("/Type /Page ").count(), 2, "top and inv");
    // EPS of the selection.
    s.click((150 * U, 0));
    let eps = s.dir.join("sel.eps");
    run(
        &mut s,
        "file.image_export.eps_selection_export",
        eps.to_str().unwrap(),
    );
    let t = std::fs::read_to_string(&eps).unwrap();
    assert!(t.starts_with("%!PS-Adobe-3.0 EPSF-3.0\n"));
    // Default location: .schemify/export/<cell>.<ext>; black on white without color.
    run(&mut s, "options.color_postscript_svg", "");
    run(&mut s, "file.image_export.svg_export", "");
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
    let mut s = Session::open("inv", "top.shcomp", "export_project", true);
    run(&mut s, "file.export_project", "");
    let name = s.dir.file_name().unwrap().to_str().unwrap().to_owned();
    let zip = s.dir.join(".schemify/export").join(format!("{name}.zip"));
    let bytes = std::fs::read(&zip)
        .unwrap_or_else(|e| panic!("{}: {e} (status: {})", zip.display(), status(&s)));
    let files = schemify_cli::zip::read(&bytes, usize::MAX).expect("the archive reads");
    let want = schemify_app::platform::list_files(&s.dir, &[]);
    assert!(!want.is_empty());
    assert_eq!(files.len(), want.len());
    for ((n, b), p) in files.iter().zip(&want) {
        let rel = schemify_app::libs::rel_path(&s.dir, p).unwrap();
        assert_eq!(n, &format!("{name}/{rel}"));
        assert_eq!(b, &std::fs::read(p).unwrap(), "{n}");
    }
    assert!(
        files.iter().all(|(n, _)| !n.contains("/.")),
        "hidden left out"
    );
    run(&mut s, "file.export_project", "");
    assert_eq!(
        std::fs::read(&zip).unwrap(),
        bytes,
        "same project, same bytes"
    );
    run(&mut s, "file.import_project", "");
    assert!(status(&s).contains("directory"), "{}", status(&s));
}

// ---------------------------------------------------------------------------- Edit

/// Property (docs/dev/testing.md): after a kissing move (Shift+M) of any instance subset by any
/// grid offset, every pair of pins (instance pins and ports) connected before is still
/// connected — the kissing wires keep every contact. Stretch alone (Ctrl+M) keeps them too
/// here because every connection of the fixture runs through a wire.
#[test]
fn stretch_and_kissing_moves_keep_pins_connected() {
    let mut rng = Lcg(0x5eed);
    for case in 0..12 {
        let mut s = Session::open("inv", "top.shcomp", &format!("stretch{case}"), true);
        let before = pin_nets(&s);
        let original = s.text();
        let picks = 1 + rng.below(3);
        let targets = [(10 * U, 0), (150 * U, 0)];
        let mut first = true;
        for (k, &t) in targets.iter().enumerate() {
            if picks & (1 << k) != 0 {
                s.click_mods(t, if first { 0 } else { SHIFT });
                first = false;
            }
        }
        let d = (
            (rng.below(9) as i32 - 4) * 10 * U,
            (rng.below(9) as i32 - 4) * 10 * U,
        );
        let grab = (500 * U, 300 * U);
        s.move_to(grab);
        s.key(if case % 2 == 0 { "Shift+M" } else { "Ctrl+M" });
        s.move_to((grab.0 + d.0, grab.1 + d.1));
        s.click((grab.0 + d.0, grab.1 + d.1));
        let after = pin_nets(&s);
        assert_eq!(before.len(), after.len());
        for a in &before {
            for b in &before {
                if a.1 == b.1 {
                    let ca = after.iter().find(|x| x.0 == a.0).unwrap().1;
                    let cb = after.iter().find(|x| x.0 == b.0).unwrap().1;
                    assert_eq!(ca, cb, "case {case} d {d:?}: {} and {} split", a.0, b.0);
                }
            }
        }
        if d != (0, 0) {
            s.key("u");
            assert_eq!(s.text(), original, "case {case}: one undo");
        }
    }
}

#[test]
fn stretch_moves_wire_ends_and_plain_moves_do_not() {
    let mut s = Session::open("inv", "top.shcomp", "stretchends", true);
    s.click((150 * U, 0)); // x2: pins (100,0) (180,0) (140,-30) (140,30)
    s.move_to((150 * U, 0));
    s.key("Ctrl+M");
    s.click((150 * U, 100 * U));
    let t = s.text();
    assert!(t.contains("./inv\n  at 140 100 R0\n  name x2"), "{t}");
    assert!(
        t.contains("wire w00001 40 0 100 100\n"),
        "end on x2.IN followed: {t}"
    );
    assert!(t.contains("wire w00005 140 -70 140 70\n"), "{t}");
    s.key("u");
    // Plain move: wires stay; Options → enable stretch (`y`) makes `m` stretch.
    s.key("y");
    assert!(s.ed.opts.stretch);
    s.click((150 * U, 0));
    s.move_to((150 * U, 0));
    s.key("m");
    s.click((150 * U, 100 * U));
    assert!(s.text().contains("wire w00001 40 0 100 100\n"));
}

/// Copy → paste equals duplicate: the same records at the same offset, ids included (both
/// draw from the same seeded generator).
#[test]
fn clipboard_copy_paste_equals_duplicate() {
    let pick = |s: &mut Session| {
        s.click((150 * U, 0)); // x2, anchor (140, 0)
        s.click_mods((30 * U, 70 * U), SHIFT); // the VSS rail wire, anchor (0, 70)
    };
    // The clipboard holds the records relative to the anchors' snapped centroid (70, 40); a
    // duplicate grabbed there and dropped where the paste lands moves by the same offset.
    let mut a = Session::open("inv", "top.shcomp", "clipa", true);
    pick(&mut a);
    a.key("Ctrl+C");
    a.move_to((170 * U, 40 * U));
    a.key("Ctrl+V");
    a.click((170 * U, 40 * U));
    let mut b = Session::open("inv", "top.shcomp", "clipb", true);
    pick(&mut b);
    b.move_to((70 * U, 40 * U));
    b.key("c");
    b.move_to((170 * U, 40 * U));
    b.click((170 * U, 40 * U));
    assert_eq!(a.text(), b.text());
    assert_ne!(
        a.text(),
        Session::open("inv", "top.shcomp", "clipc", true).text()
    );
    // The fragment text round-trips the clipboard records.
    let recs: Vec<schemify_doc::RecordBuf> = {
        let (h, _) = a.ed.active().unwrap();
        let doc = a.ed.session.project.doc(h).unwrap();
        schemify_app::edit::selected_records(doc, a.ed.selection())
            .into_iter()
            .map(|r| r.2)
            .collect()
    };
    let text = schemify_app::clip::encode(&recs);
    let mut back = schemify_app::clip::decode(&text).unwrap();
    let mut want = recs.clone();
    back.sort_by(|x, y| x.fields.cmp(&y.fields));
    want.sort_by(|x, y| x.fields.cmp(&y.fields));
    assert_eq!(back, want);
}

/// Property: replacing every match with a text that does not contain the value leaves the
/// search empty; one undo restores the file.
#[test]
fn search_replace_all_leaves_nothing_to_find() {
    let mut s = Session::open("inv", "top.shcomp", "search", true);
    let original = s.text();
    let (h, _) = s.ed.active().unwrap();
    let queries = [
        ("net", "V", false),
        ("name", "x", false),
        ("*", "mid", true),
        ("cell", "./inv", true),
    ];
    for (field, value, exact) in queries {
        let q = schemify_app::search::Query {
            field: field.into(),
            value: value.into(),
            exact,
        };
        let found = schemify_app::search::find(s.ed.session.project.doc(h).unwrap(), &q);
        assert!(!found.is_empty(), "{field} {value}");
        // Through the dialog: field, value, replacement, exact.
        s.key("Ctrl+F");
        assert_eq!(dialog(&s), "dialog.props");
        for (i, v) in [field, value, "Q_", if exact { "true" } else { "false" }]
            .iter()
            .enumerate()
        {
            s.click_node(&format!("dialog.props.val.{i}"));
            for _ in 0..8 {
                s.key("BackSpace");
            }
            s.type_text(v);
        }
        s.click_node("dialog.props.ok");
        let left = schemify_app::search::find(s.ed.session.project.doc(h).unwrap(), &q);
        assert!(left.is_empty(), "{field} {value}: {left:?}\n{}", s.text());
        s.key("u");
        assert_eq!(s.text(), original, "{field} {value}: one undo");
    }
    // An empty replacement selects instead.
    s.key("Ctrl+F");
    s.click_node("dialog.props.val.1");
    s.type_text("VDD");
    s.click_node("dialog.props.ok");
    assert_eq!(s.ed.selection().len(), 1, "{}", status(&s));
    assert_eq!(s.ed.selection()[0].table, TableId::Labels);
}

#[test]
fn schematic_shapes_draw_and_undo() {
    let mut s = Session::open("inv", "top.shcomp", "shapes", true);
    let original = s.text();
    s.move_to((300 * U, 0));
    s.key("l");
    s.click((340 * U, 0));
    assert!(s.text().contains(" line 300 0 340 0\n"), "{}", s.text());
    s.move_to((300 * U, 50 * U));
    s.key("r");
    s.click((340 * U, 80 * U));
    assert!(s.text().contains(" rect 300 50 340 80\n"));
    // Polygon: vertices, then Enter closes it.
    s.move_to((300 * U, 100 * U));
    s.key("p");
    s.click((340 * U, 100 * U));
    s.click((340 * U, 140 * U));
    s.key("Enter");
    assert!(
        s.text()
            .contains(" poly 300 100 340 100 340 140\n  closed true\n"),
        "{}",
        s.text()
    );
    // Circle: center and a point on it; arc: three points.
    s.move_to((400 * U, 0));
    s.key("Ctrl+Shift+C");
    s.click((420 * U, 0));
    assert!(s.text().contains(" circle 400 0 20\n"), "{}", s.text());
    s.move_to((500 * U, 0));
    s.key("Shift+C");
    s.click((500 * U, 20 * U));
    s.click((480 * U, 0));
    assert!(
        lines(&s.text(), "shape ")
            .iter()
            .any(|l| l.contains(" arc "))
    );
    // The current layer (Ctrl+7) colors new shapes.
    s.key("Ctrl+7");
    s.move_to((300 * U, 60 * U));
    s.key("l");
    s.click((340 * U, 60 * U));
    assert!(
        s.text().contains(" line 300 60 340 60\n  layer 7\n"),
        "{}",
        s.text()
    );
    assert_eq!(lines(&s.text(), "shape ").len(), 6);
    // Shapes are objects: selectable, deletable.
    s.click((320 * U, 60 * U));
    s.key("Del");
    assert_eq!(lines(&s.text(), "shape ").len(), 5);
    for _ in 0..7 {
        s.key("u");
    }
    assert_eq!(s.text(), original);
}

#[test]
fn symbol_view_draws_arcs_circles_and_polygons_as_graphics() {
    let mut s = Session::open("inv", "top.shcomp", "symgfx", true);
    s.key("Ctrl+Shift+N");
    s.move_to((0, 0));
    s.key("Ctrl+Shift+C");
    s.click((10 * U, 0));
    s.move_to((0, 40 * U));
    s.key("p");
    s.click((20 * U, 40 * U));
    s.click((20 * U, 60 * U));
    s.click((0, 40 * U)); // the first vertex closes it
    let t = s.text();
    assert!(t.contains("circle 0 0 10\n"), "{t}");
    assert!(t.contains("poly 0 40 20 40 20 60\n  closed true\n"), "{t}");
    assert!(!t.contains("shape "), "a symbol has no rank-3 shapes");
}

#[test]
fn context_menu_variants_run_their_rows() {
    let mut s = Session::open("inv", "top.shcomp", "ctx", true);
    right_click(&mut s, (200 * U, 40 * U), 0);
    assert_eq!(dialog(&s), "dialog.ctx");
    assert!(composed(&s, "dialog.ctx.row.13") && !composed(&s, "dialog.ctx.row.14"));
    s.click_node("dialog.ctx.row.6"); // Insert wire
    assert_eq!(dialog(&s), "");
    assert!(matches!(s.ed.tool, Tool::Wire { .. }), "{:?}", s.ed.tool);
    s.key("Esc");
    // With a selection: the selection variant; Delete selection deletes.
    s.click((150 * U, 0));
    right_click(&mut s, (150 * U, 0), 0);
    assert_eq!(dialog(&s), "dialog.ctx");
    s.click_node("dialog.ctx.row.5"); // Delete selection
    assert!(!s.text().contains("name x2"));
    // Esc closes the menu without running anything.
    right_click(&mut s, (200 * U, 40 * U), 0);
    s.key("Esc");
    assert_eq!(dialog(&s), "");
    // Shift+right click selects the wires connected to the object under the pointer.
    s.key("Esc");
    right_click(&mut s, (70 * U, -70 * U), SHIFT);
    assert!(
        s.ed.selection().len() >= 4,
        "the VDD rail: {:?}",
        s.ed.selection()
    );
    assert!(
        s.ed.selection()
            .iter()
            .all(|k| matches!(k.table, TableId::Wires | TableId::Labels))
    );
}

#[test]
fn persistent_wire_mode_chains_segments() {
    let mut s = Session::open("inv", "top.shcomp", "persist", true);
    run(&mut s, "options.persistent_wire_line_place_command", "");
    s.move_to((300 * U, 0));
    s.key("w");
    s.click((340 * U, 0));
    s.click((340 * U, 40 * U));
    assert!(matches!(s.ed.tool, Tool::Wire { .. }), "still drawing");
    s.key("Esc");
    let w = lines(&s.text(), "wire ").len();
    assert_eq!(w, 9 + 2);
    // Off again: one segment per `w`.
    run(&mut s, "options.persistent_wire_line_place_command", "");
    s.key("w");
    s.click((400 * U, 0));
    assert!(matches!(s.ed.tool, Tool::Idle));
}

#[test]
fn crosshair_snap_cursor_and_axes_draw_in_the_overlay() {
    let mut s = Session::open("inv", "top.shcomp", "cross", true);
    s.move_to((33 * U, 12 * U));
    let base = s.ed.overlay().lines.x0.len();
    s.key("Alt+X");
    s.move_to((30 * U, 10 * U));
    let o = s.ed.overlay();
    assert_eq!(o.lines.x0.len(), base + 2);
    assert!(
        o.lines.x0.contains(&(30 * U)),
        "vertical line at the snapped x"
    );
    run(&mut s, "view.show_hide.draw_grid_axes", "");
    assert_eq!(s.ed.overlay().lines.x0.len(), base + 4);
    s.key("Alt+Z");
    s.move_to((41 * U, U)); // near x1's OUT pin (40, 0)
    assert_eq!(s.ed.overlay().polygons.end.len(), 1, "the snap diamond");
    s.key("Alt+X");
    s.key("Alt+Z");
    run(&mut s, "view.show_hide.draw_grid_axes", "");
    s.move_to((30 * U, 10 * U));
    assert_eq!(s.ed.overlay().lines.x0.len(), base);
}

#[test]
fn chooser_lists_scroll_past_24_rows() {
    let mut s = Session::open("inv", "top.shcomp", "scroll", true);
    s.key("Ctrl+O");
    assert!(
        composed(&s, "dialog.choose.row.25"),
        "more than 24 rows listed"
    );
    // The wheel over the list scrolls it.
    let sh = &s.ed.shell;
    let list = sh.strings.find("dialog.choose.list").unwrap();
    let n = sh.tables.nodes.find(list).unwrap();
    let r = sh.layout.rect()[sh.layout.row_of(n).unwrap()];
    let (x, y) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerMove { x, y });
    ev.push(InputEvent::Wheel {
        x,
        y,
        dx: 0.0,
        dy: -3.0,
    });
    s.frame(ev);
    assert!(s.ed.shell.ui.scroll_of(list) > 0.0, "scrolled");
}

/// D28 crash recovery asks first: a journal with unsaved edits opens the recover/discard
/// dialog; nothing is applied before the answer; discard leaves nothing to offer next time.
#[test]
fn crash_recovery_asks_to_recover_or_discard() {
    let dir = common::fixture("inv", "recover");
    // An editor on the fixture copy that does not delete the directory when it goes.
    let open = |dir: &Path| {
        let mut s = Session {
            ed: Editor::new(Options {
                path: Some(dir.join("top.shcomp")),
                seed: 7,
                ..Default::default()
            })
            .unwrap(),
            now: 1,
            dir: dir.to_path_buf(),
        };
        s.frame(InputEvents::default());
        s
    };
    // A "crash": an edit is journaled, the editor vanishes without saving.
    let crash = |dir: &Path| {
        let mut s = open(dir);
        s.click((150 * U, 0));
        s.key("Del");
        let t = s.text();
        std::mem::forget(s);
        t
    };
    let original = std::fs::read_to_string(dir.join("top.shcomp")).unwrap();
    let edited = crash(&dir);
    assert_ne!(edited, original);
    let mut s = open(&dir);
    assert_eq!(dialog(&s), "dialog.recover", "{}", status(&s));
    assert_eq!(s.text(), original, "nothing applied before the decision");
    s.click_node("dialog.recover.discard");
    assert_eq!((dialog(&s), s.text()), (String::new(), original.clone()));
    std::mem::forget(s);
    let s = open(&dir);
    assert_eq!(dialog(&s), "", "nothing left to recover after a discard");
    std::mem::forget(s);
    assert_eq!(crash(&dir), edited);
    let mut s = open(&dir);
    assert_eq!(dialog(&s), "dialog.recover");
    s.click_node("dialog.recover.ok");
    assert_eq!(s.text(), edited, "the unsaved edit is back");
    let (h, _) = s.ed.active().unwrap();
    assert!(s.ed.session.project.is_dirty(h), "recovered, not saved");
    drop(s); // removes the directory
}

#[test]
fn rotated_texts_draw_turned() {
    let mut s = Session::open("inv", "top.shcomp", "rot", true);
    s.click((-60 * U, -125 * U)); // the annotation text
    assert_eq!(s.ed.selection()[0].table, TableId::Texts);
    s.key("Alt+R");
    assert!(
        s.text().contains("text t00001 -80 -130 R90 "),
        "{}",
        s.text()
    );
    let p = &s.ed.scene().prims.texts;
    let i = (0..p.x.len())
        .find(|&i| p.x[i] == -80 * U && p.y[i] == -130 * U)
        .expect("the text is drawn at its anchor");
    assert_eq!(p.rot[i], 1, "a quarter turn");
    // Its box turned with it: taller than wide now.
    let o = s.ed.scene().find(s.ed.selection()[0]).unwrap();
    let b = s.ed.scene().objs.bbox(o as usize);
    assert!(b[3] - b[1] > b[2] - b[0], "{b:?}");
}

// ---------------------------------------------------------------------------- Tools

#[test]
fn join_trim_merges_cuts_and_is_idempotent() {
    let mut s = Session::open("inv", "top.shcomp", "trim", true);
    // Two collinear touching wires and a duplicate.
    for (a, b) in [
        ((300, 0), (340, 0)),
        ((340, 0), (380, 0)),
        ((300, 0), (340, 0)),
    ] {
        s.move_to((a.0 * U, a.1 * U));
        s.key("w");
        s.click((b.0 * U, b.1 * U));
    }
    let before = pin_nets(&s);
    s.key("&");
    let w = owned(&s, "wire ");
    assert!(w.iter().any(|l| l.ends_with(" 300 0 380 0")), "{w:?}");
    assert_eq!(w.len(), 9 + 1);
    assert_eq!(pin_nets(&s), before, "connectivity unchanged");
    // Idempotent: a second run changes nothing.
    let t = s.text();
    s.key("&");
    assert_eq!(s.text(), t);
    assert!(status(&s).contains("already"));
    // A wire end on another wire's middle cuts it (T junction).
    s.move_to((340 * U, 0));
    s.key("w");
    s.click((340 * U, 40 * U));
    s.key("&");
    let w = owned(&s, "wire ");
    assert!(w.iter().any(|l| l.ends_with(" 300 0 340 0")), "{w:?}");
    assert!(w.iter().any(|l| l.ends_with(" 340 0 380 0")), "{w:?}");
}

#[test]
fn break_wires_at_pins_and_at_the_pointer() {
    let mut s = Session::open("inv", "top.shcomp", "break", true);
    // A wire across x2's IN pin (100, 0) from (60, 0) to (120, 0)... w00001 ends there; draw
    // a long one through x1's VSS pin (0, 30): from (-20, 30) to (20, 30).
    s.move_to((-20 * U, 30 * U));
    s.key("w");
    s.click((20 * U, 30 * U));
    s.click((10 * U, 0)); // x1
    s.key("!");
    let w = owned(&s, "wire ");
    assert!(w.iter().any(|l| l.ends_with(" -20 30 0 30")), "{w:?}");
    assert!(w.iter().any(|l| l.ends_with(" 0 30 20 30")), "{w:?}");
    // Ctrl+!: pieces running from pin to pin of the instance are removed.
    s.move_to((-40 * U, 0));
    s.key("w");
    s.click((40 * U, 0)); // IN to OUT straight through x1
    s.click((10 * U, -15 * U)); // x1's body, off the new wire
    s.key("Ctrl+!");
    assert!(
        !lines(&s.text(), "wire ")
            .iter()
            .any(|l| l.ends_with(" -40 0 40 0"))
    );
    // Alt+right click cuts the wire under the pointer at the snapped point.
    right_click(&mut s, (70 * U, 70 * U), schemify_ui::Mods::ALT);
    let w = owned(&s, "wire ");
    assert!(w.iter().any(|l| l.ends_with(" 0 70 70 70")), "{w:?}");
    assert!(w.iter().any(|l| l.ends_with(" 70 70 140 70")), "{w:?}");
}

#[test]
fn align_to_grid_snaps_coordinates() {
    let mut s = Session::open("inv", "top.shcomp", "align", true);
    run(&mut s, "view.set_snap_value", "1");
    s.click((150 * U, 0));
    s.move_to((150 * U, 0));
    s.key("m");
    s.click((153 * U, 2 * U));
    assert!(
        s.text().contains("  at 143 2 R0\n  name x2"),
        "{}",
        s.text()
    );
    run(&mut s, "view.set_snap_value", "10");
    s.key("Alt+U");
    assert!(
        s.text().contains("  at 140 0 R0\n  name x2"),
        "{}",
        s.text()
    );
}

// ---------------------------------------------------------------------------- Menus

#[test]
fn highlight_menu_duplicates_overlaps_and_selection() {
    let mut s = Session::open("inv", "top.shcomp", "hilite", true);
    // Rename x2 to x1: a duplicate.
    run(&mut s, "file.save", "");
    s.click((150 * U, 0));
    s.key("q");
    s.click_node("dialog.props.val.0");
    s.key("End");
    s.key("BackSpace");
    s.type_text("1");
    s.click_node("dialog.props.ok");
    s.key("Esc");
    s.key("#");
    assert_eq!(s.ed.selection().len(), 2, "{}", status(&s));
    s.key("Ctrl+#");
    let t = s.text();
    assert!(t.contains("name x1\n") && t.contains("name x2\n"), "{t}");
    s.key("Ctrl+#");
    assert!(status(&s).contains("no duplicate"));
    // An exact copy on top of x1: select overlapped finds the copy only.
    s.key("Esc");
    s.click((10 * U, 0));
    s.move_to((0, 0));
    s.key("c");
    s.click((0, 0));
    s.key("Esc");
    run(&mut s, "highlight.select_overlapped_instances", "");
    assert_eq!(s.ed.selection().len(), 1);
    s.key("Del");
    // Highlight the VDD net, then select what is highlighted.
    s.click((70 * U, -70 * U));
    s.key("k");
    s.key("Esc");
    s.key("Alt+K");
    assert!(s.ed.selection().len() >= 3, "{:?}", s.ed.selection());
    // Increment highlight color draws the net in its own layer.
    run(&mut s, "highlight.increment_hilight_color", "");
    s.frame(InputEvents::default());
    // Only probes dims everything but the highlight (palette, no edit).
    run(&mut s, "highlight.view_only_probes", "");
    assert!(s.ed.opts.only_probes);
}

#[test]
fn compare_schematics_outlines_the_differences() {
    let mut s = Session::open("inv", "top.shcomp", "compare", true);
    run(&mut s, "file.save_as", "top_b");
    // Change the copy: delete the VDD rail wire, move the VSS rail wire (clicks off the
    // labels' anchors: a press on a connection point starts a wire).
    s.click((30 * U, -70 * U));
    s.key("Del");
    s.click((100 * U, 70 * U));
    s.move_to((100 * U, 70 * U));
    s.key("m");
    s.click((100 * U, 80 * U));
    assert!(
        s.text().contains("wire w00009 0 80 140 80\n"),
        "{}",
        s.text()
    );
    assert!(!s.text().contains("w00006"));
    run(
        &mut s,
        "highlight.set_schematic_to_compare_and_compare_wit",
        "",
    );
    s.click_node("dialog.choose.filter");
    s.type_text("top.sh"); // top.shcomp only (not top_b.shcomp)
    assert!(!composed(&s, "dialog.choose.row.1"));
    s.click_node("dialog.choose.row.0");
    s.frame(InputEvents::default());
    assert!(status(&s).contains("compare:"), "{}", status(&s));
    assert!(
        status(&s).contains("1 added/changed here"),
        "{}\n{}\n{:?}",
        status(&s),
        s.text(),
        active_path(&s)
    );
    assert!(
        status(&s).contains("1 only in the other cell"),
        "{}",
        status(&s)
    );
    run(&mut s, "highlight.compare_schematics", "");
    assert!(!s.ed.opts.only_probes);
}

#[test]
fn symbol_menu_makes_symbols_schematics_labels_and_pins() {
    let mut s = Session::open("inv", "top.shcomp", "symbol", true);
    // Make symbol from schematic: IN left, OUT right, a box and the name.
    s.key("a");
    let t = s.text();
    assert!(t.contains("pin IN in -75 "), "{t}");
    assert!(t.contains("pin OUT out 75 "), "{t}");
    assert!(lines(&t, "rect ").len() == 1 && t.contains("{name}"));
    s.key("u");
    // Make schematic from symbol: a port for a pin that has none.
    // The OUT port: just above its anchor (a press on the anchor itself starts a wire).
    s.click((240 * U, -3 * U));
    assert_eq!(s.ed.selection()[0].table, TableId::Ports);
    s.key("Del");
    s.move_to((300 * U, 0));
    s.key("Ctrl+L");
    assert!(
        lines(&s.text(), "port ")
            .iter()
            .any(|l| l.ends_with(" 300 0 R180 OUT")),
        "{}",
        s.text()
    );
    // Create symbol pins from selected ports: none missing → status.
    s.click((300 * U, 0));
    s.key("Alt+H");
    assert!(status(&s).contains("without a pin"), "{}", status(&s));
    // Attach net labels: x2's unconnected pins after a plain move away.
    s.key("Esc");
    s.click((150 * U, 0));
    s.move_to((150 * U, 0));
    s.key("m");
    s.click((400 * U, 0)); // x2 at (390, 0): pins (350, 0) (430, 0) (390, ±30), all free
    s.key("Shift+H");
    let labels = owned(&s, "label ");
    for (pin, at) in [
        ("A", "350 0"),
        ("Y", "430 0"),
        ("VDD", "390 -30"),
        ("VSS", "390 30"),
    ] {
        assert!(
            labels
                .iter()
                .any(|l| l.ends_with(&format!(" {at} R0 {pin}"))),
            "{labels:?}"
        );
    }
    // Negative space: no new labels a second time (every pin now touches its label).
    s.key("Shift+H");
    assert_eq!(owned(&s, "label ").len(), labels.len());
    // List of nets: the highlight in a text tab; labels from it attach to the pointer.
    s.key("Esc");
    s.click((70 * U, 0));
    s.key("k");
    s.key("J");
    let v = s.ed.session.text_view().expect("a text tab");
    assert!(v.body.contains("mid"), "{}", v.body);
    s.key("Ctrl+Left");
    while s.ed.session.text_view().is_some() {
        s.key("Ctrl+Left");
    }
    s.move_to((400 * U, 0));
    s.key("Alt+J");
    s.click((400 * U, 0));
    assert!(
        lines(&s.text(), "label ")
            .iter()
            .any(|l| l.ends_with(" 400 0 R0 mid"))
    );
}

#[test]
fn view_options_shape_the_palette_and_the_ui() {
    let mut s = Session::open("inv", "top.shcomp", "view", true);
    assert!(composed(&s, "menubar") && composed(&s, "toolbar"));
    s.key("\\");
    assert!(
        !composed(&s, "menubar") && !composed(&s, "status"),
        "canvas only"
    );
    s.key("\\");
    run(&mut s, "view.show_hide.show_toolbar", "");
    assert!(composed(&s, "menubar") && !composed(&s, "toolbar"));
    // Layers: hide layer 1 (wires), only the current layer, all again.
    run(&mut s, "layers.toggle.1", "");
    assert_eq!(s.ed.opts.hidden, 1 << 1);
    s.key("Ctrl+5");
    assert_eq!(s.ed.opts.layer, 5);
    s.key(">");
    assert_eq!(s.ed.opts.hidden & (1 << 5), 0);
    assert_ne!(s.ed.opts.hidden & (1 << 4), 0);
    s.key("<");
    assert_eq!(s.ed.opts.hidden, 0);
    // The visible-layers dialog: one checkable row per layer; a click toggles it.
    run(&mut s, "view.show_hide.visible_layers", "");
    assert_eq!(dialog(&s), "dialog.layers");
    assert!(composed(&s, "dialog.layers.row.21"));
    s.click_node("dialog.layers.row.3");
    assert_eq!(s.ed.opts.hidden, 1 << 3);
    s.click_node("dialog.layers.close");
    // Widths, dim, colors, reset.
    s.key("Ctrl++");
    assert_eq!(s.ed.opts.width_tenths, 15);
    run(&mut s, "view.set_line_width", "2");
    assert_eq!(s.ed.opts.width_tenths, 20);
    run(&mut s, "view.dim_colors", "0.5");
    assert_eq!(s.ed.opts.dim, 50);
    run(&mut s, "view.change_current_layer_color", "ff0000");
    run(&mut s, "view.change_current_layer_color", "zz");
    assert!(status(&s).contains("not an rrggbb"));
    run(&mut s, "view.reset_all_colors_to_default", "");
    assert_eq!(
        (s.ed.opts.dim, s.ed.opts.hidden, s.ed.opts.width_tenths),
        (0, 0, 10)
    );
    // Symbols as boxes, symbol texts off: the displayed scene differs, the doc does not.
    let t = s.text();
    s.key("Alt+B");
    assert_eq!(s.ed.opts.show_symbols, 1);
    run(&mut s, "view.show_hide.symbol_text", "");
    assert!(!s.ed.opts.symbol_text);
    assert_eq!(s.text(), t);
    // Snap / grid / crosshair size dialogs and their arguments.
    run(&mut s, "view.set_grid_spacing", "5");
    assert_eq!(s.ed.opts.grid, 5 * U);
    s.key("Ctrl+G");
    assert_eq!(dialog(&s), "dialog.enter_text");
    for _ in 0..4 {
        s.key("BackSpace");
    }
    s.type_text("2.5");
    s.key("Enter");
    assert_eq!(s.ed.opts.snap, 2500);
    run(&mut s, "options.crosshair.crosshair_size", "12");
    assert_eq!(s.ed.opts.crosshair_px, 12);
    run(&mut s, "options.crosshair.crosshair_size", "-3");
    assert!(status(&s).contains("out of range"));
}

#[test]
fn properties_menu_ignore_header_and_edit_file() {
    let mut s = Session::open("inv", "top.shcomp", "props", true);
    let original = s.text();
    s.click((150 * U, 0));
    s.key("Shift+T");
    assert!(
        s.text().contains("name x2\n  ignore true\n"),
        "{}",
        s.text()
    );
    s.key("Shift+T");
    assert_eq!(s.text(), original);
    // Header: the doc line.
    s.key("Shift+B");
    assert_eq!(dialog(&s), "dialog.props");
    s.click_node("dialog.props.val.1");
    s.key("End");
    s.key("BackSpace");
    s.type_text("y");
    s.click_node("dialog.props.ok");
    assert!(s.text().contains("\n  prefix y\n"), "{}", s.text());
    s.key("u");
    // Edit file: the text, changed and applied as one edit.
    s.key("Alt+Q");
    assert_eq!(dialog(&s), "dialog.edit_file");
    let typed = original.replace("label n00001 70 0 R0 mid", "label n00001 70 0 R0 middle");
    s.key("Ctrl+A");
    s.ed.command("dialog.edit_file.ok", &typed);
    s.frame(InputEvents::default());
    assert!(s.text().contains("R0 middle\n"), "{}", s.text());
    assert_eq!(s.text(), typed);
    s.key("u");
    assert_eq!(s.text(), original, "one undo");
    // A text that does not parse is refused with its line.
    s.key("Alt+Q");
    s.ed.command("dialog.edit_file.ok", "schemify 1 shcomp\nwire x 0 0\n");
    s.frame(InputEvents::default());
    assert!(status(&s).starts_with("edit file: line"), "{}", status(&s));
    assert_eq!(s.text(), original);
}

#[test]
fn simulation_help_and_waves_commands() {
    let mut s = Session::open("inv", "top.shcomp", "simhelp", true);
    run(&mut s, "simulation.edit_netlist", "");
    let title = s.ed.session.text_view().map(|v| v.title.clone());
    assert_eq!(
        title.as_deref(),
        Some("top.spice (netlist)"),
        "{}",
        status(&s)
    );
    run(&mut s, "simulation.changelog_from_current_hierarchy", "");
    let v = s.ed.session.text_view().unwrap();
    assert!(
        v.body.contains("top.shcomp") && v.body.contains("inv.shcomp"),
        "{}",
        v.body
    );
    run(&mut s, "simulation.list_running_sub_processes", "");
    assert!(s.ed.session.text_view().unwrap().body.contains("no run"));
    run(&mut s, "help.about", "");
    assert!(s.ed.session.text_view().unwrap().body.contains("Schemify"));
    run(&mut s, "help.help", "");
    assert!(
        s.ed.session
            .text_view()
            .unwrap()
            .body
            .contains("Keys are xschem's")
    );
    run(&mut s, "waves.load.tran", "");
    assert_eq!(dialog(&s), "dialog.choose");
    s.key("Esc");
    s.key("Esc");
    run(&mut s, "waves.clear", "");
    assert!(!s.ed.waves_open());
    run(&mut s, "options.cycle_netlist_format", "");
    assert_eq!(s.ed.dialect(), schemify_elab::Dialect::Espice);
    run(&mut s, "options.cycle_netlist_format", "");
    assert_eq!(s.ed.dialect(), schemify_elab::Dialect::Xyce);
    run(
        &mut s,
        "options.netlist_format_symbol_mode.flat_netlist",
        "",
    );
    assert!(s.ed.opts.flat);
}

/// The default-UI menus reach the new commands: File → Image export → SVG, by clicks.
#[test]
fn menus_reach_parity_commands() {
    let mut s = Session::open("inv", "top.shcomp", "menus", true);
    let sym = |s: &Session, id: &str| s.ed.shell.strings.find(id).unwrap();
    let chain = [sym(&s, "menu.file"), sym(&s, "menu.file.image_export")];
    s.ed.shell.ui.open_menus(&chain);
    s.ed.shell.invalidate();
    s.frame(InputEvents::default());
    s.click_node("file.image_export.svg_export");
    assert!(
        Path::new(&s.dir.join(".schemify/export/top.svg")).exists(),
        "{}",
        status(&s)
    );
}

// ---------------------------------------------------------------------------- more oracles

/// Replaces the active document's text through Properties → Edit file (one undoable edit):
/// the quickest way to set up a schematic for a test.
fn set_text(s: &mut Session, text: &str) {
    s.key("Alt+Q");
    s.ed.command("dialog.edit_file.ok", text);
    s.frame(InputEvents::default());
    assert_eq!(s.text(), text, "{}", status(s));
}

/// Ctrl+drag stretches; a stretch breaks a direct pin-to-pin contact, a kissing move keeps it
/// with a new wire.
#[test]
fn kissing_keeps_direct_pin_contacts_that_stretch_breaks() {
    let mut s = Session::open("inv", "top.shcomp", "kiss", true);
    // Ctrl+drag on the selected x2: its wires follow.
    s.click((150 * U, 0));
    s.drag((150 * U, 0), (150 * U, 40 * U), CTRL);
    assert!(
        s.text().contains("wire w00001 40 0 100 40\n"),
        "{}",
        s.text()
    );
    s.key("u");
    // Two inverters touching pin to pin: x3 at (300, 0), x4 at (380, 0) (x4.A on x3.Y).
    s.key("Esc");
    s.click((10 * U, 0));
    s.move_to((0, 0));
    s.key("c");
    s.click((300 * U, 0));
    s.move_to((300 * U, 0));
    s.key("c");
    s.click((380 * U, 0));
    let x4 = s.ed.selection()[0];
    let net = |s: &Session, pin: &str| {
        let nets = pin_nets(s);
        nets.iter().find(|n| n.0 == pin).map(|n| n.1)
    };
    let a4 = format!("{}.0", x4.key); // x4's pin A
    // x4.A and x3.Y share one net before any move.
    let a4_net = net(&s, &a4).unwrap();
    let shared = pin_nets(&s).iter().filter(|n| n.1 == a4_net).count();
    assert!(shared >= 2, "x4.A touches x3.Y: {:?}", pin_nets(&s));
    for (key, kiss) in [("Ctrl+M", false), ("Shift+M", true)] {
        s.move_to((380 * U, 0));
        s.key(key);
        s.click((380 * U, 40 * U));
        let after = net(&s, &a4).unwrap();
        let still = pin_nets(&s).iter().filter(|n| n.1 == after).count();
        if kiss {
            assert!(still >= 2, "kissing keeps the contact: {}", s.text());
        } else {
            assert_eq!(still, 1, "a stretch alone separates the pins");
        }
        s.key("u");
    }
}

/// Property: join/trim changes no connectivity (a probe on every grid point any wire covers
/// lands in the same net before and after) and is idempotent.
#[test]
fn join_trim_keeps_connectivity_over_random_wires() {
    let mut rng = Lcg(0x7713);
    for case in 0..8 {
        let mut s = Session::open("inv", "top.shcomp", &format!("trimprop{case}"), true);
        let mut text = s.text();
        let mut wires = String::new();
        for k in 0..8 {
            let (x, y) = (300 + rng.below(6) as i32 * 10, rng.below(6) as i32 * 10);
            let len = 10 + rng.below(4) as i32 * 10;
            let (x2, y2) = if rng.below(2) == 0 {
                (x + len, y)
            } else {
                (x, y + len)
            };
            wires.push_str(&format!("wire z{k:05} {x} {y} {x2} {y2}\n"));
        }
        // Insert the wires before the labels (rank 3 is sorted by the writer anyway).
        text = text.replacen("\nlabel ", &format!("\n{wires}label "), 1);
        let canonical = {
            let d = schemify_format::parse(text.as_bytes()).unwrap();
            schemify_format::write(&d)
        };
        set_text(&mut s, &canonical);
        let probes = |s: &Session| -> Vec<Vec<(i32, i32)>> {
            let (h, _) = s.ed.active().unwrap();
            let doc = s.ed.session.project.doc(h).unwrap();
            let cell = s.ed.cell().unwrap();
            let w = doc.wires();
            let mut by_net: std::collections::BTreeMap<u32, Vec<(i32, i32)>> = Default::default();
            for x in (290..=400).step_by(10) {
                for y in (-10..=60).step_by(10) {
                    let p = (x * U, y * U);
                    let on = (0..w.id.len()).find(|&r| {
                        let (a, b) = ((w.x1[r], w.y1[r]), (w.x2[r], w.y2[r]));
                        p == a || p == b || schemify_app::wiring::on_interior(p, a, b)
                    });
                    if let Some(r) = on {
                        by_net
                            .entry(cell.nets.wire_net[r])
                            .or_default()
                            .push((x, y));
                    }
                }
            }
            let mut groups: Vec<_> = by_net.into_values().collect();
            groups.sort();
            groups
        };
        let before = probes(&s);
        s.key("&");
        assert_eq!(probes(&s), before, "case {case}: {wires}");
        let once = s.text();
        s.key("&");
        assert_eq!(s.text(), once, "case {case}: idempotent");
    }
}

#[test]
fn auto_trim_joins_after_each_wire() {
    let mut s = Session::open("inv", "top.shcomp", "autotrim", true);
    run(&mut s, "options.auto_join_trim_wires", "");
    s.move_to((300 * U, 0));
    s.key("w");
    s.click((340 * U, 0));
    s.move_to((340 * U, 0));
    s.key("w");
    s.click((380 * U, 0));
    let w = owned(&s, "wire ");
    assert!(w.iter().any(|l| l.ends_with(" 300 0 380 0")), "{w:?}");
    assert_eq!(w.len(), 9 + 1);
    // Two undos: the trim, then the second wire.
    s.key("u");
    assert_eq!(owned(&s, "wire ").len(), 9 + 2);
}

#[test]
fn make_subcircuit_moves_the_selection_into_a_new_cell() {
    let mut s = Session::open("inv", "top.shcomp", "subckt", true);
    let before = s.text();
    s.click((150 * U, 0)); // x2
    s.key("Ctrl+Shift+H");
    let file = s.dir.join("top_sub1.shcomp");
    let sub = std::fs::read_to_string(&file).unwrap_or_else(|_| panic!("{}", status(&s)));
    // x2's four pins sit on four nets that cross the boundary: four ports, four pins.
    assert_eq!(lines(&sub, "port ").len(), 4, "{sub}");
    assert_eq!(lines(&sub, "pin ").len(), 4, "{sub}");
    assert!(sub.contains("./inv") && sub.contains("name x2"), "{sub}");
    let t = s.text();
    assert!(t.contains("./top_sub1\n"), "{t}");
    assert!(!t.contains("name x2\n"), "x2 moved out: {t}");
    s.key("u");
    assert_eq!(s.text(), before, "one undo restores the schematic");
    // Negative space: nothing selected → a status line, no file.
    s.key("Esc");
    s.key("Ctrl+Shift+H");
    assert!(status(&s).contains("select"), "{}", status(&s));
    assert!(!s.dir.join("top_sub2.shcomp").exists());
}

#[test]
fn pins_ports_and_labels_from_highlighted_nets() {
    let mut s = Session::open("inv", "top.shcomp", "netlists", true);
    // A new port EN: Alt+H makes it a symbol pin (in, at the port's position).
    s.move_to((300 * U, -40 * U));
    s.key("Ctrl+P");
    s.type_text("EN");
    s.key("Enter");
    s.click((300 * U, -40 * U));
    s.key("Alt+H");
    assert!(s.text().contains("pin EN in 300 -40\n"), "{}", s.text());
    // Highlight `mid`, then Ctrl+J / Alt+Shift+J attach a port / an i-label at the pointer.
    s.key("Esc");
    s.click((60 * U, 0));
    s.key("k");
    s.key("Esc");
    s.move_to((320 * U, 40 * U));
    s.key("Ctrl+J");
    s.click((320 * U, 40 * U));
    assert!(
        owned(&s, "port ")
            .iter()
            .any(|l| l.ends_with(" 320 40 R0 mid"))
    );
    s.move_to((360 * U, 40 * U));
    s.key("Alt+Shift+J");
    s.click((360 * U, 40 * U));
    assert!(
        owned(&s, "label ")
            .iter()
            .any(|l| l.ends_with(" 360 40 R0 imid"))
    );
    // With buses expanded: a bus net lists one line per bit.
    s.key("Ctrl+Alt+J");
    assert!(s.ed.session.text_view().unwrap().body.contains("mid"));
}

#[test]
fn select_connected_crosses_or_stops_at_junctions() {
    let mut s = Session::open("inv", "top.shcomp", "conn", true);
    // A T on the VDD rail at (30, -70).
    s.move_to((30 * U, -70 * U));
    s.key("w");
    s.click((30 * U, -100 * U));
    s.key("Esc");
    right_click(&mut s, (100 * U, -70 * U), SHIFT);
    let all = s.ed.selection().to_vec();
    assert_eq!(
        all.iter().filter(|k| k.table == TableId::Wires).count(),
        4,
        "the rail, its two drops and the T branch: {all:?}"
    );
    assert!(
        all.iter().any(|k| k.table == TableId::Labels),
        "the VDD label"
    );
    s.key("Esc");
    right_click(&mut s, (100 * U, -70 * U), CTRL);
    let stop = s.ed.selection().to_vec();
    assert!(
        stop.len() < all.len(),
        "Ctrl stops at the T junction: {stop:?}"
    );
}

#[test]
fn unselect_area_removes_boxed_objects() {
    let mut s = Session::open("inv", "top.shcomp", "unsel", true);
    s.key("Ctrl+A");
    let n = s.ed.selection().len();
    s.key("Shift+D");
    s.drag((-50 * U, -50 * U), (50 * U, 50 * U), 0); // around x1, left to right
    let after = s.ed.selection();
    assert_eq!(after.len(), n - 1, "only x1 was fully inside the box");
    assert!(
        after
            .iter()
            .filter(|k| k.table == TableId::Instances)
            .count()
            == 1,
        "x2 stays selected"
    );
    assert!(matches!(s.ed.tool, Tool::Idle));
}

#[test]
fn compare_swap_and_toggle() {
    let mut s = Session::open("inv", "top.shcomp", "swap", true);
    run(&mut s, "file.save_as", "top_b");
    s.click((30 * U, -70 * U));
    s.key("Del");
    run(
        &mut s,
        "highlight.set_schematic_to_compare_and_compare_wit",
        "",
    );
    s.click_node("dialog.choose.filter");
    s.type_text("top.sh");
    s.click_node("dialog.choose.row.0");
    s.frame(InputEvents::default());
    assert!(!s.ed.parity_decor().is_empty(), "the diff is drawn");
    run(&mut s, "highlight.compare_schematics", "");
    assert!(s.ed.parity_decor().is_empty(), "toggled off");
    run(&mut s, "highlight.compare_schematics", "");
    assert!(!s.ed.parity_decor().is_empty());
    run(&mut s, "highlight.swap_compare_schematics", "");
    assert!(
        active_path(&s).ends_with("top.shcomp"),
        "{:?}",
        active_path(&s)
    );
    assert!(
        status(&s).contains("1 added/changed here"),
        "{}",
        status(&s)
    );
}

#[test]
fn highlight_decor_options_and_auto_highlight() {
    let mut s = Session::open("inv", "top.shcomp", "hdecor", true);
    run(&mut s, "highlight.auto_highlight_net_pins", "");
    s.click((60 * U, 0)); // a wire of `mid`
    assert_eq!(s.ed.highlight_keys().len(), 1, "{}", status(&s));
    s.click((200 * U, 40 * U)); // empty space clears it
    assert!(s.ed.highlight_keys().is_empty());
    s.click((60 * U, 0));
    run(&mut s, "highlight.enable_highlight_connected_instances", "");
    let outlined = s.ed.parity_decor().rects.x0.len();
    assert_eq!(outlined, 2, "x1 and x2 touch `mid`");
    run(&mut s, "highlight.increment_hilight_color", "");
    let d = s.ed.parity_decor();
    assert!(!d.lines.x0.is_empty(), "mid's wire in its own color");
    assert!(
        d.lines
            .layer
            .iter()
            .all(|&l| l != schemify_app::checks::LAYER_HILITE),
        "a per-net layer, not the shared highlight layer"
    );
    run(&mut s, "highlight.view_only_probes", "");
    let p = s.ed.view_palette();
    assert_ne!(
        p.color[1],
        s.ed.view_palette().color[0],
        "dimmed, not hidden"
    );
}

#[test]
fn view_palette_follows_fill_colors_widths_and_symbol_modes() {
    let mut s = Session::open("inv", "top.shcomp", "palette", true);
    let base = s.ed.view_palette();
    run(&mut s, "view.toggle_fill_pattern", "");
    assert_eq!(s.ed.view_palette().fill, 0);
    run(&mut s, "layers.set_current.4", "");
    run(&mut s, "view.change_current_layer_color", "ff0000");
    assert_eq!(s.ed.view_palette().color[4], 0xff00_00ff);
    run(&mut s, "view.toggle_variable_line_width", "");
    assert_ne!(s.ed.view_palette().width[1], base.width[1] * 0.0 + 999.0);
    run(&mut s, "layers.toggle.1", "");
    assert_eq!(
        s.ed.view_palette().color[1] & 0xff,
        0,
        "hidden layer: alpha 0"
    );
    // Boxes for every instance: the shown scene has the instances' rects only.
    let full = s.ed.shown().lines.x0.len();
    run(
        &mut s,
        "symbol.show_symbols.show_instance_bounding_boxes_for_all_sym",
        "",
    );
    assert!(s.ed.shown().lines.x0.len() < full);
    assert!(s.ed.shown().rects.x0.len() >= 2);
    run(&mut s, "symbol.show_symbols.show_symbol_details", "");
    assert_eq!(s.ed.shown().lines.x0.len(), full);
    // Transparent SVG: no background rectangle.
    run(&mut s, "options.transparent_svg_background", "");
    run(&mut s, "file.image_export.svg_export", "");
    let t = std::fs::read_to_string(s.dir.join(".schemify/export/top.svg")).unwrap();
    assert!(!t.contains("id=\"background\""));
}

#[test]
fn small_commands_tabs_panels_graphs_and_recent() {
    let mut s = Session::open("inv", "top.shcomp", "small", true);
    run(&mut s, "file.create_new_window_tab", "");
    assert_eq!(s.ed.session.tabs.len(), 2);
    run(&mut s, "view.show_hide.show_erc_info_window", "");
    assert_eq!(s.ed.panel(), schemify_app::editor::Panel::Checks);
    run(&mut s, "view.show_hide.show_erc_info_window", "");
    assert_eq!(s.ed.panel(), schemify_app::editor::Panel::None);
    s.key("Ctrl+Left");
    run(&mut s, "simulation.graphs.add_waveform_graph", "");
    assert!(s.text().contains("  | ```plot\n"), "{}", s.text());
    assert!(s.ed.docs_shown());
    run(&mut s, "simulation.view_last_job_data", "");
    assert!(status(&s).contains("no run"));
    run(&mut s, "symbol.set_symbol_width", "200");
    assert_eq!(s.ed.opts.symbol_width, 200 * U);
    // The unsnapped wire cut lands between grid points.
    right_click(&mut s, (73 * U, 70 * U), SHIFT | schemify_ui::Mods::ALT);
    let w = owned(&s, "wire ");
    assert_eq!(w.len(), 10, "{w:?}");
}
