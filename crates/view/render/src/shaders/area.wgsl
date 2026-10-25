// Exact pixel coverage, shared by both shader modules: the area of a pixel on one side of a
// line, which the stroke and ring shaders difference to get a band's area. This is the CPU
// rasterizer's (`vello_cpu`'s) area coverage, so GPU frames match the CPU oracle.

// The area of the unit pixel square (centred on the fragment) on the side `n · (x - c) ≤ t`
// of a line with unit normal `n`: exact, piecewise quadratic in `t` (the CPU rasterizer's area
// coverage; `coverage` in `canvas/distance.wgsl` is the linear distance approximation).
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
