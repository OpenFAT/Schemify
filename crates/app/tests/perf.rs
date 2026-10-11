//! D39 budgets through the real frame spine: open, frame times while panning and zooming, a
//! local edit round trip (10k / 100k / 1M objects), and a var edit with 1k dependents.
//!
//! A frame is `Editor::update` + `Editor::paint` into a vello scene with raster layers, as the
//! window does; its CPU time is reported as p50/p95/p99. With a GPU adapter that supports
//! timestamps inside encoders, each frame is also rendered (layer raster + frame scene) and
//! its GPU time reported; without one the GPU columns are skipped. Pick the adapter with
//! `SCHEMIFY_GPU=<name substring>` (default: the first non-CPU adapter).
//!
//! `cargo test --release -p schemify-app --test perf -- --ignored --nocapture --test-threads=1`
//! (NixOS: put the 64-bit vulkan-loader `lib` and `/run/opengl-driver/lib` on `LD_LIBRARY_PATH`.)

mod common;
use common::{U, VIEW, fixture};

use std::fmt::Write;
use std::time::Instant;

use schemify_app::editor::{Editor, Options};
use schemify_render::vello::kurbo::Affine;
use schemify_render::vello::peniko::Color;
use schemify_render::vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene, wgpu,
};
use schemify_render::{Layers, ScenePainter};
use schemify_ui::{Button, Chord, InputEvent, InputEvents};

fn id(prefix: char, n: u32) -> String {
    const A: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let mut s = String::from(prefix);
    for k in (0..5).rev() {
        s.push(A[((n >> (5 * k)) & 31) as usize] as char);
    }
    s
}

/// `n` objects on a square grid of tiles, each tile one instance, six wires, two labels and
/// one text (n = 100k: 10k instances, 60k wires, 20k labels, 10k texts on 100 × 100 tiles).
fn schematic(n: u32) -> String {
    let tiles = n / 10;
    let side = (f64::from(tiles).sqrt().ceil() as u32).max(1);
    let at = |t: u32| (i64::from(t % side) * 200, i64::from(t / side) * 200);
    let mut t = String::from("schemify 1 shcomp\n\nimpl sch schematic\n\n");
    for i in 0..tiles {
        let (x, y) = at(i);
        let _ = writeln!(
            t,
            "inst {} dev:nmos\n  at {x} {y} R0\n  name M{i}",
            id('m', i)
        );
    }
    for i in 0..tiles * 6 {
        let (x, y) = at(i / 6);
        let k = i64::from(i % 6);
        let y = y - 30 - k * 10;
        let _ = writeln!(t, "wire {} {} {y} {} {y}", id('w', i), x + 20, x + 120);
    }
    for i in 0..tiles * 2 {
        let (x, y) = at(i / 2);
        let y = y - 30 - i64::from(i % 2) * 10;
        let _ = writeln!(t, "label {} {} {y} R0 n{i}", id('n', i), x + 120);
    }
    for i in 0..tiles {
        let (x, y) = at(i);
        let _ = writeln!(t, "text {} {} {} R0 t{i}", id('t', i), x - 60, y + 60);
    }
    t
}

/// D39 "param edit → recompute + redraw ≤ 8 ms for 1k dependents": a var edited on the docs
/// page (Enter in its field) re-evaluates 1000 instance overrides, re-projects the page,
/// recomposes the UI and rebuilds the scene with the new symbol labels — one `update`. Once
/// for a plain var, once for a var behind a `gmid` plugin call (D20: plugin call + cache
/// write folded into the edit's undo entry; the amend's undo + recommit nets to the cache
/// ops, so the cell state stays incremental).
#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn var_edit_with_1k_dependents_through_the_docs_page() {
    let dir = fixture("ota", "perf-docs");
    let mut t = String::from("schemify 1 shcomp\n\nimpl sch schematic\n\n");
    for i in 0..1000u32 {
        let (x, y) = (i64::from(i % 40) * 120, i64::from(i / 40) * 120);
        let _ = writeln!(
            t,
            "inst {} dev:nfet_01v8\n  at {x} {y} R0\n  name M{i}\n  W Wg\n  L L1",
            id('m', i)
        );
    }
    t.push_str("\nvar L1 0.5u\nvar k 1\nvar g 15\n");
    t.push_str("var Wg \"gmid.w(\\\"nfet_01v8\\\", g, 0.5u, 10u)*k\"\n");
    std::fs::write(dir.join("big.shcomp"), t).unwrap();
    let ed = Editor::new(Options {
        no_default_ui: false,
        path: Some(dir.join("big.shcomp")),
        seed: 1,
        ..Default::default()
    })
    .unwrap();
    let mut s = common::Session { ed, now: 1, dir };
    for _ in 0..3 {
        s.frame(InputEvents::default());
    }
    let measure = |s: &mut common::Session, row: u32, expr: &str| {
        s.click_node(&format!("docs.b.{row}.expr"));
        s.key("Ctrl+A");
        s.type_text(expr);
        let t = Instant::now();
        s.key("Enter");
        t.elapsed().as_secs_f64() * 1e3
    };
    let mut plain = Vec::new();
    let mut plugin = Vec::new();
    for k in 0..9 {
        plain.push(measure(&mut s, 0, &format!("0.{}u", 5 + k % 4)));
        plugin.push(measure(&mut s, 1, &format!("1.0{k}")));
    }
    let w = s.text();
    assert!(w.contains("  cached "), "the plugin path wrote its cache");
    plain.sort_by(f64::total_cmp);
    plugin.sort_by(f64::total_cmp);
    eprintln!(
        "var edit, 1000 dependents, one frame: plain median {:.2} ms (max {:.2}); \
         gmid-backed median {:.2} ms (max {:.2})",
        plain[4], plain[8], plugin[4], plugin[8]
    );
}

