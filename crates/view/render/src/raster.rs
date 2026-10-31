//! Offscreen frames: egui output (with the canvas stages) drawn by egui-wgpu into a texture and
//! read back as an RGBA image. Golden tests and PNG export draw through the same renderer and
//! the same canvas pipelines as the window, so a golden shows what the window shows. Needs a
//! GPU, a software one (lavapipe, llvmpipe, WARP) included.
//!
//! Contents: `pub struct Raster` (`new`, `canvas`, `render`), `pub struct Image` (`pixel`,
//! `png`, `from_png`)

#[cfg(not(target_arch = "wasm32"))]
use egui_wgpu::RendererOptions;
use egui_wgpu::{Renderer, ScreenDescriptor};

#[cfg(not(target_arch = "wasm32"))]
use crate::device;
use crate::gpu::{CanvasGpu, InstanceRows};

/// A headless GPU with egui-wgpu's renderer and the canvas in its callback resources.
pub struct Raster {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
}

/// An RGBA8 image, rows top to bottom, straight alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// The texture format offscreen frames are drawn in (the window's surfaces are 8-bit unorm
/// too, so a golden matches the window).
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl std::fmt::Debug for Raster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Raster").finish_non_exhaustive()
    }
}

impl Raster {
    /// Opens a GPU (hardware, else software) for offscreen frames.
    ///
    /// # Errors
    ///
    /// No adapter, hardware or software, gives a device.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new() -> Result<Raster, String> {
        let g = device::ladder(|fallback| {
            let instance = wgpu::Instance::new(
                wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
            );
            crate::window::block_on(device::open(&instance, None, fallback))
        })?;
        let mut renderer = Renderer::new(
            &g.device,
            FORMAT,
            RendererOptions {
                dithering: false,
                ..RendererOptions::default()
            },
        );
        renderer
            .callback_resources
            .insert(CanvasGpu::new(&g.device, FORMAT));
        Ok(Raster {
            device: g.device,
            queue: g.queue,
            renderer,
        })
    }

    /// The canvas rows the next frame draws: pack them while egui runs the frame.
    ///
    /// # Panics
    ///
    /// Never: the canvas is inserted with the renderer.
    pub fn canvas(&mut self) -> &mut InstanceRows {
        let gpu = self.renderer.callback_resources.get_mut::<CanvasGpu>();
        &mut gpu.expect("the canvas is a resource").rows
    }

    /// Draws `out` (a frame `ctx` produced) at its pixels per point into a `size`-px image.
    ///
    /// # Panics
    ///
    /// Panics on an empty size or a GPU that cannot map its buffer.
    pub fn render(
        &mut self,
        ctx: &egui::Context,
        mut out: egui::FullOutput,
        size: (u32, u32),
    ) -> Image {
        let (w, h) = size;
        assert!(w > 0 && h > 0, "an image has pixels");
        let ppp = out.pixels_per_point;
        let prims = ctx.tessellate(std::mem::take(&mut out.shapes), ppp);
        let screen = ScreenDescriptor {
            size_in_pixels: [w, h],
            pixels_per_point: ppp,
        };
        for (id, deltas) in &out.textures_delta.set {
            for delta in deltas {
                self.renderer
                    .update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        CanvasGpu::prepare(&mut self.renderer, &self.device, &self.queue, &screen);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("raster"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("raster"),
            });
        let mut cmds =
            self.renderer
                .update_buffers(&self.device, &self.queue, &mut encoder, &prims, &screen);
        crate::window::draw(&self.renderer, &mut encoder, &view, &prims, &screen);
        // Rows of a buffer copy are padded to 256 bytes.
        let row = w * 4;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("raster readback"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        cmds.push(encoder.finish());
        self.queue.submit(cmds);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("the readback maps"));
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("the GPU finishes the frame");
        let mut rgba = Vec::with_capacity((row * h) as usize);
        for line in slice
            .get_mapped_range()
            .expect("the readback is mapped")
            .chunks(padded as usize)
        {
            rgba.extend_from_slice(&line[..row as usize]);
        }
        buffer.unmap();
        for id in &out.textures_delta.free {
            self.renderer.free_texture(id);
        }
        out.textures_delta.clear();
        let image = Image {
            width: w,
            height: h,
            rgba,
        };
        debug_assert_eq!(
            image.rgba.len(),
            (w * h * 4) as usize,
            "whole rows read back"
        );
        image
    }
}

impl Image {
    /// The RGBA of pixel `(x, y)`.
    ///
    /// # Panics
    ///
    /// Panics if `(x, y)` is outside the image.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        assert!(
            x < self.width && y < self.height,
            "pixel ({x}, {y}) inside the image"
        );
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    /// The image as PNG bytes.
    ///
    /// # Errors
    ///
    /// The encoder fails (it does not for an RGBA8 image in memory).
    pub fn png(&self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, self.width, self.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        w.write_image_data(&self.rgba).map_err(|e| e.to_string())?;
        w.finish().map_err(|e| e.to_string())?;
        Ok(out)
    }

    /// Decodes an 8-bit RGBA or RGB PNG.
    ///
    /// # Errors
    ///
    /// Not a PNG, or not 8-bit RGB(A).
    pub fn from_png(bytes: &[u8]) -> Result<Image, String> {
        let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
        dec.set_transformations(png::Transformations::EXPAND);
        let mut r = dec.read_info().map_err(|e| e.to_string())?;
        let mut buf = vec![0; r.output_buffer_size().ok_or("an image too large")?];
        let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
            png::ColorType::Rgb => buf[..info.buffer_size()]
                .chunks(3)
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            other => return Err(format!("a {other:?} PNG")),
        };
        Ok(Image {
            width: info.width,
            height: info.height,
            rgba,
        })
    }
}
