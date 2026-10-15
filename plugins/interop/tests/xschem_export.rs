//! Oracles of the xschem export (spec `interop.md` §11):
//!
//! 1. **Round trip.** Every fixture project (the app and elab fixtures, plus generated
//!    benches of `core` devices in every orientation) is exported, re-imported with
//!    `interop::translate_library` (search roots: the export and xschem's system `devices`
//!    library when an `xschem` binary is on PATH, else the interface stand-ins in
//!    `tests/fixtures/xschem_export/devices`), and both the original and the re-import are
//!    elaborated and netlisted; `spicecmp` must find them equivalent.
//! 2. **Real xschem.** With `xschem` on PATH, the exported top is netlisted by xschem
//!    (`-q -x -n -s -r --rcfile <export>/xschemrc --netlist_path …`) and compared with
//!    Schemify's own ngspice netlist of the original. Without the binary the test prints a
//!    skip note and passes, unless `SCHEMIFY_REQUIRE_XSCHEM=1` (then it fails).
//! 3. **Negative space.** Untranslatable values, templates and bench records produce notes
//!    and leave the xschem default (no attribute written).
//! 4. **Determinism.** The same Design exports to the same bytes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use interop::design::{self, Design, Files};
use interop::xschem_out::{XschemExport, export};
use interop::{Options, translate_library};
use schemify_doc::{DocHandle, Project, core_file, core_library};
use schemify_elab::{Dialect, Request, Severity, elaborate, no_funcs};
use schemify_format::{Orient, RecordKind, format_coord, parse};
use schemify_netlist::emit;
use schemify_spicecmp::{Options as CmpOptions, compare};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ------------------------------------------------------------------------------ fixtures

struct Fixture {
    name: String,
    files: Files,
    /// Project-relative path of the top cell.
    top: String,
}

