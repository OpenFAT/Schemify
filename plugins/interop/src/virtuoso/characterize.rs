//! Characterization files (spec `interop.md` §8.3): what a PDK device's CDF callbacks derive
//! from the parameters a user sets, measured in Virtuoso by `SfyCharacterize`. Callbacks are
//! SKILL and never cross to Schemify; this file is the measured behaviour the PDK mirror's
//! rules are built and verified against. It names a PDK's parameters and values, so it stays
//! on the machine it was made on.

use schemify_format::{Document, RecordKind, parse, write};

use crate::design::{copy_attr, copy_header, push_attr};
use crate::sfyx::{self, Record, Value};

/// Most points one device may have (`SfyCharacterize` writes at most 4024).
pub const POINTS_MAX: usize = 10_000;

/// What an input is, as measured (its CDF type and default), never its name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    /// A continuous number.
    #[default]
    Num,
    /// A whole number (a finger or segment count).
    Count,
    /// A boolean (`"t"` or `"nil"`).
    Bool,
    /// One of the CDF's choices.
    Choice,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "num" => Kind::Num,
            "count" => Kind::Count,
            "bool" => Kind::Bool,
            "choice" => Kind::Choice,
            _ => return None,
        })
    }
}

/// One characterized device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    /// Library and cell.
    pub lib: String,
    /// See [`Device::lib`].
    pub cell: String,
    /// The inputs the sweep set, in the order it set them: the candidates whose nudge
    /// changed another netlisted param, or those `?inputs` named.
    pub inputs: Vec<String>,
    /// Kind per input, parallel to [`Device::inputs`].
    pub kinds: Vec<Kind>,
    /// Default value text per input, parallel to [`Device::inputs`].
    pub defaults: Vec<String>,
    /// The netlisted parameters (spectre `instParameters`), in CDF order.
    pub outputs: Vec<String>,
    /// The discovery probes, one per candidate, in CDF order.
    pub probes: Vec<Probe>,
    /// The points, in file order.
    pub points: Vec<Point>,
}

/// One discovery probe: a candidate nudged from the defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    /// The candidate.
    pub name: String,
    /// Its kind.
    pub kind: Kind,
    /// The value it was set to.
    pub set: String,
    /// Whether every callback ran.
    pub status: Result<(), String>,
    /// The netlisted params (other than itself) the nudge changed.
    pub changed: Vec<String>,
}

/// One measured point.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    /// The value set per input (`""`: not set at this point), parallel to [`Device::inputs`].
    pub set: Vec<String>,
    /// Whether every callback ran; else the reason, as SfySetParams gave it.
    pub status: Result<(), String>,
    /// The value read back per netlisted parameter, parallel to [`Device::outputs`].
    pub values: Vec<String>,
    /// The value read back per input, parallel to [`Device::inputs`] (a callback may
    /// rewrite what was set).
    pub back: Vec<String>,
}

fn status_of(s: &str) -> Result<(), String> {
    match s {
        "ok" => Ok(()),
        why => Err(why.to_owned()),
    }
}

/// How each netlisted parameter behaved over a device's successful points.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Points whose callbacks all ran.
    pub ok: usize,
    /// Points where a callback failed.
    pub failed: usize,
    /// Parameters that are inputs themselves (they may still be rewritten by a callback).
    pub inputs: Vec<String>,
    /// Parameters that change with the inputs: the callback-derived ones.
    pub derived: Vec<String>,
    /// Parameters that kept one value.
    pub constant: Vec<String>,
}

fn strs(r: &Record, v: &[Value], what: &str) -> Result<Vec<String>, String> {
    v.iter()
        .map(|x| match x {
            Value::Str(s) => Ok(s.clone()),
            _ => Err(format!("line {}: {what}: not a string", r.line)),
        })
        .collect()
}

