//! Uploading and drawing the canvas: the rows' dirty ranges and the camera uniform written
//! before egui-wgpu's pass, then each [`Stage`] drawn inside it (`super::callback`), so the
//! rows sit at their place among egui's meshes.
//!
//! Contents:
//! - `Pipelines::upload`, `Pipelines::write_camera`, `Pipelines::take_uploaded`,
//!   `Pipelines::draw_stage`

use std::mem;

use schemify_canvas::LAYERS;

use super::pipelines::{self, Pipelines, UNIFORM_BYTES};
use super::rows::BYTES_MAX;
use super::{Frame, InstanceRows, SOURCES, Stage, TRI};

impl Pipelines {
    /// Writes the rows' dirty ranges to their buffers (a buffer too small is replaced by one
    /// twice the size and written whole) and clears them. Call before [`Pipelines::draw_stage`].
    ///
    /// Cost: one `write_buffer` per dirty range.
    ///
    /// # Panics
    ///
    /// Panics on a row array past [`wgpu::Limits::max_buffer_size`]'s default (256 MiB), which
    /// the pack never writes.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, rows: &mut InstanceRows) {
        for (src, tables) in rows.tables.iter_mut().enumerate() {
            for (kind, t) in tables.iter_mut().enumerate() {
                let dirty = mem::take(&mut t.dirty);
                if dirty.is_empty() {
                    continue;
                }
                let bytes = &t.bytes;
                assert!(bytes.len() <= BYTES_MAX, "rows fit wgpu's buffer limit");
                let slot = &mut self.buffers[src][kind];
                let b = match slot {
                    Some(b) if b.size() >= bytes.len() as u64 => b,
                    _ => {
                        let size = bytes.len().next_power_of_two().max(4096);
                        let usage = wgpu::BufferUsages::VERTEX;
                        let b = pipelines::buffer(device, "canvas rows", size as u64, usage);
                        if !bytes.is_empty() {
                            queue.write_buffer(&b, 0, bytes);
                            self.uploaded += bytes.len() as u64;
                        }
                        *slot = Some(b);
                        continue;
                    }
                };
                for r in dirty {
                    let r = r.start.min(bytes.len())..r.end.min(bytes.len());
                    if !r.is_empty() {
                        queue.write_buffer(b, r.start as u64, &bytes[r.clone()]);
                        self.uploaded += r.len() as u64;
                    }
                }
            }
        }
        assert_eq!(rows.dirty_bytes(), 0, "every dirty range uploaded");
    }

    /// Writes the camera uniform for `frame` at `dev` device px per logical px into a
    /// `size`-px target. Call before the pass that draws the stages.
    ///
    /// # Panics
    ///
    /// Panics on an empty target or a non-positive device scale.
    pub fn write_camera(&self, queue: &wgpu::Queue, frame: &Frame, dev: f64, size: (u32, u32)) {
        assert!(size.0 > 0 && size.1 > 0, "a target has a size");
        assert!(dev > 0.0, "a positive device scale, got {dev}");
        queue.write_buffer(&self.uniform, 0, &uniform(frame, dev, size));
    }

    /// Bytes [`Pipelines::upload`] wrote since the last call.
    pub fn take_uploaded(&mut self) -> u64 {
        mem::take(&mut self.uploaded)
    }

    /// Draws `stage` into `pass` (its viewport the whole `size`-px target, as the camera
    /// uniform assumes; its scissor already the canvas clip): the background and grid, or a
    /// row source layer by layer, fills before strokes.
    ///
    /// # Panics
    ///
    /// Panics when rows were packed since the last [`Pipelines::upload`].
    pub fn draw_stage(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        rows: &InstanceRows,
        stage: Stage,
        size: (u32, u32),
    ) {
        assert_eq!(rows.dirty_bytes(), 0, "rows uploaded before they are drawn");
        pass.set_viewport(0.0, 0.0, size.0 as f32, size.1 as f32, 0.0, 1.0);
        pass.set_bind_group(0, &self.bind, &[]);
        match stage {
            Stage::Background => {
                pass.set_pipeline(&self.background);
                pass.draw(0..6, 0..1);
            }
            Stage::Rows(src) => self.draw_rows(pass, rows, src),
        }
    }
}

// ---- private ----

impl Pipelines {
    /// The rows of source `src`, layer by layer, each layer's kinds in order.
    fn draw_rows(&self, pass: &mut wgpu::RenderPass<'_>, rows: &InstanceRows, src: usize) {
        assert!(src < SOURCES, "a row source");
        let tables = &rows.tables[src];
        for layer in 0..LAYERS {
            for (kind, t) in tables.iter().enumerate() {
                let used = t.used[layer];
                if used == 0 {
                    continue;
                }
                let buf = self.buffers[src][kind]
                    .as_ref()
                    .expect("rows uploaded before they are drawn");
                assert!(
                    buf.size() >= t.bytes.len() as u64,
                    "the buffer holds every row"
                );
                pass.set_pipeline(&self.kinds[kind]);
                pass.set_vertex_buffer(0, buf.slice(..));
                let verts = if kind == TRI { 3 } else { 6 };
                let first = t.start[layer];
                debug_assert!(
                    (first + used) as usize * super::STRIDE[kind] <= t.bytes.len(),
                    "a layer's slots are rows"
                );
                pass.draw(0..verts, first..first + used);
            }
        }
    }
}

