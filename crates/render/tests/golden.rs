//! Golden images on the CPU rasterizer (vello_cpu): a menu bar with an open menu, a dialog, a
//! canvas with 1k primitives, and a math expression. Re-bless with `SCHEMIFY_BLESS=1`.
//!
//! Tolerance: a pixel differs when any channel differs by more than 2; at most 0.1% of pixels may
//! differ (SIMD paths may round differently across CPUs). The GPU-vs-CPU comparison TESTING.md
//! asks for needs an adapter in CI and is not here yet.

use schemify_render::canvas::{self, Camera, CanvasPrims, CanvasScratch, Marks, Palette, PrimKind};
use schemify_render::vello::kurbo::{Affine, Rect};
use schemify_render::vello::peniko::Color;
use schemify_render::widgets::{UiScratch, draw_math, draw_ui};
use schemify_render::{CpuPainter, Painter};
use schemify_ui::{Kind, Layer, Strings, TextSystem, UiState, compose, layout, records};

mod common;
use common::thousand_prims;

fn check(name: &str, pixmap: schemify_render::Pixmap) {
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
        (pixmap.width(), pixmap.height()),
        "{name}: size"
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

struct Shell {
    strings: Strings,
    tables: schemify_ui::UiTables,
    text: TextSystem,
    state: UiState,
}

fn shell() -> Shell {
    let mut strings = Strings::new();
    let (base, rejects) = records::parse(schemify_ui::DEFAULT_UI, &mut strings);
    assert!(rejects.is_empty());
    let tables = compose(&strings, &base, &[]);
    Shell {
        strings,
        tables,
        text: TextSystem::new(),
        state: UiState::default(),
    }
}

/// Paints the whole shell (main layer, empty canvas, overlay) at `w × h`.
fn paint_shell(s: &mut Shell, w: u16, h: u16) -> schemify_render::Pixmap {
    let frame = layout(
        &s.tables,
        &s.strings,
        &mut s.text,
        &s.state,
        (f32::from(w), f32::from(h)),
    );
    let mut p = CpuPainter::new(w, h, 1.0);
    let mut ui = UiScratch::default();
    draw_ui(
        &mut p,
        &s.tables,
        &frame,
        &s.strings,
        &mut s.text,
        &s.state,
        Layer::Main,
        &[],
        &mut ui,
    );
    let canvas_node = s
        .tables
        .nodes
        .kind()
        .iter()
        .position(|&k| k == Kind::Canvas)
        .unwrap() as u16;
    let r = frame.rect()[frame.row_of(canvas_node).unwrap()];
    let view = Rect::new(
        f64::from(r.x),
        f64::from(r.y),
        f64::from(r.x + r.w),
        f64::from(r.y + r.h),
    );
    let cam = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 0.002,
    };
    let none = CanvasPrims::default();
    canvas::draw_canvas(
        &mut p,
        view,
        &cam,
        20_000,
        &none,
        &Palette::xschem_dark(),
        &none,
        &Marks::default(),
        &mut s.text,
        &mut CanvasScratch::default(),
    );
    draw_ui(
        &mut p,
        &s.tables,
        &frame,
        &s.strings,
        &mut s.text,
        &s.state,
        Layer::Overlay,
        &[],
        &mut ui,
    );
    p.finish(Color::BLACK)
}

#[test]
fn menubar_with_open_menu() {
    let mut s = shell();
    let chain = [
        s.strings.find("menu.file").unwrap(),
        s.strings.find("menu.file.image_export").unwrap(),
    ];
    s.state.open_menus(&chain);
    check("menu_file", paint_shell(&mut s, 800, 520));
}

#[test]
fn dialog() {
    let mut s = shell();
    s.state
        .set_dialog(s.strings.find("dialog.enter_text").unwrap());
    check("dialog_enter_text", paint_shell(&mut s, 800, 450));
}

#[test]
fn canvas_1k_primitives() {
    let prims = thousand_prims();
    let (w, h) = (640u16, 400u16);
    let view = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
    let mut cam = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 1.0,
    };
    cam.fit(view, [0, 0, 600_000, 380_000]);
    let mut marks = Marks::default();
    for i in 0..5 {
        marks.push(PrimKind::Line, i, 0xffff_ffff);
    }
    marks.push(PrimKind::Rect, 0, 0xff00_00ff);
    let mut p = CpuPainter::new(w, h, 1.0);
    let mut text = TextSystem::new();
    canvas::draw_canvas(
        &mut p,
        view,
        &cam,
        20_000,
        &prims,
        &Palette::xschem_dark(),
        &CanvasPrims::default(),
        &marks,
        &mut text,
        &mut CanvasScratch::default(),
    );
    check("canvas_1k", p.finish(Color::BLACK));
}

#[test]
fn math_expression() {
    let mut strings = Strings::new();
    let mut text = TextSystem::new();
    let src = strings
        .intern(r"g_m = \frac{2 I_D}{V_{GS} - V_{th}} \approx \sqrt{2 \mu C_{ox} \frac{W}{L} I_D}");
    let id = text.math(&strings, src, 28).expect("supported LaTeX");
    let list = text.math_list(id).clone();
    assert_eq!(list.missing, 0);
    let (w, h) = (
        ((list.width + 24.0).ceil()) as u16,
        ((list.height + list.depth + 24.0).ceil()) as u16,
    );
    let mut p = CpuPainter::new(w, h, 1.0);
    p.fill_rect(
        Rect::new(0.0, 0.0, f64::from(w), f64::from(h)),
        Color::WHITE,
    );
    draw_math(
        &mut p,
        &list,
        text.math_fonts(),
        Affine::translate((12.0, 12.0)),
        Color::BLACK,
        &mut UiScratch::default(),
    );
    check("math", p.finish(Color::WHITE));
}
