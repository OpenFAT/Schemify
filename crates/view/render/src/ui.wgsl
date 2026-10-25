// The GPU painter's frame batch (`batch.rs`): one instance per UI rect, atlas mask (a glyph,
// an icon, a math delimiter), stroke segment, fill triangle or image, in device px, each with
// its clip rect. Coverage is exact pixel area where it can be (a rect, a clip, the band of a
// stroke), so a frame matches the CPU painter (`vello_cpu`) within antialiasing. Output is
// premultiplied. WebGL2-compatible: instance data as per-instance vertex attributes, no
// storage buffers, no compute.

// Scroll pane slots (`ui_rows::PANES`): an instance's kind word carries its slot above the low
// byte; slot 0 is no pane.
const PANES: u32 = 32u;

struct Ui {
    size: vec2<f32>,  // the target, device px
    pad: vec2<f32>,
    // Per pane slot: its offset (device px, subtracted from the instance's coordinates), then
    // its clip (device px, x0 y0 x1 y1), intersected with the instance's.
    panes: array<vec4<f32>, 64>,
}

@group(0) @binding(0) var<uniform> ui: Ui;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var image: texture_2d<f32>;
@group(0) @binding(3) var smp: sampler;

// Instance kinds (`batch.rs`).
const RECT: u32 = 0u;
const MASK: u32 = 1u;
const SEG: u32 = 2u;
const TRI: u32 = 3u;
const IMAGE: u32 = 4u;
const MASK_SCALED: u32 = 5u;

struct In {
    // RECT, MASK, IMAGE: the box x0 y0 x1 y1; SEG: its ends; TRI: two corners.
    @location(0) pos: vec4<f32>,
    // MASK: the atlas texels x0 y0 x1 y1; SEG: half width; TRI: the third corner; IMAGE: uv.
    @location(1) aux: vec4<f32>,
    @location(2) clip: vec4<f32>,
    @location(3) color: u32,
    @location(4) kind: u32,
}

struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) p: vec2<f32>,
    @location(1) @interpolate(flat) color: vec4<f32>,
    @location(2) @interpolate(flat) a: vec4<f32>,
    @location(3) @interpolate(flat) b: vec4<f32>,
    @location(4) @interpolate(flat) clip: vec4<f32>,
    @location(5) @interpolate(flat) kind: u32,
}

fn corner(i: u32) -> vec2<f32> {
    let k = i % 6u;
    return vec2<f32>(f32(k == 1u || k == 3u || k == 4u), f32(k == 2u || k == 4u || k == 5u));
}

fn to_clip(p: vec2<f32>) -> vec4<f32> {
    let n = p / ui.size * 2.0 - 1.0;
    return vec4<f32>(n.x, -n.y, 0.0, 1.0);
}

// RGBA8 bytes in memory order (R first) → straight-alpha colour.
fn color_of(c: u32) -> vec4<f32> {
    return vec4<f32>(f32(c & 0xffu), f32((c >> 8u) & 0xffu), f32((c >> 16u) & 0xffu), f32(c >> 24u)) / 255.0;
}

@vertex
fn vs_ui(@builtin(vertex_index) i: u32, raw: In) -> Out {
    // A retained row in a scroll pane is in the pane's content coordinates: move it by the
    // pane's offset and clip it to the pane.
    let pane = min(raw.kind >> 8u, PANES - 1u);
    let off = ui.panes[2u * pane].xy;
    let pc = ui.panes[2u * pane + 1u];
    var in = raw;
    in.kind = raw.kind & 0xffu;
    in.pos = raw.pos - vec4<f32>(off, off);
    if in.kind == TRI {
        in.aux = vec4<f32>(raw.aux.xy - off, raw.aux.zw);
    }
    in.clip = vec4<f32>(max(raw.clip.xy - off, pc.xy), min(raw.clip.zw - off, pc.zw));
    var o: Out;
    o.color = color_of(in.color);
    o.a = in.pos;
    o.b = in.aux;
    o.clip = in.clip;
    o.kind = in.kind;
    var p: vec2<f32>;
    if in.kind == SEG {
        let ab = in.pos.zw - in.pos.xy;
        let len = length(ab);
        let t = select(vec2<f32>(1.0, 0.0), ab / len, len > 0.0);
        let n = vec2<f32>(-t.y, t.x);
        let pad = in.aux.x + 1.0;
        let c = corner(i);
        p = in.pos.xy + t * mix(-pad, len + pad, c.x) + n * mix(-pad, pad, c.y);
    } else if in.kind == TRI {
        let k = i % 6u;
        p = select(select(in.aux.xy, in.pos.zw, k == 1u), in.pos.xy, k == 0u);
        if k > 2u {
            p = in.pos.xy; // the second triangle of the quad is degenerate
        }
    } else {
        // The box's whole pixels, cut to the clip's.
        let lo = max(floor(in.pos.xy), floor(in.clip.xy));
        let hi = min(ceil(in.pos.zw), ceil(in.clip.zw));
        p = mix(lo, max(hi, lo), corner(i));
    }
    o.pos = to_clip(p);
    o.p = p;
    return o;
}

