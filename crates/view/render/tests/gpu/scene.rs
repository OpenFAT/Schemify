//! The canvas on the GPU against the CPU: each primitive kind, the marks, the far-zoom cut,
//! a mixed scene under the UI, the labels at several zooms and the 100k scene, with a wrong
//! instance as the negative space.

use super::*;

// ---- private ----

/// Plain and styled (weights, dashes) segments on three layers.
fn lines_scene() -> CanvasPrims {
    let mut rnd = seeded(0x1234_5678);
    let mut p = CanvasPrims::default();
    for i in 0..90 {
        let (x, y) = (rnd(560) * 1000 + 20_000, rnd(340) * 1000 + 20_000);
        let (dx, dy) = ((rnd(120) - 60) * 1000, (rnd(120) - 60) * 1000);
        let layer = [1u8, 4, 5][rnd(3) as usize];
        if i % 3 == 0 {
            let style = LineStyle::new(rnd(4) as u8 + 1, [0, 0, 3, 7][rnd(4) as usize]);
            p.line_styled(x, y, x + dx, y + dy, layer, style);
        } else {
            p.line(x, y, x + dx, y + dy, layer);
        }
    }
    p.line(30_000, 200_000, 570_000, 200_000, 1); // axis-aligned: exactly one pixel row
    p.line(300_000, 40_000, 300_000, 340_000, 4);
    p
}

/// Outlined, filled (layer 5 and by style), styled and dashed rectangles.
fn rects_scene() -> CanvasPrims {
    let mut rnd = seeded(0x9e37_79b9);
    let mut p = CanvasPrims::default();
    for i in 0..60 {
        let (x, y) = (rnd(520) * 1000 + 20_000, rnd(300) * 1000 + 20_000);
        let (w, h) = (rnd(50) * 1000 + 3_000, rnd(40) * 1000 + 3_000);
        match i % 4 {
            0 => p.rect(x, y, x + w, y + h, 4),
            1 => p.rect(x, y, x + w, y + h, 5),
            2 => p.rect_styled(x, y, x + w, y + h, 4, LineStyle::new(2, 0).filled()),
            _ => p.rect_styled(x, y, x + w, y + h, 1, LineStyle::new(1, 4)),
        }
    }
    p
}

/// Full circles, partial arcs either way round, filled circles, dashed rings.
fn arcs_scene() -> CanvasPrims {
    let mut rnd = seeded(0x2545_f491);
    let mut p = CanvasPrims::default();
    for i in 0..70 {
        let (x, y) = (rnd(520) * 1000 + 40_000, rnd(300) * 1000 + 40_000);
        let r = rnd(25) * 1000 + 3_000;
        match i % 5 {
            0 => p.arc(x, y, r, 0, 360_000, 4),
            1 => p.arc(x, y, r, rnd(360) * 1000, rnd(300) * 1000 + 20_000, 4),
            2 => p.arc(x, y, r, rnd(360) * 1000, -(rnd(300) * 1000 + 20_000), 1),
            3 => p.arc(x, y, r, 0, 360_000, 5),
            _ => p.arc_styled(x, y, r, 0, 360_000, 4, LineStyle::new(2, 3)),
        }
    }
    p
}

/// Open and closed chains, convex and concave, filled by layer and by style, one styled.
fn polys_scene() -> CanvasPrims {
    let mut rnd = seeded(0x0bad_cafe);
    let mut p = CanvasPrims::default();
    for i in 0..50 {
        let (x, y) = (rnd(500) * 1000 + 30_000, rnd(280) * 1000 + 30_000);
        let s = rnd(30) * 1000 + 10_000;
        let pts = [
            (x, y),
            (x + s, y + rnd(20) * 1000),
            (x + 2 * s, y),
            (x + s + s / 2, y - s),
            (x + s / 2, y - s / 3),
        ];
        match i % 4 {
            0 => p.poly(&pts[..4], false, 1),
            1 => p.poly(&pts, true, 4),
            2 => p.poly(&pts, true, 5),
            _ => p.poly_styled(&pts[..4], true, 4, LineStyle::new(2, 0).filled()),
        }
    }
    p
}

#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_lines_match_cpu() {
    assert!(kind_matches("lines", &lines_scene(), &Marks::default()));
}

#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_rects_match_cpu() {
    assert!(kind_matches("rects", &rects_scene(), &Marks::default()));
}

#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_arcs_match_cpu() {
    assert!(kind_matches("arcs", &arcs_scene(), &Marks::default()));
}

#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_polys_match_cpu() {
    assert!(kind_matches("polys", &polys_scene(), &Marks::default()));
}

