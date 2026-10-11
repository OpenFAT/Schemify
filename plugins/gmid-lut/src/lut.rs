//! gm/Id lookup tables: per-width device quantities on an (L, VDS, VGS) grid, their `.glut`
//! v2 binary form, and the one query every exported function is built on ([`Lut::point_at`]).
//!
//! Data design:
//! 1. In: `.glut` bytes or a characterization grid. Out: [`Lut`] (SoA), then one [`Point`] per
//!    query (gm/Id, L, VDS).
//! 2. How many: a few devices per project ([`DEVICES_MAX`]); ≤ [`CELLS_MAX`] grid cells each.
//! 3. Widths: axes `f64` (they are compared against user values), cells `f32` (7 significant
//!    digits is beyond any model's accuracy and halves the file), axis lengths `u16` in the file
//!    (capped at [`AXIS_MAX`]).
//! 4. Access: a query reads two VDS rows of one L row along VGS; VGS is the innermost index, so
//!    that scan is contiguous. Cell `(l, d, g)` is at `((l·nVDS)+d)·nVGS+g`.
//! 5. Lifetime: the plugin session; a reload replaces the whole table.
//! 6. Parallel: no (a query touches ~10^2 cells).
//!
//! Units are SI throughout: L in metres, currents per metre of width (A/m), conductances S/m,
//! capacitance F/m. A p-channel device is stored as magnitudes (|VGS|, |VDS|, |Id|).

/// File magic.
pub const MAGIC: [u8; 4] = *b"GLUT";
/// Format version this module reads and writes.
pub const VERSION: u32 = 2;
/// Longest axis.
pub const AXIS_MAX: usize = 1 << 12;
/// Most grid cells per device (4 `f32` blocks of this size are 64 MiB).
pub const CELLS_MAX: usize = 1 << 22;
/// Longest device name in bytes.
pub const NAME_MAX: usize = 256;
/// Longest provenance text in bytes.
pub const PROVENANCE_MAX: usize = 4096;
/// Most devices one plugin keeps loaded.
pub const DEVICES_MAX: usize = 64;

/// Channel type of the characterized device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Polarity {
    /// n-channel.
    N = 0,
    /// p-channel (stored as magnitudes).
    P = 1,
}

/// One device's table. Invariants (checked by [`Lut::check`], which decoding and building
/// both run): axes non-empty, finite and strictly ascending, L > 0, at least 2 VGS points,
/// `vds_nom` inside the VDS axis, every block `l·vds·vgs` long and finite.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut {
    /// Device name as characterized (provenance; the plugin keys tables by `gmid.chars` name).
    pub name: String,
    /// Channel type.
    pub polarity: Polarity,
    /// Supply the device was characterized for (V).
    pub vdd: f64,
    /// VDS [`Lut::point`] answers at (V), normally `vdd / 2`; [`Lut::point_at`] takes any VDS
    /// on the axis.
    pub vds_nom: f64,
    /// Free text: PDK, corner, temperature, and whether the data is simulated or synthetic.
    pub provenance: String,
    /// Channel lengths (m), ascending.
    pub l: Vec<f64>,
    /// |VDS| grid (V), ascending.
    pub vds: Vec<f64>,
    /// |VGS| grid (V), ascending.
    pub vgs: Vec<f64>,
    /// |Id| per width (A/m).
    pub id_w: Vec<f32>,
    /// gm per width (S/m).
    pub gm_w: Vec<f32>,
    /// gds per width (S/m).
    pub gds_w: Vec<f32>,
    /// Cgg per width (F/m).
    pub cgg_w: Vec<f32>,
}

/// A device's small-signal operating point at one (gm/Id, L), per metre of width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// |VGS| (V).
    pub vgs: f64,
    /// Drain current density |Id|/W (A/m).
    pub jd: f64,
    /// gm/W (S/m).
    pub gm_w: f64,
    /// gds/W (S/m).
    pub gds_w: f64,
    /// Cgg/W (F/m).
    pub cgg_w: f64,
}

/// Why a query has no answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Miss {
    /// L is not finite or outside the table's L axis (no extrapolation).
    L,
    /// gm/Id is not finite or not reachable at that L (above the row's highest value or below
    /// its value at the highest VGS).
    GmId,
    /// VDS is not finite or outside the table's VDS axis (no extrapolation).
    Vds,
}

impl Lut {
    /// Number of grid cells (`l·vds·vgs`).
    pub fn cells(&self) -> usize {
        self.l.len() * self.vds.len() * self.vgs.len()
    }

