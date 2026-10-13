//! Real-corpus translation oracles, gated on `SCHEMIFY_CACHE` (the `tests/corpus/.cache`
//! directory: `src/` from `fetch.sh`, `golden/` from `golden.sh`). Without it every
//! test prints a skip note and passes.
//!
//! - Every library group of the golden corpus translates with count conservation
//!   (asserted inside), every output checks (`check_output`), and a re-import with the
//!   inputs in reverse order is byte-identical.
//! - The orientation table is derived independently from xschem's golden netlists:
//!   for each device instance, every candidate orientation places its pins; the one
//!   whose pins land on the label-named nets the golden line lists wins.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use interop::attr::Attrs;
use interop::symbol;
use interop::translate::{ORIENT_TABLE, place};
use interop::{Options, check_output, translate_library, xschem};
use schemify_format::Orient;

fn cache() -> Option<PathBuf> {
    let root = std::env::var_os("SCHEMIFY_CACHE").map(PathBuf::from);
    if root.is_none() {
        eprintln!("SCHEMIFY_CACHE unset: skipping real-corpus translation test");
    }
    root
}

/// A library group: the xschem search path of one rcfile of `golden.sh`, and which of
/// its elements are converted.
struct Group {
    name: &'static str,
    path: Vec<(PathBuf, bool)>,
}

fn groups(c: &Path) -> Vec<Group> {
    let l = c.join("src/xschem/xschem_library");
    let p = c.join("src/ihp_open_pdk/ihp-sg13g2");
    let xs = [
        "devices",
        "examples",
        "ngspice",
        "ngspice_verilog_cosim",
        "logic",
        "xschem_simulator",
        "generators",
        "inst_sch_select",
        "binto7seg",
        "pcb",
        "rom8k",
    ];
    vec![
        Group {
            name: "xschem",
            path: xs.iter().map(|d| (l.join(d), true)).collect(),
        },
        Group {
            name: "sky130",
            path: vec![
                (l.join("devices"), true),
                (l.clone(), false),
                (c.join("src/xschem_sky130"), true),
            ],
        },
        Group {
            name: "gf180",
            path: vec![
                (l.join("devices"), true),
                (l.clone(), false),
                (c.join("src/gf180mcu_fd_pr/cells/xschem"), true),
            ],
        },
        Group {
            name: "ihp",
            path: vec![
                (l.join("devices"), true),
                (l.clone(), false),
                (p.join("libs.tech/xschem"), true),
                (p.join("libs.tech/xschem/sg13g2_pr"), false),
                (p.join("libs.ref/sg13g2_stdcell/sym/xschem"), true),
                (p.join("libs.ref/sg13g2_stdcell/sch/xschem"), true),
            ],
        },
    ]
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p
            .extension()
            .is_some_and(|x| x == "sym" || x == "sch" || x == "cir")
        {
            out.push(p);
        }
    }
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn inputs(g: &Group) -> (Vec<String>, Vec<(String, Vec<u8>)>) {
    let roots: Vec<String> = g
        .path
        .iter()
        .map(|(d, _)| slash(&std::fs::canonicalize(d).unwrap_or(d.clone())))
        .collect();
    let mut files = Vec::new();
    for (d, convert) in &g.path {
        if *convert {
            let mut v = Vec::new();
            walk(&std::fs::canonicalize(d).unwrap_or(d.clone()), &mut v);
            files.extend(
                v.into_iter()
                    .map(|p| (slash(&p), std::fs::read(&p).expect("readable"))),
            );
        }
    }
    files.sort();
    files.dedup_by(|a, b| a.0 == b.0);
    (roots, files)
}

