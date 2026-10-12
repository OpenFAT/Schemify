//! gm/Id retarget (spec `interop.md` §10): intent records on export, the target-map parser,
//! and the Rust reference of the interpolation and width solve that `skill/sfy_retarget.il`
//! implements again in pure SKILL. The two must agree within 1e-9 relative
//! (`tests/retarget.rs` runs the SKILL in the test interpreter on random vectors).
//!
//! The query semantics are gmid-lut's (`plugins/gmid-lut/README.md` "Query semantics",
//! `src/lut.rs`, our own MIT code), ported here with `f64` cells: the target table comes
//! from Spectre in double precision on both sides, and the interop crate cannot link the
//! gm/Id guest. Every arithmetic step is written in the order the SKILL mirror performs it,
//! so the two agree to the last bits, not just to the tolerance.
//!
//! Data design ([`Table`]):
//! 1. In: characterization sweeps (absolute id, gm, gds, cgg of one device at width `w`) on
//!    an (L, VDS, VGS) grid. Out: one [`Solution`] per intent.
//! 2. How many: a few classes per design, one table each; ≤ [`AXIS_MAX`] points per axis,
//!    ≤ [`CELLS_MAX`] cells.
//! 3. Widths: `f64` throughout (SKILL floats are doubles; agreement to 1e-9 needs the same
//!    precision on both sides).
//! 4. Access: a query scans one or two L rows along VGS, blending two VDS rows; VGS is the
//!    innermost index, cell `(l, d, g)` at `((l·nVDS)+d)·nVGS+g`, quantities SoA.
//! 5. Lifetime: one retarget run.
//! 6. Parallel: no (a query touches ~10^2 cells).

use std::borrow::Cow;

use schemify_abi::text::{self, BodyLine};
use schemify_expr::{Mode, Program};
use schemify_format::RecordKind;

use crate::design::Design;
use crate::notes::Notes;
use crate::sfyx::{self, Record};

/// Longest table axis.
pub const AXIS_MAX: usize = 1 << 12;
/// Most cells in one table.
pub const CELLS_MAX: usize = 1 << 22;
/// Most classes in one target map.
pub const CLASSES_MAX: usize = 64;
/// Largest finger count an intent or a split may produce.
pub const NF_MAX: u32 = 1 << 12;
/// Deepest chain of var references followed from a width override to the `gmid.w` call.
const VAR_DEPTH_MAX: u32 = 8;

// ---------------------------------------------------------------------------------------
// Table and solve
// ---------------------------------------------------------------------------------------

/// One device class characterized on an (L, VDS, VGS) grid, per metre of width.
/// Invariants (checked by [`Table::check`], which [`Table::from_samples`] runs): axes
/// non-empty, finite, strictly ascending, L > 0, ≥ 2 VGS points, `vds_nom` inside the VDS
/// axis, every block `l·vds·vgs` long and finite.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    /// VDS answered when an intent gives none (V); the target uses `vdd / 2`.
    pub vds_nom: f64,
    /// Channel lengths (m), ascending.
    pub l: Vec<f64>,
    /// |VDS| rows (V), ascending.
    pub vds: Vec<f64>,
    /// |VGS| points (V), ascending.
    pub vgs: Vec<f64>,
    /// |Id| per width (A/m).
    pub id_w: Vec<f64>,
    /// gm per width (S/m).
    pub gm_w: Vec<f64>,
    /// gds per width (S/m).
    pub gds_w: Vec<f64>,
    /// Cgg per width (F/m).
    pub cgg_w: Vec<f64>,
}

/// The operating point at one (gm/Id, L, VDS), per metre of width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// |VGS| (V).
    pub vgs: f64,
    /// |Id|/W (A/m).
    pub jd: f64,
    /// gm/W (S/m).
    pub gm_w: f64,
    /// gds/W (S/m).
    pub gds_w: f64,
    /// Cgg/W (F/m).
    pub cgg_w: f64,
}

/// A solved intent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Solution {
    /// |VGS| at the solution (V).
    pub vgs: f64,
    /// |Id|/W there (A/m).
    pub jd: f64,
    /// Total width `id / jd` (m), before any finger split.
    pub w: f64,
    /// `gm/Id` of the interpolated point minus the target (1/V): nonzero because id and gm
    /// are interpolated geometrically, each on its own; the log reports it.
    pub gm_over_id_residual: f64,
}

