//! GPU path (vello on wgpu, headless): the 1k-primitive canvas rendered on the GPU must match
//! the CPU golden within an antialiasing tolerance, and the 100k scene's GPU time is reported.
//! Needs a GPU adapter, so ignored by default:
//!
//! `cargo test --release -p schemify-render --test gpu -- --ignored --nocapture`

use std::time::Instant;

use schemify_render::canvas::{self, Camera, CanvasPrims, CanvasScratch, Marks, Palette};
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

/// The window's vello-free frames (`Layers::reusable` + `Layers::compose`, D39) equal a full
/// vello render of the same frame: exactly for a pan, within a bilinear tolerance for a zoom
/// stand-in; and a change outside the layers forbids reuse.
#[test]
#[ignore = "needs a GPU adapter"]
fn composed_frames_equal_vello_frames() {
    use schemify_render::Layers;
    use schemify_render::canvas::{CanvasCache, Drawn};

    let mut g = gpu();
    let size = (640u32, 400u32);
    let prims = common::thousand_prims();
    let view = Rect::new(40.0, 30.0, 600.0, 380.0);
    let mut cam = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 1.0,
    };
    cam.fit(view, [0, 0, 600_000, 380_000]);
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
    let (mut layers, mut cache, mut scene) =
        (Layers::default(), CanvasCache::default(), Scene::new());
    let (mut text, mut scratch) = (TextSystem::new(), CanvasScratch::default());
    let bg = schemify_render::rgba(Palette::xschem_dark().background);
    // One window frame: a UI rect, the cached canvas; vello or composed, as the window does.
    // `float`: 0 none; 1 a translucent rounded panel over the canvas in its own clip, painted
    // after it (the editor's canvas floats, e.g. the view switcher); 2 the same inside a clip
    // opened before the canvas.
    let mut frame =
        |cam: &Camera, crisp: bool, ui: Color, full: bool, float: u8| -> (bool, Drawn, Vec<u8>) {
            scene.reset();
            layers.begin_frame(Affine::IDENTITY);
            let mut p = ScenePainter {
                scene: &mut scene,
                base: Affine::IDENTITY,
                layers: Some(&mut layers),
            };
            p.fill_rect(Rect::new(0.0, 0.0, 640.0, 400.0), ui);
            if float == 2 {
                p.push_clip(Rect::new(0.0, 0.0, 640.0, 400.0));
            }
            let drawn = canvas::draw_cached(
                &mut p,
                &mut cache,
                view,
                cam,
                1,
                Some(&[]),
                crisp,
                bg,
                |p, v, c| {
                    p.fill_rect(v, schemify_render::rgba(Palette::xschem_dark().background));
                    canvas::draw_prims(
                        p,
                        v,
                        c,
                        &prims,
                        &Palette::xschem_dark(),
                        &mut text,
                        &mut scratch,
                    )
                },
            );
            if float > 0 {
                use schemify_render::vello::kurbo::{RoundedRect, Shape};
                let panel = Rect::new(250.0, 40.0, 420.0, 72.0);
                p.push_clip(panel.inflate(2.0, 2.0));
                let path = RoundedRect::from_rect(panel, 8.0).to_path(0.1);
                p.fill_path(&path, Color::from_rgba8(230, 230, 240, 200));
                p.fill_rect(
                    Rect::new(262.0, 50.0, 330.0, 62.0),
                    Color::from_rgba8(20, 90, 200, 255),
                );
                p.pop_clip();
            }
            if float == 2 {
                p.pop_clip();
            }
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
            (reuse, drawn, read(&g, &surface))
        };
    let gray = Color::from_rgba8(60, 60, 60, 255);
    let (reused, drawn, _) = frame(&cam, true, gray, false, 0);
    assert!(
        !reused && drawn == Drawn::Recorded,
        "the first frame goes through vello"
    );

    // A pan: composed without vello, equal to vello's frame.
    cam.pan(17.0, -9.0);
    let (reused, drawn, composed) = frame(&cam, true, gray, false, 0);
    assert!(
        reused && drawn == Drawn::Copied,
        "a pan reuses ({reused}, {drawn:?})"
    );
    let (_, _, full) = frame(&cam, true, gray, true, 0);
    let (mean, bad) = diff_px(&composed, &full);
    println!("pan: composed vs vello mean |Δ| {mean:.4}, {bad} pixels off by > 2");
    assert!(bad * 1000 <= composed.len() / 4, "pan frame differs: {bad}");

    // A zoom stand-in: the layer stretched; bilinear on both sides, so within tolerance.
    cam.zoom_at(view, 300.0, 200.0, 1.2);
    let (reused, drawn, composed) = frame(&cam, false, gray, false, 0);
    assert!(
        reused && drawn == Drawn::Stretched,
        "a zoom stand-in reuses"
    );
    let (_, _, full) = frame(&cam, false, gray, true, 0);
    let (mean, bad) = diff_px(&composed, &full);
    println!("zoom stand-in: composed vs vello mean |Δ| {mean:.4}, {bad} pixels off by > 2");
    assert!(mean < 2.0, "zoom stand-in differs: mean {mean}");

    // A zoom-out past the margin: still a stand-in, the uncovered view filled with background.
    cam.zoom_at(view, 300.0, 200.0, 1.0 / 4.0);
    let (reused, drawn, composed) = frame(&cam, false, gray, false, 0);
    assert!(
        reused && drawn == Drawn::Stretched,
        "a zoom-out stand-in reuses"
    );
    let (_, _, full) = frame(&cam, false, gray, true, 0);
    let (mean, bad) = diff_px(&composed, &full);
    println!("zoom-out stand-in: composed vs vello mean |Δ| {mean:.4}, {bad} pixels off by > 2");
    assert!(mean < 2.0, "zoom-out stand-in differs: mean {mean}");

    // Anything painted differently outside the layer: through vello again.
    let (reused, _, _) = frame(&cam, false, Color::from_rgba8(61, 60, 60, 255), false, 0);
    assert!(!reused, "a changed UI is rendered by vello");

    // Issue #45: a float painted over the canvas. Its first frame goes through vello; then a
    // pan composes the tiles with the float laid over them, equal to vello's frame.
    cam.zoom_at(view, 300.0, 200.0, 4.0);
    let (reused, _, _) = frame(&cam, true, gray, false, 1);
    assert!(!reused, "the float's first frame is rendered by vello");
    for _ in 0..64 {
        if frame(&cam, true, gray, false, 1).1 != Drawn::Partial {
            break;
        }
    }
    for k in 0..4 {
        cam.pan(23.0, 7.0 - 5.0 * f64::from(k));
        let (reused, drawn, composed) = frame(&cam, true, gray, false, 1);
        assert!(
            reused && drawn != Drawn::Partial,
            "a pan under a float reuses ({reused}, {drawn:?})"
        );
        let (_, _, full) = frame(&cam, true, gray, true, 1);
        let (mean, bad) = diff_px(&composed, &full);
        println!(
            "pan under a float: composed vs vello mean |Δ| {mean:.4}, {bad} pixels off by > 2"
        );
        assert!(
            bad * 1000 <= composed.len() / 4,
            "pan under a float differs: {bad}"
        );
        // Negative space: the composed frame does show the float (not the bare canvas).
        let (_, _, bare) = frame(&cam, true, gray, true, 0);
        let (_, off) = diff_px(&composed, &bare);
        assert!(
            off > 1000,
            "the float is in the composed frame ({off} px differ)"
        );
        frame(&cam, true, gray, true, 1); // vello's frame again: the next pan may compose
    }
    // Negative space: a float inside a clip opened before the canvas is not composed.
    frame(&cam, true, gray, true, 2);
    cam.pan(11.0, 0.0);
    let (reused, _, _) = frame(&cam, true, gray, false, 2);
    assert!(!reused, "a float under an outer clip is rendered by vello");
}

