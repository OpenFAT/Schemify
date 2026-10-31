//! Canvas labels on the GPU: each row source's labels are one egui text mesh in octave space
//! ([`LabelMesh`], tessellated by the app when its labels change), uploaded once and drawn
//! through a uniform that moves it to the screen. A pan, a hover or a keystroke writes that
//! uniform only, so labels cost egui no tessellation and the frame no vertex copy (measured
//! 2026-10-31, 1k labels: about 0.3 ms of a hover frame as an egui mesh). The mesh samples
//! egui's font atlas through egui-wgpu's own bind group for it, so a glyph looks as egui draws
//! it.
//!
//! Contents: `pub struct LabelMesh`, crate-only `struct TextGpu` (`new`, `prepare`, `draw`)

use std::sync::Arc;

use egui::emath::TSTransform;
use egui::epaint::Mesh;

use super::SOURCES;
use super::pipelines;

/// One row source's labels: triangles in octave space and where they go this frame.
#[derive(Clone, Debug)]
pub struct LabelMesh {
    /// The labels tessellated; uploaded again only when this is a new mesh.
    pub mesh: Arc<Mesh>,
    /// Octave space to logical px.
    pub to_screen: TSTransform,
    /// The glyph atlas's size the mesh's texture coordinates are normalised to.
    pub atlas: [usize; 2],
}

/// The labels' pipeline, its per-source uniforms and the meshes uploaded.
pub(crate) struct TextGpu {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    uploaded: [Option<Uploaded>; SOURCES],
    /// egui-wgpu's bind group of the font atlas, as of the last prepare.
    font: Option<wgpu::BindGroup>,
}

/// A mesh on the GPU and the mesh it was written from.
struct Uploaded {
    mesh: Arc<Mesh>,
    vertex: wgpu::Buffer,
    index: wgpu::Buffer,
}

/// Bytes between two sources' uniforms (wgpu's default dynamic offset alignment).
const UNIFORM_STRIDE: usize = 256;
/// Bytes of one uniform (`Locals` in `shaders/labels.wgsl`).
const UNIFORM_BYTES: usize = 32;
/// Bytes per vertex: position, texture coordinates, colour.
const VERTEX_BYTES: usize = 20;

const _: () = assert!(UNIFORM_BYTES <= UNIFORM_STRIDE);

impl TextGpu {
    /// The labels' pipeline for a `format` target (8-bit unorm, as egui-wgpu prefers).
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> TextGpu {
        assert!(!format.is_srgb(), "egui draws into a gamma target");
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("labels"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/src/shaders/labels.wgsl"
                ))
                .into(),
            ),
        });
        let locals = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("labels locals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(UNIFORM_BYTES as u64),
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("labels"),
            bind_group_layouts: &[Some(&locals), Some(&font_layout(device))],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("labels"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: VERTEX_BYTES as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Uint32],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            // egui-wgpu's blend: premultiplied colour.
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let usage = wgpu::BufferUsages::UNIFORM;
        let size = (UNIFORM_STRIDE * SOURCES) as u64;
        let uniform = pipelines::buffer(device, "labels locals", size, usage);
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("labels locals"),
            layout: &locals,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniform,
                    offset: 0,
                    size: wgpu::BufferSize::new(UNIFORM_BYTES as u64),
                }),
            }],
        });
        TextGpu {
            pipeline,
            uniform,
            bind,
            uploaded: Default::default(),
            font: None,
        }
    }

    /// Uploads each source's mesh when it is new and writes every source's uniform for a
    /// `size`-px target at `ppp` device px per logical px; `font` is egui-wgpu's bind group of
    /// the font atlas.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        labels: &[Option<LabelMesh>; SOURCES],
        font: Option<wgpu::BindGroup>,
        ppp: f32,
        size: [u32; 2],
    ) {
        assert!(ppp > 0.0, "a positive device scale, {ppp}");
        self.font = font;
        let mut locals = [0u8; UNIFORM_STRIDE * SOURCES];
        for (src, set) in labels.iter().enumerate() {
            let Some(set) = set else {
                continue;
            };
            let current = self.uploaded[src]
                .as_ref()
                .is_some_and(|u| Arc::ptr_eq(&u.mesh, &set.mesh));
            if !current && !set.mesh.indices.is_empty() {
                self.uploaded[src] = Some(upload(device, queue, &set.mesh));
            }
            let t = set.to_screen;
            let words = [
                size[0] as f32,
                size[1] as f32,
                t.translation.x * ppp,
                t.translation.y * ppp,
                set.atlas[0] as f32,
                set.atlas[1] as f32,
                t.scaling * ppp,
                0.0,
            ];
            let at = src * UNIFORM_STRIDE;
            for (k, w) in words.iter().enumerate() {
                locals[at + 4 * k..at + 4 * k + 4].copy_from_slice(&w.to_le_bytes());
            }
        }
        queue.write_buffer(&self.uniform, 0, &locals);
    }

    /// Draws source `src`'s labels into `pass` (its viewport the whole `size`-px target), if
    /// the frame has them.
    pub(crate) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        labels: &[Option<LabelMesh>; SOURCES],
        src: usize,
        size: (u32, u32),
    ) {
        assert!(src < SOURCES, "a row source");
        let (Some(set), Some(up), Some(font)) = (&labels[src], &self.uploaded[src], &self.font)
        else {
            return;
        };
        if set.mesh.indices.is_empty() || !Arc::ptr_eq(&up.mesh, &set.mesh) {
            return; // nothing to draw, or not prepared
        }
        pass.set_viewport(0.0, 0.0, size.0 as f32, size.1 as f32, 0.0, 1.0);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[(src * UNIFORM_STRIDE) as u32]);
        pass.set_bind_group(1, font, &[]);
        pass.set_vertex_buffer(0, up.vertex.slice(..));
        pass.set_index_buffer(up.index.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..set.mesh.indices.len() as u32, 0, 0..1);
    }
}

// ---- private ----

/// The layout of egui-wgpu's texture bind groups, entry for entry, so its font atlas's bind
/// group is group-equivalent (WebGPU) and binds to the labels' pipeline.
fn font_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("labels font"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(16),
                },
                count: None,
            },
        ],
    })
}

/// `mesh` written to new vertex and index buffers.
fn upload(device: &wgpu::Device, queue: &wgpu::Queue, mesh: &Arc<Mesh>) -> Uploaded {
    let mut vertices = Vec::with_capacity(mesh.vertices.len() * VERTEX_BYTES);
    for v in &mesh.vertices {
        for w in [v.pos.x, v.pos.y, v.uv.x, v.uv.y] {
            vertices.extend_from_slice(&w.to_le_bytes());
        }
        vertices.extend_from_slice(&v.color.to_array());
    }
    let indices: Vec<u8> = mesh.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
    let usage = wgpu::BufferUsages::VERTEX;
    let vertex = pipelines::buffer(device, "labels", vertices.len() as u64, usage);
    let usage = wgpu::BufferUsages::INDEX;
    let index = pipelines::buffer(device, "labels", indices.len() as u64, usage);
    queue.write_buffer(&vertex, 0, &vertices);
    queue.write_buffer(&index, 0, &indices);
    Uploaded {
        mesh: Arc::clone(mesh),
        vertex,
        index,
    }
}