/// Why a query has no answer (the SKILL mirror returns `nil` for every one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Miss {
    /// L not finite, not positive, or outside the L axis (no extrapolation).
    L,
    /// gm/Id not finite, not positive, or not reachable at that L.
    GmId,
    /// VDS not finite or outside the VDS axis.
    Vds,
    /// The drain current is not finite or not positive.
    Id,
}

impl Table {
    /// Builds a table from sweeps of one device at width `w` (m): `samples` are the absolute
    /// `[id, gm, gds, cgg]` blocks in cell order; each value is stored as `|x| / w` (p-channel
    /// sweeps are magnitudes, as gmid-lut stores them).
    ///
    /// # Errors
    ///
    /// `w` not positive, or the result fails [`Table::check`].
    pub fn from_samples(
        vds_nom: f64,
        l: Vec<f64>,
        vds: Vec<f64>,
        vgs: Vec<f64>,
        w: f64,
        samples: [&[f64]; 4],
    ) -> Result<Table, &'static str> {
        if !(w.is_finite() && w > 0.0) {
            return Err("characterization width must be positive");
        }
        let per_w = |b: &[f64]| b.iter().map(|x| x.abs() / w).collect::<Vec<f64>>();
        let t = Table {
            vds_nom,
            l,
            vds,
            vgs,
            id_w: per_w(samples[0]),
            gm_w: per_w(samples[1]),
            gds_w: per_w(samples[2]),
            cgg_w: per_w(samples[3]),
        };
        t.check()?;
        debug_assert_eq!(t.id_w.len(), samples[0].len());
        Ok(t)
    }

    /// Number of grid cells (`l·vds·vgs`).
    pub fn cells(&self) -> usize {
        self.l.len() * self.vds.len() * self.vgs.len()
    }

    fn idx(&self, l: usize, d: usize, g: usize) -> usize {
        debug_assert!(l < self.l.len() && d < self.vds.len() && g < self.vgs.len());
        (l * self.vds.len() + d) * self.vgs.len() + g
    }

    /// Validates the invariants listed on [`Table`].
    ///
    /// # Errors
    ///
    /// A short reason for the first violated invariant.
    pub fn check(&self) -> Result<(), &'static str> {
        let ascending =
            |a: &[f64]| a.iter().all(|x| x.is_finite()) && a.windows(2).all(|w| w[0] < w[1]);
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

    /// The operating point where gm/Id equals `gm_id` at length `l` (m) and |VDS| `vds`
    /// (V; `None` = [`Table::vds_nom`]): gmid-lut's query (its README "Query semantics").
    /// The two VDS rows around `vds` are blended linearly; VGS is found scanning down from
    /// the highest VGS to the first point whose gm/Id reaches `gm_id` (dips crossed, cutoff
    /// ends the row); inside that VGS cell the quantities are interpolated geometrically;
    /// the two L rows around `l` are blended in log L (quantities geometrically, VGS
    /// linearly), a grid length (within 1e-9 relative) answering from its row alone.
    ///
    /// # Errors
    ///
    /// [`Miss::L`] outside the L axis (1e-9 relative slack), [`Miss::Vds`] outside the VDS
    /// axis (1 nV slack), [`Miss::GmId`] when `gm_id` is not reachable.
    ///
    /// Cost: O(VGS points) for each of ≤ 2 L rows.
    pub fn point(&self, gm_id: f64, l: f64, vds: Option<f64>) -> Result<Point, Miss> {
        debug_assert!(self.id_w.len() == self.cells() && self.cgg_w.len() == self.cells());
        if !(gm_id.is_finite() && gm_id > 0.0) {
            return Err(Miss::GmId);
        }
        if !(l.is_finite() && l > 0.0) {
            return Err(Miss::L);
        }
        let d = self.vds_bracket(vds.unwrap_or(self.vds_nom))?;
        let (first, last) = (self.l[0], self.l[self.l.len() - 1]);
        if l < first * (1.0 - 1e-9) || l > last * (1.0 + 1e-9) {
            return Err(Miss::L);
        }
        let l = l.clamp(first, last);
        let i1 = self.l.partition_point(|&x| x < l);
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

    /// VDS rows around `vds` and the blend factor between them.
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
        let at = |b: &[f64]| lerp(b[self.idx(li, d0, g)], b[self.idx(li, d1, g)], td);
        [
            at(&self.id_w),
            at(&self.gm_w),
            at(&self.gds_w),
            at(&self.cgg_w),
        ]
    }

    /// [`Table::point`] on one L row.
    fn row_point(&self, li: usize, target: f64, d: (usize, usize, f64)) -> Result<Point, Miss> {
        let ng = self.vgs.len();
        assert!(li < self.l.len() && ng >= 2);
        // Every value passed so far is below `target` (else the scan stopped there), so the
        // first point at or above it closes the first upward crossing, after any dip.
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
}

