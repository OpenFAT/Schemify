//! The frame's pipelines: a device and a target format in, the GPU state every frame draws
//! with out ([`Pipelines`]): the canvas pipelines (`shaders/canvas/`) and their camera
//! uniform, the batch pipeline (`shaders/batch.wgsl`), the coverage atlas's texture and the
//! images' bind groups. Uploading and drawing a frame is [`super::pass`].
//!
//! Contents:
//! - `pub struct Pipelines`, `Pipelines::new`
//! - `pub(super) const UNIFORM_BYTES`, `UI_UNIFORM_BYTES`
//! - `pub(super) struct TexelRect`
//! - `pub(super) fn buffer`, `texture`, `write_texels`, `bind_batch` (GPU resources)

use std::collections::HashMap;

use crate::atlas::ATLAS_SIDE;
use crate::{batch, ui_rows};

use super::{ARC, EDGE, KINDS, LINE, RECT, SOURCES, STRIDE, TRI};

/// The frame's pipelines and their GPU state: the canvas pipelines with one vertex buffer per
/// `(source, kind)` of rows, grown by doubling, and the camera uniform; the batch pipeline
/// (`shaders/batch.wgsl`) with the frame's instance buffer, the coverage atlas's texture and the images'.
/// Owner: the window (or a benchmark driving the GPU itself).
pub struct Pipelines {
    pub(super) uniform: wgpu::Buffer,
    pub(super) bind: wgpu::BindGroup,
    pub(super) background: wgpu::RenderPipeline,
    pub(super) kinds: [wgpu::RenderPipeline; KINDS],
    pub(super) buffers: [[Option<wgpu::Buffer>; KINDS]; SOURCES],
    pub(super) uploaded: u64,
    pub(super) ui: wgpu::RenderPipeline,
    pub(super) ui_layout: wgpu::BindGroupLayout,
    pub(super) ui_uniform: wgpu::Buffer,
    pub(super) atlas: wgpu::Texture,
    pub(super) atlas_view: wgpu::TextureView,
    pub(super) sampler: wgpu::Sampler,
    /// The atlas with a 1 × 1 stand-in image: every batch draw but an image's.
    pub(super) ui_bind: wgpu::BindGroup,
    /// Image textures' bind groups by blob id (`Batch::images`).
    pub(super) images: HashMap<u64, wgpu::BindGroup>,
    pub(super) instances: Option<wgpu::Buffer>,
    /// The UI's retained rows per source (`Batch::ui`), grown by doubling, and the pane
    /// entries last written to the batch uniform.
    pub(super) ui_rows: [Option<wgpu::Buffer>; ui_rows::SOURCES],
    pub(super) panes: [[f32; 8]; ui_rows::PANES],
    pub(super) atlas_uploaded: u64,
    pub(super) instances_uploaded: u64,
}

/// A rectangle of a texture, texels.
#[derive(Clone, Copy, Debug)]
pub(super) struct TexelRect {
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) w: u32,
    pub(super) h: u32,
}

/// Camera uniform size (`Cam` in `shaders/canvas/camera.wgsl`).
pub(super) const UNIFORM_BYTES: usize = 96;
/// The batch uniform (`Ui` in `shaders/batch.wgsl`): the target's size, then per pane slot its offset and
/// clip (two `vec4`s).
pub(super) const UI_UNIFORM_BYTES: usize = 16 + ui_rows::PANES * 32;

/// The canvas pipelines' WGSL module, exactly as `Pipelines::new` hands it to wgpu: the shared
/// helpers, then the camera, the row style and the distances, then one file per pipeline (the
/// WebGL2 test validates this string). WGSL has no imports and its module-scope declarations
/// are order-independent, so the files are concatenated; a name defined in two of them fails
/// at module creation. WebGL2-compatible: no storage buffers, no compute, no texture; row data
/// as per-instance vertex attributes. Draw order is the pack's: rows grouped by palette layer,
/// each source's layers in turn, so antialiased edges blend as the CPU painter blends them.
const CANVAS_WGSL: &str = concat!(
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/shaders/instance.wgsl"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/shaders/area.wgsl"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/shaders/canvas/camera.wgsl"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/shaders/canvas/style.wgsl"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/shaders/canvas/distance.wgsl"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/shaders/canvas/background.wgsl"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/shaders/canvas/segment.wgsl"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/shaders/canvas/rect.wgsl"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/shaders/canvas/arc.wgsl"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/shaders/canvas/fill.wgsl"
    )),
);
/// The batch pipeline's WGSL module, as `Pipelines::new` hands it to wgpu: the shared helpers,
/// then the batch's one pipeline.
const UI_WGSL: &str = concat!(
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/shaders/instance.wgsl"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/shaders/area.wgsl"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/shaders/batch.wgsl"
    )),
);