#[test]
fn every_corpus_group_translates_checks_and_reimports_identically() {
    let Some(c) = cache() else { return };
    let (mut mapped, mut devices) = (0u32, 0u32);
    let mut all = Vec::new();
    for g in groups(&c) {
        let (roots, files) = inputs(&g);
        if files.is_empty() {
            eprintln!("{}: no files, skipped", g.name);
            continue;
        }
        let opts = Options {
            roots: &roots,
            ..Options::default()
        };
        let refs: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let (out, report) = translate_library(&refs, &opts);
        report.assert_conservation();
        let problems = check_output(&out);
        assert!(problems.is_empty(), "{}: {problems:#?}", g.name);
        let rev: Vec<(&str, &[u8])> = refs.iter().rev().copied().collect();
        let (again, _) = translate_library(&rev, &opts);
        assert!(out == again, "{}: re-import differs", g.name);
        let reviews = report
            .items
            .iter()
            .filter(|i| matches!(i.class, interop::report::Class::Review(_)))
            .count();
        eprintln!(
            "{}: {} inputs, {} outputs, {} records in, {} needs_review, devices→core {}/{}",
            g.name,
            report.files.len(),
            out.len(),
            report.records_in.iter().map(|&n| u64::from(n)).sum::<u64>(),
            reviews,
            report.core_instances,
            report.device_instances
        );
        mapped += report.core_instances;
        devices += report.device_instances;
        all.push(out);
    }
    eprintln!(
        "devices→core coverage: {mapped}/{devices} = {:.1}%",
        100.0 * f64::from(mapped) / f64::from(devices.max(1))
    );
    port_order_matches_golden_subckt_lines(&c, &all);
}

/// Interface oracle: every golden `.subckt` line of a symbol-backed subcircuit lists
/// its ports in the order of the translated cell's pins (bus pins expanded MSB first,
/// inherited pins last). Known differences are listed with their reason.
fn port_order_matches_golden_subckt_lines(c: &Path, outputs: &[Vec<(String, Vec<u8>)>]) {
    // `D[5..0]` (bits without brackets) imports as the bundle `D5,…,D0`; same-named
    // pins (`xcross`, `doublepin`) as one port plus `alias` pins.
    const XFAIL: &[(&str, &str)] = &[];
    // (file name, lib) → pin names, from the translated cells' provenance.
    let mut cells: HashMap<(String, String), Vec<String>> = HashMap::new();
    for (path, bytes) in outputs
        .iter()
        .flatten()
        .filter(|f| f.0.ends_with(".shcomp") || f.0.ends_with(".shprim"))
    {
        let d = schemify_format::parse(bytes).unwrap_or_else(|e| panic!("{path}: {e:?}"));
        let prov = d
            .find_attr(0, "provenance")
            .and_then(|a| d.attr_value(a, 0))
            .unwrap_or("");
        let Some((lib, file)) = prov.strip_prefix("xschem ").and_then(|p| p.split_once('/')) else {
            continue;
        };
        // An alias pin is a second position of an earlier port, not a port.
        let pins = (1..d.record_count())
            .filter(|&r| d.record_kind(r) == schemify_format::RecordKind::Pin)
            .filter(|&r| d.find_attr(r, "alias").is_none())
            .flat_map(|r| {
                let name = d.field(r, 0).unwrap_or("");
                name.split(',').map(str::to_owned).collect::<Vec<_>>()
            })
            .collect();
        cells.insert((file.to_owned(), lib.to_owned()), pins);
    }
    let expand = |p: &str| -> Vec<String> {
        let parsed = p
            .strip_suffix(']')
            .and_then(|q| q.rsplit_once('['))
            .and_then(|(b, r)| {
                let (a, z) = r.split_once(':')?;
                Some((b, a.parse::<i64>().ok()?, z.parse::<i64>().ok()?))
            });
        match parsed {
            Some((b, a, z)) if a >= z => (z..=a).rev().map(|i| format!("{b}[{i}]")).collect(),
            Some((b, a, z)) => (a..=z).map(|i| format!("{b}[{i}]")).collect(),
            None => vec![p.to_owned()],
        }
    };
    let (mut ok, mut bad) = (0u32, Vec::new());
    let Ok(dir) = std::fs::read_dir(c.join("golden")) else {
        return;
    };
    let mut goldens: Vec<PathBuf> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "spice"))
        .collect();
    goldens.sort();
    for g in goldens {
        let text = std::fs::read_to_string(&g).unwrap_or_default();
        let mut lines: Vec<String> = Vec::new();
        for l in text.lines() {
            match (l.strip_prefix('+'), lines.last_mut()) {
                (Some(rest), Some(last)) => last.push_str(rest),
                _ => lines.push(l.to_owned()),
            }
        }
        for w in lines.windows(3) {
            let (Some(sym), Some(_), Some(sub)) = (
                w[0].strip_prefix("** sym_path: "),
                w[1].strip_prefix("** sch_path: "),
                w[2].strip_prefix(".subckt "),
            ) else {
                continue;
            };
            let mut toks = sub.split_whitespace();
            let name = toks.next().unwrap_or("");
            let ports: Vec<String> = toks
                .take_while(|t| !t.contains('='))
                .map(str::to_ascii_lowercase)
                .collect();
            let sym = Path::new(sym);
            let file = sym
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            let lib = sym
                .parent()
                .and_then(Path::file_name)
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Some(pins) = cells
                .iter()
                .find(|((f, l), _)| *f == file && l.starts_with(&lib))
                .map(|(_, p)| p)
            else {
                continue;
            };
            let ours: Vec<String> = pins
                .iter()
                .flat_map(|p| expand(p))
                .map(|p| p.to_ascii_lowercase())
                .collect();
            if ours == ports {
                ok += 1;
            } else if !XFAIL.iter().any(|(n, _)| *n == name) {
                bad.push(format!(
                    "{}: {name}: golden {ports:?} ours {ours:?}",
                    g.display()
                ));
            }
        }
    }
    eprintln!("port order: {ok} golden .subckt lines match; xfail {XFAIL:?}");
    assert!(bad.is_empty(), "{bad:#?}");
    assert!(ok > 100, "too little evidence: {ok}");
}

