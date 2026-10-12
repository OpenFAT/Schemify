//! The canvas raster cache as world-anchored tiles (D39): a pan copies the tiles it already
//! has and records only the ones it uncovers; a zoom shows the old tiles stretched until the
//! new ones are recorded, a few per frame; an edit redraws only the changed parts of the
//! tiles it touches.
//!
//! Six questions:
//! 1. In: a camera, a content key with its damage, and the content's `draw`. Out: the canvas
//!    in the frame, drawn from [`TILE_PX`]-square tiles of one atlas layer.
//! 2. A view needs ~10–150 tiles; the atlas holds twice that (the current zoom's tiles and the
//!    previous zoom's, kept as stand-ins), at most [`CELLS_MAX`].
//! 3. Tile indices `i32` (device px / 256 of world coordinates, ±2^31 px covers any zoom of
//!    the i32 world); cell indices `usize` < 1024; zoom and phase `f64` (the camera's).
//! 4. Every frame walks the visible tiles (and the atlas cells, for stand-ins); recording a
//!    tile calls `draw` once for that tile.
//! 5. The atlas lives in the painter (one layer slot); cells live until evicted, least
//!    recently used first, or until the content they show changes.
//! 6. Tiles are independent (parallel recording would need a painter per thread; the
//!    per-frame budget keeps them serial and cheap).

use std::sync::atomic::{AtomicU64, Ordering};

use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

use crate::canvas::Camera;
use crate::painter::Painter;

/// Tile edge in device px (at an integral device scale `s`, the largest multiple of `s` up to
/// it, so tile corners sit on whole logical px and pixel snapping agrees across tiles).
pub const TILE_PX: u32 = 256;
/// Tiles (or tile parts) one frame records at most by default (see
/// [`CanvasCache::tiles_per_frame`]).
pub const TILES_PER_FRAME: usize = 16;
/// Cost (what `draw` reports, see [`draw_cached`]) one frame spends on recording by default
/// (see [`CanvasCache::cost_per_frame`]): ~1–1.5 ms in [`draw_prims_in`]'s units.
///
/// [`draw_prims_in`]: crate::canvas::draw_prims_in
pub const COST_PER_FRAME: usize = 80_000;
/// Cost a tile part counts on top of its rows (its own render pass and atlas copy).
const TILE_COST: usize = 2_000;
/// How much more a tile's world may cost at a finer zoom than a coarser cell over it says:
/// rows drawn as pixels there may become paths (see `PATH_COST` in the canvas).
const ZOOM_IN_COST: f64 = 4.0;
/// Most parts across a tile a costly tile is recorded in, one budget step each (16 parts).
const SPLIT_MAX: u32 = 4;
/// Most atlas cells (a 32 × 32 grid of tiles: 8192 px square).
pub const CELLS_MAX: usize = 1024;
/// Logical px a damage box is widened by: half the widest stroke, mark strokes, antialiasing
/// and pixel snapping. Content must not draw farther than this outside its rows' boxes.
pub const PATCH_PAD_PX: f64 = 8.0;
/// Patches are whole multiples of this many logical px from their tile's corner (so of 16
/// device px at integral scales): vello's tiles, so every pixel outside the patches sees
/// exactly the path segments it saw when recorded, and pixel snapping lands alike inside.
const PATCH_GRID: f64 = 16.0;
/// Most patches per tile and frame (more are merged).
const PATCHES_MAX: usize = 4;
/// Most damaged parts of one tile merged down to [`PATCHES_MAX`] (the merge is quadratic).
const PARTS_MAX: usize = 64;
/// Most damage boxes kept between frames before the change counts as unknown.
const DAMAGE_BOXES_MAX: usize = 4096;

/// Unique layer tags across every cache in the process.
static STAMPS: AtomicU64 = AtomicU64::new(1);

/// What one atlas cell holds: the tile `tile` of the picture at zoom `scale` and device phase
/// `phase` (the fractional device position of world (0, 0)), showing content `key`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Cell {
    /// Camera zoom; 0 = empty.
    scale: f64,
    phase: (f64, f64),
    tile: (i32, i32),
    /// Content key; 0 = stale.
    key: u64,
    /// Frame it was last drawn or recorded.
    used: u64,
    /// Parts across (and down) the tile is recorded in, and the parts recorded so far (bit
    /// `y · split + x`): the picture is whole once every part is.
    split: u8,
    done: u16,
    /// Cost estimate when given out, then the cost of the parts recorded (see
    /// [`CanvasCache::estimate`]).
    est: u32,
    cost: u32,
}

