//! GPU raster layers: pictures a [`ScenePainter`](crate::ScenePainter) records once and later
//! frames compose as images (D39: a pan or zoom frame re-encodes no canvas primitive).
//!
//! Data design:
//! 1. In: per slot a recorded vello scene; per frame a log of what was painted outside the
//!    layers (a hash) and where each layer was drawn. Out: layer textures; the frame either
//!    through vello or, when only layer placements changed, as quads over the last frame.
//! 2. How many: [`LAYER_SLOTS`] slots (one per cached canvas); a handful of draws per frame.
//! 3. Widths: placements in device pixels (`f64` transforms); the hash is a `u64`.
//! 4. Access: the log is appended by every painter call and read once by the window.
//! 5. Lifetime: slots and textures live with the window; the log is per frame.
//! 6. Parallel: no (one frame at a time on the GPU queue).

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use vello::kurbo::{Affine, Rect};
use vello::peniko::{Blob, Color, ImageAlphaType, ImageData, ImageFormat};
use vello::{AaConfig, RenderParams, Renderer, Scene, wgpu};

/// Raster layer slots a painter keeps (one per cached canvas).
pub const LAYER_SLOTS: usize = 8;

/// GPU raster layers kept across frames (owner: the window, or a benchmark driving vello
/// itself): per slot the recorded scene, the texture it is rasterized into, and the image a
/// frame scene samples; plus the log of the frame being painted. A texture lives as long as
/// its slot keeps its size.
#[derive(Default)]
pub struct Layers {
    pub(crate) slots: Vec<GpuLayer>,
    /// Images whose textures were replaced; unregistered from the renderer by the next raster.
    pub(crate) retired: Vec<ImageData>,
    pub(crate) log: FrameLog,
    /// Hash and target size of the last frame vello rendered (what the target texture holds).
    shown: Option<(u64, u32, u32)>,
    quad: Option<Quad>,
}

#[derive(Default)]
pub(crate) struct GpuLayer {
    pub(crate) stamp: u64,
    pub(crate) scene: Scene,
    pub(crate) image: Option<ImageData>,
    texture: Option<wgpu::Texture>,
    pub(crate) dirty: bool,
    /// Parts recorded since the last raster: a scene of `w × h` device px for `[x, y, w, h]`.
    pub(crate) patches: Vec<(Scene, [u32; 4])>,
}

/// What one frame painted outside its layers.
#[derive(Default)]
pub(crate) struct FrameLog {
    /// Every painter call outside the layers, in order (layer draws by slot and clip only).
    pub(crate) hash: std::collections::hash_map::DefaultHasher,
    /// Layer draws: (slot, layer px → device px, device clip, stretch fill).
    pub(crate) draws: Vec<(usize, Affine, Rect, Option<Color>)>,
    /// Something was painted after the first layer draw (it could cover the layer).
    pub(crate) after: bool,
    /// Inside [`crate::Painter::layer`]: calls belong to the layer, not to the frame.
    pub(crate) recording: bool,
}

impl FrameLog {
    /// Folds one painter call into the frame hash (unless a layer is being recorded).
    pub(crate) fn call(&mut self, data: impl Hash) {
        if self.recording {
            return;
        }
        self.after |= !self.draws.is_empty();
        data.hash(&mut self.hash);
    }
}

impl std::fmt::Debug for Layers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Layers")
            .field("slots", &self.slots.len())
            .finish()
    }
}

impl Layers {
    /// Starts logging a frame painted with device scale `base`. Call before painting it.
    pub fn begin_frame(&mut self, base: Affine) {
        self.log = FrameLog::default();
        base.as_coeffs().map(f64::to_bits).hash(&mut self.log.hash);
    }

