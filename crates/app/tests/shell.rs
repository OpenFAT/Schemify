//! The editor shell: boot without the default UI plugin (D6), keymap coverage against the
//! xschem inventory (D25), hit-testing and box-selection properties, and the CPU golden image
//! of the editor on the inverter fixture (dark theme).

mod common;
use common::{Session, U, VIEW};

use schemify_app::commands::{CORE, Commands, Target};
use schemify_app::scene::{self, Objects, Scene};
use schemify_ui::{Button, Chord, InputEvent, InputEvents, Kind};

#[test]
fn boots_without_the_default_ui_plugin() {
    let mut s = Session::open("inv", "top.shcomp", "bare", false);
    let n = &s.ed.shell.tables.nodes;
    let kinds: Vec<Kind> = (0..n.len()).map(|i| n.kind()[i]).collect();
    // The core base also holds the docs pane (D21: docs render without plugins); a cell
    // without doc blocks starts with it removed.
    assert_eq!(
        kinds,
        [Kind::Window, Kind::Split, Kind::Canvas],
        "blank UI: window + work split + canvas only"
    );
    assert!(
        s.ed.shell.tables.keymap.chords().is_empty(),
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

/// Expected chords from docs/research/xschem-feature-inventory.md §2.1: every canvas key
/// row except graph-only, cadence-only, placeholder and Tk-dialog rows.
fn inventory_chords() -> Vec<(String, String)> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/research/xschem-feature-inventory.md"
    );
    let text = std::fs::read_to_string(path).expect("inventory");
    let start = text.find("### 2.1").expect("§2.1");
    let end = text[start..].find("### 2.2").expect("§2.2") + start;
    let mut out = Vec::new();
    for row in text[start..end].lines().filter(|l| l.starts_with("| ")) {
        let cols: Vec<&str> = row
            .split(" | ")
            .map(|c| c.trim_matches(['|', ' ']))
            .collect();
        if cols.len() < 4 || cols[0] == "Key" || cols[0].starts_with("---") {
            continue;
        }
        let (key, m, ctx) = (cols[0], cols[1], cols[2]);
        if ctx == "G only" || ctx.contains("cad=1") || key.contains(';') || key == "Ctrl+Enter" {
            continue;
        }
        let mods = match m {
            "Ctrl" => "Ctrl+",
            "Alt" => "Alt+",
            "Ctrl+Alt" => "Ctrl+Alt+",
            "Shift" => "Shift+",
            _ => "", // –, (Shift), any
        };
        let keys: Vec<String> = if key == "0-9" {
            (0..10).map(|d| d.to_string()).collect()
        } else {
            vec![key.trim_matches('`').to_owned()]
        };
        for k in keys {
            let k = match k.as_str() {
                "Return" => "Enter".to_owned(),
                "Escape" => "Esc".to_owned(),
                "Delete" => "Del".to_owned(),
                "Insert" => "Ins".to_owned(),
                "\\\\" => "\\".to_owned(),
                s if s.len() == 1 && s.chars().all(|c| c.is_ascii_uppercase()) => {
                    format!("Shift+{s}")
                }
                s => s.to_owned(),
            };
            let chord = format!("{mods}{k}");
            out.push((chord, row.to_owned()));
        }
    }
    out
}

