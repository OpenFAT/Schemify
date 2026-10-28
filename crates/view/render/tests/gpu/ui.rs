//! The UI on the GPU against the CPU: text at fractional positions and two device scales,
//! prose and math, the whole shell, and retained UI rows against fresh frames.

use super::*;

// ---- private ----

/// Shapes `s` at `px` into a fresh layout.
fn shaped(text: &mut TextSystem, s: &str, px: f32) -> schemify_ui::parley::Layout<()> {
    let mut l = schemify_ui::parley::Layout::new();
    text.shape_into(s, px, &mut l);
    l
}

/// UI text: runs at the UI's sizes at fractional positions, at device scale 1 and 1.5;
/// mean |Δ| ≤ [`TEXT_MEAN_MAX`]. Then at quarter-pixel positions (no phase error) every pixel
/// within [`EXACT_LEVELS`] — the masks are the CPU's own pixels.
#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_ui_text_matches_cpu() {
    let mut g = gpu();
    let mut text = TextSystem::new();
    let lines = [
        ("File  Edit  View  Tools  Help", 13.0),
        ("Instance properties: value = 10k, W = 2u", 12.0),
        ("Simulation finished in 1.23 ms — 0 errors", 11.0),
        ("Schematic Editor", 16.0),
        ("gm/ID = 15 S/A; VDS,sat ≈ 2/(gm/ID)", 14.0),
    ];
    let layouts: Vec<_> = lines
        .iter()
        .map(|&(s, px)| shaped(&mut text, s, px))
        .collect();
    let at = |i: usize| (7.3 + 1.37 * i as f64, 6.0 + 23.6 * i as f64);
    let white = Color::from_rgba8(230, 230, 230, 255);
    for scale in [1.0, 1.5] {
        let size = ((420.0 * scale) as u32, (130.0 * scale) as u32);
        let (got, _) = gpu_frame(&mut g, size, scale, Color::BLACK, |p| {
            let mut s = UiScratch::default();
            for (i, l) in layouts.iter().enumerate() {
                draw_text(p, l, at(i).0, at(i).1, white, &mut s);
            }
        });
        let want = cpu_frame(size, scale, Color::BLACK, |p| {
            let mut s = UiScratch::default();
            for (i, l) in layouts.iter().enumerate() {
                draw_text(p, l, at(i).0, at(i).1, white, &mut s);
            }
        });
        if scale == 1.0 {
            amplified_diff("ui_text", &got, &want, size);
        }
        let name = format!("UI text at scale {scale}");
        assert!(within_mean(&name, &got, &want, TEXT_MEAN_MAX), "{name}");
    }
    // Quarter-pixel origins: the glyph positions parley gives are whole at these sizes only
    // where they are, so draw single glyphs at exact quarters.
    let l = &layouts[0];
    let (font, ids) = l
        .lines()
        .flat_map(|ln| ln.items())
        .find_map(|i| match i {
            schemify_ui::parley::PositionedLayoutItem::GlyphRun(r) => Some((
                r.run().font().clone(),
                r.positioned_glyphs().map(|g| g.id).collect::<Vec<_>>(),
            )),
            _ => None,
        })
        .expect("a run");
    let glyphs: Vec<Glyph> = ids
        .iter()
        .enumerate()
        .map(|(k, &id)| Glyph {
            id,
            x: 9.0 * k as f32 + 0.25 * (k % 4) as f32,
            y: 14.0,
        })
        .collect();
    let size = (300u32, 24u32);
    let (got, _) = gpu_frame(&mut g, size, 1.0, Color::BLACK, |p| {
        p.glyphs(
            &font,
            13.0,
            &[],
            white,
            Affine::translate((4.0, 2.0)),
            &glyphs,
        )
    });
    let want = cpu_frame(size, 1.0, Color::BLACK, |p| {
        p.glyphs(
            &font,
            13.0,
            &[],
            white,
            Affine::translate((4.0, 2.0)),
            &glyphs,
        )
    });
    let (mean, off) = stats(&got, &want, EXACT_LEVELS);
    println!("glyphs at quarter pixels: mean |Δ| {mean:.3}, {off} pixels off by > {EXACT_LEVELS}");
    assert_eq!(off, 0, "a mask at its own phase is the CPU's pixels");
    // Negative space: a wrong glyph, and the run shifted a pixel, fail the text oracle.
    let mut wrong = glyphs.clone();
    wrong[3].id = wrong[5].id;
    let (bad, _) = gpu_frame(&mut g, size, 1.0, Color::BLACK, |p| {
        p.glyphs(
            &font,
            13.0,
            &[],
            white,
            Affine::translate((4.0, 2.0)),
            &wrong,
        )
    });
    assert!(
        !within_mean("a wrong glyph", &bad, &want, TEXT_MEAN_MAX)
            || stats(&bad, &want, FAR_LEVELS).1 > 0,
        "a wrong glyph passes the oracle"
    );
    assert!(
        stats(&bad, &want, FAR_LEVELS).1 > 0,
        "a wrong glyph has wrong pixels"
    );
    let (shifted, _) = gpu_frame(&mut g, size, 1.0, Color::BLACK, |p| {
        p.glyphs(
            &font,
            13.0,
            &[],
            white,
            Affine::translate((5.0, 2.0)),
            &glyphs,
        )
    });
    assert!(
        stats(&shifted, &want, FAR_LEVELS).1 > 0,
        "a run shifted a pixel passes the oracle"
    );
}

