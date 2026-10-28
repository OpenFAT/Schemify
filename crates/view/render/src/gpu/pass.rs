//! A frame on the GPU: the instance rows and the batch in, buffer and texture writes and one
//! render pass out. [`Pipelines::upload`] and [`Pipelines::upload_batch`] write what changed
//! since the last frame; [`Pipelines::draw`] draws the batch's draws in order with each canvas
//! stage among them.
//!
//! Contents:
//! - `Pipelines::upload`, `Pipelines::upload_batch`, `Pipelines::take_uploaded`,
//!   `Pipelines::take_batch_uploaded`, `Pipelines::draw`

use schemify_canvas::LAYERS;

use crate::atlas::ATLAS_SIDE;
use crate::batch::{self, Batch, Stage, What};

use super::pipelines::{Pipelines, UNIFORM_BYTES, bind_batch, buffer, texture, write_texels};
use super::rows::BYTES_MAX;
use super::{Frame, InstanceRows, SOURCES, TRI};

impl Pipelines {
    /// Writes the rows' dirty ranges to their buffers (a buffer too small is replaced by one
    /// twice the size and written whole) and clears them. Call before [`Pipelines::draw`].
    ///
    /// Cost: one `write_buffer` per dirty range.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, rows: &mut InstanceRows) {
        for (src, tables) in rows.tables.iter_mut().enumerate() {
            for (kind, t) in tables.iter_mut().enumerate() {
                let dirty = std::mem::take(&mut t.dirty);
                if dirty.is_empty() {
                    continue;
                }
                let bytes = &t.bytes;
                assert!(bytes.len() <= BYTES_MAX, "rows fit wgpu's buffer limit");
                let slot = &mut self.buffers[src][kind];
                let fits = slot
                    .as_ref()
                    .is_some_and(|b| b.size() >= bytes.len() as u64);
                if !fits {
                    let size = bytes.len().next_power_of_two().max(4096);
                    let usage = wgpu::BufferUsages::VERTEX;
                    let b = buffer(device, "canvas rows", size as u64, usage);
                    if !bytes.is_empty() {
                        queue.write_buffer(&b, 0, bytes);
                        self.uploaded += bytes.len() as u64;
                    }
                    *slot = Some(b);
                    continue;
                }
                let b = slot.as_ref().expect("fits");
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

    /// Writes the batch's instances, its atlas's pending masks and the textures of images it
    /// draws for the first time, and clears the pending masks. Call before
    /// [`Pipelines::draw`].
    ///
    /// Cost: one `write_buffer`, one `write_texture` per new mask and per new image.
    pub fn upload_batch(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, batch: &mut Batch) {
        let bytes = batch.instances();
        if !bytes.is_empty() {
            let fits = self
                .instances
                .as_ref()
                .is_some_and(|b| b.size() >= bytes.len() as u64);
            if !fits {
                let size = bytes.len().next_power_of_two().max(1 << 16);
                let usage = wgpu::BufferUsages::VERTEX;
                self.instances = Some(buffer(device, "batch", size as u64, usage));
            }
            queue.write_buffer(self.instances.as_ref().expect("made"), 0, bytes);
            self.instances_uploaded += bytes.len() as u64;
        }
        // The retained UI rows: only what changed since the last upload.
        for (k, s) in batch.ui.src.iter_mut().enumerate() {
            let dirty = std::mem::take(&mut s.dirty);
            let bytes = &s.bytes;
            let slot = &mut self.ui_rows[k];
            if slot.as_ref().is_none_or(|b| b.size() < bytes.len() as u64) {
                let size = bytes.len().next_power_of_two().max(1 << 16);
                let b = buffer(device, "ui rows", size as u64, wgpu::BufferUsages::VERTEX);
                queue.write_buffer(&b, 0, bytes);
                self.instances_uploaded += bytes.len() as u64;
                *slot = Some(b);
                continue;
            }
            let b = slot.as_ref().expect("fits");
            for r in dirty {
                assert!(r.end <= bytes.len(), "a dirty range inside the rows");
                queue.write_buffer(b, r.start as u64, &bytes[r.clone()]);
                self.instances_uploaded += r.len() as u64;
            }
        }
        assert_eq!(batch.ui.dirty_bytes(), 0, "every UI row uploaded");
        // The panes' offsets and clips that moved (a scroll writes one entry, 32 bytes).
        for (k, (have, want)) in self.panes.iter_mut().zip(&batch.ui.panes).enumerate() {
            if have != want {
                *have = *want;
                let bytes: Vec<u8> = want.iter().flat_map(|v| v.to_ne_bytes()).collect();
                queue.write_buffer(&self.ui_uniform, 16 + 32 * k as u64, &bytes);
                self.instances_uploaded += bytes.len() as u64;
            }
        }
        let (slots, coverage) = batch.atlas.pending();
        for &(s, at) in slots {
            let n = usize::from(s.w) * usize::from(s.h);
            let at = at as usize;
            let rect = (
                u32::from(s.x),
                u32::from(s.y),
                u32::from(s.w),
                u32::from(s.h),
            );
            write_texels(queue, &self.atlas, rect, 1, &coverage[at..at + n]);
        }
        self.atlas_uploaded += batch.atlas.pending_bytes() as u64;
        batch.atlas.clear_pending();
        // ponytail: image textures are dropped wholesale past twice the prose image cap (as
        // `painter::image_data` drops its blobs); an LRU when pages show more images.
        if self.images.len() + batch.images().len() > 2 * schemify_ui::TextSystem::IMAGES_MAX {
            self.images.clear();
        }
        for image in batch.images() {
            let id = image.data.id();
            if self.images.contains_key(&id) {
                continue;
            }
            assert!(
                image.format == peniko::ImageFormat::Rgba8,
                "prose images are RGBA8"
            );
            let size = (image.width, image.height);
            let t = texture(device, "image", size, wgpu::TextureFormat::Rgba8Unorm);
            write_texels(queue, &t, (0, 0, size.0, size.1), 4, image.data.data());
            let view = t.create_view(&Default::default());
            let bind = bind_batch(
                device,
                &self.ui_layout,
                &self.ui_uniform,
                &self.atlas_view,
                &view,
                &self.sampler,
            );
            self.images.insert(id, bind);
        }
        debug_assert!(self.atlas.width() == u32::from(ATLAS_SIDE));
    }

    /// Bytes written to the GPU by [`Pipelines::upload`] since the last call (a report's
    /// "bytes per edit").
    pub fn take_uploaded(&mut self) -> u64 {
        std::mem::take(&mut self.uploaded)
    }

    /// Atlas bytes and batch instance bytes written by [`Pipelines::upload_batch`] since the
    /// last call.
    pub fn take_batch_uploaded(&mut self) -> (u64, u64) {
        (
            std::mem::take(&mut self.atlas_uploaded),
            std::mem::take(&mut self.instances_uploaded),
        )
    }

    /// Draws one frame into `target` (a `size`-px texture of these pipelines' format) as one
    /// render pass cleared to `clear`: the batch's draws in order, each canvas stage among
    /// them (when `frame` has a canvas) scissored to its view at `dev` device px per logical
    /// px, the rows as the CPU painter draws them: source by source, layer by layer, fills
    /// before strokes. A batch with no canvas stage draws the whole canvas first. Call after
    /// [`Pipelines::upload`] and [`Pipelines::upload_batch`].
    ///
    /// Cost: one pass, two uniform writes, one draw per batch draw and per non-empty (source,
    /// layer, kind).
    ///
    /// # Panics
    ///
    /// Panics when rows were packed since the last [`Pipelines::upload`].
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        clear: wgpu::Color,
        rows: &InstanceRows,
        batch: &Batch,
        frame: Option<&Frame>,
        dev: f64,
        size: (u32, u32),
    ) {
        let (w, h) = size;
        assert!(w > 0 && h > 0, "a target has a size");
        assert_eq!(rows.dirty_bytes(), 0, "rows uploaded before they are drawn");
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        let mut ui = [0u8; 16];
        ui[..4].copy_from_slice(&(w as f32).to_ne_bytes());
        ui[4..8].copy_from_slice(&(h as f32).to_ne_bytes());
        queue.write_buffer(&self.ui_uniform, 0, &ui);
        // The canvas's scissor, when there is a canvas in view.
        let scissor = frame.and_then(|frame| {
            let v = frame.view;
            let at = |c: f64, max: u32| c.clamp(0.0, f64::from(max)) as u32;
            let (x0, y0) = (at((v.x0 * dev).floor(), w), at((v.y0 * dev).floor(), h));
            let (x1, y1) = (at((v.x1 * dev).ceil(), w), at((v.y1 * dev).ceil(), h));
            if x1 <= x0 || y1 <= y0 {
                return None;
            }
            queue.write_buffer(&self.uniform, 0, &uniform(frame, dev, size));
            Some([x0, y0, x1 - x0, y1 - y0])
        });
        let stage = |pass: &mut wgpu::RenderPass<'_>, stage: Stage| {
            let Some([x, y, sw, sh]) = scissor else {
                return;
            };
            pass.set_scissor_rect(x, y, sw, sh);
            pass.set_bind_group(0, &self.bind, &[]);
            match stage {
                Stage::Background => {
                    pass.set_pipeline(&self.background);
                    pass.draw(0..6, 0..1);
                }
                Stage::Rows(src) => self.draw_rows(pass, rows, src),
            }
        };
        let staged = batch
            .draws()
            .iter()
            .any(|d| matches!(d.what, What::Canvas(_)));
        if !staged {
            stage(&mut pass, Stage::Background);
            for src in 0..SOURCES {
                stage(&mut pass, Stage::Rows(src));
            }
        }
        for d in batch.draws() {
            let bind = match d.what {
                What::Canvas(s) => {
                    stage(&mut pass, s);
                    continue;
                }
                What::Ui | What::Retained(_) => &self.ui_bind,
                What::Image(k) => {
                    let id = batch.images()[k as usize].data.id();
                    self.images
                        .get(&id)
                        .expect("images uploaded before they are drawn")
                }
            };
            if d.range.is_empty() {
                continue;
            }
            let buf = match d.what {
                What::Retained(k) => self.ui_rows[k].as_ref(),
                _ => self.instances.as_ref(),
            }
            .expect("instances uploaded before they are drawn");
            assert!(
                buf.size() >= u64::from(d.range.end) * batch::STRIDE as u64,
                "the buffer holds every instance"
            );
            pass.set_scissor_rect(0, 0, w, h);
            pass.set_pipeline(&self.ui);
            pass.set_bind_group(0, bind, &[]);
            pass.set_vertex_buffer(0, buf.slice(..));
            pass.draw(0..6, d.range.clone());
        }
    }

}

// ---- private ----

impl Pipelines {
    /// The rows of source `src`, layer by layer, each layer's kinds in order.
    fn draw_rows(&self, pass: &mut wgpu::RenderPass<'_>, rows: &InstanceRows, src: usize) {
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
                pass.draw(0..verts, first..first + used);
            }
        }
    }
}

/// The camera uniform (`Cam` in `shaders/canvas/camera.wgsl`) for `frame` at `dev` device px per logical px
/// into a `target`-sized texture.
fn uniform(frame: &Frame, dev: f64, (w, h): (u32, u32)) -> [u8; UNIFORM_BYTES] {
    let (cam, v) = (&frame.cam, frame.view);
    assert!(dev > 0.0 && cam.scale > 0.0, "a drawable camera");
    let (fx, fy) = (cam.cx.floor(), cam.cy.floor());
    let clamp = |c: f64| c.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    let (ox, oy) = cam.origin(v);
    let grid = match crate::canvas::grid_dots(v, cam, frame.grid) {
        Some((s, ..)) => {
            let p = s * cam.scale;
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
    for (i, wd) in words.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&wd.to_ne_bytes());
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
