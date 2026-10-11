//! Oracles for the UI tables: composition (last wins + health), layout geometry, input routing,
//! and the AccessKit snapshot. Re-bless snapshots with `SCHEMIFY_BLESS=1`.

use schemify_ui::{
    Button, Chord, HealthKind, InputEvent, InputEvents, Kind, Layer, NO_NODE, Prop, Routed, Shown,
    Strings, StyleProp, Sym, TextSystem, UiLayout, UiState, UiTables, access, compose, layout,
    records, route_input,
};

struct Fixture {
    strings: Strings,
    tables: UiTables,
    text: TextSystem,
    state: UiState,
    frame: UiLayout,
}

const VIEW: (f32, f32) = (1280.0, 800.0);

fn fixture(plugins: &[&str]) -> Fixture {
    let mut strings = Strings::new();
    let (base, rejects) = records::parse(schemify_ui::DEFAULT_UI, &mut strings);
    assert_eq!(rejects, vec![], "default UI must parse cleanly");
    let parsed: Vec<_> = plugins
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (c, r) = records::parse(p, &mut strings);
            assert_eq!(r, vec![]);
            (strings.intern(&format!("plugin{i}")), c)
        })
        .collect();
    let refs: Vec<_> = parsed.iter().map(|(n, c)| (*n, c)).collect();
    let tables = compose(&strings, &base, &refs);
    let mut text = TextSystem::new();
    let state = UiState::default();
    let frame = layout(&tables, &strings, &mut text, &state, VIEW);
    Fixture {
        strings,
        tables,
        text,
        state,
        frame,
    }
}

impl Fixture {
    fn sym(&self, s: &str) -> Sym {
        self.strings
            .find(s)
            .unwrap_or_else(|| panic!("{s} not interned"))
    }
    fn node(&self, s: &str) -> u16 {
        self.tables
            .nodes
            .find(self.sym(s))
            .unwrap_or_else(|| panic!("{s} not composed"))
    }
    fn row(&self, s: &str) -> usize {
        self.frame
            .row_of(self.node(s))
            .unwrap_or_else(|| panic!("{s} not laid out"))
    }
    fn relayout(&mut self) {
        self.frame = layout(
            &self.tables,
            &self.strings,
            &mut self.text,
            &self.state,
            VIEW,
        );
    }
    fn route(&mut self, events: &InputEvents) -> Routed {
        let mut out = Routed::default();
        route_input(
            &self.tables,
            &self.frame,
            &self.strings,
            &mut self.text,
            &mut self.state,
            events,
            &mut out,
        );
        if out.relayout {
            self.relayout();
        }
        out
    }
    fn center(&self, s: &str) -> (f32, f32) {
        let r = self.frame.rect()[self.row(s)];
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }
    fn click(&mut self, s: &str) -> Routed {
        let (x, y) = self.center(s);
        let mut ev = InputEvents::default();
        ev.push(InputEvent::PointerDown {
            x,
            y,
            button: Button::Left,
        });
        ev.push(InputEvent::PointerUp {
            x,
            y,
            button: Button::Left,
        });
        self.route(&ev)
    }
    fn commands(&self, r: &Routed) -> Vec<String> {
        r.commands
            .command
            .iter()
            .map(|&c| self.strings.get(c).to_owned())
            .collect()
    }
}

