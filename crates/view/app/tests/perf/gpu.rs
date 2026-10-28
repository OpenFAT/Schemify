//! GPU against CPU: a settled frame equals an uncached one, the GPU editor frame matches the
//! CPU painter's, and the CPU frame times at 100k objects.

use std::time::Instant;

use schemify_editor::editor::{Editor, Options};
use schemify_render::gpu::InstanceRows;
use schemify_render::kurbo::Affine;
use schemify_render::peniko::Color;
use schemify_render::wgpu;
use schemify_render::{Batch, GpuPainter};
use schemify_ui::{Button, InputEvent, InputEvents};

use super::{Bench, Gpu, event, key_event, schematic};
use crate::common::{U, VIEW, fixture};

impl Bench {
    /// Paints and renders one frame (no update), as the window's `RedrawRequested` does.
    fn paint(&mut self) {
        self.batch.begin_frame();
        let mut p = GpuPainter {
            batch: &mut self.batch,
            base: Affine::scale(self.scale),
            rows: Some(&mut self.rows),
        };
        schemify_app::paint::frame(&mut self.ed, &mut self.caches, &mut p);
        if let Some(g) = self.gpu.as_mut() {
            g.render(&mut self.batch, &mut self.rows);
        }
        self.rows.frame = None; // the render took it; without a GPU nobody does
    }

    /// One input batch as the window loop runs it: `update`, a frame only when it reports a
    /// change, then the ticks `wake_ms` asks for (each painted only when its update changed
    /// something) until the editor sleeps. Returns the frames painted.
    fn window(&mut self, ev: &InputEvents, dt_ms: u64) -> usize {
        let mut frames = 0;
        self.now += dt_ms;
        if self.ed.update(ev, VIEW, self.now) {
            self.paint();
            frames += 1;
        }
        for _ in 0..4096 {
            let Some(ms) = self.ed.wake_ms() else {
                return frames;
            };
            self.now += u64::from(ms).max(1);
            if self.ed.update(&InputEvents::default(), VIEW, self.now) {
                self.paint();
                frames += 1;
            }
        }
        panic!("the window never sleeps");
    }