/// Polls a wgpu future to completion (native wgpu resolves them without a reactor).
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

/// A headless GPU with timestamp queries: renders each benchmark frame as the window does.
struct Gpu {
    name: String,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    /// vello's target and the stand-in for the window surface it is blitted to.
    view: wgpu::TextureView,
    surface: wgpu::TextureView,
    blitter: wgpu::util::TextureBlitter,
    /// Frames composed without vello (`Layers::reusable`).
    reused: usize,
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    /// Copied before the first timestamp so the GPU is still busy when this frame's work is
    /// submitted: the measured span then excludes CPU encoding gaps.
    busy: (wgpu::Buffer, wgpu::Buffer),
}

impl Gpu {
    fn new() -> Option<Gpu> {
        let inst = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let want = std::env::var("SCHEMIFY_GPU")
            .unwrap_or_default()
            .to_lowercase();
        let need =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        let adapter = block_on(inst.enumerate_adapters(wgpu::Backends::all()))
            .into_iter()
            .filter(|a| a.get_info().device_type != wgpu::DeviceType::Cpu)
            .filter(|a| a.features().contains(need))
            .find(|a| a.get_info().name.to_lowercase().contains(&want))?;
        let name = adapter.get_info().name;
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: need | (adapter.features() & wgpu::Features::CLEAR_TEXTURE),
            ..Default::default()
        }))
        .ok()?;
        let options = RendererOptions {
            antialiasing_support: AaSupport::area_only(),
            ..Default::default()
        };
        let renderer = Renderer::new(&device, options).ok()?;
        let texture = |usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d {
                        width: VIEW.0 as u32,
                        height: VIEW.1 as u32,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let view =
            texture(wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING);
        let surface = texture(wgpu::TextureUsages::RENDER_ATTACHMENT);
        let blitter = wgpu::util::TextureBlitter::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: None,
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let buffer = |size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let resolve = buffer(
            16,
            wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        );
        let read = buffer(
            16,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        let busy = (
            buffer(64 << 20, wgpu::BufferUsages::COPY_SRC),
            buffer(64 << 20, wgpu::BufferUsages::COPY_DST),
        );
        Some(Gpu {
            name,
            device,
            queue,
            renderer,
            view,
            surface,
            blitter,
            reused: 0,
            queries,
            resolve,
            read,
            busy,
        })
    }

    /// Renders one painted frame exactly as the window does (layer raster; vello, or quads
    /// over the last frame when `Layers::reusable`; blit to the surface), returning (CPU ms of
    /// the render calls, GPU ms between timestamps around them).
    fn render(&mut self, scene: &Scene, layers: &mut Layers) -> (f64, f64) {
        self.timed(|g| {
            layers
                .raster(&g.device, &g.queue, &mut g.renderer)
                .expect("layer raster");
            let (w, h) = (VIEW.0 as u32, VIEW.1 as u32);
            let reuse = layers.reusable(w, h);
            if !reuse {
                let params = RenderParams {
                    base_color: Color::BLACK,
                    width: w,
                    height: h,
                    antialiasing_method: AaConfig::Area,
                };
                g.renderer
                    .render_to_texture(&g.device, &g.queue, scene, &g.view, &params)
                    .expect("render");
                layers.rendered(w, h);
            }
            let mut e = g.device.create_command_encoder(&Default::default());
            g.blitter.copy(&g.device, &mut e, &g.view, &g.surface);
            if reuse {
                let format = wgpu::TextureFormat::Rgba8Unorm;
                layers.compose(&g.device, &g.queue, &mut e, &g.surface, format, (w, h));
                g.reused += 1;
            }
            g.queue.submit([e.finish()]);
        })
    }

    /// The median GPU span of two timestamps with no work between them: what this harness
    /// adds to every GPU number (submission boundaries; ~0 on NVIDIA, ~2 ms on Intel here).
    fn floor(&mut self) -> f64 {
        let mut v: Vec<f64> = (0..15).map(|_| self.timed(|_| {}).1).collect();
        v.sort_by(f64::total_cmp);
        v[7]
    }

    /// Runs `work` between two GPU timestamps: (CPU ms of `work`, GPU ms between them).
    fn timed(&mut self, work: impl FnOnce(&mut Self)) -> (f64, f64) {
        let mut e = self.device.create_command_encoder(&Default::default());
        for _ in 0..4 {
            e.copy_buffer_to_buffer(&self.busy.0, 0, &self.busy.1, 0, self.busy.0.size());
        }
        e.write_timestamp(&self.queries, 0);
        self.queue.submit([e.finish()]);
        let t = Instant::now();
        work(self);
        let cpu = t.elapsed().as_secs_f64() * 1e3;
        let mut e = self.device.create_command_encoder(&Default::default());
        e.write_timestamp(&self.queries, 1);
        e.resolve_query_set(&self.queries, 0..2, &self.resolve, 0);
        e.copy_buffer_to_buffer(&self.resolve, 0, &self.read, 0, 16);
        self.queue.submit([e.finish()]);
        self.read
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.expect("map"));
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .expect("poll");
        let ticks: Vec<u64> = {
            let d = self.read.slice(..).get_mapped_range().expect("mapped");
            d.chunks_exact(8)
                .map(|c| u64::from_le_bytes(c.try_into().expect("8 bytes")))
                .collect()
        };
        self.read.unmap();
        let ns =
            ticks[1].saturating_sub(ticks[0]) as f64 * f64::from(self.queue.get_timestamp_period());
        (cpu, ns / 1e6)
    }
}

