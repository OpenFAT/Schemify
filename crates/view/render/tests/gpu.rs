//! GPU path, headless: the instanced canvas pipelines (`render::gpu`) must match the CPU
//! painter (`vello_cpu`, the goldens' oracle) within an antialiasing tolerance, one test per
//! primitive kind plus a mixed scene under a UI frame, with a deliberately wrong instance as
//! the negative space; the 100k canvas pass's GPU time is reported. The vello-era tests of
//! the raster-layer cache (composed frames, patched layers, live layers) stay until the tile
//! cache goes. Needs a GPU adapter, so ignored by default:
//!
//! `cargo test --release -p schemify-render --test gpu -- --ignored --nocapture`

use std::time::Instant;

use schemify_canvas::{Camera, CanvasPrims, LineStyle, Marks, Palette, PrimKind};
use schemify_render::canvas::{self, CanvasScratch};
use schemify_render::gpu::{Change, Frame, InstanceRows, Pipelines};
use schemify_render::vello::kurbo::{Affine, Rect};
use schemify_render::vello::peniko::Color;
use schemify_render::vello::util::RenderContext;
use schemify_render::vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene, wgpu,
};
use schemify_render::{CpuPainter, Painter, ScenePainter};
use schemify_ui::TextSystem;

mod common;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    /// The canvas pipelines for an `Rgba8Unorm` target.
    pipes: Pipelines,
}

fn gpu() -> Gpu {
    let mut cx = RenderContext::new();
    let id = pollster::block_on(cx.device(None)).expect("no GPU adapter");
    let dev = &cx.devices[id];
    let info = dev.adapter().get_info();
    println!("adapter: {} ({:?})", info.name, info.backend);
    let options = RendererOptions {
        antialiasing_support: AaSupport::area_only(),
        ..RendererOptions::default()
    };
    let renderer = Renderer::new(&dev.device, options).expect("vello renderer");
    Gpu {
        pipes: Pipelines::new(&dev.device, wgpu::TextureFormat::Rgba8Unorm),
        device: dev.device.clone(),
        queue: dev.queue.clone(),
        renderer,
    }
}

/// Renders `scene` to an RGBA8 buffer (row-major, `w*h*4` bytes); returns pixels and GPU ms.
fn render(g: &mut Gpu, scene: &Scene, w: u32, h: u32) -> (Vec<u8>, f64) {
    let texture = g.device.create_texture(&wgpu::TextureDescriptor {
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
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let params = RenderParams {
        base_color: Color::BLACK,
        width: w,
        height: h,
        antialiasing_method: AaConfig::Area,
    };
    let t0 = Instant::now();
    g.renderer
        .render_to_texture(&g.device, &g.queue, scene, &view, &params)
        .expect("render");
    g.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("poll");
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    let row = (w * 4).div_ceil(256) * 256;
    let buffer = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = g
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    enc.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
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
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let start = (y * row) as usize;
        out.extend_from_slice(&data[start..start + (w * 4) as usize]);
    }
    (out, ms)
}

fn paint<P: Painter>(p: &mut P, prims: &CanvasPrims, view: Rect) {
    let mut cam = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 1.0,
    };
    cam.fit(view, [0, 0, 600_000, 380_000]);
    let mut text = TextSystem::new();
    canvas::draw_canvas(
        p,
        view,
        &cam,
        20_000,
        prims,
        &Palette::xschem_dark(),
        &CanvasPrims::default(),
        &Marks::default(),
        &mut text,
        &mut CanvasScratch::default(),
    );
}