/// Reads a characterization file.
///
/// # Errors
///
/// A record that is not `char`/`probe`/`pt` between the `sfyx` header and `end`, a `probe`
/// or `pt` before any `char`, lists that do not match the `char`'s, an unknown kind, or
/// more than [`POINTS_MAX`] points for one device.
pub fn read(text: &str) -> Result<Vec<Device>, String> {
    let recs = sfyx::read(text).map_err(|e| format!("line {}: {}", e.line, e.msg))?;
    let mut out: Vec<Device> = Vec::new();
    for r in &recs {
        match r.tag.as_str() {
            "sfyx" | "end" => {}
            "char" => {
                r.arity(6).map_err(|e| e.msg)?;
                let inputs = strs(r, r.list(2).map_err(|e| e.msg)?, "inputs")?;
                let kinds = strs(r, r.list(4).map_err(|e| e.msg)?, "kinds")?
                    .iter()
                    .map(|k| Kind::parse(k).ok_or_else(|| format!("line {}: kind `{k}`", r.line)))
                    .collect::<Result<Vec<_>, _>>()?;
                let defaults = strs(r, r.list(5).map_err(|e| e.msg)?, "defaults")?;
                if kinds.len() != inputs.len() || defaults.len() != inputs.len() {
                    return Err(format!("line {}: kinds and defaults per input", r.line));
                }
                out.push(Device {
                    lib: r.str(0).map_err(|e| e.msg)?.to_owned(),
                    cell: r.str(1).map_err(|e| e.msg)?.to_owned(),
                    inputs,
                    kinds,
                    defaults,
                    outputs: strs(r, r.list(3).map_err(|e| e.msg)?, "netlisted")?,
                    probes: Vec::new(),
                    points: Vec::new(),
                });
            }
            "probe" => {
                r.arity(5).map_err(|e| e.msg)?;
                let d = out
                    .last_mut()
                    .ok_or_else(|| format!("line {}: a probe before any `char`", r.line))?;
                let kind = r.str(1).map_err(|e| e.msg)?;
                d.probes.push(Probe {
                    name: r.str(0).map_err(|e| e.msg)?.to_owned(),
                    kind: Kind::parse(kind)
                        .ok_or_else(|| format!("line {}: kind `{kind}`", r.line))?,
                    set: r.str(2).map_err(|e| e.msg)?.to_owned(),
                    status: status_of(r.str(3).map_err(|e| e.msg)?),
                    changed: strs(r, r.list(4).map_err(|e| e.msg)?, "changed")?,
                });
            }
            "pt" => {
                r.arity(4).map_err(|e| e.msg)?;
                let d = out
                    .last_mut()
                    .ok_or_else(|| format!("line {}: a point before any `char`", r.line))?;
                let set = strs(r, r.list(0).map_err(|e| e.msg)?, "set")?;
                let values = strs(r, r.list(2).map_err(|e| e.msg)?, "values")?;
                let back = strs(r, r.list(3).map_err(|e| e.msg)?, "read back")?;
                if set.len() != d.inputs.len()
                    || back.len() != d.inputs.len()
                    || values.len() != d.outputs.len()
                {
                    return Err(format!(
                        "line {}: {} set and {} values for {} inputs and {} parameters",
                        r.line,
                        set.len(),
                        values.len(),
                        d.inputs.len(),
                        d.outputs.len()
                    ));
                }
                if d.points.len() == POINTS_MAX {
                    return Err(format!("line {}: more than {POINTS_MAX} points", r.line));
                }
                let status = status_of(r.str(1).map_err(|e| e.msg)?);
                d.points.push(Point {
                    set,
                    status,
                    values,
                    back,
                });
            }
            t => return Err(format!("line {}: unknown record `{t}`", r.line)),
        }
    }
    debug_assert!(out.iter().all(|d| d.points.len() <= POINTS_MAX));
    Ok(out)
}

impl Device {
    /// The value input `k` was set to at `p`: its default where the point left it.
    pub fn set_value<'a>(&'a self, p: &'a Point, k: usize) -> &'a str {
        if p.set[k].is_empty() {
            self.defaults.get(k).map_or("", String::as_str)
        } else {
            &p.set[k]
        }
    }

    /// Each netlisted parameter classified over the points whose callbacks ran.
    pub fn summary(&self) -> Summary {
        let ok: Vec<&Point> = self.points.iter().filter(|p| p.status.is_ok()).collect();
        let mut s = Summary {
            ok: ok.len(),
            failed: self.points.len() - ok.len(),
            ..Summary::default()
        };
        for (k, name) in self.outputs.iter().enumerate() {
            let first = ok.first().map(|p| &p.values[k]);
            if ok.iter().all(|p| Some(&p.values[k]) == first) {
                s.constant.push(name.clone());
            } else if self.inputs.contains(name) {
                s.inputs.push(name.clone());
            } else {
                s.derived.push(name.clone());
            }
        }
        debug_assert_eq!(
            s.inputs.len() + s.derived.len() + s.constant.len(),
            self.outputs.len()
        );
        s
    }
}

/// A CDF number as Virtuoso prints it (`6.72f`, `80n`, `1.2u`, `2`): AEL scale factors (`M`
/// is mega), `None` for anything else (`iPar("m")`, `""`).
pub fn ael_num(s: &str) -> Option<f64> {
    let s = s.trim();
    let (body, scale) = match s.chars().last()? {
        c if c.is_ascii_digit() || c == '.' => (s, 1.0),
        c => (
            &s[..s.len() - c.len_utf8()],
            match c {
                'T' => 1e12,
                'G' => 1e9,
                'M' => 1e6,
                'K' | 'k' => 1e3,
                'm' => 1e-3,
                'u' => 1e-6,
                'n' => 1e-9,
                'p' => 1e-12,
                'f' => 1e-15,
                'a' => 1e-18,
                _ => return None,
            },
        ),
    };
    let x: f64 = body.parse().ok()?;
    (x * scale).is_finite().then_some(x * scale)
}

/// Tightest relative tolerance any printed value gets: the floor of [`printed_tol`].
pub const TOL_FLOOR: f64 = 1e-6;

/// Most breakpoints a step table may have; a rule needing more explains nothing.
pub const BREAKS_MAX: usize = 64;

/// A printed number and how far it may be from the value it was printed from: half a unit
/// in its last printed digit (`6.72f` is good to ±0.005f, `3` to ±0.5), at least
/// [`TOL_FLOOR`] of it. `None` when the text is not a number.
pub fn printed_tol(text: &str) -> Option<(f64, f64)> {
    let v = ael_num(text)?;
    let t = text.trim();
    let t = t.strip_prefix(['-', '+']).unwrap_or(t);
    let mant = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let decimals = t[..mant].split_once('.').map_or(0, |(_, f)| f.len());
    // The scale of the exponent or suffix after the mantissa: `1` followed by it.
    let scale = ael_num(&format!("1{}", &t[mant..]))?;
    let half = 0.5 * 10f64.powi(-i32::try_from(decimals).ok()?) * scale;
    let tol = half.max(TOL_FLOOR * v.abs());
    debug_assert!(tol > 0.0 && tol.is_finite());
    Some((v, tol))
}

