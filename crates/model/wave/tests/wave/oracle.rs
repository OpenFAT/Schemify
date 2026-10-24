//! Oracle 1 (docs/dev/testing.md): binary and ASCII `.raw` of the same simulation agree. Two independent
//! checks of the decoded numbers back it up: ngspice and Xyce agree on the same deck, and the
//! series RLC response matches its closed form.

use std::path::Path;
use std::process::Command;

use schemify_wave::{RawPlot, VarKind, group_datasets, parse_raw};

use crate::{close, fixture};

/// Fixture name infix and the relative precision of that simulator's ASCII output: ngspice
/// prints `%.15e` (16 significant digits), Xyce `%.8e` (9).
const FLAVOURS: [(&str, f64); 2] = [("", 1e-14), (".xyce", 1e-8)];
const DECKS: [&str; 3] = ["rc_tran", "rlc_ac", "dc_step"];

fn assert_same(bin: &[RawPlot], asc: &[RawPlot], rtol: f64, what: &str) {
    assert_eq!(bin.len(), asc.len(), "{what}: block count");
    for (k, (b, a)) in bin.iter().zip(asc).enumerate() {
        // Xyce's binary and ASCII writers word `Plotname:` differently for `.step` runs.
        if !what.contains("xyce") {
            assert_eq!(b.name(), a.name(), "{what}[{k}]: plot name");
        }
        assert_eq!(b.vars(), a.vars(), "{what}[{k}]: variables");
        assert_eq!(b.is_complex(), a.is_complex(), "{what}[{k}]: real/complex");
        assert_eq!(b.len(), a.len(), "{what}[{k}]: points");
        assert_eq!(b.steps(), a.steps(), "{what}[{k}]: sweep steps");
        for v in 0..b.vars().len() {
            for (part, (x, y)) in [(b.col(v), a.col(v)), (b.col_im(v), a.col_im(v))]
                .into_iter()
                .enumerate()
            {
                assert_eq!(x.len(), y.len());
                for (p, (x, y)) in x.iter().zip(y).enumerate() {
                    assert!(
                        close(*x, *y, rtol),
                        "{what}[{k}] {} part {part} point {p}: {x} vs {y}",
                        b.vars()[v].name
                    );
                }
            }
        }
    }
}

#[test]
fn binary_and_ascii_agree() {
    for (flavour, rtol) in FLAVOURS {
        for deck in DECKS {
            let bin = fixture(&format!("{deck}{flavour}.bin.raw"));
            let asc = fixture(&format!("{deck}{flavour}.ascii.raw"));
            assert_same(&bin, &asc, rtol, &format!("{deck}{flavour}"));
        }
    }
}

#[test]
fn fixtures_have_the_expected_shape() {
    let ac = &fixture("rlc_ac.bin.raw")[0];
    assert!(ac.is_complex() && ac.len() == 101 && ac.vars()[0].kind == VarKind::Frequency);
    let xac = &fixture("rlc_ac.xyce.ascii.raw")[0];
    assert!(xac.is_complex() && xac.vars()[4].name == "OUT");
    // ngspice: a nested `.dc` (3 steps in one block) plus two appended runs.
    let ng = fixture("dc_step.ascii.raw");
    assert_eq!(ng.len(), 3);
    assert_eq!(ng[0].steps(), &[0..41, 41..82, 82..123]);
    let g = group_datasets(ng);
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].steps().len(), 5);
    assert_eq!(g[0].len(), 123 + 21 + 21);
    // Xyce: one block per `.step`.
    let xy = group_datasets(fixture("dc_step.xyce.bin.raw"));
    assert_eq!((xy.len(), xy[0].steps()), (1, &[0..21, 21..42, 42..63][..]));
}

/// Complex `(re, im)` of the analytic series-RLC transfer `Zc / (R + ZL + Zc)` at `f` Hz.
fn rlc(f: f64) -> (f64, f64) {
    let (r, l, c) = (10.0, 10e-6, 100e-9);
    let w = 2.0 * std::f64::consts::PI * f;
    // Zc = -j/(wC); den = R + j(wL - 1/(wC)); H = Zc / den.
    let (zc_re, zc_im) = (0.0, -1.0 / (w * c));
    let (d_re, d_im) = (r, w * l - 1.0 / (w * c));
    let m = d_re * d_re + d_im * d_im;
    (
        (zc_re * d_re + zc_im * d_im) / m,
        (zc_im * d_re - zc_re * d_im) / m,
    )
}

