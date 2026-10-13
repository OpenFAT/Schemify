//! ESPice against ngspice on a real PDK (README "The sweep"): the same FreePDK45 devices
//! characterized both ways through the plugin's own config parser, netlists, parsers and LUT
//! builder, with `std::process` in place of the host's `spawn` (cwd = the project root, as the
//! app runs it). Ignored: needs `ngspice` and `espice` on PATH and `GMID_FREEPDK45_LIB`, the
//! `corners.lib` of a pdk-manager FreePDK45 install (`.schemify/pdks/freepdk45/<h>/models/`).
//!
//! Tolerances (espice.md §4c): Id agrees to 1e-5 (median) where it is not leakage (ESPice's is
//! the terminal current, ngspice's `@m[id]` the channel current: they part in deep
//! subthreshold); gm and gds loosely (AC-derived and terminal vs the model's OP values); Cgg
//! from the gate current includes the overlap capacitances, so it is larger than ngspice's
//! intrinsic `@m[cgg]` by a factor in (1, 4).

use std::path::Path;
use std::process::Command;

use gmid_lut::chars::{self, Block, Sim};

fn run(dir: &Path, argv: &[String]) {
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("{}: {e}", argv[0]));
    assert!(
        out.status.success(),
        "{} failed:\n{}{}",
        argv[0],
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Characterizes `dev` of `config` in `dir` and returns its blocks.
fn blocks(dir: &Path, config: &str, dev: &str) -> (chars::Sweep, Vec<Block>) {
    let d = chars::parse(config).unwrap();
    let sweep = d
        .into_iter()
        .find(|d| d.name == dev)
        .unwrap()
        .sweep
        .unwrap();
    std::fs::write(
        dir.join(chars::netlist_path(dev)),
        chars::netlist(dev, &sweep),
    )
    .unwrap();
    let t = std::time::Instant::now();
    run(dir, &chars::spawn_argv(dev, &sweep));
    eprintln!(
        "{dev}: {} in {:.2} s",
        sweep.sim.program(),
        t.elapsed().as_secs_f64()
    );
    let b = match sweep.sim {
        Sim::Ngspice => (1..=sweep.l_um.len())
            .map(|n| {
                let data = std::fs::read_to_string(dir.join(chars::data_path(dev, n))).unwrap();
                chars::parse_block(&data).unwrap()
            })
            .collect(),
        Sim::Espice => {
            let raw = std::fs::read(dir.join(chars::raw_path(dev))).unwrap();
            eprintln!("{dev}: raw file {} bytes", raw.len());
            chars::parse_espice(&sweep, &raw).unwrap()
        }
    };
    chars::build(dev, &sweep, &b).unwrap();
    (sweep, b)
}

fn median(mut v: Vec<f64>) -> f64 {
    assert!(!v.is_empty());
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[test]
#[ignore = "needs ngspice, espice and GMID_FREEPDK45_LIB"]
fn espice_matches_ngspice_on_freepdk45() {
    let lib = std::env::var("GMID_FREEPDK45_LIB").expect("GMID_FREEPDK45_LIB");
    let dir = std::env::temp_dir().join(format!("gmid-espice-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(chars::WORK_DIR)).unwrap();
    std::fs::create_dir_all(dir.join("gmid")).unwrap();
    std::fs::write(dir.join("gmid/fp45_tt.sp"), format!(".lib \"{lib}\" tt\n")).unwrap();
    let mut config = String::new();
    for (pol, model) in [("n", "NMOS_VTL"), ("p", "PMOS_VTL")] {
        for sim in ["ngspice", "espice"] {
            config.push_str(&format!(
                "device {pol}_{sim}\n  deck gmid/fp45_tt.sp\n  inst \"M{{n}} d g s b {model} w={{w}}u l={{l}}u\"\n  \
                 op @m{{n}}\n  sim {sim}\n  polarity {pol}\n  vdd 0.8\n  l 0.05 0.2 1\n\n"
            ));
        }
    }
    for pol in ["n", "p"] {
        let (s, ng) = blocks(&dir, &config, &format!("{pol}_ngspice"));
        let (_, es) = blocks(&dir, &config, &format!("{pol}_espice"));
        let mut rel: [Vec<f64>; 4] = Default::default();
        let mut ratio_cgg = Vec::new();
        for (a, b) in ng.iter().zip(&es) {
            assert_eq!(a.scale.len(), b.scale.len());
            for (r, (qa, qb)) in a.q.iter().zip(&b.q).enumerate() {
                assert!((a.scale[r] - b.scale[r]).abs() < 1e-9, "same VGS grid");
                // Above 1 µA per µm the channel current dominates the leakage.
                if qa[0].abs() < 1e-6 * s.w_um {
                    continue;
                }
                for k in 0..3 {
                    rel[k].push((qa[k].abs() - qb[k].abs()).abs() / qa[k].abs());
                }
                ratio_cgg.push(qb[3].abs() / qa[3].abs());
            }
        }
        let worst = |v: &[f64]| v.iter().copied().fold(0.0, f64::max);
        let (id, gm, gds) = (
            median(rel[0].clone()),
            median(rel[1].clone()),
            median(rel[2].clone()),
        );
        let cgg = median(ratio_cgg.clone());
        eprintln!(
            "{pol}: {} points; id median {id:.2e} worst {:.2e}; gm median {gm:.2e} worst {:.2e}; \
             gds median {gds:.2e} worst {:.2e}; cgg espice/ngspice median {cgg:.3} range {:.3}..{:.3}",
            rel[0].len(),
            worst(&rel[0]),
            worst(&rel[1]),
            worst(&rel[2]),
            ratio_cgg.iter().copied().fold(f64::INFINITY, f64::min),
            worst(&ratio_cgg),
        );
        // Measured 2026-10-12 (ESPice 1.0.0, ngspice 44.2): n median 8.9e-6, p 2.2e-5 (§4c
        // measured NMOS only). The worst n points (≈ 1.4e-3, L = 1 µm, linear region) are
        // definitional: there ngspice's own terminal current i(vd) = 1.062016e-5 matches
        // ESPice's 1.062021e-5 while `@m[id]` = 1.063035e-5 (VGS 0.7, VDS 0.067).
        let id_max = if pol == "n" { 1e-5 } else { 5e-5 };
        assert!(id <= id_max && worst(&rel[0]) <= 1e-2, "id {id}");
        assert!(gm <= 1e-4 && gds <= 1e-3, "gm {gm} gds {gds}");
        assert!(cgg > 1.0 && cgg < 4.0, "cgg ratio {cgg}");
    }
}

/// ASAP7 on ESPice (README "ASAP7"): the deck loads the kit's BSIM-CMG with `.hdl` (no OSDI),
/// the instance grounds the 5th (thermal) port. Oracle: ngspice + OpenVAF OSDI gave
/// Id = 6.284561e-5 A for `nmos_rvt nfin=14` at VGS = VDS = 0.4 V (espice.md §4c, both
/// simulators). Ignored: needs `espice` (with `ZIG`) and `GMID_ASAP7_PDK`, a pdk-manager ASAP7
/// install; the first BSIM-CMG compile takes ≈ 4 min and 2.8 GB.
#[test]
#[ignore = "needs espice, ZIG and GMID_ASAP7_PDK; minutes on a cold ESPice cache"]
fn espice_characterizes_asap7() {
    let pdk = std::env::var("GMID_ASAP7_PDK").expect("GMID_ASAP7_PDK");
    let dir = std::env::temp_dir().join(format!("gmid-asap7-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(chars::WORK_DIR)).unwrap();
    std::fs::create_dir_all(dir.join("gmid")).unwrap();
    std::fs::write(
        dir.join("gmid/asap7_tt.sp"),
        format!(
            ".hdl \"{pdk}/va/code/bsimcmg/vacode/bsimcmg.va\"\n.lib \"{pdk}/models/corners.lib\" tt\n"
        ),
    )
    .unwrap();
    let config = "device n7\n  deck gmid/asap7_tt.sp\n  inst \"N{n} d g s b 0 nmos_rvt nfin=14 l={l}u\"\n  \
                  sim espice\n  vdd 0.8\n  l 0.021\n  w 0.987\n";
    let (s, b) = blocks(&dir, config, "n7");
    let lut = chars::build("n7", &s, &b).unwrap();
    // VGS 0.4 V is point 36 of 0..=72, VDS 0.4 V point 5 of 1..=12.
    let id = b[0].q[5 * 73 + 36][0];
    eprintln!(
        "asap7 nmos_rvt nfin=14: id(0.4, 0.4) = {id:e}; {}",
        lut.provenance
    );
    assert!((id - 6.284561e-5).abs() <= 1e-5 * 6.284561e-5, "id {id}");
}