// ------------------------------------------------------------------ orientation

struct Dev {
    name: String,
    class: (usize, usize),
    at: [i32; 2],
    pins: Vec<[i32; 2]>,
}

struct Bench {
    wires: Vec<[i32; 4]>,
    labels: Vec<([i32; 2], String)>,
    devs: Vec<Dev>,
}

fn resolve(roots: &[PathBuf], here: &Path, symref: &str, v10: bool) -> Option<PathBuf> {
    let mut r = symref.trim().to_owned();
    if v10 && !r.ends_with(".sym") && !r.ends_with(".sch") {
        r.push_str(".sym");
    }
    roots
        .iter()
        .map(|d| d.join(&r))
        .chain([here.join(&r)])
        .find(|p| p.is_file())
}

fn bench(sch: &Path, roots: &[PathBuf]) -> Option<Bench> {
    let text = String::from_utf8_lossy(&std::fs::read(sch).ok()?).into_owned();
    let f = xschem::read(&text).ok()?;
    let version = f.global(xschem::GlobalTag::Version).unwrap_or("");
    let v10 = !version.contains("file_version=") || version.contains("file_version=1.0");
    let here = sch.parent()?;
    let mut b = Bench {
        wires: f.wires.xy.clone(),
        labels: Vec::new(),
        devs: Vec::new(),
    };
    let mut syms: HashMap<PathBuf, Option<symbol::SymbolInterface>> = HashMap::new();
    for row in 0..f.instances.symref.len() {
        let Some(path) = resolve(roots, here, f.str(f.instances.symref[row]), v10) else {
            continue;
        };
        let iface = syms.entry(path.clone()).or_insert_with(|| {
            let t = String::from_utf8_lossy(&std::fs::read(&path).ok()?).into_owned();
            Some(symbol::extract(&xschem::read(&t).ok()?))
        });
        let Some(iface) = iface else { continue };
        let props = Attrs::parse(f.str(f.instances.props[row]));
        let ty = iface.attrs.get("type").unwrap_or("");
        let at = f.instances.xy[row];
        if matches!(ty, "label" | "ipin" | "opin" | "iopin") {
            let lab = props
                .get("lab")
                .or_else(|| iface.template.get("lab"))
                .unwrap_or("")
                .trim();
            if iface.pins.center.as_slice() == [[0, 0]] && !lab.is_empty() {
                b.labels.push((at, lab.to_ascii_lowercase()));
            }
            continue;
        }
        let fmt = iface.attrs.get("format").unwrap_or("").trim();
        let simple =
            fmt.starts_with("@name @pinlist ") || fmt.starts_with("@spiceprefix@name @pinlist ");
        let name = props.get("name").unwrap_or("").trim();
        if !simple || props.get("format").is_some() || name.is_empty() || name.contains('[') {
            continue;
        }
        let rot = f.instances.rot[row].rem_euclid(4) as usize;
        let flip = usize::from(f.instances.flip[row] != 0);
        b.devs.push(Dev {
            name: name.to_ascii_lowercase(),
            class: (rot, flip),
            at,
            pins: iface.pins.center.clone(),
        });
    }
    Some(b)
}

