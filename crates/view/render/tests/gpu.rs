//! GPU path, headless: the GPU painter (the instanced canvas pipelines, `render::gpu`, and the
//! frame batch with its glyph atlas, `render::batch`) must match the CPU painter (`vello_cpu`,
//! the goldens' oracle) within an antialiasing tolerance: one test per primitive kind, the
//! texts (canvas labels at several zooms, UI text, prose, math), the UI shell, and a mixed
//! scene under the UI, with a wrong instance, a wrong glyph and a shifted run as the negative
//! space; the 100k canvas pass's GPU time is reported. Needs a GPU adapter, so ignored by
//! default:
//!
//! `cargo test --release -p schemify-render --test gpu -- --ignored --nocapture`

use std::time::Instant;

use schemify_canvas::{Camera, CanvasPrims, LineStyle, Marks, Palette, PrimKind};
use schemify_render::canvas::{self, CanvasScratch};
use schemify_render::gpu::{Change, Frame, InstanceRows, Pipelines};
use schemify_render::kurbo::{Affine, Rect};
use schemify_render::peniko::Color;
use schemify_render::widgets::{UiScratch, draw_math, draw_prose, draw_text, draw_ui};
use schemify_render::{Batch, CpuPainter, GpuPainter, Painter};
use schemify_render::{Glyph, wgpu};
use schemify_ui::{Kind, Layer, Strings, TextSystem, UiState, compose, layout, records};

mod common;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// The frame's pipelines for an `Rgba8Unorm` target.
    pipes: Pipelines,
}

fn gpu() -> Gpu {
    let inst = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(inst.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .expect("no GPU adapter");
    let info = adapter.get_info();
    println!("adapter: {} ({:?})", info.name, info.backend);
    // WebGL2's guaranteed limits, as the window asks (`Gpu::new`): what the web's WebGL2 tier
    // could not build fails here too.
    let required_limits =
        wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits());
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits,
        ..Default::default()
    }))
    .expect("a device");
    Gpu {
        pipes: Pipelines::new(&device, wgpu::TextureFormat::Rgba8Unorm),
        device,
        queue,
    }
}

fn texture(g: &Gpu, (w, h): (u32, u32), usage: wgpu::TextureUsages) -> wgpu::Texture {
    g.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage,
        view_formats: &[],
    })
}

/// Reads an `Rgba8Unorm` texture back (row-major, `w*h*4` bytes).
fn read(g: &Gpu, t: &wgpu::Texture) -> Vec<u8> {
    let (w, h) = (t.width(), t.height());
    let row = (w * 4).div_ceil(256) * 256;
    let buffer = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = g.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        t.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
        },
        t.size(),
    );
    g.queue.submit([enc.finish()]);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    g.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("poll");
    let data = buffer.slice(..).get_mapped_range().expect("mapped range");
    (0..h)
        .flat_map(|y| data[(y * row) as usize..(y * row + w * 4) as usize].to_vec())
        .collect()
}

fn wgpu_color(c: Color) -> wgpu::Color {
    let c = c.components.map(f64::from);
    wgpu::Color {
        r: c[0],
        g: c[1],
        b: c[2],
        a: c[3],
    }
}