    /// Records a fresh image of `w × h` for `slot` when its size changed (a new image id: the
    /// next raster binds a texture of that size).
    pub(crate) fn sized(&mut self, slot: usize, w: u32, h: u32) -> &mut GpuLayer {
        assert!(slot < LAYER_SLOTS && w > 0 && h > 0);
        if self.slots.len() <= slot {
            self.slots.resize_with(slot + 1, GpuLayer::default);
        }
        let l = &mut self.slots[slot];
        if l.image
            .as_ref()
            .is_none_or(|i| (i.width, i.height) != (w, h))
        {
            let old = l.image.replace(ImageData {
                data: Blob::new(Arc::new([])),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::AlphaPremultiplied,
                width: w,
                height: h,
            });
            l.texture = None;
            self.retired.extend(old);
        }
        &mut self.slots[slot]
    }

    /// Rasterizes every layer recorded since the last call into its texture (vello renders it
    /// into an `Rgba8Unorm` texture registered as the layer's image), then every part patched
    /// since (rendered into a texture of its size and copied over its place). Call once per
    /// frame after painting and before rendering or composing the frame.
    ///
    /// Cost: one vello render per recorded layer or patch; nothing when none was recorded.
    ///
    /// # Errors
    ///
    /// Fails when vello cannot render a layer (the frame then shows its previous content).
    pub fn raster(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
    ) -> Result<(), vello::Error> {
        for image in self.retired.drain(..) {
            renderer.override_image(&image, None);
        }
        for l in self.slots.iter_mut().filter(|l| l.dirty) {
            let image = l.image.as_ref().expect("a recorded layer has an image");
            let texture = l.texture.get_or_insert_with(|| {
                let t = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("layer"),
                    size: wgpu::Extent3d {
                        width: image.width,
                        height: image.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                renderer.override_image(
                    image,
                    Some(wgpu::TexelCopyTextureInfoBase {
                        texture: t.clone(),
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    }),
                );
                t
            });
            let params = RenderParams {
                base_color: Color::TRANSPARENT,
                width: image.width,
                height: image.height,
                antialiasing_method: AaConfig::Area,
            };
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            renderer.render_to_texture(device, queue, &l.scene, &view, &params)?;
            renderer.mark_override_image_dirty(image);
            l.dirty = false;
        }
        for l in self.slots.iter_mut().filter(|l| !l.patches.is_empty()) {
            let (Some(image), Some(texture)) = (l.image.as_ref(), l.texture.as_ref()) else {
                l.patches.clear(); // no picture to patch (its first raster failed)
                continue;
            };
            for (scene, [x, y, w, h]) in l.patches.drain(..) {
                assert!(x + w <= image.width && y + h <= image.height);
                let size = wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                };
                let part = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("layer patch"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let params = RenderParams {
                    base_color: Color::TRANSPARENT,
                    width: w,
                    height: h,
                    antialiasing_method: AaConfig::Area,
                };
                let view = part.create_view(&wgpu::TextureViewDescriptor::default());
                renderer.render_to_texture(device, queue, &scene, &view, &params)?;
                let mut encoder = device.create_command_encoder(&Default::default());
                encoder.copy_texture_to_texture(
                    part.as_image_copy(),
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x, y, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    size,
                );
                queue.submit([encoder.finish()]);
            }
            renderer.mark_override_image_dirty(image);
        }
        Ok(())
    }

    /// Whether this frame may skip vello: it painted exactly what the frame vello last rendered
    /// into a `w × h` target painted, apart from where its layers are drawn, and nothing after
    /// them. Then [`Layers::compose`] over a copy of that target is the whole frame.
    pub fn reusable(&self, w: u32, h: u32) -> bool {
        let hash = self.log.hash.finish();
        !self.log.after
            && !self.log.draws.is_empty()
            && self.shown == Some((hash, w, h))
            && self
                .log
                .draws
                .iter()
                .all(|d| self.slots.get(d.0).is_some_and(|l| l.texture.is_some()))
    }

    /// Notes that vello rendered this frame into a `w × h` target.
    pub fn rendered(&mut self, w: u32, h: u32) {
        self.shown = Some((self.log.hash.finish(), w, h));
    }

    /// Draws this frame's layer placements as textured quads into `target` (a `format`
    /// render attachment of `w × h` device px), over whatever it holds: nearest sampling for
    /// a copy, bilinear for a stretched stand-in (whose uncovered clip pixels get its fill).
    ///
    /// Cost: one render pass, one quad per layer draw.
    pub fn compose(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        (w, h): (u32, u32),
    ) {
        assert!(
            self.log.draws.len() <= LAYER_SLOTS,
            "one draw per slot at most"
        );
        let quad = match &self.quad {
            Some(q) if q.format == format => self.quad.as_ref().expect("checked"),
            _ => self.quad.insert(Quad::new(device, format)),
        };
        let mut groups = Vec::with_capacity(self.log.draws.len());
        for (k, &(slot, at, clip, stretch)) in self.log.draws.iter().enumerate() {
            let l = &self.slots[slot];
            let (Some(texture), Some(image)) = (&l.texture, &l.image) else {
                continue;
            };
            let clip = clip.intersect(Rect::new(0.0, 0.0, f64::from(w), f64::from(h)));
            if clip.area() <= 0.0 {
                continue;
            }
            // The clip's corners in layer texels, as texture coordinates.
            let inv = at.inverse();
            let (p, q) = (
                inv * clip.origin(),
                inv * vello::kurbo::Point::new(clip.x1, clip.y1),
            );
            let (tw, th) = (f64::from(image.width), f64::from(image.height));
            // Uniform: uv rect, then the fill (alpha 0: a copy, every pixel sampled).
            let fill = stretch.map_or([0.0; 4], |c| c.components);
            let uv = [p.x / tw, p.y / th, q.x / tw, q.y / th].map(|v| v as f32);
            let mut bytes = [0u8; 32];
            for (i, v) in uv.into_iter().chain(fill).enumerate() {
                bytes[4 * i..4 * i + 4].copy_from_slice(&v.to_ne_bytes());
            }
            let offset = (k * Quad::STRIDE) as u64;
            queue.write_buffer(&quad.uniforms, offset, &bytes);
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("layer quad"),
                layout: &quad.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(match stretch {
                            Some(_) => &quad.linear,
                            None => &quad.nearest,
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &quad.uniforms,
                            offset,
                            size: wgpu::BufferSize::new(32),
                        }),
                    },
                ],
            });
            groups.push((group, clip));
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("layer quads"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&quad.pipeline);
        for (group, clip) in &groups {
            pass.set_viewport(
                clip.x0 as f32,
                clip.y0 as f32,
                clip.width() as f32,
                clip.height() as f32,
                0.0,
                1.0,
            );
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..4, 0..1);
        }
    }
}

