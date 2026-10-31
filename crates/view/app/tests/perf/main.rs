//! D39 budgets through the real frame spine: open, frame times while panning and zooming, a
//! local edit round trip (10k / 100k / 1M objects), every other interactive frame (hover,
//! select, drag, typing in a field, opening a menu; `interactions_100k`, D39 amended
//! 2026-10-18: under 1 ms of CPU each), and a var edit with 1k dependents.
//!
//! A frame is `Editor::update` + `Editor::paint` into the GPU painter's batch and the canvas's
//! instance rows, as the window does; its CPU time is reported as p50/p95/p99. With a GPU
//! adapter that supports timestamps inside encoders, each frame is also rendered (the uploads
//! and the one pass: the batch with the canvas stages in it) and its GPU time reported, with
//! the row bytes uploaded and the atlas and instance bytes per frame; without one the GPU
//! columns are skipped.
//! Pick the adapter with `SCHEMIFY_GPU=<name substring>` (default: the first non-CPU adapter).
//!
//! `cargo test --release -p schemify-app --test perf -- --ignored --nocapture --test-threads=1`
//! (NixOS: put the 64-bit vulkan-loader `lib` and `/run/opengl-driver/lib` on `LD_LIBRARY_PATH`.)

#[path = "../common/mod.rs"]
mod common;
mod edits;
mod frames;
mod gpu;
mod interactions;

use std::fmt::Write;
use std::time::Instant;

use common::VIEW;
use schemify_editor::editor::Editor;
use schemify_render::block_on;
use schemify_render::egui;
use schemify_render::egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use schemify_render::gpu::{CanvasGpu, InstanceRows};
use schemify_render::wgpu;
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

