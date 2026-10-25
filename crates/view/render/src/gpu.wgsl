// The GPU canvas (`gpu.rs`): one quad per primitive, expanded in the vertex shader and shaded
// with analytic (signed-distance) coverage, under one camera uniform. WebGL2-compatible: no
// storage buffers, no compute, no texture; instance data as per-instance vertex attributes.
//
// Rows are `i32` world milli-units (D33). The camera centre is split into an integer and a
// fractional part, so the subtraction happens in integers and stays exact over the D33 range
// (`render-backend.md` § 4). Strokes snap to logical pixels as `canvas::draw_prims` snaps
// them, so a 1 px line covers one pixel row and the CPU painter stays the oracle.
//
// Drawing order: the rows come grouped by palette layer (`gpu.rs`), and each source's layers
// are drawn in turn, so antialiased edges blend over the layers below them as the CPU painter
// blends them.

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
// Style flags (`gpu.rs`): a filled closed shape; snap by width parity. Every stroke has round
// caps and joins (kurbo's `Stroke::new`, what the CPU painter strokes with).
const FILL: u32 = 1u;
const PARITY: u32 = 2u;

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

// Corner (0|1, 0|1) of vertex `i` of a two-triangle quad.
fn corner(i: u32) -> vec2<f32> {
    let k = i % 6u;
    return vec2<f32>(f32(k == 1u || k == 3u || k == 4u), f32(k == 2u || k == 4u || k == 5u));
}

// RGBA8 bytes in memory order (R first) → straight-alpha colour.
fn color_of(c: u32) -> vec4<f32> {
    return vec4<f32>(f32(c & 0xffu), f32((c >> 8u) & 0xffu), f32((c >> 16u) & 0xffu), f32(c >> 24u)) / 255.0;
}

fn width_px(style: u32) -> f32 {
    return f32(style & 0xffffu) / 16.0 * dev();
}

// Dashes and gaps of this many device px each (0: solid): `2 · dash` logical px.
fn dash_px(style: u32) -> f32 {
    return f32((style >> 16u) & 0xfu) * 2.0 * dev();
}

fn flags(style: u32) -> u32 {
    return (style >> 20u) & 0xfu;
}

// The fragment's colour at `cov` coverage.
fn shade(color: vec4<f32>, cov: f32) -> vec4<f32> {
    return vec4<f32>(color.rgb, color.a * cov);
}

// The snap offset of a stroke: a pixel centre, or the pixel edge for a styled stroke of even
// rounded width (`canvas::styled`).
fn snap_offset(style: u32) -> f32 {
    let w = f32(style & 0xffffu) / 16.0;
    let parity = (flags(style) & PARITY) != 0u;
    let even = (i32(round(w)) % 2) == 0;
    return select(0.5, 0.0, parity && even);
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

fn coverage(d: f32) -> f32 {
    return clamp(0.5 - d, 0.0, 1.0);
}

// The area of the unit pixel square (centred on the fragment) on the side `n · (x - c) ≤ t`
// of a line with unit normal `n`: exact, piecewise quadratic in `t` (the CPU rasterizer's area
// coverage, where `coverage` above is the linear distance approximation).
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

// Signed distance to an axis-aligned box of half-size `h` centred at the origin.
fn sd_box(p: vec2<f32>, h: vec2<f32>) -> f32 {
    let q = abs(p) - h;
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0);
}

// The point of a stroke of length `len` nearest to position `s` along it: `s` clamped to the
// stroke, or with dashes and gaps of `d` px each from its start (`d > 0`; the last dash
// clipped at `len`, vello_cpu's dashing) the nearest point of the nearest dash.
fn dash_near(s: f32, len: f32, d: f32) -> f32 {
    let sc = clamp(s, 0.0, len);
    if d <= 0.0 {
        return sc;
    }
    let period = 2.0 * d;
    let k = floor(sc / period);
    var best = sc;
    var gap = 1e9;
    for (var j = -1.0; j <= 1.0; j += 1.0) {
        let s0 = (k + j) * period;
        if k + j < 0.0 || s0 > len {
            continue;
        }
        let h = clamp(sc, s0, min(s0 + d, len));
        if abs(h - s) < gap {
            gap = abs(h - s);
            best = h;
        }
    }
    return best;
}

fn degenerate() -> vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}

// Background and grid: one quad over the view (everything draws over it).

struct BgOut {
    @builtin(position) pos: vec4<f32>,
}

@vertex
fn vs_bg(@builtin(vertex_index) i: u32) -> BgOut {
    var o: BgOut;
    o.pos = to_clip(mix(cam.view.xy, cam.view.zw, corner(i)));
    return o;
}

