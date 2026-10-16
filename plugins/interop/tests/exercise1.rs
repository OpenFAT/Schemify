//! The user's Exercise 1 benches (`tests/fixtures/exercise1`, copied from the course
//! project): `virtuoso-export` must give maestro setups and OCEAN scripts that run on a
//! Cadence server with no hand edits (spec `interop.md` §6.6, §9). Each test pins one gap of
//! the first export's report with literal expected text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use interop::job::{Command, Job, Step};
use interop::sfyx::{self, Record};
use interop::virtuoso::ExportOptions;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exercise1")
}

const BENCHES: [&str; 4] = [
    "exercise1/dc_vgs.shbench",
    "exercise1/dc_vds.shbench",
    "exercise1/ac_vgs.shbench",
    "exercise1/cap_w.shbench",
];

/// Runs the export job over the fixture project (the CLI's driver, reading the disk) and
/// returns every written file by path.
fn export() -> BTreeMap<String, String> {
    let opts = ExportOptions {
        producer: "test".into(),
        ..ExportOptions::default()
    };
    let mut job = Job::new(Command::VirtuosoExport {
        tops: BENCHES.map(String::from).to_vec(),
        out: "cadence/exercise1.sfyx".into(),
        opts,
    });
    for _ in 0..64 {
        match job.step() {
            Step::Read(paths) => {
                for p in paths {
                    job.supply(&p, std::fs::read(root().join(&p)).ok());
                }
            }
            Step::Done { writes, .. } => {
                return writes
                    .into_iter()
                    .map(|(p, b)| (p, String::from_utf8(b).unwrap()))
                    .collect();
            }
            Step::Failed(p) => panic!("{p:?}"),
        }
    }
    panic!("the job did not finish");
}

fn records(files: &BTreeMap<String, String>) -> Vec<Record> {
    sfyx::read(&files["cadence/exercise1.sfyx"]).unwrap()
}

fn line(r: &Record) -> String {
    let mut s = String::new();
    sfyx::write_record(r, &mut s);
    s.trim_end().to_owned()
}

/// The `prop` records of instance `name` in cell `cell`, as `name=value`.
fn inst_props(recs: &[Record], cell: &str, name: &str) -> Vec<String> {
    let mut cur = String::new();
    let mut r = None;
    let mut out = Vec::new();
    for x in recs {
        match x.tag.as_str() {
            "cell" => cur = x.str(0).unwrap().to_owned(),
            "inst" if cur == cell && x.str(1).unwrap() == name => r = Some(x.int(0).unwrap()),
            "cv" => r = None,
            "prop" if cur == cell && Some(x.int(0).unwrap()) == r => {
                let n = x.str(1).unwrap();
                if n != "schemifyId" {
                    out.push(format!("{n}={}", x.str(3).unwrap()));
                }
            }
            _ => {}
        }
    }
    out
}

fn report(files: &BTreeMap<String, String>) -> String {
    files["cadence/exercise1.report.txt"].clone()
}

#[test]
fn sources_with_design_variables_map_to_cdf_params() {
    let f = export();
    let recs = records(&f);
    assert_eq!(
        inst_props(&recs, "ac_vgs", "VGNA"),
        ["acm=1", "srcType=dc", "vdc=VGS"]
    );
    assert_eq!(
        inst_props(&recs, "ac_vgs", "VDD"),
        ["srcType=dc", "vdc=VDD"]
    );
    assert_eq!(
        inst_props(&recs, "ac_vgs", "VDPA"),
        ["acm=1", "srcType=dc", "vdc=-VDS"]
    );
    assert_eq!(
        inst_props(&recs, "cap_w", "IREFN"),
        ["idc=IDW*W/1u", "srcType=dc"]
    );
    assert!(
        !report(&f).contains("needs_review:source"),
        "{}",
        report(&f)
    );
}

/// The records of maestro test `test`, one line each.
fn maestro_of(recs: &[Record], test: &str) -> Vec<String> {
    let start = recs
        .iter()
        .position(|r| r.tag == "maestro" && r.str(0).unwrap() == test)
        .unwrap_or_else(|| panic!("no maestro test {test}"));
    recs[start..]
        .iter()
        .enumerate()
        .take_while(|(k, r)| *k == 0 || (r.tag != "maestro" && r.tag != "end"))
        .map(|(_, r)| line(r))
        .collect()
}

