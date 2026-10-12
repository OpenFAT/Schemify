//! gm/Id retarget oracles (spec `interop.md` §10.4): the Rust reference against a
//! closed-form device, the SKILL mirror against the Rust reference, the vectors embedded in
//! `sfy_retarget.il`, intents from a Design, the target-map parser, and `SfyRetarget` end to
//! end through the interpreter with a mock OCEAN.

mod skill;

use std::collections::BTreeSet;

use interop::design::{self, Files};
use interop::retarget::{
    self, Intent, Miss, Polarity, Table, TargetClass, WidthMode, fingers, snap_l, solve,
};
use schemify_expr::{Mode, Program, Scratch, Value};
use skill::mockdb::MockDb;
use skill::{Interp, V};

// ---------------------------------------------------------------------------------------
// The analytic device: gmid-lut's `src/synth.rs` (our own MIT code), ported because the
// interop crate does not link the gm/Id guest.
// ---------------------------------------------------------------------------------------

/// `Id/W = 2nβUt²·F²·(1+λVds)`, `F = ln(1+e^u)`, `u = (Vgs−Vt)/(2nUt)`, `β = µCox/L`,
/// `λ = λ0·L0/L`, `Cgg/W = Cox·L`; gm/Id = σ(u)/(n·Ut·F), independent of L and VDS.
const N: f64 = 1.3;
const UT: f64 = 0.025_85;
const VT: f64 = 0.45;
const MU_COX: f64 = 270e-6;
const LAMBDA0: f64 = 0.1;
const L0: f64 = 0.15e-6;
const COX: f64 = 8.8e-3;

fn softplus(u: f64) -> f64 {
    if u > 30.0 { u } else { u.exp().ln_1p() }
}

fn sigmoid(u: f64) -> f64 {
    1.0 / (1.0 + (-u).exp())
}

/// `[id, gm, gds, cgg]` per metre of width.
fn ekv(vgs: f64, vds: f64, l: f64) -> [f64; 4] {
    let beta = MU_COX / l;
    let lambda = LAMBDA0 * L0 / l;
    let u = (vgs - VT) / (2.0 * N * UT);
    let f = softplus(u);
    let id0 = 2.0 * N * beta * UT * UT * f * f;
    let gm0 = 2.0 * beta * UT * f * sigmoid(u);
    [
        id0 * (1.0 + lambda * vds),
        gm0 * (1.0 + lambda * vds),
        id0 * lambda,
        COX * l,
    ]
}

/// The VGS where gm/Id equals `target` (bisection).
fn ekv_vgs_for(target: f64) -> f64 {
    let gmid = |v: f64| {
        let u = (v - VT) / (2.0 * N * UT);
        sigmoid(u) / (N * UT * softplus(u))
    };
    let (mut lo, mut hi) = (-2.0, 3.0);
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

/// Axes and absolute sample blocks of `ekv` at width `w`, cell order `(l, vds, vgs)`.
struct Grid {
    vds_nom: f64,
    l: Vec<f64>,
    vds: Vec<f64>,
    vgs: Vec<f64>,
    w: f64,
    blocks: [Vec<f64>; 4],
}

fn grid(l: Vec<f64>, vds: Vec<f64>, vgs: Vec<f64>, vds_nom: f64, w: f64) -> Grid {
    let mut blocks: [Vec<f64>; 4] = Default::default();
    for &li in &l {
        for &d in &vds {
            for &g in &vgs {
                let q = ekv(g, d, li);
                for k in 0..4 {
                    blocks[k].push(q[k] * w);
                }
            }
        }
    }
    Grid {
        vds_nom,
        l,
        vds,
        vgs,
        w,
        blocks,
    }
}

/// gmid-lut's characterization grid (VGS 0..1.8 in 25 mV, VDS 0.15..1.8 in 0.15, 14 L).
fn synth_grid() -> Grid {
    let vgs = (0..=72).map(|k| 1.8 * f64::from(k) / 72.0).collect();
    let vds = (1..=12).map(|k| 1.8 * f64::from(k) / 12.0).collect();
    let l = (0..14)
        .map(|k| 0.15e-6 * (8.0f64 / 0.15).powf(f64::from(k) / 13.0))
        .collect();
    grid(l, vds, vgs, 0.9, 1e-6)
}

impl Grid {
    fn table(&self) -> Table {
        let b = &self.blocks;
        Table::from_samples(
            self.vds_nom,
            self.l.clone(),
            self.vds.clone(),
            self.vgs.clone(),
            self.w,
            [&b[0], &b[1], &b[2], &b[3]],
        )
        .unwrap()
    }

    /// The samples nested as `SfyRtTableMake` takes them.
    fn skill_samples(&self) -> V {
        let (nd, ng) = (self.vds.len(), self.vgs.len());
        V::list(
            (0..self.l.len())
                .map(|li| {
                    V::list(
                        (0..nd)
                            .map(|d| {
                                V::list(
                                    (0..ng)
                                        .map(|g| {
                                            let i = (li * nd + d) * ng + g;
                                            V::list(
                                                self.blocks
                                                    .iter()
                                                    .map(|b| V::Float(b[i]))
                                                    .collect(),
                                            )
                                        })
                                        .collect(),
                                )
                            })
                            .collect(),
                    )
                })
                .collect(),
        )
    }

    fn skill_table(&self, i: &mut Interp<MockDb>) -> V {
        let floats = |v: &[f64]| V::list(v.iter().map(|x| V::Float(*x)).collect());
        i.call(
            "SfyRtTableMake",
            vec![
                V::Float(self.vds_nom),
                floats(&self.l),
                floats(&self.vds),
                floats(&self.vgs),
                V::Float(self.w),
                self.skill_samples(),
            ],
        )
        .unwrap()
    }
}

fn interp() -> Interp<MockDb> {
    let mut i = Interp::new(MockDb::default());
    i.load(include_str!("../skill/sfy_retarget.il"))
        .expect("sfy_retarget.il loads");
    i
}

/// Deterministic xorshift in [0, 1).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next() * xs.len() as f64) as usize % xs.len()]
    }
}

