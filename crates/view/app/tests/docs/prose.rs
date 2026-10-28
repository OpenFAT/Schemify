//! Prose blocks: editing in a text area, adding, moving and deleting blocks, scrolling, images
//! from inside the project, the preview and the source toggle, and the page goldens.

use super::{num, open, page_nodes, row};
use crate::common::{self, Session, lines};

#[test]
fn prose_blocks_edit_in_a_text_area_and_blocks_add_move_delete() {
    let mut s = open("prose");
    let (h, _) = s.ed.active().unwrap();
    let before = s.text();
    // Click the first text block: its editor replaces it, focused.
    s.click_node("docs.b.0.0");
    assert_eq!(s.ed.page().editing.as_ref().map(|e| e.0), Some(0));
    assert!(page_nodes(&s).contains(&"docs.b.0.edit".to_owned()));
    s.key("Ctrl+A");
    s.type_text("# Retitled");
    s.key("Enter");
    s.type_text("Body with $x^2$.");
    s.key("Ctrl+Enter");
    let t = s.text();
    assert!(
        t.contains("prose d00001\n  | # Retitled\n  | Body with $x^2$.\n"),
        "{t}"
    );
    assert!(s.ed.page().editing.is_none());
    s.key("u");
    assert_eq!(s.text(), before, "one undo per text edit");
    // Escape cancels an edit.
    s.click_node("docs.b.0.0");
    s.key("Ctrl+A");
    s.type_text("discarded");
    s.key("Esc");
    assert_eq!(s.text(), before);
    // + Var after the selected block, then type its expression.
    s.click_node("docs.add_var");
    let r = s.ed.page().sel.unwrap();
    assert_eq!(r, 1, "inserted after block 0");
    s.key("Ctrl+A");
    s.type_text("2*Id_tail");
    s.key("Enter");
    assert_eq!(num(&s, "v1"), 40e-6);
    assert!(lines(&s.text(), "var v1 ").contains(&"var v1 2*Id_tail"));
    // Move it down one block and delete it: one undo each.
    s.click_node(&format!("docs.b.{r}.name"));
    s.click_node("docs.down");
    let b = s.ed.session().project.doc(h).unwrap().doc_blocks();
    let st = s.ed.session().project.doc(h).unwrap().strings();
    assert_eq!(st.get(b.fields[2][0]), "v1");
    s.click_node("docs.delete");
    assert!(lines(&s.text(), "var v1").is_empty());
    for _ in 0..4 {
        s.key("u");
    }
    assert_eq!(s.text(), before, "add, set, move, delete: four undos");
    // + Text adds an editable block; Alt+D hides the page and gives the canvas the width.
    s.key("Alt+D");
    assert!(!s.ed.docs_shown());
    assert!(s.ed.canvas_rect().width() > 1200.0);
    s.key("Alt+D");
    assert!(s.ed.docs_shown());
}

#[test]
fn the_page_scrolls_and_hides_scrolled_out_rows_from_clicks() {
    let mut s = open("scroll");
    let sh = &s.ed.shell();
    let page = sh
        .tables()
        .nodes
        .find(sh.strings().find("docs.page").unwrap())
        .unwrap();
    let max = sh.layout().scroll_max(page).expect("laid out");
    assert!(max > 100.0, "the OTA page is taller than the window: {max}");
    let r = sh.layout().row_of(page).unwrap();
    let area = sh.layout().rect()[r];
    let top = |s: &Session| {
        let sh = &s.ed.shell();
        let n = sh
            .tables()
            .nodes
            .find(sh.strings().find("docs.b.0").unwrap())
            .unwrap();
        sh.layout().rect()[sh.layout().row_of(n).unwrap()].y
    };
    let y0 = top(&s);
    let mut ev = schemify_ui::InputEvents::default();
    ev.push(schemify_ui::InputEvent::Wheel {
        x: area.x + 20.0,
        y: area.y + 40.0,
        dx: 0.0,
        dy: -3.0,
    });
    s.frame(ev);
    let y1 = top(&s);
    assert!(
        (y0 - y1 - 3.0 * schemify_ui::UiState::WHEEL_PX).abs() < 0.5,
        "{y0} → {y1}"
    );
    // Far past the end: clamped to the content.
    for _ in 0..50 {
        let mut ev = schemify_ui::InputEvents::default();
        ev.push(schemify_ui::InputEvent::Wheel {
            x: area.x + 20.0,
            y: area.y + 40.0,
            dx: 0.0,
            dy: -5.0,
        });
        s.frame(ev);
    }
    let sh = &s.ed.shell();
    let off = y0 - top(&s);
    assert!((off - max).abs() < 0.5, "offset {off} max {max}");
    // A row scrolled above the pane is not clickable through the toolbar.
    let n = sh
        .tables()
        .nodes
        .find(sh.strings().find("docs.b.0").unwrap())
        .unwrap();
    let rr = sh.layout().rect()[sh.layout().row_of(n).unwrap()];
    assert!(rr.y + rr.h < area.y);
    assert!(
        sh.layout()
            .hit(rr.x + 5.0, rr.y + rr.h - 1.0, false)
            .is_none_or(|h| sh.layout().node()[h] != n)
    );
}