/// The number in `[lo, hi]` with the fewest significant digits, the one nearest `aim`
/// when several have as few: the value a table or a breakpoint was most likely written
/// with (a value aims at the middle of what its tolerances allow, a breakpoint at the
/// first value measured past it).
pub fn nicest(lo: f64, hi: f64, aim: f64) -> f64 {
    debug_assert!(lo <= hi && lo <= aim && aim <= hi, "{lo} {aim} {hi}");
    if lo <= 0.0 && 0.0 <= hi {
        return 0.0;
    }
    if hi < 0.0 {
        return -nicest(-hi, -lo, -aim);
    }
    let top = hi.log10().floor() as i32;
    for d in 1..=17 {
        let e = top - d + 1;
        let k = aim / 10f64.powi(e);
        // Through text, so the result is the decimal itself (no binary noise).
        let at = |k: f64| -> f64 { format!("{k}e{e}").parse().unwrap_or(f64::NAN) };
        let (a, b) = (at(k.floor()), at(k.ceil()));
        let pick = [a, b]
            .into_iter()
            .filter(|c| *c >= lo && *c <= hi)
            .min_by(|x, y| (x - aim).abs().total_cmp(&(y - aim).abs()));
        if let Some(c) = pick {
            return c;
        }
    }
    aim
}

/// One factor of a monomial over a numeric input `n`: the count shapes are the source and
/// drain shares of a multi-finger device (`⌊n/2⌋+1`, `⌈n/2⌉`) and those per finger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fac {
    Id,
    Inv,
    Odd,
    Even,
    OddPer,
    EvenPer,
}

const NUM_FACS: [Fac; 2] = [Fac::Id, Fac::Inv];
const COUNT_FACS: [Fac; 6] = [
    Fac::Id,
    Fac::Inv,
    Fac::Odd,
    Fac::Even,
    Fac::OddPer,
    Fac::EvenPer,
];

impl Fac {
    fn eval(self, n: f64) -> f64 {
        let (odd, even) = ((n / 2.0).floor() + 1.0, (n / 2.0).ceil());
        match self {
            Fac::Id => n,
            Fac::Inv => 1.0 / n,
            Fac::Odd => odd,
            Fac::Even => even,
            Fac::OddPer => odd / n,
            Fac::EvenPer => even / n,
        }
    }

    /// The factor appended to a product (`first`: it starts the product).
    fn push(self, out: &mut String, n: &str, first: bool) {
        let star = if first { "" } else { "*" };
        let s = match self {
            Fac::Id => format!("{star}{n}"),
            Fac::Inv if first => format!("1/{n}"),
            Fac::Inv => format!("/{n}"),
            Fac::Odd => format!("{star}(floor({n}/2)+1)"),
            Fac::Even => format!("{star}ceil({n}/2)"),
            Fac::OddPer => format!("{star}(floor({n}/2)+1)/{n}"),
            Fac::EvenPer => format!("{star}ceil({n}/2)/{n}"),
        };
        out.push_str(&s);
    }
}

/// A product of factors over at most two distinct inputs (empty: 1).
type Mono = Vec<(usize, Fac)>;

/// One rule body: `m · (alpha·u + T(x))`, `T` a step table over input `x` (`None`: one
/// value).
#[derive(Clone, Debug, PartialEq)]
struct Body {
    m: Mono,
    u: Option<(Mono, f64)>,
    x: Option<usize>,
    /// (from, value) per step, increasing; one entry when `x` is `None`.
    steps: Vec<(f64, f64)>,
    /// The table's last breakpoint: the largest `x` measured.
    end: f64,
}

/// One derived parameter's rule: a body, or a body per value of a boolean or choice input.
#[derive(Clone, Debug, PartialEq)]
enum Rule {
    One(Body),
    Mux(usize, Vec<(String, Body)>),
}

/// The measured coordinates rules are fitted over: per input, per successful point, the
/// value read back (what the device holds, after its callbacks) and the value set.
struct Grid<'a> {
    dev: &'a Device,
    /// Indices into `dev.points` of the points whose callbacks ran.
    ok: Vec<usize>,
    /// Per input, per ok point: the number read back (NaN for a boolean or choice).
    back: Vec<Vec<f64>>,
    /// Per input, per ok point: the number set (NaN for a boolean or choice).
    set: Vec<Vec<f64>>,
}