    /// Index of cell (`l`, `d`, `g`).
    #[inline]
    pub fn idx(&self, l: usize, d: usize, g: usize) -> usize {
        debug_assert!(l < self.l.len() && d < self.vds.len() && g < self.vgs.len());
        (l * self.vds.len() + d) * self.vgs.len() + g
    }

    /// Validates the invariants listed on [`Lut`].
    ///
    /// # Errors
    ///
    /// A short reason for the first violated invariant.
    pub fn check(&self) -> Result<(), &'static str> {
        let ascending =
            |a: &[f64]| a.iter().all(|x| x.is_finite()) && a.windows(2).all(|w| w[0] < w[1]);
        if self.name.is_empty() || self.name.len() > NAME_MAX {
            return Err("device name empty or too long");
        }
        if self.provenance.len() > PROVENANCE_MAX {
            return Err("provenance too long");
        }
        if !(self.vdd.is_finite() && self.vdd > 0.0) {
            return Err("vdd must be positive");
        }
        for (axis, min) in [(&self.l, 1), (&self.vds, 1), (&self.vgs, 2)] {
            if axis.len() < min || axis.len() > AXIS_MAX || !ascending(axis) {
                return Err("axis empty, too long, non-finite or not ascending");
            }
        }
        if self.l[0] <= 0.0 {
            return Err("L must be positive");
        }
        let (v0, v1) = (self.vds[0], self.vds[self.vds.len() - 1]);
        if !(self.vds_nom >= v0 && self.vds_nom <= v1) {
            return Err("nominal VDS outside the VDS axis");
        }
        let n = self.cells();
        if n > CELLS_MAX {
            return Err("too many cells");
        }
        for block in [&self.id_w, &self.gm_w, &self.gds_w, &self.cgg_w] {
            if block.len() != n || !block.iter().all(|x| x.is_finite()) {
                return Err("data block has the wrong length or a non-finite value");
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ query

    /// The operating point where gm/Id equals `gm_id` at length `l` (m), at the nominal VDS
    /// ([`Lut::vds_nom`]); [`Lut::point_at`] with that VDS.
    ///
    /// # Errors
    ///
    /// As [`Lut::point_at`]; never [`Miss::Vds`] on a checked table.
    pub fn point(&self, gm_id: f64, l: f64) -> Result<Point, Miss> {
        self.point_at(gm_id, l, self.vds_nom)
    }

    /// The operating point where gm/Id equals `gm_id` at length `l` (m) and |VDS| `vds` (V).
    ///
    /// Steps: the VDS rows bracketing `vds` are blended linearly; VGS is found by inverse
    /// lookup of gm/Id, scanning down from the highest VGS (gm/Id rises as VGS falls) to the
    /// first VGS where it reaches `gm_id`. A dip on the way (a local maximum in moderate
    /// inversion, as sky130 pfet bins show) is crossed, so the reachable gm/Id at one L row run
    /// from the value at the highest VGS to the row's highest value ([`Lut::gm_id_range`]).
    /// Inside the bracketing VGS cell the positive quantities are interpolated geometrically
    /// (exact for the exponential weak-inversion current); the two bracketing L rows are
    /// blended in log L, quantities geometrically (exact for the power laws jd ∝ 1/L,
    /// ft ∝ 1/L²), VGS linearly.
    ///
    /// # Errors
    ///
    /// [`Miss::L`] outside the L axis (a relative 1e-9 slack absorbs `0.5u` ≠ `0.5e-6`
    /// rounding); [`Miss::Vds`] outside the VDS axis (1 nV slack); [`Miss::GmId`] when `gm_id`
    /// is not reachable.
    ///
    /// Cost: O(VGS points) for each of ≤ 2 L rows.
    pub fn point_at(&self, gm_id: f64, l: f64, vds: f64) -> Result<Point, Miss> {
        debug_assert!(self.id_w.len() == self.cells() && self.cgg_w.len() == self.cells());
        if !(gm_id.is_finite() && gm_id > 0.0) {
            return Err(Miss::GmId);
        }
        if !(l.is_finite() && l > 0.0) {
            return Err(Miss::L);
        }
        let d = self.vds_bracket(vds)?;
        let (first, last) = (self.l[0], self.l[self.l.len() - 1]);
        if l < first * (1.0 - 1e-9) || l > last * (1.0 + 1e-9) {
            return Err(Miss::L);
        }
        let l = l.clamp(first, last);
        let i1 = self.l.partition_point(|&x| x < l);
        // On a grid length (within rounding) the row answers alone.
        if i1 == 0 || 1.0 - l / self.l[i1] < 1e-9 {
            return self.row_point(i1, gm_id, d);
        }
        let i0 = i1 - 1;
        if l / self.l[i0] - 1.0 < 1e-9 {
            return self.row_point(i0, gm_id, d);
        }
        let (p0, p1) = (self.row_point(i0, gm_id, d)?, self.row_point(i1, gm_id, d)?);
        let t = (l / self.l[i0]).ln() / (self.l[i1] / self.l[i0]).ln();
        debug_assert!((0.0..=1.0).contains(&t));
        Ok(Point {
            vgs: lerp(p0.vgs, p1.vgs, t),
            jd: geo(p0.jd, p1.jd, t),
            gm_w: geo(p0.gm_w, p1.gm_w, t),
            gds_w: geo(p0.gds_w, p1.gds_w, t),
            cgg_w: geo(p0.cgg_w, p1.cgg_w, t),
        })
    }

    /// VDS rows bracketing `vds` and the blend factor between them.
    fn vds_bracket(&self, vds: f64) -> Result<(usize, usize, f64), Miss> {
        let (first, last) = (self.vds[0], self.vds[self.vds.len() - 1]);
        if !(vds.is_finite() && vds >= first - 1e-9 && vds <= last + 1e-9) {
            return Err(Miss::Vds);
        }
        let v = vds.clamp(first, last);
        let d1 = self.vds.partition_point(|&x| x < v).min(self.vds.len() - 1);
        if d1 == 0 || self.vds[d1] == v {
            return Ok((d1, d1, 0.0));
        }
        let d0 = d1 - 1;
        let t = (v - self.vds[d0]) / (self.vds[d1] - self.vds[d0]);
        debug_assert!((0.0..=1.0).contains(&t));
        Ok((d0, d1, t))
    }

    /// `[id, gm, gds, cgg]` per width at L row `li`, VGS point `g`, VDS bracket `d`.
    fn quantities(&self, li: usize, (d0, d1, td): (usize, usize, f64), g: usize) -> [f64; 4] {
        let at = |block: &[f32]| {
            let a = f64::from(block[self.idx(li, d0, g)]);
            lerp(a, f64::from(block[self.idx(li, d1, g)]), td)
        };
        [
            at(&self.id_w),
            at(&self.gm_w),
            at(&self.gds_w),
            at(&self.cgg_w),
        ]
    }

    /// [`Lut::point_at`] for one L row and VDS bracket `d`.
    fn row_point(&self, li: usize, target: f64, d: (usize, usize, f64)) -> Result<Point, Miss> {
        let ng = self.vgs.len();
        assert!(li < self.l.len() && ng >= 2);
        // Scan from the strong-inversion end down. Every value passed so far is below
        // `target` (else the scan stopped there), so the first point at or above it closes the
        // first upward crossing, after any dip. Cutoff (no current, a sign flip) ends the row.
        let mut hi = self.quantities(li, d, ng - 1);
        let mut hi_v = gm_id_of(&hi);
        if hi_v <= 0.0 || target < hi_v {
            return Err(Miss::GmId);
        }
        for g in (0..ng - 1).rev() {
            let lo = self.quantities(li, d, g);
            let lo_v = gm_id_of(&lo);
            if lo_v <= 0.0 {
                break;
            }
            if target <= lo_v {
                debug_assert!(hi_v <= target);
                // Equal ends only on the first step, where `target == hi_v == lo_v`.
                let t = if lo_v > hi_v {
                    (target - hi_v) / (lo_v - hi_v)
                } else {
                    0.0
                };
                return Ok(Point {
                    vgs: lerp(self.vgs[g + 1], self.vgs[g], t),
                    jd: geo(hi[0], lo[0], t),
                    gm_w: geo(hi[1], lo[1], t),
                    gds_w: geo(hi[2], lo[2], t),
                    cgg_w: geo(hi[3], lo[3], t),
                });
            }
            (hi, hi_v) = (lo, lo_v);
        }
        Err(Miss::GmId)
    }

    /// The gm/Id interval [`Lut::point`] answers at L row `li`: from the value at the highest
    /// VGS up to the row's highest value above cutoff (dips crossed). `(0, 0)` when the row has
    /// no usable branch.
    ///
    /// # Panics
    ///
    /// Panics if `li` is not a row of the L axis.
    pub fn gm_id_range(&self, li: usize) -> (f64, f64) {
        assert!(li < self.l.len());
        let Ok(d) = self.vds_bracket(self.vds_nom) else {
            return (0.0, 0.0);
        };
        let ng = self.vgs.len();
        let low = gm_id_of(&self.quantities(li, d, ng - 1));
        if low <= 0.0 {
            return (0.0, 0.0);
        }
        let mut peak = low;
        for g in (0..ng - 1).rev() {
            let v = gm_id_of(&self.quantities(li, d, g));
            if v <= 0.0 {
                break;
            }
            peak = peak.max(v);
        }
        debug_assert!(peak >= low);
        (low, peak)
    }

    // ------------------------------------------------------------------ file format

    /// Appends the `.glut` v2 encoding (layout in the crate README).
    ///
    /// # Panics
    ///
    /// Panics if the table fails [`Lut::check`] (only valid tables are written).
    pub fn encode(&self, out: &mut Vec<u8>) {
        assert_eq!(self.check(), Ok(()), "encoding an invalid LUT");
        let start = out.len();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.push(self.polarity as u8);
        out.extend_from_slice(&self.vdd.to_le_bytes());
        out.extend_from_slice(&self.vds_nom.to_le_bytes());
        for s in [&self.name, &self.provenance] {
            out.extend_from_slice(&(s.len() as u16).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        for axis in [&self.l, &self.vds, &self.vgs] {
            out.extend_from_slice(&(axis.len() as u16).to_le_bytes());
        }
        for axis in [&self.l, &self.vds, &self.vgs] {
            axis.iter()
                .for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
        }
        for block in [&self.id_w, &self.gm_w, &self.gds_w, &self.cgg_w] {
            block
                .iter()
                .for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
        }
        debug_assert_eq!(out.len() - start, self.encoded_len());
    }

    fn encoded_len(&self) -> usize {
        4 + 4
            + 1
            + 16
            + 2
            + self.name.len()
            + 2
            + self.provenance.len()
            + 6
            + 8 * (self.l.len() + self.vds.len() + self.vgs.len())
            + 16 * self.cells()
    }

    /// Parses `.glut` v2 bytes. Never panics; allocates only after checking that the declared
    /// sizes are within the caps and present in `bytes`.
    ///
    /// # Errors
    ///
    /// A short reason: bad magic or version, truncation, trailing bytes, oversized axes, bad
    /// UTF-8, or any [`Lut::check`] violation.
    pub fn decode(bytes: &[u8]) -> Result<Lut, &'static str> {
        let mut r = Reader { b: bytes, at: 0 };
        if r.take(4)? != MAGIC {
            return Err("not a .glut file (bad magic)");
        }
        if r.u32()? != VERSION {
            return Err("unsupported .glut version");
        }
        let polarity = match r.u8()? {
            0 => Polarity::N,
            1 => Polarity::P,
            _ => return Err("bad polarity"),
        };
        let (vdd, vds_nom) = (r.f64()?, r.f64()?);
        let name = r.text(NAME_MAX)?;
        let provenance = r.text(PROVENANCE_MAX)?;
        let (nl, nd, ng) = (r.u16()? as usize, r.u16()? as usize, r.u16()? as usize);
        if nl.max(nd).max(ng) > AXIS_MAX {
            return Err("axis too long");
        }
        let n = nl * nd * ng;
        if n > CELLS_MAX {
            return Err("too many cells");
        }
        if r.left() != 8 * (nl + nd + ng) + 16 * n {
            return Err("size does not match the declared axes (truncated or trailing bytes)");
        }
        let (l, vds, vgs) = (r.f64s(nl)?, r.f64s(nd)?, r.f64s(ng)?);
        let lut = Lut {
            name,
            polarity,
            vdd,
            vds_nom,
            provenance,
            l,
            vds,
            vgs,
            id_w: r.f32s(n)?,
            gm_w: r.f32s(n)?,
            gds_w: r.f32s(n)?,
            cgg_w: r.f32s(n)?,
        };
        debug_assert_eq!(r.left(), 0);
        lut.check()?;
        Ok(lut)
    }
}

/// Bounds-checked little-endian cursor.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn left(&self) -> usize {
        self.b.len() - self.at
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let end = self.at.checked_add(n).ok_or("truncated")?;
        let s = self.b.get(self.at..end).ok_or("truncated")?;
        self.at = end;
        Ok(s)
    }

    fn arr<const N: usize>(&mut self) -> Result<[u8; N], &'static str> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }

    fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(self.arr::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, &'static str> {
        Ok(u16::from_le_bytes(self.arr()?))
    }

    fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_le_bytes(self.arr()?))
    }

    fn f64(&mut self) -> Result<f64, &'static str> {
        Ok(f64::from_le_bytes(self.arr()?))
    }

    fn text(&mut self, max: usize) -> Result<String, &'static str> {
        let n = self.u16()? as usize;
        if n > max {
            return Err("text field too long");
        }
        let s = std::str::from_utf8(self.take(n)?).map_err(|_| "text field is not UTF-8")?;
        Ok(s.to_owned())
    }

    fn f64s(&mut self, n: usize) -> Result<Vec<f64>, &'static str> {
        let raw = self.take(n * 8)?;
        Ok(raw
            .chunks_exact(8)
            .map(|c| f64::from_le_bytes(c.try_into().expect("8 bytes")))
            .collect())
    }

    fn f32s(&mut self, n: usize) -> Result<Vec<f32>, &'static str> {
        let raw = self.take(n * 4)?;
        Ok(raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().expect("4 bytes")))
            .collect())
    }
}