/// Solves one intent: the point at (`gm_id`, `l`, `vds`) and `W = id / jd`.
///
/// # Errors
///
/// As [`Table::point`], plus [`Miss::Id`] for a drain current that is not positive.
pub fn solve(t: &Table, gm_id: f64, l: f64, id: f64, vds: Option<f64>) -> Result<Solution, Miss> {
    if !(id.is_finite() && id > 0.0) {
        return Err(Miss::Id);
    }
    let p = t.point(gm_id, l, vds)?;
    let s = Solution {
        vgs: p.vgs,
        jd: p.jd,
        w: id / p.jd,
        gm_over_id_residual: p.gm_w / p.jd - gm_id,
    };
    debug_assert!(s.w > 0.0 && s.jd > 0.0);
    Ok(s)
}

/// `l` (m) rounded to the nearest multiple of `lgrid` (m), at least one grid step;
/// `lgrid <= 0` leaves `l` unchanged. The product `round(l / lgrid) · lgrid` is formed
/// exactly as the SKILL mirror forms it.
pub fn snap_l(l: f64, lgrid: f64) -> f64 {
    assert!(l.is_finite() && l > 0.0, "snap_l: L must be positive");
    if lgrid.is_nan() || lgrid <= 0.0 {
        return l;
    }
    let s = (l / lgrid).round().max(1.0) * lgrid;
    debug_assert!(s > 0.0);
    s
}

/// The finger count for total width `w` (m): `nf`, raised to `ceil(w / fingerw_max)` when a
/// finger would be wider than `fingerw_max` (§10.3; a 1e-9 relative slack keeps `4.0000001`
/// at 4). The per-finger width is `w / result`.
///
/// # Panics
///
/// `w` not positive, `nf` outside `1..=NF_MAX`, or a split beyond [`NF_MAX`] fingers.
pub fn fingers(w: f64, nf: u32, fingerw_max: Option<f64>) -> u32 {
    assert!(w.is_finite() && w > 0.0 && (1..=NF_MAX).contains(&nf));
    let Some(m) = fingerw_max.filter(|m| *m > 0.0) else {
        return nf;
    };
    let q = w / m * (1.0 - 1e-9);
    assert!(q <= f64::from(NF_MAX), "finger split beyond {NF_MAX}");
    let k = q.ceil() as u32;
    nf.max(k)
}

/// gm/Id of `[id, gm, gds, cgg]`; 0 off the usable branch (a zero or non-positive value).
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

/// Geometric interpolation of positive quantities; linear when a sign is off.
fn geo(a: f64, b: f64, t: f64) -> f64 {
    if a > 0.0 && b > 0.0 {
        a * (b / a).powf(t)
    } else {
        lerp(a, b, t)
    }
}

// ---------------------------------------------------------------------------------------
// Intents (export side, §10.1)
// ---------------------------------------------------------------------------------------