fn rel(a: f64, b: f64) -> f64 {
    (a - b).abs() / a.abs().max(b.abs()).max(f64::MIN_POSITIVE)
}

fn floats(v: &V) -> Option<Vec<f64>> {
    match v {
        V::Nil => None,
        _ => Some(
            v.items()
                .iter()
                .map(|x| match x {
                    V::Float(f) => *f,
                    V::Int(i) => *i as f64,
                    other => panic!("not a number: {other:?}"),
                })
                .collect(),
        ),
    }
}

// ---------------------------------------------------------------------------------------
// (a) Rust reference vs the analytic device
// ---------------------------------------------------------------------------------------

#[test]
fn reference_matches_the_analytic_device_within_two_percent() {
    let g = synth_grid();
    let t = g.table();
    let mut worst: f64 = 0.0;
    let ls = [t.l[0], t.l[3], t.l[13], 0.3e-6, 0.77e-6, 2.5e-6, 6.1e-6];
    for &l in &ls {
        for vds in [
            None,
            Some(0.15),
            Some(0.6),
            Some(1.0),
            Some(1.37),
            Some(1.8),
        ] {
            for gm_id in [3.0, 5.0, 8.0, 10.0, 15.0, 20.0, 24.0] {
                let id = 10e-6;
                let s = solve(&t, gm_id, l, id, vds).unwrap();
                let vgs = ekv_vgs_for(gm_id);
                let q = ekv(vgs, vds.unwrap_or(0.9), l);
                assert!(
                    (s.vgs - vgs).abs() < 2e-3,
                    "vgs {} vs {vgs} at {gm_id} {l} {vds:?}",
                    s.vgs
                );
                let w_exact = id / q[0];
                let e = rel(s.w, w_exact);
                assert!(
                    e < 0.02,
                    "W {} vs {w_exact} at gm/Id {gm_id} L {l} VDS {vds:?}",
                    s.w
                );
                assert!(s.gm_over_id_residual.abs() < 0.02 * gm_id);
                worst = worst.max(e);
            }
        }
    }
    eprintln!(
        "retarget reference vs EKV: worst W error {:.3} %",
        worst * 100.0
    );
    // Negative space: off the axes, unreachable, bad current.
    assert_eq!(solve(&t, 15.0, t.l[0] * 0.9, 1e-6, None), Err(Miss::L));
    assert_eq!(solve(&t, 15.0, 9e-6, 1e-6, None), Err(Miss::L));
    assert_eq!(solve(&t, 15.0, 1e-6, 1e-6, Some(0.1)), Err(Miss::Vds));
    assert_eq!(solve(&t, 100.0, 1e-6, 1e-6, None), Err(Miss::GmId));
    assert_eq!(solve(&t, 0.5, 1e-6, 1e-6, None), Err(Miss::GmId));
    assert_eq!(solve(&t, f64::NAN, 1e-6, 1e-6, None), Err(Miss::GmId));
    assert_eq!(solve(&t, 15.0, 1e-6, 0.0, None), Err(Miss::Id));
    assert!(
        solve(&t, 15.0, t.l[13] * (1.0 + 1e-12), 1e-6, None).is_ok(),
        "edge slack"
    );
    // A table that breaks its invariants is refused.
    let b = &g.blocks;
    assert!(
        Table::from_samples(
            0.9,
            vec![1e-6, 1e-6],
            g.vds.clone(),
            g.vgs.clone(),
            1e-6,
            [&b[0], &b[1], &b[2], &b[3]]
        )
        .is_err()
    );
    assert!(
        Table::from_samples(
            5.0,
            g.l.clone(),
            g.vds.clone(),
            g.vgs.clone(),
            1e-6,
            [&b[0], &b[1], &b[2], &b[3]]
        )
        .is_err()
    );
    assert!(
        Table::from_samples(
            0.9,
            g.l.clone(),
            g.vds.clone(),
            g.vgs.clone(),
            0.0,
            [&b[0], &b[1], &b[2], &b[3]]
        )
        .is_err()
    );
}

