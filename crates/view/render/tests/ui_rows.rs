//! The UI's retained rows (`render::ui_rows`), headless: what a frame writes for the GPU when
//! the UI did not change (nothing), when one menu item hovers (that item's instances), when one
//! character is typed (that field's glyph and caret), when a menu opens and closes (its slots
//! taken, then given back), when a 1000-row list scrolls (no row, one pane entry), and that the
//! atlas never evicts a mask a kept row draws. The oracle for each "kept" frame is a frame
//! painted from scratch on the same atlas: the instances it draws, in order, must be the same.

use schemify_render::atlas::Atlas;
use schemify_render::batch::{STRIDE, What};
use schemify_render::kurbo::Affine;
use schemify_render::ui_rows::UiRows;
use schemify_render::widgets::{UiScratch, draw_ui};
use schemify_render::{Batch, GpuPainter};
use schemify_ui::{
    InputEvent, InputEvents, Layer, Routed, Strings, Sym, TextSystem, UiLayout, UiState, UiTables,
    compose, layout, records, route_input,
};

const VIEW: (f32, f32) = (1280.0, 800.0);

struct Ui {
    strings: Strings,
    tables: UiTables,
    text: TextSystem,
    state: UiState,
    frame: UiLayout,
    scratch: UiScratch,
    batch: Batch,
}

// ---- private ----

impl Ui {
    fn new(plugin: &str) -> Ui {
        let mut strings = Strings::new();
        let (base, rejects) = records::parse(schemify_ui::DEFAULT_UI, &mut strings);
        assert!(rejects.is_empty());
        let (extra, rejects) = records::parse(plugin, &mut strings);
        assert!(rejects.is_empty(), "{rejects:?}");
        let name = strings.intern("test");
        let tables = compose(&strings, &base, &[(name, &extra)]);
        let mut text = TextSystem::new();
        let state = UiState::default();
        let frame = layout(&tables, &strings, &mut text, &state, VIEW);
        Ui {
            strings,
            tables,
            text,
            state,
            frame,
            scratch: UiScratch::default(),
            batch: Batch::default(),
        }
    }

