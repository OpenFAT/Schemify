//! Glyph runs as atlas masks: a [`crate::Painter::glyphs`] call in, mask instances out. A
//! glyph is rasterized once per font, glyph, em matrix and subpixel phase ([`PHASES`]) by
//! `vello_cpu` into the atlas and drawn one texel per pixel; past [`EM_MAX`] it is rasterized
//! at that em and drawn scaled; while a zoom meets new sizes faster than
//! `EXACT_PER_FRAME` masks a frame, a glyph's mask at another size stands in, scaled.
//!
//! Contents:
//! - `GpuPainter::draw_glyphs` (`pub(crate)`)

use std::hash::{Hash, Hasher};

use kurbo::{Affine, Point};
use peniko::FontData;
use vello_cpu::Glyph;

use schemify_ui::skrifa::{self, MetadataProvider};

use super::{GpuPainter, MASK_MAX, MASK_SCALED, What};
use crate::atlas::{Key, Placed};

/// Subpixel positions per pixel a glyph is rasterized at (horizontally; vertically too when
/// it is not hinted): 1/8 px. At 1/4 px (glifo's choice) dense UI text measured twice as far
/// from the CPU painter's continuous positions (mean |Δ| 0.72 against 0.34).
const PHASES: f64 = 8.0;

/// Glyphs larger than this em (device px) are rasterized at it and drawn scaled.
const EM_MAX: f64 = 96.0;

/// What every glyph of one run shares: its em (device px), whether `vello_cpu` hints it,
/// its [`run_hash`], and its direction's hash (the em matrix at unit em, which a zoom keeps).
#[derive(Clone, Copy)]
struct Run {
    em: f64,
    hint: bool,
    hash: u64,
    dir: u64,
}

impl GpuPainter<'_> {
    /// Draws `glyphs` of `font` at `px` through `at` (logical) in colour word `c`: each an
    /// atlas mask, rasterized on first use. A transparent colour or a degenerate size or
    /// transform draws nothing.
    pub(crate) fn draw_glyphs(
        &mut self,
        font: &FontData,
        px: f32,
        coords: &[i16],
        c: u32,
        at: Affine,
        glyphs: &[Glyph],
    ) {
        if c >> 24 == 0 || px <= 0.0 {
            return;
        }
        let t = self.base * at;
        let [a, b, cc, d, _, _] = t.as_coeffs();
        let em = (a * d - b * cc).abs().sqrt() * f64::from(px);
        if em <= 0.0 {
            return; // a degenerate transform draws nothing
        }
        if em > EM_MAX {
            for g in glyphs {
                self.glyph_scaled(font, px, coords, t, g, c, EM_MAX / em);
            }
            return;
        }
        let m = [a, b, cc, d].map(|v| v * f64::from(px));
        let run = Run {
            em,
            hint: hinted(&t),
            hash: run_hash(m, coords),
            dir: run_hash(m.map(|v| v / em), coords),
        };
        for g in glyphs {
            self.glyph(font, px, coords, t, run, g, c);
        }
    }
}

// ---- private ----