/// One instance sized by gm/Id: what export writes as an `("intent" …)` record.
#[derive(Clone, Debug, PartialEq)]
pub struct Intent {
    /// The instance's Schemify id.
    pub inst_id: String,
    /// The `dev` argument of the call, verbatim (the class name before `interop.map`).
    pub class: String,
    /// Target gm/Id (1/V).
    pub gmid: f64,
    /// Channel length (m).
    pub l: f64,
    /// Drain current (A).
    pub id: f64,
    /// |VDS| (V) for `gmid.w_at`; `None` for `gmid.w` (the target uses `vdd / 2`).
    pub vds: Option<f64>,
    /// The width the sample PDK gave (m), when the caller could evaluate the override.
    pub w_sample: Option<f64>,
    /// Finger count (override `nf`, any case; default 1).
    pub nf: u32,
}

impl Intent {
    /// The `("intent" ref class (("gmid" g) ("l" l) ("id" i) ["vds" v] ["w" w] ("nf" n)))`
    /// record (§4.2), numbers in canonical text (expr §7, shortest round trip); `vds` and `w`
    /// are left out when unknown.
    pub fn to_record(&self, r: i32) -> Record {
        assert!(r > 0, "intent ref must be positive");
        let num = |x: f64| {
            let mut s = String::new();
            schemify_expr::write_num(&mut s, x);
            s
        };
        let mut kv: Vec<(&str, String)> = vec![
            ("gmid", num(self.gmid)),
            ("l", num(self.l)),
            ("id", num(self.id)),
        ];
        if let Some(v) = self.vds {
            kv.push(("vds", num(v)));
        }
        if let Some(w) = self.w_sample {
            kv.push(("w", num(w)));
        }
        kv.push(("nf", self.nf.to_string()));
        let rec = Record::new(
            "intent",
            vec![
                sfyx::i(r),
                sfyx::s(&self.class),
                sfyx::pairs(kv.iter().map(|(k, v)| (*k, v.as_str()))),
            ],
        );
        debug_assert_eq!(rec.fields.len(), 3);
        rec
    }
}

/// Evaluates `src` in cell `ci`'s own scope for [`intents`] (the CLI's evaluator, §10.1):
/// identifiers resolve to the cell's vars, then its parameter defaults, recursively (at most 16
/// levels). A var with a D20 `cached` value (docs-page.md §6: a plugin-computed number, e.g.
/// `L1 "gmid.l(…)"`) stands for that value, as it does wherever the plugin is not loaded; any
/// other plugin call or anything else that does not evaluate gives `None`, so the sample width
/// is simply left out.
pub fn cell_value(d: &Design, ci: usize, src: &str) -> Option<f64> {
    fn go(c: &crate::design::Cell, src: &str, depth: u32) -> Option<f64> {
        if depth >= 16 {
            return None;
        }
        let p = Program::compile(src, Mode::Scalar).ok()?;
        if p.plugin_fns().len() > 0 {
            return None;
        }
        let lookup = |n: &str| -> Option<String> {
            c.records(RecordKind::Var)
                .find(|r| c.field(*r, 0) == n)
                .or_else(|| c.records(RecordKind::Param).find(|r| c.field(*r, 0) == n))
                .map(|r| c.field(r, 1).to_owned())
        };
        let cached = |n: &str| -> Option<f64> {
            let r = c.records(RecordKind::Var).find(|r| c.field(*r, 0) == n)?;
            c.attr(r, "cached")?
                .parse::<f64>()
                .ok()
                .filter(|x| x.is_finite())
        };
        let vals: Vec<Option<f64>> = p
            .idents()
            .map(|n| cached(n).or_else(|| lookup(n).and_then(|e| go(c, &e, depth + 1))))
            .collect();
        let mut inputs = Vec::new();
        let mut k = 0;
        p.bind(&mut inputs, |_| {
            let v = vals[k].map(schemify_expr::Value::Num);
            k += 1;
            v
        })
        .ok()?;
        match p
            .eval(&inputs, &mut schemify_expr::Scratch::default())
            .ok()?
        {
            schemify_expr::Value::Num(x) => Some(x),
            schemify_expr::Value::Int(i) => Some(i as f64),
            _ => None,
        }
    }
    go(&d.cells[ci], src, 0)
}