#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_matches_cpu_and_reports_frame_time() {
    let mut g = gpu();
    let (w, h) = (640u32, 400u32);
    let view = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
    let prims = common::thousand_prims();
    let mut scene = Scene::new();
    paint(
        &mut ScenePainter {
            scene: &mut scene,
            base: Affine::IDENTITY,
            layers: None,
            rows: None,
        },
        &prims,
        view,
    );
    let (gpu_px, _) = render(&mut g, &scene, w, h);
    let mut cpu = CpuPainter::new(w as u16, h as u16, 1.0);
    paint(&mut cpu, &prims, view);
    let cpu_px = cpu.finish(Color::BLACK);
    // Opaque output, so premultiplied CPU bytes equal straight GPU bytes where they agree.
    let (mut sum, mut far) = (0u64, 0usize);
    for (a, b) in cpu_px.data().iter().zip(gpu_px.chunks_exact(4)) {
        let d = [a.r.abs_diff(b[0]), a.g.abs_diff(b[1]), a.b.abs_diff(b[2])];
        sum += d.iter().map(|&x| u64::from(x)).sum::<u64>();
        far += usize::from(d.iter().any(|&x| x > 64));
    }
    let n = (w * h) as usize;
    let mean = sum as f64 / (n * 3) as f64;
    println!("GPU vs CPU (canvas_1k): mean |Δ| {mean:.3}/255, {far} of {n} pixels differ by > 64");
    assert!(
        mean < 2.0 && far * 100 < n,
        "GPU and CPU renderings diverge"
    );

    let big = common::hundred_k();
    let view = Rect::new(0.0, 0.0, 1600.0, 900.0);
    let mut cam = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 1.0,
    };
    cam.fit(view, [0, 0, 1_600_000, 900_000]);
    let (mut text, mut scratch) = (TextSystem::new(), CanvasScratch::default());
    let mut times = Vec::new();
    for i in 0..30 {
        scene.reset();
        cam.pan(if i % 2 == 0 { 1.0 } else { -1.0 }, 0.0);
        let mut p = ScenePainter {
            scene: &mut scene,
            base: Affine::IDENTITY,
            layers: None,
            rows: None,
        };
        canvas::draw_canvas(
            &mut p,
            view,
            &cam,
            20_000,
            &big,
            &Palette::xschem_dark(),
            &CanvasPrims::default(),
            &Marks::default(),
            &mut text,
            &mut scratch,
        );
        let (_, ms) = render(&mut g, &scene, 1600, 900);
        if i >= 5 {
            times.push(ms);
        }
    }
    times.sort_by(f64::total_cmp);
    println!(
        "GPU render (100k primitives, 1600x900, incl. submit+wait): median {:.2} ms",
        times[times.len() / 2]
    );
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

/// (mean |Δ| per channel, pixels with a channel off by more than 2).
fn diff_px(a: &[u8], b: &[u8]) -> (f64, usize) {
    let (mut sum, mut bad) = (0u64, 0usize);
    for (p, q) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let d: Vec<u8> = p.iter().zip(q).map(|(x, y)| x.abs_diff(*y)).collect();
        sum += d.iter().map(|&x| u64::from(x)).sum::<u64>();
        bad += usize::from(d.iter().any(|&x| x > 2));
    }
    (sum as f64 / a.len() as f64, bad)
}

