// A canvas row's style word (`gpu::style_word`): stroke width in 1/16 px (bits 0–15), dash
// (bits 16–19, `LineStyle::dash`), flags (bits 20–23, `gpu::FILL`, `gpu::PARITY`; the CPU-only
// `gpu::FIRST` and `gpu::CLOSED` are ignored here), and the colour a fragment is shaded with.

// Style flags (`gpu.rs`): a filled closed shape; snap by width parity. Every stroke has round
// caps and joins (kurbo's `Stroke::new`, what the CPU painter strokes with).
const FILL: u32 = 1u;

const PARITY: u32 = 2u;

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

// The snap offset of a stroke: a pixel centre, or the pixel edge for a styled stroke of even
// rounded width (`canvas::styled`).
fn snap_offset(style: u32) -> f32 {
    let w = f32(style & 0xffffu) / 16.0;
    let parity = (flags(style) & PARITY) != 0u;
    let even = (i32(round(w)) % 2) == 0;
    return select(0.5, 0.0, parity && even);
}

// The fragment's colour at `cov` coverage.
fn shade(color: vec4<f32>, cov: f32) -> vec4<f32> {
    return vec4<f32>(color.rgb, color.a * cov);
}
