//! The docs page end to end, headless (D19, D20, D21): the OTA fixture's doc blocks beside its
//! schematic, edits through the real frame spine (click a field, type, Enter), the D20 cache
//! paths, and a CPU golden of the page. Oracles: the `gmid` plugin's own answers, the
//! canonical file text (undo restores it byte for byte), and a fresh `CellState` build.

mod common;
use common::{Session, lines};

use schemify_elab::{CellState, Val, no_funcs};
use schemify_expr::{Mode, Program, Scratch, Template, Value};

/// The OTA fixture's module, as the manifest loads it.
const WASM: &[u8] = include_bytes!("fixtures/ota/plugins/gmid_lut.wasm");

fn open(tag: &str) -> Session {
    let mut s = Session::open("ota", "ota.shcomp", tag, true);
    s.frame(Default::default());
    s
}

/// Value and unverified flag of own var `name`.
fn var(s: &Session, name: &str) -> (Val, bool) {
    let cell = s.ed.cell().expect("elaborated");
    var_in(cell, name)
}

fn var_in(cell: &CellState, name: &str) -> (Val, bool) {
    let g = &cell.graph;
    let v = g
        .var_name
        .iter()
        .position(|n| cell.names.get(*n) == name)
        .unwrap_or_else(|| panic!("var {name}"));
    let node = g.params as usize + v;
    (g.vals[node].clone(), g.unverified[node])
}

fn num(s: &Session, name: &str) -> f64 {
    var(s, name)
        .0
        .as_f64()
        .unwrap_or_else(|| panic!("{name} = {:?}", var(s, name)))
}

/// Evaluated value of param `param` of the instance named `inst`.
fn inst_param(s: &Session, inst: &str, param: &str) -> (Val, bool) {
    let (h, _) = s.ed.active().unwrap();
    let doc = s.ed.session.project.doc(h).unwrap();
    let st = doc.strings();
    let i = (0..doc.instances().id.len())
        .find(|&i| st.get(doc.instances().name[i]) == inst)
        .unwrap_or_else(|| panic!("instance {inst}"));
    let cell = s.ed.cell().unwrap();
    let iface = &cell.ifaces[cell.inst_iface[i] as usize];
    let k = iface
        .param_name
        .iter()
        .position(|n| cell.names.get(*n) == param)
        .unwrap();
    let node = cell.graph.inst_param(i, k).unwrap() as usize;
    (cell.graph.vals[node].clone(), cell.graph.unverified[node])
}

/// Doc block row of var `name`.
fn row(s: &Session, name: &str) -> u32 {
    let (h, _) = s.ed.active().unwrap();
    let doc = s.ed.session.project.doc(h).unwrap();
    let b = doc.doc_blocks();
    (0..b.kind.len())
        .find(|&r| doc.strings().get(b.fields[r][0]) == name)
        .unwrap_or_else(|| panic!("block {name}")) as u32
}

/// Replaces var `name`'s expression through its page field: click, select all, type, Enter.
fn edit_var(s: &mut Session, name: &str, expr: &str) {
    let r = row(s, name);
    s.click_node(&format!("docs.b.{r}.expr"));
    s.key("Ctrl+A");
    s.type_text(expr);
    s.key("Enter");
}

/// `cached` attribute values of var `name` in the file text.
fn cached(text: &str, name: &str) -> Option<(f64, String)> {
    let mut lines = text
        .lines()
        .skip_while(|l| !l.starts_with(&format!("var {name} ")));
    lines.next()?;
    let line = lines
        .take_while(|l| l.starts_with("  "))
        .find(|l| l.starts_with("  cached "))?;
    let mut f = line.split_whitespace().skip(1);
    let v = schemify_expr::parse_num(f.next()?).ok()?;
    Some((v, f.next()?.to_owned()))
}

/// The content hash the app writes for the fixture plugin (first 8 bytes of its sha256).
fn plugin_hash() -> String {
    let mut h = schemify_doc::hex(&schemify_doc::sha256(WASM));
    h.truncate(16);
    h
}

fn eng(x: f64) -> String {
    let t = Template::parse("{eng(x)}").unwrap();
    let mut inputs = Vec::new();
    t.program()
        .bind(&mut inputs, |_| Some(Value::Num(x)))
        .unwrap();
    let mut out = String::new();
    t.render(&inputs, &mut Scratch::default(), &mut out)
        .unwrap();
    out
}

