//! Smoke tests on whole designs: the hand-written fixtures in `tests/fixtures/`
//! (inverter, a two-level OTA hierarchy, a bus array; original designs, not copies of
//! xschem's GPL examples), and, when `SCHEMIFY_CORPUS` names a directory of Schemify
//! files (an imported corpus), every bench in it: no panics, deterministic output.

mod common;

use common::codes;
use schemify_doc::{DocHandle, Project};
use schemify_elab::{Elab, Target, elaborate, no_funcs};
use std::path::{Path, PathBuf};

/// A deterministic text dump of an elaboration (definitions and rows by name).
fn dump(e: &Elab) -> String {
    let mut defs: Vec<u32> = (0..e.defs.cell.len() as u32).collect();
    defs.sort_by(|a, b| e.names.cmp(e.defs.name[*a as usize], e.defs.name[*b as usize]));
    let n = |x| if x == schemify_elab::Name::NONE { "-" } else { e.names.get(x) };
    let mut out = String::new();
    for d in defs {
        let ports: Vec<&str> = e.ports(d).iter().map(|p| n(*p)).collect();
        out.push_str(&format!("def {} [{}] used={}\n", n(e.defs.name[d as usize]), ports.join(" "), e.defs.used[d as usize]));
        for r in e.insts_of(d) {
            let target = match e.insts.target[r] {
                Target::Def(c) => n(e.defs.name[c as usize]).to_owned(),
                Target::Leaf(l) => format!("leaf:{}", n(e.cells[e.impls.cell[l as usize] as usize].cell)),
            };
            let conns: Vec<&str> = e.conns(r).iter().map(|c| n(*c)).collect();
            let params: Vec<String> = e
                .params_of(r)
                .map(|k| format!("{}={}", n(e.insts.param_name[k]), n(e.insts.param_text[k])))
                .collect();
            out.push_str(&format!("  {} {target} [{}] {}\n", n(e.insts.name[r]), conns.join(" "), params.join(" ")));
        }
    }
    for v in &e.violations.rows {
        out.push_str(&format!("{} {} {}\n", v.code.as_str(), n(v.path), v.detail));
    }
    out
}