impl<'a> Grid<'a> {
    fn new(dev: &'a Device) -> Option<Grid<'a>> {
        let ok: Vec<usize> = (0..dev.points.len())
            .filter(|&i| dev.points[i].status.is_ok())
            .collect();
        let mut back = Vec::new();
        let mut set = Vec::new();
        for (k, kind) in dev.kinds.iter().enumerate() {
            let numeric = matches!(kind, Kind::Num | Kind::Count);
            let col = |was_set: bool| -> Option<Vec<f64>> {
                ok.iter()
                    .map(|&i| {
                        let p = &dev.points[i];
                        match (numeric, was_set) {
                            (false, _) => Some(f64::NAN),
                            (true, true) => ael_num(dev.set_value(p, k)),
                            (true, false) => ael_num(&p.back[k]),
                        }
                    })
                    .collect()
            };
            back.push(col(false)?);
            set.push(col(true)?);
        }
        Some(Grid { dev, ok, back, set })
    }

    fn numeric(&self) -> Vec<usize> {
        (0..self.dev.inputs.len())
            .filter(|&k| matches!(self.dev.kinds[k], Kind::Num | Kind::Count))
            .collect()
    }

    fn mono(&self, m: &Mono, i: usize) -> f64 {
        m.iter().map(|&(k, f)| f.eval(self.back[k][i])).product()
    }

    /// Fits `m · (alpha·u + T(x))` to `y` (value, tolerance per ok point) over the ok points
    /// `idx`; `alpha` and the table come from the data, each value the nicest the
    /// tolerances allow.
    fn fit(
        &self,
        idx: &[usize],
        y: &[(f64, f64)],
        m: &Mono,
        u: Option<&Mono>,
        x: Option<usize>,
    ) -> Option<Body> {
        // z = y/m within zt, per point.
        let mut z: Vec<(f64, f64)> = Vec::with_capacity(idx.len());
        for &i in idx {
            let mv = self.mono(m, i);
            if !mv.is_finite() || mv == 0.0 {
                return None;
            }
            z.push((y[i].0 / mv, y[i].1 / mv.abs()));
        }
        let mut alpha = 0.0;
        if let Some(u) = u {
            let uv: Vec<f64> = idx.iter().map(|&i| self.mono(u, i)).collect();
            if uv.iter().any(|v| !v.is_finite()) {
                return None;
            }
            // alpha: what every point at the first point's x allows, against the first.
            let same = |j: usize| x.is_none_or(|k| self.back[k][idx[j]] == self.back[k][idx[0]]);
            let (mut lo, mut hi) = (f64::NEG_INFINITY, f64::INFINITY);
            for j in (1..idx.len()).filter(|&j| same(j)) {
                let du = uv[j] - uv[0];
                if du.abs() <= 1e-9 * uv[0].abs().max(uv[j].abs()) {
                    continue;
                }
                let (a, at) = ((z[j].0 - z[0].0) / du, (z[j].1 + z[0].1) / du.abs());
                lo = lo.max(a - at);
                hi = hi.min(a + at);
            }
            if !(lo.is_finite() && hi.is_finite() && lo <= hi) {
                return None;
            }
            let a = (lo + hi) / 2.0;
            let at = (hi - lo) / 2.0;
            alpha = nicest(a - at, a + at, a);
            if alpha == 0.0 {
                return None;
            }
            for (k, zz) in z.iter_mut().enumerate() {
                zz.0 -= alpha * uv[k];
            }
        }
        let (steps, end) = self.table(idx, &z, x)?;
        if x.is_some() && steps.len() < 2 {
            return None; // one value: the form without the table is simpler
        }
        Some(Body {
            m: m.clone(),
            u: u.map(|u| (u.clone(), alpha)),
            x,
            steps,
            end,
        })
    }

    /// A step table over input `x` (one value when `None`) through every `(z, tol)`, with as
    /// few steps as the tolerances allow; `None` when the points disagree at one `x`, a
    /// step change is not bracketed tightly (the scan's neighbours, not a bisection: the
    /// value varies continuously there), or it needs more than [`BREAKS_MAX`] breakpoints.
    fn table(
        &self,
        idx: &[usize],
        z: &[(f64, f64)],
        x: Option<usize>,
    ) -> Option<(Vec<(f64, f64)>, f64)> {
        let xv = |j: usize| x.map_or(0.0, |k| self.back[k][idx[j]]);
        let mut order: Vec<usize> = (0..idx.len()).collect();
        order.sort_by(|&a, &b| xv(a).total_cmp(&xv(b)));
        // Segments: (first x, last x, lo, hi of the value).
        let mut segs: Vec<(f64, f64, f64, f64)> = Vec::new();
        for &j in &order {
            let (lo, hi) = (z[j].0 - z[j].1, z[j].0 + z[j].1);
            match segs.last_mut() {
                Some(s) if lo <= s.3 && hi >= s.2 => {
                    s.1 = xv(j);
                    s.2 = s.2.max(lo);
                    s.3 = s.3.min(hi);
                }
                Some(s) if s.1 == xv(j) => return None,
                _ => {
                    if segs.len() > BREAKS_MAX {
                        return None;
                    }
                    segs.push((xv(j), xv(j), lo, hi));
                }
            }
        }
        let first = segs.first()?.0;
        let mid = |s: &(f64, f64, f64, f64)| nicest(s.2, s.3, (s.2 + s.3) / 2.0);
        let mut steps = vec![(first, mid(&segs[0]))];
        for w in segs.windows(2) {
            let (lo, hi) = (w[0].1, w[1].0);
            let k = x.expect("a single segment without x");
            if !self.tight(k, lo, hi) {
                return None;
            }
            let b = nicest(lo + (hi - lo) * 1e-6, hi, hi);
            steps.push((b, mid(&w[1])));
        }
        let end = segs.last()?.1;
        debug_assert!(steps.windows(2).all(|w| w[0].0 < w[1].0));
        Some((steps, end))
    }

    /// Whether a step between read-back values `lo` and `hi` of input `k` is located: the
    /// two are next whole numbers (a count), within 1e-4 of each other, or two points set
    /// within 1e-4 of each other read back on either side (a rewrite that jumps over the
    /// values between).
    fn tight(&self, k: usize, lo: f64, hi: f64) -> bool {
        let near = |a: f64, b: f64| (a - b).abs() <= 1e-4 * a.abs().max(b.abs());
        if near(lo, hi) || (self.dev.kinds[k] == Kind::Count && hi - lo <= 1.0) {
            return true;
        }
        let mut by_set: Vec<usize> = (0..self.ok.len()).collect();
        by_set.sort_by(|&a, &b| self.set[k][a].total_cmp(&self.set[k][b]));
        by_set.windows(2).any(|w| {
            let (a, b) = (w[0], w[1]);
            near(self.set[k][a], self.set[k][b]) && self.back[k][a] <= lo && self.back[k][b] >= hi
        })
    }

    /// The simplest body that fits `y` over `idx`, trying in order: one value, a step
    /// table, a table times a monomial, an affine form; `skip` is an input left out.
    fn simplest(&self, idx: &[usize], y: &[(f64, f64)], skip: Option<usize>) -> Option<Body> {
        let nums: Vec<usize> = self
            .numeric()
            .into_iter()
            .filter(|&k| Some(k) != skip)
            .collect();
        let xs: Vec<Option<usize>> = std::iter::once(None)
            .chain(nums.iter().map(|&k| Some(k)))
            .collect();
        let none: Mono = Vec::new();
        for &x in &xs {
            if let Some(b) = self.fit(idx, y, &none, None, x) {
                return Some(b);
            }
        }
        for size in 1..=2 {
            for &x in &xs {
                let avail: Vec<usize> = nums.iter().copied().filter(|&k| Some(k) != x).collect();
                for m in self.monos(&avail, size) {
                    if let Some(b) = self.fit(idx, y, &m, None, x) {
                        return Some(b);
                    }
                }
            }
        }
        for &x in &xs {
            let avail: Vec<usize> = nums.iter().copied().filter(|&k| Some(k) != x).collect();
            let us: Vec<Mono> = (1..=2).flat_map(|s| self.monos(&avail, s)).collect();
            let ms: Vec<Mono> = std::iter::once(Vec::new())
                .chain((1..=2).flat_map(|s| self.monos(&avail, s)))
                .collect();
            for u in &us {
                for m in &ms {
                    let mut ins: Vec<usize> = u.iter().chain(m).map(|f| f.0).collect();
                    ins.sort_unstable();
                    ins.dedup();
                    if ins.len() > 2 {
                        continue;
                    }
                    if let Some(b) = self.fit(idx, y, m, Some(u), x) {
                        return Some(b);
                    }
                }
            }
        }
        None
    }

    /// Every monomial of `size` distinct inputs from `avail`, each with every factor its
    /// kind allows.
    fn monos(&self, avail: &[usize], size: usize) -> Vec<Mono> {
        let facs = |k: usize| -> &'static [Fac] {
            if self.dev.kinds[k] == Kind::Count {
                &COUNT_FACS
            } else {
                &NUM_FACS
            }
        };
        let mut out = Vec::new();
        for (a, &ka) in avail.iter().enumerate() {
            for &fa in facs(ka) {
                if size == 1 {
                    out.push(vec![(ka, fa)]);
                    continue;
                }
                for &kb in &avail[a + 1..] {
                    for &fb in facs(kb) {
                        out.push(vec![(ka, fa), (kb, fb)]);
                    }
                }
            }
        }
        out
    }
}