impl Cell {
    /// Whether every part is recorded.
    fn whole(&self) -> bool {
        let n = u32::from(self.split).pow(2);
        n > 0 && u32::from(self.done) == (1 << n) - 1
    }
}

/// The raster cache of one canvas: which painter layer holds its tile atlas and what each
/// tile shows. One per canvas; the pixels live in the painter (see [`Painter::layer`]).
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasCache {
    /// Painter layer slot (`< painter::LAYER_SLOTS`), distinct per canvas sharing a painter.
    pub slot: usize,
    /// Tiles (or parts of one) a frame records at most; the rest show stand-ins and the frame
    /// reports [`Drawn::Partial`] (draw again). `usize::MAX` (with
    /// [`CanvasCache::cost_per_frame`]) records every visible tile at once.
    pub tiles_per_frame: usize,
    /// Cost a frame records at most: it records nothing further once the cost spent plus the
    /// estimate of the next part would pass it (the first part always records). A tile whose
    /// estimate passes it is recorded in parts, up to [`SPLIT_MAX`]² of them.
    pub cost_per_frame: usize,
    /// The last tile part's cost times the parts, at zoom `last_scale`: the estimate of a
    /// tile no recorded cell overlaps (see [`CanvasCache::estimate`]).
    last_cost: usize,
    last_scale: f64,
    /// Tag of the atlas layer; 0 = none.
    stamp: u64,
    /// Atlas cells across and down, the device scale the atlas was made for, and its tile
    /// edge in device px.
    grid: (usize, usize),
    device: f64,
    edge: u32,
    cells: Vec<Cell>,
    /// Content key the current cells show.
    key: u64,
    /// Key of the previous call (what its `damage` was relative to).
    last: u64,
    /// World boxes where the content of `last` differs from `key`'s; `None` = unknown.
    pending: Option<Vec<[i32; 4]>>,
    frame: u64,
}

impl Default for CanvasCache {
    fn default() -> CanvasCache {
        CanvasCache {
            slot: 0,
            tiles_per_frame: TILES_PER_FRAME,
            cost_per_frame: COST_PER_FRAME,
            last_cost: 0,
            last_scale: 0.0,
            stamp: 0,
            grid: (0, 0),
            device: 0.0,
            edge: TILE_PX,
            cells: Vec::new(),
            key: 0,
            last: 0,
            pending: None,
            frame: 0,
        }
    }
}

impl CanvasCache {
    /// The cost of recording tile `tile` of grid `g`: the densest whole cell over the same
    /// world (any zoom) scaled to the tile's world area, times [`ZOOM_IN_COST`] when that cell
    /// is coarser (rows it drew as pixels become paths); else the last tile's cost times the
    /// zoom-out factor squared (clamped to 64×, as content is not uniform).
    ///
    /// Cost: O(cells).
    fn estimate(&self, tile: (i32, i32), g: &Grid) -> usize {
        assert!(g.scale > 0.0 && self.device > 0.0);
        let (s, t) = (self.device, f64::from(self.edge));
        // A cell's world box: device x = w·scale·s + phase - i·T within its tile.
        let span = |scale: f64, phase: (f64, f64), tile: (i32, i32)| {
            let (x, y) = (
                (f64::from(tile.0) * t - phase.0) / (scale * s),
                (f64::from(tile.1) * t - phase.1) / (scale * s),
            );
            let e = t / (scale * s);
            Rect::new(x, y, x + e, y + e)
        };
        let want = span(g.scale, g.phase, tile);
        let mut best: Option<f64> = None;
        for c in self.cells.iter().filter(|c| c.scale > 0.0 && c.whole()) {
            let b = span(c.scale, c.phase, c.tile);
            if b.intersect(want).area() > 0.0 {
                let mut d = f64::from(c.cost) / b.area();
                if c.scale < g.scale {
                    d *= ZOOM_IN_COST;
                }
                best = Some(best.map_or(d, |m: f64| m.max(d)));
            }
        }
        if let Some(d) = best {
            return (d * want.area()) as usize;
        }
        if self.last_scale <= 0.0 {
            return self.last_cost;
        }
        let area = (self.last_scale / g.scale).powi(2).clamp(1.0 / 64.0, 64.0);
        (self.last_cost as f64 * area) as usize
    }
}

