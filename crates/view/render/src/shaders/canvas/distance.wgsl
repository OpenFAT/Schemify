// Distances the canvas fragment shaders turn into coverage: the 1 px distance ramp, a box's
// signed distance, and the nearest point of a dashed stroke.

fn coverage(d: f32) -> f32 {
    return clamp(0.5 - d, 0.0, 1.0);
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
