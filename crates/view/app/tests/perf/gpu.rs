//! The GPU frame the window settles on equals the same state rendered afresh.

use schemify_editor::editor::{Editor, Options};
use schemify_render::wgpu;
use schemify_ui::{InputEvent, InputEvents};

use super::{Bench, Gpu, Measure, event, key_event, schematic};
use crate::common::fixture;

impl Bench {
    /// One input batch as the window loop runs it: a frame, then the ticks `wake_ms` asks
    /// for until the editor sleeps. Returns the frames that changed a table.
    fn window(&mut self, ev: &InputEvents, dt_ms: u64) -> usize {
        let mut frames = usize::from(self.frame(ev, dt_ms, Measure::Skip));
        for _ in 0..4096 {
            let Some(ms) = self.ed.wake_ms() else {
                return frames;
            };
            frames += usize::from(self.frame(
                &InputEvents::default(),
                u64::from(ms).max(1),
                Measure::Skip,
            ));
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
    /// frame the window shows and the same editor state painted afresh (fresh caches and rows:
    /// every row packed in that frame).
    fn stale_share(&mut self) -> f64 {
        let shown = self.pixels();
        let caches = std::mem::take(&mut self.caches);
        let rows = std::mem::take(&mut self.gpu.as_mut().expect("a GPU").canvas().rows);
        self.frame(&InputEvents::default(), 1, Measure::Skip);
        let fresh = self.pixels();
        self.caches = caches;
        self.gpu.as_mut().expect("a GPU").canvas().rows = rows;
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
        Some(Bench::new(ed, 1.0, Some(Gpu::new(1.0)?)))
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
