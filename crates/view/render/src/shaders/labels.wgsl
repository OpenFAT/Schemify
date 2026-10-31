// Canvas labels (`gpu/text.rs`): an egui text mesh in octave space, moved to the screen by one
// uniform per label set and drawn as egui-wgpu draws text (premultiplied sRGB colour times
// the font atlas, into an 8-bit unorm target).

struct Locals {
    // Target size, device px.
    size: vec2<f32>,
    // Octave space to device px: `pos * scale + offset`.
    offset: vec2<f32>,
    // The atlas size the texture coordinates were normalised to (it only grows since).
    atlas: vec2<f32>,
    scale: f32,
    pad: f32,
};
@group(0) @binding(0) var<uniform> u: Locals;

// egui's texture bind group (binding 2, its filtering flags, unused here).
@group(1) @binding(0) var font: texture_2d<f32>;
@group(1) @binding(1) var font_sampler: sampler;

struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: u32,
) -> Out {
    let px = pos * u.scale + u.offset;
    var o: Out;
    o.pos = vec4<f32>(2.0 * px.x / u.size.x - 1.0, 1.0 - 2.0 * px.y / u.size.y, 0.0, 1.0);
    o.uv = uv;
    o.color = vec4<f32>(
        f32(color & 255u),
        f32((color >> 8u) & 255u),
        f32((color >> 16u) & 255u),
        f32((color >> 24u) & 255u),
    ) / 255.0;
    return o;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    let uv = in.uv * u.atlas / vec2<f32>(textureDimensions(font));
    return in.color * textureSample(font, font_sampler, uv);
}