fn num_text(x: f64) -> String {
    let mut s = String::new();
    schemify_expr::write_num(&mut s, x);
    s
}

impl Body {
    fn expr(&self, names: &[String]) -> String {
        let prod = |m: &Mono| {
            let mut s = String::new();
            for (n, &(k, f)) in m.iter().enumerate() {
                f.push(&mut s, &names[k], n == 0);
            }
            s
        };
        let table = match self.x {
            None => num_text(self.steps[0].1),
            Some(k) => {
                let mut s = format!("steps({}", names[k]);
                for (b, v) in &self.steps {
                    s.push_str(&format!(", {}, {}", num_text(*b), num_text(*v)));
                }
                s.push_str(&format!(", {})", num_text(self.end)));
                s
            }
        };
        let inner = match &self.u {
            None => table,
            Some((u, a)) => {
                let au = if *a == 1.0 {
                    prod(u)
                } else {
                    format!("{}*{}", num_text(*a), prod(u))
                };
                match self.x {
                    None if self.steps[0].1 == 0.0 => au,
                    _ => format!("{au} + {table}"),
                }
            }
        };
        if self.m.is_empty() {
            inner
        } else if self.u.is_none() && self.x.is_none() {
            // c·m
            match self.steps[0].1 {
                1.0 => prod(&self.m),
                c => format!("{}*{}", num_text(c), prod(&self.m)),
            }
        } else if self.u.is_none() {
            format!("{}*{inner}", prod(&self.m))
        } else {
            format!("{}*({inner})", prod(&self.m))
        }
    }
}

impl Rule {
    fn expr(&self, dev: &Device) -> String {
        match self {
            Rule::One(b) => b.expr(&dev.inputs),
            Rule::Mux(s, arms) => {
                let sel = &dev.inputs[*s];
                let mut out = arms.last().expect("two arms at least").1.expr(&dev.inputs);
                for (v, b) in arms[..arms.len() - 1].iter().rev() {
                    let cond = if dev.kinds[*s] == Kind::Bool {
                        // Arms are sorted true first: the other is the else.
                        sel.clone()
                    } else {
                        format!("{sel} == \"{v}\"")
                    };
                    out = format!("if({cond}, {}, {out})", b.expr(&dev.inputs));
                }
                out
            }
        }
    }
}

/// Whether a boolean read back true (false reads back `""` or `"nil"`).
fn is_true(t: &str) -> bool {
    matches!(t, "t" | "TRUE" | "true")
}

/// The value of input `k` read back at `p`, as schemify-expr sees the param: a number, a
/// boolean (`"t"`: true) or a choice's text.
fn input_value<'p>(dev: &Device, p: &'p Point, k: usize) -> Option<schemify_expr::Value<'p>> {
    let t = p.back[k].as_str();
    Some(match dev.kinds[k] {
        Kind::Num | Kind::Count => schemify_expr::Value::Num(ael_num(t)?),
        Kind::Bool => schemify_expr::Value::Bool(is_true(t)),
        Kind::Choice => schemify_expr::Value::Str(t),
    })
}

