//! The canvas inside egui's pass: each [`Stage`] is an egui paint callback, which egui never
//! tessellates, so a 10^5-row canvas costs egui one shape per stage and a pan or zoom writes
//! only the camera and label uniforms. The rows and pipelines live in egui-wgpu's callback resources as
//! one [`CanvasGpu`]; the app packs the rows and hands over its label meshes during the frame,
//! and [`CanvasGpu::prepare`] uploads them before the pass.
//!
//! Contents: `pub enum Stage`, `pub struct CanvasGpu` (`new`, `prepare`), `Stage::shape`

use egui_wgpu::{CallbackResources, CallbackTrait, Renderer, ScreenDescriptor};

use super::text::TextGpu;
use super::{InstanceRows, Pipelines};

/// One part of the canvas in drawing order: the background and grid, then each row source
/// (the four tables, the marks, the overlay) with its labels after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Background fill and grid dots.
    Background,
    /// Row source `0..SOURCES`.
    Rows(usize),
    /// Row source `0..SOURCES`'s labels.
    Labels(usize),
}

/// The canvas's GPU state: its pipelines and the retained rows they draw. Lives in
/// egui-wgpu's `callback_resources`; the app reaches the rows through it to pack them.
pub struct CanvasGpu {
    /// The pipelines and their buffers.
    pub pipelines: Pipelines,
    /// The rows, patched from the canvas tables' changes, and the frame's label meshes.
    pub rows: InstanceRows,
    text: TextGpu,
}

/// The egui paint callback's payload: which [`Stage`] to draw.
struct StageCallback(Stage);

impl std::fmt::Debug for CanvasGpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasGpu")
            .field("dirty", &self.rows.dirty_bytes())
            .finish_non_exhaustive()
    }
}

impl CanvasGpu {
    /// Pipelines for a `format` target and empty rows.
    #[must_use]
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> CanvasGpu {
        CanvasGpu {
            pipelines: Pipelines::new(device, format),
            rows: InstanceRows::default(),
            text: TextGpu::new(device, format),
        }
    }

    /// Uploads the rows packed this frame, the label meshes that are new and the camera of
    /// their frame (none drawn when the frame showed no canvas) for `renderer`'s canvas, if it
    /// has one. Call after the frame's egui pass and texture updates, before its render pass.
    pub fn prepare(
        renderer: &mut Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
    ) {
        let font = renderer
            .texture(&egui::TextureId::default())
            .map(|t| t.bind_group.clone());
        let Some(gpu) = renderer.callback_resources.get_mut::<CanvasGpu>() else {
            return;
        };
        gpu.pipelines.upload(device, queue, &mut gpu.rows);
        if let Some(frame) = gpu.rows.frame {
            let size = (screen.size_in_pixels[0], screen.size_in_pixels[1]);
            let ppp = screen.pixels_per_point;
            gpu.pipelines
                .write_camera(queue, &frame, f64::from(ppp), size);
            let labels = &gpu.rows.labels;
            gpu.text
                .prepare(device, queue, labels, font, ppp, screen.size_in_pixels);
        }
        debug_assert_eq!(gpu.rows.dirty_bytes(), 0, "every packed row uploaded");
    }
}

impl Stage {
    /// The egui shape that draws this stage at its place in the shape list. `rect` is the
    /// whole screen: the stage draws in target pixels, clipped by its shape's clip rectangle.
    pub fn shape(self, rect: egui::Rect) -> egui::Shape {
        egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
            rect,
            StageCallback(self),
        ))
    }
}

impl CallbackTrait for StageCallback {
    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let Some(gpu) = resources.get::<CanvasGpu>() else {
            return; // a renderer without the canvas (a UI-only raster)
        };
        if self.0 != Stage::Background && gpu.rows.frame.is_none() {
            return;
        }
        let size = (info.screen_size_px[0], info.screen_size_px[1]);
        match self.0 {
            Stage::Labels(src) => gpu.text.draw(pass, &gpu.rows.labels, src, size),
            stage => gpu.pipelines.draw_stage(pass, &gpu.rows, stage, size),
        }
    }
}