impl GpuPainter<'_> {
    /// One glyph of a run through `t` (run space → device px) at `px`.
    #[allow(clippy::too_many_arguments)]
    fn glyph(
        &mut self,
        font: &FontData,
        px: f32,
        coords: &[i16],
        t: Affine,
        run: Run,
        g: &Glyph,
        c: u32,
    ) {
        let o = t * Point::new(f64::from(g.x), f64::from(g.y));
        let (ix, qx) = phase(o.x);
        let (iy, qy) = if run.hint {
            (o.y.round() as i32, 0)
        } else {
            phase(o.y)
        };
        // Off the clip by more than a few ems: skip (cheaper than the lookup).
        let r = 4.0 * run.em;
        if self.clipped_out([o.x - r, o.y - r, o.x + r, o.y + r]) {
            return;
        }
        let key = key_in_run(font, g.id, run.hash, [qx, qy]);
        let same = [key[0], key[1], run.dir];
        let placed = match self.batch.atlas.get(&key) {
            Some(p) => p,
            None => {
                if self.batch.exact_left == 0 && self.stand_in(&same, run.em, o, c) {
                    self.batch.unsettled = true;
                    return;
                }
                self.batch.exact_left = self.batch.exact_left.saturating_sub(1);
                // The glyph's origin at its phase inside a cell around its outline's box.
                let at = (f64::from(qx) / PHASES, f64::from(qy) / PHASES);
                let Some(cell) = glyph_cell(font, px, coords, t, g, o, at) else {
                    return;
                };
                let local = (at.0 - f64::from(cell[0]), at.1 - f64::from(cell[1]));
                let shift = Affine::translate((local.0 - o.x, local.1 - o.y)) * t;
                let Some(p) = self.raster_glyph(key, font, px, coords, g, shift, cell) else {
                    return;
                };
                self.batch
                    .latest
                    .insert(same, (key, run.em as f32, [qx, qy]));
                p
            }
        };
        self.mask(placed, ix, iy, c);
    }

    /// Draws glyph `same` (its font, id and direction) of em `em` with its origin at device
    /// `o` from its last exact mask at another size, scaled about the origin; false when no
    /// such mask is held.
    fn stand_in(&mut self, same: &Key, em: f64, o: Point, c: u32) -> bool {
        let Some(&(key, em0, q)) = self.batch.latest.get(same) else {
            return false;
        };
        let Some(p) = self.batch.atlas.get(&key) else {
            return false;
        };
        let s = p.slot;
        if s.w == 0 {
            return true; // no outline at any size
        }
        // The old cell relative to its glyph's origin, scaled to this size.
        let k = em / f64::from(em0);
        assert!(k > 0.0, "a positive em ratio");
        let at = (f64::from(q[0]) / PHASES, f64::from(q[1]) / PHASES);
        let (x0, y0) = (f64::from(p.off[0]) - at.0, f64::from(p.off[1]) - at.1);
        let (x1, y1) = (x0 + f64::from(s.w), y0 + f64::from(s.h));
        let pos = [o.x + k * x0, o.y + k * y0, o.x + k * x1, o.y + k * y1].map(|v| v as f32);
        let aux = [s.x, s.y, s.x + s.w, s.y + s.h].map(f32::from);
        self.batch.push(MASK_SCALED, pos, aux, c, What::Ui);
        true
    }

    /// A glyph whose em is over [`EM_MAX`]: rasterized at `k` times its size (an em of
    /// `EM_MAX`, so every size past it shares one mask per direction) and drawn scaled up.
    #[allow(clippy::too_many_arguments)]
    fn glyph_scaled(
        &mut self,
        font: &FontData,
        px: f32,
        coords: &[i16],
        t: Affine,
        g: &Glyph,
        c: u32,
        k: f64,
    ) {
        assert!(k > 0.0 && k < 1.0, "a glyph scaled down to rasterize");
        let o = t * Point::new(f64::from(g.x), f64::from(g.y));
        let [a, b, cc, d, _, _] = t.as_coeffs();
        // Through `ts`, the glyph's origin lands where it does through `t`, its outline `k`
        // times as large.
        let lin = Affine::new([a * k, b * k, cc * k, d * k, 0.0, 0.0]);
        let go = lin * Point::new(f64::from(g.x), f64::from(g.y));
        let ts = Affine::translate((o.x - go.x, o.y - go.y)) * lin;
        let m = [a, b, cc, d].map(|v| v * k * f64::from(px));
        // A phase no unscaled key has.
        let key = key_in_run(font, g.id, run_hash(m, coords), [PHASES as u8, 0]);
        let placed = match self.batch.atlas.get(&key) {
            Some(p) => p,
            None => {
                let Some(cell) = glyph_cell(font, px, coords, ts, g, o, (0.0, 0.0)) else {
                    return;
                };
                let shift =
                    Affine::translate((-f64::from(cell[0]) - o.x, -f64::from(cell[1]) - o.y)) * ts;
                match self.raster_glyph(key, font, px, coords, g, shift, cell) {
                    Some(p) => p,
                    None => return,
                }
            }
        };
        let s = placed.slot;
        if s.w == 0 {
            return;
        }
        // The cell, `1/k` times as large about the glyph's origin.
        let (x0, y0) = (f64::from(placed.off[0]) / k, f64::from(placed.off[1]) / k);
        let (x1, y1) = (x0 + f64::from(s.w) / k, y0 + f64::from(s.h) / k);
        let pos = [o.x + x0, o.y + y0, o.x + x1, o.y + y1].map(|v| v as f32);
        if self.clipped_out(pos.map(f64::from)) {
            return;
        }
        let aux = [s.x, s.y, s.x + s.w, s.y + s.h].map(f32::from);
        self.batch.push(MASK_SCALED, pos, aux, c, What::Ui);
    }

    /// Rasterizes glyph `g` through `shift` (run space → the cell's pixels) into a mask of
    /// `cell`'s size and stores it under `key`, its cell at `cell`'s corner; `None` when the
    /// atlas cannot place it. A cell or coverage with no pixels is stored empty.
    #[allow(clippy::too_many_arguments)]
    fn raster_glyph(
        &mut self,
        key: Key,
        font: &FontData,
        px: f32,
        coords: &[i16],
        g: &Glyph,
        shift: Affine,
        cell: [i32; 4],
    ) -> Option<Placed> {
        let (w, h) = ((cell[2] - cell[0]) as u16, (cell[3] - cell[1]) as u16);
        let off = [cell[0] as i16, cell[1] as i16];
        self.batch.rastered += 1;
        let batch = &mut *self.batch;
        let cov = if w == 0 || h == 0 {
            &[][..]
        } else {
            batch.raster.run(w, h, |ctx, res| {
                ctx.set_transform(shift);
                let run = ctx
                    .glyph_run(res, font)
                    .font_size(px)
                    .normalized_coords(coords);
                let _ = run.fill_glyphs(std::iter::once(*g));
            })
        };
        let (w, h) = if cov.is_empty() { (0, 0) } else { (w, h) };
        batch.atlas.insert(key, w, h, off, cov)
    }
}

