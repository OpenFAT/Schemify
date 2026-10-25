// The canvas background and grid (`batch::Stage::Background`): one quad over the view, drawn
// before every row.

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