/// Top-level instance lines of a golden netlist: lowercase name → net tokens.
fn golden_lines(text: &str) -> HashMap<String, Vec<String>> {
    let mut joined: Vec<String> = Vec::new();
    for l in text.lines() {
        let t = l.trim_end();
        if let Some(rest) = t.strip_prefix('+') {
            if let Some(last) = joined.last_mut() {
                last.push(' ');
                last.push_str(rest);
            }
        } else {
            joined.push(t.to_owned());
        }
    }
    let mut out = HashMap::new();
    for l in joined {
        if l.to_ascii_lowercase().starts_with(".subckt") {
            break;
        }
        if l.starts_with(|c: char| c.is_ascii_alphabetic()) {
            let toks: Vec<String> = l.split_whitespace().map(str::to_ascii_lowercase).collect();
            out.insert(toks[0].clone(), toks);
        }
    }
    out
}

fn on_segment(p: [i32; 2], w: [i32; 4]) -> bool {
    let (x1, y1, x2, y2) = (
        i64::from(w[0]),
        i64::from(w[1]),
        i64::from(w[2]),
        i64::from(w[3]),
    );
    let (px, py) = (i64::from(p[0]), i64::from(p[1]));
    (px - x1) * (y2 - y1) == (py - y1) * (x2 - x1)
        && px >= x1.min(x2)
        && px <= x1.max(x2)
        && py >= y1.min(y2)
        && py <= y1.max(y2)
}

/// Label names reachable from point `p` through wires (union-find over wires+labels).
fn names_at(
    b: &Bench,
    comp: &[usize],
    comp_names: &HashMap<usize, Vec<String>>,
    p: [i32; 2],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (k, w) in b.wires.iter().enumerate() {
        if on_segment(p, *w) {
            out.extend(comp_names.get(&comp[k]).cloned().unwrap_or_default());
        }
    }
    for (k, (at, name)) in b.labels.iter().enumerate() {
        if *at == p {
            out.push(name.clone());
            out.extend(
                comp_names
                    .get(&comp[b.wires.len() + k])
                    .cloned()
                    .unwrap_or_default(),
            );
        }
    }
    out
}

