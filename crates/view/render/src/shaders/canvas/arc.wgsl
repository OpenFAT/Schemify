// Arcs and circles (`gpu::ARC` rows, 28 B: centre, radius, start and sweep in millidegrees,
// colour, style): a ring with an angle mask, unsnapped like the CPU's cubics; a full circle
// may be filled.

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
            // Parenthesized: WGSL forbids mixing `&&` and `||` (naga accepts it, Tint does not).
            clear = (full && dash <= 0.0)
                || (abs(dash_near(lo, len, dash) - lo) < 1e-4 && abs(dash_near(hi, len, dash) - hi) < 1e-4);
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