/// How [`draw_cached`] drew a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drawn {
    /// Every visible tile copied pixel for pixel from the atlas (a pan, or nothing moved).
    Copied,
    /// The old zoom's tiles stretched to a new zoom (a stand-in until the next crisp frame).
    Stretched,
    /// The changed parts of visible tiles redrawn (see `damage` of [`draw_cached`]), every
    /// tile shown.
    Patched,
    /// Tiles recorded this frame; every visible tile shown.
    Recorded,
    /// Some visible tiles not recorded yet (stand-ins shown in their place): draw again.
    Partial,
    /// Content drawn straight into the frame (the painter keeps no layers).
    Direct,
}

/// Device placement of the current zoom's tile grid: world (0, 0) at `int + phase` device px.
#[derive(Clone, Copy)]
struct Grid {
    scale: f64,
    phase: (f64, f64),
    int: (f64, f64),
}

/// Draws a canvas through its tile cache: `draw(p, view, cam)` paints the content of a view
/// (it must fill `view` opaquely with `background` first), returns its cost (e.g. the rows it
/// looked at, as [`draw_prims_in`] does) and runs once per tile or part the cache records. Tiles are anchored to the world at the camera's zoom: a pan by whole pixels
/// copies the tiles it has and records the uncovered ones; a zoom with `crisp == false`
/// stretches the old tiles (pixels none covers show `background`); a crisp zoom records the
/// new tiles while the frame's budget lasts ([`CanvasCache::tiles_per_frame`],
/// [`CanvasCache::cost_per_frame`]; nearest the view's top-left first), showing the old ones
/// until then; new content whose change is known redraws only
/// the changed parts of the visible tiles. Content outside the cache (gesture previews) is
/// drawn by the caller afterwards.
///
/// `key` must change whenever anything `draw` reads changes, except the camera. `damage`
/// says where the content for `key` differs from the content of the previous call's key:
/// world boxes (each row added or removed, e.g. from [`CanvasDiff::update`]; `draw` must
/// paint nothing farther than [`PATCH_PAD_PX`] outside them), empty when the key is the
/// same, or `None` when unknown.
///
/// Cost: one layer draw per visible tile; `draw` per recorded tile (within the budget, the
/// view's tiles first, then the 1-tile ring around it) and per changed part of a visible tile.
///
/// [`CanvasDiff::update`]: crate::canvas::CanvasDiff::update
/// [`draw_prims_in`]: crate::canvas::draw_prims_in
#[allow(clippy::too_many_arguments)]
pub fn draw_cached<P: Painter>(
    p: &mut P,
    cache: &mut CanvasCache,
    view: Rect,
    cam: &Camera,
    key: u64,
    damage: Option<&[[i32; 4]]>,
    crisp: bool,
    background: Color,
    mut draw: impl FnMut(&mut P, Rect, &Camera) -> usize,
) -> Drawn {
    assert!(view.width() >= 0.0 && view.height() >= 0.0 && cam.scale > 0.0);
    assert!(
        cache.tiles_per_frame > 0 && cache.cost_per_frame > 0,
        "a frame records at least one tile"
    );
    cache.frame += 1;
    // What changed since the cells were recorded: this call's damage on top of the earlier.
    if key != cache.last {
        cache.pending = match (cache.pending.take(), damage) {
            (Some(mut boxes), Some(d)) if boxes.len() + d.len() <= DAMAGE_BOXES_MAX => {
                boxes.extend_from_slice(d);
                Some(boxes)
            }
            _ => None,
        };
        cache.last = key;
    }
    if view.area() <= 0.0 {
        return Drawn::Copied;
    }
    let s = p.device_scale();
    let edge = tile_edge(s);
    let t = f64::from(edge);
    // The atlas: twice the cells the view's tiles (and a ring) need, made once per size.
    let across = ((view.width() * s / t).ceil() as usize + 3).max(1);
    let down = ((view.height() * s / t).ceil() as usize + 3).max(1);
    let want = (2 * across * down).min(CELLS_MAX);
    let cols = (want as f64).sqrt().ceil() as usize;
    let grid = (cols, want.div_ceil(cols));
    let held = cache.stamp != 0
        && cache.device == s
        && cache.grid == grid
        && p.holds_layer(cache.slot, cache.stamp);
    if !held {
        let stamp = STAMPS.fetch_add(1, Ordering::Relaxed);
        let (w, h) = ((grid.0 as u32) * edge, (grid.1 as u32) * edge);
        if !p.layer(cache.slot, stamp, w, h, |_| {}) {
            cache.stamp = 0;
            p.push_clip(view);
            draw(p, view, cam);
            p.pop_clip();
            return Drawn::Direct;
        }
        cache.stamp = stamp;
        cache.device = s;
        cache.edge = edge;
        cache.grid = grid;
        cache.cells = vec![Cell::default(); grid.0 * grid.1];
        cache.key = key;
        cache.pending = Some(Vec::new());
    }
    // The current zoom's grid: world (0, 0) at device `origin * s`, split into a whole part
    // (where tiles land: whole logical px at an integral scale, so snapping agrees) and a
    // phase (what they were recorded with).
    let (ox, oy) = cam.origin(view);
    let (dx, dy) = (ox * s, oy * s);
    // ponytail: at a fractional device scale tiles land on whole device px only, so labels and
    // snapped strokes may sit a device px apart across a tile seam.
    let unit = if s.fract() == 0.0 { s } else { 1.0 };
    let whole = |d: f64| ((d + 1e-6) / unit).floor() * unit;
    let int = (whole(dx), whole(dy));
    let g = Grid {
        scale: cam.scale,
        phase: (dx - int.0, dy - int.1),
        int,
    };
    let same = |c: &Cell| {
        c.scale == g.scale
            && (c.phase.0 - g.phase.0).abs() < 1e-6
            && (c.phase.1 - g.phase.1).abs() < 1e-6
    };
    // New content: cells away from the damage keep showing it; damaged ones need patches.
    let mut dirty: Vec<usize> = Vec::new();
    let mut boxes: Vec<[i32; 4]> = Vec::new();
    if key != cache.key {
        // Unknown damage: every cell keeps its old picture (a stand-in) until re-recorded.
        let known = cache.pending.take();
        for (k, c) in cache.cells.iter_mut().enumerate() {
            let Some(b) = known.as_deref() else { break };
            if c.scale == 0.0 || c.key != cache.key {
                continue;
            }
            if b.iter().any(|b| touches(c, *b, t, s)) {
                dirty.push(k);
            } else {
                c.key = key;
            }
        }
        boxes = known.unwrap_or_default();
        cache.key = key;
        cache.pending = Some(Vec::new());
    }
    // Tiles meeting the view, in device px of the tile grid.
    let v = Rect::new(view.x0 * s, view.y0 * s, view.x1 * s, view.y1 * s);
    let range = |a: f64, b: f64, o: f64| {
        let lo = ((a - o) / t).floor() as i32;
        let hi = ((b - o - 1e-9) / t).floor() as i32;
        (lo, hi.max(lo))
    };
    let (i0, i1) = range(v.x0, v.x1, g.int.0);
    let (j0, j1) = range(v.y0, v.y1, g.int.1);
    let find = |cells: &[Cell], tile: (i32, i32)| {
        cells
            .iter()
            .position(|c| c.key == key && c.tile == tile && same(c) && c.whole())
    };
    let building = |cells: &[Cell], tile: (i32, i32)| {
        cells
            .iter()
            .position(|c| c.key == key && c.tile == tile && same(c) && !c.whole())
    };
    let mut patched = false;
    for k in dirty {
        let c = cache.cells[k];
        let visible =
            same(&c) && (i0..=i1).contains(&c.tile.0) && (j0..=j1).contains(&c.tile.1) && crisp;
        if visible && patch_tile(p, cache, k, &g, &boxes, &mut draw) {
            cache.cells[k].key = key;
            cache.cells[k].used = cache.frame;
            patched = true;
        }
    }
    // Visible tiles whose picture is out of date (the content changed where no damage said)
    // are recorded again at once, whatever the budget: content is shown right; only tiles a
    // camera move uncovers wait their turn.
    for j in j0..=j1 {
        for i in i0..=i1 {
            let stale = cache
                .cells
                .iter()
                .position(|c| c.tile == (i, j) && same(c) && c.key != key && c.scale != 0.0);
            if let (Some(k), true, None) = (stale, crisp, find(&cache.cells, (i, j))) {
                claim(cache, k, (i, j), &g, key);
                while !cache.cells[k].whole() {
                    record_part(p, cache, k, &g, &mut draw);
                }
                patched = true;
            }
        }
    }
    // Record the missing visible tiles while the budget lasts (all of them on a fresh atlas: a
    // first frame shows everything), then the ring around the view (a pan finds its tiles
    // ready). Recording comes before any drawing: the CPU painter's atlas is shared with the
    // frame once drawn from, and a later write would copy it.
    let (mut budget, mut left) = match (held, crisp) {
        (false, _) => (usize::MAX, usize::MAX),
        (true, false) => (0, 0),
        (true, true) => (cache.tiles_per_frame, cache.cost_per_frame),
    };
    let (mut recorded, mut any) = (false, false);
    let ring = |j: i32, i: i32| !((i0..=i1).contains(&i) && (j0..=j1).contains(&j));
    // The visible tiles at hand are this frame's: no recording evicts them.
    for j in j0..=j1 {
        for i in i0..=i1 {
            if let Some(k) = find(&cache.cells, (i, j)) {
                cache.cells[k].used = cache.frame;
            }
        }
    }
    for pass in [false, true] {
        if pass && !held {
            break; // a fresh atlas records what the view shows, nothing ahead of it
        }
        for j in j0 - 1..=j1 + 1 {
            for i in i0 - 1..=i1 + 1 {
                if budget == 0 || ring(j, i) != pass || find(&cache.cells, (i, j)).is_some() {
                    continue;
                }
                // The tile's cell under way, else a new one.
                let k = building(&cache.cells, (i, j)).unwrap_or_else(|| {
                    let k = lru(cache);
                    claim(cache, k, (i, j), &g, key);
                    k
                });
                cache.cells[k].used = cache.frame;
                let c = cache.cells[k];
                let part = c.est as usize / usize::from(c.split).pow(2);
                while budget > 0 && !cache.cells[k].whole() {
                    if any && left < part {
                        budget = 0; // the next part would pass the budget: next frame
                        break;
                    }
                    left = left.saturating_sub(record_part(p, cache, k, &g, &mut draw));
                    (budget, any) = (budget - 1, true);
                }
                // The ring's tiles are not this frame's picture.
                recorded |= !pass && cache.cells[k].whole();
            }
        }
    }
    // Stand-ins under the missing tiles first (over their bounding box, so each stand-in cell
    // is one draw), then every tile at hand on top.
    let mut gap: Option<Rect> = None;
    for j in j0..=j1 {
        for i in i0..=i1 {
            if find(&cache.cells, (i, j)).is_none() {
                let (x, y) = (f64::from(i) * t + g.int.0, f64::from(j) * t + g.int.1);
                let r = Rect::new(x / s, y / s, (x + t) / s, (y + t) / s).intersect(view);
                gap = Some(gap.map_or(r, |b| b.union(r)));
            }
        }
    }
    p.push_clip(view);
    let stretched = gap.is_some_and(|gap| stand_ins(p, cache, gap, &g, key, background));
    for j in j0..=j1 {
        for i in i0..=i1 {
            if let Some(k) = find(&cache.cells, (i, j)) {
                show(p, cache, k, &g, view, t, s);
            }
        }
    }
    p.pop_clip();
    assert!(cache.cells.len() == grid.0 * grid.1);
    match (gap.is_none(), crisp) {
        (false, true) => Drawn::Partial,
        (false, false) => {
            if stretched {
                Drawn::Stretched
            } else {
                Drawn::Partial
            }
        }
        _ if recorded => Drawn::Recorded,
        _ if patched => Drawn::Patched,
        _ => Drawn::Copied,
    }
}