#[test]
fn default_ui_composes_without_health_rows() {
    let f = fixture(&[]);
    assert!(f.tables.health.is_empty(), "{:?}", f.tables.health);
    let n = &f.tables.nodes;
    assert!(n.len() > 300, "xschem shell has {} nodes", n.len());
    // Pre-order invariants: parents precede children; subtree ends nest.
    for i in 0..n.len() {
        let p = n.parent()[i];
        assert!(p == NO_NODE || (p as usize) < i);
        assert!(n.end()[i] as usize > i && n.end()[i] as usize <= n.len());
        if p != NO_NODE {
            assert!(n.end()[i] <= n.end()[p as usize]);
        }
    }
    let titles: Vec<&str> = n
        .children(f.node("menubar"))
        .map(|c| f.strings.get(n.prop(Prop::Label)[c as usize]))
        .collect();
    assert_eq!(
        titles,
        [
            "File",
            "Edit",
            "Options",
            "View",
            "Properties",
            "Layers",
            "Tools",
            "Symbol",
            "Highlight",
            "Simulation",
            "Help",
            "-",
            "Netlist",
            "Simulate",
            "Waves"
        ]
    );
    assert_eq!(
        f.tables.keymap.lookup(Chord::parse("Ctrl+S").unwrap()),
        Some(f.sym("file.save"))
    );
    assert_eq!(
        f.tables.keymap.lookup(Chord::parse("shift+z").unwrap()),
        Some(f.sym("view.zoom_in"))
    );
    assert_eq!(
        f.tables.keymap.lookup(Chord::parse("%").unwrap()),
        Some(f.sym("options.draw_grid"))
    );
}

#[test]
fn plugins_override_last_wins_with_health_report() {
    let p1 = "set file.save label \"Save!\"\nremove menu.help\nnode tools.count_nets item\n  parent menu.tools\n  label \"Count nets\"\n  command count_nets.run\n  order -1\nstyle item\n  pad 9\nkey Ctrl+S count_nets.run\nset ghost label x\nnode stray item\n  parent nowhere\n";
    let p2 = "set file.save label \"Save!!\"\n";
    let f = fixture(&[p1, p2]);
    let h = &f.tables.health;
    let rows: Vec<(HealthKind, &str, u8, u8)> = (0..h.len())
        .map(|i| (h.kind[i], f.strings.get(h.target[i]), h.by[i], h.over[i]))
        .collect();
    assert_eq!(
        rows,
        vec![
            (HealthKind::PropOverridden, "file.save", 1, 0),
            (HealthKind::NodeRemoved, "menu.help", 1, 0),
            (HealthKind::StyleOverridden, "item", 1, 0),
            (HealthKind::KeyOverridden, "file.save", 1, 0),
            (HealthKind::UnknownTarget, "ghost", 1, 1),
            (HealthKind::PropOverridden, "file.save", 2, 0),
            (HealthKind::Orphaned, "stray", 1, 1),
        ]
    );
    let n = &f.tables.nodes;
    assert_eq!(
        f.strings
            .get(n.prop(Prop::Label)[f.node("file.save") as usize]),
        "Save!!",
        "last wins"
    );
    assert!(
        n.find(f.sym("menu.help")).is_none() && n.find(f.sym("help.about_xschem")).is_none(),
        "subtree removed"
    );
    let first_tool = n.children(f.node("menu.tools")).next().unwrap();
    assert_eq!(
        n.id()[first_tool as usize],
        f.sym("tools.count_nets"),
        "order -1 sorts first"
    );
    assert_eq!(
        f.tables.keymap.lookup(Chord::parse("Ctrl+S").unwrap()),
        Some(f.sym("count_nets.run"))
    );
    assert_eq!(n.owner()[f.node("tools.count_nets") as usize], 1);
}