/// Whether `t` is a positive uniform scale with no vertical skew: `vello_cpu` then hints the
/// glyphs it draws through it (vertically) and rounds their baseline to whole pixels.
fn hinted(t: &Affine) -> bool {
    const EPS: f64 = 1.0 / 4096.0; // glifo's SCALAR_NEARLY_ZERO
    let [a, b, _, d, _, _] = t.as_coeffs();
    (a - d).abs() <= EPS && a > 0.0 && d > 0.0 && b.abs() <= EPS
}

/// The hash of what every glyph of a run shares in its key: the em-to-device matrix `m` (to
/// 1/256 px of em: two runs that round alike rasterize alike) and the variation `coords`.
fn run_hash(m: [f64; 4], coords: &[i16]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    m.map(|v| (v * 256.0).round() as i64).hash(&mut h);
    coords.hash(&mut h);
    h.finish()
}

/// The atlas key of glyph `id` of `font` in a run of [`run_hash`] `run`, at subpixel phases
/// `q`: the phases mixed in by an odd multiplier, so each phase pair of a run is its own key
/// without hashing the run again per glyph. What decides the glyph's mask, and nothing else
/// (not where it is drawn).
fn key_in_run(font: &FontData, id: u32, run: u64, q: [u8; 2]) -> Key {
    let phase = u64::from(q[0]) | (u64::from(q[1]) << 8);
    [
        font.data.id(),
        (u64::from(font.index) << 32) | u64::from(id),
        run ^ (phase + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15),
    ]
}

/// `v` as a whole pixel and the nearest of [`PHASES`] subpixel positions in it (the last
/// rounds up to the next pixel's first).
fn phase(v: f64) -> (i32, u8) {
    let i = v.floor();
    let q = ((v - i) * PHASES).round();
    let (i, q) = if q >= PHASES { (i + 1.0, 0.0) } else { (i, q) };
    assert!((0.0..PHASES).contains(&q), "a phase in range");
    (i as i32, q as u8)
}

