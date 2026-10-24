//! GPU raster layers: pictures a [`ScenePainter`](crate::ScenePainter) records once and later
//! frames compose as images (D39: a pan or zoom frame re-encodes no canvas primitive).
//!
//! Data design:
//! 1. In: per slot a recorded vello scene; per frame a log of what was painted outside the
//!    layers (a hash), where each layer was drawn, and what was painted over the layers (the
//!    top part: canvas floats, popups). Out: layer textures; the frame either through vello
//!    or, when only layer placements changed, as quads over the last frame with the top part's
//!    texture blended over them.
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
/// Layer draws one frame composes without vello at most (a canvas's tiles and stand-ins);
/// a frame with more goes through vello.
const DRAWS_MAX: usize = 1024;

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
    /// This frame's calls painted over its layers (see [`FrameLog::after`]), recorded beside
    /// the frame scene: a composed frame lays them over the layer quads.
    pub(crate) top: Scene,
    /// `top` rasterized (straight alpha, transparent elsewhere), and the frame hash and target
    /// size it was rasterized for.
    top_texture: Option<wgpu::Texture>,
    top_shown: Option<(u64, u32, u32)>,
    quad: Option<Quad>,
    /// Where a frame's patches are rendered together before their copies: grown to the
    /// largest batch, kept.
    scratch: Option<wgpu::Texture>,
}