#[test]
fn recomposition_is_immediate_mode() {
    let with = fixture(&["remove menu.file\n"]);
    assert!(with.tables.nodes.find(with.sym("file.save")).is_none());
    let without = fixture(&[]);
    assert!(
        without
            .tables
            .nodes
            .find(without.sym("file.save"))
            .is_some(),
        "dropping a plugin restores the base"
    );
}

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
    assert_eq!((docs.x, docs.y, docs.h), (canvas.w, canvas.y, canvas.h));
    assert_eq!(canvas.w + docs.w, VIEW.0);
    assert_eq!(docs.w, 560.0, "the theme's docs-pane width");
    let (file, edit) = (r("menu.file"), r("menu.edit"));
    assert!(file.x < edit.x && (file.y - edit.y).abs() < 0.5 && file.x + file.w <= edit.x + 0.5);
    assert!(f.frame.rows(Layer::Overlay).is_empty(), "nothing open");
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
fn routing_keys_menus_clicks_and_canvas() {
    let mut f = fixture(&[]);
    let mut ev = InputEvents::default();
    ev.push(InputEvent::Key {
        chord: Chord::parse("Ctrl+S").unwrap(),
    });
    ev.push(InputEvent::Key {
        chord: Chord::parse("Ctrl+Alt+Shift+F12").unwrap(),
    });
    let out = f.route(&ev);
    assert_eq!(
        f.commands(&out),
        ["file.save"],
        "bound chord runs, unbound is ignored"
    );

    let out = f.click("menu.file");
    assert!(out.commands.is_empty());
    assert_eq!(f.state.open(), [f.sym("menu.file")]);
    // Hover tracking moves the open menu along the bar.
    let (x, y) = f.center("menu.edit");
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerMove { x, y });
    f.route(&ev);
    assert_eq!(f.state.open(), [f.sym("menu.edit")]);
    // Clicking an item runs it and closes the menu.
    let undo_row = f
        .frame
        .rows(Layer::Overlay)
        .find(|&r| f.frame.node()[r] == f.node("edit.undo"))
        .unwrap();
    let rr = f.frame.rect()[undo_row];
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerDown {
        x: rr.x + 5.0,
        y: rr.y + 5.0,
        button: Button::Left,
    });
    let out = f.route(&ev);
    assert_eq!(f.commands(&out), ["edit.undo"]);
    assert!(f.state.open().is_empty());
    // Escape closes an open menu without running a command.
    f.click("menu.view");
    let mut ev = InputEvents::default();
    ev.push(InputEvent::Key {
        chord: Chord::parse("Esc").unwrap(),
    });
    let out = f.route(&ev);
    assert!(out.commands.is_empty() && f.state.open().is_empty());
    // Toolbar button and canvas forwarding (with drag capture outside the canvas).
    let out = f.click("tb.wire");
    assert_eq!(f.commands(&out), ["tools.insert_wire"]);
    let (cx, cy) = f.center("canvas");
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerDown {
        x: cx,
        y: cy,
        button: Button::Middle,
    });
    ev.push(InputEvent::PointerMove { x: 5.0, y: 5.0 });
    ev.push(InputEvent::PointerUp {
        x: 5.0,
        y: 5.0,
        button: Button::Middle,
    });
    ev.push(InputEvent::Wheel {
        x: cx,
        y: cy,
        dx: 0.0,
        dy: 1.0,
    });
    ev.push(InputEvent::Wheel {
        x: 5.0,
        y: 5.0,
        dx: 0.0,
        dy: 1.0,
    });
    let out = f.route(&ev);
    assert_eq!(
        out.canvas.len(),
        4,
        "drag captured; wheel only over the canvas"
    );
}

#[test]
fn text_field_editing_with_ime_submits_on_enter() {
    let mut f = fixture(&[]);
    f.click("status.snap");
    assert_eq!(f.state.focus(), f.sym("status.snap"));
    let mut ev = InputEvents::default();
    ev.push(InputEvent::Key {
        chord: Chord::parse("End").unwrap(),
    });
    ev.push(InputEvent::Key {
        chord: Chord::parse("BackSpace").unwrap(),
    });
    ev.push_preedit("ｇ");
    ev.push_preedit("");
    ev.push_text("5");
    ev.push(InputEvent::Key {
        chord: Chord::parse("Ctrl+S").unwrap(),
    }); // swallowed while editing
    ev.push(InputEvent::Key {
        chord: Chord::parse("Enter").unwrap(),
    });
    let out = f.route(&ev);
    assert_eq!(f.commands(&out), ["view.set_snap"]);
    assert_eq!(out.commands.arg(0), "15");
    assert!(f.state.focus().is_none() && f.state.editor().is_none());
}