    /// The surface's pixels (RGBA8, row-major).
    fn pixels(&self) -> Vec<u8> {
        let g = self.gpu.as_ref().expect("a GPU");
        let t = &g.surface_tex;
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

    /// The oracle: the share of pixels (a channel off by more than 2) that differ between the
    /// frame the window shows and the same editor state painted afresh (fresh caches, batch,
    /// atlas and rows: every row packed and every mask rasterized in that frame).
    fn stale_share(&mut self) -> f64 {
        let shown = self.pixels();
        let caches = std::mem::take(&mut self.caches);
        let batch = std::mem::take(&mut self.batch);
        let rows = std::mem::take(&mut self.rows);
        self.paint();
        let fresh = self.pixels();
        (self.caches, self.batch, self.rows) = (caches, batch, rows);
        let bad = shown
            .chunks_exact(4)
            .zip(fresh.chunks_exact(4))
            .filter(|(p, q)| p.iter().zip(*q).any(|(x, y)| x.abs_diff(*y) > 2))
            .count();
        bad as f64 / (shown.len() / 4) as f64
    }

    /// A GPU bench on `path` at device scale 1, or `None` without a timestamp-capable GPU.
    fn on_gpu(path: &std::path::Path) -> Option<Bench> {
        let ed = Editor::new(Options {
            no_default_ui: false,
            path: Some(path.to_owned()),
            seed: 1,
            ..Default::default()
        })
        .expect("the fixture project opens");
        Some(Bench {
            caches: Default::default(),
            ed,
            scale: 1.0,
            batch: Batch::default(),
            rows: InstanceRows::default(),
            now: 0,
            gpu: Some(Gpu::new(1.0)?),
            cpu: Vec::new(),
            render: Vec::new(),
            gpu_ms: Vec::new(),
            atlas: Vec::new(),
            instances: Vec::new(),
        })
    }
}

/// Share of pixels a settled frame may differ from the fresh one: patched rows drawn in
/// another slot order blend their antialiased overlaps differently (well under 0.1%); one
/// stale 256-px square is ~6% of the 1280 × 800 view.
const SEAM_SHARE: f64 = 0.005;

/// After pans, zooms, arrow keys and opening another cell, the frame the window settles on
/// (the loop asleep) equals the fresh render of the same state: no stale row from another
/// zoom or cell is left on screen.
#[test]
#[ignore = "needs a GPU adapter"]
fn settled_frame_equals_uncached_frame() {
    let dir = fixture("inv", "perf-settled");
    std::fs::write(dir.join("a.shcomp"), schematic(10_000)).unwrap();
    std::fs::write(dir.join("b.shcomp"), schematic(1_000)).unwrap();
    let Some(mut b) = Bench::on_gpu(&dir.join("a.shcomp")) else {
        eprintln!("no GPU adapter with timestamps: skipped");
        return;
    };
    let check = |b: &mut Bench, what: &str, frames: usize| {
        let stale = b.stale_share();
        eprintln!("{what}: {frames} frames, {:.3}% px off", stale * 100.0);
        assert!(
            stale <= SEAM_SHARE,
            "{what}: {:.2}% px stale",
            stale * 100.0
        );
    };
    let f = b.window(&InputEvents::default(), 1000);
    check(&mut b, "open", f);
    let (cx, cy) = (640.0f32, 400.0f32);
    let f = b.window(&event(InputEvent::PointerMove { x: cx, y: cy }), 16);
    check(&mut b, "move", f);
    for (what, dy, n) in [("zoom in", 1.0, 4), ("zoom out far", -1.0, 12)] {
        let mut f = 0;
        for _ in 0..n {
            let wheel = InputEvent::Wheel {
                x: cx,
                y: cy,
                dx: 0.0,
                dy,
            };
            f += b.window(&event(wheel), 16);
        }
        check(&mut b, what, f);
    }
    for key in ["Left", "Up", "Right", "Down"] {
        let f = b.window(&key_event(key), 16);
        check(&mut b, &format!("arrow {key}"), f);
    }
    let path = dir.join("b.shcomp");
    b.ed.command("file.open", path.to_str().expect("utf-8 path"));
    let f = b.window(&InputEvents::default(), 16);
    check(&mut b, "open cell b", f);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Mean |Δ| per channel the window's GPU frame of the editor may differ from the CPU painter's
/// (the goldens' painter) by: glyph masks at quarter-pixel phases, stroke segments for the
/// large borders (their round joins blended twice at a corner), the canvas pipelines' own
/// coverage. Measured on the RTX 4060: see `gpu_editor_frame_matches_cpu_frame`.
const EDITOR_MEAN_MAX: f64 = 0.5;
/// Share of pixels that may be off by more than a third of the range there.
const EDITOR_FAR_SHARE: f64 = 0.001;

/// The whole editor frame (the default UI, the canvas with its symbol texts, labels and pin
/// names, the status bar) through the GPU painter, as the window draws it, against the CPU
/// painter painting the same state with fresh caches: opened, hovered, with the Tools menu
/// open. Writes the amplified (×8) diff of the first next to the test's scratch files.
#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_editor_frame_matches_cpu_frame() {
    use schemify_render::CpuPainter;
    let dir = fixture("inv", "perf-gpu-cpu");
    let Some(mut b) = Bench::on_gpu(&dir.join("top.shcomp")) else {
        eprintln!("no GPU adapter with timestamps: skipped");
        return;
    };
    let compare = |b: &mut Bench, what: &str| {
        b.paint();
        let gpu = b.pixels();
        let mut cpu = CpuPainter::new(VIEW.0 as u16, VIEW.1 as u16, 1.0);
        let mut caches = schemify_app::paint::Caches::default();
        schemify_app::paint::frame(&mut b.ed, &mut caches, &mut cpu);
        let cpu: Vec<u8> = cpu
            .finish(Color::BLACK)
            .data()
            .iter()
            .flat_map(|c| [c.r, c.g, c.b, c.a])
            .collect();
        let (mut sum, mut far) = (0u64, 0usize);
        for (p, q) in gpu.chunks_exact(4).zip(cpu.chunks_exact(4)) {
            let d = [
                p[0].abs_diff(q[0]),
                p[1].abs_diff(q[1]),
                p[2].abs_diff(q[2]),
            ];
            sum += d.iter().map(|&x| u64::from(x)).sum::<u64>();
            far += usize::from(d.iter().any(|&x| x > 96));
        }
        let n = gpu.len() / 4;
        let mean = sum as f64 / (n * 3) as f64;
        eprintln!("{what}: GPU vs CPU mean |Δ| {mean:.3}/255, {far} of {n} px off by > 96");
        if what == "open" {
            let mut px = schemify_render::Pixmap::new(VIEW.0 as u16, VIEW.1 as u16);
            for (o, (p, q)) in px
                .data_mut()
                .iter_mut()
                .zip(gpu.chunks_exact(4).zip(cpu.chunks_exact(4)))
            {
                let d = |k: usize| p[k].abs_diff(q[k]).saturating_mul(8);
                *o = schemify_render::peniko::color::PremulRgba8 {
                    r: d(0),
                    g: d(1),
                    b: d(2),
                    a: 255,
                };
            }
            let out = format!("{}/editor_frame.diff.png", env!("CARGO_TARGET_TMPDIR"));
            std::fs::write(&out, px.into_png().expect("png")).expect("write");
            eprintln!("amplified diff (×8): {out}");
        }
        assert!(mean <= EDITOR_MEAN_MAX, "{what}: mean |Δ| {mean:.3}");
        assert!(
            far as f64 <= EDITOR_FAR_SHARE * n as f64,
            "{what}: {far} px far off"
        );
    };
    b.window(&InputEvents::default(), 1000);
    compare(&mut b, "open");
    b.window(&event(InputEvent::PointerMove { x: 500.0, y: 380.0 }), 16);
    compare(&mut b, "hover");
    b.click_node("menu.tools", 300, false);
    compare(&mut b, "menu");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Reports p50/p95/p99 of `(update + paint, raster)` samples as one line and clears them.
fn report_cpu(what: &str, t: &mut Vec<(f64, f64)>) {
    let pct = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        let at = |q: f64| v[((v.len().max(1) - 1) as f64 * q).round() as usize];
        format!("{:.3} / {:.3} / {:.3}", at(0.5), at(0.95), at(0.99))
    };
    let n = t.len();
    let (mut paint, mut raster, mut total): (Vec<f64>, Vec<f64>, Vec<f64>) = (
        t.iter().map(|s| s.0).collect(),
        t.iter().map(|s| s.1).collect(),
        t.iter().map(|s| s.0 + s.1).collect(),
    );
    eprintln!(
        "  {what:<8} {n:>3} frames  update+paint {} ms  raster {} ms  total {} ms",
        pct(&mut paint),
        pct(&mut raster),
        pct(&mut total)
    );
    t.clear();
}

/// What a frame costs when the CPU rasterizes it whole (`CpuPainter`: vello_cpu, one thread,
/// a fresh 1280×800 pixmap per frame), for the same gestures as `frame_times` and
/// `interactions` at 100k objects: the cost of a frame with no GPU (the web's last-resort painter).
/// `cargo test --release -p schemify-app --test perf -- --ignored --nocapture cpu_frames`
#[test]
#[ignore = "release-mode benchmark"]
fn cpu_frames_100k() {
    use schemify_render::CpuPainter;

    struct Cpu {
        ed: Editor,
        caches: schemify_app::paint::Caches,
        p: CpuPainter,
        now: u64,
        times: Vec<(f64, f64)>,
    }
    impl Cpu {
        fn frame(&mut self, ev: &InputEvents, dt: u64, measure: bool) {
            self.now += dt;
            let t = Instant::now();
            self.ed.update(ev, VIEW, self.now);
            self.p.reset();
            schemify_app::paint::frame(&mut self.ed, &mut self.caches, &mut self.p);
            let painted = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            std::hint::black_box(self.p.finish(Color::BLACK));
            let raster = t.elapsed().as_secs_f64() * 1e3;
            if measure {
                self.times.push((painted, raster));
            }
        }
        fn settle(&mut self) {
            for _ in 0..1024 {
                if self.ed.wake_ms().is_none() {
                    break;
                }
                self.frame(&InputEvents::default(), 200, false);
            }
        }
        /// A click on a wire of tile `k`, as `frame_times` selects.
        fn select(&mut self, k: i32, measure: bool) {
            let at = self.ed.to_screen((70 * U + 200 * U * (k % 4), -30 * U));
            self.frame(
                &event(InputEvent::PointerMove { x: at.0, y: at.1 }),
                50,
                false,
            );
            let mut click = event(InputEvent::PointerDown {
                x: at.0,
                y: at.1,
                button: Button::Left,
            });
            click.push(InputEvent::PointerUp {
                x: at.0,
                y: at.1,
                button: Button::Left,
            });
            self.frame(&click, 500, measure);
            assert_eq!(self.ed.selection().len(), 1, "a wire under the pointer");
        }
    }

    let n = 100_000u32;
    let dir = fixture("inv", &format!("perf-cpu-{n}"));
    std::fs::write(dir.join("big.shcomp"), schematic(n)).unwrap();
    let ed = Editor::new(Options {
        no_default_ui: false,
        path: Some(dir.join("big.shcomp")),
        seed: 1,
        ..Default::default()
    })
    .unwrap();
    let mut c = Cpu {
        ed,
        caches: Default::default(),
        p: CpuPainter::new(VIEW.0 as u16, VIEW.1 as u16, 1.0),
        now: 0,
        times: Vec::new(),
    };
    c.frame(&InputEvents::default(), 1000, false);
    c.settle();
    let (cx, cy) = (640.0f32, 400.0f32);
    // Pan: as `frame_times`.
    let mid = Button::Middle;
    c.frame(
        &event(InputEvent::PointerMove { x: 300.0, y: cy }),
        8,
        false,
    );
    c.frame(
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
        c.frame(&event(InputEvent::PointerMove { x, y: cy }), 8, true);
    }
    c.frame(
        &event(InputEvent::PointerUp {
            x,
            y: cy,
            button: mid,
        }),
        8,
        false,
    );
    report_cpu("pan", &mut c.times);
    // Zoom bursts: as `frame_times`.
    c.frame(&event(InputEvent::PointerMove { x: cx, y: cy }), 8, false);
    for burst in 0..10 {
        let dy = if burst % 2 == 0 { 1.0 } else { -1.0 };
        for _ in 0..6 {
            let wheel = InputEvent::Wheel {
                x: cx,
                y: cy,
                dx: 0.0,
                dy,
            };
            c.frame(&event(wheel), 16, true);
        }
    }
    report_cpu("zoom", &mut c.times);
    // Select a wire (click), then delete and undo: as `frame_times`.
    for k in 0..12 {
        c.select(k, true);
    }
    report_cpu("select", &mut c.times);
    for k in 0..12 {
        c.select(k, false);
        c.frame(&key_event("Del"), 500, true);
        c.frame(&key_event("U"), 500, true);
    }
    report_cpu("edit", &mut c.times);
    // Hover zoomed in: as `interactions`.
    for _ in 0..8 {
        let wheel = InputEvent::Wheel {
            x: cx,
            y: cy,
            dx: 0.0,
            dy: 1.0,
        };
        c.frame(&event(wheel), 16, false);
    }
    c.settle();
    for i in 0..240 {
        let x = 200.0 + (i % 120) as f32 * 4.0;
        let y = cy + (i / 120) as f32 * 3.0;
        c.frame(&event(InputEvent::PointerMove { x, y }), 8, true);
    }
    report_cpu("hover", &mut c.times);
    let _ = std::fs::remove_dir_all(&dir);
}
