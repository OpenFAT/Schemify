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