/// Atlas device px of cell `k`'s corner.
fn corner(cache: &CanvasCache, k: usize) -> (u32, u32) {
    let (x, y) = (k % cache.grid.0, k / cache.grid.0);
    ((x as u32) * cache.edge, (y as u32) * cache.edge)
}

/// The tile edge at device scale `s`: see [`TILE_PX`].
fn tile_edge(s: f64) -> u32 {
    if s.fract() == 0.0 && s >= 1.0 {
        ((f64::from(TILE_PX) / s).floor() * s) as u32
    } else {
        TILE_PX
    }
}

/// The camera that draws tile `tile` of grid `g` into a `w × h` logical-px view whose corner
/// sits `(px, py)` device px into the tile.
fn tile_camera(
    g: &Grid,
    tile: (i32, i32),
    (px, py): (f64, f64),
    local: Rect,
    (s, t): (f64, f64),
) -> Camera {
    // World (0, 0) in the view's logical px.
    let ox = (g.phase.0 - f64::from(tile.0) * t - px) / s;
    let oy = (g.phase.1 - f64::from(tile.1) * t - py) / s;
    Camera {
        cx: (local.center().x - ox) / g.scale,
        cy: (local.center().y - oy) / g.scale,
        scale: g.scale,
    }
}

/// A free cell, else the least recently used one not drawn this frame (out-of-date pictures
/// first).
fn lru(cache: &CanvasCache) -> usize {
    let (frame, current) = (cache.frame, cache.key);
    cache
        .cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.used != frame)
        .min_by_key(|(_, c)| (c.scale != 0.0, c.key == current, c.used))
        .map(|(k, _)| k)
        .expect("the atlas holds more cells than a frame uses")
}