fn components(b: &Bench) -> (Vec<usize>, HashMap<usize, Vec<String>>) {
    let n = b.wires.len() + b.labels.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let union = |p: &mut Vec<usize>, a: usize, c: usize| {
        let (ra, rc) = (find(p, a), find(p, c));
        p[ra] = rc;
    };
    for i in 0..b.wires.len() {
        for j in 0..b.wires.len() {
            let w = b.wires[j];
            if i != j
                && (on_segment([b.wires[i][0], b.wires[i][1]], w)
                    || on_segment([b.wires[i][2], b.wires[i][3]], w))
            {
                union(&mut parent, i, j);
            }
        }
        for (k, (at, _)) in b.labels.iter().enumerate() {
            if on_segment(*at, b.wires[i]) {
                union(&mut parent, i, b.wires.len() + k);
            }
        }
    }
    let comp: Vec<usize> = (0..n).map(|x| find(&mut parent, x)).collect();
    let mut names: HashMap<usize, Vec<String>> = HashMap::new();
    for (k, (_, name)) in b.labels.iter().enumerate() {
        names
            .entry(comp[b.wires.len() + k])
            .or_default()
            .push(name.clone());
    }
    (comp, names)
}

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

#[test]
fn orientation_table_reproduces_golden_connectivity() {
    let Some(c) = cache() else { return };
    let benches = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/benches.txt"),
    )
    .expect("benches.txt");
    let groups: HashMap<&str, Vec<PathBuf>> = groups(&c)
        .into_iter()
        .map(|g| (g.name, g.path.into_iter().map(|(d, _)| d).collect()))
        .collect();
    // votes[(rot, flip)][orient] = devices whose every checked pin matches under it.
    let mut votes: BTreeMap<(usize, usize), [u32; 8]> = BTreeMap::new();
    let mut checked_devices = 0u32;
    for line in benches
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let [name, rc, path] = cols[..] else { continue };
        let Some(roots) = groups.get(rc) else {
            continue;
        };
        let Ok(golden) = std::fs::read_to_string(c.join("golden").join(format!("{name}.spice")))
        else {
            continue;
        };
        let Some(b) = bench(&c.join("src").join(path), roots) else {
            continue;
        };
        let lines = golden_lines(&golden);
        let (comp, comp_names) = components(&b);
        for d in &b.devs {
            let Some(toks) = lines
                .get(&d.name)
                .or_else(|| lines.get(&format!("x{}", d.name)))
            else {
                continue;
            };
            if toks.len() <= d.pins.len() {
                continue;
            }
            let mut hits = [0u32; 8];
            let mut checked = 0u32;
            for (k, pin) in d.pins.iter().enumerate() {
                let g = &toks[k + 1];
                let auto = g
                    .strip_prefix("net")
                    .is_some_and(|r| r.bytes().all(|c| c.is_ascii_digit()));
                if auto || g.contains('[') {
                    continue;
                }
                checked += 1;
                for (o, orient) in ORIENTS.iter().enumerate() {
                    let p = place(*pin, *orient, d.at).expect("in range");
                    if names_at(&b, &comp, &comp_names, p).iter().any(|n| n == g) {
                        hits[o] += 1;
                    }
                }
            }
            if checked < 2 {
                continue;
            }
            checked_devices += 1;
            let v = votes.entry(d.class).or_insert([0; 8]);
            for o in 0..8 {
                v[o] += u32::from(hits[o] == checked);
            }
        }
    }
    eprintln!(
        "orientation evidence over {checked_devices} device instances (votes per candidate R0 R90 R180 R270 MR0 MR90 MR180 MR270):"
    );
    for (&(rot, flip), v) in &votes {
        let table = ORIENT_TABLE[rot][flip];
        let best = *v.iter().max().expect("8 candidates");
        let winners: Vec<&str> = (0..8)
            .filter(|&o| v[o] == best)
            .map(|o| ORIENTS[o].as_str())
            .collect();
        eprintln!(
            "  rot {rot} flip {flip}: {v:?} → {winners:?}, table {}",
            table.as_str()
        );
        assert_eq!(
            v[table as usize],
            best,
            "rot {rot} flip {flip}: the table's {} is not a best fit",
            table.as_str()
        );
        assert!(best > 0);
    }
    assert!(votes.len() >= 4, "too little evidence: {votes:?}");
}
