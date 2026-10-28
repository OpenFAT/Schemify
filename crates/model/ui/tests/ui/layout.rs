//! Layout geometry: bars and canvas, popups and cascades, every widget kind, toolbar icons,
//! and the AccessKit snapshot (re-bless with `SCHEMIFY_BLESS=1`).
//!
//! Contents: no `pub` items.

use schemify_ui::{
    Kind, Layer, NO_TEXT, SPLIT_GAP, Shown, Strings, TextSystem, UiState, access, compose, layout,
    records,
};

use crate::{VIEW, fixture};

// ---- private ----

#[test]
fn layout_stacks_bars_and_fills_canvas() {
    let f = fixture(&[]);
    let r = |s: &str| f.frame.rect()[f.row(s)];
    let (bar, tools, tabs, canvas, status) = (
        r("menubar"),
        r("toolbar"),
        r("tabs"),
        r("canvas"),
        r("status"),
    );
    assert_eq!((bar.x, bar.y, bar.w), (0.0, 0.0, VIEW.0));
    assert_eq!(tools.y, bar.y + bar.h);
    assert_eq!(tabs.y, tools.y + tools.h);
    assert_eq!(canvas.y, tabs.y + tabs.h);
    assert_eq!(status.y + status.h, VIEW.1);
    assert_eq!(canvas.y + canvas.h, status.y, "canvas grows to fill");
    // Schematic | docs page side by side (the app removes the pane while docs are hidden).
    let docs = r("docs.pane");
    assert_eq!(
        (docs.x, docs.y, docs.h),
        (canvas.w + SPLIT_GAP, canvas.y, canvas.h),
        "beyond the split's divider"
    );
    // Then the cell settings pane (likewise removed by the app while hidden).
    let cell = r("cell.pane");
    assert_eq!(cell.x, docs.x + docs.w + SPLIT_GAP);
    assert_eq!(canvas.w + SPLIT_GAP + docs.w + SPLIT_GAP + cell.w, VIEW.0);
    assert_eq!(docs.w, 560.0, "the theme's docs-pane width");
    assert_eq!(cell.w, 380.0, "the theme's cell-pane width");
    let (file, edit) = (r("menu.file"), r("menu.edit"));
    assert!(file.x < edit.x && (file.y - edit.y).abs() < 0.5 && file.x + file.w <= edit.x + 0.5);
    // The overlay holds only the canvas's floats (the view switcher): no dialog, no popup.
    assert_eq!(f.frame.floats().end, f.frame.len(), "nothing open");
    let fl = f.frame.floats();
    assert!(!fl.is_empty() && fl.start == f.frame.rows(Layer::Overlay).start);
    let sw = f.frame.rect()[fl.start];
    assert!(
        sw.x > canvas.x && sw.x + sw.w < canvas.x + canvas.w,
        "floats inside the canvas"
    );
}

#[test]
fn open_menu_and_cascade_place_popups() {
    let mut f = fixture(&[]);
    f.state
        .open_menus(&[f.sym("menu.file"), f.sym("menu.file.image_export")]);
    f.relayout();
    let overlay = f.frame.rows(Layer::Overlay);
    let popups: Vec<usize> = overlay
        .clone()
        .filter(|&r| f.frame.shown()[r] == Shown::Popup)
        .collect();
    assert_eq!(popups.len(), 2);
    let (title, p1, p2) = (
        f.frame.rect()[f.row("menu.file")],
        f.frame.rect()[popups[0]],
        f.frame.rect()[popups[1]],
    );
    assert_eq!(
        (p1.x, p1.y),
        (title.x, title.y + title.h),
        "drops below its title"
    );
    let cascade = overlay
        .clone()
        .find(|&r| {
            f.frame.shown()[r] == Shown::Cascade
                && f.frame.node()[r] == f.node("menu.file.image_export")
        })
        .unwrap();
    let c = f.frame.rect()[cascade];
    assert_eq!(
        (p2.x, p2.y),
        (c.x + c.w, c.y),
        "cascades to the right of its row"
    );
    let save = overlay
        .clone()
        .find(|&r| f.frame.node()[r] == f.node("file.save"))
        .unwrap();
    let open = overlay
        .clone()
        .find(|&r| f.frame.node()[r] == f.node("file.open"))
        .unwrap();
    let (rs, ro) = (f.frame.rect()[save], f.frame.rect()[open]);
    assert!(
        rs.y > ro.y && rs.x == ro.x && rs.w == ro.w,
        "rows stack and share the popup width"
    );
    assert!(f.frame.accel()[save] != schemify_ui::NO_TEXT);
}