#[test]
fn golden_docs_page() {
    let mut s = open("golden");
    let px = s.render();
    common::check_golden("docs_page", px);
}

/// Prose images come from the project only: a PNG inside it is drawn at its pixel size; a path
/// leaving the project (the file exists there) and a file that is not a PNG are never decoded
/// and show their alt text.
#[test]
fn prose_images_load_from_inside_the_project_only() {
    let dir = common::fixture("ota", "images");
    let png = schemify_render::Pixmap::new(40, 20).into_png().unwrap();
    std::fs::create_dir_all(dir.join("img")).unwrap();
    std::fs::write(dir.join("img/dot.png"), &png).unwrap();
    std::fs::write(dir.join("img/fake.png"), b"not a png").unwrap();
    let outside = format!("schemify-outside-{}.png", std::process::id());
    let outside_file = dir.parent().unwrap().join(&outside);
    std::fs::write(&outside_file, &png).unwrap();
    let p = dir.join("ota.shcomp");
    let head = "  | # Five-transistor OTA, sized by gm/Id\n";
    let with = format!(
        "{head}  |\n  | ![dot](img/dot.png)\n  |\n  | ![out](../{outside})\n  |\n  | ![fake](img/fake.png)\n"
    );
    let text = std::fs::read_to_string(&p).unwrap().replace(head, &with);
    std::fs::write(&p, text).unwrap();
    let ed = schemify_editor::editor::Editor::new(schemify_editor::editor::Options {
        no_default_ui: false,
        path: Some(p),
        seed: 7,
        ..Default::default()
    })
    .expect("opens");
    let mut s = Session {
        ed,
        now: 1,
        dir,
        caches: Default::default(),
    };
    for _ in 0..3 {
        s.frame(Default::default());
    }
    let _ = std::fs::remove_file(&outside_file);
    let sh = &s.ed.shell();
    let t = &sh.text();
    let id = t
        .image_id("img/dot.png")
        .expect("a PNG inside the project loads");
    assert_eq!((t.image(id).width, t.image(id).height), (40, 20));
    let escaped = format!("../{outside}");
    assert_eq!(
        t.image_id(&escaped),
        None,
        "outside the project: never read"
    );
    assert_eq!(t.image_id("img/fake.png"), None, "not a PNG");
    let n = sh
        .tables()
        .nodes
        .find(sh.strings().find("docs.b.0.0").unwrap());
    let r = sh
        .layout()
        .row_of(n.unwrap())
        .expect("the first prose block is laid out");
    let list = t.prose_list(sh.layout().prose()[r]);
    assert_eq!(list.image_rect.len(), 1, "one image drawn, two alt texts");
    assert_eq!(list.image_rect[0][2..], [40.0, 20.0]);
}

/// Prop `p` of UI node `id`, `None` when the node is not composed.
fn prop_of(s: &Session, id: &str, p: schemify_ui::Prop) -> Option<String> {
    let sh = &s.ed.shell();
    let n = sh
        .strings()
        .find(id)
        .and_then(|i| sh.tables().nodes.find(i))?;
    Some(
        sh.strings()
            .get(sh.tables().nodes.prop(p)[n as usize])
            .to_owned(),
    )
}

/// `(text runs, formulas)` of prose node `id` as laid out this frame.
fn laid_out(s: &Session, id: &str) -> (usize, usize) {
    let sh = &s.ed.shell();
    let n = sh
        .tables()
        .nodes
        .find(sh.strings().find(id).expect("the UI names the node"))
        .unwrap_or_else(|| panic!("{id} composed"));
    let r = sh
        .layout()
        .row_of(n)
        .unwrap_or_else(|| panic!("{id} laid out"));
    let p = sh.layout().prose()[r];
    assert_ne!(p, u32::MAX, "{id} is prose");
    let list = sh.text().prose_list(p);
    (list.text_id.len(), list.math_id.len())
}

const PREVIEW: &str = "docs.b.0.preview";