/// Gives cell `k` to tile `tile` of grid `g` with no part recorded, split into as many parts
/// as its estimated cost asks for (each part a whole number of [`PATCH_GRID`] cells, as a
/// patch is: a part then draws exactly what a whole tile would there).
fn claim(cache: &mut CanvasCache, k: usize, tile: (i32, i32), g: &Grid, key: u64) {
    let (s, edge) = (cache.device, cache.edge);
    let fits = |n: u32| {
        let part = f64::from(edge / n) / s;
        edge % n == 0 && (PATCH_GRID * s).fract() == 0.0 && part % PATCH_GRID == 0.0
    };
    let mut n = 1u32;
    let estimate = cache.estimate(tile, g);
    while n < SPLIT_MAX && estimate > cache.cost_per_frame * (n * n) as usize && fits(2 * n) {
        n *= 2;
    }
    cache.cells[k] = Cell {
        scale: g.scale,
        phase: g.phase,
        tile,
        key,
        used: cache.frame,
        split: n as u8,
        done: 0,
        est: estimate.min(u32::MAX as usize) as u32,
        cost: 0,
    };
    assert!(!cache.cells[k].whole() && n <= SPLIT_MAX);
}

/// Records the next part of cell `k` (given out by [`claim`]); returns its cost.
fn record_part<P: Painter>(
    p: &mut P,
    cache: &mut CanvasCache,
    k: usize,
    g: &Grid,
    draw: &mut impl FnMut(&mut P, Rect, &Camera) -> usize,
) -> usize {
    let c = cache.cells[k];
    assert!(!c.whole(), "a part left to record");
    let n = u32::from(c.split);
    let q = (!c.done).trailing_zeros();
    let w = cache.edge / n;
    let cost = draw_part(p, cache, k, g, ((q % n) * w, (q / n) * w, w, w), draw) + TILE_COST;
    let cell = &mut cache.cells[k];
    cell.done |= 1 << q;
    cell.cost = cell.cost.saturating_add(cost.min(u32::MAX as usize) as u32);
    (cache.last_cost, cache.last_scale) = (cost * (n * n) as usize, c.scale);
    cost
}