/// A live layer (`Painter::live_layer`: the status bar, whose text changes on every pointer
/// move) lets a frame that changed nothing else compose without vello, equal to vello's frame
/// of the new picture; a change outside it still goes through vello.
#[test]
#[ignore = "needs a GPU adapter"]
fn live_layers_compose_without_vello() {
    use schemify_render::Layers;

    let mut g = gpu();
    let size = (320u32, 200u32);
    let target = texture(
        &g,
        size,
        wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
    );
    let surface = texture(
        &g,
        size,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let (target_view, surface_view) = (
        target.create_view(&Default::default()),
        surface.create_view(&Default::default()),
    );
    let blitter = wgpu::util::TextureBlitter::new(&g.device, wgpu::TextureFormat::Rgba8Unorm);
    let (mut layers, mut scene) = (Layers::default(), Scene::new());
    let canvas = Rect::new(10.0, 10.0, 310.0, 170.0);
    let strip = Rect::new(0.0, 174.0, 320.0, 200.0);
    // One window frame: the chrome, a canvas stand-in, then the status strip as a live layer whose mark sits `k` steps to the right; vello or composed, as the
    // window does.
    let mut frame = |k: u32, chrome: Color, full: bool| -> (bool, Vec<u8>) {
        scene.reset();
        layers.begin_frame(Affine::IDENTITY);
        let mut p = ScenePainter {
            scene: &mut scene,
            base: Affine::IDENTITY,
            layers: Some(&mut layers),
            rows: None,
        };
        p.fill_rect(Rect::new(0.0, 0.0, 320.0, 200.0), chrome);
        p.fill_rect(canvas, Color::from_rgba8(30, 30, 60, 255));
        p.fill_rect(
            Rect::new(50.0, 50.0, 130.0, 100.0),
            Color::from_rgba8(200, 80, 80, 255),
        );
        p.live_layer(7, strip, |p| {
            p.fill_rect(strip, Color::from_rgba8(217, 217, 217, 255));
            let x = 8.0 + 9.0 * f64::from(k);
            p.fill_rect(Rect::new(x, 180.0, x + 6.0, 194.0), Color::BLACK);
        });
        layers
            .raster(&g.device, &g.queue, &mut g.renderer)
            .expect("raster");
        let reuse = layers.reusable(size.0, size.1) && !full;
        if !reuse {
            let params = RenderParams {
                base_color: Color::BLACK,
                width: size.0,
                height: size.1,
                antialiasing_method: AaConfig::Area,
            };
            g.renderer
                .render_to_texture(&g.device, &g.queue, &scene, &target_view, &params)
                .expect("render");
            layers.rendered(size.0, size.1);
        }
        let mut enc = g.device.create_command_encoder(&Default::default());
        blitter.copy(&g.device, &mut enc, &target_view, &surface_view);
        if reuse {
            let format = wgpu::TextureFormat::Rgba8Unorm;
            layers.compose(&g.device, &g.queue, &mut enc, &surface_view, format, size);
        }
        g.queue.submit([enc.finish()]);
        (reuse, read(&g, &surface))
    };
    let gray = Color::from_rgba8(60, 60, 60, 255);
    let (reused, _) = frame(0, gray, false);
    assert!(!reused, "the first frame goes through vello");
    for k in 1..4 {
        let (reused, composed) = frame(k, gray, false);
        assert!(
            reused,
            "a frame that changed only its live layer composes (step {k})"
        );
        let (_, full) = frame(k, gray, true);
        let (mean, bad) = diff_px(&composed, &full);
        println!(
            "live layer step {k}: composed vs vello mean |Δ| {mean:.4}, {bad} pixels off by > 2"
        );
        if bad != 0 {
            let (mut bb, mut n) = ([u32::MAX, u32::MAX, 0, 0], 0);
            for (i, (p, q)) in composed
                .chunks_exact(4)
                .zip(full.chunks_exact(4))
                .enumerate()
            {
                if p.iter().zip(q).any(|(a, b)| a.abs_diff(*b) > 2) {
                    let (x, y) = (i as u32 % size.0, i as u32 / size.0);
                    bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x), bb[3].max(y)];
                    n += 1;
                }
            }
            let at =
                |px: &[u8], x: usize, y: usize| px[4 * (y * size.0 as usize + x)..][..4].to_vec();
            println!(
                "diff bbox {bb:?} ({n} px); (100,100) composed {:?} full {:?}; (20,187) composed {:?} full {:?}; (5,5) composed {:?} full {:?}",
                at(&composed, 100, 100),
                at(&full, 100, 100),
                at(&composed, 20, 187),
                at(&full, 20, 187),
                at(&composed, 5, 5),
                at(&full, 5, 5)
            );
        }
        assert_eq!(bad, 0, "the composed frame differs from vello's");
        // Negative space: the composed frame shows the new mark, not the last vello frame's.
        let (_, stale) = frame(k - 1, gray, true);
        let (_, moved) = diff_px(&composed, &stale);
        assert!(
            moved > 50,
            "the mark moved in the composed frame ({moved} px differ)"
        );
        frame(k, gray, true); // vello's frame of this step again: the next step may compose
    }
    // Negative space: a change outside the live layer is rendered by vello.
    let (reused, _) = frame(3, Color::from_rgba8(61, 60, 60, 255), false);
    assert!(!reused, "changed chrome is rendered by vello");
}

// The instanced canvas pipelines against the CPU painter.

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
/// At most this share of pixels may be wrong. Within it in the scenes built on the thousand
/// (`thousand_prims`): one hole of about 14 px the CPU painter leaves where a filled layer-5
/// rect and polygon of opposite winding overlap (it fills a layer's closed shapes as one
/// non-zero path, so they cancel); the GPU fills both.
const FAR_SHARE: f64 = 0.001;

/// Whether `gpu` matches `cpu` within the tolerance, printing the stats under `name`.
fn within(name: &str, gpu: &[u8], cpu: &[u8]) -> bool {
    within_mean(name, gpu, cpu, MEAN_MAX)
}