/// Marks of every kind over the mixed thousand (the marked rows drawn again, 2 px, in the
/// mark colour).
#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_marks_match_cpu() {
    let prims = common::thousand_prims();
    let mut marks = Marks::default();
    for i in 0..40 {
        marks.push(PrimKind::Line, i * 13, 0xffff_00ff);
    }
    for i in 0..10 {
        marks.push(PrimKind::Rect, i * 11, 0xff80_00ff);
        marks.push(PrimKind::Arc, i * 7, 0xffff_00ff);
    }
    if !prims.polylines.end.is_empty() {
        marks.push(PrimKind::Polyline, 0, 0xffff_00ff);
    }
    if !prims.polygons.end.is_empty() {
        marks.push(PrimKind::Polygon, 0, 0xffff_00ff);
    }
    let mut no_text = prims.clone();
    no_text.texts = Default::default();
    assert!(kind_matches("marks", &no_text, &marks));
}

/// Negative space: one rect of the rects scene drawn 60 px away (a wrong instance) fails the
/// oracle the rects pass.
#[test]
#[ignore = "needs a GPU adapter"]
fn a_wrong_instance_fails_the_oracle() {
    let prims = rects_scene();
    let mut wrong = prims.clone();
    // Rect 5 is a filled pin rect (`i % 4 == 1`): moved, it leaves and covers whole areas.
    wrong.rects.x0[5] += 60_000;
    wrong.rects.x1[5] += 60_000;
    let mut g = gpu();
    let size = (640u32, 400u32);
    let view = Rect::new(40.0, 30.0, 600.0, 380.0);
    let cam = fit(view);
    let mut rows = InstanceRows::default();
    rows.pack(0, &wrong, &Palette::xschem_dark(), Change::All);
    let (got, _) = canvas_pass(&mut g, &mut rows, &frame_of(view, cam), size, Color::BLACK);
    let want = cpu_canvas(&prims, &Marks::default(), view, &cam, size, Color::BLACK);
    assert!(
        !within("one rect moved", &got, &want),
        "a wrong instance passes the oracle"
    );
}

/// The far-zoom cut: rows at most 2 px across are the whole pixels they touch on both sides,
/// rows just over it are strokes; both agree at the threshold.
#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_lod_cut_matches_cpu_at_the_threshold() {
    let mut g = gpu();
    let size = (640u32, 400u32);
    let view = Rect::new(0.0, 0.0, 640.0, 400.0);
    // 1 px per 1000 milli-units, world (0, 0) at the view's top-left.
    let cam = Camera {
        cx: 320_000.0,
        cy: 200_000.0,
        scale: 1e-3,
    };
    let mut p = CanvasPrims::default();
    for (k, len) in [500, 1_000, 1_500, 2_000, 2_100, 2_500, 3_000, 6_000]
        .into_iter()
        .enumerate()
    {
        let y = 20_000 + k as i32 * 12_000;
        for (j, x) in [20_000, 60_300, 100_700, 140_250].into_iter().enumerate() {
            match j {
                0 => p.line(x, y, x + len, y, 4),
                1 => p.line(x, y, x + len, y + len, 1),
                2 => p.rect(x, y, x + len, y + len, 4),
                _ => p.arc(x, y, len / 2, 0, 360_000, 5),
            }
        }
        p.rect(300_000, y, 300_000 + len, y + len, 5); // filled: still a pixel box when small
        p.poly(
            &[
                (400_000, y),
                (400_000 + len, y + len / 2),
                (400_000, y + len),
            ],
            true,
            4,
        );
    }
    let mut rows = InstanceRows::default();
    rows.pack(0, &p, &Palette::xschem_dark(), Change::All);
    let (got, _) = canvas_pass(&mut g, &mut rows, &frame_of(view, cam), size, Color::BLACK);
    let want = cpu_canvas(&p, &Marks::default(), view, &cam, size, Color::BLACK);
    assert!(within("far-zoom cut", &got, &want));
}