#[test]
fn keymap_covers_the_xschem_inventory() {
    let s = Session::open("inv", "top.shcomp", "keys", true);
    let keymap = &s.ed.shell.tables.keymap;
    let expected = inventory_chords();
    assert!(
        expected.len() >= 150,
        "parsed {} inventory combos",
        expected.len()
    );
    // xschem functions with no meaning here keep no binding (docs/BACKLOG.md "Parity:
    // intentionally dropped"): the logic simulator (0-4), the launcher (Ctrl+h), insertion
    // order (Shift+S), floaters (Ctrl+u), X11 drawing modes ($, Ctrl+$).
    const DROPPED: [&str; 10] = [
        "0", "1", "2", "3", "4", "Ctrl+h", "Shift+S", "Ctrl+u", "$", "Ctrl+$",
    ];
    let mut missing = Vec::new();
    for (chord, row) in &expected {
        let c = Chord::parse(chord).unwrap_or_else(|| panic!("{chord} parses ({row})"));
        let dropped = DROPPED.contains(&chord.as_str());
        if keymap.lookup(c).is_none() != dropped {
            missing.push(chord.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "unbound (or dropped but bound) inventory chords: {missing:?}"
    );
    // How many bindings reach an implementation (core command or plugin command).
    let commands = Commands::default();
    let strings = &s.ed.shell.strings;
    let bound = keymap.commands();
    let implemented = bound
        .iter()
        .filter(|&&c| {
            let name = strings.get(c);
            !matches!(commands.resolve(name), Target::Unknown) || name.starts_with("help.")
        })
        .count();
    eprintln!(
        "keymap: {} inventory combos, {} bindings, {} reach an implementation, {} core commands",
        expected.len(),
        bound.len(),
        implemented,
        CORE.len()
    );
    // The editing, view and file keys of the M2 scope are all implemented.
    let required = [
        "w",
        "Shift+W",
        "m",
        "Ctrl+M",
        "Shift+M",
        "c",
        "Del",
        "Shift+R",
        "Shift+F",
        "Shift+V",
        "Alt+R",
        "Alt+F",
        "Alt+V",
        "u",
        "Shift+U",
        "Ctrl+S",
        "Ctrl+N",
        "Ctrl+Shift+N",
        "Ctrl+O",
        "Ctrl+W",
        "Ctrl+Q",
        "Alt+S",
        "z",
        "Shift+Z",
        "Ctrl+Z",
        "f",
        "Ctrl+Shift+F",
        "q",
        "Shift+Q",
        "Ctrl+Shift+Q",
        "Ins",
        "Shift+I",
        "Shift+Ins",
        "Ctrl+I",
        "e",
        "i",
        "Ctrl+E",
        "BackSpace",
        "Alt+E",
        "Alt+I",
        "Ctrl+A",
        "Ctrl+C",
        "Ctrl+X",
        "Ctrl+V",
        "Left",
        "Right",
        "Up",
        "Down",
        "Ctrl+Left",
        "Ctrl+Right",
        "Shift+P",
        "Esc",
        "Alt+L",
        "Alt+Shift+L",
        "Ctrl+P",
        "Ctrl+Shift+P",
        "Alt+P",
        "t",
        "l",
        "r",
        "g",
        "Shift+G",
        "%",
        "Shift+O",
        "Space",
        "h",
        "v",
        "d",
        "Shift+L",
        "n",
        "Shift+N",
        "s",
        "k",
        "Shift+K",
    ];
    for chord in required {
        let c = Chord::parse(chord).unwrap();
        let cmd = keymap
            .lookup(c)
            .unwrap_or_else(|| panic!("{chord} unbound"));
        let name = strings.get(cmd);
        assert!(
            matches!(commands.resolve(name), Target::Core(_)),
            "{chord} → {name} has no core implementation"
        );
    }
    assert!(implemented >= required.len());
}

fn covers(objs: &Objects, i: usize, p: (f64, f64), tol: f64) -> bool {
    let g = objs.geom[i];
    if objs.seg[i] {
        let (ax, ay, bx, by) = (
            f64::from(g[0]),
            f64::from(g[1]),
            f64::from(g[2]),
            f64::from(g[3]),
        );
        let (dx, dy) = (bx - ax, by - ay);
        let l2 = (dx * dx + dy * dy).max(1e-9);
        let t = (((p.0 - ax) * dx + (p.1 - ay) * dy) / l2).clamp(0.0, 1.0);
        let (qx, qy) = (ax + t * dx - p.0, ay + t * dy - p.1);
        (qx * qx + qy * qy).sqrt() <= tol + 1e-6
    } else {
        let b = objs.bbox(i).map(f64::from);
        p.0 >= b[0] - tol && p.0 <= b[2] + tol && p.1 >= b[1] - tol && p.1 <= b[3] + tol
    }
}

/// One point on every primitive, with its owner.
fn prim_points(sc: &Scene) -> Vec<((f64, f64), u32)> {
    let p = &sc.prims;
    let o = &sc.owner;
    let mut out = Vec::new();
    for i in 0..p.lines.x0.len() {
        let mid = (
            (f64::from(p.lines.x0[i]) + f64::from(p.lines.x1[i])) / 2.0,
            (f64::from(p.lines.y0[i]) + f64::from(p.lines.y1[i])) / 2.0,
        );
        out.push((mid, o.lines[i]));
    }
    for i in 0..p.rects.x0.len() {
        out.push((
            (f64::from(p.rects.x0[i]), f64::from(p.rects.y0[i])),
            o.rects[i],
        ));
    }
    for i in 0..p.arcs.cx.len() {
        let a = f64::from(p.arcs.start[i]).to_radians() / 1000.0;
        let r = f64::from(p.arcs.r[i]);
        let c = (f64::from(p.arcs.cx[i]), f64::from(p.arcs.cy[i]));
        out.push(((c.0 + r * a.cos(), c.1 + r * a.sin()), o.arcs[i]));
    }
    for (polys, owners) in [(&p.polylines, &o.polylines), (&p.polygons, &o.polygons)] {
        let mut start = 0;
        for (k, &end) in polys.end.iter().enumerate() {
            out.push((
                (f64::from(polys.x[start]), f64::from(polys.y[start])),
                owners[k],
            ));
            start = end as usize;
        }
    }
    for i in 0..p.texts.x.len() {
        let q = (
            f64::from(p.texts.x[i]) + 500.0,
            f64::from(p.texts.y[i]) + 500.0,
        );
        out.push((q, o.texts[i]));
    }
    out
}

fn check_hits(sc: &Scene) {
    let tol = f64::from(U);
    let mut checked = 0;
    for (p, owner) in prim_points(sc) {
        if owner == u32::MAX {
            continue; // junction dots are cues, not objects
        }
        assert!(
            covers(&sc.objs, owner as usize, p, tol),
            "object {owner} does not cover its own primitive at {p:?}"
        );
        let hit = scene::hit(&sc.objs, p, tol).expect("every drawn primitive is hittable");
        assert!(
            covers(&sc.objs, hit as usize, p, tol),
            "hit returned a non-covering object"
        );
        checked += 1;
    }
    assert!(checked > 10, "{checked} primitives checked");
}

#[test]
fn every_primitive_is_hittable_schematic_and_symbol() {
    let mut s = Session::open("inv", "top.shcomp", "hits", true);
    check_hits(s.ed.scene());
    s.click((10 * U, 0));
    s.key("e");
    check_hits(s.ed.scene());
    s.key("Ctrl+E");
    s.click((10 * U, 0));
    s.key("i");
    check_hits(s.ed.scene());
}

#[test]
fn box_select_is_consistent_with_point_hits() {
    let s = Session::open("inv", "top.shcomp", "boxes", true);
    let sc = s.ed.scene();
    let objs = &sc.objs;
    let e = sc.extent.map(f64::from);
    // The whole extent selects everything, in both modes; an empty box far away nothing.
    let all = scene::box_select(
        objs,
        [e[0] - 1.0, e[1] - 1.0, e[2] + 1.0, e[3] + 1.0],
        false,
    );
    assert_eq!(all.len(), objs.len());
    assert!(scene::box_select(objs, [9e8, 9e8, 9e8 + 1.0, 9e8 + 1.0], true).is_empty());
    let mut seed = 0x2545_f491_u32;
    let mut rnd = |m: f64| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        f64::from(seed) / f64::from(u32::MAX) * m
    };
    let (w, h) = (e[2] - e[0], e[3] - e[1]);
    for _ in 0..200 {
        let (x0, y0) = (e[0] + rnd(w), e[1] + rnd(h));
        let b = [x0, y0, x0 + rnd(w / 2.0), y0 + rnd(h / 2.0)];
        let inside = scene::box_select(objs, b, false);
        let touch = scene::box_select(objs, b, true);
        assert!(
            inside.iter().all(|i| touch.contains(i)),
            "inside ⊆ touching"
        );
        for &i in &inside {
            let bb = objs.bbox(i as usize).map(f64::from);
            assert!(bb[0] >= b[0] && bb[2] <= b[2] && bb[1] >= b[1] && bb[3] <= b[3]);
        }
        // Union of point hits inside the box ⊆ touching selection.
        for k in 0..64 {
            let p = (
                b[0] + (b[2] - b[0]) * f64::from(k % 8) / 7.0,
                b[1] + (b[3] - b[1]) * f64::from(k / 8) / 7.0,
            );
            if let Some(j) = scene::hit(objs, p, 0.0) {
                assert!(
                    touch.contains(&j),
                    "point hit {j} at {p:?} outside the touch selection"
                );
            }
        }
    }
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

/// The editor on the inverter fixture (top cell with two `inv` sub-cell instances), dark
/// theme, default UI. Re-bless with `SCHEMIFY_BLESS=1`.
#[test]
fn golden_editor_dark() {
    let mut s = Session::open("inv", "top.shcomp", "golden", true);
    let px = s.render();
    check_golden("editor_dark", px);
}

/// A plugin-owned command (the default UI's `help.keys`, bound to `/`) re-contributes UI.
#[test]
fn default_ui_plugin_owns_its_commands() {
    let mut s = Session::open("inv", "top.shcomp", "helpkeys", true);
    let panel = |s: &Session| {
        let id = s.ed.shell.strings.find("keys.panel");
        id.and_then(|id| s.ed.shell.tables.nodes.find(id)).is_some()
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
        env!("CARGO_MANIFEST_DIR"),
        "/../plugin/tests/fixtures/hello.wasm"
    );
    std::fs::copy(wasm, dir.join("hello.wasm")).unwrap();
    let manifest = std::fs::read_to_string(dir.join("schemify.project")).unwrap();
    std::fs::write(
        dir.join("schemify.project"),
        format!("{manifest}\nplugin hello path hello.wasm\n"),
    )
    .unwrap();
    let ed = schemify_app::editor::Editor::new(schemify_app::editor::Options {
        no_default_ui: false,
        path: Some(dir.join("top.shcomp")),
        seed: 3,
        ..Default::default()
    })
    .unwrap();
    let mut s = Session { ed, now: 1, dir };
    s.frame(Default::default());
    s.frame(Default::default());
    assert_eq!(
        s.ed.shell.tables.owners.len(),
        4,
        "base, default-ui, hello, state"
    );
    s.click_node("menu.tools");
    s.click_node("tools.count_nets");
    assert!(
        s.ed.session.status.contains("single-connection nets"),
        "{}",
        s.ed.session.status
    );
}

/// Waves: the chooser lists `.raw` files of the project; picking one shows the viewer;
/// Ctrl+W returns to the schematic.
#[test]
fn waves_window_opens_a_raw_file_and_closes() {
    let mut s = Session::open("inv", "top.shcomp", "waves", true);
    let raw = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../wave/tests/fixtures/rc_tran.bin.raw"
    );
    std::fs::copy(raw, s.dir.join("rc_tran.raw")).unwrap();
    s.click_node("tb.waves");
    s.click_node("dialog.choose.row.0");
    assert!(s.ed.waves_open(), "{}", s.ed.session.status);
    let px = s.render();
    assert_eq!(px.width(), VIEW.0 as u16);
    s.key("Ctrl+W");
    assert!(!s.ed.waves_open());
}

/// The same with a selection and the light canvas (Shift+O), to pin marks and the palette.
#[test]
fn golden_editor_light_selection() {
    let mut s = Session::open("inv", "top.shcomp", "golden-light", true);
    s.click((10 * U, 0));
    s.key("Shift+O");
    assert!(s.ed.opts.light);
    let px = s.render();
    check_golden("editor_light_selection", px);
}

/// Pointer moves rewrite the status line (coordinates) every frame; the UI string table stays
/// the same size (live strings), so a long session cannot fill it.
#[test]
fn pointer_moves_do_not_grow_the_ui_string_table() {
    let mut s = Session::open("inv", "top.shcomp", "strings", true);
    s.move_to((0, 0));
    let len = s.ed.shell.strings.len();
    for i in 0..300 {
        s.move_to((i * 10 * U, (i % 7) * 10 * U));
    }
    assert_eq!(
        s.ed.shell.strings.len(),
        len,
        "the permanent table did not grow"
    );
    assert!(s.ed.shell.strings.live_len() < 4096, "one live generation");
    let status = s.ed.shell.strings.find("status.msg").unwrap();
    let n = s.ed.shell.tables.nodes.find(status).unwrap();
    let label = s.ed.shell.tables.nodes.prop(schemify_ui::Prop::Label)[n as usize];
    assert!(
        s.ed.shell.strings.get(label).contains("mouse = 2990 "),
        "the status line shows the last move: {}",
        s.ed.shell.strings.get(label)
    );
}

/// The waves dock divider: a press on the boundary and a drag move it (the editor above
/// shrinks), clamped to the allowed range; a press away from it is the editor's.
#[test]
fn waves_dock_divider_drags() {
    let mut s = Session::open("inv", "top.shcomp", "dock-split", true);
    let raw = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../wave/tests/fixtures/rc_tran.bin.raw"
    );
    std::fs::copy(raw, s.dir.join("rc_tran.raw")).unwrap();
    s.click_node("tb.waves");
    s.click_node("dialog.choose.row.0");
    assert!(s.ed.waves_open(), "{}", s.ed.session.status);
    let split = (VIEW.1 * 0.6).floor();
    let before = s.ed.canvas_rect();
    assert!(
        before.y1 <= f64::from(split) && before.y1 > 400.0,
        "{before:?}"
    );
    let drag = |s: &mut Session, from: f32, to: f32| {
        let mut ev = InputEvents::default();
        ev.push(InputEvent::PointerMove { x: 600.0, y: from });
        ev.push(InputEvent::PointerDown {
            x: 600.0,
            y: from,
            button: Button::Left,
        });
        ev.push(InputEvent::PointerMove { x: 600.0, y: to });
        ev.push(InputEvent::PointerUp {
            x: 600.0,
            y: to,
            button: Button::Left,
        });
        s.frame(ev);
    };
    drag(&mut s, split, 300.0);
    let after = s.ed.canvas_rect();
    assert!(after.y1 <= 300.0 && after.y1 > 200.0, "{after:?}");
    assert!(s.ed.waves_open());
    // Clamped at 15% of the window.
    drag(&mut s, 300.0, 5.0);
    assert!(s.ed.canvas_rect().y1 <= f64::from(VIEW.1 * 0.15) + 1.0);
    // A press 40 px above the divider belongs to the editor: the divider stays.
    let at = (VIEW.1 * 0.15).floor();
    drag(&mut s, at - 40.0, 600.0);
    assert!(s.ed.canvas_rect().y1 <= f64::from(VIEW.1 * 0.15) + 1.0);
}