/// One benchmark run: the editor, its frame scene and layers, the clock, and the samples.
struct Bench {
    ed: Editor,
    scene: Scene,
    layers: Layers,
    now: u64,
    gpu: Option<Gpu>,
    cpu: Vec<f64>,
    vello: Vec<f64>,
    gpu_ms: Vec<f64>,
}

impl Bench {
    /// One frame as the window runs it; records its times when `measure`.
    fn frame(&mut self, ev: &InputEvents, dt_ms: u64, measure: bool) {
        self.now += dt_ms;
        let t = Instant::now();
        self.ed.update(ev, VIEW, self.now);
        self.scene.reset();
        self.layers.begin_frame(Affine::IDENTITY);
        let mut p = ScenePainter {
            scene: &mut self.scene,
            base: Affine::IDENTITY,
            layers: Some(&mut self.layers),
        };
        self.ed.paint(&mut p);
        let cpu = t.elapsed().as_secs_f64() * 1e3;
        let rendered = self
            .gpu
            .as_mut()
            .map(|g| g.render(&self.scene, &mut self.layers));
        if measure {
            self.cpu.push(cpu);
            if let Some((v, g)) = rendered {
                self.vello.push(v);
                self.gpu_ms.push(g);
            }
        }
    }

    /// Prints p50/p95/p99 of the samples since the last report and clears them.
    fn report(&mut self, what: &str) {
        let pct = |v: &mut Vec<f64>| {
            if v.is_empty() {
                return "-".to_owned();
            }
            v.sort_by(f64::total_cmp);
            let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
            let s = format!("{:.3} / {:.3} / {:.3}", at(0.5), at(0.95), at(0.99));
            v.clear();
            s
        };
        let n = self.cpu.len();
        let reused = self
            .gpu
            .as_mut()
            .map_or(0, |g| std::mem::take(&mut g.reused));
        let (cpu, vello, gpu) = (
            pct(&mut self.cpu),
            pct(&mut self.vello),
            pct(&mut self.gpu_ms),
        );
        eprintln!(
            "  {what:<8} {n:>3} frames ({reused} without vello)  CPU update+paint p50/p95/p99 \
             {cpu} ms  render calls {vello} ms  GPU {gpu} ms"
        );
    }
}

fn event(e: InputEvent) -> InputEvents {
    let mut ev = InputEvents::default();
    ev.push(e);
    ev
}