#[test]
fn the_prose_editor_previews_what_is_typed_until_applied_or_cancelled() {
    use schemify_ui::Prop::{Label, Value};
    let mut s = open("preview");
    let before = s.text();
    assert_eq!(prop_of(&s, PREVIEW, Value), None, "no preview while closed");
    s.click_node("docs.b.0.0");
    // Opening previews the block's own source, its formulas rendered.
    let (h, _) = s.ed.active().unwrap();
    let src = schemify_editor::docs::prose_text(s.ed.session().project.doc(h).unwrap(), 0);
    assert_eq!(prop_of(&s, PREVIEW, Value).as_deref(), Some(src.trim_end()));
    assert!(
        laid_out(&s, PREVIEW).1 >= 3,
        "the fixture's formulas render"
    );
    assert_eq!(
        prop_of(&s, "docs.b.0.preview.cap", Label).as_deref(),
        Some("Preview")
    );
    // It follows every keystroke; the document is untouched.
    s.key("Ctrl+A");
    s.type_text("Gain $A_0$");
    assert_eq!(prop_of(&s, PREVIEW, Value).as_deref(), Some("Gain $A_0$"));
    assert_eq!(laid_out(&s, PREVIEW).1, 1);
    s.type_text(" and $x^2$");
    assert_eq!(laid_out(&s, PREVIEW).1, 2);
    s.key("Ctrl+A");
    s.type_text("plain words");
    assert_eq!(laid_out(&s, PREVIEW), (1, 0), "no math, one paragraph");
    s.key("Ctrl+A");
    s.key("Delete");
    let empty = prop_of(&s, PREVIEW, Value).unwrap();
    assert!(empty.contains("applying deletes"), "{empty}");
    assert_eq!(s.text(), before, "preview never edits the document");
    // Esc cancels: preview gone, document unchanged.
    s.key("Esc");
    assert_eq!(prop_of(&s, PREVIEW, Value), None);
    assert_eq!(s.text(), before);
    // Ctrl+Enter applies: preview gone, text written.
    s.click_node("docs.b.0.0");
    s.key("Ctrl+A");
    s.type_text("Applied $y$");
    s.key("Ctrl+Enter");
    assert_eq!(prop_of(&s, PREVIEW, Value), None);
    assert!(s.text().contains("  | Applied $y$\n"), "{}", s.text());
    // A click on the preview is a click away from the editor: it applies.
    s.click_node("docs.b.0.0");
    s.key("Ctrl+A");
    s.type_text("Clicked away");
    s.click_node(PREVIEW);
    assert!(s.ed.page().editing.is_none());
    assert_eq!(prop_of(&s, PREVIEW, Value), None);
    assert!(s.text().contains("  | Clicked away\n"));
}

#[test]
fn the_source_toggle_shows_prose_verbatim_per_tab() {
    use schemify_ui::Prop::{Checked, Label, Value};
    let mut s = open("source");
    s.ed.command("docs.edit", "0");
    s.frame(Default::default());
    s.key("Ctrl+A");
    s.type_text("Body with $x^2$ and **bold**.");
    s.key("Ctrl+Enter");
    let before = s.text();
    assert_eq!(laid_out(&s, "docs.b.0.0").1, 1, "rendered: one formula");
    assert_eq!(
        prop_of(&s, "docs.source", Label).as_deref(),
        Some("Rendered")
    );
    assert_eq!(prop_of(&s, "docs.source", Checked).as_deref(), Some("0"));
    let w = row(&s, "W_in");
    let var_before = prop_of(&s, &format!("docs.b.{w}.expr"), Value);
    assert!(var_before.is_some());
    // Source: the prose block shows its text literally, math and markup unrendered.
    s.click_node("docs.source");
    assert!(s.ed.docs_source());
    let v = prop_of(&s, "docs.b.0.0", Value).unwrap();
    assert!(v.contains("\nBody with $x^2$ and **bold**.\n"), "{v}");
    assert_eq!(
        laid_out(&s, "docs.b.0.0"),
        (1, 0),
        "one verbatim run, no math"
    );
    assert_eq!(prop_of(&s, "docs.source", Label).as_deref(), Some("Source"));
    assert_eq!(prop_of(&s, "docs.source", Checked).as_deref(), Some("1"));
    // Var blocks are unchanged: same field, no prose node.
    assert_eq!(prop_of(&s, &format!("docs.b.{w}.expr"), Value), var_before);
    assert_eq!(prop_of(&s, &format!("docs.b.{w}.0"), Value), None);
    // A click still opens the editor, with its preview rendered.
    s.click_node("docs.b.0.0");
    assert_eq!(laid_out(&s, PREVIEW).1, 1);
    s.key("Esc");
    assert_eq!(s.text(), before);
    // Per tab: a new tab starts rendered; coming back keeps source.
    s.ed.command("tab.new", "");
    s.frame(Default::default());
    assert!(!s.ed.docs_source());
    s.ed.command("tab.prev", "");
    s.frame(Default::default());
    assert!(s.ed.docs_source());
    assert_eq!(laid_out(&s, "docs.b.0.0").1, 0);
    // Back to rendered; the document never changed.
    s.ed.command("docs.source", "");
    s.frame(Default::default());
    assert!(!s.ed.docs_source());
    assert_eq!(laid_out(&s, "docs.b.0.0").1, 1);
    assert_eq!(s.text(), before);
}

/// The prose editor open with its live preview, under the Modern preset.
#[test]
fn golden_docs_preview_modern() {
    let mut s = open("golden-preview");
    s.ed.command("view.theme_modern", "");
    s.frame(Default::default());
    s.click_node("docs.b.0.0");
    s.key("Ctrl+A");
    s.type_text("## Gain");
    s.key("Enter");
    s.key("Enter");
    s.type_text("The DC gain is $A_0 = g_m r_o$, about **40 dB**.");
    let px = s.render();
    common::check_golden("docs_preview_modern", px);
}