const SEG_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Sint32x2, 1 => Sint32x2, 2 => Uint32, 3 => Uint32];
const ARC_ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    0 => Sint32x2, 1 => Sint32, 2 => Sint32, 3 => Sint32, 4 => Uint32, 5 => Uint32
];
const TRI_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Sint32x2, 1 => Sint32x2, 2 => Sint32x2, 3 => Uint32];
const UI_ATTRS: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
    0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Uint32, 4 => Uint32
];

const _: () = assert!(
    UNIFORM_BYTES.is_multiple_of(16),
    "the uniform is vec4-aligned"
);
const _: () = assert!(
    UI_UNIFORM_BYTES.is_multiple_of(16),
    "the batch uniform is vec4-aligned"
);
const _: () = assert!(batch::STRIDE.is_multiple_of(4), "instances are whole words");

impl std::fmt::Debug for Pipelines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipelines")
            .field("uploaded", &self.uploaded)
            .field("atlas_uploaded", &self.atlas_uploaded)
            .finish()
    }
}

impl Pipelines {
    /// Builds the pipelines for a `format` render target (the surface's, or an offscreen
    /// `Rgba8Unorm`). The canvas blends straight alpha, the batch premultiplied.
    ///
    /// # Panics
    ///
    /// Panics on a device whose textures cannot be [`ATLAS_SIDE`] texels across.
    #[must_use]
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Pipelines {
        let side = u32::from(ATLAS_SIDE);
        assert!(
            device.limits().max_texture_dimension_2d >= side,
            "an atlas-sized texture"
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas"),
            source: wgpu::ShaderSource::Wgsl(CANVAS_WGSL.into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty,
            count: None,
        };
        let uniform_entry = |bytes: usize| wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(bytes as u64),
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("canvas camera"),
            entries: &[entry(0, uniform_entry(UNIFORM_BYTES))],
        });
        let uniform = buffer(
            device,
            "canvas camera",
            UNIFORM_BYTES as u64,
            wgpu::BufferUsages::UNIFORM,
        );
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("canvas camera"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("canvas"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let canvas = |vs_fs, rows| {
            let blend = wgpu::BlendState::ALPHA_BLENDING;
            pipeline(
                device,
                &pipeline_layout,
                &module,
                vs_fs,
                rows,
                format,
                blend,
            )
        };
        // The batch: a uniform, the atlas, an image and a sampler.
        let tex = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let ui_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("batch"),
            entries: &[
                entry(0, uniform_entry(UI_UNIFORM_BYTES)),
                entry(1, tex),
                entry(2, tex),
                entry(
                    3,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
            ],
        });
        let ui_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("batch"),
            source: wgpu::ShaderSource::Wgsl(UI_WGSL.into()),
        });
        let ui_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("batch"),
            bind_group_layouts: &[Some(&ui_layout)],
            immediate_size: 0,
        });
        let ui = pipeline(
            device,
            &ui_pipeline_layout,
            &ui_module,
            ("vs_ui", "fs_ui"),
            Some((batch::STRIDE, &UI_ATTRS)),
            format,
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
        );
        let ui_uniform = buffer(
            device,
            "batch",
            UI_UNIFORM_BYTES as u64,
            wgpu::BufferUsages::UNIFORM,
        );
        let atlas = texture(device, "atlas", (side, side), wgpu::TextureFormat::R8Unorm);
        debug_assert_eq!(atlas.width(), side, "an atlas-sized texture");
        let atlas_view = atlas.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("batch"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let blank = texture(device, "no image", (1, 1), wgpu::TextureFormat::Rgba8Unorm);
        let ui_bind = bind_batch(
            device,
            &ui_layout,
            &ui_uniform,
            &atlas_view,
            &blank.create_view(&Default::default()),
            &sampler,
        );
        Pipelines {
            background: canvas(("vs_bg", "fs_bg"), None),
            kinds: [
                canvas(("vs_tri", "fs_tri"), Some((STRIDE[TRI], &TRI_ATTRS))),
                canvas(("vs_rect", "fs_rect"), Some((STRIDE[RECT], &SEG_ATTRS))),
                canvas(("vs_arc", "fs_arc"), Some((STRIDE[ARC], &ARC_ATTRS))),
                canvas(("vs_seg", "fs_seg"), Some((STRIDE[LINE], &SEG_ATTRS))),
                canvas(("vs_seg", "fs_seg"), Some((STRIDE[EDGE], &SEG_ATTRS))),
            ],
            uniform,
            bind,
            buffers: Default::default(),
            uploaded: 0,
            ui,
            ui_bind,
            ui_layout,
            ui_uniform,
            atlas,
            atlas_view,
            sampler,
            images: HashMap::new(),
            instances: None,
            ui_rows: Default::default(),
            panes: [[0.0; 8]; ui_rows::PANES],
            atlas_uploaded: 0,
            instances_uploaded: 0,
        }
    }
}