/// The whole-pixel cell (relative to the glyph's whole-pixel origin, which sits at `at`
/// inside it) around glyph `g`'s outline drawn through `t` with its origin at device `o`:
/// one pixel of antialiasing margin around, two vertically for hinting. `Some` of an empty
/// cell for a glyph with no outline; `None` for a face that does not parse.
fn glyph_cell(
    font: &FontData,
    px: f32,
    coords: &[i16],
    t: Affine,
    g: &Glyph,
    o: Point,
    at: (f64, f64),
) -> Option<[i32; 4]> {
    let face = skrifa::FontRef::from_index(font.data.data(), font.index).ok()?;
    let coords: Vec<skrifa::instance::NormalizedCoord> = coords
        .iter()
        .map(|&c| skrifa::instance::NormalizedCoord::from_bits(c))
        .collect();
    let size = skrifa::instance::Size::new(px);
    let metrics = face.glyph_metrics(size, skrifa::instance::LocationRef::new(&coords));
    let Some(b) = metrics.bounds(skrifa::GlyphId::new(g.id)) else {
        return Some([0, 0, 0, 0]);
    };
    // Outline corners in run space (font y up, run y down), through `t`, from the origin.
    let (gx, gy) = (f64::from(g.x), f64::from(g.y));
    let corners = [
        (b.x_min, b.y_min),
        (b.x_max, b.y_min),
        (b.x_min, b.y_max),
        (b.x_max, b.y_max),
    ]
    .map(|(x, y)| t * Point::new(gx + f64::from(x), gy - f64::from(y)) - o);
    let lo = corners
        .iter()
        .fold((f64::MAX, f64::MAX), |m, v| (m.0.min(v.x), m.1.min(v.y)));
    let hi = corners
        .iter()
        .fold((f64::MIN, f64::MIN), |m, v| (m.0.max(v.x), m.1.max(v.y)));
    let cell = [
        (at.0 + lo.0).floor() as i32 - 1,
        (at.1 + lo.1).floor() as i32 - 2,
        (at.0 + hi.0).ceil() as i32 + 1,
        (at.1 + hi.1).ceil() as i32 + 2,
    ];
    if cell[2] - cell[0] > MASK_MAX || cell[3] - cell[1] > MASK_MAX {
        return None; // ponytail: an outline over 5 ems of EM_MAX; none of the faces has one
    }
    assert!(cell[0] < cell[2], "a cell around the outline");
    assert!(cell[1] < cell[3], "a cell around the outline");
    Some(cell)
}

#[cfg(test)]
mod tests {
    use super::super::{Batch, EXACT_PER_FRAME, MASK, STRIDE};
    use super::*;
    use crate::Painter;
    use peniko::Color;
    use schemify_ui::TextSystem;

    /// The font and first glyph id `text` shapes to.
    fn shaped(text: &str) -> (FontData, u32) {
        let mut sys = TextSystem::new();
        let mut layout = schemify_ui::parley::Layout::new();
        sys.shape_into(text, 13.0, &mut layout);
        layout
            .lines()
            .flat_map(|l| l.items())
            .find_map(|i| match i {
                schemify_ui::parley::PositionedLayoutItem::GlyphRun(r) => Some((
                    r.run().font().clone(),
                    r.positioned_glyphs().next().expect("a glyph").id,
                )),
                _ => None,
            })
            .expect("a run")
    }

    /// The key of glyph `id` of `font` through em matrix `m` at phases `q` with `coords`.
    fn glyph_key(font: &FontData, id: u32, m: [f64; 4], q: [u8; 2], coords: &[i16]) -> Key {
        key_in_run(font, id, run_hash(m, coords), q)
    }

    /// Phases are exact: a position rounds to the nearest eighth of a pixel, half a step up,
    /// and the last half step wraps to the next pixel's first; whole-pixel shifts keep the
    /// phase.
    #[test]
    fn subpixel_phases_are_exact() {
        assert_eq!(PHASES, 8.0);
        assert_eq!(phase(10.0), (10, 0));
        assert_eq!(phase(10.0624), (10, 0));
        assert_eq!(phase(10.0625), (10, 1), "half a step rounds up");
        assert_eq!(phase(10.1874), (10, 1));
        assert_eq!(phase(10.1875), (10, 2));
        assert_eq!(phase(10.93), (10, 7));
        assert_eq!(phase(10.9375), (11, 0), "the last half step wraps");
        assert_eq!(phase(-0.05), (0, 0));
        assert_eq!(phase(-0.1), (-1, 7));
        for k in [-3, 0, 7, 1000] {
            let v = 0.3;
            assert_eq!(phase(f64::from(k) + v).1, phase(v).1, "shift by {k}");
        }
    }

    /// Glyph keys: equal for what rasterizes alike (the same em matrix within 1/512 px, any
    /// position), different for another glyph, face, size, phase, direction or coords.
    #[test]
    fn glyph_keys_bucket_exactly() {
        let (font, _) = shaped("a");
        let m = [13.0, 0.0, 0.0, 13.0];
        let k = glyph_key(&font, 7, m, [1, 0], &[]);
        assert_eq!(
            k,
            glyph_key(&font, 7, [13.001, 0.0, 0.0, 13.0], [1, 0], &[])
        );
        assert_ne!(k, glyph_key(&font, 8, m, [1, 0], &[]), "glyph");
        assert_ne!(
            k,
            glyph_key(&font, 7, [13.25, 0.0, 0.0, 13.25], [1, 0], &[]),
            "size"
        );
        assert_ne!(k, glyph_key(&font, 7, m, [2, 0], &[]), "x phase");
        assert_ne!(k, glyph_key(&font, 7, m, [1, 1], &[]), "y phase");
        assert_ne!(
            k,
            glyph_key(&font, 7, [0.0, 13.0, -13.0, 0.0], [1, 0], &[]),
            "turned"
        );
        assert_ne!(k, glyph_key(&font, 7, m, [1, 0], &[100]), "coords");
        let mut other = font.clone();
        other.index += 1;
        assert_ne!(k, glyph_key(&other, 7, m, [1, 0], &[]), "face");
    }