/// A text area grows with its lines up to `AREA_EM_MAX` font sizes, then scrolls inside itself:
/// the wheel over it scrolls it, a click lands on the scrolled line, and caret moves keep the
/// caret in view. One that fits leaves the wheel alone.
#[test]
fn text_area_scrolls_inside_itself() {
    let long: Vec<String> = (0..60).map(|k| format!("line {k}")).collect();
    let src = format!(
        "node area.box text_area\n  parent window\n  value \"{}\"\nnode area.short text_area\n  parent window\n  value \"one\\ntwo\"\n",
        long.join("\\n")
    );
    let mut f = fixture(&[src.as_str()]);
    let (node, r) = (f.node("area.box"), f.row("area.box"));
    let style = |p| {
        let v = schemify_ui::row_style(&f.tables, f.frame.title_style(), node, Shown::Normal, p);
        v as f32
    };
    let (font, pad_y) = (style(StyleProp::Font).max(8.0), style(StyleProp::PadY));
    let rect = f.frame.rect()[r];
    assert!(
        rect.h <= font * schemify_ui::AREA_EM_MAX + 2.0 * pad_y + 0.5,
        "capped: {rect:?}"
    );
    let max = schemify_ui::area_scroll_max(&f.tables, &f.frame, &f.text, &f.state, r);
    assert!(max > 100.0, "60 lines overflow the box: {max}");
    let id = f.sym("area.box");
    let wheel = |x: f32, y: f32| {
        let mut ev = InputEvents::default();
        ev.push(InputEvent::Wheel {
            x,
            y,
            dx: 0.0,
            dy: -2.0,
        });
        ev
    };
    let (cx, cy) = f.center("area.box");
    f.route(&wheel(cx, cy));
    assert_eq!(f.state.scroll_of(id), 2.0 * UiState::WHEEL_PX);
    // A click lands on the line under the pointer in the scrolled text.
    f.click("area.box");
    let caret = f.state.editor().unwrap().cursor_geometry(1.0).unwrap();
    let view = rect.h - 2.0 * pad_y;
    let off = f.state.scroll_of(id);
    assert!(caret.y0 as f32 >= off - 1.0 && caret.y1 as f32 <= off + view + 1.0);
    assert!(
        caret.y0 as f32 > 2.0 * UiState::WHEEL_PX,
        "below the scrolled-out lines"
    );
    let mut ev = InputEvents::default();
    for _ in 0..40 {
        ev.push(InputEvent::Key {
            chord: Chord::parse("Down").unwrap(),
        });
    }
    f.route(&ev);
    let caret = f.state.editor().unwrap().cursor_geometry(1.0).unwrap();
    let off = f.state.scroll_of(id);
    assert!(
        off > 2.0 * UiState::WHEEL_PX,
        "followed the caret down: {off}"
    );
    assert!(caret.y1 as f32 <= off + view + 1.0 && caret.y0 as f32 >= off - 1.0);
    // Negative space: an area that fits does not scroll.
    let short = f.row("area.short");
    assert_eq!(
        schemify_ui::area_scroll_max(&f.tables, &f.frame, &f.text, &f.state, short),
        0.0
    );
    let (sx, sy) = f.center("area.short");
    f.route(&wheel(sx, sy));
    assert_eq!(f.state.scroll_of(f.sym("area.short")), 0.0);
}

#[test]
fn dialog_is_modal_and_escape_closes_it() {
    let mut f = fixture(&[]);
    f.state.set_dialog(f.sym("dialog.enter_text"));
    f.relayout();
    let d = f.frame.rect()[f.frame.rows(Layer::Overlay).start];
    assert!(d.x > 0.0 && d.w > 300.0, "centered dialog");
    assert!(
        f.click("tb.wire").commands.is_empty(),
        "main window is blocked"
    );
    let out = f.click("dialog.enter_text.cancel");
    assert_eq!(f.commands(&out), ["dialog.close"]);
    let mut ev = InputEvents::default();
    ev.push(InputEvent::Key {
        chord: Chord::parse("Escape").unwrap(),
    });
    f.route(&ev);
    assert!(f.state.dialog().is_none());
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