/// One GPU frame as the window settles on it: `paint` into a fresh batch and rows through a
/// [`GpuPainter`] at `scale` device px per logical px, again while the batch drew glyph
/// stand-ins (as the window redraws), uploaded, drawn in one pass into a `size` target
/// cleared to `clear`. Returns the pixels and the batch (its atlas's counts).
fn gpu_frame(
    g: &mut Gpu,
    size: (u32, u32),
    scale: f64,
    clear: Color,
    mut paint: impl FnMut(&mut GpuPainter<'_>),
) -> (Vec<u8>, Batch) {
    let (mut batch, mut rows) = (Batch::default(), InstanceRows::default());
    for _ in 0..64 {
        rows.frame = None;
        batch.begin_frame();
        paint(&mut GpuPainter {
            batch: &mut batch,
            base: Affine::scale(scale),
            rows: Some(&mut rows),
        });
        if batch.settled() {
            break;
        }
    }
    assert!(batch.settled(), "the frame settles");
    let tex = texture(
        g,
        size,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let view = tex.create_view(&Default::default());
    g.pipes.upload(&g.device, &g.queue, &mut rows);
    g.pipes.upload_batch(&g.device, &g.queue, &mut batch);
    let frame = rows.frame.take();
    let mut enc = g.device.create_command_encoder(&Default::default());
    let clear = wgpu_color(clear);
    g.pipes.draw(
        &mut enc,
        &g.queue,
        &view,
        clear,
        &rows,
        &batch,
        frame.as_ref(),
        scale,
        size,
    );
    g.queue.submit([enc.finish()]);
    (read(g, &tex), batch)
}

/// The same frame on the CPU painter over `clear`.
fn cpu_frame(
    size: (u32, u32),
    scale: f64,
    clear: Color,
    paint: impl FnOnce(&mut CpuPainter),
) -> Vec<u8> {
    let mut p = CpuPainter::new(size.0 as u16, size.1 as u16, scale);
    paint(&mut p);
    rgba(&p.finish(clear))
}

/// Mean |Δ| per channel the GPU canvas may differ from the CPU painter by, in 1/255: the
/// shader's coverage (exact band area along a stroke, 16 subsamples at its caps) against
/// vello_cpu's exact area of the flattened outline differs on edge pixels only.
const MEAN_MAX: f64 = 1.0;
/// The mean bound of the 100k scene fitted to the view, where nearly every pixel is an edge
/// of a 1 px stroke and strokes overlap everywhere. Two model differences add up there, both
/// measured 2026-10-24 on the Intel iGPU: the CPU strokes each layer as one path (overlapping
/// strokes cover a pixel once), the GPU blends every instance (CPU per-path against CPU
/// per-instance: mean 1.17), and the cap coverage residue on lines 2–10 px long, mostly
/// cap (GPU against CPU per-instance: 1.35). Measured in total: 3.24.
const MEAN_MAX_DENSE: f64 = 4.0;
/// A channel this many levels off is a wrong pixel, not antialiasing (edge coverage differs
/// by a fraction of the colour; a misplaced primitive by the whole of it).
const FAR_LEVELS: u8 = 96;
/// At most this share of pixels may be wrong: an edge where the two painters' coverage
/// models round a pixel to opposite sides of the colour (both fill each closed shape on its
/// own, so overlapping fills never cancel on either side).
const FAR_SHARE: f64 = 0.001;

/// Text's bound: a glyph mask is vello_cpu's own raster of the glyph with its origin moved to
/// the nearest eighth of a pixel horizontally (and vertically when unhinted), so an edge
/// pixel differs by up to a sixteenth of a pixel's coverage, and blending rounds differently.
/// Measured on the RTX 4060 (mean |Δ|): dense UI text 0.34 at scale 1 and 0.27 at 1.5, the UI
/// shell 0.21 and 0.07, prose and math 0.12, canvas labels 0.01–0.05.
const TEXT_MEAN_MAX: f64 = 0.5;
/// Text drawn at whole and quarter pixels (no phase error): only blending's rounding is left.
const EXACT_LEVELS: u8 = 3;

/// (mean |Δ| per channel, pixels with a channel more than `far` levels off).
fn stats(gpu: &[u8], cpu: &[u8], far: u8) -> (f64, usize) {
    assert_eq!(gpu.len(), cpu.len());
    let (mut sum, mut n_far) = (0u64, 0usize);
    for (a, b) in gpu.chunks_exact(4).zip(cpu.chunks_exact(4)) {
        let d = [
            a[0].abs_diff(b[0]),
            a[1].abs_diff(b[1]),
            a[2].abs_diff(b[2]),
        ];
        sum += d.iter().map(|&x| u64::from(x)).sum::<u64>();
        n_far += usize::from(d.iter().any(|&x| x > far));
    }
    (sum as f64 / (gpu.len() / 4 * 3) as f64, n_far)
}

/// Whether `gpu` matches `cpu` within the tolerance, printing the stats under `name`.
fn within(name: &str, gpu: &[u8], cpu: &[u8]) -> bool {
    within_mean(name, gpu, cpu, MEAN_MAX)
}

/// [`within`] with a mean bound of `mean_max`.
fn within_mean(name: &str, gpu: &[u8], cpu: &[u8], mean_max: f64) -> bool {
    let (mean, far) = stats(gpu, cpu, FAR_LEVELS);
    let n = gpu.len() / 4;
    println!(
        "{name}: GPU vs CPU mean |Δ| {mean:.3}/255, {far} of {n} pixels off by > {FAR_LEVELS}"
    );
    mean <= mean_max && far as f64 <= FAR_SHARE * n as f64
}

fn rgba(px: &schemify_render::Pixmap) -> Vec<u8> {
    px.data()
        .iter()
        .flat_map(|c| [c.r, c.g, c.b, c.a])
        .collect()
}

/// Writes `|gpu − cpu| × 8` (an amplified diff) as `<name>.diff.png` next to the test
/// binary's scratch directory, for a person to look at.
fn amplified_diff(name: &str, gpu: &[u8], cpu: &[u8], (w, h): (u32, u32)) {
    let mut px = schemify_render::Pixmap::new(w as u16, h as u16);
    for (o, (a, b)) in px
        .data_mut()
        .iter_mut()
        .zip(gpu.chunks_exact(4).zip(cpu.chunks_exact(4)))
    {
        let d = |k: usize| a[k].abs_diff(b[k]).saturating_mul(8);
        *o = schemify_render::peniko::color::PremulRgba8 {
            r: d(0),
            g: d(1),
            b: d(2),
            a: 255,
        };
    }
    let out = format!("{}/{name}.diff.png", env!("CARGO_TARGET_TMPDIR"));
    std::fs::write(&out, px.into_png().expect("png")).expect("write");
    println!("{name}: amplified diff (×8) in {out}");
}

fn fit(view: Rect) -> Camera {
    let mut cam = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 1.0,
    };
    cam.fit(view, [0, 0, 600_000, 380_000]);
    cam
}

fn frame_of(view: Rect, cam: Camera) -> Frame {
    let palette = Palette::xschem_dark();
    Frame {
        view,
        cam,
        grid: 20_000,
        background: palette.background,
        grid_color: palette.grid,
    }
}

/// The canvas pass alone: `rows` (packed) drawn for `frame` into a fresh `size` target
/// cleared to `clear`; returns the pixels and the ms from submit to completion.
fn canvas_pass(
    g: &mut Gpu,
    rows: &mut InstanceRows,
    frame: &Frame,
    size: (u32, u32),
    clear: Color,
) -> (Vec<u8>, f64) {
    let tex = texture(
        g,
        size,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let view = tex.create_view(&Default::default());
    g.pipes.upload(&g.device, &g.queue, rows);
    let t0 = Instant::now();
    let mut enc = g.device.create_command_encoder(&Default::default());
    let (queue, none) = (&g.queue, Batch::default());
    let clear = wgpu_color(clear);
    g.pipes.draw(
        &mut enc,
        queue,
        &view,
        clear,
        rows,
        &none,
        Some(frame),
        1.0,
        size,
    );
    g.queue.submit([enc.finish()]);
    g.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("poll");
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    (read(g, &tex), ms)
}

/// The oracle: `prims` and `marks` drawn by the CPU painter (`draw_canvas`: background,
/// grid, every kind, marks) in `view` through `cam` over an `under`-filled `size` target.
fn cpu_canvas(
    prims: &CanvasPrims,
    marks: &Marks,
    view: Rect,
    cam: &Camera,
    size: (u32, u32),
    under: Color,
) -> Vec<u8> {
    let mut p = CpuPainter::new(size.0 as u16, size.1 as u16, 1.0);
    p.fill_rect(
        Rect::new(0.0, 0.0, f64::from(size.0), f64::from(size.1)),
        under,
    );
    canvas::draw_canvas(
        &mut p,
        view,
        cam,
        20_000,
        prims,
        &Palette::xschem_dark(),
        &CanvasPrims::default(),
        marks,
        &mut TextSystem::new(),
        &mut CanvasScratch::default(),
    );
    rgba(&p.finish(Color::BLACK))
}

/// `prims` (one source, no other tables) and `marks` through the canvas pass against the
/// CPU painter at `view`'s fit; returns whether they agree.
fn kind_matches(name: &str, prims: &CanvasPrims, marks: &Marks) -> bool {
    let mut g = gpu();
    let size = (640u32, 400u32);
    let view = Rect::new(40.0, 30.0, 600.0, 380.0);
    let cam = fit(view);
    let mut rows = InstanceRows::default();
    rows.pack(0, prims, &Palette::xschem_dark(), Change::All);
    rows.pack_marks(prims, marks, true);
    let (got, _) = canvas_pass(&mut g, &mut rows, &frame_of(view, cam), size, Color::BLACK);
    let want = cpu_canvas(prims, marks, view, &cam, size, Color::BLACK);
    within(name, &got, &want)
}

fn seeded(seed: u32) -> impl FnMut(i32) -> i32 {
    let mut s = seed;
    move |m: i32| {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        (s % m as u32) as i32
    }
}

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

/// `prims` (one table, with its labels, and `marks`) as a frame paints it: through
/// `Painter::canvas` with the labels as the frame's texts (`draw_texts`), then the gesture
/// `overlay`, between UI chrome before (a grey fill) and after (a popup).
#[allow(clippy::too_many_arguments)]
fn canvas_scene<P: Painter>(
    p: &mut P,
    whole: Rect,
    view: Rect,
    cam: Camera,
    prims: &CanvasPrims,
    marks: &Marks,
    overlay: &CanvasPrims,
    popup: Option<Rect>,
) {
    thread_local! {
        /// One text system for every frame of a test, as the window keeps one: its faces are
        /// the atlas keys' fonts, so a settle frame finds the masks the frame before drew.
        static TEXT: std::cell::RefCell<TextSystem> = std::cell::RefCell::new(TextSystem::new());
    }
    TEXT.with_borrow_mut(|text| {
        canvas_scene_in(p, whole, view, cam, prims, marks, overlay, popup, text)
    });
}

#[allow(clippy::too_many_arguments)]
fn canvas_scene_in<P: Painter>(
    p: &mut P,
    whole: Rect,
    view: Rect,
    cam: Camera,
    prims: &CanvasPrims,
    marks: &Marks,
    overlay: &CanvasPrims,
    popup: Option<Rect>,
    text: &mut TextSystem,
) {
    let palette = Palette::xschem_dark();
    let mut scratch = CanvasScratch::default();
    let empty = CanvasPrims::default();
    p.fill_rect(whole, Color::from_rgba8(60, 60, 60, 255));
    let tables = [
        (prims, Change::All),
        (&empty, Change::All),
        (&empty, Change::All),
        (&empty, Change::All),
    ];
    let texts = |p: &mut P, k: usize| {
        if k == 0 {
            let t = &prims.texts;
            canvas::draw_texts(p, view, &cam, t, None, &palette, text, &mut scratch);
        }
    };
    p.canvas(
        &frame_of(view, cam),
        tables,
        (prims, marks, true),
        &palette,
        texts,
    );
    if !overlay.is_empty() {
        p.overlay(view, &cam, overlay, &palette, text, &mut scratch);
    }
    if let Some(r) = popup {
        p.fill_rect(r, Color::from_rgba8(200, 50, 50, 255));
    }
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

/// Labels of every size the canvas shows (3 px up), turned and not, over a few symbols.
fn labels_scene() -> CanvasPrims {
    let mut p = CanvasPrims::default();
    let words = [
        "R1",
        "10k",
        "VDD",
        "M1 nmos W=2u L=180n",
        "out",
        "Vin+",
        "C3 4.7p",
        "gnd",
    ];
    let mut rnd = seeded(0x5eed_1abe);
    for i in 0..60 {
        let (x, y) = (rnd(560) * 1000 + 10_000, rnd(350) * 1000 + 20_000);
        let size = [3_000, 4_500, 6_000, 8_000, 11_000, 16_000][i % 6];
        let rot = if i % 7 == 3 { 1 + (i % 3) as u8 } else { 0 };
        p.text_rot(x, y, size, [3u8, 4, 5][i % 3], words[i % words.len()], rot);
        p.rect(x - 2_000, y - 2_000, x + 2_000, y + 2_000, 4);
    }
    p
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