/// Node ids under the docs page, in composition order.
fn page_nodes(s: &Session) -> Vec<String> {
    let sh = &s.ed.shell;
    let n = &sh.tables.nodes;
    (0..n.len())
        .map(|i| sh.strings.get(n.id()[i]).to_owned())
        .filter(|id| id.starts_with("docs.b.") || id == "docs.empty")
        .collect()
}

fn label_of(s: &Session, id: &str) -> String {
    let sh = &s.ed.shell;
    let n = &sh.tables.nodes;
    let i = n
        .find(sh.strings.find(id).unwrap_or_else(|| panic!("{id}")))
        .unwrap_or_else(|| panic!("{id} composed"));
    sh.strings
        .get(n.prop(schemify_ui::Prop::Label)[i as usize])
        .to_owned()
}

#[test]
fn ota_page_shows_blocks_with_gmid_values_beside_the_schematic() {
    let s = open("page");
    assert!(
        s.ed.docs_shown(),
        "a cell with doc blocks opens with its page"
    );
    let canvas = s.ed.canvas_rect();
    assert!(
        canvas.x1 < 800.0 && canvas.height() > 600.0,
        "schematic | docs side by side: {canvas:?}"
    );
    // Values come from the plugin, exactly (the same LUT query the plugin's own tests pin).
    let w_in = num(&s, "W_in");
    assert!((w_in - 3.881e-6).abs() < 2e-9, "W_in = {w_in}");
    assert!(!var(&s, "W_in").1, "plugin loaded: verified");
    assert!((num(&s, "Av_in") - 112.2).abs() < 0.1);
    assert!((num(&s, "GBW") - 150e-6 / (2.0 * std::f64::consts::PI * 1e-12)).abs() < 1.0);
    // Instance overrides `W = W_in` evaluate to the var; the symbol label shows it.
    assert_eq!(inst_param(&s, "M1", "W"), (Val::Num(w_in), false));
    assert_eq!(inst_param(&s, "M5", "L"), (Val::Num(1e-6), false));
    let texts = &s.ed.scene().prims.texts.text;
    assert!(
        texts.contains(&format!("W={}", eng(w_in))),
        "evaluated W on the canvas"
    );
    // The page: prose segments, var rows with values, the plot placeholder, in file order.
    let ids = page_nodes(&s);
    assert!(ids.contains(&"docs.b.0.0".to_owned()), "{ids:?}");
    let r = row(&s, "W_in");
    assert_eq!(
        label_of(&s, &format!("docs.b.{r}.val")),
        format!("= {}m", schemify_wave::format_si(w_in, 4))
    );
    let g = row(&s, "gm_id_in");
    assert_eq!(label_of(&s, &format!("docs.b.{g}.val")), "= 15 1/V");
    let plot = ids.iter().find(|i| {
        i.ends_with(".expr") && label_of_opt(&s, i).is_some_and(|l| l.starts_with("plot"))
    });
    assert!(
        plot.is_some(),
        "the ```plot fence becomes a graph slot: {ids:?}"
    );
    // Caches carry this plugin's hash; opening wrote only when they were stale.
    let text = s.text();
    for v in ["W_in", "gm_in", "Av_in", "ft_in", "W_load", "W_tail"] {
        let (value, hash) = cached(&text, v).unwrap_or_else(|| panic!("{v} cached"));
        assert_eq!(hash, plugin_hash(), "{v}");
        assert_eq!(value, num(&s, v), "{v}");
    }
    let (h, _) = s.ed.active().unwrap();
    let fixture = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/ota/ota.shcomp"
    ))
    .unwrap();
    let fresh = cached(&fixture, "W_in").is_some_and(|c| c.1 == plugin_hash());
    assert_eq!(
        s.ed.session.project.is_dirty(h),
        !fresh,
        "clean iff the caches matched"
    );
}

fn label_of_opt(s: &Session, id: &str) -> Option<String> {
    let sh = &s.ed.shell;
    let n = &sh.tables.nodes;
    let i = n.find(sh.strings.find(id)?)?;
    let l = n.prop(schemify_ui::Prop::Label)[i as usize];
    (!l.is_none()).then(|| sh.strings.get(l).to_owned())
}

