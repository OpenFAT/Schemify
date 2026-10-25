// Polygon fills (`gpu::TRI` rows, 28 B: three `i32` points and a colour): plain triangles from
// the pack's ear clipping. The outline drawn over them is antialiased, so the fill's edge needs
// no feather.

struct TriIn {
    @location(0) p0: vec2<i32>,
    @location(1) p1: vec2<i32>,
    @location(2) p2: vec2<i32>,
    @location(3) color: u32,
}

struct TriOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) color: vec4<f32>,
}

@vertex
fn vs_tri(@builtin(vertex_index) i: u32, in: TriIn) -> TriOut {
    var o: TriOut;
    o.color = color_of(in.color);
    if o.color.a == 0.0 {
        o.pos = degenerate();
        return o;
    }
    let k = i % 3u;
    var p = in.p0;
    if k == 1u {
        p = in.p1;
    } else if k == 2u {
        p = in.p2;
    }
    o.pos = to_clip(to_screen(p));
    return o;
}

@fragment
fn fs_tri(o: TriOut) -> @location(0) vec4<f32> {
    return o.color;
}
