// Segments (`gpu::LINE` and `gpu::EDGE` rows, 24 B: two `i32` points, colour, style): lines,
// wires, polyline and polygon edges, line marks. A round-capped capsule, dashed along its
// length; `SegIn` is also the rect rows' layout (`rect.wgsl`).

struct SegIn {
    @location(0) a: vec2<i32>,
    @location(1) b: vec2<i32>,
    @location(2) color: u32,
    @location(3) style: u32,
}

struct SegOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) p: vec2<f32>,
    @location(1) @interpolate(flat) color: vec4<f32>,
    @location(2) @interpolate(flat) a: vec2<f32>,
    @location(3) @interpolate(flat) b: vec2<f32>,
    // half width, dash, flags, solid (the far-zoom pixel box)
    @location(4) @interpolate(flat) par: vec4<f32>,
}

@vertex
fn vs_seg(@builtin(vertex_index) i: u32, in: SegIn) -> SegOut {
    var o: SegOut;
    o.color = color_of(in.color);
    let fl = flags(in.style);
    let ra = to_screen(in.a);
    let rb = to_screen(in.b);
    let c = corner(i);
    let raw = vec4<f32>(min(ra, rb), max(ra, rb));
    if o.color.a == 0.0 {
        o.pos = degenerate();
        return o;
    }
    if lod_small(raw) {
        let q = lod_box(raw);
        let p = mix(q.xy, q.zw, c);
        o.pos = to_clip(p);
        o.p = p;
        o.par = vec4<f32>(0.0, 0.0, f32(fl), 1.0);
        return o;
    }
    let so = snap_offset(in.style);
    let a = vec2<f32>(snap(ra.x, so), snap(ra.y, so));
    let b = vec2<f32>(snap(rb.x, so), snap(rb.y, so));
    let ab = b - a;
    let len = length(ab);
    let hw = width_px(in.style) * 0.5;
    let pad = hw + 1.0;
    let t = select(vec2<f32>(1.0, 0.0), ab / len, len > 0.0);
    let n = vec2<f32>(-t.y, t.x);
    let p = a + t * mix(-pad, len + pad, c.x) + n * mix(-pad, pad, c.y);
    o.pos = to_clip(p);
    o.p = p;
    o.a = a;
    o.b = b;
    o.par = vec4<f32>(hw, dash_px(in.style), f32(fl), 0.0);
    return o;
}

@fragment
fn fs_seg(o: SegOut) -> @location(0) vec4<f32> {
    if o.par.w > 0.0 {
        return o.color;
    }
    let ab = o.b - o.a;
    let len = length(ab);
    let t = select(vec2<f32>(1.0, 0.0), ab / len, len > 0.0);
    let q = o.p - o.a;
    let s = dot(q, t);
    let hw = o.par.x;
    // A capsule: round caps; a dashed stroke is round-capped dashes (vello_cpu strokes each one).
    // Within the span (a pixel whole inside it along the stroke) the coverage is the exact area
    // of the band the stroke covers; at the ends the distance approximation of the caps.
    let h = dash_near(s, len, o.par.y);
    let d = length(q - t * h) - hw;
    let n = vec2<f32>(-t.y, t.x);
    var cov: f32;
    let solid = o.par.y <= 0.0 || abs(h - s) < 1e-4;
    if d >= 1.0 {
        cov = 0.0;
    } else if solid && s >= 0.7 && s <= len - 0.7 {
        let off = dot(q, n);
        cov = half_plane_area(n, hw - off) - half_plane_area(n, -hw - off);
    } else {
        // Near a cap: the share of 4 × 4 subsamples of the pixel inside the capsule.
        var inside = 0.0;
        for (var j = 0; j < 4; j += 1) {
            for (var i = 0; i < 4; i += 1) {
                let qs = q + (vec2<f32>(f32(i), f32(j)) - 1.5) * 0.25;
                let ss = dot(qs, t);
                let hs = dash_near(ss, len, o.par.y);
                inside += f32(length(qs - t * hs) <= hw);
            }
        }
        cov = inside / 16.0;
    }
    return shade(o.color, cov);
}