// The share of the pixel centred at `p` that box `r` (x0 y0 x1 y1) covers: exact.
fn box_cov(p: vec2<f32>, r: vec4<f32>) -> f32 {
    let lo = max(p - 0.5, r.xy);
    let hi = min(p + 0.5, r.zw);
    let d = clamp(hi - lo, vec2<f32>(0.0), vec2<f32>(1.0));
    return d.x * d.y;
}

// The area of the unit pixel square (centred on the fragment) on the side `n · (x - c) ≤ t`
// of a line with unit normal `n` (as `gpu.wgsl`'s).
fn half_plane_area(n: vec2<f32>, t: f32) -> f32 {
    let a = min(abs(n.x), abs(n.y));
    let b = max(abs(n.x), abs(n.y));
    let w = a + b;
    let u = clamp(t + 0.5 * w, 0.0, w);
    if a < 1e-4 {
        return u / w;
    }
    if u < a {
        return u * u / (2.0 * a * b);
    }
    if u > b {
        return 1.0 - (w - u) * (w - u) / (2.0 * a * b);
    }
    return (u - 0.5 * a) / b;
}

// A capsule's coverage: the exact band area along it, 4 × 4 subsamples near its round caps.
fn seg_cov(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>, hw: f32) -> f32 {
    let ab = b - a;
    let len = length(ab);
    let t = select(vec2<f32>(1.0, 0.0), ab / len, len > 0.0);
    let q = p - a;
    let s = dot(q, t);
    let d = length(q - t * clamp(s, 0.0, len)) - hw;
    if d >= 1.0 {
        return 0.0;
    }
    if s >= 0.7 && s <= len - 0.7 {
        let n = vec2<f32>(-t.y, t.x);
        let off = dot(q, n);
        return half_plane_area(n, hw - off) - half_plane_area(n, -hw - off);
    }
    var inside = 0.0;
    for (var j = 0; j < 4; j += 1) {
        for (var i = 0; i < 4; i += 1) {
            let qs = q + (vec2<f32>(f32(i), f32(j)) - 1.5) * 0.25;
            let ss = clamp(dot(qs, t), 0.0, len);
            inside += f32(length(qs - t * ss) <= hw);
        }
    }
    return inside / 16.0;
}

@fragment
fn fs_ui(o: Out) -> @location(0) vec4<f32> {
    let clip = box_cov(o.p, o.clip);
    if o.kind == IMAGE {
        let uv = mix(o.b.xy, o.b.zw, (o.p - o.a.xy) / (o.a.zw - o.a.xy));
        return textureSampleLevel(image, smp, uv, 0.0) * clip * box_cov(o.p, o.a);
    }
    var cov = 1.0;
    if o.kind == RECT {
        cov = box_cov(o.p, o.a);
    } else if o.kind == MASK {
        // One texel per pixel: the cell sits on whole pixels.
        let t = vec2<i32>(floor(o.p - o.a.xy + o.b.xy));
        cov = textureLoad(atlas, t, 0).r;
    } else if o.kind == MASK_SCALED {
        // A glyph rasterized smaller, magnified: bilinear, kept off the neighbouring cells.
        let f = (o.p - o.a.xy) / (o.a.zw - o.a.xy);
        let t = clamp(mix(o.b.xy, o.b.zw, f), o.b.xy + 0.5, o.b.zw - 0.5);
        let size = vec2<f32>(textureDimensions(atlas));
        cov = textureSampleLevel(atlas, smp, t / size, 0.0).r * box_cov(o.p, o.a);
    } else if o.kind == SEG {
        cov = seg_cov(o.p, o.a.xy, o.a.zw, o.b.x);
    }
    let a = o.color.a * cov * clip;
    return vec4<f32>(o.color.rgb * a, a);
}
