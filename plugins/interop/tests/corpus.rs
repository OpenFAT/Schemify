//! Real-corpus oracles, gated on `SCHEMIFY_CORPUS` (a directory holding the PDK and
//! xschem example libraries; see `tests/corpus/fetch.sh`). Without it every test here
//! prints a skip note and passes.
//!
//! - Every `.sym`/`.sch` reads with zero dropped records.
//! - The idiom frequency table of `docs/research/pdk-xschem-devices.md` §5.6 is
//!   reproduced exactly: the classifier sees what the research saw.

use std::path::{Path, PathBuf};

use interop::attr::{Attrs, Idioms, at_tokens, classify};
use interop::symbol;
use interop::xschem::{self, NumField};
use interop::{spice, tech};

fn corpus() -> Option<PathBuf> {
    let root = std::env::var_os("SCHEMIFY_CORPUS").map(PathBuf::from);
    if root.is_none() {
        eprintln!("SCHEMIFY_CORPUS unset: skipping real-corpus test");
    }
    root
}

/// Every file under `dir` (recursively) whose extension is in `exts` (all if empty), sorted.
fn files(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if exts.is_empty() || p.extension().is_some_and(|x| exts.iter().any(|e| x == *e))
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// The first directory under `root` whose path ends with `suffix` and holds `.sym` files.
fn find_lib(root: &Path, suffix: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut found = Vec::new();
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut has_sym = false;
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "sym") {
                has_sym = true;
            }
        }
        if has_sym && d.ends_with(suffix) {
            found.push(d);
        }
    }
    found.sort();
    found.into_iter().next()
}

#[test]
fn every_corpus_file_reads_without_dropped_records() {
    let Some(root) = corpus() else { return };
    let paths = files(&root, &["sym", "sch"]);
    assert!(!paths.is_empty(), "no .sym/.sch under {}", root.display());
    let (mut records, mut coords, mut rounded, mut other_rounded, mut max_error) =
        (0u64, 0u64, 0u64, 0u64, 0i16);
    let mut failures = Vec::new();
    for p in &paths {
        let text = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let f = xschem::read(&text).expect("corpus files are small");
        let mut levels = vec![&f];
        while let Some(l) = levels.pop() {
            records += u64::from(l.records_read);
            coords += (4 * (l.lines.xy.len() + l.boxes.xy.len() + l.wires.xy.len())
                + 2 * (l.points.len() + l.instances.xy.len() + l.texts.xy.len())
                + 3 * l.arcs.circle.len()) as u64;
            for r in &l.roundings {
                if r.field == NumField::Coord {
                    rounded += 1;
                    max_error = max_error.max(r.error_micro.abs());
                } else {
                    other_rounded += 1;
                }
            }
            for i in &l.issues {
                failures.push(format!(
                    "{}:{}:{}: {:?} (tag {:?})",
                    p.display(),
                    i.line,
                    i.col,
                    i.kind,
                    i.tag as char
                ));
            }
            levels.extend(l.embedded.iter().map(|e| &e.body));
        }
    }
    eprintln!(
        "xschem corpus: {} files, {records} records, {coords} coordinates, {rounded} rounded ({:.3}%, max error {max_error}/1000 milli), {other_rounded} rounded scales/angles, {} dropped",
        paths.len(),
        100.0 * rounded as f64 / coords.max(1) as f64,
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "dropped records:\n{}",
        failures.join("\n")
    );
}

/// Research §5.6 rows: (label, per-library symbol counts).
const LIBS: [(&str, usize); 5] = [
    ("sky130_fd_pr", 79),
    ("gf180mcu_fd_pr", 68),
    ("sg13g2_pr", 45),
    ("sky130_stdcells", 437),
    ("sg13g2_stdcell/sym/xschem", 84),
];