/// The mixed thousand under a UI frame, drawn as the window draws it (one pass: the chrome,
/// the canvas stages with each table's labels over its rows, the marks, the gesture overlay,
/// a popup over it all) against the CPU painter painting the same calls.
#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_mixed_scene_matches_cpu_under_the_ui() {
    let mut g = gpu();
    let size = (640u32, 400u32);
    let whole = Rect::new(0.0, 0.0, 640.0, 400.0);
    let view = Rect::new(40.0, 30.0, 600.0, 380.0);
    let cam = fit(view);
    let mut prims = common::thousand_prims();
    prims.text(5_000, 5_000, 1_500, 3, "tiny"); // under 3 px: skipped on both sides
    let mut marks = Marks::default();
    for i in 0..20 {
        marks.push(PrimKind::Line, i * 29, 0xffff_00ff);
    }
    let mut overlay = CanvasPrims::default();
    overlay.rect(100_000, 100_000, 300_000, 200_000, 1);
    overlay.line_styled(100_000, 250_000, 400_000, 260_000, 4, LineStyle::new(2, 3));
    overlay.text(120_000, 120_000, 12_000, 3, "moving");
    let popup = Some(Rect::new(300.0, 10.0, 420.0, 70.0));
    let (got, _) = gpu_frame(&mut g, size, 1.0, Color::BLACK, |p| {
        canvas_scene(p, whole, view, cam, &prims, &marks, &overlay, popup)
    });
    let want = cpu_frame(size, 1.0, Color::BLACK, |p| {
        canvas_scene(p, whole, view, cam, &prims, &marks, &overlay, popup)
    });
    assert!(within("mixed scene under the UI", &got, &want));
    let at = |px: &[u8], x: usize, y: usize| px[4 * (y * size.0 as usize + x)..][..3].to_vec();
    assert_eq!(
        at(&got, 5, 5),
        vec![60, 60, 60],
        "the UI outside the canvas"
    );
    assert_eq!(
        at(&got, 350, 40),
        vec![200, 50, 50],
        "the popup over the canvas"
    );
    assert_ne!(
        at(&got, 45, 375),
        vec![60, 60, 60],
        "the canvas inside the view"
    );
}

/// Canvas labels (symbol texts, pin names, values) at the fit and zoomed in 2.3× and 5.1× and
/// out 0.6×: the GPU's atlas glyphs against the CPU's outlines, mean |Δ| ≤ [`TEXT_MEAN_MAX`]
/// and no wrong pixel. Writes an amplified diff of the fit.
#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_canvas_labels_match_cpu_at_several_zooms() {
    let mut g = gpu();
    let size = (640u32, 400u32);
    let whole = Rect::new(0.0, 0.0, 640.0, 400.0);
    let view = Rect::new(20.0, 10.0, 620.0, 390.0);
    let prims = labels_scene();
    let none = (Marks::default(), CanvasPrims::default());
    for (k, zoom) in [1.0, 2.3, 5.1, 0.6].into_iter().enumerate() {
        let mut cam = fit(view);
        cam.zoom_at(view, 333.3, 201.7, zoom);
        let (got, batch) = gpu_frame(&mut g, size, 1.0, Color::BLACK, |p| {
            canvas_scene(p, whole, view, cam, &prims, &none.0, &none.1, None)
        });
        let want = cpu_frame(size, 1.0, Color::BLACK, |p| {
            canvas_scene(p, whole, view, cam, &prims, &none.0, &none.1, None)
        });
        if k == 0 {
            amplified_diff("canvas_labels", &got, &want, size);
        }
        let name = format!("canvas labels at zoom {zoom}");
        assert!(within_mean(&name, &got, &want, TEXT_MEAN_MAX), "{name}");
        assert_eq!(batch.atlas.overflows, 0, "every glyph fits the atlas");
        assert!(batch.rastered > 0, "{name}: glyphs drawn");
    }
}

/// The 100k canvas at zoom-fit (most rows under the far-zoom cut) agrees with the CPU painter,
/// and the pass's GPU time with the camera moving one pixel a frame is reported.
#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_100k_canvas_matches_cpu_and_reports_frame_time() {
    let mut g = gpu();
    let size = (1600u32, 900u32);
    let view = Rect::new(0.0, 0.0, 1600.0, 900.0);
    let mut cam = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 1.0,
    };
    cam.fit(view, [0, 0, 1_600_000, 900_000]);
    let big = common::hundred_k();
    let mut rows = InstanceRows::default();
    let t0 = Instant::now();
    rows.pack(0, &big, &Palette::xschem_dark(), Change::All);
    let pack_ms = t0.elapsed().as_secs_f64() * 1e3;
    let bytes: usize = (0..schemify_render::gpu::KINDS)
        .map(|k| rows.bytes(0, k).len())
        .sum();
    println!("100k pack: {pack_ms:.2} ms, {} KiB of rows", bytes / 1024);
    let (got, _) = canvas_pass(&mut g, &mut rows, &frame_of(view, cam), size, Color::BLACK);
    let want = cpu_canvas(&big, &Marks::default(), view, &cam, size, Color::BLACK);
    assert!(within_mean("100k at fit", &got, &want, MEAN_MAX_DENSE));
    let mut times = Vec::new();
    for i in 0..30 {
        cam.pan(if i % 2 == 0 { 1.0 } else { -1.0 }, 0.0);
        let (_, ms) = canvas_pass(&mut g, &mut rows, &frame_of(view, cam), size, Color::BLACK);
        if i >= 5 {
            times.push(ms);
        }
    }
    times.sort_by(f64::total_cmp);
    println!(
        "GPU canvas pass (100k primitives, 1600x900, submit to completion): median {:.2} ms",
        times[times.len() / 2]
    );
}