/// Draws the part `(x, y, w, h)` (device px from the tile's corner) of cell `k`'s tile, as
/// grid `g` places it, into the atlas; returns `draw`'s cost.
fn draw_part<P: Painter>(
    p: &mut P,
    cache: &CanvasCache,
    k: usize,
    g: &Grid,
    (x, y, w, h): (u32, u32, u32, u32),
    draw: &mut impl FnMut(&mut P, Rect, &Camera) -> usize,
) -> usize {
    assert!(
        x + w <= cache.edge && y + h <= cache.edge,
        "inside the tile"
    );
    let (s, t) = (cache.device, f64::from(cache.edge));
    let local = Rect::new(0.0, 0.0, f64::from(w) / s, f64::from(h) / s);
    let cam = tile_camera(
        g,
        cache.cells[k].tile,
        (f64::from(x), f64::from(y)),
        local,
        (s, t),
    );
    let (cx, cy) = corner(cache, k);
    let mut cost = 0;
    let ok = p.patch_layer(cache.slot, cache.stamp, cx + x, cy + y, w, h, |p| {
        cost = draw(p, local, &cam);
    });
    assert!(ok, "the painter holds the atlas");
    cost
}

/// Draws cell `k` (of the current grid) at its place: a pixel copy.
fn show<P: Painter>(
    p: &mut P,
    cache: &mut CanvasCache,
    k: usize,
    g: &Grid,
    view: Rect,
    t: f64,
    s: f64,
) {
    let c = cache.cells[k];
    let (cx, cy) = corner(cache, k);
    let (x, y) = (
        f64::from(c.tile.0) * t + g.int.0,
        f64::from(c.tile.1) * t + g.int.1,
    );
    let at = Affine::translate(((x - f64::from(cx)) / s, (y - f64::from(cy)) / s));
    let clip = Rect::new(x / s, y / s, (x + t) / s, (y + t) / s).intersect(view);
    if clip.area() > 0.0 {
        let shown = p.draw_layer(cache.slot, cache.stamp, at, clip, None);
        assert!(shown, "the painter holds the atlas");
    }
    cache.cells[k].used = cache.frame;
}