/// Finds the instances of cell `ci` sized by intent: a width override (param `w`, any case)
/// that is `gmid.w(dev, gm_id, l, id)` or `gmid.w_at(dev, gm_id, l, id, vds)`, directly or
/// through a chain of the cell's vars (`W W_in` with `var W_in "gmid.w(…)"`, the OTA idiom).
/// `dev` must be a string literal; every other argument, the override itself (the sample
/// width) and `nf` are evaluated by `values`, which the caller backs with an evaluator in
/// the instance's scope (host `eval` in the editor, `schemify_expr` + bench vars in the
/// CLI) and which returns `None` for anything it cannot evaluate.
///
/// Widths that never call `gmid.w`/`gmid.w_at` give nothing. A call that cannot become an
/// intent (inside a larger expression, wrong arity, non-literal `dev`, an argument `values`
/// cannot evaluate or that is not positive, a bad `nf`) gives a `needs_review:retarget`
/// note and no intent: export then writes the instance with its evaluated width.
///
/// # Panics
///
/// `ci` must be a cell of `d`.
pub fn intents(
    d: &Design,
    ci: usize,
    values: impl Fn(&str) -> Option<f64>,
) -> (Vec<Intent>, Notes) {
    let c = &d.cells[ci];
    let mut out = Vec::new();
    let mut notes = Notes::default();
    let vars: Vec<(&str, &str)> = c
        .records(RecordKind::Var)
        .map(|r| (c.field(r, 0), c.field(r, 1)))
        .collect();
    for r in c.records(RecordKind::Inst) {
        let id = c.field(r, 0);
        let attr_of = |key: &str| {
            c.doc
                .attrs(r)
                .find(|&a| c.doc.attr_key(a).eq_ignore_ascii_case(key))
                .and_then(|a| c.doc.attr_value(a, 0))
        };
        let Some(w_text) = attr_of("w") else { continue };
        // Follow `W W_in` through the cell's vars to the expression that calls gmid.w.
        let mut expr = w_text;
        for _ in 0..VAR_DEPTH_MAX {
            match vars.iter().find(|(n, _)| *n == expr.trim()) {
                Some((_, e)) => expr = e,
                None => break,
            }
        }
        let call = match gmid_call(expr) {
            Call::None => continue,
            Call::Bad(why) => {
                notes.review(
                    "retarget",
                    &c.path,
                    format!("inst {id}: width `{expr}`: {why}"),
                );
                continue;
            }
            Call::Ok(call) => call,
        };
        let mut bad = |why: String| notes.review("retarget", &c.path, format!("inst {id}: {why}"));
        let mut nums = [0.0f64; 4];
        let mut ok = true;
        for (k, slot) in nums.iter_mut().enumerate().take(call.args.len() - 1) {
            let a = call.args[k + 1];
            match values(a).filter(|v| v.is_finite() && *v > 0.0) {
                Some(v) => *slot = v,
                None => {
                    bad(format!(
                        "gmid argument `{a}` is not a positive number in scope"
                    ));
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        let nf = match attr_of("nf") {
            None => 1,
            Some(t) => match values(t) {
                Some(v) if v.fract() == 0.0 && v >= 1.0 && v <= f64::from(NF_MAX) => v as u32,
                _ => {
                    bad(format!("nf `{t}` is not an integer in 1..={NF_MAX}"));
                    continue;
                }
            },
        };
        out.push(Intent {
            inst_id: id.to_owned(),
            class: call.dev.into_owned(),
            gmid: nums[0],
            l: nums[1],
            id: nums[2],
            vds: (call.args.len() == 5).then_some(nums[3]),
            w_sample: values(w_text).filter(|v| v.is_finite() && *v > 0.0),
            nf,
        });
    }
    debug_assert!(out.iter().all(|i| i.gmid > 0.0 && i.l > 0.0 && i.id > 0.0));
    (out, notes)
}

/// A parsed `gmid.w`/`gmid.w_at` call: the unescaped `dev` and every argument's text.
struct GmidCall<'a> {
    dev: Cow<'a, str>,
    args: Vec<&'a str>,
}

enum Call<'a> {
    /// The expression does not call `gmid.w`/`gmid.w_at`.
    None,
    /// It does, but not in a form an intent can carry.
    Bad(&'static str),
    Ok(GmidCall<'a>),
}

/// Recognizes an expression that is exactly one `gmid.w(…)`/`gmid.w_at(…)` call.
fn gmid_call(expr: &str) -> Call<'_> {
    let Ok(p) = Program::compile(expr, Mode::Scalar) else {
        return Call::None; // not an expression: the format checker reports it, not us
    };
    if !p
        .plugin_fns()
        .any(|(ns, f)| ns == "gmid" && (f == "w" || f == "w_at"))
    {
        return Call::None;
    }
    let e = expr.trim();
    let (arity, body) = if let Some(b) = e.strip_prefix("gmid.w_at(") {
        (5, b)
    } else if let Some(b) = e.strip_prefix("gmid.w(") {
        (4, b)
    } else {
        return Call::Bad("gmid.w inside a larger expression (the whole width must be the call)");
    };
    let Some(args) = split_args(body) else {
        return Call::Bad("gmid.w inside a larger expression (the whole width must be the call)");
    };
    if args.len() != arity {
        return Call::Bad("wrong number of arguments");
    }
    let Some(dev) = string_literal(args[0]) else {
        return Call::Bad("the device argument must be a string literal");
    };
    debug_assert!(args.iter().all(|a| !a.is_empty()));
    Call::Ok(GmidCall { dev, args })
}

/// Splits `a, b, c)` (the text after a call's `(`) at top-level commas; `None` unless the
/// matching `)` is the last byte.
fn split_args(body: &str) -> Option<Vec<&str>> {
    let b = body.as_bytes();
    let (mut depth, mut start, mut i) = (0u32, 0usize, 0usize);
    let mut args = Vec::new();
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'(' => depth += 1,
            b')' if depth > 0 => depth -= 1,
            b')' => {
                args.push(body[start..i].trim());
                return (i + 1 == b.len() && args.iter().all(|a| !a.is_empty())).then_some(args);
            }
            b',' if depth == 0 => {
                args.push(body[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The value of a string literal with expr §2 escapes `\" \\ \n \t`; `None` for anything
/// else (including `\u{…}`, which no class name needs).
fn string_literal(s: &str) -> Option<Cow<'_, str>> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    if !inner.contains('\\') {
        return (!inner.contains('"')).then_some(Cow::Borrowed(inner));
    }
    let mut out = String::with_capacity(inner.len());
    let mut it = inner.chars();
    while let Some(ch) = it.next() {
        match ch {
            '\\' => out.push(match it.next()? {
                '"' => '"',
                '\\' => '\\',
                'n' => '\n',
                't' => '\t',
                _ => return None,
            }),
            '"' => return None,
            c => out.push(c),
        }
    }
    Some(Cow::Owned(out))
}

// ---------------------------------------------------------------------------------------
// Target map (target machine, §10.2)
// ---------------------------------------------------------------------------------------

/// Channel type of a target class: `p` characterizes with negative VGS/VDS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polarity {
    /// n-channel (default).
    N,
    /// p-channel.
    P,
}

/// What the target's width parameter means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidthMode {
    /// The width parameter is one finger's width (`W / nf`); default.
    Finger,
    /// The width parameter is the total width `W`.
    Total,
}

/// One `class` of a `.retarget` file.
#[derive(Clone, Debug, PartialEq)]
pub struct TargetClass {
    /// Class name (what intents name).
    pub name: String,
    /// Target library.
    pub lib: String,
    /// Target cell.
    pub cell: String,
    /// `(class pin, target terminal)`.
    pub pins: Vec<(String, String)>,
    /// `(w|l|nf, target CDF parameter)`; `w` and `l` always present.
    pub params: Vec<(String, String)>,
    /// The Spectre model line (`include … section=…`), verbatim.
    pub model: String,
    /// The Spectre instance line on nodes `d g s b`, without width, length or fingers.
    pub device: String,
    /// Operating-point prefix (`M0`, or `M0.main` for a subcircuit macro); default the
    /// first word of `device`.
    pub op: String,
    /// Supply (V): the VGS sweep spans `[0, vdd]`, the default VDS is `vdd / 2`.
    pub vdd: f64,
    /// L grid (m); `None` = no snapping.
    pub lgrid: Option<f64>,
    /// Widest finger (m); `None` = never split.
    pub fingerw_max: Option<f64>,
    /// Channel type.
    pub polarity: Polarity,
    /// Meaning of the width parameter.
    pub width: WidthMode,
}

/// Parses a `.retarget` target map (§10.2): `class <name> lib <lib> cell <cell>` records
/// with indented attributes, `#` comments and blank lines.
///
/// # Errors
///
/// Every problem found, one per entry, as `line N: <reason>`: malformed lines, unknown
/// records or attributes, a repeated single attribute or class, a missing `model`,
/// `device`, `vdd`, `param w` or `param l`, a number that is not
/// `digits[.digits][e±digits][f|p|n|u|m|k|meg|g|t]` and positive, more than
/// [`CLASSES_MAX`] classes.
pub fn parse_target_map(src: &str) -> Result<Vec<TargetClass>, Vec<String>> {
    let mut out: Vec<TargetClass> = Vec::new();
    let mut errs: Vec<String> = Vec::new();
    // (class under construction, its first line) — attributes go to it.
    let mut cur: Option<(TargetClass, usize)> = None;
    let finish =
        |c: Option<(TargetClass, usize)>, out: &mut Vec<TargetClass>, errs: &mut Vec<String>| {
            let Some((c, at)) = c else { return };
            let mut miss =
                |what: &str| errs.push(format!("line {at}: class {}: missing {what}", c.name));
            if c.model.is_empty() {
                miss("model");
            }
            if c.device.is_empty() {
                miss("device");
            }
            if c.vdd <= 0.0 {
                miss("vdd");
            }
            for p in ["w", "l"] {
                if !c.params.iter().any(|(s, _)| s == p) {
                    miss(&format!("param {p}"));
                }
            }
            if out.iter().any(|o| o.name == c.name) {
                errs.push(format!("line {at}: class {} defined twice", c.name));
            } else if out.len() >= CLASSES_MAX {
                errs.push(format!("line {at}: more than {CLASSES_MAX} classes"));
            } else {
                out.push(c);
            }
        };
    for (k, raw) in src.split('\n').enumerate() {
        let n = k + 1;
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let at_col0 = !line.starts_with([' ', '\t']);
        let trimmed = line.trim_start_matches([' ', '\t']);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut err = |why: String| errs.push(format!("line {n}: {why}"));
        if at_col0 {
            finish(cur.take(), &mut out, &mut errs);
            let mut err = |why: String| errs.push(format!("line {n}: {why}"));
            let Ok((word, rest)) = text::split_word(line) else {
                err("malformed record".into());
                continue;
            };
            if word != "class" {
                err(format!("unknown record `{word}` (only `class`)"));
                continue;
            }
            let vals: Result<Vec<Cow<str>>, _> = text::values(rest).collect();
            match vals.as_deref() {
                Ok([name, kl, lib, kc, cell])
                    if kl == "lib" && kc == "cell" && text::is_word(name) =>
                {
                    cur = Some((
                        TargetClass {
                            name: name.to_string(),
                            lib: lib.to_string(),
                            cell: cell.to_string(),
                            pins: Vec::new(),
                            params: Vec::new(),
                            model: String::new(),
                            device: String::new(),
                            op: String::new(),
                            vdd: 0.0,
                            lgrid: None,
                            fingerw_max: None,
                            polarity: Polarity::N,
                            width: WidthMode::Finger,
                        },
                        n,
                    ));
                }
                _ => err("expected `class <name> lib <lib> cell <cell>`".into()),
            }
            continue;
        }
        let Some((c, _)) = cur.as_mut() else {
            err("attribute outside a class".into());
            continue;
        };
        let (key, rest) = match text::body_line(line) {
            Ok(BodyLine::Attr(k, r)) => (k, r),
            _ => {
                err("malformed attribute line".into());
                continue;
            }
        };
        let vals: Vec<Cow<str>> = match text::values(rest).collect() {
            Ok(v) => v,
            Err(_) => {
                err(format!("`{key}`: malformed value"));
                continue;
            }
        };
        let one = |v: &[Cow<str>]| match v {
            [x] => Ok(x.to_string()),
            _ => Err(format!("`{key}` takes one value")),
        };
        let num = |v: &[Cow<str>]| {
            one(v)
                .and_then(|s| map_num(&s).ok_or(format!("`{key}`: `{s}` is not a positive number")))
        };
        let r: Result<(), String> = match key {
            "pin" | "param" => match vals.as_slice() {
                [a, b] => {
                    let list = if key == "pin" {
                        &mut c.pins
                    } else {
                        &mut c.params
                    };
                    if key == "param" && !matches!(a.as_ref(), "w" | "l" | "nf") {
                        Err(format!("param `{a}`: only w, l and nf are set"))
                    } else if list.iter().any(|(s, _)| s == a) {
                        Err(format!("{key} `{a}` given twice"))
                    } else {
                        list.push((a.to_string(), b.to_string()));
                        Ok(())
                    }
                }
                _ => Err(format!("`{key}` takes two values")),
            },
            "model" | "device" | "op" => one(&vals).and_then(|s| {
                let slot = match key {
                    "model" => &mut c.model,
                    "device" => &mut c.device,
                    _ => &mut c.op,
                };
                if !slot.is_empty() {
                    return Err(format!("`{key}` given twice"));
                }
                if s.trim().is_empty() {
                    return Err(format!("`{key}` is empty"));
                }
                *slot = s;
                Ok(())
            }),
            "vdd" => num(&vals).and_then(|v| {
                if c.vdd > 0.0 {
                    return Err("`vdd` given twice".into());
                }
                c.vdd = v;
                Ok(())
            }),
            "lgrid" | "fingerw_max" => num(&vals).and_then(|v| {
                let slot = if key == "lgrid" {
                    &mut c.lgrid
                } else {
                    &mut c.fingerw_max
                };
                if slot.replace(v).is_some() {
                    return Err(format!("`{key}` given twice"));
                }
                Ok(())
            }),
            "polarity" => one(&vals).and_then(|s| {
                c.polarity = match s.as_str() {
                    "n" => Polarity::N,
                    "p" => Polarity::P,
                    _ => return Err("`polarity` is n or p".into()),
                };
                Ok(())
            }),
            "width" => one(&vals).and_then(|s| {
                c.width = match s.as_str() {
                    "finger" => WidthMode::Finger,
                    "total" => WidthMode::Total,
                    _ => return Err("`width` is finger or total".into()),
                };
                Ok(())
            }),
            _ => Err(format!("unknown attribute `{key}`")),
        };
        if let Err(e) = r {
            err(e);
        }
    }
    finish(cur.take(), &mut out, &mut errs);
    for c in &mut out {
        if c.op.is_empty() {
            c.op = c.device.split_whitespace().next().unwrap_or("").to_owned();
        }
    }
    if errs.is_empty() {
        debug_assert!(out.len() <= CLASSES_MAX && out.iter().all(|c| !c.op.is_empty()));
        Ok(out)
    } else {
        Err(errs)
    }
}

/// A target-map number: a C decimal times one suffix factor (`f p n u m k meg g t`, any
/// case), formed as `mantissa · factor` exactly as the SKILL reader (`SfyRtNum`) forms it;
/// `None` unless it is that and positive and finite.
pub fn map_num(s: &str) -> Option<f64> {
    const SUFFIX: [(&str, f64); 9] = [
        ("meg", 1e6),
        ("f", 1e-15),
        ("p", 1e-12),
        ("n", 1e-9),
        ("u", 1e-6),
        ("m", 1e-3),
        ("k", 1e3),
        ("g", 1e9),
        ("t", 1e12),
    ];
    let low = s.to_ascii_lowercase();
    let (body, scale) = SUFFIX
        .iter()
        .find_map(|(suf, f)| low.strip_suffix(suf).map(|b| (b, *f)))
        .unwrap_or((low.as_str(), 1.0));
    // A C decimal: what strtod reads, without hex, inf, nan or a sign.
    let ok = !body.is_empty()
        && body
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'-' | b'+'))
        && body.as_bytes()[0].is_ascii_digit();
    let v: f64 = if ok { body.parse().ok()? } else { return None };
    let v = v * scale;
    (v.is_finite() && v > 0.0).then_some(v)
}