const EXPECTED: [(&str, [u32; 5]); 19] = [
    ("N1 plain @attr", [78, 67, 43, 437, 84]),
    ("N2 @pinlist", [77, 66, 42, 0, 0]),
    ("N3 @@pin", [0, 0, 0, 423, 78]),
    ("N4 @spiceprefix", [71, 57, 41, 0, 0]),
    ("N5 net-as-param (@body, extra=)", [13, 6, 4, 437, 84]),
    ("N6 \\\\ token terminator", [0, 0, 3, 437, 84]),
    ("N7 SPICE-quoted expr", [11, 28, 0, 0, 0]),
    ("E1 expr('…')", [36, 0, 0, 0, 0]),
    ("E2 expr_eng(…)", [0, 0, 3, 0, 0]),
    ("T1 tcleval + $VAR", [2, 0, 0, 0, 0]),
    ("T2 tcleval + [ev]/[ev7]", [0, 0, 8, 0, 0]),
    ("T3 [abs_sym_path]/pre_osdi", [1, 0, 0, 0, 0]),
    ("T4 type=tcleval([symtype])", [0, 0, 0, 0, 84]),
    ("T5 schematic=tcleval([hierarchy_config])", [0, 0, 0, 0, 84]),
    ("D1 drc= Tcl proc", [40, 0, 24, 0, 0]),
    ("V1 label @spice_get_*", [40, 0, 2, 0, 0]),
    ("V2 label [ngspice::get_node]", [0, 19, 0, 0, 0]),
    ("V3 label expr()/expr_eng()", [67, 0, 0, 0, 0]),
    // Research V4 matched only `sky130_display_fet_params|cap_cmomi_C`; the classifier
    // flags every unknown proc, so it also counts `display_fet_params` (gf180, IHP) and
    // `display_bip_params` (IHP). Research row: [1, 0, 1, 0, 0].
    ("V4 label unknown Tcl proc", [1, 1, 3, 0, 0]),
];

/// Keys the research counted as netlist-relevant.
const NET_KEYS: [&str; 7] = [
    "type",
    "format",
    "lvs_format",
    "template",
    "device_model",
    "spice_sym_def",
    "schematic",
];

/// Returns which §5.6 rows one symbol hits.
fn idiom_rows(text: &str) -> [bool; 19] {
    let f = xschem::read(text).expect("small");
    let s = symbol::extract(&f);
    let mut net = Idioms::default();
    let mut body = false;
    for key in NET_KEYS {
        if let Some(v) = s.attrs.get(key) {
            net |= classify(v);
            body |= at_tokens(v).any(|t| t.name == "body");
        }
    }
    let mut label = Idioms::default();
    for t in &f.texts.text {
        label |= classify(f.str(*t));
    }
    let has = |i: Idioms, bits: Idioms| i.intersects(bits);
    let tcl = has(net, Idioms::TCLEVAL);
    [
        has(net, Idioms::AT_ATTR),
        has(net, Idioms::AT_PINLIST),
        has(net, Idioms::AT_PIN_NET),
        has(net, Idioms::AT_SPICEPREFIX),
        body || s.attrs.find("extra").is_some(),
        has(net, Idioms::AT_TERMINATOR),
        has(net, Idioms::SPICE_QUOTED),
        has(net, Idioms::EXPR),
        has(net, Idioms::EXPR_ENG),
        tcl && has(net, Idioms::TCL_VAR),
        tcl && has(net, Idioms::TCL_EV),
        has(net, Idioms::TCL_ABS_SYM_PATH | Idioms::PRE_OSDI),
        s.attrs
            .get("type")
            .is_some_and(|v| classify(v).intersects(Idioms::TCL_SYMTYPE)),
        s.attrs
            .get("schematic")
            .is_some_and(|v| classify(v).intersects(Idioms::TCL_HIER_CONFIG)),
        s.attrs.get("drc").is_some_and(|v| !v.trim().is_empty()),
        has(label, Idioms::AT_SIM_VALUE),
        has(label, Idioms::TCL_NGSPICE),
        has(label, Idioms::EXPR | Idioms::EXPR_ENG),
        has(label, Idioms::TCL_OTHER),
    ]
}