/// Width of the patch batch (patches are packed in rows across it); its height is at most
/// the same, more patches go in another batch.
const BATCH_PX: u32 = 2048;
/// Patch placements in a batch are multiples of this (vello's tile edge), so a patch
/// rasterizes exactly as it would alone.
const BATCH_ALIGN: u32 = 16;

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
    /// Frame clips open (outside layer recordings), and how many of them are the top part's
    /// (opened after the first layer draw; the innermost ones, as clips nest).
    pub(crate) depth: u32,
    pub(crate) top_depth: u32,
    /// The top part cannot be laid over the layer quads on its own: it was painted inside a
    /// clip opened before the layers, or a layer was drawn after it.
    pub(crate) tangled: bool,
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

    /// Like [`FrameLog::call`] for a call that paints nothing over a layer (a layer draw, the
    /// end of a clip): hashed, but it does not count as painting after the layers.
    pub(crate) fn mark(&mut self, data: impl Hash) {
        if !self.recording {
            data.hash(&mut self.hash);
        }
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
        self.top.reset();
        base.as_coeffs().map(f64::to_bits).hash(&mut self.log.hash);
    }

    /// The image of layer `slot` when it is tagged `stamp` (recorded and not replaced since).
    pub(crate) fn held(&self, slot: usize, stamp: u64) -> Option<&ImageData> {
        self.slots
            .get(slot)
            .filter(|l| stamp != 0 && l.stamp == stamp)
            .and_then(|l| l.image.as_ref())
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
    /// since (rendered side by side into a scratch texture, each copied over its place), then
    /// the part painted over the layers when the frame may be composed and that part's texture
    /// is out of date (see [`Layers::reusable`]). Call once per frame after painting and before
    /// rendering or composing the frame.
    ///
    /// Cost: one vello render per recorded layer and per 2048-px-square batch of
    /// patches (one, for a frame's few tiles), plus one target-sized render of the top part
    /// on the first composed frame after it changed; nothing when none was recorded.
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
        // Every slot's patches, packed in rows across one scratch texture (in batches).
        let mut parts: Vec<(usize, Scene, [u32; 4])> = Vec::new();
        for (k, l) in self.slots.iter_mut().enumerate() {
            if l.image.is_none() || l.texture.is_none() {
                l.patches.clear(); // no picture to patch (its first raster failed)
            }
            parts.extend(l.patches.drain(..).map(|(sc, r)| (k, sc, r)));
        }
        let mut next = 0;
        while next < parts.len() {
            // Shelf packing: (x, y) per part of this batch.
            let (mut x, mut y, mut row, mut at) = (0u32, 0u32, 0u32, Vec::new());
            for (_, _, [_, _, w, h]) in &parts[next..] {
                let (w, h) = (
                    w.next_multiple_of(BATCH_ALIGN),
                    h.next_multiple_of(BATCH_ALIGN),
                );
                assert!(w <= BATCH_PX && h <= BATCH_PX, "a patch fits a batch");
                if x + w > BATCH_PX {
                    (x, y, row) = (0, y + row, 0);
                }
                if y + h > BATCH_PX {
                    break;
                }
                at.push((x, y));
                (x, row) = (x + w, row.max(h));
            }
            let batch = &parts[next..next + at.len()];
            let height = y + row;
            let mut scene = Scene::new();
            for ((_, part, [_, _, w, h]), &(px, py)) in batch.iter().zip(&at) {
                let place = Affine::translate((f64::from(px), f64::from(py)));
                let clip = Rect::new(0.0, 0.0, f64::from(*w), f64::from(*h));
                scene.push_clip_layer(vello::peniko::Fill::NonZero, place, &clip);
                scene.append(part, Some(place));
                scene.pop_layer();
            }
            let small = self.scratch.as_ref().is_none_or(|t| t.height() < height);
            if small {
                self.scratch = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("layer patches"),
                    size: wgpu::Extent3d {
                        width: BATCH_PX,
                        height: height.next_power_of_two().min(BATCH_PX),
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                }));
            }
            let scratch = self.scratch.as_ref().expect("made above");
            let params = RenderParams {
                base_color: Color::TRANSPARENT,
                width: scratch.width(),
                height: scratch.height(),
                antialiasing_method: AaConfig::Area,
            };
            let view = scratch.create_view(&wgpu::TextureViewDescriptor::default());
            renderer.render_to_texture(device, queue, &scene, &view, &params)?;
            let mut encoder = device.create_command_encoder(&Default::default());
            for ((k, _, [x, y, w, h]), &(px, py)) in batch.iter().zip(&at) {
                let l = &self.slots[*k];
                let (image, texture) = (
                    l.image.as_ref().expect("checked above"),
                    l.texture.as_ref().expect("checked above"),
                );
                assert!(x + w <= image.width && y + h <= image.height);
                encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: scratch,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x: px, y: py, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x: *x, y: *y, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: *w,
                        height: *h,
                        depth_or_array_layers: 1,
                    },
                );
            }
            queue.submit([encoder.finish()]);
            for (k, _, _) in batch {
                let image = self.slots[*k].image.as_ref().expect("checked above");
                renderer.mark_override_image_dirty(image);
            }
            assert!(!at.is_empty(), "a batch holds a patch");
            next += at.len();
        }
        // The top part, once per picture of it: when this frame paints what vello last
        // rendered (so it may be composed) and the texture holds an older top part.
        let hash = self.log.hash.finish();
        let due = self
            .shown
            .filter(|s| s.0 == hash && self.top_shown != Some(*s));
        if let (Some((_, w, h)), true, false) = (due, self.log.after, self.log.tangled) {
            let stale = self
                .top_texture
                .as_ref()
                .is_none_or(|t| t.size().width != w || t.size().height != h);
            if stale {
                self.top_texture = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("layers top"),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                }));
            }
            let texture = self.top_texture.as_ref().expect("made above");
            let params = RenderParams {
                base_color: Color::TRANSPARENT,
                width: w,
                height: h,
                antialiasing_method: AaConfig::Area,
            };
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.top_shown = None; // until it is rendered
            renderer.render_to_texture(device, queue, &self.top, &view, &params)?;
            self.top_shown = due;
        }
        assert!(self.top_shown.is_none() || self.top_texture.is_some());
        Ok(())
    }

    /// Whether this frame may skip vello: it painted exactly what the frame vello last rendered
    /// into a `w × h` target painted, apart from where its layers are drawn. Then
    /// [`Layers::compose`] over a copy of that target is the whole frame: the layer quads, then
    /// what was painted over them (canvas floats, popups) from its own texture, which
    /// [`Layers::raster`] renders once per change of it. Not when that part was painted inside
    /// a clip opened before the layers or under a later layer draw.
    pub fn reusable(&self, w: u32, h: u32) -> bool {
        let hash = self.log.hash.finish();
        !self.log.tangled
            && (!self.log.after || self.top_shown == Some((hash, w, h)))
            && !self.log.draws.is_empty()
            && self.log.draws.len() <= DRAWS_MAX
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
    /// a copy, bilinear for a stretched stand-in (whose uncovered clip pixels get its fill);
    /// then, when the frame painted over its layers, that top part blended over them
    /// ("over", as vello composes it). Call only when [`Layers::reusable`].
    ///
    /// Cost: one render pass, one quad per layer draw plus one for the top part.
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
            self.log.draws.len() <= DRAWS_MAX,
            "Layers::reusable caps the draws"
        );
        let quad = match &self.quad {
            Some(q) if q.format == format => self.quad.as_ref().expect("checked"),
            _ => self.quad.insert(Quad::new(device, format)),
        };
        let bind = |view: &wgpu::TextureView, sampler: &wgpu::Sampler, offset: u64| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("layer quad"),
                layout: &quad.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
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
            })
        };
        let mut groups = Vec::with_capacity(self.log.draws.len() + 1);
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
            let sampler = match stretch {
                Some(_) => &quad.linear,
                None => &quad.nearest,
            };
            let group = bind(&view, sampler, offset);
            groups.push((group, clip, &quad.pipeline));
        }
        // The top part over everything, from the slot after the draws' last.
        if self.log.after {
            let texture = self
                .top_texture
                .as_ref()
                .expect("Layers::reusable: rasterized");
            assert_eq!(
                (texture.width(), texture.height()),
                (w, h),
                "the top part fits"
            );
            let offset = (DRAWS_MAX * Quad::STRIDE) as u64;
            let mut bytes = [0u8; 32];
            for (i, v) in [0.0f32, 0.0, 1.0, 1.0].into_iter().enumerate() {
                bytes[4 * i..4 * i + 4].copy_from_slice(&v.to_ne_bytes());
            }
            queue.write_buffer(&quad.uniforms, offset, &bytes);
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let full = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
            groups.push((bind(&view, &quad.nearest, offset), full, &quad.over));
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
        for (group, clip, pipeline) in &groups {
            pass.set_pipeline(pipeline);
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
    /// Replaces the target's pixels (a layer draw) or blends over them (the top part).
    pipeline: wgpu::RenderPipeline,
    over: wgpu::RenderPipeline,
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
        let pipeline = |blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                        blend,
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
            })
        };
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
            pipeline: pipeline(None),
            // vello stores straight (unpremultiplied) alpha in its targets.
            over: pipeline(Some(wgpu::BlendState::ALPHA_BLENDING)),
            layout,
            uniforms: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("layer quad"),
                // One slot per layer draw, then the top part's.
                size: (Self::STRIDE * (DRAWS_MAX + 1)) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            nearest: sampler(wgpu::FilterMode::Nearest),
            linear: sampler(wgpu::FilterMode::Linear),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painter::{Painter, ScenePainter};

    const CANVAS: Rect = Rect::new(0.0, 0.0, 64.0, 64.0);
    const FLOAT: Rect = Rect::new(8.0, 8.0, 40.0, 20.0);

    /// Paints one frame: `before`, a draw of layer slot 0 (recorded on first use), then
    /// `after`; returns (painted after the layers, tangled, paths in the top part).
    fn frame(
        layers: &mut Layers,
        before: impl FnOnce(&mut ScenePainter),
        after: impl FnOnce(&mut ScenePainter),
    ) -> (bool, bool, u32) {
        let mut scene = Scene::new();
        layers.begin_frame(Affine::IDENTITY);
        let mut p = ScenePainter {
            scene: &mut scene,
            base: Affine::IDENTITY,
            layers: Some(layers),
        };
        if !p.holds_layer(0, 1) {
            assert!(p.layer(0, 1, 64, 64, |p| p.fill_rect(CANVAS, Color::BLACK)));
        }
        before(&mut p);
        assert!(p.draw_layer(0, 1, Affine::IDENTITY, CANVAS, None));
        after(&mut p);
        let log = &layers.log;
        (log.after, log.tangled, layers.top.encoding().n_paths)
    }

    #[test]
    fn what_is_painted_over_the_layers_is_the_top_part() {
        let mut layers = Layers::default();
        let float = |p: &mut ScenePainter| {
            p.push_clip(FLOAT);
            p.fill_rect(FLOAT, Color::WHITE);
            p.pop_clip();
        };
        // A float in its own clip: composable, the top part holds it.
        let (after, tangled, paths) = frame(&mut layers, |_| {}, float);
        assert!(after && !tangled && paths > 0, "{after} {tangled} {paths}");
        assert_eq!((layers.log.depth, layers.log.top_depth), (0, 0));
        // Nothing over the layers: no top part (painting before them stays out of it).
        let (after, tangled, paths) =
            frame(&mut layers, |p| p.fill_rect(CANVAS, Color::WHITE), |_| {});
        assert!(
            !after && !tangled && paths == 0,
            "{after} {tangled} {paths}"
        );
        // Negative space: a float inside a clip opened before the layers cannot be laid over
        // them alone; nor can a float under a later layer draw.
        let (after, tangled, _) = frame(
            &mut layers,
            |p| p.push_clip(CANVAS),
            |p| {
                p.fill_rect(FLOAT, Color::WHITE);
                p.pop_clip();
            },
        );
        assert!(after && tangled);
        assert_eq!((layers.log.depth, layers.log.top_depth), (0, 0));
        // A float in its own clip inside an outer one: the clips still balance in both scenes.
        let (_, tangled, _) = frame(
            &mut layers,
            |p| p.push_clip(CANVAS),
            |p| {
                float(p);
                let open = p.layers.as_deref().map(|l| (l.log.depth, l.log.top_depth));
                assert_eq!(
                    open,
                    Some((1, 0)),
                    "the float's clip closed in the top part"
                );
                p.pop_clip();
            },
        );
        assert!(tangled);
        assert_eq!((layers.log.depth, layers.log.top_depth), (0, 0));
        let (_, tangled, _) = frame(
            &mut layers,
            |_| {},
            |p| {
                float(p);
                assert!(p.draw_layer(0, 1, Affine::IDENTITY, CANVAS, None));
            },
        );
        assert!(tangled);
        // The top part was never rasterized (no GPU here): no frame is reusable.
        let _ = frame(&mut layers, |_| {}, float);
        assert!(!layers.reusable(64, 64));
    }
}