#[test]
fn snapping_and_finger_split() {
    assert_eq!(snap_l(0.5e-6, 30e-9), 17.0 * 30e-9);
    assert_eq!(snap_l(0.51e-6, 30e-9), 17.0 * 30e-9);
    assert_eq!(snap_l(1e-9, 30e-9), 30e-9, "at least one step");
    assert_eq!(snap_l(0.5e-6, 0.0), 0.5e-6);
    assert_eq!(fingers(10e-6, 1, Some(2e-6)), 5);
    assert_eq!(fingers(10e-6 * (1.0 + 1e-12), 1, Some(2e-6)), 5, "slack");
    assert_eq!(fingers(10.1e-6, 1, Some(2e-6)), 6);
    assert_eq!(
        fingers(1e-6, 4, Some(2e-6)),
        4,
        "never fewer than the intent"
    );
    assert_eq!(fingers(10e-6, 2, None), 2);
}

// ---------------------------------------------------------------------------------------
// (b) SKILL mirror vs Rust reference
// ---------------------------------------------------------------------------------------

#[test]
fn skill_math_agrees_with_the_rust_reference() {
    let g = synth_grid();
    let t = g.table();
    let mut i = interp();
    let tab = g.skill_table(&mut i);
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let (mut solved, mut missed, mut worst) = (0u32, 0u32, 0.0f64);
    for n in 0..240 {
        let gm_id = 1.0 + 30.0 * rng.next();
        let l = match n % 4 {
            0 => *rng.pick(&t.l),
            3 if n % 20 == 3 => *rng.pick(&[0.1e-6, 9e-6]),
            _ => t.l[0] * (t.l[13] / t.l[0]).powf(rng.next()),
        };
        let vds = match n % 3 {
            0 => None,
            1 => Some(*rng.pick(&t.vds)),
            _ if n % 30 == 2 => Some(*rng.pick(&[0.05, 2.0])),
            _ => Some(0.15 + 1.65 * rng.next()),
        };
        let id = 1e-7 * 1e4f64.powf(rng.next());
        let want = solve(&t, gm_id, l, id, vds);
        let got = i
            .call(
                "SfyRtSolve",
                vec![
                    tab.clone(),
                    V::Float(gm_id),
                    V::Float(l),
                    V::Float(id),
                    vds.map_or(V::Nil, V::Float),
                ],
            )
            .unwrap();
        match (want, floats(&got)) {
            (Ok(s), Some(v)) => {
                for (a, b) in [(s.vgs, v[0]), (s.jd, v[1]), (s.w, v[2])] {
                    let e = rel(a, b);
                    worst = worst.max(e);
                    assert!(
                        e <= 1e-9,
                        "vector {n}: {a} vs {b} ({gm_id} {l} {id} {vds:?})"
                    );
                }
                assert!((s.gm_over_id_residual - v[3]).abs() <= 1e-9);
                solved += 1;
            }
            (Err(_), None) => missed += 1,
            (w, g) => panic!("vector {n} ({gm_id} {l} {vds:?}): rust {w:?}, skill {g:?}"),
        }
    }
    eprintln!(
        "SKILL vs Rust: {solved} solved, {missed} missed by both, worst relative difference {worst:e}"
    );
    assert!(solved >= 200, "too few reachable vectors: {solved}");
    assert!(missed > 0, "negative space not exercised");
    // Snapping and fingers agree too.
    for (l, lg) in [
        (0.5e-6, 30e-9),
        (0.73e-6, 5e-9),
        (1e-9, 30e-9),
        (2.0e-6, 0.0),
    ] {
        let got = i
            .call("SfyRtSnapL", vec![V::Float(l), V::Float(lg)])
            .unwrap();
        assert!(
            matches!(got, V::Float(x) if x == snap_l(l, lg)),
            "{l} {lg}: {got:?}"
        );
    }
    for (w, nf, m) in [
        (10e-6, 1, Some(2e-6)),
        (10.1e-6, 1, Some(2e-6)),
        (1e-6, 4, Some(2e-6)),
        (3e-6, 2, None),
    ] {
        let got = i
            .call(
                "SfyRtFingers",
                vec![
                    V::Float(w),
                    V::Int(i64::from(nf)),
                    m.map_or(V::Nil, V::Float),
                ],
            )
            .unwrap();
        assert_eq!(
            got.as_int(),
            Some(i64::from(fingers(w, nf, m))),
            "{w} {nf} {m:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// (c) the vectors embedded in sfy_retarget.il, (f) SfyRetargetSelfTest
// ---------------------------------------------------------------------------------------

const BEGIN: &str =
    ";; ---- BEGIN GENERATED by plugins/interop/tests/retarget.rs (SCHEMIFY_BLESS=1 rewrites it)\n";
const END: &str = ";; ---- END GENERATED\n";

/// A SKILL float literal that reads back to exactly `x` (digits on both sides of `.`).
fn sk(x: f64) -> String {
    let s = format!("{x:e}");
    let (m, e) = s.split_once('e').unwrap();
    if m.contains('.') {
        s
    } else {
        format!("{m}.0e{e}")
    }
}

fn sk_list(xs: &[f64]) -> String {
    let v: Vec<String> = xs.iter().map(|x| sk(*x)).collect();
    format!("({})", v.join(" "))
}

/// The generated block: `(SfyRtSelfData)` returning (vdsNom l vds vgs w samples vectors
/// snaps fingers), every expected value from the Rust reference.
fn embedded() -> String {
    let g = grid(
        vec![0.15e-6, 0.5e-6, 2e-6],
        vec![0.3, 0.9, 1.5],
        (0..=18).map(|k| 0.1 * f64::from(k)).collect(),
        0.9,
        1e-6,
    );
    let t = g.table();
    let mut out = String::from(BEGIN);
    out.push_str("(procedure (SfyRtSelfData)\n  '(");
    out.push_str(&format!(
        "{}\n    {}\n    {}\n    {}\n    {}\n    (",
        sk(g.vds_nom),
        sk_list(&g.l),
        sk_list(&g.vds),
        sk_list(&g.vgs),
        sk(g.w)
    ));
    let (nd, ng) = (g.vds.len(), g.vgs.len());
    for li in 0..g.l.len() {
        out.push_str(if li == 0 { "(" } else { "\n     (" });
        for d in 0..nd {
            out.push_str(if d == 0 { "(" } else { "\n      (" });
            for gi in 0..ng {
                let k = (li * nd + d) * ng + gi;
                let q: Vec<f64> = g.blocks.iter().map(|b| b[k]).collect();
                if gi > 0 {
                    out.push_str("\n       ");
                }
                out.push_str(&sk_list(&q));
            }
            out.push(')');
        }
        out.push(')');
    }
    out.push_str(")\n    (");
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    // Random vectors, then three every implementation must miss (gm/Id, L, VDS).
    let fixed = [
        (40.0, 0.5e-6, None, 1e-6),
        (15.0, 3e-6, None, 1e-6),
        (15.0, 0.5e-6, Some(2.0), 1e-6),
    ];
    for n in 0..51 {
        let gm_id = 2.0 + 28.0 * rng.next();
        let l = if n % 3 == 0 {
            *rng.pick(&g.l)
        } else {
            0.15e-6 * (2e-6f64 / 0.15e-6).powf(rng.next())
        };
        let vds = match n % 4 {
            0 => None,
            1 => Some(*rng.pick(&g.vds)),
            _ => Some(0.3 + 1.2 * rng.next()),
        };
        let id = 1e-6 * 100f64.powf(rng.next());
        let (gm_id, l, vds, id) = if n < 48 {
            (gm_id, l, vds, id)
        } else {
            fixed[n - 48]
        };
        let want = match solve(&t, gm_id, l, id, vds) {
            Ok(s) => sk_list(&[s.vgs, s.jd, s.w, s.gm_over_id_residual]),
            Err(_) => "nil".into(),
        };
        let vds = vds.map_or("nil".to_owned(), sk);
        out.push_str(&format!(
            "{}({} {} {vds} {} {want})",
            if n == 0 { "" } else { "\n     " },
            sk(gm_id),
            sk(l),
            sk(id)
        ));
    }
    out.push_str(")\n    (");
    for (k, (l, lg)) in [(0.5e-6, 30e-9), (0.73e-6, 5e-9), (1e-9, 30e-9)]
        .iter()
        .enumerate()
    {
        out.push_str(&format!(
            "{}({} {} {})",
            if k == 0 { "" } else { " " },
            sk(*l),
            sk(*lg),
            sk(snap_l(*l, *lg))
        ));
    }
    out.push_str(")\n    (");
    for (k, (w, nf, m)) in [
        (10e-6, 1u32, Some(2e-6)),
        (10.1e-6, 1, Some(2e-6)),
        (1e-6, 4, Some(2e-6)),
        (3e-6, 2, None),
    ]
    .iter()
    .enumerate()
    {
        out.push_str(&format!(
            "{}({} {nf} {} {})",
            if k == 0 { "" } else { " " },
            sk(*w),
            m.map_or("nil".to_owned(), sk),
            fingers(*w, *nf, *m)
        ));
    }
    out.push_str(")))\n");
    out.push_str(END);
    out
}

#[test]
fn embedded_vectors_are_current() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/skill/sfy_retarget.il");
    let text = std::fs::read_to_string(path).unwrap();
    let start = text.find(BEGIN).expect("BEGIN GENERATED marker");
    let end = text.find(END).expect("END GENERATED marker") + END.len();
    let want = embedded();
    if std::env::var("SCHEMIFY_BLESS").as_deref() == Ok("1") {
        let new = format!("{}{want}{}", &text[..start], &text[end..]);
        std::fs::write(path, new).unwrap();
        return;
    }
    assert!(
        text[start..end] == want,
        "sfy_retarget.il's generated vectors are stale: rerun with SCHEMIFY_BLESS=1"
    );
    // The embedded vectors cover both outcomes.
    assert!(want.matches(" nil)").count() >= 3 && want.matches("e-").count() > 100);
}

#[test]
fn skill_self_test_prints_pass() {
    let mut i = interp();
    let r = i.call("SfyRetargetSelfTest", vec![]).unwrap();
    assert!(i.log.contains("SfyRetargetSelfTest: PASS"), "{}", i.log);
    assert!(matches!(r, V::T));
    // Negative space: a corrupted expectation makes it fail.
    let mut i = interp();
    i.load("(procedure (SfyRtSelfData) (list 0.9 (list 1.0e-6) (list 0.9) (list 0.0 1.0) 1.0e-6 (list (list (list (list 1.0e-6 1.0e-5 1.0e-7 1.0e-9) (list 1.0e-4 1.0e-4 1.0e-6 1.0e-9))))
        (list (list 5.0 1.0e-6 nil 1.0e-6 (list 0.5 1.0 1.0 0.0))) nil nil))").unwrap();
    assert!(!i.call("SfyRetargetSelfTest", vec![]).unwrap().truthy());
    assert!(i.log.contains("FAIL"), "{}", i.log);
}

// ---------------------------------------------------------------------------------------
// (d) intents from a Design
// ---------------------------------------------------------------------------------------

const BENCH: &str = r#"schemify 1 shbench

inst m00001 core:nmos4
  at 0 0 R0
  name M1
  W "gmid.w(\"nfet\", gmid1, L1, Id1)"
  nf 2
inst m00002 core:pmos4
  at 100 0 R0
  name M2
  w W2
inst m00003 core:nmos4
  at 200 0 R0
  name M3
  w 1u
inst m00004 core:nmos4
  at 300 0 R0
  name M4
  W "gmid.w(\"nfet\", gmid1)"
inst m00005 core:nmos4
  at 400 0 R0
  name M5
  W "2*gmid.w(\"nfet\", gmid1, L1, Id1)"
inst m00006 core:nmos4
  at 500 0 R0
  name M6
  W "gmid.w(nfet, gmid1, L1, Id1)"
inst m00007 core:nmos4
  at 600 0 R0
  name M7
  W "gmid.w(\"nfet\", gmid1, Lx, Id1)"
inst m00008 core:nmos4
  at 700 0 R0
  name M8
  W "gmid.w(\"nfet\", gmid1, L1, Id1)"
  nf 1.5
inst m00009 core:nmos4
  at 800 0 R0
  name M9

var gmid1 15
var L1 0.5u
var Id1 10u
var W1 "gmid.w_at(\"pfet\", gmid1, L1, Id1, 0.6)"
var W2 W1
"#;

/// The bench's scope: vars evaluated recursively; any plugin call gives a fake sample width.
fn scope_value(vars: &[(&str, &str)], src: &str, depth: u32) -> Option<f64> {
    assert!(depth < 16, "var cycle");
    let p = Program::compile(src, Mode::Scalar).ok()?;
    if p.plugin_fns().len() > 0 {
        return Some(3.75e-6);
    }
    let vals: Vec<Option<f64>> = p
        .idents()
        .map(|n| {
            vars.iter()
                .find(|(k, _)| *k == n)
                .and_then(|(_, e)| scope_value(vars, e, depth + 1))
        })
        .collect();
    let mut inputs = Vec::new();
    let mut k = 0;
    p.bind(&mut inputs, |_| {
        let v = vals[k].map(Value::Num);
        k += 1;
        v
    })
    .ok()?;
    match p.eval(&inputs, &mut Scratch::default()).ok()? {
        Value::Num(x) => Some(x),
        Value::Int(i) => Some(i as f64),
        _ => None,
    }
}

#[test]
fn intents_from_a_design() {
    let files: Files = [
        ("schemify.project", "schemify 1 project\n"),
        ("tb.shbench", BENCH),
    ]
    .iter()
    .map(|(p, t)| ((*p).to_owned(), t.as_bytes().to_vec()))
    .collect();
    let d =
        design::load(&files, &BTreeSet::new(), &["tb.shbench"]).unwrap_or_else(|e| panic!("{e:?}"));
    let vars: Vec<(&str, &str)> = vec![
        ("gmid1", "15"),
        ("L1", "0.5u"),
        ("Id1", "10u"),
        ("W1", "gmid.w_at(\"pfet\", gmid1, L1, Id1, 0.6)"),
        ("W2", "W1"),
    ];
    let (got, notes) = retarget::intents(&d, 0, |s| scope_value(&vars, s, 0));
    assert_eq!(
        got,
        [
            Intent {
                inst_id: "m00001".into(),
                class: "nfet".into(),
                gmid: 15.0,
                l: 0.5e-6,
                id: 10e-6,
                vds: None,
                w_sample: Some(3.75e-6),
                nf: 2,
            },
            Intent {
                inst_id: "m00002".into(),
                class: "pfet".into(),
                gmid: 15.0,
                l: 0.5e-6,
                id: 10e-6,
                vds: Some(0.6),
                w_sample: Some(3.75e-6),
                nf: 1,
            },
        ]
    );
    let mut line = String::new();
    interop::sfyx::write_record(&got[0].to_record(3), &mut line);
    assert_eq!(
        line.trim_end(),
        r#"("intent" 3 "nfet" (("gmid" "15") ("l" "5e-7") ("id" "1e-5") ("w" "3.75e-6") ("nf" "2")))"#
    );
    let mut line = String::new();
    interop::sfyx::write_record(&got[1].to_record(1), &mut line);
    assert_eq!(
        line.trim_end(),
        r#"("intent" 1 "pfet" (("gmid" "15") ("l" "5e-7") ("id" "1e-5") ("vds" "0.6") ("w" "3.75e-6") ("nf" "1")))"#
    );
    // M3 (a number) and M9 (no width) give nothing; M4–M8 give one note each.
    let text = notes.text();
    for (id, why) in [
        ("m00004", "wrong number of arguments"),
        ("m00005", "inside a larger expression"),
        ("m00006", "string literal"),
        ("m00007", "`Lx`"),
        ("m00008", "nf `1.5`"),
    ] {
        assert!(
            text.lines().any(|l| l.starts_with("needs_review:retarget")
                && l.contains(id)
                && l.contains(why)),
            "{id}: {text}"
        );
    }
    assert_eq!(notes.rows.len(), 5, "{text}");
    assert!(!text.contains("m00003") && !text.contains("m00009"));
}

// ---------------------------------------------------------------------------------------
// (e) the target map
// ---------------------------------------------------------------------------------------

/// The §10.2 example, verbatim.
const SPEC_MAP: &str = r#"class nmos lib tsmc28 cell nch_lvt_mac
  pin d D
  pin g G
  pin s S
  pin b B
  param w w
  param l l
  param nf nf
  model "include \"/pdk/models/spectre/toplevel.scs\" section=tt"
  device "M0 (d g s b) nch_lvt_mac"
  vdd 0.9
  lgrid 30n
  fingerw_max 2u
"#;

#[test]
fn target_map_parses_the_spec_example_and_rejects_junk() {
    let m = retarget::parse_target_map(SPEC_MAP).unwrap();
    assert_eq!(
        m,
        [TargetClass {
            name: "nmos".into(),
            lib: "tsmc28".into(),
            cell: "nch_lvt_mac".into(),
            pins: [("d", "D"), ("g", "G"), ("s", "S"), ("b", "B")]
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
                .to_vec(),
            params: [("w", "w"), ("l", "l"), ("nf", "nf")]
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
                .to_vec(),
            model: r#"include "/pdk/models/spectre/toplevel.scs" section=tt"#.into(),
            device: "M0 (d g s b) nch_lvt_mac".into(),
            op: "M0".into(),
            vdd: 0.9,
            lgrid: Some(30.0 * 1e-9),
            fingerw_max: Some(2.0 * 1e-6),
            polarity: Polarity::N,
            width: WidthMode::Finger,
        }]
    );
    let two = format!(
        "# comment\n\n{SPEC_MAP}\nclass pmos lib tsmc28 cell pch_lvt_mac\n  param w w\n  param l l\n  model x\n  device \"M0 (d g s b) pch_mac\"\n  op M0.main\n  vdd 900m\n  polarity p\n  width total\n"
    );
    let m = retarget::parse_target_map(&two).unwrap();
    assert_eq!(
        (m[1].polarity, m[1].width, m[1].op.as_str(), m[1].vdd),
        (Polarity::P, WidthMode::Total, "M0.main", 0.9)
    );
    for (bad, want) in [
        ("cls nmos lib a cell b\n", "line 1: unknown record"),
        ("  vdd 1\n", "line 1: attribute outside a class"),
        ("class nmos lib a\n", "line 1: expected `class"),
        (
            "class nmos lib a cell b\n  param w w\n  param l l\n  model m\n  device d\n  vdd 1x\n",
            "line 6: `vdd`: `1x`",
        ),
        (
            "class nmos lib a cell b\n  param w w\n  model m\n  device d\n  vdd 1\n",
            "line 1: class nmos: missing param l",
        ),
        (
            "class nmos lib a cell b\n  param w w\n  param l l\n  model m\n  device d\n  vdd 1\n  colour red\n",
            "line 7: unknown attribute `colour`",
        ),
        (
            "class nmos lib a cell b\n  param w w\n  param w x\n  param l l\n  model m\n  device d\n  vdd 1\n",
            "line 3: param `w` given twice",
        ),
        (
            "class nmos lib a cell b\n  param m m\n  param w w\n  param l l\n  model m\n  device d\n  vdd -1\n",
            "line 2: param `m`",
        ),
        (
            "class nmos lib a cell b\n  param w w\n  param l l\n  model \"unterminated\n  device d\n  vdd 1\n",
            "line 4:",
        ),
    ] {
        let e = retarget::parse_target_map(bad).unwrap_err();
        assert!(e.iter().any(|l| l.starts_with(want)), "{bad:?}: {e:?}");
    }
    let dup = format!("{SPEC_MAP}{SPEC_MAP}");
    assert!(retarget::parse_target_map(&dup).unwrap_err()[0].contains("defined twice"));
    for (s, v) in [
        ("30n", Some(30.0 * 1e-9)),
        ("1meg", Some(1e6)),
        ("0.9", Some(0.9)),
        ("2U", Some(2.0 * 1e-6)),
        ("1e-6", Some(1e-6)),
        ("x", None),
        (".5", None),
        ("-1", None),
        ("1mil", None),
        ("0", None),
    ] {
        assert_eq!(retarget::map_num(s), v, "{s}");
    }
}

/// The SKILL reader of the target map sees what the Rust parser sees.
#[test]
fn skill_reads_the_spec_map_like_rust() {
    let mut i = interp();
    i.files.insert("/t/a.retarget".into(), SPEC_MAP.into());
    let r = i
        .call("SfyRtReadMap", vec![V::str("/t/a.retarget")])
        .unwrap();
    let cls = r.items()[0].items()[0].clone();
    let attr = |k: &str| {
        i_str(&V::list(
            cls.items()
                .iter()
                .find(|row| row.items()[0].as_str() == Some(k))
                .unwrap()
                .items()[1..]
                .to_vec(),
        ))
    };
    assert_eq!(
        attr("model"),
        r#"include "/pdk/models/spectre/toplevel.scs" section=tt"#
    );
    assert_eq!(attr("device"), "M0 (d g s b) nch_lvt_mac");
    let rust = &retarget::parse_target_map(SPEC_MAP).unwrap()[0];
    for (k, want) in [
        ("vdd", rust.vdd),
        ("lgrid", rust.lgrid.unwrap()),
        ("fingerw_max", rust.fingerw_max.unwrap()),
    ] {
        let s = attr(k);
        let got = i.call("SfyRtNum", vec![V::str(&s)]).unwrap();
        assert!(
            matches!(got, V::Float(x) if x == want),
            "{k}: {got:?} vs {want}"
        );
    }
    for bad in ["x", ".5", "1x", "-2"] {
        assert!(
            !i.call("SfyRtNum", vec![V::str(bad)]).unwrap().truthy(),
            "{bad}"
        );
    }
    i.files.insert(
        "/t/b.retarget".into(),
        "class nmos lib a cell b\n  param w w\n  vdd 1\n".into(),
    );
    let r = i
        .call("SfyRtReadMap", vec![V::str("/t/b.retarget")])
        .unwrap();
    assert!(!r.items()[0].truthy(), "missing attributes must be refused");
}

fn i_str(v: &V) -> String {
    v.items()[0].as_str().unwrap().to_owned()
}

// ---------------------------------------------------------------------------------------
// SfyRetarget end to end: mock OCEAN + stub placement (the cells agent provides the real
// SfyPlaceConn / SfySetParams and the database functions)
// ---------------------------------------------------------------------------------------

const STUBS: &str = r#"
(procedure (dbOpenCellViewByType lib cell view vt mode) (list "cv" lib cell view))
(procedure (schCreateInst cv master name xy orient) (list "inst" name xy orient (nth 2 master)))
(procedure (dbSave cv) t)
(procedure (SfySetParams inst params callbacks)
  (setq sfyTestParams (append1 sfyTestParams (list (nth 1 inst) params (nth 4 inst)))) t)
(procedure (SfyPlaceConn cv inst conns)
  (setq sfyTestConns (append1 sfyTestConns (list (nth 1 inst) conns))) (length conns))
"#;

#[test]
fn sfy_retarget_characterizes_solves_and_places() {
    let mut i = interp();
    i.host.ocean.device = Some(ekv);
    i.load(STUBS).unwrap();
    i.set_global("sfyTestParams", V::Nil);
    i.set_global("sfyTestConns", V::Nil);
    let map = format!("{SPEC_MAP}  op M0\n");
    i.files.insert("/w/tsmc.retarget".into(), map);
    let intents = [
        Intent {
            inst_id: "a".into(),
            class: "nmos".into(),
            gmid: 15.0,
            l: 0.5e-6,
            id: 10e-6,
            vds: None,
            w_sample: Some(4e-6),
            nf: 1,
        },
        Intent {
            inst_id: "b".into(),
            class: "nmos".into(),
            gmid: 8.0,
            l: 1e-6,
            id: 200e-6,
            vds: Some(0.3),
            w_sample: None,
            nf: 2,
        },
        Intent {
            inst_id: "c".into(),
            class: "nmos".into(),
            gmid: 40.0,
            l: 1e-6,
            id: 1e-6,
            vds: None,
            w_sample: None,
            nf: 1,
        },
    ];
    let mut sfyx = String::from(
        "(\"sfyx\" 1 \"test\")\n(\"lib\" \"top\")\n(\"cell\" \"amp\" \"design\")\n(\"cv\" \"schematic\" \"schematic\" 160)\n",
    );
    for (k, n) in ["M1", "M2", "M3"].iter().enumerate() {
        sfyx.push_str(&format!(
            "(\"inst\" {} \"{n}\" \"\" \"nmos\" \"\" {} 0 \"R0\")\n",
            k + 1,
            160 * k
        ));
    }
    for k in 1..=3 {
        for (t, net) in [("d", "out"), ("g", "in"), ("s", "0"), ("b", "0")] {
            sfyx.push_str(&format!("(\"conn\" {k} \"{t}\" \"{net}\")\n"));
        }
    }
    for (k, it) in intents.iter().enumerate() {
        interop::sfyx::write_record(&it.to_record(k as i32 + 1), &mut sfyx);
    }
    sfyx.push_str("(\"end\")\n");
    i.files.insert("/w/amp.sfyx".into(), sfyx);
    let ok = i
        .load(r#"(SfyRetarget "/w/amp.sfyx" "/w/tsmc.retarget")"#)
        .unwrap();
    let log = i.files["/w/amp.sfyx.retarget.log"].clone();
    assert!(
        !ok.truthy(),
        "M3 is unreachable, so the run reports failure:\n{log}"
    );
    assert!(
        log.contains("inst M3: gm/Id 40 not reachable") && log.ends_with("FAIL\n"),
        "{log}"
    );
    assert!(i.files.contains_key("/w/amp.sfyx.rt.nmos/netlist"));
    assert!(
        i.files["/w/amp.sfyx.rt.nmos/netlist"].contains("M0 (d g s b) nch_lvt_mac w=1u l=lch nf=1")
    );
    // The Rust reference on the same characterization grid gives the same widths.
    let class = &retarget::parse_target_map(SPEC_MAP).unwrap()[0];
    let ls: Vec<f64> = {
        let mut v: Vec<f64> = intents
            .iter()
            .map(|x| snap_l(x.l, class.lgrid.unwrap()))
            .collect();
        v.sort_by(f64::total_cmp);
        v.dedup();
        v
    };
    let vgs: Vec<f64> = (0..=72).map(|k| (k as f64 * 0.9) / 72.0).collect();
    // The mock reports `device · 1µm`, exactly as `grid` builds its blocks.
    let g = grid(ls, vec![0.3, 0.45], vgs, 0.45, 1e-6);
    let t = g.table();
    let placed = i.load("sfyTestParams").unwrap();
    assert_eq!(placed.items().len(), 2, "{placed:?}");
    for (row, it) in placed.items().iter().zip(&intents) {
        let p = row.items()[1].items();
        let l = snap_l(it.l, class.lgrid.unwrap());
        let s = solve(&t, it.gmid, l, it.id, it.vds).unwrap();
        let nf = fingers(s.w, it.nf, class.fingerw_max);
        let wv = match &p[0].items()[1] {
            V::Float(x) => *x,
            other => panic!("{other:?}"),
        };
        assert_eq!(wv, s.w / f64::from(nf), "same arithmetic, same bits");
        assert!(matches!(&p[1].items()[1], V::Float(x) if *x == l));
        assert_eq!(p[2].items()[1].as_int(), Some(i64::from(nf)));
        assert_eq!(row.items()[2].as_str(), Some("nch_lvt_mac"));
    }
    let conns = i.load("sfyTestConns").unwrap();
    let mut s = String::new();
    skill::print(&conns.items()[0], &mut s);
    assert_eq!(s, r#"("M1" (("D" "out") ("G" "in") ("S" "0") ("B" "0")))"#);
    assert!(
        i.host.ocean.runs == 4,
        "2 lengths x 2 drain voltages: {}",
        i.host.ocean.runs
    );
}