/// Prose (headings, emphasis, code shades, a rule, a list, inline and display math, an
/// image) and a formula on its own: mean |Δ| ≤ [`TEXT_MEAN_MAX`].
#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_prose_and_math_match_cpu() {
    let mut g = gpu();
    let mut strings = Strings::new();
    let mut text = TextSystem::new();
    let rgba: Vec<u8> = (0..32 * 16)
        .flat_map(|i| [(i * 7 % 256) as u8, (i * 3 % 256) as u8, 128, 255])
        .collect();
    assert!(text.set_image(schemify_ui::Image::new("dots.png", 32, 16, rgba)));
    let src = strings.intern(
        "# Common source\n\nThe gain is $A_v = -g_m R_D$ for *small* signals, with `gm_id` \
         from the **lookup**.\n\n- bias at $V_{ov} = 0.2$ V\n- check $\\frac{g_m}{I_D}$\n\n---\n\n\
         $$f_T = \\frac{g_m}{2 \\pi C_{gg}}$$\n\n![dots](dots.png)\n\n```\n.op\n.tran 1n 1u\n```\n",
    );
    let prose = text.prose(&strings, src, 13, 380);
    let formula = strings
        .intern(r"g_m = \frac{2 I_D}{V_{GS} - V_{th}} \approx \sqrt{2 \mu C_{ox} \frac{W}{L} I_D}");
    let math = text.math(&strings, formula, 22).expect("supported LaTeX");
    let size = (420u32, 520u32);
    let fg = Color::from_rgba8(30, 30, 30, 255);
    let page = Rect::new(0.0, 0.0, 420.0, 520.0);
    let t = &text;
    macro_rules! page {
        ($p:expr) => {{
            let p = $p;
            p.fill_rect(page, Color::WHITE);
            let mut s = UiScratch::default();
            draw_prose(p, prose, t, (12.0, 10.0), fg, &mut s);
            let at = Affine::translate((14.0, 440.0));
            draw_math(p, t.math_list(math), t.math_fonts(), at, fg, &mut s);
        }};
    }
    let (gpu_px, _) = gpu_frame(&mut g, size, 1.0, Color::WHITE, |p| page!(p));
    let cpu_px = cpu_frame(size, 1.0, Color::WHITE, |p| page!(p));
    amplified_diff("prose_math", &gpu_px, &cpu_px, size);
    assert!(within_mean(
        "prose and math",
        &gpu_px,
        &cpu_px,
        TEXT_MEAN_MAX
    ));
}

/// The default UI shell (menu bar, tabs, panels, status bar) with an open menu, and with a
/// dialog: every widget (rects, borders, separators, check marks, dropdown arrows, icons,
/// labels) through the batch against the CPU painter.
#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_ui_shell_matches_cpu() {
    let mut g = gpu();
    let mut strings = Strings::new();
    let (base, rejects) = records::parse(schemify_ui::DEFAULT_UI, &mut strings);
    assert!(rejects.is_empty());
    let tables = compose(&strings, &base, &[]);
    let mut text = TextSystem::new();
    for (k, open) in [true, false].into_iter().enumerate() {
        let mut state = UiState::default();
        if open {
            let chain = [
                strings.find("menu.file").expect("menu"),
                strings.find("menu.file.image_export").expect("submenu"),
            ];
            state.open_menus(&chain);
        } else {
            state.set_dialog(strings.find("dialog.enter_text").expect("dialog"));
        }
        let size = (800u32, 520u32);
        let frame = layout(&tables, &strings, &mut text, &state, (800.0, 520.0));
        let canvas = tables
            .nodes
            .kind()
            .iter()
            .position(|&k| k == Kind::Canvas)
            .expect("a canvas") as u16;
        let r = frame.rect()[frame.row_of(canvas).expect("laid out")];
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
        macro_rules! shell {
            ($p:expr) => {{
                let p = $p;
                let mut ui = UiScratch::default();
                draw_ui(
                    p,
                    &tables,
                    &frame,
                    &strings,
                    &mut text,
                    &state,
                    Layer::Main,
                    &mut ui,
                );
                canvas_scene(p, view, view, cam, &none, &Marks::default(), &none, None);
                draw_ui(
                    p,
                    &tables,
                    &frame,
                    &strings,
                    &mut text,
                    &state,
                    Layer::Overlay,
                    &mut ui,
                );
            }};
        }
        let (got, _) = gpu_frame(&mut g, size, 1.0, Color::BLACK, |p| shell!(p));
        let want = cpu_frame(size, 1.0, Color::BLACK, |p| shell!(p));
        let name = if open {
            "UI shell with a menu"
        } else {
            "UI shell with a dialog"
        };
        if k == 0 {
            amplified_diff("ui_shell", &got, &want, size);
        }
        assert!(within_mean(name, &got, &want, TEXT_MEAN_MAX), "{name}");
    }
}

