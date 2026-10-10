//! Test oracle: a synthetic EKV-style transistor with closed-form gm, gds and Cgg, a LUT built
//! from it on a realistic grid, and the exact (bisection) answers the LUT queries must match.
//! Synthetic, not a PDK: it exists so interpolation error is measurable.

use crate::lut::{Lut, Polarity};

/// `Id/W = 2nβUt²·F²·(1+λVds)` with `F = ln(1+e^u)`, `u = (Vgs−Vt)/(2nUt)`, `β = µCox/L`,
/// `λ = λ0·L0/L`; `Cgg/W = Cox·L`. Then `gm/Id = σ(u)/(n·Ut·F)`, independent of L and VDS.
#[derive(Clone, Copy, Debug)]
pub struct Ekv {
    /// Subthreshold slope factor.
    pub n: f64,
    /// Thermal voltage (V).
    pub ut: f64,
    /// Threshold (V).
    pub vt: f64,
    /// µ·Cox (A/V²).
    pub mu_cox: f64,
    /// Channel-length modulation at `l0` (1/V).
    pub lambda0: f64,
    /// Reference length for `lambda0` (m).
    pub l0: f64,
    /// Gate capacitance per area (F/m²).
    pub cox: f64,
}

impl Default for Ekv {
    fn default() -> Ekv {
        Ekv {
            n: 1.3,
            ut: 0.025_85,
            vt: 0.45,
            mu_cox: 270e-6,
            lambda0: 0.1,
            l0: 0.15e-6,
            cox: 8.8e-3,
        }
    }
}

fn softplus(u: f64) -> f64 {
    if u > 30.0 { u } else { u.exp().ln_1p() }
}

fn sigmoid(u: f64) -> f64 {
    1.0 / (1.0 + (-u).exp())
}

impl Ekv {
    fn u(&self, vgs: f64) -> f64 {
        (vgs - self.vt) / (2.0 * self.n * self.ut)
    }

    /// `[id, gm, gds, cgg]` per metre of width.
    pub fn quantities(&self, vgs: f64, vds: f64, l: f64) -> [f64; 4] {
        let beta = self.mu_cox / l;
        let lambda = self.lambda0 * self.l0 / l;
        let (u, f) = (self.u(vgs), softplus(self.u(vgs)));
        let id0 = 2.0 * self.n * beta * self.ut * self.ut * f * f;
        let gm0 = 2.0 * beta * self.ut * f * sigmoid(u);
        [
            id0 * (1.0 + lambda * vds),
            gm0 * (1.0 + lambda * vds),
            id0 * lambda,
            self.cox * l,
        ]
    }

    /// The VGS where gm/Id equals `target` (bisection; gm/Id falls monotonically with VGS).
    pub fn vgs_for(&self, target: f64) -> f64 {
        let gmid = |v: f64| {
            let u = self.u(v);
            sigmoid(u) / (self.n * self.ut * softplus(u))
        };
        let (mut lo, mut hi) = (-2.0, 3.0);
        assert!(
            gmid(lo) > target && gmid(hi) < target,
            "target {target} outside the model"
        );
        for _ in 0..200 {
            let mid = 0.5 * (lo + hi);
            if gmid(mid) > target {
                lo = mid
            } else {
                hi = mid
            }
        }
        0.5 * (lo + hi)
    }
}

/// A LUT sampled from `m` on the default characterization grid (VGS 0..1.8 V in 25 mV,
/// VDS 0.15..1.8 V in 0.15 V, nominal 0.9 V) at 14 log-spaced L from 0.15 to 8 µm.
pub fn lut(m: &Ekv, name: &str) -> Lut {
    let vgs: Vec<f64> = (0..=72).map(|k| 1.8 * f64::from(k) / 72.0).collect();
    let vds: Vec<f64> = (1..=12).map(|k| 1.8 * f64::from(k) / 12.0).collect();
    let l: Vec<f64> = (0..14)
        .map(|k| 0.15e-6 * (8.0f64 / 0.15).powf(f64::from(k) / 13.0))
        .collect();
    let mut t = Lut {
        name: name.to_owned(),
        polarity: Polarity::N,
        vdd: 1.8,
        vds_nom: 0.9,
        provenance: "synthetic EKV model (test oracle), not a PDK".to_owned(),
        l,
        vds,
        vgs,
        id_w: Vec::new(),
        gm_w: Vec::new(),
        gds_w: Vec::new(),
        cgg_w: Vec::new(),
    };
    for &l in &t.l {
        for &d in &t.vds {
            for &g in &t.vgs {
                let q = m.quantities(g, d, l);
                t.id_w.push(q[0] as f32);
                t.gm_w.push(q[1] as f32);
                t.gds_w.push(q[2] as f32);
                t.cgg_w.push(q[3] as f32);
            }
        }
    }
    assert_eq!(t.check(), Ok(()));
    t
}