/// D39 edits on the GPU: a layer patched where `CanvasDiff` says the content changed (each part
/// rendered into its own texture and copied over its place) shows what a fresh recording of the
/// same camera shows.
#[test]
#[ignore = "needs a GPU adapter"]
fn patched_layers_equal_recorded_layers() {
    use schemify_render::Layers;
    use schemify_render::canvas::{CanvasCache, CanvasDiff, Drawn};

    let mut g = gpu();
    let (w, h) = (640u32, 400u32);
    let mut prims = common::thousand_prims();
    let view = Rect::new(40.0, 30.0, 600.0, 380.0);
    let mut cam = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 1.0,
    };
    cam.fit(view, [0, 0, 600_000, 380_000]);
    let (mut text, mut scratch) = (TextSystem::new(), CanvasScratch::default());
    let bg = schemify_render::rgba(Palette::xschem_dark().background);
    let mut frame = |layers: &mut Layers,
                     cache: &mut CanvasCache,
                     prims: &CanvasPrims,
                     key: u64,
                     damage: Option<&[[i32; 4]]>| {
        let mut scene = Scene::new();
        layers.begin_frame(Affine::IDENTITY);
        let mut p = ScenePainter {
            scene: &mut scene,
            base: Affine::IDENTITY,
            layers: Some(layers),
        };
        let drawn = canvas::draw_cached(
            &mut p,
            cache,
            view,
            &cam,
            key,
            damage,
            true,
            bg,
            |p, v, c| {
                p.fill_rect(v, bg);
                canvas::draw_prims(
                    p,
                    v,
                    c,
                    prims,
                    &Palette::xschem_dark(),
                    &mut text,
                    &mut scratch,
                )
            },
        );
        layers
            .raster(&g.device, &g.queue, &mut g.renderer)
            .expect("raster");
        (drawn, render(&mut g, &scene, w, h).0)
    };
    let (mut layers, mut cache, mut diff) = (
        Layers::default(),
        CanvasCache::default(),
        CanvasDiff::default(),
    );
    let mut boxes = Vec::new();
    let marks = Marks::default();
    assert!(!diff.update(&[(&prims, true)], (&prims, &marks, true), &mut boxes));
    assert_eq!(
        frame(&mut layers, &mut cache, &prims, 1, None).0,
        Drawn::Recorded
    );
    for (key, (x, y)) in [(2u64, (150_000, 90_000)), (3, (420_000, 260_000))] {
        prims.line(x, y, x + 60_000, y + 20_000, 1);
        let l = &mut prims.lines;
        for col in [&mut l.x0, &mut l.y0, &mut l.x1, &mut l.y1] {
            col.swap_remove(key as usize * 37);
        }
        l.layer.swap_remove(key as usize * 37);
        l.style.swap_remove(key as usize * 37);
        boxes.clear();
        assert!(diff.update(&[(&prims, true)], (&prims, &marks, true), &mut boxes));
        let (drawn, patched) = frame(&mut layers, &mut cache, &prims, key, Some(&boxes));
        assert_eq!(drawn, Drawn::Patched, "an edit patches");
        let (again, fresh) = frame(
            &mut Layers::default(),
            &mut CanvasCache::default(),
            &prims,
            key,
            None,
        );
        assert_eq!(again, Drawn::Recorded);
        let (mean, bad) = diff_px(&patched, &fresh);
        println!("edit {key}: patched vs recorded mean |Δ| {mean:.5}, {bad} pixels off by > 2");
        // vello flattens curves on the GPU in f32 relative to the target, so a part rendered
        // on its own can move an arc's edge by a few levels; a missed row would be many
        // pixels at full contrast.
        assert!(
            mean < 0.01 && bad * 10_000 <= patched.len() / 4,
            "a patched layer differs from a recording: mean {mean}, {bad} pixels"
        );
    }
}