#[test]
fn idiom_table_matches_research() {
    let Some(root) = corpus() else { return };
    let mut got = [[0u32; 5]; 19];
    let mut missing = Vec::new();
    for (lib, (suffix, n_expected)) in LIBS.iter().enumerate() {
        let Some(dir) = find_lib(&root, suffix) else {
            missing.push(*suffix);
            continue;
        };
        let syms: Vec<_> = files(&dir, &["sym"])
            .into_iter()
            .filter(|p| p.parent() == Some(&dir))
            .collect();
        assert_eq!(
            syms.len(),
            *n_expected,
            "{suffix}: symbol count differs from the research corpus"
        );
        for p in syms {
            let rows = idiom_rows(&std::fs::read_to_string(&p).expect("readable"));
            for (r, hit) in rows.iter().enumerate() {
                got[r][lib] += u32::from(*hit);
            }
        }
    }
    let mut table = String::from(
        "| idiom | sky130_fd_pr | gf180mcu_fd_pr | sg13g2_pr | sky130 stdcells | sg13g2_stdcell |\n",
    );
    let mut mismatches = Vec::new();
    for (r, (label, expected)) in EXPECTED.iter().enumerate() {
        table += &format!(
            "| {label} | {} |\n",
            got[r].map(|n| n.to_string()).join(" | ")
        );
        for lib in 0..5 {
            if !missing.contains(&LIBS[lib].0) && got[r][lib] != expected[lib] {
                mismatches.push(format!(
                    "{label} / {}: got {} research {}",
                    LIBS[lib].0, got[r][lib], expected[lib]
                ));
            }
        }
    }
    eprintln!("{table}");
    if !missing.is_empty() {
        eprintln!("libraries not found under the corpus (rows not checked): {missing:?}");
    }
    assert!(
        mismatches.is_empty(),
        "idiom table differs from research:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn every_corpus_attribute_string_tokenizes() {
    let Some(root) = corpus() else { return };
    let mut attrs = Attrs::default();
    let (mut strings, mut rows) = (0u64, 0u64);
    for p in files(&root, &["sym", "sch"]) {
        let f = xschem::read(&std::fs::read_to_string(&p).expect("readable")).expect("small");
        let props = f
            .lines
            .props
            .iter()
            .chain(&f.boxes.props)
            .chain(&f.polygons.props)
            .chain(&f.arcs.props);
        let props = props
            .chain(&f.texts.props)
            .chain(&f.wires.props)
            .chain(&f.instances.props)
            .chain(&f.globals.text);
        for span in props {
            attrs.parse_into(f.str(*span));
            strings += 1;
            rows += attrs.len() as u64;
        }
    }
    eprintln!("attribute strings: {strings}, key/value rows: {rows}");
}

/// The first file under `root` whose path ends with `suffix`.
fn find_file(root: &Path, suffix: &str) -> Option<PathBuf> {
    let all = files(root, &[]);
    all.into_iter()
        .find(|p| p.to_string_lossy().ends_with(suffix))
}

fn model_scan(root: &Path, entry_suffix: &str, section: Option<&str>) -> Option<spice::ModelScan> {
    let Some(entry) = find_file(root, entry_suffix) else {
        eprintln!("{entry_suffix} not in corpus: skipped");
        return None;
    };
    let dirs = [entry.parent().expect("file has a parent").to_path_buf()];
    let opts = spice::ScanOptions {
        section,
        search_dirs: &dirs,
    };
    let s = spice::scan(&entry, &opts, |p| std::fs::read_to_string(p).ok());
    eprintln!(
        "{entry_suffix} [{section:?}]: {} files, {} lines, {} models, {} bins, {} subckts, {} devices, {} params, {} issues",
        s.files.len(),
        s.lines_read,
        s.models.name.len(),
        s.bins.model.len(),
        s.subckts.name.len(),
        s.devices.letter.len(),
        s.params.name.len(),
        s.issues.len()
    );
    for i in s.issues.iter().take(8) {
        let file = s
            .files
            .get(i.file as usize)
            .map_or("?".into(), |f| f.display().to_string());
        eprintln!("  {:?} {file}:{} {}", i.kind, i.line, s.str(i.detail));
    }
    Some(s)
}

#[test]
fn model_cards_bins_and_osdi() {
    let Some(root) = corpus() else { return };
    // sky130: binned BSIM4 cards inside the `sky130_fd_pr__nfet_01v8` wrapper subckt.
    if let Some(s) = model_scan(
        &root,
        "cells/nfet_01v8/sky130_fd_pr__nfet_01v8__tt.corner.spice",
        None,
    ) {
        assert!(s.issues.is_empty());
        let bins = s.bins_of("sky130_fd_pr__nfet_01v8");
        let (l, w) = spice::union(&bins);
        eprintln!("sky130 nfet_01v8: {} bins, L {l:?}, W {w:?}", bins.len());
        assert_eq!((l, w), (vec![[1.5e-7, 1e-4]], vec![[3.6e-7, 1e-4]]));
        assert!(spice::contains(&bins, 0.15e-6, 0.42e-6) && !spice::contains(&bins, 0.1e-6, 1e-6));
        assert!(!s.uses_osdi("sky130_fd_pr__nfet_01v8"));
    }
    // gf180: `.lib` sections calling sections of the same file; mins match the symbol
    // template defaults (L=0.28u W=0.22u).
    if let Some(s) = model_scan(&root, "models/ngspice/sm141064.spice", Some("typical")) {
        assert!(s.issues.is_empty());
        for (name, lmin, wmin) in [
            ("nfet_03v3", 2.8e-7, 2.2e-7),
            ("pfet_03v3", 2.8e-7, 2.2e-7),
            ("nfet_06v0", 6e-7, 3e-7),
        ] {
            let (l, w) = spice::union(&s.bins_of(name));
            eprintln!("gf180 {name}: L {l:?}, W {w:?}");
            assert_eq!(
                (l.len(), w.len(), l[0][0], w[0][0]),
                (1, 1, lmin, wmin),
                "{name}"
            );
        }
    }
    // IHP: PSP via OSDI hidden behind `.subckt` wrappers (PDK doc §2.3).
    if let Some(s) = model_scan(&root, "ngspice/models/cornerMOSlv.lib", Some("mos_tt")) {
        assert!(s.issues.is_empty());
        assert!(s.uses_osdi("sg13_lv_nmos") && s.uses_osdi("sg13_lv_pmos"));
        let psp = (0..s.models.name.len())
            .find(|&m| s.str(s.models.kind[m]) == "psp103va")
            .expect("psp card");
        assert_eq!(s.backing(psp), spice::Backing::Osdi);
    }
    if let Some(s) = model_scan(&root, "ngspice/.spiceinit", None) {
        let loads: Vec<_> = s.osdi_loads.text.iter().map(|t| s.str(*t)).collect();
        assert_eq!(loads.len(), 6, "{loads:?}");
        assert!(loads.iter().all(|p| p.ends_with(".osdi")));
    }
}

#[test]
fn tech_file_grids() {
    let Some(root) = corpus() else { return };
    let read = |suffix: &str| {
        let p = find_file(&root, suffix);
        if p.is_none() {
            eprintln!("{suffix} not in corpus: skipped");
        }
        p.map(|p| std::fs::read_to_string(p).expect("readable"))
    };
    for (suffix, dbu, grid) in [
        ("gf180mcu.lyt", 1000, Some(5000)),
        ("sg13g2.lyt", 1000, Some(5000)),
        ("sky130.lyt", 1000, None),
    ] {
        if let Some(text) = read(suffix) {
            let g = tech::read_lyt(&text).expect("valid lyt");
            eprintln!("{suffix}: {g:?}");
            assert_eq!((g.dbu_pm, g.default_grid_pm), (Some(dbu), grid), "{suffix}");
        }
    }
    for suffix in ["sky130.tech", "gf180mcu.tech", "ihp-sg13g2-cifout.tech"] {
        if let Some(text) = read(suffix) {
            let t = tech::read_magic(&text);
            eprintln!(
                "{suffix}: grid {:?} pm, {} styles",
                t.manufacturing_grid_pm(),
                t.styles.name.len()
            );
            assert_eq!(t.manufacturing_grid_pm(), Some(5000), "{suffix}");
            assert!(t.bad_lines.is_empty(), "{suffix}: {:?}", t.bad_lines);
        }
    }
    if let Some(text) = read("ihp-sg13g2.tech") {
        // The IHP main file has no CIF sections of its own; they come by `include`.
        let t = tech::read_magic(&text);
        assert_eq!(t.manufacturing_grid_pm(), None);
        assert!(
            t.includes.iter().any(|i| i == "ihp-sg13g2-cifout"),
            "{:?}",
            t.includes
        );
    }
}