/// `src` evaluated by schemify-expr at point `p` (its inputs as read back).
fn eval_at(dev: &Device, src: &str, p: &Point) -> Option<f64> {
    let prog = schemify_expr::Program::compile(src, schemify_expr::Mode::Scalar).ok()?;
    let mut inputs = Vec::new();
    prog.bind(&mut inputs, |n| {
        let k = dev.inputs.iter().position(|i| i == n)?;
        input_value(dev, p, k)
    })
    .ok()?;
    let mut scratch = schemify_expr::Scratch::default();
    match prog.eval(&inputs, &mut scratch).ok()? {
        schemify_expr::Value::Num(x) => Some(x),
        schemify_expr::Value::Int(x) => Some(x as f64),
        _ => None,
    }
}

/// The rules measured for one device: Schemify expressions over its own inputs that
/// reproduce what the callbacks derive, each checked at every point.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rules {
    /// (param, expression) for each derived netlisted param a rule explains.
    pub computed: Vec<(String, String)>,
    /// Derived params no rule explains: they keep Virtuoso's values (or go stale, §8.3).
    pub unexplained: Vec<String>,
    /// Points every rule was checked at.
    pub points: usize,
}

impl Device {
    /// The rules that reproduce this device's derived params (spec §8.3), found by their
    /// role in the data, never by a name: per derived param the simplest of one value, a
    /// step table over one number input, a table times a monomial of up to two other
    /// inputs, an affine form `m·(a·u + T(x))`, then one of those per value of a boolean
    /// or choice input. Coordinates are the inputs as read back. A rule is kept only when
    /// schemify-expr, evaluating its expression, reproduces the param at every successful
    /// point within the digits it was printed with ([`printed_tol`]).
    pub fn rules(&self) -> Rules {
        let s = self.summary();
        let mut rules = Rules {
            points: s.ok,
            ..Rules::default()
        };
        let Some(g) = Grid::new(self) else {
            rules.unexplained = s.derived;
            return rules;
        };
        let all: Vec<usize> = (0..g.ok.len()).collect();
        for d in &s.derived {
            let k = self
                .outputs
                .iter()
                .position(|o| o == d)
                .expect("a derived output");
            let y: Option<Vec<(f64, f64)>> =
                g.ok.iter()
                    .map(|&i| printed_tol(&self.points[i].values[k]))
                    .collect();
            let rule = y.and_then(|y| self.infer(&g, &all, &y));
            let src = rule.map(|r| r.expr(self)).filter(|src| {
                g.ok.iter().all(|&i| {
                    let p = &self.points[i];
                    let (v, t) = printed_tol(&p.values[k]).expect("checked above");
                    eval_at(self, src, p).is_some_and(|got| (got - v).abs() <= t * (1.0 + 1e-9))
                })
            });
            match src {
                Some(src) => rules.computed.push((d.clone(), src)),
                None => rules.unexplained.push(d.clone()),
            }
        }
        debug_assert!(rules.computed.len() + rules.unexplained.len() == s.derived.len());
        rules
    }

    fn infer(&self, g: &Grid<'_>, idx: &[usize], y: &[(f64, f64)]) -> Option<Rule> {
        if let Some(b) = g.simplest(idx, y, None) {
            return Some(Rule::One(b));
        }
        // A multiplexer: one body per value of a boolean or choice input.
        for (s, kind) in self.kinds.iter().enumerate() {
            if !matches!(kind, Kind::Bool | Kind::Choice) {
                continue;
            }
            let mut vals: Vec<&str> = idx
                .iter()
                .map(|&i| self.points[g.ok[i]].back[s].as_str())
                .collect();
            vals.sort_unstable();
            vals.dedup();
            if vals.len() < 2 {
                continue;
            }
            let arms: Option<Vec<(String, Body)>> = vals
                .iter()
                .map(|v| {
                    let part: Vec<usize> = idx
                        .iter()
                        .copied()
                        .filter(|&i| self.points[g.ok[i]].back[s] == *v)
                        .collect();
                    Some(((*v).to_owned(), g.simplest(&part, y, Some(s))?))
                })
                .collect();
            if let Some(mut arms) = arms {
                // A boolean's true arm first: `if(b, …, …)`.
                arms.sort_by_key(|a| !is_true(&a.0));
                return Some(Rule::Mux(s, arms));
            }
        }
        None
    }
}