// Grid dot `i` lights logical pixel `floor(i · pitch + phase + 1e-6)` (`canvas::draw_grid`):
// pixel `l` is lit when the first dot at or past it starts before `l + 1`.
@fragment
fn fs_bg(o: BgOut) -> @location(0) vec4<f32> {
    let pitch = cam.grid.x;
    if pitch > 0.0 {
        let l = floor(o.pos.xy / dev());
        let i = ceil((l - cam.grid.yz - SNAP_BIAS) / pitch);
        let at = i * pitch + cam.grid.yz + SNAP_BIAS;
        if all(at < l + 1.0) {
            return cam.grid_color;
        }
    }
    return cam.background;
}

// Segments: lines, wires, polyline and polygon edges, line marks.

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

// Rectangles: an outline snapped like a stroke, a fill over the raw box.

struct RectOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) p: vec2<f32>,
    @location(1) @interpolate(flat) color: vec4<f32>,
    @location(2) @interpolate(flat) outline: vec4<f32>,
    @location(3) @interpolate(flat) fill: vec4<f32>,
    @location(4) @interpolate(flat) par: vec4<f32>,
}

@vertex
fn vs_rect(@builtin(vertex_index) i: u32, in: SegIn) -> RectOut {
    var o: RectOut;
    o.color = color_of(in.color);
    let fl = flags(in.style);
    let ra = to_screen(in.a);
    let rb = to_screen(in.b);
    let raw = vec4<f32>(min(ra, rb), max(ra, rb));
    let c = corner(i);
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
    let sn = vec4<f32>(snap(raw.x, so), snap(raw.y, so), snap(raw.z, so), snap(raw.w, so));
    let hw = width_px(in.style) * 0.5;
    let pad = hw + 1.0;
    let lo = min(sn.xy, raw.xy) - pad;
    let hi = max(sn.zw, raw.zw) + pad;
    let p = mix(lo, hi, c);
    o.pos = to_clip(p);
    o.p = p;
    o.outline = sn;
    o.fill = raw;
    o.par = vec4<f32>(hw, dash_px(in.style), f32(fl), 0.0);
    return o;
}

@fragment
fn fs_rect(o: RectOut) -> @location(0) vec4<f32> {
    if o.par.w > 0.0 {
        return o.color;
    }
    let hw = o.par.x;
    let fl = u32(o.par.z);
    let c = 0.5 * (o.outline.xy + o.outline.zw);
    let h = 0.5 * (o.outline.zw - o.outline.xy);
    let q = o.p - c;
    // Round joins: the distance to the outline.
    var d = abs(sd_box(q, h)) - hw;
    if o.par.y > 0.0 {
        // Round-capped dashes along the perimeter from the top-left corner, clockwise
        // (`push_rect`'s path): the distance to the nearest point of the nearest dash.
        let w = 2.0 * h.x;
        let hh = 2.0 * h.y;
        let r = o.p - o.outline.xy;
        let dl = vec4<f32>(abs(r.y), abs(r.x - w), abs(r.y - hh), abs(r.x));
        var s = r.x;
        var m = dl.x;
        if dl.y < m {
            m = dl.y;
            s = w + r.y;
        }
        if dl.z < m {
            m = dl.z;
            s = w + hh + (w - r.x);
        }
        if dl.w < m {
            s = 2.0 * w + hh + (hh - r.y);
        }
        let at = perimeter(dash_near(s, 2.0 * (w + hh), o.par.y), w, hh);
        d = length(r - at) - hw;
    }
    var cov = coverage(d);
    if (fl & FILL) != 0u {
        let fc = 0.5 * (o.fill.xy + o.fill.zw);
        let fh = 0.5 * (o.fill.zw - o.fill.xy);
        cov = max(cov, coverage(sd_box(o.p - fc, fh)));
    }
    return shade(o.color, cov);
}

// The point at `s` along a `w × h` rectangle's outline from its top-left corner, clockwise.
fn perimeter(s: f32, w: f32, h: f32) -> vec2<f32> {
    if s <= w {
        return vec2<f32>(s, 0.0);
    }
    if s <= w + h {
        return vec2<f32>(w, s - w);
    }
    if s <= 2.0 * w + h {
        return vec2<f32>(w - (s - w - h), h);
    }
    return vec2<f32>(0.0, h - (s - 2.0 * w - h));
}

// Arcs and circles: a ring with an angle mask, unsnapped like the CPU's cubics.

struct ArcIn {
    @location(0) c: vec2<i32>,
    @location(1) r: i32,
    @location(2) start: i32,
    @location(3) sweep: i32,
    @location(4) color: u32,
    @location(5) style: u32,
}

struct ArcOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) p: vec2<f32>,
    @location(1) @interpolate(flat) color: vec4<f32>,
    // centre x, centre y, radius, half width (device px)
    @location(2) @interpolate(flat) geo: vec4<f32>,
    // start (degrees, clockwise on screen), sweep (degrees), dash, flags
    @location(3) @interpolate(flat) ang: vec4<f32>,
    // the far-zoom pixel box (1) or the arc (0)
    @location(4) @interpolate(flat) solid: f32,
}

