//! The canvas pipelines: a device and a target format in, the GPU state the canvas draws with
//! out ([`Pipelines`]): one instanced pipeline per row kind (`shaders/canvas/`), the background
//! and grid, and their camera uniform. Uploading and drawing is [`super::pass`].
//!
//! Contents:
//! - `pub struct Pipelines`, `Pipelines::new`
//! - `pub(super) const UNIFORM_BYTES`
//! - `pub(super) fn buffer`

use super::{ARC, EDGE, KINDS, LINE, RECT, SOURCES, STRIDE, TRI};

/// The canvas pipelines and their GPU state: one vertex buffer per `(source, kind)` of rows,
/// grown by doubling, and the camera uniform. Owner: [`super::CanvasGpu`] in egui-wgpu's
/// callback resources (or a benchmark driving the GPU itself).
pub struct Pipelines {
    pub(super) uniform: wgpu::Buffer,
    pub(super) bind: wgpu::BindGroup,
    pub(super) background: wgpu::RenderPipeline,
    pub(super) kinds: [wgpu::RenderPipeline; KINDS],
    pub(super) buffers: [[Option<wgpu::Buffer>; KINDS]; SOURCES],
    pub(super) uploaded: u64,
}

/// Camera uniform size (`Cam` in `shaders/canvas/camera.wgsl`).
pub(super) const UNIFORM_BYTES: usize = 96;

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

const SEG_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Sint32x2, 1 => Sint32x2, 2 => Uint32, 3 => Uint32];
const ARC_ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    0 => Sint32x2, 1 => Sint32, 2 => Sint32, 3 => Sint32, 4 => Uint32, 5 => Uint32
];
const TRI_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Sint32x2, 1 => Sint32x2, 2 => Sint32x2, 3 => Uint32];

const _: () = assert!(
    UNIFORM_BYTES.is_multiple_of(16),
    "the uniform is vec4-aligned"
);

impl std::fmt::Debug for Pipelines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipelines")
            .field("uploaded", &self.uploaded)
            .finish()
    }
}

impl Pipelines {
    /// Builds the pipelines for a `format` render target (the surface's, or an offscreen
    /// `Rgba8Unorm`). The rows blend straight alpha.
    #[must_use]
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Pipelines {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas"),
            source: wgpu::ShaderSource::Wgsl(CANVAS_WGSL.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("canvas camera"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(UNIFORM_BYTES as u64),
                },
                count: None,
            }],
        });
        let uniform = buffer(device, "canvas camera", UNIFORM_BYTES as u64, wgpu::BufferUsages::UNIFORM);
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
            pipeline(device, &pipeline_layout, &module, vs_fs, rows, format, blend)
        };
        let p = Pipelines {
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
        };
        debug_assert!(p.buffers.iter().flatten().all(Option::is_none), "no rows yet");
        p
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