/// Every cell file and the manifest under `root`, project-relative.
fn read_project(root: &Path) -> Files {
    let mut out = Files::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel == "schemify.project"
                || [".shprim", ".shcomp", ".shbench"]
                    .iter()
                    .any(|x| rel.ends_with(x))
            {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

fn fixtures() -> Vec<Fixture> {
    let repo = crate_dir().join("../..");
    let mut v: Vec<Fixture> = [
        ("app-rc", "crates/app/tests/fixtures/rc", "tb.shbench"),
        ("app-inv", "crates/app/tests/fixtures/inv", "top.shcomp"),
        ("app-ota", "crates/app/tests/fixtures/ota", "ota.shcomp"),
        (
            "elab-inv_tb",
            "crates/elab/tests/fixtures",
            "inv_tb.shbench",
        ),
        (
            "elab-bus_tb",
            "crates/elab/tests/fixtures",
            "bus_tb.shbench",
        ),
        (
            "elab-opamp_tb",
            "crates/elab/tests/fixtures",
            "opamp_tb.shbench",
        ),
    ]
    .iter()
    .map(|(name, dir, top)| Fixture {
        name: (*name).to_owned(),
        files: read_project(&repo.join(dir)),
        top: (*top).to_owned(),
    })
    .collect();
    for seed in 0..8 {
        v.push(generated(seed));
    }
    v
}

/// splitmix64.
struct Mix(u64);

impl Mix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Core devices with overrides that differ from both the core and xschem defaults.
/// `(core cell, instance-name prefix, overrides)`.
type Device = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

const DEVICES: &[Device] = &[
    ("res", "R", &[("value", "10k")]),
    ("cap", "C", &[("value", "2p"), ("m", "2")]),
    ("ind", "L", &[("value", "3n")]),
    ("vsource", "V", &[("value", "\"dc 1.8 ac 1\"")]),
    ("isource", "I", &[("value", "1m")]),
    ("diode", "D", &[("model", "dmod"), ("area", "2")]),
    ("nmos4", "M", &[("model", "nch"), ("l", "0.5u")]),
    ("pmos4", "M", &[("w", "4u")]),
    ("npn", "Q", &[("model", "qmod")]),
    ("vcvs", "E", &[("value", "10")]),
];

const ORIENTS: [Orient; 8] = [
    Orient::R0,
    Orient::R90,
    Orient::R180,
    Orient::R270,
    Orient::MR0,
    Orient::MR90,
    Orient::MR180,
    Orient::MR270,
];

/// A bench of core devices: seed 0 places every device in every orientation, other seeds
/// a random handful. Every pin gets a label (directly, or at the end of a short wire) with
/// a random net name, so a wrong orientation inversion changes connectivity.
fn generated(seed: u64) -> Fixture {
    let mut rng = Mix(seed.wrapping_mul(7919) + 1);
    let mut picks: Vec<(usize, Orient)> = Vec::new();
    if seed == 0 {
        for d in 0..DEVICES.len() {
            for o in ORIENTS {
                picks.push((d, o));
            }
        }
    } else {
        for _ in 0..3 + rng.below(6) {
            picks.push((rng.below(DEVICES.len()), ORIENTS[rng.below(8)]));
        }
    }
    let nets = ["a", "b", "c", "0"];
    let mut ids = 0u32;
    let mut id = || {
        ids += 1;
        format!("{ids:06}")
    };
    let (mut insts, mut wires, mut labels) = (String::new(), String::new(), String::new());
    let c = |v: i32| {
        let mut s = String::new();
        format_coord(v, &mut s);
        s
    };
    for (k, &(dev, o)) in picks.iter().enumerate() {
        let (cell, prefix, over) = DEVICES[dev];
        let at = [(k as i32 % 10) * 200_000, (k as i32 / 10) * 200_000];
        insts.push_str(&format!(
            "inst {} core:{cell}\n  at {} {} {}\n  name {prefix}{k}\n",
            id(),
            c(at[0]),
            c(at[1]),
            o.as_str()
        ));
        for (p, v) in over {
            insts.push_str(&format!("  {p} {v}\n"));
        }
        let doc = parse(core_file(cell).unwrap()).unwrap();
        for r in 0..doc.record_count() {
            if doc.record_kind(r) != RecordKind::Pin {
                continue;
            }
            let pc = |i| schemify_format::parse_coord(doc.field(r, i).unwrap()).unwrap();
            let p = interop::translate::place([pc(2), pc(3)], o, at).unwrap();
            let net = nets[rng.below(nets.len())];
            let mut q = p;
            if rng.below(2) == 0 {
                q = [p[0] + 30_000, p[1] - 10_000];
                let (a, b) = if p <= q { (p, q) } else { (q, p) };
                wires.push_str(&format!(
                    "wire {} {} {} {} {}\n",
                    id(),
                    c(a[0]),
                    c(a[1]),
                    c(b[0]),
                    c(b[1])
                ));
            }
            labels.push_str(&format!(
                "label {} {} {} R0 {net}\n",
                id(),
                c(q[0]),
                c(q[1])
            ));
        }
    }
    let text = format!(
        "schemify 1 shbench\n  doc \"generated core bench {seed}\"\n\n{insts}\n{wires}\n{labels}\ncode op spice\n  | .op\n"
    );
    let mut files = Files::new();
    files.insert("tb.shbench".to_owned(), text.into_bytes());
    Fixture {
        name: format!("gen-{seed}"),
        files,
        top: "tb.shbench".to_owned(),
    }
}

// ------------------------------------------------------------------------------ helpers

/// Loads the closure of `top` from `files` (every path not in `files` is absent).
fn load_design(files: &Files, top: &str) -> Design {
    design::load_closed(files, &[top]).unwrap_or_else(|p| panic!("{top}: {p:?}"))
}

/// A Schemify project over `files` (project-relative paths), every cell opened.
fn project(files: &Files) -> (Project, BTreeMap<String, DocHandle>) {
    let manifest = files.get("schemify.project").map(Vec::as_slice);
    let libs = manifest.map_or_else(BTreeMap::new, |m| design::manifest_libs(m).unwrap());
    let mut p = Project::new(manifest, None, 7).unwrap();
    // library → (library-relative path, bytes, project path)
    type Listing<'a> = Vec<(String, &'a [u8], String)>;
    let mut groups: BTreeMap<&str, Listing> = BTreeMap::new();
    for (path, bytes) in files {
        if ![".shprim", ".shcomp", ".shbench"]
            .iter()
            .any(|x| path.ends_with(x))
        {
            continue;
        }
        let lib = libs
            .iter()
            .find(|(_, d)| path.starts_with(&format!("{d}/")))
            .map(|(n, d)| (n.as_str(), &path[d.len() + 1..]));
        let (lib, rel) = lib.unwrap_or(("", path.as_str()));
        groups
            .entry(lib)
            .or_default()
            .push((rel.to_owned(), bytes.as_slice(), path.clone()));
    }
    p.index_library("core", "builtin", core_library()).unwrap();
    let mut handles = BTreeMap::new();
    for (lib, list) in &groups {
        let listing: Vec<(&str, &[u8])> = list.iter().map(|(r, b, _)| (r.as_str(), *b)).collect();
        p.index_library(lib, "", &listing).unwrap();
    }
    for (lib, list) in &groups {
        for (rel, bytes, path) in list {
            let o = p
                .open(lib, rel, bytes, None)
                .unwrap_or_else(|e| panic!("{path}: {e:?}"));
            handles.insert(path.clone(), o.handle);
        }
    }
    (p, handles)
}

/// The ngspice netlist of `top`, and its blocking violations.
fn netlist(files: &Files, top: &str) -> (String, Vec<String>) {
    let (p, h) = project(files);
    let req = Request {
        config: None,
        corner: None,
        overrides: &[],
        env: &[],
        dialect: Dialect::Ngspice,
    };
    let e = elaborate(&p, h[top], &req, &mut no_funcs);
    let errors = e
        .violations
        .rows
        .iter()
        .filter(|v| v.code.severity() == Severity::Error)
        .map(|v| format!("{} {}", v.code.as_str(), v.detail))
        .collect();
    (emit(&e, Dialect::Ngspice).text, errors)
}

/// The xschem binary on PATH.
fn xschem_bin() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("xschem"))
        .find(|p| p.is_file())
}