#[test]
fn outputs_specs_and_models_map() {
    let f = export();
    let recs = records(&f);
    let dc = maestro_of(&recs, "dc_vgs");
    for want in [
        r#"("model" "tt" "$PDK_MODELS" "tt")"#,
        r#"("output" "ION_n" "point" "abs(value(IS(\"/VMDN/p\") VAR(\"VDD\")))/VAR(\"W\")*1u")"#,
        r#"("output" "rg_n" "point" "1/abs(value(deriv(IS(\"/VMGN/p\")) VAR(\"VDD\")))")"#,
        r#"("spec" "SSW_n" "gt" "59.5")"#,
        r#"("var" "p" "0,1,2,3,4" "")"#,
    ] {
        assert!(dc.iter().any(|l| l == want), "{want}\n{dc:#?}");
    }
    let ac = maestro_of(&recs, "ac_vgs");
    for want in [
        r#"("output" "gm_n" "point" "abs(real(IF(\"/VMDNG/p\")))")"#,
        r#"("output" "fT_n" "point" "abs(real(IF(\"/VMDNG/p\")))*xval(IF(\"/VMDNG/p\"))/abs(imag(IF(\"/VMGNG/p\")))")"#,
    ] {
        assert!(ac.iter().any(|l| l == want), "{want}\n{ac:#?}");
    }
    // Gap 11: the 81-point VGS sweep is the decimal grid, no binary noise.
    let vgs = ac
        .iter()
        .find(|l| l.starts_with(r#"("var" "VGS""#))
        .unwrap();
    assert!(
        vgs.starts_with(r#"("var" "VGS" "0,0.01,0.02,0.03,"#),
        "{vgs}"
    );
    assert!(!vgs.contains("0000"), "{vgs}");
    // Every output and spec of the four benches maps: only cosmetic rows remain.
    let rep = report(&f);
    for class in ["output", "spec", "source", "sweep"] {
        assert!(!rep.contains(&format!("needs_review:{class}")), "{rep}");
    }
}

/// The OCEAN scripts (§9.6) equal their goldens (`SCHEMIFY_BLESS=1` rewrites them), and
/// every line is a comment or one call of the OCEAN commands the spec lists, with balanced
/// parentheses and quotes.
#[test]
fn ocean_scripts_match_their_goldens() {
    let f = export();
    let gold = root().join("ocean");
    let bless = std::env::var_os("SCHEMIFY_BLESS").is_some();
    const CALLS: [&str; 14] = [
        "createDir(",
        "simulator(",
        "design(",
        "resultsDir(",
        "modelFile(",
        "definitionFile(",
        "desVar(",
        "temp(",
        "analysis(",
        "save(",
        "paramAnalysis(",
        "paramRun(",
        "run(",
        "ocnPrint(",
    ];
    let mut n = 0;
    for (p, text) in &f {
        let Some(rel) = p.strip_prefix("cadence/ocean/") else {
            continue;
        };
        n += 1;
        let path = gold.join(rel);
        if bless {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        let want = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(text, &want, "{rel} (SCHEMIFY_BLESS=1 rewrites)");
        if !rel.ends_with(".ocn") {
            continue;
        }
        // A statement may continue on the next line (nested paramAnalysis).
        let mut depth = 0i32;
        for l in text.lines() {
            if depth == 0 && !l.starts_with(';') && l != "exit()" {
                assert!(CALLS.iter().any(|c| l.starts_with(c)), "{rel}: {l}");
            }
            let mut quoted = false;
            let mut esc = false;
            for ch in l.chars() {
                match ch {
                    _ if esc => esc = false,
                    '\\' if quoted => esc = true,
                    '"' => quoted = !quoted,
                    '(' if !quoted && !l.starts_with(';') => depth += 1,
                    ')' if !quoted && !l.starts_with(';') => depth -= 1,
                    _ => {}
                }
            }
            assert!(!quoted && depth >= 0, "{rel}: {l}");
        }
        assert_eq!(depth, 0, "{rel}");
        assert!(text.ends_with("exit()\n"), "{rel}");
    }
    assert_eq!(n, 5, "four scripts and cap_w's definition file");
}