/// A headless GPU with timestamp queries: renders each benchmark frame as the window does.
struct Gpu {
    name: String,
    /// Target size in device px.
    size: (u32, u32),
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// egui-wgpu's renderer with the canvas (pipelines and rows) in its callback resources.
    renderer: Renderer,
    /// The stand-in for the window surface.
    surface: wgpu::TextureView,
    /// The texture behind `surface` (read back by the oracle tests).
    surface_tex: wgpu::Texture,
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
            required_features: need,
            ..Default::default()
        }))
        .ok()?;
        let texture_of = |usage| {
            device.create_texture(&wgpu::TextureDescriptor {
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
        };
        let surface_tex =
            texture_of(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC);
        let surface = surface_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let mut renderer = Renderer::new(
            &device,
            format,
            RendererOptions {
                dithering: false,
                ..RendererOptions::default()
            },
        );
        renderer
            .callback_resources
            .insert(CanvasGpu::new(&device, format));
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
            surface,
            surface_tex,
            queries,
            resolve,
            read,
            busy,
        })
    }

    /// The canvas: its pipelines and the rows the frames pack.
    fn canvas(&mut self) -> &mut CanvasGpu {
        self.renderer
            .callback_resources
            .get_mut::<CanvasGpu>()
            .expect("the canvas is a resource")
    }

    /// Renders one frame's tessellated output exactly as the window does (textures, rows and
    /// camera uploaded, egui's buffers, then the one pass into the surface), returning (CPU ms
    /// of the render calls, GPU ms between timestamps around them).
    fn render(
        &mut self,
        prims: &[egui::ClippedPrimitive],
        textures: &egui::TexturesDelta,
        ppp: f32,
    ) -> (f64, f64) {
        self.timed(|g| {
            let (w, h) = g.size;
            let screen = ScreenDescriptor {
                size_in_pixels: [w, h],
                pixels_per_point: ppp,
            };
            for (id, deltas) in &textures.set {
                for d in deltas {
                    g.renderer.update_texture(&g.device, &g.queue, *id, d);
                }
            }
            let (device, queue) = (g.device.clone(), g.queue.clone());
            g.canvas().prepare(&device, &queue, &screen);
            let mut e = g.device.create_command_encoder(&Default::default());
            let mut cmds = g
                .renderer
                .update_buffers(&g.device, &g.queue, &mut e, prims, &screen);
            {
                let pass = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("bench"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &g.surface,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                g.renderer
                    .render(&mut pass.forget_lifetime(), prims, &screen);
            }
            cmds.push(e.finish());
            g.queue.submit(cmds);
            for id in &textures.free {
                g.renderer.free_texture(id);
            }
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

/// Whether a benchmark frame's times go into the samples.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Measure {
    /// A measured frame: its times are recorded.
    Record,
    /// A warm-up or set-up frame: run, not recorded.
    Skip,
}

/// One benchmark run: the editor, its paint caches, the clock, and the samples.
struct Bench {
    ed: Editor,
    caches: schemify_app::paint::Caches,
    /// The canvas's instance rows when there is no GPU (with one, its canvas holds them).
    rows: InstanceRows,
    now: u64,
    gpu: Option<Gpu>,
    cpu: Vec<f64>,
    /// CPU ms of the render calls (uploads and the pass).
    render: Vec<f64>,
    gpu_ms: Vec<f64>,
    /// egui texture and mesh bytes each measured frame uploaded (KiB).
    textures: Vec<f64>,
    meshes: Vec<f64>,
}

impl Bench {
    /// A bench over `ed` at `scale` device px per logical px, rendering on `gpu` when given.
    fn new(ed: Editor, scale: f64, gpu: Option<Gpu>) -> Bench {
        assert!(scale > 0.0, "a positive scale");
        ed.shell().ctx().set_pixels_per_point(scale as f32);
        Bench {
            ed,
            caches: Default::default(),
            rows: InstanceRows::default(),
            now: 0,
            gpu,
            cpu: Vec::new(),
            render: Vec::new(),
            gpu_ms: Vec::new(),
            textures: Vec::new(),
            meshes: Vec::new(),
        }
    }

    /// One frame as the window runs it (the editor's egui pass with the canvas packed, egui's
    /// tessellation, then the render); records its times when `measure`. Returns whether a
    /// table changed.
    fn frame(&mut self, ev: &InputEvents, dt_ms: u64, measure: Measure) -> bool {
        self.now += dt_ms;
        let t = Instant::now();
        let rows = match self.gpu.as_mut() {
            Some(g) => &mut g.canvas().rows,
            None => &mut self.rows,
        };
        let mut f = schemify_app::paint::frame(
            &mut self.ed,
            &mut self.caches,
            ev.raw(VIEW),
            self.now,
            rows,
        );
        let ctx = self.ed.shell().ctx().clone();
        let ppp = f.output.pixels_per_point;
        let prims = ctx.tessellate(std::mem::take(&mut f.output.shapes), ppp);
        let cpu = t.elapsed().as_secs_f64() * 1e3;
        let tex: usize = f
            .output
            .textures_delta
            .set
            .values()
            .flatten()
            .map(|d| d.image.width() * d.image.height() * 4)
            .sum();
        let mesh: usize = prims
            .iter()
            .map(|p| match &p.primitive {
                egui::epaint::Primitive::Mesh(m) => m.vertices.len() * 20 + m.indices.len() * 4,
                egui::epaint::Primitive::Callback(_) => 0,
            })
            .sum();
        let rendered = self
            .gpu
            .as_mut()
            .map(|g| g.render(&prims, &f.output.textures_delta, ppp));
        f.output.textures_delta.clear();
        self.rows.frame = None; // without a GPU nobody draws it
        if measure == Measure::Record {
            self.cpu.push(cpu);
            if let Some((v, g)) = rendered {
                self.render.push(v);
                self.gpu_ms.push(g);
                self.textures.push(tex as f64 / 1024.0);
                self.meshes.push(mesh as f64 / 1024.0);
            }
        }
        f.changed
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
        // Row bytes the frames since the last report uploaded (an edit's cost on the GPU side).
        let uploaded = self
            .gpu
            .as_mut()
            .map_or(0, |g| g.canvas().pipelines.take_uploaded());
        let (cpu, render, gpu, textures, meshes) = (
            pct(&mut self.cpu),
            pct(&mut self.render),
            pct(&mut self.gpu_ms),
            pct(&mut self.textures),
            pct(&mut self.meshes),
        );
        eprintln!(
            "  {what:<8} {n:>3} frames  CPU frame (egui pass + tessellate) p50/p95/p99 {cpu} ms  \
             render calls {render} ms  GPU {gpu} ms  rows uploaded {} KiB  textures {textures} \
             KiB/frame  meshes {meshes} KiB/frame",
            uploaded / 1024
        );
        cpu_pct
    }

    /// Runs the ticks the editor asks for at once (`wake_ms() == Some(0)`: the plugin snapshot
    /// after an edit), as the window does after an input
    /// frame; returns their CPU times (not counted with the input frames).
    fn ticks(&mut self) -> Vec<f64> {
        let mut out = Vec::new();
        for _ in 0..64 {
            if self.ed.wake_ms() != Some(0) {
                break;
            }
            let (c, v, g) = (self.cpu.len(), self.render.len(), self.gpu_ms.len());
            self.frame(&InputEvents::default(), 1, Measure::Record);
            out.push(self.cpu[c]);
            self.cpu.truncate(c);
            self.render.truncate(v);
            self.gpu_ms.truncate(g);
            self.textures.truncate(v);
            self.meshes.truncate(v);
        }
        assert!(out.len() < 64, "ticks end");
        out
    }

    /// A click on UI node `id` (its center) as one frame.
    fn click_node(&mut self, id: &str, dt_ms: u64, measure: Measure) {
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