/// Points to check `rules` at that the characterization never measured (spec §8.3), as
/// SKILL value lists in input order for `SfyCharacterize ?points` (`nil`: the default):
/// three points, each number input at an odd multiple of its default inside the range read
/// back (×2.9, ×0.62, ×6.7), each count at 5 6 7 (at most the largest measured), each
/// boolean or choice cycling through the values measured.
pub fn holdout_points(d: &Device) -> Vec<String> {
    let ok: Vec<&Point> = d.points.iter().filter(|p| p.status.is_ok()).collect();
    let mut cols: Vec<[String; 3]> = Vec::new();
    for (k, kind) in d.kinds.iter().enumerate() {
        let backs: Vec<f64> = ok.iter().filter_map(|p| ael_num(&p.back[k])).collect();
        let (lo, hi) = backs
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |a, &x| {
                (a.0.min(x), a.1.max(x))
            });
        let def = ael_num(&d.defaults[k]);
        cols.push(match kind {
            Kind::Num => [2.9, 0.62, 6.7].map(|f| match def {
                Some(x) if lo <= hi && x != 0.0 => format!("{:e}", (x * f).clamp(lo, hi)),
                _ => "nil".into(),
            }),
            Kind::Count => [5.0, 6.0, 7.0].map(|n: f64| {
                if lo <= hi {
                    format!("{}", n.min(hi).max(lo) as i64)
                } else {
                    "nil".into()
                }
            }),
            Kind::Bool | Kind::Choice => {
                let mut vals: Vec<&str> = ok.iter().map(|p| p.back[k].as_str()).collect();
                vals.sort_unstable();
                vals.dedup();
                [0, 1, 2].map(|j| match vals.get((j + 1) % vals.len().max(1)) {
                    Some(v) => format!("{v:?}"),
                    None => "nil".into(),
                })
            }
        });
    }
    let out: Vec<String> = (0..3)
        .map(|j| {
            let vals: Vec<&str> = cols.iter().map(|c| c[j].as_str()).collect();
            format!("(list {})", vals.join(" "))
        })
        .collect();
    debug_assert!(out.len() == 3);
    out
}

/// How the rules fared at a holdout: per computed param, the worst error over the check's
/// successful points (the rule evaluated by schemify-expr at each point's inputs as read
/// back), relative to the value, and whether every point stayed within the digits the
/// callbacks printed ([`printed_tol`]).
pub fn check(rules: &Rules, holdout: &Device) -> Vec<(String, f64, bool)> {
    let mut out = Vec::new();
    for (name, src) in &rules.computed {
        let k = holdout.outputs.iter().position(|o| o == name);
        let mut worst: f64 = 0.0;
        let mut all = true;
        for p in holdout.points.iter().filter(|p| p.status.is_ok()) {
            let want = k.and_then(|k| printed_tol(&p.values[k]));
            match (eval_at(holdout, src, p), want) {
                (Some(g), Some((v, t))) => {
                    worst = worst.max((g - v).abs() / v.abs().max(1e-300));
                    all &= (g - v).abs() <= t * (1.0 + 1e-9);
                }
                _ => {
                    worst = f64::INFINITY;
                    all = false;
                }
            }
        }
        out.push((name.clone(), worst, all));
    }
    out
}

/// Appends record `r` of `src` to `out` without the attributes `skip` names.
fn copy_without(out: &mut Document<'static>, src: &Document<'_>, r: u32, skip: &[&str]) {
    out.begin_record(src.record_kind(r));
    for f in src.fields(r) {
        out.push_field(f);
    }
    for a in src.attrs(r) {
        if !skip.contains(&src.attr_key(a)) {
            copy_attr(out, src, a);
        }
    }
    for v in src.verbatim(r) {
        out.push_verbatim(v);
    }
}

/// A mirror cell file with `rules` applied (spec §8.3): each explained param `computed
/// <expr>`, each unexplained derived param `vderived true` (its value is Virtuoso's and goes
/// stale when the inputs change).
///
/// # Errors
///
/// The text is not a Schemify file.
pub fn apply_rules(text: &[u8], rules: &Rules) -> Result<String, String> {
    let src = parse(text).map_err(|e| e.to_string())?;
    let mut out = Document::new(src.kind());
    copy_header(&mut out, &src, &[]);
    for r in 1..src.record_count() {
        let name = src.field(r, 0).unwrap_or("");
        if src.record_kind(r) != RecordKind::Param {
            out.copy_record(&src, r);
        } else if let Some((_, e)) = rules.computed.iter().find(|c| c.0 == name) {
            copy_without(&mut out, &src, r, &["computed"]);
            push_attr(&mut out, "computed", &[e]);
        } else if rules.unexplained.iter().any(|u| u == name) {
            copy_without(&mut out, &src, r, &["vderived"]);
            push_attr(&mut out, "vderived", &["true"]);
        } else {
            out.copy_record(&src, r);
        }
    }
    debug_assert_eq!(out.record_count(), src.record_count());
    Ok(write(&out))
}