#[test]
fn accesskit_tree_snapshot() {
    let mut f = fixture(&[]);
    f.state
        .open_menus(&[f.sym("menu.options"), f.sym("menu.options.crosshair")]);
    f.relayout();
    let update = access::access_tree(&f.tables, &f.frame, &f.strings, &f.state);
    let text = access::dump(&update);
    assert_eq!(
        update.nodes.len(),
        f.frame.len(),
        "one AccessKit node per visible row"
    );
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/snapshots/access_options_menu.txt"
    );
    if std::env::var_os("SCHEMIFY_BLESS").is_some() {
        std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
        std::fs::write(path, &text).unwrap();
    }
    let want = std::fs::read_to_string(path).expect("snapshot missing; run with SCHEMIFY_BLESS=1");
    if text != want {
        // CI uploads `*.actual.*` from here, so a PR can re-bless from its run.
        let out = concat!(
            env!("CARGO_TARGET_TMPDIR"),
            "/access_options_menu.actual.txt"
        );
        std::fs::write(out, &text).unwrap();
    }
    assert_eq!(text, want);
    // Negative space: closed menus' items are not exported.
    assert!(!text.contains("MenuItem \"Undo\""));
    assert!(text.contains("MenuItem \"Draw crosshair\""));
}

#[test]
fn every_kind_lays_out() {
    let mut src = String::from("node w window\n");
    for k in Kind::ALL
        .iter()
        .filter(|k| !matches!(k, Kind::Window | Kind::Dialog))
    {
        src += &format!("node w.{n} {n}\n  parent w\n  label {n}\n", n = k.name());
    }
    src += "set w.math value \"x^2\"\n";
    let mut strings = Strings::new();
    let (c, r) = records::parse(&src, &mut strings);
    assert!(r.is_empty());
    let tables = compose(&strings, &c, &[]);
    let mut text = TextSystem::new();
    let frame = layout(
        &tables,
        &strings,
        &mut text,
        &UiState::default(),
        (400.0, 2000.0),
    );
    assert_eq!(frame.len(), Kind::COUNT - 1);
    let math = frame
        .row_of(tables.nodes.find(strings.find("w.math").unwrap()).unwrap())
        .unwrap();
    assert!(frame.math()[math] != u32::MAX && frame.rect()[math].h > 4.0);
}

/// Toolbar icons: every default toolbar button parses its icon and shows it instead of its
/// label (a 1.4 em square plus padding), the label stays the accessible name, and bad icon
/// data falls back to the label.
#[test]
fn toolbar_buttons_draw_icons_and_keep_labels_for_access() {
    let f = fixture(&["node t.bad button\n  parent toolbar\n  label Bad\n  icon \"L1 2\"\n"]);
    let n = &f.tables.nodes;
    let mut buttons = 0;
    for c in n.children(f.node("toolbar")) {
        let id = f.strings.get(n.id()[c as usize]).to_owned();
        if n.kind()[c as usize] != Kind::Button || id == "t.bad" {
            continue;
        }
        let r = f.row(&id);
        assert!(f.frame.icon()[r] != u32::MAX, "{id} has an icon");
        assert_eq!(f.frame.label()[r], NO_TEXT, "{id}: icon instead of label");
        buttons += 1;
    }
    assert!(buttons >= 25, "{buttons}");
    let wire = f.frame.rect()[f.row("tb.wire")];
    assert_eq!(wire.w, (12.0f32 * 1.4).round() + 2.0 * 5.0);
    let bad = f.row("t.bad");
    assert_eq!(f.frame.icon()[bad], u32::MAX);
    assert!(f.frame.label()[bad] != NO_TEXT);
    let tree = access::access_tree(&f.tables, &f.frame, &f.strings, &f.state);
    let dump = access::dump(&tree);
    assert!(dump.contains("Button \"Wire\""), "{dump}");
}