/// gm/Id of `[id, gm, gds, cgg]`; 0 off the usable branch (cutoff noise, a sign flip).
fn gm_id_of(q: &[f64; 4]) -> f64 {
    if q.iter().all(|&x| x > 0.0) {
        q[1] / q[0]
    } else {
        0.0
    }
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + t * (b - a)
}

/// Geometric interpolation for positive quantities; falls back to linear when a sign is off.
fn geo(a: f64, b: f64, t: f64) -> f64 {
    if a > 0.0 && b > 0.0 {
        a * (b / a).powf(t)
    } else {
        lerp(a, b, t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{self, Ekv};

    #[test]
    fn format_round_trips_and_rejects_every_truncation() {
        let lut = synth::lut(&Ekv::default(), "nsynth");
        let mut bytes = Vec::new();
        lut.encode(&mut bytes);
        assert_eq!(Lut::decode(&bytes).unwrap(), lut);
        for len in 0..bytes.len() {
            assert!(
                Lut::decode(&bytes[..len]).is_err(),
                "truncated at {len} parsed"
            );
        }
        let mut long = bytes.clone();
        long.push(0);
        assert!(Lut::decode(&long).is_err(), "trailing byte accepted");
    }

    #[test]
    fn malformed_bytes_never_panic() {
        let lut = synth::lut(&Ekv::default(), "nsynth");
        let mut good = Vec::new();
        lut.encode(&mut good);
        // Deterministic xorshift flips over the header and a sample of the body.
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..4000 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let mut b = good.clone();
            let at = if s & 1 == 0 {
                (s >> 8) as usize % 96.min(b.len())
            } else {
                (s >> 8) as usize % b.len()
            };
            b[at] ^= 1 << ((s >> 40) % 8);
            if let Ok(l) = Lut::decode(&b) {
                assert_eq!(l.check(), Ok(()));
                let _ = l.point(15.0, l.l[0]);
            }
        }
        // Huge declared sizes: must fail before allocating.
        let mut huge = good.clone();
        let axes_at = 4 + 4 + 1 + 16 + 2 + lut.name.len() + 2 + lut.provenance.len();
        huge[axes_at..axes_at + 6].copy_from_slice(&[0xff; 6]);
        assert!(Lut::decode(&huge).is_err());
        assert!(Lut::decode(b"GLUT").is_err());
        assert!(Lut::decode(&[]).is_err());
    }

    /// A row whose gm/Id dips in moderate inversion (as sky130 pfet bins do): targets below
    /// the local peak answer on its strong-inversion side, targets above it past the dip, up to
    /// the row's highest value; VGS still falls as gm/Id rises.
    #[test]
    fn a_dip_in_gm_id_is_crossed() {
        // gm/Id per VGS point (0 V … 1 V in 0.1 V); read from the top: 2 4 6 8, dip 7 6, 9 12,
        // then 11 10 9 below the highest value.
        let profile = [9.0f32, 10.0, 11.0, 12.0, 9.0, 6.0, 7.0, 8.0, 6.0, 4.0, 2.0];
        let n = profile.len();
        let lut = Lut {
            name: "dip".into(),
            polarity: Polarity::N,
            vdd: 1.0,
            vds_nom: 0.5,
            provenance: String::new(),
            l: vec![1e-6],
            vds: vec![0.5],
            vgs: (0..n).map(|k| 0.1 * k as f64).collect(),
            id_w: vec![1.0; n],
            gm_w: profile.to_vec(),
            gds_w: vec![1.0; n],
            cgg_w: vec![1.0; n],
        };
        assert_eq!(lut.check(), Ok(()));
        let vgs = |g: f64| lut.point(g, 1e-6).map(|p| p.vgs);
        assert!(
            (vgs(7.5).unwrap() - 0.725).abs() < 1e-12,
            "before the local peak"
        );
        assert!((vgs(8.0).unwrap() - 0.7).abs() < 1e-12, "at the local peak");
        let past = vgs(10.0).unwrap();
        assert!(
            (past - (0.4 - 0.1 / 3.0)).abs() < 1e-12,
            "past the dip: {past}"
        );
        assert!(
            (vgs(12.0).unwrap() - 0.3).abs() < 1e-12,
            "the row's highest value"
        );
        assert_eq!(vgs(12.5), Err(Miss::GmId));
        assert_eq!(vgs(1.5), Err(Miss::GmId));
        assert_eq!(lut.gm_id_range(0), (2.0, 12.0));
        let ts: Vec<f64> = (0..=100).map(|k| 2.0 + 0.1 * f64::from(k)).collect();
        for w in ts.windows(2) {
            assert!(vgs(w[1]).unwrap() <= vgs(w[0]).unwrap(), "{w:?}");
        }
    }

    /// Off-nominal VDS against the analytic model: id and gm are linear in VDS there and gds
    /// does not depend on it, so the VDS blend is exact up to f32 storage. The nominal VDS
    /// reproduces [`Lut::point`] bit for bit; a VDS off the axis misses.
    #[test]
    fn queries_at_any_vds_on_the_axis() {
        let m = Ekv::default();
        let lut = synth::lut(&m, "nsynth");
        let l = lut.l[3];
        for vds in [0.15, 0.3, 0.5, 0.9, 1.0, 1.35, 1.7, 1.8] {
            for g in [5.0, 10.0, 15.0, 20.0] {
                let p = lut.point_at(g, l, vds).unwrap();
                let vgs = m.vgs_for(g);
                let q = m.quantities(vgs, vds, l);
                assert!(
                    (p.vgs - vgs).abs() < 2e-3,
                    "vgs at {vds}: {} vs {vgs}",
                    p.vgs
                );
                let jd_err = (p.jd - q[0]).abs() / q[0];
                assert!(jd_err < 0.02, "jd at {vds}: {} vs {}", p.jd, q[0]);
                let (gain, want) = (p.gm_w / p.gds_w, q[1] / q[2]);
                assert!(
                    (gain - want).abs() < 0.02 * want,
                    "gain at {vds}: {gain} vs {want}"
                );
            }
        }
        assert_eq!(lut.point_at(15.0, l, lut.vds_nom), lut.point(15.0, l));
        assert_eq!(lut.point_at(15.0, l, 0.1), Err(Miss::Vds));
        assert_eq!(lut.point_at(15.0, l, 1.9), Err(Miss::Vds));
        assert_eq!(lut.point_at(15.0, l, f64::NAN), Err(Miss::Vds));
        assert!(lut.point_at(15.0, l, 1.8 + 1e-12).is_ok(), "edge slack");
    }

    #[test]
    fn out_of_range_queries_miss() {
        let m = Ekv::default();
        let lut = synth::lut(&m, "nsynth");
        let (lo, hi) = (lut.l[0], lut.l[lut.l.len() - 1]);
        assert_eq!(lut.point(15.0, lo * 0.9), Err(Miss::L));
        assert_eq!(lut.point(15.0, hi * 1.1), Err(Miss::L));
        assert_eq!(lut.point(15.0, f64::NAN), Err(Miss::L));
        assert_eq!(lut.point(100.0, lo), Err(Miss::GmId));
        assert_eq!(lut.point(0.5, lo), Err(Miss::GmId));
        assert_eq!(lut.point(-3.0, lo), Err(Miss::GmId));
        assert!(lut.point(15.0, lo * (1.0 + 1e-12)).is_ok(), "edge slack");
        let (low, peak) = lut.gm_id_range(0);
        assert!(peak > 20.0 && peak < 1.0 / (m.n * m.ut), "peak {peak}");
        assert!(low > 1.0 && low < 5.0, "low {low}");
        assert!(lut.point(peak * 0.999, lo).is_ok() && lut.point(low * 1.001, lo).is_ok());
    }
}