/// `text` (a design file) without the overrides of `params` on instances of `cellref`, and
/// how many it dropped; `None` when it drops none. Computed params take no override (E030),
/// and a value from Virtuoso's callbacks is the rule's value at those inputs.
pub fn strip_overrides(text: &[u8], cellref: &str, params: &[&str]) -> Option<(String, usize)> {
    let src = parse(text).ok()?;
    let mut out = Document::new(src.kind());
    copy_header(&mut out, &src, &[]);
    let mut dropped = 0;
    for r in 1..src.record_count() {
        if src.record_kind(r) == RecordKind::Inst && src.field(r, 1) == Some(cellref) {
            dropped += src
                .attrs(r)
                .filter(|&a| params.contains(&src.attr_key(a)))
                .count();
            copy_without(&mut out, &src, r, params);
        } else {
            out.copy_record(&src, r);
        }
    }
    (dropped > 0).then(|| (write(&out), dropped))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"("sfyx" 1 "schemify.il 1")
("char" "pdk" "nch" ("w" "nf") ("w" "nf" "ad" "m") ("num" "count") ("1e-06" "1"))
("probe" "w" "num" "2e-06" "ok" ("ad"))
("probe" "flip" "bool" "t" "ok" ())
("pt" ("1e-06" "1") "ok" ("1u" "1" "1f" "1") ("1u" "1"))
("pt" ("1e-06" "2") "ok" ("1u" "2" "0.5f" "1") ("1u" "2"))
("pt" ("4e-06" "1") "\"callback: w failed\"" ("4u" "1" "?" "1") ("4u" "1"))
("end")
"#;

    #[test]
    fn a_file_reads_and_classifies_its_parameters() {
        let d = &read(FILE).unwrap()[0];
        assert_eq!((d.lib.as_str(), d.cell.as_str()), ("pdk", "nch"));
        assert_eq!(d.points.len(), 3);
        assert_eq!(d.kinds, [Kind::Num, Kind::Count]);
        assert_eq!(d.probes.len(), 2);
        assert_eq!(d.probes[0].changed, ["ad"]);
        assert!(d.probes[1].changed.is_empty() && d.probes[1].kind == Kind::Bool);
        assert!(d.points[2].status.is_err());
        let s = d.summary();
        assert_eq!((s.ok, s.failed), (2, 1));
        // The failed point's values are not evidence: w is constant over the ok points.
        assert_eq!(s.constant, ["w", "m"]);
        assert_eq!(s.inputs, ["nf"]);
        assert_eq!(s.derived, ["ad"]);
    }

    #[test]
    fn printed_digits_bound_each_value() {
        let tol = |s: &str| printed_tol(s).unwrap();
        let near = |a: f64, b: f64| (a - b).abs() <= 1e-9 * b.abs();
        let (v, t) = tol("6.72f");
        assert!(near(v, 6.72e-15) && near(t, 0.005e-15), "{t}");
        assert!(near(tol("1.25e-15").1, 0.005e-15));
        assert_eq!(tol("3").1, 0.5);
        assert_eq!(tol("-40").1, 0.5);
        // Many digits: the floor of 1e-6 relative.
        let (v, t) = tol("1.234567891e-06");
        assert!(near(t, v * TOL_FLOOR));
        assert!(printed_tol("iPar(\"m\")").is_none() && printed_tol("").is_none());
    }

    #[test]
    fn the_nicest_number_in_a_bracket_is_the_shortest() {
        let mid = |lo: f64, hi: f64| nicest(lo, hi, (lo + hi) / 2.0);
        assert_eq!(nicest(1.99998e-6, 2.00001e-6, 2.00001e-6), 2e-6);
        // A breakpoint aims at the first value past it, a value at the middle.
        assert_eq!(nicest(1.2e-8, 1.4e-8, 1.4e-8), 1.4e-8);
        assert_eq!(mid(4.29e-8, 4.51e-8), 4.4e-8);
        assert_eq!(mid(3.0, 3.0), 3.0);
        assert_eq!(mid(-2.0, 1.0), 0.0);
        assert_eq!(mid(-7.34, -7.18), -7.3);
        assert_eq!(mid(1.21, 1.29), 1.25);
        for (lo, hi) in [(1.234e-9, 1.2341e-9), (0.5, 0.7), (123.4, 123.5)] {
            let x = mid(lo, hi);
            assert!(lo <= x && x <= hi, "{lo} {hi} {x}");
        }
    }

    #[test]
    fn rules_apply_to_a_mirror_cell_and_strip_design_overrides() {
        let r = Rules {
            computed: vec![(
                "as".into(),
                "(floor(nf/2)+1)/nf*w*steps(l, 1e-08, 5e-08, 2e-08, 6e-08, 4e-08)".into(),
            )],
            unexplained: vec!["m".into()],
            points: 1,
        };
        let cell = b"schemify 1 shprim\n\npin d inout 0 0\n\nparam w 100n\nparam l 10n\nparam nf 1\nparam as 5f\n  unit m\nparam m 1\n\nimpl spice spice\n  format spice \"M{name} {d} x w={w} as={as}\"\n";
        let out = apply_rules(cell, &r).unwrap();
        assert!(
            out.contains("param as 5f\n  unit m\n  computed \"(floor(nf/2)+1)/nf*w*steps(l, "),
            "{out}"
        );
        assert!(out.contains("param m 1\n  vderived true\n"), "{out}");
        let design = b"schemify 1 shcomp\n\nimpl schematic schematic\n\ninst a00001 pdk:mos\n  at 0 0 R0\n  name M1\n  w 1u\n  as 3f\n  ad 2f\n\ninst a00002 pdk:other\n  at 10 0 R0\n  name M2\n  as 3f\n";
        let (text, n) = strip_overrides(design, "pdk:mos", &["as", "ad"]).unwrap();
        assert_eq!(n, 2);
        assert!(
            text.contains("  w 1u\n") && !text.contains("  ad 2f"),
            "{text}"
        );
        assert!(
            text.contains("name M2\n  as 3f"),
            "another cell keeps its overrides: {text}"
        );
        assert!(strip_overrides(design, "pdk:none", &["as"]).is_none());
    }

    #[test]
    fn malformed_files_are_refused() {
        let bad = [
            FILE.replace(
                "(\"char\" \"pdk\" \"nch\" (\"w\" \"nf\") (\"w\" \"nf\" \"ad\" \"m\") (\"num\" \"count\") (\"1e-06\" \"1\"))\n",
                "",
            ),
            FILE.replace(
                "(\"1e-06\" \"1\") \"ok\" (\"1u\" \"1\" \"1f\" \"1\")",
                "(\"1e-06\") \"ok\" (\"1u\" \"1\" \"1f\" \"1\")",
            ),
            FILE.replace("(\"end\")", "(\"what\")\n(\"end\")"),
            FILE.replace("(\"num\" \"count\")", "(\"num\" \"wat\")"),
        ];
        for b in &bad {
            assert!(read(b).is_err(), "{b}");
        }
    }
}