/// The UI's retained rows (`render::ui_rows`) draw what a frame from scratch draws: one kept
/// batch (and its own pipelines, as the window keeps them) through a sequence of UI states (a
/// menu over canvas labels, a hover, a chooser dialog with a 300-row list scrolled by whole
/// and fractional offsets, the menu closed) is pixel-identical, frame by frame, to a fresh
/// batch on fresh pipelines.
#[test]
#[ignore = "needs a GPU adapter"]
fn retained_ui_frames_equal_fresh_frames() {
    let (mut kept_gpu, mut fresh_gpu) = (gpu(), gpu());
    let mut strings = Strings::new();
    let (base, rejects) = records::parse(schemify_ui::DEFAULT_UI, &mut strings);
    assert!(rejects.is_empty());
    let rows: String = (0..300)
        .map(|k| format!("node row.{k} item\n  parent dialog.choose.list\n  label \"Row {k}\"\n  style list-row\n"))
        .collect();
    let (extra, rejects) = records::parse(&rows, &mut strings);
    assert!(rejects.is_empty());
    let name = strings.intern("test");
    let tables = compose(&strings, &base, &[(name, &extra)]);
    let mut text = TextSystem::new();
    let mut state = UiState::default();
    let size = (800u32, 520u32);
    let labels = labels_scene();
    let (mut kept, mut kept_rows) = (Batch::default(), InstanceRows::default());
    let list = strings.find("dialog.choose.list").expect("the list");
    for (step, scale) in [
        (0, 1.0),
        (1, 1.0),
        (2, 1.0),
        (3, 1.0),
        (4, 1.0),
        (5, 1.0),
        (6, 1.0),
        (7, 1.5),
        (8, 1.5),
    ] {
        match step {
            1 => state.open_menus(&[strings.find("menu.file").expect("menu")]),
            2 => state.open_menus(&[
                strings.find("menu.file").expect("menu"),
                strings.find("menu.file.image_export").expect("submenu"),
            ]),
            3 => state.set_dialog(strings.find("dialog.choose").expect("dialog")),
            4 => state.set_scroll(list, 280.0, 1.0e6),
            5 => state.set_scroll(list, 401.5, 1.0e6),
            6 => state.set_scroll(list, 120.0, 1.0e6),
            8 => state.set_dialog(schemify_ui::Sym::NONE),
            _ => {}
        }
        let frame = layout(
            &tables,
            &strings,
            &mut text,
            &state,
            (800.0 / scale as f32, 520.0 / scale as f32),
        );
        let canvas = tables
            .nodes
            .kind()
            .iter()
            .position(|&k| k == Kind::Canvas)
            .expect("a canvas") as u16;
        let r = frame.rect()[frame.row_of(canvas).expect("laid out")];
        let view = Rect::new(
            f64::from(r.x),
            f64::from(r.y),
            f64::from(r.x + r.w),
            f64::from(r.y + r.h),
        );
        let cam = fit(view);
        let mut ui = UiScratch::default();
        let mut paint = |p: &mut GpuPainter<'_>| {
            draw_ui(
                p,
                &tables,
                &frame,
                &strings,
                &mut text,
                &state,
                Layer::Main,
                &mut ui,
            );
            canvas_scene(
                p,
                view,
                view,
                cam,
                &labels,
                &Marks::default(),
                &CanvasPrims::default(),
                None,
            );
            draw_ui(
                p,
                &tables,
                &frame,
                &strings,
                &mut text,
                &state,
                Layer::Overlay,
                &mut ui,
            );
        };
        let got = gpu_frame_in(
            &mut kept_gpu,
            &mut kept,
            &mut kept_rows,
            size,
            scale,
            Color::BLACK,
            &mut paint,
        );
        let (want, _) = gpu_frame(&mut fresh_gpu, size, scale, Color::BLACK, &mut paint);
        let off = got.iter().zip(&want).filter(|(a, b)| a != b).count();
        if off > 0 {
            amplified_diff(&format!("retained_{step}"), &got, &want, size);
        }
        assert_eq!(off, 0, "step {step}: the kept frame is the fresh frame");
        let draws = kept.draws().len();
        eprintln!(
            "step {step}: {draws} draws, {} retained",
            kept.draws()
                .iter()
                .filter(|d| matches!(d.what, schemify_render::batch::What::Retained(_)))
                .count()
        );
    }
}