/// The camera uniform (`Cam` in `shaders/canvas/camera.wgsl`) for `frame` at `dev` device px per logical px
/// into a `target`-sized texture.
fn uniform(frame: &Frame, dev: f64, (w, h): (u32, u32)) -> [u8; UNIFORM_BYTES] {
    let (cam, v) = (&frame.cam, frame.view);
    assert!(dev > 0.0, "a positive device scale");
    assert!(cam.scale > 0.0, "a drawable camera");
    let (fx, fy) = (cam.cx.floor(), cam.cy.floor());
    let clamp = |c: f64| c.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    let (ox, oy) = cam.origin(v);
    let grid = match super::grid_dots(v, cam, frame.grid) {
        Some(dots) => {
            let p = dots.pitch * cam.scale;
            [
                p as f32,
                ox.rem_euclid(p) as f32,
                oy.rem_euclid(p) as f32,
                0.0,
            ]
        }
        None => [0.0; 4],
    };
    let color = |rgba: u32| rgba.to_be_bytes().map(|b| f32::from(b) / 255.0);
    let (gc, bg) = (color(frame.grid_color), color(frame.background));
    let words: [u32; UNIFORM_BYTES / 4] = [
        clamp(fx) as u32,
        clamp(fy) as u32,
        ((cam.cx - fx) as f32).to_bits(),
        ((cam.cy - fy) as f32).to_bits(),
        ((v.x0 * dev) as f32).to_bits(),
        ((v.y0 * dev) as f32).to_bits(),
        ((v.x1 * dev) as f32).to_bits(),
        ((v.y1 * dev) as f32).to_bits(),
        ((cam.scale * dev) as f32).to_bits(),
        (dev as f32).to_bits(),
        (w as f32).to_bits(),
        (h as f32).to_bits(),
        grid[0].to_bits(),
        grid[1].to_bits(),
        grid[2].to_bits(),
        grid[3].to_bits(),
        gc[0].to_bits(),
        gc[1].to_bits(),
        gc[2].to_bits(),
        gc[3].to_bits(),
        bg[0].to_bits(),
        bg[1].to_bits(),
        bg[2].to_bits(),
        bg[3].to_bits(),
    ];
    let mut out = [0u8; UNIFORM_BYTES];
    let (chunks, rest) = out.as_chunks_mut::<4>();
    debug_assert!(rest.is_empty(), "the uniform is whole words");
    for (b, wd) in chunks.iter_mut().zip(&words) {
        *b = wd.to_ne_bytes();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Rect;
    use schemify_canvas::Camera;

    fn words(b: &[u8]) -> Vec<u32> {
        b.chunks_exact(4)
            .map(|c| u32::from_ne_bytes(c.try_into().expect("4 bytes")))
            .collect()
    }

    /// The uniform carries the camera split into an integer centre and a fraction, the view in
    /// device px, and the grid's pitch and phase (none when the grid is off).
    #[test]
    fn uniform_splits_the_camera() {
        let frame = Frame {
            view: Rect::new(10.0, 20.0, 650.0, 420.0),
            cam: Camera {
                cx: 12_345.75,
                cy: -6_789.5,
                scale: 0.02,
            },
            grid: 20_000,
            background: 0x0000_00ff,
            grid_color: 0x7070_70ff,
        };
        let u = words(&uniform(&frame, 2.0, (1300, 840)));
        assert_eq!((u[0] as i32, u[1] as i32), (12_345, -6_790));
        assert_eq!((f32::from_bits(u[2]), f32::from_bits(u[3])), (0.75, 0.5));
        assert_eq!(f32::from_bits(u[4]), 20.0);
        assert_eq!(f32::from_bits(u[7]), 840.0);
        assert_eq!((f32::from_bits(u[8]), f32::from_bits(u[9])), (0.04, 2.0));
        // 20 000 mu × 0.02 = 400 px per dot: no doubling; the phase is the origin mod pitch.
        let (ox, _) = frame.cam.origin(frame.view);
        assert_eq!(f32::from_bits(u[12]), 400.0);
        assert_eq!(f32::from_bits(u[13]), ox.rem_euclid(400.0) as f32);
        // "No grid" is a pitch past any view (`Editor::picture`); a view holding the world
        // origin would still show that one dot, as the CPU painter does.
        let off = Frame {
            grid: i32::MAX / 4,
            cam: Camera {
                cx: 12_345_675.75,
                ..frame.cam
            },
            ..frame
        };
        assert_eq!(
            f32::from_bits(words(&uniform(&off, 1.0, (1300, 840)))[12]),
            0.0
        );
    }
}