/// [`within`] with a mean bound of `mean_max`.
fn within_mean(name: &str, gpu: &[u8], cpu: &[u8], mean_max: f64) -> bool {
    assert_eq!(gpu.len(), cpu.len());
    let (mut sum, mut far) = (0u64, 0usize);
    for (a, b) in gpu.chunks_exact(4).zip(cpu.chunks_exact(4)) {
        let d = [
            a[0].abs_diff(b[0]),
            a[1].abs_diff(b[1]),
            a[2].abs_diff(b[2]),
        ];
        sum += d.iter().map(|&x| u64::from(x)).sum::<u64>();
        far += usize::from(d.iter().any(|&x| x > FAR_LEVELS));
    }
    let n = gpu.len() / 4;
    let mean = sum as f64 / (n * 3) as f64;
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
    let c = clear.components.map(f64::from);
    let clear = wgpu::Color {
        r: c[0],
        g: c[1],
        b: c[2],
        a: c[3],
    };
    let queue = &g.queue;
    g.pipes
        .draw(&mut enc, queue, &view, clear, rows, Some(frame), 1.0, size);
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

/// The mixed thousand under a UI frame, composed as the window composes it: the UI painted
/// before the canvas is cleared by the hole, the canvas pass shows through it, the texts and
/// a popup painted after stay over it.
#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_mixed_scene_matches_cpu_under_the_ui() {
    let mut g = gpu();
    let size = (640u32, 400u32);
    let whole = Rect::new(0.0, 0.0, 640.0, 400.0);
    let view = Rect::new(40.0, 30.0, 600.0, 380.0);
    let cam = fit(view);
    let palette = Palette::xschem_dark();
    let mut prims = common::thousand_prims();
    prims.text(5_000, 5_000, 1_500, 3, "tiny"); // under 3 px: skipped on both sides
    let mut marks = Marks::default();
    for i in 0..20 {
        marks.push(PrimKind::Line, i * 29, 0xffff_00ff);
    }
    let gray = Color::from_rgba8(60, 60, 60, 255);
    let popup = Rect::new(300.0, 10.0, 420.0, 70.0);
    let red = Color::from_rgba8(200, 50, 50, 255);
    let (mut text, mut scratch) = (TextSystem::new(), CanvasScratch::default());
    let mut rows = InstanceRows::default();
    let mut scene = Scene::new();
    let empty = CanvasPrims::default();
    {
        let mut p = ScenePainter {
            scene: &mut scene,
            base: Affine::IDENTITY,
            layers: None,
            rows: Some(&mut rows),
        };
        p.fill_rect(whole, gray);
        let tables = [
            (&prims, Change::All),
            (&empty, Change::All),
            (&empty, Change::All),
            (&empty, Change::All),
        ];
        let texts = |p: &mut ScenePainter, k: usize| {
            if k == 0 {
                let t = &prims.texts;
                canvas::draw_texts(p, view, &cam, t, None, &palette, &mut text, &mut scratch);
            }
        };
        p.canvas(
            &frame_of(view, cam),
            tables,
            (&prims, &marks, true),
            &palette,
            texts,
        );
        p.fill_rect(popup, red);
    }
    let ui = texture(
        &g,
        size,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let ui_view = ui.create_view(&Default::default());
    let params = RenderParams {
        base_color: Color::TRANSPARENT,
        width: size.0,
        height: size.1,
        antialiasing_method: AaConfig::Area,
    };
    g.renderer
        .render_to_texture(&g.device, &g.queue, &scene, &ui_view, &params)
        .expect("render");
    let surface = texture(
        &g,
        size,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let surface_view = surface.create_view(&Default::default());
    let frame = rows.frame.take().expect("the canvas call left a frame");
    g.pipes.upload(&g.device, &g.queue, &mut rows);
    let mut enc = g.device.create_command_encoder(&Default::default());
    let queue = &g.queue;
    let black = wgpu::Color::BLACK;
    g.pipes.draw(
        &mut enc,
        queue,
        &surface_view,
        black,
        &rows,
        Some(&frame),
        1.0,
        size,
    );
    let blitter =
        wgpu::util::TextureBlitterBuilder::new(&g.device, wgpu::TextureFormat::Rgba8Unorm)
            .blend_state(wgpu::BlendState::ALPHA_BLENDING)
            .build();
    blitter.copy(&g.device, &mut enc, &ui_view, &surface_view);
    g.queue.submit([enc.finish()]);
    let got = read(&g, &surface);
    let mut cpu = CpuPainter::new(size.0 as u16, size.1 as u16, 1.0);
    cpu.fill_rect(whole, gray);
    canvas::draw_canvas(
        &mut cpu,
        view,
        &cam,
        20_000,
        &prims,
        &palette,
        &empty,
        &marks,
        &mut TextSystem::new(),
        &mut CanvasScratch::default(),
    );
    cpu.fill_rect(popup, red);
    let want = rgba(&cpu.finish(Color::BLACK));
    assert!(within("mixed scene under the UI", &got, &want));
    // Negative space: without the hole the UI would cover the canvas.
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
        "the canvas, not the UI, inside the view"
    );
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
