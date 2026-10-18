//! D39 budgets through the real frame spine: open, frame times while panning and zooming, a
//! local edit round trip (10k / 100k / 1M objects), every other interactive frame (hover,
//! select, drag, typing in a field, opening a menu; `interactions_100k`, D39 amended
//! 2026-10-18: under 1 ms of CPU each), and a var edit with 1k dependents.
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
    eprintln!("PERF app.var_edit_1k_dependents_ms {:.3}", plain[4]);
    eprintln!("PERF app.var_edit_1k_dependents_gmid_ms {:.3}", plugin[4]);
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
    /// Target size in device px.
    size: (u32, u32),
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
    /// A GPU rendering `scale` device px per logical px of [`VIEW`].
    fn new(scale: f64) -> Option<Gpu> {
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
                        width: (f64::from(VIEW.0) * scale) as u32,
                        height: (f64::from(VIEW.1) * scale) as u32,
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
            size: (
                (f64::from(VIEW.0) * scale) as u32,
                (f64::from(VIEW.1) * scale) as u32,
            ),
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
            let (w, h) = g.size;
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
    /// Device px per logical px (the window's scale factor).
    scale: f64,
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
        let base = Affine::scale(self.scale);
        self.layers.begin_frame(base);
        let mut p = ScenePainter {
            scene: &mut self.scene,
            base,
            layers: Some(&mut self.layers),
        };
        let drawn = self.ed.paint(&mut p);
        self.ed.painted(drawn);
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

    /// Prints p50/p95/p99 of the samples since the last report, clears them, and returns the
    /// CPU p50, p95 and p99.
    fn report(&mut self, what: &str) -> [f64; 3] {
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
        let mut sorted = self.cpu.clone();
        sorted.sort_by(f64::total_cmp);
        let q = |q: f64| {
            let i = ((n.max(1) - 1) as f64 * q).round() as usize;
            sorted.get(i).copied().unwrap_or(0.0)
        };
        let cpu_pct = [q(0.5), q(0.95), q(0.99)];
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
        cpu_pct
    }

    /// Runs the ticks the editor asks for at once (`wake_ms() == Some(0)`: the plugin snapshot
    /// after an edit, the rest of a partly recorded canvas), as the window does after an input
    /// frame; returns their CPU times (not counted with the input frames).
    fn ticks(&mut self) -> Vec<f64> {
        let mut out = Vec::new();
        for _ in 0..64 {
            if self.ed.wake_ms() != Some(0) {
                break;
            }
            let (c, v, g) = (self.cpu.len(), self.vello.len(), self.gpu_ms.len());
            self.frame(&InputEvents::default(), 1, true);
            out.push(self.cpu[c]);
            self.cpu.truncate(c);
            self.vello.truncate(v);
            self.gpu_ms.truncate(g);
        }
        assert!(out.len() < 64, "ticks end");
        out
    }

    /// A click on UI node `id` (its center) as one frame.
    fn click_node(&mut self, id: &str, dt_ms: u64, measure: bool) {
        let s = self.ed.shell();
        let sym = s
            .strings()
            .find(id)
            .unwrap_or_else(|| panic!("{id} interned"));
        let n = s
            .tables()
            .nodes
            .find(sym)
            .unwrap_or_else(|| panic!("{id} composed"));
        let r = s.layout().rect()[s.layout().row_of(n).expect("laid out")];
        let (x, y) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let mut ev = event(InputEvent::PointerMove { x, y });
        ev.push(InputEvent::PointerDown {
            x,
            y,
            button: Button::Left,
        });
        ev.push(InputEvent::PointerUp {
            x,
            y,
            button: Button::Left,
        });
        self.frame(&ev, dt_ms, measure);
    }
}