/// The quad pipeline: one textured quad per layer draw, its viewport the draw's clip.
struct Quad {
    format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    nearest: wgpu::Sampler,
    linear: wgpu::Sampler,
}

impl Quad {
    /// Uniform slot stride (wgpu's minimum uniform offset alignment).
    const STRIDE: usize = 256;

    const SHADER: &'static str = "
struct Uv { rect: vec4<f32>, fill: vec4<f32> }
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> uv: Uv;
struct Out { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
    let p = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    var o: Out;
    o.pos = vec4<f32>(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
    o.uv = mix(uv.rect.xy, uv.rect.zw, p);
    return o;
}
@fragment fn fs(o: Out) -> @location(0) vec4<f32> {
    let texel = textureSample(tex, samp, o.uv);
    let outside = any(o.uv < vec2<f32>(0.0)) || any(o.uv > vec2<f32>(1.0));
    return select(texel, uv.fill, outside && uv.fill.a > 0.0);
}
";

    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Quad {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("layer quad"),
            source: wgpu::ShaderSource::Wgsl(Self::SHADER.into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty,
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layer quad"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(
                    1,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
                entry(
                    2,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(32),
                    },
                ),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("layer quad"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("layer quad"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = |filter| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("layer quad"),
                mag_filter: filter,
                min_filter: filter,
                ..Default::default()
            })
        };
        Quad {
            format,
            pipeline,
            layout,
            uniforms: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("layer quad"),
                size: (Self::STRIDE * LAYER_SLOTS) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            nearest: sampler(wgpu::FilterMode::Nearest),
            linear: sampler(wgpu::FilterMode::Linear),
        }
    }
}