/// xschem's system `devices` library: `(root path, files)` as import inputs, or the
/// stand-ins this test ships.
fn devices() -> (String, Vec<(String, Vec<u8>)>) {
    let system = xschem_bin()
        .and_then(|b| std::fs::canonicalize(b).ok())
        .map(|b| {
            b.parent()
                .unwrap()
                .join("../share/xschem/xschem_library/devices")
        })
        .filter(|d| d.is_dir());
    let dir = system.unwrap_or_else(|| crate_dir().join("tests/fixtures/xschem_export/devices"));
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "sym"))
        .map(|e| {
            (
                format!("sys/devices/{}", e.file_name().to_string_lossy()),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    files.sort();
    ("sys/devices".to_owned(), files)
}

/// The export path of the top cell (`<dir>/<name>.sch`).
fn exported_top(x: &XschemExport, top: &str) -> String {
    let stem = top.rsplit('/').next().unwrap().rsplit_once('.').unwrap().0;
    x.files
        .iter()
        .map(|f| &f.0)
        .find(|p| p.rsplit('/').next() == Some(&format!("{stem}.sch")))
        .unwrap_or_else(|| panic!("no {stem}.sch exported"))
        .clone()
}

fn export_fixture(f: &Fixture) -> XschemExport {
    let d = load_design(&f.files, &f.top);
    let top = d.find(&f.top).unwrap();
    export(&d, &[top]).unwrap_or_else(|e| panic!("{}: {e:?}", f.name))
}

// ------------------------------------------------------------------------------ oracles

#[test]
fn round_trip_netlists_agree() {
    let (dev_root, dev_files) = devices();
    let mut failures = Vec::new();
    for f in fixtures() {
        let x = export_fixture(&f);
        let mut inputs: Vec<(String, Vec<u8>)> = x
            .files
            .iter()
            .map(|(p, b)| (format!("out/{p}"), b.clone()))
            .collect();
        inputs.extend(dev_files.iter().cloned());
        let refs: Vec<(&str, &[u8])> = inputs
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let roots = ["out".to_owned(), dev_root.clone()];
        let opts = Options {
            roots: &roots,
            ..Options::default()
        };
        let (out, report) = translate_library(&refs, &opts);
        let back: Files = out.into_iter().collect();
        // The re-imported top: same directory and stem, the original's kind.
        let sch = exported_top(&x, &f.top);
        let ext = f.top.rsplit_once('.').unwrap().1;
        let want = format!("{}.{ext}", sch.strip_suffix(".sch").unwrap());
        if !back.contains_key(&want) {
            failures.push(format!(
                "{}: re-import has no {want}: {:?}",
                f.name,
                back.keys()
            ));
            continue;
        }
        let (orig, e1) = netlist(&f.files, &f.top);
        let (re, e2) = netlist(&back, &want);
        let r = compare(&orig, &re, &CmpOptions::default());
        eprintln!(
            "{}: {} notes, {} devices, equal={}",
            f.name,
            x.notes.rows.len(),
            r.devices[0],
            r.is_equal()
        );
        if !r.is_equal() || !e1.is_empty() || e1.len() != e2.len() {
            // Kept for inspection: the re-imported project.
            let dump = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
                .join("xschem_roundtrip")
                .join(&f.name);
            let _ = std::fs::remove_dir_all(&dump);
            for (p, b) in &back {
                let path = dump.join(p);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, b).unwrap();
            }
            failures.push(format!(
                "{}: errors {e1:?} / {e2:?}\n{:#?}\n--- original\n{orig}\n--- re-import\n{re}\n--- notes\n{}--- import report\n{}",
                f.name,
                r.diffs,
                x.notes.text(),
                report
                    .text()
                    .lines()
                    .filter(|l| !l.starts_with("file"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Runs `cmd` for at most `limit`; `None` when it had to be killed.
fn run_bounded(cmd: &mut Command, limit: Duration) -> Option<std::process::Output> {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let start = Instant::now();
    while child.try_wait().ok()?.is_none() {
        if start.elapsed() > limit {
            let _ = child.kill();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().ok()
}

#[test]
fn real_xschem_netlists_like_schemify() {
    let Some(bin) = xschem_bin() else {
        let msg = "xschem not on PATH: real-xschem oracle skipped (nix shell nixpkgs#xschem)";
        assert!(
            std::env::var("SCHEMIFY_REQUIRE_XSCHEM").as_deref() != Ok("1"),
            "{msg}"
        );
        eprintln!("{msg}");
        return;
    };
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("xschem_export");
    let mut failures = Vec::new();
    for f in fixtures() {
        let x = export_fixture(&f);
        let dir = base.join(&f.name);
        let _ = std::fs::remove_dir_all(&dir);
        for (p, b) in &x.files {
            let path = dir.join("export").join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b).unwrap();
        }
        let out = dir.join("netlist");
        let home = dir.join("home");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let sch = exported_top(&x, &f.top);
        let run = run_bounded(
            Command::new(&bin)
                .args(["-q", "-x", "-n", "-s", "-r", "--rcfile"])
                .arg(dir.join("export/xschemrc"))
                .arg("--netlist_path")
                .arg(&out)
                .arg(dir.join("export").join(&sch))
                .env("HOME", &home)
                .current_dir(&dir),
            Duration::from_secs(60),
        );
        let stem = sch
            .rsplit('/')
            .next()
            .unwrap()
            .strip_suffix(".sch")
            .unwrap();
        let Ok(theirs) = std::fs::read_to_string(out.join(format!("{stem}.spice"))) else {
            failures.push(format!("{}: xschem wrote no netlist: {run:?}", f.name));
            continue;
        };
        let (ours, _) = netlist(&f.files, &f.top);
        let r = compare(&ours, &theirs, &CmpOptions::default());
        eprintln!(
            "{}: xschem equal={} ({} devices)",
            f.name,
            r.is_equal(),
            r.devices[1]
        );
        if !r.is_equal() {
            failures.push(format!(
                "{}: {:#?}\n--- schemify\n{ours}\n--- xschem\n{theirs}",
                f.name, r.diffs
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn untranslatable_constructs_are_reviewed_and_left_default() {
    let mut files = Files::new();
    files.insert(
        "dev.shprim".into(),
        b"schemify 1 shprim\n\npin a inout 0 -30\npin b inout 0 30\n\nparam v 1\nparam s x\n  kind string\nparam f 2\n  min 1\n\nimpl spice spice\n  format spice \"R{name} {a} {b} {v} {params}\"\n  format xyce \"R{name} {a} {b} {v}\"\n\nline 0 -30 0 30\n\nstext 5 0 R0 \"{name} {eng(v)}\"\n"
            .to_vec(),
    );
    files.insert(
        "tb.shbench".into(),
        b"schemify 1 shbench\n  corner tt\n\ninst 000001 ./dev\n  at 0 0 R0\n  name X1\n  v \"a ? 1 : 2\"\n  s {v*2}\n  f gmid.w(1)\n  nope 3\n\nlabel 000002 0 -30 R0 n1\n\nvar k \"if(1 > 0, 1, 2)\"\nvar ok 2\n\ncorner tt\n  temp 27\ncorner ff\n  temp 85\n\nsweep s1\n  var ok\n  values 1 2\n\noutput o \"v(n1)\"\n\nspec o\n  max 1\n\ncode x xyce\n  | .print tran v(n1)\n"
            .to_vec(),
    );
    let d = load_design(&files, "tb.shbench");
    let x = export(&d, &[d.find("tb.shbench").unwrap()]).unwrap();
    let file =
        |p: &str| String::from_utf8(x.files.iter().find(|f| f.0 == p).unwrap().1.clone()).unwrap();
    let sch = file("root/tb.sch");
    let sym = file("root/dev.sym");
    let inst = sch
        .lines()
        .find(|l| l.starts_with("C {root/dev.sym}"))
        .unwrap();
    assert_eq!(inst, "C {root/dev.sym} 0 0 0 0 {name=X1}", "{sch}");
    assert!(
        !sym.contains("\nformat="),
        "untranslatable format omitted: {sym}"
    );
    assert!(sym.contains("T {@name } 5 0 0 0"), "{sym}");
    assert!(
        sch.contains(".param ok=2") && !sch.contains(".param k="),
        "{sch}"
    );
    assert!(
        sch.contains(".temp 27") && !sch.contains(".temp 85"),
        "{sch}"
    );
    assert!(!sch.contains(".print"), "{sch}");
    let notes = x.notes.text();
    for want in [
        "needs_review:expression\troot/tb.shbench\tX1.v=",
        "needs_review:template\troot/tb.shbench\tX1.s=",
        "needs_review:expression\troot/tb.shbench\tX1.f=",
        "needs_review:expression\troot/tb.shbench\tX1.nope: not a parameter",
        "needs_review:expression\troot/tb.shbench\tvar k",
        "needs_review:corner\troot/tb.shbench\tcorner ff",
        "needs_review:sweep\troot/tb.shbench\tsweep s1",
        "needs_review:output\troot/tb.shbench\toutput o",
        "needs_review:spec\troot/tb.shbench\tspec o",
        "needs_review:code\troot/tb.shbench\tcode x xyce",
        "needs_review:format\tdev.shprim\tformat xyce",
        "needs_review:template\tdev.shprim\tformat spice: {params}",
        "needs_review:template\tdev.shprim\tstext",
        "needs_review:param-attr\tdev.shprim\tparam f min",
    ] {
        let want = want.replace("root/tb.shbench", "tb.shbench");
        assert!(notes.contains(&want), "missing {want:?} in\n{notes}");
    }
    // Bad tops are refused, not exported.
    assert!(export(&d, &[]).is_err());
    assert!(export(&d, &[99]).is_err());
}

#[test]
fn export_is_deterministic() {
    for f in fixtures() {
        let a = export_fixture(&f);
        let b = export_fixture(&f);
        assert_eq!(a.files, b.files, "{}", f.name);
        assert_eq!(a.notes.text(), b.notes.text(), "{}", f.name);
        assert!(a.files.iter().any(|p| p.0 == "xschemrc"));
    }
}