/// The ticks' median and largest CPU ms, for the report line.
fn tick_stats(mut t: Vec<f64>) -> String {
    if t.is_empty() {
        return "none".to_owned();
    }
    t.sort_by(f64::total_cmp);
    format!(
        "{} ticks, median {:.3} ms, max {:.3} ms",
        t.len(),
        t[t.len() / 2],
        t[t.len() - 1]
    )
}

/// Asserts a D39 frame budget (release builds only; a debug run only reports).
fn budget(what: &str, got: f64, ms: f64) {
    if cfg!(debug_assertions) {
        return;
    }
    assert!(got <= ms, "D39: {what} {got:.3} ms > {ms} ms budget");
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
    let mut gpu = Gpu::new(1.0);
    let floor = gpu.as_mut().map_or(0.0, Gpu::floor);
    eprintln!(
        "{n} objects ({objs} drawn, {prims} primitives): open + first build {open:.0} ms; GPU: {} \
         (timestamp floor {floor:.3} ms, included in the GPU numbers)",
        gpu.as_ref().map_or("none (skipped)", |g| g.name.as_str())
    );
    let mut b = Bench {
        ed,
        scale: 1.0,
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
    let [pan, _, pan99] = b.report("pan");

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
        // A stretched last frame asks for its crisp frame, which records the new zoom's tiles a
        // few per frame: frames follow until the view is whole (all counted).
        for _ in 0..256 {
            let Some(wake) = b.ed.wake_ms() else { break };
            assert!(wake <= 120, "the crisp frame is due within the settle time");
            b.frame(&InputEvents::default(), u64::from(wake) + 1, true);
        }
        assert_eq!(b.ed.wake_ms(), None, "idle once crisp (D39 idle 0%)");
    }
    let [_, zoom95, zoom99] = b.report("zoom");

    // A selection (click a wire), then a local edit: select a wire, delete it, undo; the
    // click frames, then the delete and undo frames, are measured.
    let key = |c: &str| {
        event(InputEvent::Key {
            chord: Chord::parse(c).expect("chord"),
        })
    };
    let select = |b: &mut Bench, k: i32, measure: bool| {
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
        b.frame(&click, 500, measure);
        assert_eq!(b.ed.selection().len(), 1, "a wire under the pointer");
    };
    for k in 0..12 {
        select(&mut b, k, true);
        b.ticks();
    }
    let [select50, ..] = b.report("select");
    // The edit's tick (the plugin snapshot behind its check pass) runs as the window runs it:
    // after the frame, before the next input.
    let mut ticks = Vec::new();
    for k in 0..12 {
        select(&mut b, k, false);
        ticks.extend(b.ticks());
        let before = b.ed.scene().objs.len();
        b.frame(&key("Del"), 500, true);
        assert_eq!(b.ed.scene().objs.len(), before - 1, "deleted");
        ticks.extend(b.ticks());
        b.frame(&key("U"), 500, true);
        assert_eq!(b.ed.scene().objs.len(), before, "undone");
        ticks.extend(b.ticks());
    }
    let [edit, ..] = b.report("edit");
    eprintln!("  after the edits: {}", tick_stats(ticks));
    if n == 100_000 {
        // The nightly regression gate's metrics (tests/perf/baseline.txt): CPU p50 per frame,
        // p95 of the zoom (its settle frames record).
        eprintln!("PERF app.open_100k_ms {open:.3}");
        eprintln!("PERF app.pan_frame_100k_ms {pan:.3}");
        eprintln!("PERF app.zoom_frame_p95_100k_ms {zoom95:.3}");
        eprintln!("PERF app.select_frame_100k_ms {select50:.3}");
        eprintln!("PERF app.edit_100k_ms {edit:.3}");
        // D39 (amended 2026-10-18): interactive frames under 1 ms of CPU at 100k objects; the
        // tail of a continuous gesture, the median of a dozen clicks (one descheduled frame
        // is a dozen clicks' p95). Measured 2026-10-18 (load ~20): pan p99 0.31, zoom p99
        // 0.54, select p50 0.49.
        budget("pan p99", pan99, 1.0);
        budget("zoom p99", zoom99, 1.0);
        budget("select p50", select50, 1.0);
    }
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

/// With a GPU: every measured frame since the last report was composed from the layer
/// textures without vello (the canvas floats painted over the canvas included).
fn composed_all(b: &Bench) {
    if let Some(g) = &b.gpu {
        assert!(
            g.reused >= b.cpu.len(),
            "{} of {} camera frames composed without vello",
            g.reused,
            b.cpu.len()
        );
    }
}

/// Issue #45: grid dots lag behind pan and zoom. Opens the symbol contact sheet (light content:
/// the picture is mostly grid) at device scale `scale`, pans with a middle drag (`step` px per
/// 8 ms frame) and zooms in bursts of wheel notches, and counts the frames that leave part of
/// the canvas unrecorded (stand-ins or bare background where the grid belongs): `(pan frames
/// left partial, most frames a settled zoom took to show whole)`. `grid` false turns the grid
/// off (its share of the frame). With a GPU, panics unless every camera frame was composed
/// without vello: before the fix the view switcher floating over the canvas sent every frame
/// through vello (5.5 ms GPU per pan frame at 1×, 14 ms at 2×, on the Intel iGPU).
///
/// Prints frame times (`report`) and the partial counts; `grid_keeps_up_with_pan_and_zoom` runs
/// it at 1× and 2×, 6 and 40 px per frame, grid on and off.
fn grid_lag(scale: f64, step: f32, grid: bool) -> (usize, usize) {
    let dir = fixture("symbols", &format!("perf-grid-{scale}-{step}-{grid}"));
    let ed = Editor::new(Options {
        no_default_ui: false,
        path: Some(dir.join("sheet.shcomp")),
        seed: 1,
        ..Default::default()
    })
    .unwrap();
    let mut b = Bench {
        ed,
        scale,
        scene: Scene::new(),
        layers: Layers::default(),
        now: 0,
        gpu: Gpu::new(scale),
        cpu: Vec::new(),
        vello: Vec::new(),
        gpu_ms: Vec::new(),
    };
    let partial = |b: &Bench| b.ed.wake_ms() == Some(0);
    if !grid {
        b.ed.command("options.draw_grid", "");
    }
    b.frame(&InputEvents::default(), 1000, false);
    while partial(&b) {
        b.frame(&InputEvents::default(), 8, false);
    }
    let (cx, cy) = (640.0f32, 400.0f32);
    b.frame(&event(InputEvent::PointerMove { x: cx, y: cy }), 8, false);
    let mid = Button::Middle;
    let down = InputEvent::PointerDown {
        x: cx,
        y: cy,
        button: mid,
    };
    b.frame(&event(down), 8, false);
    let (mut x, mut pan_partial) = (cx, 0);
    for i in 0..240 {
        x += if (i / 60) % 2 == 0 { step } else { -step };
        b.frame(&event(InputEvent::PointerMove { x, y: cy }), 8, true);
        pan_partial += usize::from(partial(&b));
    }
    let up = InputEvent::PointerUp {
        x,
        y: cy,
        button: mid,
    };
    b.frame(&event(up), 8, false);
    while partial(&b) {
        b.frame(&InputEvents::default(), 8, false);
    }
    composed_all(&b);
    b.report("pan");
    let mut zoom_catch_up = 0;
    for burst in 0..10 {
        let dy = if burst % 2 == 0 { 1.0 } else { -1.0 };
        for _ in 0..3 {
            let wheel = InputEvent::Wheel {
                x: cx,
                y: cy,
                dx: 0.0,
                dy,
            };
            b.frame(&event(wheel), 16, true);
        }
        // Frames after the settle frame until the view is whole.
        let mut extra = 0;
        for _ in 0..256 {
            let Some(wake) = b.ed.wake_ms() else { break };
            extra += usize::from(wake == 0);
            b.frame(&InputEvents::default(), u64::from(wake) + 1, true);
        }
        zoom_catch_up = zoom_catch_up.max(extra);
    }
    composed_all(&b);
    b.report("zoom");
    eprintln!(
        "grid {grid} @{scale}x, pan {step} px/frame: {pan_partial}/240 pan frames partial; a \
         settled zoom takes {zoom_catch_up} extra frame(s) to show whole"
    );
    let _ = std::fs::remove_dir_all(&dir);
    (pan_partial, zoom_catch_up)
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn grid_keeps_up_with_pan_and_zoom() {
    for scale in [1.0, 2.0] {
        for step in [6.0, 40.0] {
            for grid in [true, false] {
                grid_lag(scale, step, grid);
            }
        }
    }
}

/// Every other interactive frame on an `n`-object schematic, zoomed in so objects are big
/// enough to hit: hover (the pointer crossing the canvas, the status line following it),
/// select (a click on a wire), drag (a press on the selected wire and 60 moves: the move
/// preview), the drop that commits it (an edit) and the tick after it, opening a menu, and
/// typing into a properties dialog field. Pan and zoom are [`frame_times`].
fn interactions(n: u32) {
    let dir = fixture("inv", &format!("perf-int-{n}"));
    std::fs::write(dir.join("big.shcomp"), schematic(n)).unwrap();
    let mut ed = Editor::new(Options {
        no_default_ui: false,
        path: Some(dir.join("big.shcomp")),
        seed: 1,
        ..Default::default()
    })
    .unwrap();
    ed.update(&InputEvents::default(), VIEW, 0);
    let mut b = Bench {
        ed,
        scale: 1.0,
        scene: Scene::new(),
        layers: Layers::default(),
        now: 0,
        gpu: Gpu::new(1.0),
        cpu: Vec::new(),
        vello: Vec::new(),
        gpu_ms: Vec::new(),
    };
    let settle = |b: &mut Bench| {
        for _ in 0..256 {
            if b.ed.wake_ms().is_none() {
                break;
            }
            b.frame(&InputEvents::default(), 200, false);
        }
        assert_eq!(b.ed.wake_ms(), None, "settled");
    };
    b.frame(&InputEvents::default(), 1000, false);
    settle(&mut b);
    let (cx, cy) = (640.0f32, 400.0f32);
    b.frame(&event(InputEvent::PointerMove { x: cx, y: cy }), 8, false);
    for _ in 0..8 {
        let wheel = InputEvent::Wheel {
            x: cx,
            y: cy,
            dx: 0.0,
            dy: 1.0,
        };
        b.frame(&event(wheel), 16, false);
    }
    settle(&mut b);
    // Hover: 4 px per frame across the canvas; the first frames after a zoom also fill the
    // ring of tiles ahead of the view (within its share of the budget).
    for i in 0..240 {
        let x = 200.0 + (i % 120) as f32 * 4.0;
        let y = cy + (i / 120) as f32 * 3.0;
        b.frame(&event(InputEvent::PointerMove { x, y }), 8, true);
    }
    let [hover, hover95, _] = b.report("hover");
    // Select: wires of the tiles around the view's center.
    let cam = b.ed.camera();
    let tile = f64::from(200 * U);
    let (tx, ty) = (
        (cam.cx / tile).round() as i32 * 200 * U,
        (cam.cy / tile).round() as i32 * 200 * U,
    );
    let wire = |b: &Bench, k: i32| {
        b.ed.to_screen((tx + 70 * U + 200 * U * (k % 4 - 2), ty - 30 * U))
    };
    let press = |at: (f32, f32)| InputEvent::PointerDown {
        x: at.0,
        y: at.1,
        button: Button::Left,
    };
    let release = |at: (f32, f32)| InputEvent::PointerUp {
        x: at.0,
        y: at.1,
        button: Button::Left,
    };
    for k in 0..12 {
        let at = wire(&b, k);
        b.frame(
            &event(InputEvent::PointerMove { x: at.0, y: at.1 }),
            50,
            false,
        );
        let mut click = event(press(at));
        click.push(release(at));
        b.frame(&click, 500, true);
        assert_eq!(b.ed.selection().len(), 1, "a wire under the pointer");
        b.ticks();
    }
    let [select, ..] = b.report("select");
    // Drag the selected wire (past the double-click time), then drop it.
    let at = wire(&b, 11);
    b.frame(&event(press(at)), 1000, false);
    for i in 1..=60 {
        let to = InputEvent::PointerMove {
            x: at.0 + i as f32 * 2.0,
            y: at.1 + i as f32,
        };
        b.frame(&event(to), 8, true);
    }
    assert!(!b.ed.overlay().is_empty(), "the move preview");
    let [drag, drag95, _] = b.report("drag");
    let to = (at.0 + 120.0, at.1 + 60.0);
    b.frame(&event(release(to)), 8, true);
    b.report("drop");
    eprintln!("  after the drop: {}", tick_stats(b.ticks()));
    // Open the Tools menu (a click on its title), close it.
    let escape = event(InputEvent::Key {
        chord: Chord::parse("Escape").expect("chord"),
    });
    for _ in 0..8 {
        b.click_node("menu.tools", 300, true);
        b.frame(&escape, 300, false);
    }
    let [menu, ..] = b.report("menu");
    // Type into the first field of an instance's properties dialog, one character a frame.
    let inst = b.ed.to_screen((tx, ty));
    let mut click = event(InputEvent::PointerMove {
        x: inst.0,
        y: inst.1,
    });
    click.push(press(inst));
    click.push(release(inst));
    b.frame(&click, 500, false);
    let q = event(InputEvent::Key {
        chord: Chord::parse("q").expect("chord"),
    });
    b.frame(&q, 300, false);
    b.click_node("dialog.props.val.0", 300, false);
    for i in 0..60 {
        let mut ev = InputEvents::default();
        ev.push_text(if i % 2 == 0 { "a" } else { "b" });
        b.frame(&ev, 30, true);
    }
    let [typing, typing95, _] = b.report("type");
    if n == 100_000 {
        eprintln!("PERF app.hover_frame_100k_ms {hover:.3}");
        eprintln!("PERF app.click_select_100k_ms {select:.3}");
        eprintln!("PERF app.drag_frame_100k_ms {drag:.3}");
        eprintln!("PERF app.menu_open_100k_ms {menu:.3}");
        eprintln!("PERF app.type_frame_100k_ms {typing:.3}");
        // D39 (amended 2026-10-18): under 1 ms of CPU per interactive frame at 100k objects:
        // the p95 of the continuous ones, the median of the clicks (see `frame_times`).
        // Measured 2026-10-18 (load ~20): hover p95 0.21, drag p95 0.21, type p95 0.25,
        // select p50 0.50, menu p50 0.20 (its first open, which shapes the rows: 0.50).
        budget("hover p95", hover95, 1.0);
        budget("drag p95", drag95, 1.0);
        budget("type p95", typing95, 1.0);
        budget("select p50", select, 1.0);
        budget("menu p50", menu, 1.0);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn interactions_100k() {
    interactions(100_000);
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn interactions_1m() {
    interactions(1_000_000);
}

/// A key press as the window delivers it (modifiers down, the key, modifiers up).
fn key_event(c: &str) -> InputEvents {
    let chord = Chord::parse(c).expect("chord");
    let mut ev = event(InputEvent::Mods { mods: chord.mods() });
    ev.push(InputEvent::Key { chord });
    ev.push(InputEvent::Mods {
        mods: schemify_ui::Mods::default(),
    });
    ev
}

/// Every edit frame on an `n`-object schematic, zoomed in as [`interactions`] is: the first
/// edit of the document (it creates the journal), then a dozen each of delete, undo, redo,
/// move (the drop of a drag), a wire drawn with the wire tool, a rename (an instance's name
/// through its properties dialog) and a parameter change (its first parameter). Each kind's
/// frames are reported apart, with the ticks the editor asks for after them (the window runs
/// those before the next input). Returns (kind, CPU p50/p95/p99, largest tick) per kind.
fn edit_frames(n: u32) -> Vec<(&'static str, [f64; 3], f64)> {
    let dir = fixture("inv", &format!("perf-edit-{n}"));
    std::fs::write(dir.join("big.shcomp"), schematic(n)).unwrap();
    let mut ed = Editor::new(Options {
        no_default_ui: false,
        path: Some(dir.join("big.shcomp")),
        seed: 1,
        ..Default::default()
    })
    .unwrap();
    ed.update(&InputEvents::default(), VIEW, 0);
    let mut b = Bench {
        ed,
        scale: 1.0,
        scene: Scene::new(),
        layers: Layers::default(),
        now: 0,
        gpu: Gpu::new(1.0),
        cpu: Vec::new(),
        vello: Vec::new(),
        gpu_ms: Vec::new(),
    };
    let settle = |b: &mut Bench| {
        for _ in 0..256 {
            if b.ed.wake_ms().is_none() {
                break;
            }
            b.frame(&InputEvents::default(), 200, false);
        }
        assert_eq!(b.ed.wake_ms(), None, "settled");
    };
    b.frame(&InputEvents::default(), 1000, false);
    settle(&mut b);
    let (cx, cy) = (640.0f32, 400.0f32);
    b.frame(&event(InputEvent::PointerMove { x: cx, y: cy }), 8, false);
    for _ in 0..8 {
        let wheel = InputEvent::Wheel {
            x: cx,
            y: cy,
            dx: 0.0,
            dy: 1.0,
        };
        b.frame(&event(wheel), 16, false);
    }
    settle(&mut b);
    let cam = b.ed.camera();
    let tile = f64::from(200 * U);
    let (tx, ty) = (
        (cam.cx / tile).round() as i32 * 200 * U,
        (cam.cy / tile).round() as i32 * 200 * U,
    );
    let wire = |b: &Bench, k: i32| {
        b.ed.to_screen((tx + 70 * U + 200 * U * (k % 4 - 2), ty - 30 * U))
    };
    let press = |at: (f32, f32)| InputEvent::PointerDown {
        x: at.0,
        y: at.1,
        button: Button::Left,
    };
    let release = |at: (f32, f32)| InputEvent::PointerUp {
        x: at.0,
        y: at.1,
        button: Button::Left,
    };
    let click = |b: &mut Bench, at: (f32, f32)| {
        b.frame(
            &event(InputEvent::PointerMove { x: at.0, y: at.1 }),
            50,
            false,
        );
        let mut c = event(press(at));
        c.push(release(at));
        b.frame(&c, 500, false);
        b.ticks();
    };
    let mut out = Vec::new();
    let mut ticks: Vec<f64> = Vec::new();
    let mut done = |b: &mut Bench, what: &'static str, ticks: &mut Vec<f64>| {
        let pct = b.report(what);
        let worst = ticks.iter().copied().fold(0.0, f64::max);
        eprintln!("  after {what}: {}", tick_stats(std::mem::take(ticks)));
        out.push((what, pct, worst));
    };
    // The first edit of the document: its journal starts.
    let at = wire(&b, 0);
    click(&mut b, at);
    assert_eq!(b.ed.selection().len(), 1, "a wire under the pointer");
    b.frame(&key_event("Del"), 500, true);
    ticks.extend(b.ticks());
    done(&mut b, "first", &mut ticks);
    b.frame(&key_event("U"), 500, false);
    b.ticks();
    // Delete, undo, redo (then undo again, so the document stays as it was).
    let objs = b.ed.scene().objs.len();
    let (mut del, mut undo, mut redo) = (Vec::new(), Vec::new(), Vec::new());
    let (mut tdel, mut tundo, mut tredo) = (Vec::new(), Vec::new(), Vec::new());
    for k in 0..12 {
        let at = wire(&b, k);
        click(&mut b, at);
        assert_eq!(b.ed.selection().len(), 1, "a wire under the pointer");
        for (key, times, t, len) in [
            ("Del", &mut del, &mut tdel, objs - 1),
            ("U", &mut undo, &mut tundo, objs),
            ("Shift+U", &mut redo, &mut tredo, objs - 1),
        ] {
            b.frame(&key_event(key), 500, true);
            times.push(b.cpu.pop().expect("measured"));
            assert_eq!(b.ed.scene().objs.len(), len, "{key}");
            t.extend(b.ticks());
        }
        b.frame(&key_event("U"), 500, false);
        b.ticks();
    }
    for (what, times, t) in [
        ("delete", del, tdel),
        ("undo", undo, tundo),
        ("redo", redo, tredo),
    ] {
        b.cpu = times;
        ticks = t;
        done(&mut b, what, &mut ticks);
    }
    // Move: press on a wire, drag it, drop it (the drop commits).
    for k in 0..12 {
        let at = wire(&b, k);
        click(&mut b, at);
        b.frame(&event(press(at)), 1000, false);
        for i in 1..=8 {
            let to = InputEvent::PointerMove {
                x: at.0 + i as f32 * 2.0,
                y: at.1 + i as f32,
            };
            b.frame(&event(to), 8, false);
        }
        b.frame(&event(release((at.0 + 16.0, at.1 + 8.0))), 8, true);
        ticks.extend(b.ticks());
        b.frame(&key_event("U"), 500, false);
        b.ticks();
    }
    done(&mut b, "move", &mut ticks);
    // A wire drawn with the wire tool: `w` at a free point, then a click.
    for k in 0..12 {
        let from = b
            .ed
            .to_screen((tx - 90 * U + 200 * U * (k % 4 - 2), ty + 90 * U));
        b.frame(
            &event(InputEvent::PointerMove {
                x: from.0,
                y: from.1,
            }),
            50,
            false,
        );
        b.frame(&key_event("w"), 300, false);
        let to = (from.0 + 30.0, from.1);
        b.frame(
            &event(InputEvent::PointerMove { x: to.0, y: to.1 }),
            50,
            false,
        );
        let mut c = event(press(to));
        c.push(release(to));
        b.frame(&c, 500, true);
        ticks.extend(b.ticks());
        b.frame(&key_event("Escape"), 300, false);
        b.frame(&key_event("U"), 500, false);
        b.ticks();
    }
    done(&mut b, "wire", &mut ticks);
    // Property changes through an instance's properties dialog: field 0 is its name, field
    // 1 its first parameter.
    for (what, field) in [("rename", 0), ("param", 1)] {
        for k in 0..12 {
            let inst = b.ed.to_screen((tx + 200 * U * (k % 4 - 2), ty));
            click(&mut b, inst);
            b.frame(&key_event("q"), 300, false);
            b.click_node(&format!("dialog.props.val.{field}"), 300, false);
            b.frame(&key_event("Ctrl+A"), 30, false);
            let mut ev = InputEvents::default();
            ev.push_text(match (field, k % 2) {
                (0, 0) => "MA",
                (0, _) => "MB",
                (_, 0) => "2",
                _ => "3",
            });
            b.frame(&ev, 30, false);
            b.click_node("dialog.props.ok", 300, true);
            ticks.extend(b.ticks());
        }
        done(&mut b, what, &mut ticks);
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn edit_frames_100k() {
    let r = edit_frames(100_000);
    for (what, [p50, p95, p99], tick) in &r {
        eprintln!(
            "PERF app.edit_{what}_100k_ms {p50:.3} (p95 {p95:.3}, p99 {p99:.3}, tick max {tick:.3})"
        );
    }
}