/// Indexes and opens every cell file under `root` (one library, the project's own).
fn load(root: &Path) -> (Project, Vec<(String, DocHandle)>) {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path: PathBuf = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if matches!(path.extension().and_then(|e| e.to_str()), Some("shprim" | "shcomp" | "shbench")) {
                let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                files.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    files.sort();
    let mut p = Project::new(None, None, 5).unwrap();
    let listing: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    p.index_library("", "", &listing).unwrap();
    let mut handles = Vec::new();
    for (rel, bytes) in &files {
        if let Ok(o) = p.open("", rel, bytes, None) {
            handles.push((rel.clone(), o.handle));
        }
    }
    (p, handles)
}

fn fixtures() -> (Project, Vec<(String, DocHandle)>) {
    load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"))
}

fn top(hs: &[(String, DocHandle)], name: &str) -> DocHandle {
    hs.iter().find(|(n, _)| n == name).unwrap().1
}

fn row(e: &Elab, d: u32, name: &str) -> usize {
    e.insts_of(d).find(|&r| e.names.get(e.insts.name[r]) == name).unwrap()
}

#[test]
fn inverter_bench() {
    let (p, hs) = fixtures();
    let e = elaborate(&p, top(&hs, "inv_tb.shbench"), &common::ngspice(), &mut no_funcs);
    assert!(!e.violations.blocking(), "{}", dump(&e));
    let x1 = row(&e, e.top, "X1");
    let conns: Vec<&str> = e.conns(x1).iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(conns, ["in", "out"]);
    let Target::Def(inv) = e.insts.target[x1] else { panic!() };
    for (dev, want) in [("MN", ["Y", "A", "0", "0"]), ("MP", ["Y", "A", "vdd!", "vdd!"])] {
        let got: Vec<&str> = e.conns(row(&e, inv, dev)).iter().map(|n| e.names.get(*n)).collect();
        // nmos pins D G S B; pmos S G D B.
        let want: Vec<&str> = if dev == "MP" { vec![want[2], want[1], want[0], want[3]] } else { want.to_vec() };
        assert_eq!(got, want, "{dev}");
    }
    let vars: Vec<&str> = e.bench.var_text.iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(vars, ["1.8"], "corner tt has no set");
    assert!(e.names.get(e.defs.name[inv as usize]).starts_with("inv__"), "wn=0.5 specializes");
}

#[test]
fn opamp_hierarchy_specializes_per_size() {
    let (p, hs) = fixtures();
    let e = elaborate(&p, top(&hs, "opamp_tb.shbench"), &common::ngspice(), &mut no_funcs);
    assert!(!e.violations.blocking(), "{}", dump(&e));
    let mut names: Vec<&str> = e.defs.name.iter().map(|n| e.names.get(*n)).collect();
    names.sort_unstable();
    assert_eq!(names.len(), 5, "{names:?}");
    assert_eq!(&names[..1], ["buffer"]);
    assert!(names[1].starts_with("buffer__") && names[2] == "opamp" && names[3].starts_with("opamp__"));
    assert_eq!(e.flat_leaves(e.top), 3 * 9 + 2);
    // B1 and B3 share the default buffer; the feedback label closed the loop.
    let (b1, b3) = (row(&e, e.top, "B1"), row(&e, e.top, "B3"));
    assert_eq!(e.insts.target[b1], e.insts.target[b3]);
    let Target::Def(buf) = e.insts.target[b1] else { panic!() };
    let x1: Vec<&str> = e.conns(row(&e, buf, "X1")).iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(x1, ["IN", "OUT", "OUT", "IB"]);
    let Target::Def(amp) = e.insts.target[row(&e, buf, "X1")] else { panic!() };
    let m6: Vec<&str> = e.conns(row(&e, amp, "M6")).iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(m6, ["vdd!", "n2", "OUT", "vdd!"]);
    let tail: Vec<&str> = e.conns(row(&e, amp, "M2")).iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(tail[2], "tail", "the wire carries the tail label to M2");
    assert_eq!(dump(&e), dump(&elaborate(&p, top(&hs, "opamp_tb.shbench"), &common::ngspice(), &mut no_funcs)));
}

#[test]
fn bus_array_slices_per_element() {
    let (p, hs) = fixtures();
    let e = elaborate(&p, top(&hs, "bus_tb.shbench"), &common::ngspice(), &mut no_funcs);
    assert!(!e.violations.blocking(), "{}", dump(&e));
    for (x, d_in, d_out) in [("X[3]", "din[3]", "dout[0]"), ("X[0]", "din[0]", "dout[3]")] {
        let c: Vec<&str> = e.conns(row(&e, e.top, x)).iter().map(|n| e.names.get(*n)).collect();
        assert_eq!(c, [d_in, d_out], "{x}");
    }
    assert_eq!(codes(&e).iter().filter(|c| *c == "W016").count(), 8);
}

/// Every bench under `$SCHEMIFY_CORPUS`: elaboration never panics and is
/// deterministic. Skipped when the variable is unset.
#[test]
fn corpus_smoke() {
    let Some(root) = std::env::var_os("SCHEMIFY_CORPUS") else {
        eprintln!("SCHEMIFY_CORPUS unset: corpus smoke skipped");
        return;
    };
    let (p, hs) = load(Path::new(&root));
    let mut benches = 0;
    let mut by_code: std::collections::BTreeMap<String, usize> = Default::default();
    for (name, h) in &hs {
        if !name.ends_with(".shbench") {
            continue;
        }
        benches += 1;
        let e = elaborate(&p, *h, &common::ngspice(), &mut no_funcs);
        assert_eq!(dump(&e), dump(&elaborate(&p, *h, &common::ngspice(), &mut no_funcs)), "{name}");
        for c in codes(&e) {
            *by_code.entry(c).or_default() += 1;
        }
    }
    eprintln!("corpus: {} files, {benches} benches, findings {by_code:?}", hs.len());
}
