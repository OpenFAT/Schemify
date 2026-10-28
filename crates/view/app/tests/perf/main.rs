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
use schemify_render::gpu::{InstanceRows, Pipelines};
use schemify_render::kurbo::Affine;
use schemify_render::wgpu;
use schemify_render::{Batch, GpuPainter, block_on};
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
    /// Target size in device px, and device px per logical px.
    size: (u32, u32),
    scale: f64,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// The frame's pipelines, row buffers and atlas texture.
    pipes: Pipelines,
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
        let pipes = Pipelines::new(&device, wgpu::TextureFormat::Rgba8Unorm);
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
            scale,
            device,
            queue,
            pipes,
            surface,
            surface_tex,
            queries,
            resolve,
            read,
            busy,
        })
    }

    /// Renders one painted frame exactly as the window does (the rows', the batch's and the
    /// atlas's uploads, then the one pass into the surface), returning (CPU ms of the render
    /// calls, GPU ms between timestamps around them).
    fn render(&mut self, batch: &mut Batch, rows: &mut InstanceRows) -> (f64, f64) {
        self.timed(|g| {
            let (w, h) = g.size;
            let mut e = g.device.create_command_encoder(&Default::default());
            g.pipes.upload(&g.device, &g.queue, rows);
            g.pipes.upload_batch(&g.device, &g.queue, batch);
            let canvas = rows.frame.take();
            let (queue, surface) = (&g.queue, &g.surface);
            let black = wgpu::Color::BLACK;
            g.pipes.draw(
                &mut e,
                queue,
                surface,
                black,
                rows,
                batch,
                canvas.as_ref(),
                g.scale,
                (w, h),
            );
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

/// One benchmark run: the editor, its frame batch, the clock, and the samples.
struct Bench {
    ed: Editor,
    caches: schemify_app::paint::Caches,
    /// Device px per logical px (the window's scale factor).
    scale: f64,
    batch: Batch,
    /// The canvas's GPU instance rows (the window's).
    rows: InstanceRows,
    now: u64,
    gpu: Option<Gpu>,
    cpu: Vec<f64>,
    /// CPU ms of the render calls (uploads and the pass).
    render: Vec<f64>,
    gpu_ms: Vec<f64>,
    /// Atlas and instance bytes each measured frame uploaded.
    atlas: Vec<f64>,
    instances: Vec<f64>,
}

impl Bench {
    /// One frame as the window runs it; records its times when `measure`.
    fn frame(&mut self, ev: &InputEvents, dt_ms: u64, measure: bool) {
        self.now += dt_ms;
        let t = Instant::now();
        self.ed.update(ev, VIEW, self.now);
        self.batch.begin_frame();
        let mut p = GpuPainter {
            batch: &mut self.batch,
            base: Affine::scale(self.scale),
            rows: Some(&mut self.rows),
        };
        schemify_app::paint::frame(&mut self.ed, &mut self.caches, &mut p);
        let cpu = t.elapsed().as_secs_f64() * 1e3;
        let rendered = self.gpu.as_mut().map(|g| {
            let times = g.render(&mut self.batch, &mut self.rows);
            (times, g.pipes.take_batch_uploaded())
        });
        self.rows.frame = None; // the render took it; without a GPU nobody does
        if measure {
            self.cpu.push(cpu);
            if let Some(((v, g), (atlas, instances))) = rendered {
                self.render.push(v);
                self.gpu_ms.push(g);
                self.atlas.push(atlas as f64 / 1024.0);
                self.instances.push(instances as f64 / 1024.0);
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
        // Row bytes the frames since the last report uploaded (an edit's cost on the GPU side).
        let uploaded = self.gpu.as_mut().map_or(0, |g| g.pipes.take_uploaded());
        let (cpu, render, gpu, atlas, instances) = (
            pct(&mut self.cpu),
            pct(&mut self.render),
            pct(&mut self.gpu_ms),
            pct(&mut self.atlas),
            pct(&mut self.instances),
        );
        eprintln!(
            "  {what:<8} {n:>3} frames  CPU update+paint p50/p95/p99 {cpu} ms  render calls \
             {render} ms  GPU {gpu} ms  rows uploaded {} KiB  atlas {atlas} KiB/frame  \
             instances {instances} KiB/frame",
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
            self.frame(&InputEvents::default(), 1, true);
            out.push(self.cpu[c]);
            self.cpu.truncate(c);
            self.render.truncate(v);
            self.gpu_ms.truncate(g);
            self.atlas.truncate(v);
            self.instances.truncate(v);
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