/// Pan (middle drag), zoom (wheel notches, then the settle frame) and a local edit (delete a
/// wire, undo) on an `n`-object schematic.
fn frame_times(n: u32) {
    let dir = fixture("inv", &format!("perf-{n}"));
    std::fs::write(dir.join("big.shcomp"), schematic(n)).unwrap();
    let t0 = Instant::now();
    let mut ed = Editor::new(Options {
        no_default_ui: false,
        path: Some(dir.join("big.shcomp")),
        seed: 1,
        ..Default::default()
    })
    .unwrap();
    ed.update(&InputEvents::default(), VIEW, 0);
    let open = t0.elapsed().as_secs_f64() * 1e3;
    let (objs, prims) = (ed.scene().objs.len(), ed.scene().prims.len());
    assert!(objs >= n as usize, "{objs} objects");
    let mut gpu = Gpu::new();
    let floor = gpu.as_mut().map_or(0.0, Gpu::floor);
    eprintln!(
        "{n} objects ({objs} drawn, {prims} primitives): open + first build {open:.0} ms; GPU: {} \
         (timestamp floor {floor:.3} ms, included in the GPU numbers)",
        gpu.as_ref().map_or("none (skipped)", |g| g.name.as_str())
    );
    let mut b = Bench {
        ed,
        scene: Scene::new(),
        layers: Layers::default(),
        now: 0,
        gpu,
        cpu: Vec::new(),
        vello: Vec::new(),
        gpu_ms: Vec::new(),
    };
    // The whole schematic in view (zoom full), first frame records the layer.
    b.frame(&InputEvents::default(), 1000, false);

    // Pan: a middle-button drag, 6 px per 8 ms frame, 720 px one way then back (it leaves the
    // recorded margin a few times: those frames re-record and show in p95/p99).
    let (cx, cy) = (640.0f32, 400.0f32);
    b.frame(
        &event(InputEvent::PointerMove { x: 300.0, y: cy }),
        8,
        false,
    );
    let mid = Button::Middle;
    b.frame(
        &event(InputEvent::PointerDown {
            x: 300.0,
            y: cy,
            button: mid,
        }),
        8,
        false,
    );
    let mut x = 300.0f32;
    for i in 0..240 {
        x += if (i / 120) % 2 == 0 { 6.0 } else { -6.0 };
        b.frame(&event(InputEvent::PointerMove { x, y: cy }), 8, true);
    }
    b.frame(
        &event(InputEvent::PointerUp {
            x,
            y: cy,
            button: mid,
        }),
        8,
        false,
    );
    b.report("pan");

    // Zoom: bursts of 6 wheel notches at the pointer, 16 ms apart, alternately in and out,
    // each followed by the frame that redraws crisp once the zoom has settled (counted too).
    // The wheel turns where the pointer is (as winit reports it).
    b.frame(&event(InputEvent::PointerMove { x: cx, y: cy }), 8, false);
    for burst in 0..10 {
        let dy = if burst % 2 == 0 { 1.0 } else { -1.0 };
        for _ in 0..6 {
            let wheel = InputEvent::Wheel {
                x: cx,
                y: cy,
                dx: 0.0,
                dy,
            };
            b.frame(&event(wheel), 16, true);
        }
        // A stretched last frame asks for its crisp frame; a recorded one is crisp already.
        if let Some(wake) = b.ed.wake_ms() {
            assert!(wake <= 120, "the crisp frame is due within the settle time");
            b.frame(&InputEvents::default(), u64::from(wake) + 1, true);
        }
        assert_eq!(b.ed.wake_ms(), None, "idle once crisp (D39 idle 0%)");
    }
    b.report("zoom");

    // A local edit: select a wire, delete it, undo; the delete and undo frames are measured.
    let key = |c: &str| {
        event(InputEvent::Key {
            chord: Chord::parse(c).expect("chord"),
        })
    };
    for k in 0..12 {
        let at = b.ed.to_screen((70 * U + 200 * U * (k % 4), -30 * U));
        b.frame(
            &event(InputEvent::PointerMove { x: at.0, y: at.1 }),
            50,
            false,
        );
        let down = InputEvent::PointerDown {
            x: at.0,
            y: at.1,
            button: Button::Left,
        };
        let up = InputEvent::PointerUp {
            x: at.0,
            y: at.1,
            button: Button::Left,
        };
        let mut click = event(down);
        click.push(up);
        b.frame(&click, 50, false);
        assert_eq!(b.ed.selection().len(), 1, "a wire under the pointer");
        let before = b.ed.scene().objs.len();
        b.frame(&key("Del"), 500, true);
        assert_eq!(b.ed.scene().objs.len(), before - 1, "deleted");
        b.frame(&key("U"), 500, true);
        assert_eq!(b.ed.scene().objs.len(), before, "undone");
    }
    b.report("edit");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn frame_times_10k() {
    frame_times(10_000);
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn frame_times_100k() {
    frame_times(100_000);
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn frame_times_1m() {
    frame_times(1_000_000);
}