#[test]
fn a_var_edit_recomputes_page_and_instances_in_one_undo() {
    let mut s = open("edit");
    let (h, _) = s.ed.active().unwrap();
    let before = s.text();
    let undo_before = s.ed.session.project.history_len(h).0;
    let w_before = num(&s, "W_in");
    edit_var(&mut s, "gm_id_in", "10");
    assert_eq!(var(&s, "gm_id_in").0, Val::Int(10));
    let w = num(&s, "W_in");
    assert!(
        w < w_before,
        "stronger inversion: higher current density, narrower device ({w} vs {w_before})"
    );
    assert_eq!(
        inst_param(&s, "M2", "W"),
        (Val::Num(w), false),
        "M2 resized"
    );
    let texts = &s.ed.scene().prims.texts.text;
    assert!(
        texts.contains(&format!("W={}", eng(w))),
        "label updated in the same frame"
    );
    // D20: the cache followed the value inside the same undo entry.
    let text = s.text();
    assert_eq!(cached(&text, "W_in"), Some((w, plugin_hash())));
    assert!(text.contains("var gm_id_in 10\n"), "{text}");
    assert_eq!(
        s.ed.session.project.history_len(h).0,
        undo_before + 1,
        "one edit, one undo entry"
    );
    assert_eq!(
        s.ed.session.project.undo_labels(h).last().copied(),
        Some("edit variable")
    );
    s.key("u");
    assert_eq!(
        s.text(),
        before,
        "undo restores expression and cache together"
    );
    assert_eq!(num(&s, "W_in"), w_before);
    assert_eq!(inst_param(&s, "M1", "W").0, Val::Num(w_before));
    s.key("Shift+U");
    assert_eq!(s.text(), text, "redo replays both");
    // Oracle from the plugin's own sky130 sample checks: gmid.w(nfet, 10, 1u, 20u) = 4.930µm.
    edit_var(&mut s, "L_in", "1u");
    edit_var(&mut s, "Id_tail", "40u");
    assert!(
        (num(&s, "W_in") - 4.930e-6).abs() < 1e-9,
        "{}",
        num(&s, "W_in")
    );
    // A bad expression is kept and shown, never a crash (D13: errors are live findings).
    edit_var(&mut s, "L_in", "0.5u +");
    let r = row(&s, "L_in");
    assert!(
        label_of(&s, &format!("docs.b.{r}.val")).starts_with("X009"),
        "{}",
        label_of(&s, &format!("docs.b.{r}.val"))
    );
    assert!(matches!(var(&s, "W_in").0, Val::Upstream));
    // Out of the LUT's reach: the plugin's range error is the value's error.
    edit_var(&mut s, "L_in", "0.1u");
    let r = row(&s, "W_in");
    assert!(label_of(&s, &format!("docs.b.{r}.val")).starts_with("X010"));
}

#[test]
fn bounds_colour_the_value_red() {
    let mut s = open("bounds");
    edit_var(&mut s, "gm_id_in", "26");
    let r = row(&s, "gm_id_in");
    let label = label_of(&s, &format!("docs.b.{r}.val"));
    assert!(label.contains("above max 25"), "{label}");
    let sh = &s.ed.shell;
    let n = &sh.tables.nodes;
    let i = n
        .find(sh.strings.find(&format!("docs.b.{r}.val")).unwrap())
        .unwrap() as usize;
    assert_eq!(
        sh.strings.get(n.prop(schemify_ui::Prop::Style)[i]),
        "docs-value-bad"
    );
}

/// Copies the fixture and edits one of its files before opening.
fn open_edited(tag: &str, file: &str, edit: impl Fn(String) -> String) -> Session {
    let dir = common::fixture("ota", tag);
    let p = dir.join(file);
    let text = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, edit(text)).unwrap();
    let ed = schemify_app::editor::Editor::new(schemify_app::editor::Options {
        no_default_ui: false,
        path: Some(dir.join("ota.shcomp")),
        seed: 7,
        ..Default::default()
    })
    .expect("opens");
    let mut s = Session { ed, now: 1, dir };
    for _ in 0..3 {
        s.frame(Default::default());
    }
    s
}