#[test]
fn ac_matches_closed_form_in_both_simulators() {
    for (file, out) in [("rlc_ac.bin.raw", "v(out)"), ("rlc_ac.xyce.bin.raw", "OUT")] {
        let p = &fixture(file)[0];
        let v = p.find_var(out).expect("output column");
        for k in 0..p.len() {
            let (re, im) = rlc(p.scale()[k]);
            let (sre, sim) = (p.col(v)[k], p.col_im(v)[k]);
            let mag = re.hypot(im);
            assert!(
                (sre - re).hypot(sim - im) <= 1e-6 * mag,
                "{file} f={}: sim ({sre}, {sim}) vs closed form ({re}, {im})",
                p.scale()[k]
            );
        }
    }
}

#[test]
fn ngspice_and_xyce_agree_on_the_rc_transient() {
    let ng = &fixture("rc_tran.bin.raw")[0];
    let xy = &fixture("rc_tran.xyce.bin.raw")[0];
    let (vn, vx) = (ng.find_var("v(out)").unwrap(), xy.find_var("OUT").unwrap());
    let at = |p: &RawPlot, v: usize, t: f64| {
        let x = p.scale();
        let k = x.partition_point(|&s| s < t);
        let (x0, x1, y0, y1) = (x[k - 1], x[k], p.col(v)[k - 1], p.col(v)[k]);
        y0 + (y1 - y0) * (t - x0) / (x1 - x0)
    };
    for t in [0.5e-6, 3e-6, 6.02e-6, 9e-6, 13e-6, 19.5e-6] {
        let (a, b) = (at(ng, vn, t), at(xy, vx, t));
        assert!((a - b).abs() < 2e-3, "t={t}: ngspice {a} vs Xyce {b}");
    }
}

/// Reruns the simulators on the decks and checks the fresh outputs the same way, so the
/// fixtures can be regenerated (copy the outputs over `tests/fixtures/`) and the oracle holds
/// for the installed versions too. Needs `ngspice` on PATH; Xyce via `$XYCE` or `Xyce` on PATH.
#[test]
#[ignore = "runs ngspice/Xyce; `cargo test -p schemify-wave -- --ignored regenerate`"]
fn regenerate_and_compare() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("wave-raw");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for deck in DECKS {
        std::fs::copy(
            src.join(format!("{deck}.cir")),
            dir.join(format!("{deck}.cir")),
        )
        .unwrap();
        let ok = Command::new("ngspice")
            .args(["-b", &format!("{deck}.cir")])
            .current_dir(&dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "ngspice failed on {deck}");
        let xyce = std::env::var("XYCE").unwrap_or_else(|_| "Xyce".into());
        let cir = format!("{deck}.xyce.cir");
        std::fs::copy(src.join(&cir), dir.join(&cir)).unwrap();
        for (out, extra) in [("bin", None), ("ascii", Some("-a"))] {
            let mut c = Command::new(&xyce);
            c.args(["-r", &format!("{deck}.xyce.{out}.raw")]);
            c.args(extra);
            let ok = c
                .arg(&cir)
                .current_dir(&dir)
                .output()
                .map(|o| o.status.success());
            assert!(ok.unwrap_or(false), "Xyce failed on {deck}");
        }
        for (flavour, rtol) in FLAVOURS {
            let read = |kind: &str| {
                let f = dir.join(format!("{deck}{flavour}.{kind}.raw"));
                parse_raw(&std::fs::read(&f).unwrap()).unwrap()
            };
            let (bin, asc) = (read("bin"), read("ascii"));
            assert_same(&bin, &asc, rtol, &format!("fresh {deck}{flavour}"));
            let old = fixture(&format!("{deck}{flavour}.bin.raw"));
            assert_eq!(old.len(), bin.len(), "{deck}{flavour}: block count changed");
        }
    }
}