/// Fills `gap` (the box around the missing tiles of grid `g`) with `background`, then draws
/// over it every cell not current at `g` (another zoom or phase, or an older picture),
/// stretched to where its world lies now; the tiles at hand are drawn over it afterwards.
/// Returns whether any cell did.
fn stand_ins<P: Painter>(
    p: &mut P,
    cache: &mut CanvasCache,
    gap: Rect,
    g: &Grid,
    key: u64,
    background: Color,
) -> bool {
    let (s, t) = (cache.device, f64::from(cache.edge));
    // Background first, as a layer draw that samples nothing (the atlas placed far away): a
    // stand-in frame then stays one the window can compose without vello.
    let nowhere = Affine::translate((-1e9, -1e9));
    p.draw_layer(cache.slot, cache.stamp, nowhere, gap, Some(background));
    let mut any = false;
    let (o0, o1) = (g.int.0 + g.phase.0, g.int.1 + g.phase.1);
    for k in 0..cache.cells.len() {
        let c = cache.cells[k];
        let current = c.key == key
            && c.scale == g.scale
            && (c.phase.0 - g.phase.0).abs() < 1e-6
            && (c.phase.1 - g.phase.1).abs() < 1e-6;
        if c.scale == 0.0 || current || !c.whole() {
            continue;
        }
        // Atlas px q of the cell ↔ screen device X = (q - corner + i·T - φ0)·z + O.
        let z = g.scale / c.scale;
        let (cx, cy) = corner(cache, k);
        let (bx, by) = (
            f64::from(c.tile.0) * t - c.phase.0 - f64::from(cx),
            f64::from(c.tile.1) * t - c.phase.1 - f64::from(cy),
        );
        let at = Affine::translate((o0 / s, o1 / s))
            * Affine::scale(z)
            * Affine::translate((bx / s, by / s));
        let place = at.transform_rect_bbox(Rect::new(
            f64::from(cx) / s,
            f64::from(cy) / s,
            (f64::from(cx) + t) / s,
            (f64::from(cy) + t) / s,
        ));
        let clip = gap.intersect(place);
        if clip.area() > 0.0 {
            p.draw_layer(cache.slot, cache.stamp, at, clip, Some(background));
            cache.cells[k].used = cache.frame;
            any = true;
        }
    }
    any
}

