// How an instance becomes a quad and a colour, shared by both shader modules (the canvas's
// `CANVAS_WGSL` and the batch's `UI_WGSL` in `gpu.rs`): every pipeline draws six vertices per
// instance, and every instance carries its colour as one `u32`.

// Corner (0|1, 0|1) of vertex `i` of a two-triangle quad.
fn corner(i: u32) -> vec2<f32> {
    let k = i % 6u;
    return vec2<f32>(f32(k == 1u || k == 3u || k == 4u), f32(k == 2u || k == 4u || k == 5u));
}

// RGBA8 bytes in memory order (R first) → straight-alpha colour.
fn color_of(c: u32) -> vec4<f32> {
    return vec4<f32>(f32(c & 0xffu), f32((c >> 8u) & 0xffu), f32((c >> 16u) & 0xffu), f32(c >> 24u)) / 255.0;
}
