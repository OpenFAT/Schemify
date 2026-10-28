//! GPU path, headless: the GPU painter (the instanced canvas pipelines, `render::gpu`, and the
//! frame batch with its glyph atlas, `render::batch`) must match the CPU painter (`vello_cpu`,
//! the goldens' oracle) within an antialiasing tolerance: one test per primitive kind, the
//! texts (canvas labels at several zooms, UI text, prose, math), the UI shell, and a mixed
//! scene under the UI, with a wrong instance, a wrong glyph and a shifted run as the negative
//! space; the 100k canvas pass's GPU time is reported. Needs a GPU adapter, so ignored by
//! default:
//!
//! `cargo test --release -p schemify-render --test gpu -- --ignored --nocapture`
//!
//! One test binary: the shared device, frame and comparison helpers here; the canvas's tests
//! in `scene`, the UI's (text, prose, math, the shell, retained rows) in `ui`.
//!
//! Contents: none public (helpers shared with the test modules through `super::*`).

#[path = "../common/mod.rs"]
mod common;
mod scene;
mod ui;

use schemify_canvas::{Camera, CanvasPrims, LineStyle, Marks, Palette, PrimKind};
use schemify_render::canvas::{self, CanvasScratch};
use schemify_render::gpu::{Change, Frame, InstanceRows, OVERLAY, Pipelines};
use schemify_render::kurbo::{Affine, Rect};
use schemify_render::peniko::Color;
use schemify_render::widgets::{UiScratch, draw_math, draw_prose, draw_text, draw_ui};
use schemify_render::{Batch, CpuPainter, GpuPainter, Painter};
use schemify_render::{Glyph, wgpu};
use schemify_ui::{Kind, Layer, Strings, TextSystem, UiState, compose, layout, records};
use std::time::Instant;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// The frame's pipelines for an `Rgba8Unorm` target.
    pipes: Pipelines,
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

// ---- private ----

fn gpu() -> Gpu {
    let inst = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = schemify_render::block_on(inst.request_adapter(&wgpu::RequestAdapterOptions {
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
    let (device, queue) =
        schemify_render::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
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
    paint: impl FnMut(&mut GpuPainter<'_>),
) -> (Vec<u8>, Batch) {
    let (mut batch, mut rows) = (Batch::default(), InstanceRows::default());
    let px = gpu_frame_in(g, &mut batch, &mut rows, size, scale, clear, paint);
    (px, batch)
}

/// As [`gpu_frame`] on a kept batch and rows (the window's, across frames): the pipelines
/// must be the ones that drew this batch before, as the window's are.
fn gpu_frame_in(
    g: &mut Gpu,
    batch: &mut Batch,
    rows: &mut InstanceRows,
    size: (u32, u32),
    scale: f64,
    clear: Color,
    mut paint: impl FnMut(&mut GpuPainter<'_>),
) -> Vec<u8> {
    for _ in 0..64 {
        rows.frame = None;
        batch.begin_frame();
        paint(&mut GpuPainter {
            batch,
            base: Affine::scale(scale),
            rows: Some(rows),
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
    g.pipes.upload(&g.device, &g.queue, rows);
    g.pipes.upload_batch(&g.device, &g.queue, batch);
    let frame = rows.frame.take();
    let mut enc = g.device.create_command_encoder(&Default::default());
    let clear = wgpu_color(clear);
    g.pipes.draw(
        &mut enc,
        &g.queue,
        &view,
        clear,
        rows,
        batch,
        frame.as_ref(),
        scale,
        size,
    );
    g.queue.submit([enc.finish()]);
    read(g, &tex)
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

/// `prims` (one table, with its labels, and `marks`) as a frame paints it: through
/// `Painter::canvas_begin` … `canvas_end` with the labels as the frame's texts (`draw_texts`), then the gesture
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
    assert!(
        p.canvas_begin(&frame_of(view, cam)),
        "a painter with a canvas"
    );
    for (k, t) in [prims, &empty, &empty, &empty].into_iter().enumerate() {
        p.canvas_rows(k, t, &palette, Change::All);
        if k == 0 {
            canvas::draw_texts(p, view, &cam, &t.texts, None, &palette, text, &mut scratch);
        }
    }
    p.canvas_marks(prims, marks, true);
    if !overlay.is_empty() {
        p.canvas_rows(OVERLAY, overlay, &palette, Change::All);
        canvas::draw_texts(
            p,
            view,
            &cam,
            &overlay.texts,
            None,
            &palette,
            text,
            &mut scratch,
        );
    }
    p.canvas_end();
    if let Some(r) = popup {
        p.fill_rect(r, Color::from_rgba8(200, 50, 50, 255));
    }
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