/// Whether world box `b` (padded) reaches into cell `c`'s tile.
fn touches(c: &Cell, b: [i32; 4], t: f64, s: f64) -> bool {
    let pad = PATCH_PAD_PX * s;
    let d = |w: i32, ph: f64, i: i32| f64::from(w) * c.scale * s + ph - f64::from(i) * t;
    let (x0, x1) = (
        d(b[0], c.phase.0, c.tile.0) - pad,
        d(b[2], c.phase.0, c.tile.0) + pad,
    );
    let (y0, y1) = (
        d(b[1], c.phase.1, c.tile.1) - pad,
        d(b[3], c.phase.1, c.tile.1) + pad,
    );
    x1 > 0.0 && x0 < t && y1 > 0.0 && y0 < t
}

/// Redraws the parts of cell `k` that the damage `boxes` reach (or the whole tile when most of
/// it changed). False, drawing nothing, when its device scale cannot place patches on the
/// whole-pixel grid.
fn patch_tile<P: Painter>(
    p: &mut P,
    cache: &mut CanvasCache,
    k: usize,
    g: &Grid,
    boxes: &[[i32; 4]],
    draw: &mut impl FnMut(&mut P, Rect, &Camera) -> usize,
) -> bool {
    let s = cache.device;
    if (PATCH_GRID * s).fract() != 0.0 || s <= 0.0 {
        return false;
    }
    let (t, c) = (f64::from(cache.edge), cache.cells[k]);
    let (pad, step) = (PATCH_PAD_PX * s, PATCH_GRID * s);
    let d = |w: i32, ph: f64, i: i32| f64::from(w) * c.scale * s + ph - f64::from(i) * t;
    let snap = |v: f64, up: bool| {
        let q = v / step;
        (if up { q.ceil() } else { q.floor() } * step).clamp(0.0, t)
    };
    let mut parts: Vec<Rect> = Vec::new();
    for b in boxes {
        let part = Rect::new(
            snap(d(b[0], c.phase.0, c.tile.0) - pad, false),
            snap(d(b[1], c.phase.1, c.tile.1) - pad, false),
            snap(d(b[2], c.phase.0, c.tile.0) + pad, true),
            snap(d(b[3], c.phase.1, c.tile.1) + pad, true),
        );
        if part.width() > 0.0 && part.height() > 0.0 {
            parts.push(part);
        }
        if parts.len() > PARTS_MAX {
            parts = vec![Rect::new(0.0, 0.0, t, t)];
            break;
        }
    }
    merge(&mut parts);
    if parts.iter().map(Rect::area).sum::<f64>() > t * t / 2.0 {
        parts = vec![Rect::new(0.0, 0.0, t, t)]; // most of the tile: redraw it whole
    }
    for part in &parts {
        let r = (part.x0, part.y0, part.width(), part.height());
        draw_part(
            p,
            cache,
            k,
            g,
            (r.0 as u32, r.1 as u32, r.2 as u32, r.3 as u32),
            draw,
        );
    }
    true
}

/// Merges overlapping parts, then the closest pairs until at most [`PATCHES_MAX`] are left.
fn merge(parts: &mut Vec<Rect>) {
    assert!(parts.len() <= PARTS_MAX + 1);
    let mut merged = true;
    while merged {
        merged = false;
        'pairs: for i in 0..parts.len() {
            for j in i + 1..parts.len() {
                if parts[i].intersect(parts[j]).area() > 0.0 {
                    parts[i] = parts[i].union(parts[j]);
                    parts.swap_remove(j);
                    merged = true;
                    break 'pairs;
                }
            }
        }
        if !merged && parts.len() > PATCHES_MAX {
            let (mut best, mut cost) = ((0, 1), f64::INFINITY);
            for i in 0..parts.len() {
                for j in i + 1..parts.len() {
                    let c = parts[i].union(parts[j]).area();
                    if c < cost {
                        (best, cost) = ((i, j), c);
                    }
                }
            }
            parts[best.0] = parts[best.0].union(parts[best.1]);
            parts.swap_remove(best.1);
            merged = true;
        }
    }
    assert!(parts.len() <= PATCHES_MAX);
}