    /// The same glyph drawn at two whole-pixel positions is one mask; at another phase it is a
    /// second; the masks land a pixel apart where the positions are.
    #[test]
    fn a_glyph_is_rasterized_once_per_phase() {
        let (font, id) = shaped("e");
        let glyph = Glyph {
            id,
            x: 0.0,
            y: 12.0,
        };
        let mut batch = Batch::default();
        batch.begin_frame();
        let mut p = GpuPainter {
            batch: &mut batch,
            base: Affine::IDENTITY,
            rows: None,
        };
        let at = |x: f64| Affine::translate((x, 0.0));
        p.glyphs(&font, 13.0, &[], Color::WHITE, at(10.0), &[glyph]);
        p.glyphs(&font, 13.0, &[], Color::WHITE, at(31.0), &[glyph]);
        assert_eq!(
            p.batch.rastered, 1,
            "one mask for both whole-pixel positions"
        );
        p.glyphs(&font, 13.0, &[], Color::WHITE, at(31.5), &[glyph]);
        assert_eq!(p.batch.rastered, 2, "another phase, another mask");
        assert_eq!(p.batch.len(), 3);
        let x0 = |i: usize| {
            let b = &p.batch.instances()[i * STRIDE..];
            f32::from_ne_bytes(b[..4].try_into().expect("4 bytes"))
        };
        assert_eq!(
            x0(1) - x0(0),
            21.0,
            "a whole-pixel shift moves the cell by it"
        );
        assert!(
            p.batch.atlas.pending_bytes() > 0,
            "the masks wait for the GPU"
        );
    }

    /// A zoom: once a frame's exact budget is spent, a glyph met at a new size is drawn from
    /// its mask at another size (scaled) and the batch is unsettled; frames with no input
    /// rasterize the rest and settle on exact masks, then stay settled rasterizing nothing
    /// (idle at zero).
    #[test]
    fn a_zoom_draws_stand_ins_then_settles() {
        let (font, id) = shaped("x");
        let glyph = Glyph { id, x: 0.0, y: 0.0 };
        let mut batch = Batch::default();
        let n = EXACT_PER_FRAME as usize + 20;
        let frame = |batch: &mut Batch, sizes: &[f32]| {
            batch.begin_frame();
            let mut p = GpuPainter {
                batch,
                base: Affine::IDENTITY,
                rows: None,
            };
            for (k, &px) in sizes.iter().enumerate() {
                let at = Affine::translate((20.0 * k as f64, 30.0));
                p.glyphs(&font, px, &[], Color::WHITE, at, &[glyph]);
            }
            let kinds: Vec<u32> = (0..p.batch.len())
                .map(|i| {
                    let b = &p.batch.instances()[i * STRIDE + 52..][..4];
                    u32::from_ne_bytes(b.try_into().expect("4 bytes"))
                })
                .collect();
            (p.batch.settled(), kinds)
        };
        assert!(frame(&mut batch, &[10.0]).0, "nothing to stand in for");
        let sizes: Vec<f32> = (0..n).map(|k| 10.5 + 0.25 * k as f32).collect();
        let (settled, kinds) = frame(&mut batch, &sizes);
        assert!(!settled, "a zoom frame draws stand-ins");
        assert_eq!(kinds.len(), n, "every glyph drawn");
        let stand_ins = kinds.iter().filter(|&&k| k == MASK_SCALED).count();
        assert_eq!(stand_ins, n - EXACT_PER_FRAME as usize, "past the budget");
        let mut frames = 0;
        let kinds = loop {
            frames += 1;
            let (settled, kinds) = frame(&mut batch, &sizes);
            if settled {
                break kinds;
            }
            assert!(frames < 8, "settles");
        };
        assert!(kinds.iter().all(|&k| k == MASK), "settled on exact masks");
        let r = batch.rastered;
        assert!(frame(&mut batch, &sizes).0);
        assert_eq!(batch.rastered, r, "a settled frame rasterizes nothing");
    }
}