@vertex
fn vs_arc(@builtin(vertex_index) i: u32, in: ArcIn) -> ArcOut {
    var o: ArcOut;
    o.color = color_of(in.color);
    o.solid = 0.0;
    let fl = flags(in.style);
    let c = to_screen(in.c);
    let r = f32(in.r) * cam.scale_dev.x;
    let raw = vec4<f32>(c - r, c + r);
    let k = corner(i);
    if o.color.a == 0.0 {
        o.pos = degenerate();
        return o;
    }
    if lod_small(raw) {
        let q = lod_box(raw);
        let p = mix(q.xy, q.zw, k);
        o.pos = to_clip(p);
        o.p = p;
        o.solid = 1.0;
        return o;
    }
    let hw = width_px(in.style) * 0.5;
    let pad = r + hw + 1.0;
    let p = c + mix(vec2<f32>(-pad), vec2<f32>(pad), k);
    o.pos = to_clip(p);
    o.p = p;
    o.geo = vec4<f32>(c, r, hw);
    o.ang = vec4<f32>(f32(in.start) / 1000.0, f32(in.sweep) / 1000.0, dash_px(in.style), f32(fl));
    return o;
}

// The distance from `q` (relative to the centre) to an arc of radius `r` and half width `hw`
// starting at `start` degrees and sweeping `sweep` (dashes `dash` px; 0: solid), and 1 when
// the nearest point lies in the stroke's span more than a pixel clear of its ends (else 0).
fn arc_distance(q: vec2<f32>, r: f32, hw: f32, start: f32, sweep: f32, dash: f32) -> vec2<f32> {
    let rp = length(q);
    let full = abs(sweep) >= 360.0;
    let dir = select(1.0, -1.0, sweep < 0.0);
    // Arc length from the start along the sweep's direction, in [0, 2πr).
    var rel = (degrees(atan2(q.y, q.x)) - start) * dir;
    rel = rel - floor(rel / 360.0) * 360.0;
    let circ = radians(360.0) * r;
    let s = radians(rel) * r;
    let len = select(radians(abs(sweep)) * r, circ, full);
    // The nearest point of the stroke (its dashes) along the arc, from this side of the start
    // or the other: a round cap, or a dash end, past the start reaches back across it.
    var d = 1e9;
    var span = 0.0;
    for (var i = 0; i < 2; i += 1) {
        let sh = select(s - circ, s, i == 0);
        let h = dash_near(sh, len, dash);
        var e: f32;
        var clear = false;
        if abs(h - sh) < 1e-4 {
            e = abs(rp - r) - hw;
            let lo = sh - 0.7;
            let hi = sh + 0.7;
            clear = full && dash <= 0.0
                || abs(dash_near(lo, len, dash) - lo) < 1e-4 && abs(dash_near(hi, len, dash) - hi) < 1e-4;
        } else {
            let a = radians(start + dir * degrees(h / max(r, 1e-6)));
            e = length(q - r * vec2<f32>(cos(a), sin(a))) - hw;
        }
        if e < d {
            d = e;
            span = f32(clear);
        }
    }
    return vec2<f32>(d, span);
}

@fragment
fn fs_arc(o: ArcOut) -> @location(0) vec4<f32> {
    if o.solid > 0.0 {
        return o.color;
    }
    let q = o.p - o.geo.xy;
    let r = o.geo.z;
    let hw = o.geo.w;
    let ds = arc_distance(q, r, hw, o.ang.x, o.ang.y, o.ang.z);
    let rp = length(q);
    let n = q / max(rp, 1e-6);
    var cov: f32;
    if ds.x >= 1.0 {
        cov = 0.0;
    } else if ds.y > 0.0 {
        // Across the ring clear of its ends: the exact area of the band at the local normal.
        let off = rp - r;
        cov = half_plane_area(n, hw - off) - half_plane_area(n, -hw - off);
    } else {
        // Near a cap or a dash end: the share of 4 × 4 subsamples of the pixel on the stroke.
        var inside = 0.0;
        for (var j = 0; j < 4; j += 1) {
            for (var i = 0; i < 4; i += 1) {
                let qs = q + (vec2<f32>(f32(i), f32(j)) - 1.5) * 0.25;
                inside += f32(arc_distance(qs, r, hw, o.ang.x, o.ang.y, o.ang.z).x <= 0.0);
            }
        }
        cov = inside / 16.0;
    }
    if abs(o.ang.y) >= 360.0 && (u32(o.ang.w) & FILL) != 0u {
        cov = max(cov, half_plane_area(n, r - rp));
    }
    return shade(o.color, cov);
}

// Polygon fills: plain triangles from the pack's ear clipping; the outline over them is
// antialiased, so the fill's edge needs no feather.

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
