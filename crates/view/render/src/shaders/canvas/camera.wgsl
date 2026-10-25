// The canvas camera (`gpu::Frame` written by `Pipelines::draw` as `Cam`): world milli-units
// to device px, pixel snapping, and the far-zoom cut. Rows are `i32` world milli-units (D33);
// the camera centre is split into an integer and a fractional part, so the subtraction happens
// in integers and stays exact over the D33 range (`render-backend.md` § 4). Strokes snap to
// logical pixels as `canvas::draw_prims` snaps them, so a 1 px line covers one pixel row and
// the CPU painter stays the oracle.

struct Cam {
    center: vec2<i32>,     // world milli-units at the view centre, integer part
    frac: vec2<f32>,       // its fractional part, milli-units
    view: vec4<f32>,       // the canvas rectangle, device px: x0 y0 x1 y1
    scale_dev: vec2<f32>,  // device px per milli-unit; device px per logical px
    size: vec2<f32>,       // the target's size, device px
    grid: vec4<f32>,       // dot pitch (logical px, 0 = no grid), phase x, phase y, unused
    grid_color: vec4<f32>,
    background: vec4<f32>,
}

@group(0) @binding(0) var<uniform> cam: Cam;

// `canvas::LOD_PX`: a row at most this many logical px each way is the whole pixels it touches.
const LOD_PX: f32 = 2.0;

// The CPU painter snaps `floor(v + 1e-6)` in f64. In f32 a screen coordinate lands within
// about 1e-4 px of that value (an 800 px offset from the view centre times an f32 scale), so
// a coordinate the CPU puts exactly on a pixel edge (a grid multiple at a round zoom) needs a
// bias past that error to snap the same way; one within the bias below a pixel edge snaps a
// pixel apart (measured: a few in 10^5 rows at the 100k fit).
const SNAP_BIAS: f32 = 2.5e-4;

fn dev() -> f32 {
    return cam.scale_dev.y;
}

// World → device px: the integer difference first (exact), then f32.
fn to_screen(p: vec2<i32>) -> vec2<f32> {
    let d = vec2<f32>(p - cam.center) - cam.frac;
    return d * cam.scale_dev.x + 0.5 * (cam.view.xy + cam.view.zw);
}

fn to_clip(p: vec2<f32>) -> vec4<f32> {
    let n = p / cam.size * 2.0 - 1.0;
    return vec4<f32>(n.x, -n.y, 0.0, 1.0);
}

// A stroke coordinate snapped as `canvas::snap` / `canvas::pt_at` do, in logical px: to the
// pixel centre (o = 0.5) or the pixel edge (o = 0).
fn snap(v: f32, o: f32) -> f32 {
    let d = dev();
    return (floor(v / d + SNAP_BIAS) + o) * d;
}

fn lod_small(b: vec4<f32>) -> bool {
    let d = dev();
    return (b.z - b.x) <= LOD_PX * d && (b.w - b.y) <= LOD_PX * d;
}

// The whole logical pixels box `b` (device px) touches, as `canvas::Lod::mark` lights them.
fn lod_box(b: vec4<f32>) -> vec4<f32> {
    let d = dev();
    let lo = floor(b.xy / d + SNAP_BIAS) * d;
    let hi = (floor(b.zw / d + SNAP_BIAS) + 1.0) * d;
    return vec4<f32>(lo, hi);
}

fn degenerate() -> vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