    fn sym(&self, s: &str) -> Sym {
        self.strings.find(s).expect("interned")
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

    fn route(&mut self, ev: &InputEvents) {
        let mut out = Routed::default();
        route_input(
            &self.tables,
            &self.frame,
            &self.strings,
            &mut self.text,
            &mut self.state,
            ev,
            &mut out,
        );
        self.relayout();
    }

    fn center(&self, s: &str) -> (f32, f32) {
        let n = self.tables.nodes.find(self.sym(s)).expect("composed");
        let r = self.frame.rect()[self.frame.row_of(n).expect("laid out")];
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    /// Paints both layers as the window does, then returns the UI bytes the frame left to
    /// upload and takes them as the upload does.
    fn paint(&mut self) -> usize {
        self.batch.begin_frame();
        let mut p = GpuPainter {
            batch: &mut self.batch,
            base: Affine::IDENTITY,
            rows: None,
        };
        for layer in [Layer::Main, Layer::Overlay] {
            draw_ui(
                &mut p,
                &self.tables,
                &self.frame,
                &self.strings,
                &mut self.text,
                &self.state,
                layer,
                &mut self.scratch,
            );
        }
        let n = self.batch.ui.dirty_bytes();
        self.batch.ui.clear_dirty();
        n
    }

    /// The instances the frame draws, in drawing order, empty slots left out: a kept frame
    /// and a frame from scratch (fresh rows, the same atlas) must agree on every one.
    fn drawn(&self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for d in self.batch.draws() {
            let bytes = match d.what {
                What::Retained(k) => self.batch.ui.bytes(k),
                What::Ui | What::Image(_) => self.batch.instances(),
                What::Canvas(_) => continue,
            };
            let r = d.range.start as usize * STRIDE..d.range.end as usize * STRIDE;
            for i in bytes[r].chunks_exact(STRIDE) {
                if i.iter().any(|&b| b != 0) {
                    out.push(i.to_vec());
                }
            }
        }
        out
    }

    /// Paints until no glyph is drawn from a stand-in (as the window does), returning the UI
    /// bytes written in all.
    fn settle(&mut self) -> usize {
        let mut n = self.paint();
        for _ in 0..64 {
            if self.batch.settled() {
                return n;
            }
            n += self.paint();
        }
        panic!("the UI settles");
    }

    /// Asserts this frame's kept instances equal a frame painted from scratch.
    fn assert_fresh(&mut self, what: &str) {
        let kept = self.drawn();
        let rows = std::mem::take(&mut self.batch.ui);
        self.settle();
        assert!(
            kept == self.drawn(),
            "{what}: the kept frame equals a fresh one"
        );
        self.batch.ui = rows; // ponytail: the fresh rows' atlas pins stay counted (a test)
        self.paint();
    }

    /// The slots of the row showing node `s` in UI source `src`.
    fn slots(&self, src: usize, s: &str) -> std::ops::Range<usize> {
        let r = self
            .batch
            .ui
            .slots_of(src, self.sym(s))
            .expect("a kept row");
        r.start as usize * STRIDE..r.end as usize * STRIDE
    }
}

fn dirty_within(rows: &UiRows, src: usize, r: &std::ops::Range<usize>) -> bool {
    rows.dirty(src)
        .iter()
        .all(|d| d.start >= r.start && d.end <= r.end)
}

/// A frame whose UI did not change writes no UI byte and draws each source in one draw.
#[test]
fn an_unchanged_ui_writes_nothing() {
    let mut ui = Ui::new("");
    assert!(ui.settle() > 0, "the first frames write the rows");
    for k in 0..3 {
        assert_eq!(ui.paint(), 0, "frame {k} writes no UI byte");
    }
    let draws = ui
        .batch
        .draws()
        .iter()
        .filter(|d| matches!(d.what, What::Retained(_)));
    assert_eq!(
        draws.count(),
        2,
        "one draw per source (the main layer, the floats)"
    );
    assert_eq!(ui.batch.len(), 0, "no per-frame instance");
    ui.assert_fresh("unchanged");
}

/// Opening a menu takes the overlay's slots and hovering one item rewrites only that item's
/// instances; closing the menu gives the slots back.
#[test]
fn a_menu_hover_rewrites_only_its_item() {
    let mut ui = Ui::new("");
    ui.settle();
    let (floats, bytes) = (ui.batch.ui.rows(1), ui.batch.ui.bytes(1).len());
    let file = ui.sym("menu.file");
    ui.state.open_menus(&[file]);
    ui.relayout();
    assert!(ui.settle() > 0, "the menu's rows are written");
    let open = ui.batch.ui.rows(1);
    assert!(open > floats + 10, "{open} rows");
    assert!(ui.batch.ui.bytes(1).len() > bytes, "the menu takes slots");
    ui.assert_fresh("menu open");
    let (x, y) = ui.center("file.clear_symbol");
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerMove { x, y });
    ui.route(&ev);
    assert_eq!(ui.state.hover(), ui.sym("file.clear_symbol"));
    let item = ui.slots(1, "file.clear_symbol");
    let n = ui.paint_keep_dirty();
    assert!(n > 0, "the hover is written");
    assert!(
        dirty_within(&ui.batch.ui, 1, &item),
        "only the item's slots"
    );
    assert!(n <= item.len(), "{n} bytes, the item has {}", item.len());
    eprintln!(
        "hover: {} instances rewritten of the item's {}",
        n / STRIDE,
        item.len() / STRIDE
    );
    ui.batch.ui.clear_dirty();
    ui.assert_fresh("hover");
    ui.state.open_menus(&[]);
    ui.relayout();
    ui.paint();
    assert_eq!(ui.batch.ui.rows(1), floats, "the menu's rows are dropped");
    assert_eq!(
        ui.batch.ui.bytes(1).len(),
        bytes,
        "and their slots given back"
    );
}

impl Ui {
    /// As [`Ui::paint`], leaving the dirty ranges in place for the caller to look at.
    fn paint_keep_dirty(&mut self) -> usize {
        self.batch.begin_frame();
        let mut p = GpuPainter {
            batch: &mut self.batch,
            base: Affine::IDENTITY,
            rows: None,
        };
        for layer in [Layer::Main, Layer::Overlay] {
            draw_ui(
                &mut p,
                &self.tables,
                &self.frame,
                &self.strings,
                &mut self.text,
                &self.state,
                layer,
                &mut self.scratch,
            );
        }
        self.batch.ui.dirty_bytes()
    }
}

/// Typing one character rewrites only the field's instances: its new glyph and the caret.
#[test]
fn typing_rewrites_the_glyph_and_the_caret() {
    let mut ui = Ui::new("");
    ui.state.set_dialog(ui.sym("dialog.enter_text"));
    ui.relayout();
    let input = ui.sym("dialog.enter_text.input");
    assert!(
        ui.state
            .focus_text(&ui.tables, &ui.frame, &ui.strings, &mut ui.text, input)
    );
    ui.relayout();
    ui.settle();
    for (k, c) in ["a", "b", "c"].into_iter().enumerate() {
        let mut ev = InputEvents::default();
        ev.push_text(c);
        ui.route(&ev);
        let field = ui.slots(1, "dialog.enter_text.input");
        let n = ui.paint_keep_dirty();
        assert!(
            dirty_within(&ui.batch.ui, 1, &field),
            "char {k}: only the field"
        );
        assert_eq!(n, 2 * STRIDE, "char {k}: the glyph and the caret");
        ui.batch.ui.clear_dirty();
        ui.assert_fresh("typed");
    }
}

/// A 1000-row list scrolls without writing a row: the rows stay in the pane's content
/// coordinates and the pane's offset moves.
#[test]
fn scrolling_a_long_list_writes_no_row() {
    let rows: String = (0..1000)
        .map(|k| format!("node row.{k} item\n  parent dialog.choose.list\n  label \"Row {k}\"\n  style list-row\n"))
        .collect();
    let mut ui = Ui::new(&rows);
    ui.state.set_dialog(ui.sym("dialog.choose"));
    ui.relayout();
    let first = ui.settle();
    assert!(first > 1000 * STRIDE, "every row is kept, in view or not");
    let list = ui.sym("dialog.choose.list");
    let node = ui.tables.nodes.find(list).expect("the list");
    let max = ui.frame.scroll_max(node).expect("a scroll node");
    assert!(max > 10_000.0, "the list scrolls ({max} px)");
    let mut panes = ui.batch.ui.panes();
    for k in 1..=10 {
        ui.state.set_scroll(list, 40.0 * k as f32 * 7.0, max);
        ui.relayout();
        assert_eq!(ui.paint(), 0, "scroll {k} writes no row");
        assert_ne!(ui.batch.ui.panes(), panes, "scroll {k} moves the pane");
        panes = ui.batch.ui.panes();
    }
    ui.assert_fresh("scrolled");
}

/// The atlas never evicts a shelf a kept row draws from: with a small atlas flooded by
/// per-frame glyphs at many sizes (evicting), every mask a kept row names is still the
/// atlas's mask, and the kept frame equals a fresh one.
#[test]
fn eviction_spares_the_masks_kept_rows_draw() {
    let mut ui = Ui::new("");
    ui.batch.atlas = Atlas::new(256);
    ui.settle();
    let kept = |ui: &Ui| -> Vec<[u16; 4]> {
        ui.batch
            .ui
            .bytes(0)
            .chunks_exact(STRIDE)
            .filter(|i| {
                let k = u32::from_ne_bytes(i[52..56].try_into().unwrap()) & 0xff;
                k == schemify_render::batch::MASK
            })
            .map(|i| {
                let f = |o: usize| f32::from_ne_bytes(i[o..o + 4].try_into().unwrap()) as u16;
                [f(16), f(20), f(24) - f(16), f(28) - f(20)]
            })
            .collect()
    };
    let masks = kept(&ui);
    assert!(masks.len() > 50, "the UI draws glyph masks");
    for m in &masks {
        assert!(
            ui.batch.atlas.pins(m[1]) > 0,
            "a kept mask's shelf is pinned"
        );
    }
    let evictions = ui.batch.atlas.evictions;
    let mut layout = schemify_ui::parley::Layout::new();
    for px in 0..120 {
        ui.batch.begin_frame();
        let s = "The quick brown fox jumps over the lazy dog 0123456789";
        ui.text.shape_into(s, 9.0 + px as f32 * 0.37, &mut layout);
        let mut p = GpuPainter {
            batch: &mut ui.batch,
            base: Affine::IDENTITY,
            rows: None,
        };
        let mut scratch = UiScratch::default();
        schemify_render::widgets::draw_text(
            &mut p,
            &layout,
            10.0,
            10.0,
            schemify_render::peniko::Color::WHITE,
            &mut scratch,
        );
    }
    assert!(
        ui.batch.atlas.evictions > evictions,
        "the flood evicted shelves"
    );
    let held: Vec<_> = ui
        .batch
        .atlas
        .entries()
        .map(|(_, s)| [s.x, s.y, s.w, s.h])
        .collect();
    for m in &masks {
        assert!(held.contains(m), "kept mask {m:?} survives");
    }
    assert_eq!(ui.paint(), 0, "the kept rows need no repaint");
    ui.assert_fresh("after the flood");
}

/// The CPU painter keeps its rows as recorded calls under the same keys: across a sequence of
/// UI states (a menu, a hover, a dialog, typing, a scroll), each frame of one kept painter is
/// byte-identical to a fresh painter's, and the kept painter records only the changed rows.
#[test]
fn the_cpu_painter_replays_kept_rows_byte_identically() {
    let rows: String = (0..200)
        .map(|k| format!("node row.{k} item\n  parent dialog.choose.list\n  label \"Row {k}\"\n  style list-row\n"))
        .collect();
    let mut ui = Ui::new(&rows);
    let (w, h) = (VIEW.0 as u16, VIEW.1 as u16);
    let mut kept = schemify_render::CpuPainter::new(w, h, 1.0);
    let frame = |ui: &mut Ui, p: &mut schemify_render::CpuPainter| {
        p.reset();
        for layer in [Layer::Main, Layer::Overlay] {
            draw_ui(
                p,
                &ui.tables,
                &ui.frame,
                &ui.strings,
                &mut ui.text,
                &ui.state,
                layer,
                &mut ui.scratch,
            );
        }
        p.finish(schemify_render::peniko::Color::BLACK)
            .data_as_u8_slice()
            .to_vec()
    };
    let mut total = 0;
    for step in 0..6 {
        match step {
            1 => ui.state.open_menus(&[ui.sym("menu.file")]),
            2 => {
                let (x, y) = ui.center("file.clear_symbol");
                let mut ev = InputEvents::default();
                ev.push(InputEvent::PointerMove { x, y });
                ui.route(&ev);
            }
            3 => ui.state.set_dialog(ui.sym("dialog.choose")),
            4 => {
                let list = ui.sym("dialog.choose.list");
                ui.state.set_scroll(list, 120.0, 1.0e6);
            }
            _ => {}
        }
        ui.relayout();
        let before = kept.recorded;
        let got = frame(&mut ui, &mut kept);
        let want = frame(&mut ui, &mut schemify_render::CpuPainter::new(w, h, 1.0));
        assert!(
            got == want,
            "step {step}: the kept painter's frame is the fresh one's"
        );
        let n = kept.recorded - before;
        total += ui.frame.len();
        if step == 5 {
            assert_eq!(n, 0, "an unchanged frame records nothing");
        }
        if step == 2 {
            assert!(n <= 2, "a hover records its row ({n})");
        }
    }
    assert!(kept.recorded < total as u64 / 2, "most rows replay");
}