#[test]
fn d20_without_the_plugin_files_open_on_cached_values_marked_unverified() {
    let mut s = open_edited("noplugin", "schemify.project", |_| {
        "schemify 1 project\n\nlib dev path dev\n".to_owned()
    });
    let (h, _) = s.ed.active().unwrap();
    let text = s.text();
    let (w_cached, _) = cached(&text, "W_in").unwrap();
    assert_eq!(var(&s, "W_in"), (Val::Num(w_cached), true));
    assert_eq!(
        inst_param(&s, "M1", "W"),
        (Val::Num(w_cached), true),
        "dependents inherit the marker"
    );
    assert!(!var(&s, "id_in").1, "plain vars stay verified");
    let r = row(&s, "W_in");
    assert!(label_of(&s, &format!("docs.b.{r}.unverified")).starts_with("unverified"));
    assert!(
        !s.ed.session.project.is_dirty(h),
        "no plugin, nothing written"
    );
    // The oracle: the same values as a plugin-less elaboration of the file.
    let fresh = CellState::build(&s.ed.session.project, h, &mut no_funcs).unwrap();
    assert_eq!(var_in(&fresh, "W_tail"), var(&s, "W_tail"));
    // A new expression has no cache: X006 until the plugin is back, and the stale cache goes.
    edit_var(&mut s, "W_in", "gmid.w(\"nfet_01v8\", 12, L_in, id_in)");
    assert!(matches!(var(&s, "W_in").0, Val::Err(e) if e.code == schemify_expr::Code::X006));
    assert_eq!(cached(&s.text(), "W_in"), None);
}

#[test]
fn d20_a_changed_plugin_hash_recomputes_and_rewrites_the_cache() {
    let s = open_edited("rehash", "ota.shcomp", |t| {
        t.replace(&format!(" {}\n", fixture_hash(&t)), " 00000000deadbeef\n")
            .replace("cached 112.20605997990423", "cached 1")
    });
    let (h, _) = s.ed.active().unwrap();
    let text = s.text();
    assert!(!text.contains("deadbeef"), "every stale cache rewritten");
    assert_eq!(cached(&text, "Av_in").unwrap().1, plugin_hash());
    assert!(
        (cached(&text, "Av_in").unwrap().0 - 112.2).abs() < 0.1,
        "recomputed value"
    );
    assert!(s.ed.session.project.is_dirty(h));
    assert_eq!(
        s.ed.session.project.undo_labels(h).last().copied(),
        Some("update plugin cache")
    );
    assert!(!var(&s, "Av_in").1);
}

/// The hash the fixture file was cached with.
fn fixture_hash(text: &str) -> String {
    cached(text, "W_in").unwrap().1
}

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
    let b = s.ed.session.project.doc(h).unwrap().doc_blocks();
    let st = s.ed.session.project.doc(h).unwrap().strings();
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
    let sh = &s.ed.shell;
    let page = sh
        .tables
        .nodes
        .find(sh.strings.find("docs.page").unwrap())
        .unwrap();
    let max = sh.layout.scroll_max(page).expect("laid out");
    assert!(max > 100.0, "the OTA page is taller than the window: {max}");
    let r = sh.layout.row_of(page).unwrap();
    let area = sh.layout.rect()[r];
    let top = |s: &Session| {
        let sh = &s.ed.shell;
        let n = sh
            .tables
            .nodes
            .find(sh.strings.find("docs.b.0").unwrap())
            .unwrap();
        sh.layout.rect()[sh.layout.row_of(n).unwrap()].y
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
    let sh = &s.ed.shell;
    let off = y0 - top(&s);
    assert!((off - max).abs() < 0.5, "offset {off} max {max}");
    // A row scrolled above the pane is not clickable through the toolbar.
    let n = sh
        .tables
        .nodes
        .find(sh.strings.find("docs.b.0").unwrap())
        .unwrap();
    let rr = sh.layout.rect()[sh.layout.row_of(n).unwrap()];
    assert!(rr.y + rr.h < area.y);
    assert!(
        sh.layout
            .hit(rr.x + 5.0, rr.y + rr.h - 1.0, false)
            .is_none_or(|h| sh.layout.node()[h] != n)
    );
}

#[test]
fn golden_docs_page() {
    let mut s = open("golden");
    let px = s.render();
    check_golden("docs_page", px);
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

#[test]
fn programs_name_their_plugin_functions() {
    // The cache hash covers exactly the functions an expression calls (docs::cache_ops).
    let p = Program::compile(
        "gmid.w(\"n\", 1, 2, 3) + gmid.jd(\"n\", 1, 2) * x",
        Mode::Scalar,
    )
    .unwrap();
    let f: Vec<(&str, &str)> = p.plugin_fns().collect();
    assert_eq!(f, [("gmid", "w"), ("gmid", "jd")]);
}