impl TexelRect {
    /// Texels covered.
    fn texels(self) -> usize {
        self.w as usize * self.h as usize
    }
}

/// A buffer of `size` bytes for `usage` (plus copies into it).
pub(super) fn buffer(
    device: &wgpu::Device,
    label: &str,
    size: u64,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    assert!(size > 0, "a buffer holds something");
    assert!(
        size.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT),
        "queue writes are word-aligned"
    );
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// A `w × h` 2D texture of `format`, sampled and written by copies.
pub(super) fn texture(
    device: &wgpu::Device,
    label: &str,
    (w, h): (u32, u32),
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    assert!(w > 0, "a texture has texels");
    assert!(h > 0, "a texture has texels");
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// Writes `data` (`rect`'s texels of `bpp` bytes, row-major) into `t` at `rect`.
pub(super) fn write_texels(
    queue: &wgpu::Queue,
    t: &wgpu::Texture,
    rect: TexelRect,
    bpp: u32,
    data: &[u8],
) {
    assert_eq!(data.len(), rect.texels() * bpp as usize, "whole texels");
    assert!(
        rect.x + rect.w <= t.width(),
        "the rect is inside the texture"
    );
    assert!(
        rect.y + rect.h <= t.height(),
        "the rect is inside the texture"
    );
    let TexelRect { x, y, w, h } = rect;
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: t,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * bpp),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

/// The batch's bind group: its uniform, the atlas, `image`, the sampler.
pub(super) fn bind_batch(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    atlas: &wgpu::TextureView,
    image: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("batch"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(atlas),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(image),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

// ---- private ----

/// A render pipeline of `module`'s `vs`/`fs` into `format`, its rows `stride`-byte instances
/// with `attributes` (none: a full-quad pipeline), blended by `blend`.
fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    (vs, fs): (&str, &str),
    rows: Option<(usize, &[wgpu::VertexAttribute])>,
    format: wgpu::TextureFormat,
    blend: wgpu::BlendState,
) -> wgpu::RenderPipeline {
    assert!(vs.starts_with("vs_"), "a vertex entry point");
    assert!(fs.starts_with("fs_"), "a fragment entry point");
    if let Some((stride, attributes)) = rows {
        debug_assert!(stride.is_multiple_of(4), "instances are whole words");
        debug_assert!(!attributes.is_empty(), "an instance has fields");
    }
    let buffers: Vec<Option<wgpu::VertexBufferLayout>> = rows
        .map(|(stride, attributes)| {
            Some(wgpu::VertexBufferLayout {
                array_stride: stride as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes,
            })
        })
        .into_iter()
        .collect();
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(vs),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(vs),
            compilation_options: Default::default(),
            buffers: &buffers,
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fs),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// User locations an entry point's arguments (`args`) or result take, struct members
    /// counted one by one.
    fn locations(module: &naga::Module, ep: &naga::EntryPoint, args: bool) -> usize {
        let count = |ty: naga::Handle<naga::Type>, binding: &Option<naga::Binding>| match (
            &module.types[ty].inner,
            binding,
        ) {
            (naga::TypeInner::Struct { members, .. }, None) => members
                .iter()
                .filter(|m| matches!(m.binding, Some(naga::Binding::Location { .. })))
                .count(),
            (_, Some(naga::Binding::Location { .. })) => 1,
            _ => 0,
        };
        if args {
            ep.function
                .arguments
                .iter()
                .map(|a| count(a.ty, &a.binding))
                .sum()
        } else {
            ep.function
                .result
                .as_ref()
                .map_or(0, |r| count(r.ty, &r.binding))
        }
    }

    /// The WebGL2 tier (web spec §7) runs these shaders through wgpu's GL backend, which
    /// translates them with naga to GLSL ES 3.00. Oracle: naga's own validator with no
    /// optional capability and its GLSL writer set up as wgpu-hal sets it for WebGL2 (no draw
    /// parameters, so `first_instance` goes through a uniform), plus WebGL2's guaranteed limits
    /// (`Limits::downlevel_webgl2_defaults`: 16 vertex attributes, 15 inter-stage variables).
    /// Negative space: a compute entry point and a storage buffer, which WebGL2 lacks, are
    /// rejected by the same checks.
    #[test]
    fn shaders_compile_to_glsl_es_300_for_webgl2() {
        use naga::back::glsl;
        let limits = wgpu::Limits::downlevel_webgl2_defaults();
        let glsl_of = |src: &str| -> Result<Vec<String>, String> {
            let module = naga::front::wgsl::parse_str(src).map_err(|e| e.emit_to_string(src))?;
            let info = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::empty(),
            )
            .validate(&module)
            .map_err(|e| format!("{e:?}"))?;
            let mut binding_map = glsl::BindingMap::default();
            for (_, g) in module.global_variables.iter() {
                if let Some(b) = g.binding {
                    let slot = u8::try_from(binding_map.len()).expect("few bindings");
                    binding_map.insert(b, slot);
                }
            }
            let options = glsl::Options {
                version: glsl::Version::Embedded {
                    version: 300,
                    is_webgl: true,
                },
                writer_flags: glsl::WriterFlags::ADJUST_COORDINATE_SPACE
                    | glsl::WriterFlags::FORCE_POINT_SIZE,
                binding_map,
                zero_initialize_workgroup_memory: true,
            };
            let mut out = Vec::new();
            for ep in &module.entry_points {
                if ep.stage == naga::ShaderStage::Vertex {
                    let (ins, outs) = (locations(&module, ep, true), locations(&module, ep, false));
                    if ins > limits.max_vertex_attributes as usize {
                        return Err(format!("{}: {ins} vertex attributes", ep.name));
                    }
                    if outs > limits.max_inter_stage_shader_variables as usize {
                        return Err(format!("{}: {outs} inter-stage variables", ep.name));
                    }
                }
                let pipeline = glsl::PipelineOptions {
                    shader_stage: ep.stage,
                    entry_point: ep.name.clone(),
                    multiview: None,
                };
                let mut text = String::new();
                glsl::Writer::new(
                    &mut text,
                    &module,
                    &info,
                    &options,
                    &pipeline,
                    naga::proc::BoundsCheckPolicies::default(),
                )
                .and_then(|mut w| w.write())
                .map_err(|e| format!("{}: {e}", ep.name))?;
                out.push(text);
            }
            Ok(out)
        };
        // Every module exactly as `Pipelines::new` hands it to wgpu.
        for (name, src) in [("canvas", CANVAS_WGSL), ("ui", UI_WGSL)] {
            let out = glsl_of(src).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!out.is_empty(), "{name}: has entry points");
            for text in &out {
                assert!(text.starts_with("#version 300 es"), "{name}: {text:.40}");
            }
        }
        // Negative space: what WebGL2 lacks fails the same checks.
        let compute = "@compute @workgroup_size(1) fn main() {}";
        assert!(glsl_of(compute).is_err(), "no compute in GLSL ES 3.00");
        let storage = "@group(0) @binding(0) var<storage, read> s: array<u32>;
            @vertex fn main() -> @builtin(position) vec4<f32> { return vec4<f32>(f32(s[0])); }";
        assert!(
            glsl_of(storage).is_err(),
            "no storage buffers in GLSL ES 3.00"
        );
    }
}
