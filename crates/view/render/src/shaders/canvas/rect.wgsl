// Rectangles (`gpu::RECT` rows, 24 B, `SegIn`'s layout: two opposite corners): an outline
// snapped like a stroke, dashed clockwise from the top-left corner as `push_rect` draws it, and
// a fill over the raw box.

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
