//! The Rust side of the Virtuoso bridge (spec `interop.md` §3.4, §6, §7, §8.2, §12):
//! export → import round trips, hand-written dumps → golden files and netlists, merge,
//! re-mastering, and malformed dumps.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use interop::design::{Design, Files, LoadError, load};
use interop::map::Mapping;
use interop::sfyx;
use interop::virtuoso::{self, ExportOptions, ImportOptions};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every file under `dir`, keyed by its `/`-separated path relative to `dir`.
fn read_dir(dir: &Path) -> Files {
    fn walk(base: &Path, d: &Path, out: &mut Files) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = Files::new();
    walk(dir, dir, &mut out);
    out
}

fn is_cell(p: &str) -> bool {
    [".shprim", ".shcomp", ".shbench"]
        .iter()
        .any(|e| p.ends_with(e))
}

/// Loads every cell file of `files`.
fn design(files: &Files) -> Design {
    let tops: Vec<&str> = files
        .keys()
        .map(String::as_str)
        .filter(|p| is_cell(p))
        .collect();
    let mut absent = BTreeSet::new();
    loop {
        match load(files, &absent, &tops) {
            Ok(d) => return d,
            Err(LoadError::Missing(m)) => absent.extend(m),
            Err(LoadError::Invalid(p)) => panic!("{p:?}"),
        }
    }
}

/// `text` without the import fingerprints (`vsym`, `vsch` header lines, §7.5), which an
/// import adds by design.
fn strip_fp(text: &str) -> String {
    text.lines()
        .filter(|l| !l.starts_with("  vsym ") && !l.starts_with("  vsch "))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// Canonical text of a cell file (what the format writer produces).
fn canonical(bytes: &[u8]) -> String {
    let d = schemify_format::parse(bytes).unwrap();
    schemify_format::write(&d)
}

/// Exports every cell of `files`, the root library as `top`.
fn export_all(files: &Files) -> (String, Design) {
    let d = design(files);
    let tops: Vec<usize> = (0..d.cells.len())
        .filter(|&k| d.cells[k].lib != "core")
        .collect();
    let mut opts = ExportOptions {
        producer: "test".into(),
        ..ExportOptions::default()
    };
    opts.lib_map.insert(String::new(), "top".into());
    let e =
        virtuoso::export(&d, &tops, &Mapping::default(), &opts).unwrap_or_else(|e| panic!("{e:?}"));
    (e.sfyx, d)
}

fn import_opts() -> ImportOptions {
    let mut o = ImportOptions::default();
    o.lib_map.insert("top".into(), String::new());
    o
}

/// The `.sfyx` text without its stash properties.
fn strip_stash(text: &str) -> String {
    let recs = sfyx::read(text).unwrap();
    let kept: Vec<sfyx::Record> = recs
        .into_iter()
        .filter(|r| !(r.tag == "prop" && r.str(1).is_ok_and(|n| n.starts_with("schemifyRecords"))))
        .collect();
    sfyx::write(&kept)
}

fn fixture_projects() -> Vec<PathBuf> {
    let r = root();
    vec![
        r.join("crates/app/tests/fixtures/rc"),
        r.join("crates/app/tests/fixtures/inv"),
        r.join("crates/app/tests/fixtures/ota"),
        r.join("crates/elab/tests/fixtures"),
    ]
}

/// Cell files of a project, canonical text by path.
fn cells(files: &Files) -> BTreeMap<String, String> {
    files
        .iter()
        .filter(|(p, _)| is_cell(p))
        .map(|(p, b)| (p.clone(), canonical(b)))
        .collect()
}

#[test]
fn export_import_round_trip_is_exact_with_the_stash() {
    for dir in fixture_projects() {
        let files = read_dir(&dir);
        let (text, _) = export_all(&files);
        // Deterministic.
        assert_eq!(text, export_all(&files).0, "{}", dir.display());
        let man: Files = files
            .iter()
            .filter(|(p, _)| *p == "schemify.project")
            .map(|(p, b)| (p.clone(), b.clone()))
            .collect();
        let imp = virtuoso::import(&text, &man, &Mapping::default(), &import_opts())
            .unwrap_or_else(|e| panic!("{e:?}"));
        let got: BTreeMap<String, String> = imp
            .files
            .iter()
            .filter(|(p, _)| is_cell(p))
            .map(|(p, b)| (p.clone(), strip_fp(&String::from_utf8(b.clone()).unwrap())))
            .collect();
        let want = cells(&files);
        assert_eq!(
            got.keys().collect::<Vec<_>>(),
            want.keys().collect::<Vec<_>>(),
            "{}",
            dir.display()
        );
        for (p, w) in &want {
            assert_eq!(&got[p], w, "{}: {p}", dir.display());
        }
    }
}

// ---------------------------------------------------------------------------------------
// Hand-written dumps → golden files and netlists
// ---------------------------------------------------------------------------------------

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/virtuoso")
}

fn bless() -> bool {
    std::env::var_os("SCHEMIFY_BLESS").is_some()
}

/// Imports `<name>.sfyx` into an empty project.
fn import_fixture(name: &str) -> virtuoso::Import {
    let text = std::fs::read_to_string(fixtures().join(format!("{name}.sfyx"))).unwrap();
    virtuoso::import(
        &text,
        &Files::new(),
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{name}: {e:?}"))
}

/// Compares (or with `SCHEMIFY_BLESS`, writes) the golden files of fixture `name`.
fn golden(name: &str, files: &[(String, Vec<u8>)], report: &str) {
    let dir = fixtures().join(name);
    let mut all: Vec<(String, Vec<u8>)> = files.to_vec();
    all.push(("report.txt".into(), report.as_bytes().to_vec()));
    if bless() {
        let _ = std::fs::remove_dir_all(&dir);
        for (p, b) in &all {
            let path = dir.join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b).unwrap();
        }
        return;
    }
    let want = read_dir(&dir);
    let got: Files = all.into_iter().collect();
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        want.keys().collect::<Vec<_>>(),
        "{name}: file set (re-bless with SCHEMIFY_BLESS=1)"
    );
    for (p, w) in &want {
        assert_eq!(
            String::from_utf8_lossy(&got[p]),
            String::from_utf8_lossy(w),
            "{name}/{p} (re-bless with SCHEMIFY_BLESS=1)"
        );
    }
}

/// Elaborates `bench` of the imported files (plus the core library) and emits ngspice.
fn netlist(files: &[(String, Vec<u8>)], bench: &str) -> String {
    use schemify_doc::{Project, core_library};
    use schemify_elab::{Dialect, Request, elaborate, no_funcs};
    let manifest = files
        .iter()
        .find(|f| f.0 == "schemify.project")
        .map(|f| f.1.as_slice());
    let mut p = Project::new(manifest, None, 7).expect("manifest parses");
    let mut libs: BTreeMap<String, Vec<(&str, &[u8])>> = BTreeMap::new();
    libs.insert("core".into(), core_library().to_vec());
    let dirs = manifest
        .map(|m| interop::design::manifest_libs(m).unwrap())
        .unwrap_or_default();
    for (path, bytes) in files.iter().filter(|f| is_cell(&f.0)) {
        let (dir, rel) = path.rsplit_once('/').unwrap_or(("", path));
        let lib = dirs
            .iter()
            .find(|(_, d)| d.as_str() == dir)
            .map_or(String::new(), |(l, _)| l.clone());
        libs.entry(lib).or_default().push((rel, bytes));
    }
    let mut handles = BTreeMap::new();
    for (lib, listing) in &libs {
        p.index_library(lib, "import", listing).unwrap();
    }
    for (lib, listing) in &libs {
        if lib == "core" {
            continue;
        }
        for (rel, bytes) in listing {
            let o = p
                .open(lib, rel, bytes, None)
                .unwrap_or_else(|e| panic!("{lib}/{rel}: {e:?}"));
            let errors: Vec<String> = o
                .diags
                .iter()
                .filter(|d| !d.code.is_warning())
                .map(|d| format!("{d:?}"))
                .collect();
            assert!(errors.is_empty(), "{lib}/{rel}: {errors:?}");
            handles.insert(
                if lib.is_empty() {
                    (*rel).to_owned()
                } else {
                    format!("{}/{rel}", dirs[lib])
                },
                o.handle,
            );
        }
    }
    let req = Request {
        config: None,
        corner: None,
        overrides: &[],
        env: &[],
        dialect: Dialect::Ngspice,
    };
    let e = elaborate(&p, handles[bench], &req, &mut no_funcs);
    let blocking: Vec<String> = e
        .violations
        .rows
        .iter()
        .filter(|v| v.code.severity() == schemify_elab::Severity::Error)
        .map(|v| format!("{} {}", v.code.as_str(), v.detail))
        .collect();
    assert!(blocking.is_empty(), "{bench}: {blocking:?}");
    schemify_netlist::emit(&e, Dialect::Ngspice).text
}

fn same_netlist(name: &str, ours: &str) {
    let want = std::fs::read_to_string(fixtures().join(format!("{name}.ngspice"))).unwrap();
    let r = schemify_spicecmp::compare(&want, ours, &schemify_spicecmp::Options::default());
    assert!(
        r.is_equal(),
        "{name}: netlists differ\n--- ours\n{ours}\n--- diffs\n{:#?}",
        r.diffs
    );
}

/// Every imported cell file loads in `doc` without errors and round-trips (the import
/// oracle of the xschem importer, reused).
fn loads(files: &[(String, Vec<u8>)]) {
    let problems = interop::check_output(files);
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn dump_fixtures_import_to_the_golden_files() {
    for (name, bench) in [
        ("inverter", Some("demo/tb_inv.shbench")),
        ("bus", Some("demo/tb_buf4.shbench")),
        ("symbol", None),
        ("pdk", None),
        ("netexpr", Some("demo/tb_ne.shbench")),
    ] {
        let imp = import_fixture(name);
        golden(name, &imp.files, &imp.notes.text());
        loads(&imp.files);
        if let Some(b) = bench {
            let n = netlist(&imp.files, b);
            same_netlist(name, &n);
        }
    }
}

// ---------------------------------------------------------------------------------------
// The round trip without the stash: Virtuoso-carried data exact, losses documented
// ---------------------------------------------------------------------------------------

/// Header attributes, param attributes and record kinds the Virtuoso side cannot hold:
/// the documented losses of a round trip without the stash (spec §7.1, §7.4; the list the
/// stash exists for). Everything else must come back exactly.
const LOST_PARAM_ATTRS: [&str; 8] = [
    "min", "max", "step", "grid", "show", "computed", "cached", "unit",
];
const LOST_KINDS: [&str; 13] = [
    "prose",
    "var",
    "body",
    "waive",
    "constraint",
    "corner",
    "config",
    "sweep",
    "montecarlo",
    "optimize",
    "output",
    "spec",
    "code",
];

fn recs(text: &str) -> schemify_format::Document<'_> {
    schemify_format::parse(text.as_bytes()).unwrap_or_else(|e| panic!("{e:?}\n{text}"))
}

fn attr<'a>(d: &'a schemify_format::Document<'_>, r: u32, k: &str) -> Option<&'a str> {
    d.find_attr(r, k).and_then(|a| d.attr_value(a, 0))
}

/// The text of record `r` without the attributes `skip`.
fn rec_text(d: &schemify_format::Document<'_>, r: u32, skip: &[&str]) -> String {
    let mut out: Vec<String> = d.fields(r).map(str::to_owned).collect();
    for a in d.attrs(r) {
        if skip.contains(&d.attr_key(a)) {
            continue;
        }
        out.push(format!(
            "{}={}",
            d.attr_key(a),
            d.attr_values(a).collect::<Vec<_>>().join(",")
        ));
    }
    out.join(" ")
}

/// Differences between `orig` and its stash-less round trip `got` that are not documented
/// losses.
fn undocumented_losses(path: &str, orig: &str, got: &str) -> Vec<String> {
    use schemify_format::RecordKind as K;
    let (o, g) = (recs(orig), recs(got));
    let mut bad = Vec::new();
    let by = |d: &schemify_format::Document<'_>, k: K| -> BTreeMap<String, u32> {
        (1..d.record_count())
            .filter(|&r| d.record_kind(r) == k)
            .map(|r| (d.field(r, 0).unwrap_or("").to_owned(), r))
            .collect()
    };
    // Pins: the same set, positions and connection attributes (order may differ).
    // Ground spellings `0` and `gnd` come back as `GND` (§3.3).
    let ground = |t: String| {
        t.replace("inherit=0", "inherit=GND")
            .replace("inherit=gnd", "inherit=GND")
    };
    let pins = |d: &schemify_format::Document<'_>| -> BTreeSet<String> {
        by(d, K::Pin)
            .values()
            .map(|&r| ground(rec_text(d, r, &["domain", "number"])))
            .collect()
    };
    if pins(&o) != pins(&g) {
        bad.push(format!("{path}: pins {:?} → {:?}", pins(&o), pins(&g)));
    }
    // Params: the same names; kinds equal (a computed param comes back a number).
    let (op, gp) = (by(&o, K::Param), by(&g, K::Param));
    if op.keys().collect::<Vec<_>>() != gp.keys().collect::<Vec<_>>() {
        bad.push(format!("{path}: params {:?} → {:?}", op.keys(), gp.keys()));
    }
    for (n, &r) in &op {
        let Some(&gr) = gp.get(n) else { continue };
        let ok = attr(&o, r, "kind").unwrap_or("number");
        let gk = attr(&g, gr, "kind").unwrap_or("number");
        let ok = if attr(&o, r, "computed").is_some() {
            "number"
        } else {
            ok
        };
        if ok != gk {
            bad.push(format!("{path}: param {n} kind {ok} → {gk}"));
        }
        for a in o.attrs(r) {
            let k = o.attr_key(a);
            if !LOST_PARAM_ATTRS.contains(&k)
                && !["kind", "doc", "choices"].contains(&k)
                && attr(&g, gr, k).is_none()
            {
                bad.push(format!("{path}: param {n} lost `{k}`"));
            }
        }
    }
    // Symbol graphics: the same count per form (order, attributes, closing points and
    // label templates outside §3.5 are documented losses).
    let forms = |d: &schemify_format::Document<'_>| -> BTreeMap<K, usize> {
        let mut m = BTreeMap::new();
        for r in 1..d.record_count() {
            let k = d.record_kind(r);
            if k.rank() == 2 && k != K::Stext {
                *m.entry(k).or_insert(0) += 1;
            }
        }
        m
    };
    if forms(&o) != forms(&g) {
        bad.push(format!(
            "{path}: graphics {:?} → {:?}",
            forms(&o),
            forms(&g)
        ));
    }
    // Rank 3: wires, labels, ports, noconns exactly; texts but their size; instances
    // their placement, name, ignore, inherits and the set of overrides; shapes their form.
    for k in [
        K::Wire,
        K::Label,
        K::Port,
        K::Noconn,
        K::Text,
        K::Shape,
        K::Inst,
    ] {
        let (om, gm) = (by(&o, k), by(&g, k));
        for (id, &r) in &om {
            let Some(&gr) = gm.get(id) else {
                bad.push(format!("{path}: {} {id} missing", k.word()));
                continue;
            };
            let same = match k {
                K::Text => rec_text(&o, r, &["size"]) == rec_text(&g, gr, &["size"]),
                K::Shape => o.field(r, 1) == g.field(gr, 1),
                K::Inst => {
                    let keys = |d: &schemify_format::Document<'_>, r: u32| -> BTreeSet<String> {
                        d.attrs(r)
                            .map(|a| d.attr_key(a).to_owned())
                            .filter(|k| k != "ignore")
                            .collect()
                    };
                    attr(&o, r, "name") == attr(&g, gr, "name")
                        && o.find_attr(r, "at")
                            .map(|a| o.attr_values(a).collect::<Vec<_>>())
                            == g.find_attr(gr, "at")
                                .map(|a| g.attr_values(a).collect::<Vec<_>>())
                        && keys(&g, gr).is_subset(&keys(&o, r))
                        && (attr(&o, r, "ignore") == Some("true"))
                            == (attr(&g, gr, "ignore") == Some("true"))
                }
                _ => rec_text(&o, r, &[]) == rec_text(&g, gr, &[]),
            };
            if !same {
                bad.push(format!(
                    "{path}: {} {id}: {} → {}",
                    k.word(),
                    rec_text(&o, r, &[]),
                    rec_text(&g, gr, &[])
                ));
            }
        }
        if gm.len() > om.len() {
            bad.push(format!(
                "{path}: {} extra {}",
                gm.len() - om.len(),
                k.word()
            ));
        }
    }
    // Rank 4–6 may be lost; nothing may appear from nowhere.
    for r in 1..g.record_count() {
        let w = g.record_word(r);
        if LOST_KINDS.contains(&w) && !(1..o.record_count()).any(|x| o.record_word(x) == w) {
            bad.push(format!("{path}: {w} appeared"));
        }
    }
    bad
}

/// Round trip of `files` both ways: exact with the stash; without it, every difference a
/// documented loss.
fn round_trip(files: &Files, what: &str) {
    let (text, _) = export_all(files);
    let man: Files = files
        .iter()
        .filter(|(p, _)| *p == "schemify.project")
        .map(|(p, b)| (p.clone(), b.clone()))
        .collect();
    let want = cells(files);
    for (stash, t) in [(true, text.clone()), (false, strip_stash(&text))] {
        let imp = virtuoso::import(&t, &man, &Mapping::default(), &import_opts())
            .unwrap_or_else(|e| panic!("{what}: {e:?}"));
        let got: BTreeMap<String, String> = imp
            .files
            .iter()
            .filter(|(p, _)| is_cell(p))
            .map(|(p, b)| (p.clone(), strip_fp(&String::from_utf8(b.clone()).unwrap())))
            .collect();
        assert_eq!(
            got.keys().collect::<Vec<_>>(),
            want.keys().collect::<Vec<_>>(),
            "{what}"
        );
        for (p, w) in &want {
            if stash {
                assert_eq!(&got[p], w, "{what}: {p} (with the stash)\n{t}");
            } else {
                let bad = undocumented_losses(p, w, &got[p]);
                assert!(
                    bad.is_empty(),
                    "{what}: {bad:#?}\n--- want\n{w}\n--- got\n{}",
                    got[p]
                );
            }
        }
    }
}

#[test]
fn round_trip_without_the_stash_loses_only_what_is_documented() {
    for dir in fixture_projects() {
        round_trip(&read_dir(&dir), &dir.display().to_string());
    }
}

/// A bench instantiating every core cell (with overrides), so the core cells export as
/// `schemify_core` cells (§6.5) — here renamed `mycore` so the import writes them back.
fn core_project() -> Files {
    let mut t = String::from("schemify 1 shbench\n\n");
    let mut x = 0;
    for (k, (name, _)) in schemify_doc::core_library().iter().enumerate() {
        let stem = name.trim_end_matches(".shprim");
        t.push_str(&format!(
            "inst a{k:05} core:{stem}\n  at {x} 0 R0\n  name X{k}\n"
        ));
        match stem {
            "vsource" => t.push_str("  value \"dc 1 ac 1 pulse(0 1.8 0 10p 10p 5n 10n)\"\n"),
            "isource" => t.push_str("  value \"sin(0 1m 1meg)\"\n"),
            "res" => t.push_str("  value 2k\n  m 2\n"),
            "nmos4" => t.push_str("  w 2u\n  l 0.5u\n  model nch\n"),
            _ => {}
        }
        x += 200;
    }
    [("tb.shbench".to_owned(), t.into_bytes())]
        .into_iter()
        .collect()
}

#[test]
fn the_core_library_round_trips() {
    let files = core_project();
    let d = design(&files);
    let tops: Vec<usize> = (0..d.cells.len()).collect();
    let mut opts = ExportOptions::default();
    opts.lib_map.insert(String::new(), "top".into());
    opts.lib_map.insert("core".into(), "mycore".into());
    let e = virtuoso::export(&d, &tops, &Mapping::default(), &opts).unwrap();
    assert!(e.sfyx.contains("(\"cell\" \"res\" \"design\")"));
    assert!(
        e.sfyx.contains("(\"componentName\" \"resistor\")"),
        "§6.5 simInfo"
    );
    assert!(
        e.sfyx
            .contains("(\"prop\" 1 \"srcType\" \"string\" \"pulse\")")
            || e.sfyx.contains("\"srcType\" \"string\" \"pulse\"")
    );
    let imp =
        virtuoso::import(&e.sfyx, &Files::new(), &Mapping::default(), &import_opts()).unwrap();
    for (name, bytes) in schemify_doc::core_library() {
        let path = format!("mycore/{name}");
        let got = imp
            .files
            .iter()
            .find(|f| f.0 == path)
            .unwrap_or_else(|| panic!("{path} missing"));
        assert_eq!(
            strip_fp(&String::from_utf8_lossy(&got.1)),
            canonical(bytes),
            "{path}"
        );
    }
    // Without the stash the core cells keep pins, params and graphics (§7.4 losses only).
    let bare = virtuoso::import(
        &strip_stash(&e.sfyx),
        &Files::new(),
        &Mapping::default(),
        &import_opts(),
    )
    .unwrap();
    for (name, bytes) in schemify_doc::core_library() {
        let path = format!("mycore/{name}");
        let got = bare.files.iter().find(|f| f.0 == path).unwrap();
        let mut bad =
            undocumented_losses(&path, &canonical(bytes), &String::from_utf8_lossy(&got.1));
        // A core source's `value` is SPICE text in Schemify and waveform params in its
        // CDF (§6.6): documented.
        bad.retain(|b| {
            !(name.starts_with("vsource") || name.starts_with("isource")) || !b.contains("params")
        });
        assert!(bad.is_empty(), "{bad:#?}");
    }
}

// ---------------------------------------------------------------------------------------
// Generated projects (property)
// ---------------------------------------------------------------------------------------

/// A small deterministic generator driven by one proptest seed.
struct Gen {
    s: u64,
    ids: BTreeSet<String>,
}

impl Gen {
    fn next(&mut self) -> u64 {
        self.s ^= self.s << 13;
        self.s ^= self.s >> 7;
        self.s ^= self.s << 17;
        self.s
    }
    fn n(&mut self, k: u64) -> u64 {
        self.next() % k
    }
    fn coin(&mut self) -> bool {
        self.n(2) == 0
    }
    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[self.n(v.len() as u64) as usize]
    }
    /// A grid coordinate (xschem units, 10-unit grid).
    fn c(&mut self) -> i64 {
        (self.n(40) as i64 - 20) * 10
    }
    fn id(&mut self) -> String {
        const A: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
        loop {
            let s: String = (0..6).map(|_| A[self.n(32) as usize] as char).collect();
            if self.ids.insert(s.clone()) {
                return s;
            }
        }
    }
}

fn gen_symbol(g: &mut Gen, t: &mut String, params: &[&str]) {
    for _ in 0..1 + g.n(4) {
        match g.n(5) {
            0 => t.push_str(&format!(
                "line {} {} {} {}\n",
                g.c(),
                g.c(),
                g.c() + 10,
                g.c()
            )),
            1 => t.push_str(&format!(
                "rect {} {} {} {}\n  fill true\n",
                g.c(),
                g.c(),
                g.c() + 10,
                g.c() + 10
            )),
            2 => t.push_str(&format!("circle {} {} {}\n", g.c(), g.c(), 5 + 5 * g.n(3))),
            3 => t.push_str(&format!(
                "arc {} {} 10 {} {}\n",
                g.c(),
                g.c(),
                90 * g.n(4),
                90 * (1 + g.n(3))
            )),
            _ => t.push_str(&format!(
                "poly {} {} {} {} {} {}\n  closed true\n",
                g.c(),
                g.c(),
                g.c() + 20,
                g.c(),
                g.c(),
                g.c() + 20
            )),
        }
    }
    t.push_str(&format!(
        "stext {} {} R0 {{name}}\n  size 0.2\n",
        g.c(),
        g.c()
    ));
    if let Some(p) = params.first() {
        t.push_str(&format!(
            "stext {} {} R90 {p}={{{p}}}\n  size 0.15\n  hcenter\n",
            g.c(),
            g.c()
        ));
    }
}

/// Rank-3 objects: wires, labels (on and off wires, globals, buses), ports of `pins`,
/// noconns, texts, shapes.
fn gen_schematic(g: &mut Gen, t: &mut String, pins: &[&str], bus_ok: bool) -> Vec<String> {
    let mut wires = Vec::new();
    for _ in 0..g.n(6) {
        let (x, y) = (g.c(), g.c());
        let (x2, y2) = if g.coin() {
            (x + 10 * (1 + g.n(5) as i64), y)
        } else {
            (x, y + 10 * (1 + g.n(5) as i64))
        };
        let id = g.id();
        t.push_str(&format!("wire {id} {x} {y} {x2} {y2}\n"));
        if bus_ok && g.n(4) == 0 {
            t.push_str("  bus true\n");
        }
        wires.push((x, y, x2, y2));
    }
    for _ in 0..g.n(5) {
        let (x, y) = match wires.get(g.n(wires.len() as u64 + 1) as usize) {
            Some(w) if g.coin() => (w.0, w.1),
            _ => (g.c(), g.c()),
        };
        let net = g.pick(&[
            "a", "net1", "VDD", "0", "d[3:0]", "2*b", "q[1]", "x!", "n_2",
        ]);
        let id = g.id();
        let o = g.pick(&["R0", "R90", "R180", "MR0"]);
        t.push_str(&format!("label {id} {x} {y} {o} {net}\n"));
        if net == "VDD" && g.coin() {
            t.push_str("  global true\n");
        }
    }
    for p in pins {
        let id = g.id();
        t.push_str(&format!(
            "port {id} {} {} {} {p}\n",
            g.c(),
            g.c(),
            g.pick(&["R0", "R180", "R270"])
        ));
    }
    for _ in 0..g.n(2) {
        let id = g.id();
        t.push_str(&format!("noconn {id} {} {}\n", g.c(), g.c()));
    }
    for _ in 0..g.n(2) {
        let id = g.id();
        t.push_str(&format!(
            "text {id} {} {} R0 \"note {}\"\n  size 0.3\n",
            g.c(),
            g.c(),
            g.n(100)
        ));
    }
    if g.coin() {
        let id = g.id();
        t.push_str(&format!(
            "shape {id} rect {} {} {} {}\n  dash 1\n",
            g.c(),
            g.c(),
            g.c() + 30,
            g.c() + 30
        ));
    }
    if g.coin() {
        let id = g.id();
        t.push_str(&format!("shape {id} circle {} {} 15\n", g.c(), g.c()));
    }
    Vec::new()
}

/// A project: `dev/dev.shprim` (a device with spectre and spice formats), `sub.shcomp`
/// (bus, inherited and alias pins, params of every kind, docs), `top.shcomp` (iterated
/// instances, overrides of every kind, waivers, constraints) and `tb.shbench`.
fn gen_project(seed: u64) -> Files {
    let mut g = Gen {
        s: seed | 1,
        ids: BTreeSet::new(),
    };
    let mut f = Files::new();
    f.insert(
        "schemify.project".into(),
        b"schemify 1 project\n\nlib dev path dev\n".to_vec(),
    );
    // The device.
    let mut dev = String::from(
        "schemify 1 shprim\n  doc \"generated device\"\n  prefix M\n\npin d inout 20 -30\npin g in -20 0\npin s inout 20 30\npin b in 20 0\n  inherit VSS\n\n",
    );
    dev.push_str("param w 1u\n  unit m\n  min 0\nparam l 180n\n  unit m\nparam nf 1\n  kind int\nparam model nch\n  kind string\nparam flavor lvt\n  kind choice\n  choices lvt hvt\n\n");
    dev.push_str("impl spice spice\n  format spice \"M{name} {d} {g} {s} {b} {model} w={w} l={l} nf={nf}\"\n  format spectre \"{name} ({d} {g} {s} {b}) {model} w={w} l={l} nf={nf}\"\n\n");
    gen_symbol(&mut g, &mut dev, &["w"]);
    f.insert("dev/dev.shprim".into(), dev.into_bytes());
    // The component.
    let mut sub = String::from(
        "schemify 1 shcomp\n  doc \"generated component\"\n\npin A in -40 0\npin Y out 40 0\npin D[3:0] inout 0 -40\npin VDD inout 0 40\n  inherit VDD\npin Y2 out 40 20\n  alias Y\n\n",
    );
    sub.push_str("param k 2*n\n  unit Ohm\n  max 10k\nparam n 3\n  kind int\nparam s abc\n  kind string\nparam c x\n  kind choice\n  choices x y\nparam on true\n  kind bool\nparam kk 1\n  computed k*2\n\nimpl sch schematic\n\n");
    gen_symbol(&mut g, &mut sub, &["k"]);
    let di = g.id();
    sub.push_str(&format!(
        "inst {di} dev:dev\n  at 100 0 R0\n  name M1\n  w k*1u\n  nf 2\n"
    ));
    gen_schematic(&mut g, &mut sub, &["A", "Y", "D[3:0]"], true);
    sub.push_str(&format!(
        "\nprose {}\n  | # Sub\n  |\n  | $x^2$\nvar wq 2*k\n  unit m\n",
        g.id()
    ));
    f.insert("sub.shcomp".into(), sub.into_bytes());
    // The top.
    let mut top = String::from(
        "schemify 1 shcomp\n\npin IN in -40 0\npin OUT out 40 0\n\nparam p 1k\n\nimpl sch schematic\n\n",
    );
    gen_symbol(&mut g, &mut top, &["p"]);
    let x1 = g.id();
    let name = g.pick(&["X1", "X1[1:0]"]);
    top.push_str(&format!(
        "inst {x1} ./sub\n  at 0 0 {}\n  name {name}\n",
        g.pick(&["R0", "R90", "MR0", "MR270"])
    ));
    if g.coin() {
        top.push_str("  k 2*p\n");
    }
    if g.coin() {
        top.push_str("  n 5\n  s hello\n  c y\n  on false\n");
    }
    if g.coin() {
        top.push_str("  inherit VDD vdda\n");
    }
    let r1 = g.id();
    top.push_str(&format!(
        "inst {r1} core:res\n  at 200 0 R0\n  name R1\n  value {}\n",
        g.pick(&["2k", "1meg", "4.7k"])
    ));
    let v1 = g.id();
    top.push_str(&format!(
        "inst {v1} core:vsource\n  at 300 0 R0\n  name V1\n  value \"{}\"\n",
        g.pick(&["dc 1", "DC 1 AC 1", "pulse(0 1 0 1n 1n 5n 10n)"])
    ));
    if g.coin() {
        top.push_str("  ignore true\n");
    }
    gen_schematic(&mut g, &mut top, &["IN", "OUT"], true);
    top.push_str(&format!(
        "\nconstraint {} match\n  member {x1}\n  member {r1}\n",
        g.id()
    ));
    top.push_str(&format!(
        "waive {} {r1} value\n  reason \"generated\"\n",
        g.id()
    ));
    f.insert("top.shcomp".into(), top.into_bytes());
    // The bench.
    let mut tb = String::from("schemify 1 shbench\n  corner tt\n\n");
    let t1 = g.id();
    tb.push_str(&format!(
        "inst {t1} ./top\n  at 0 0 R0\n  name X0\n  p vbias*2\n"
    ));
    gen_schematic(&mut g, &mut tb, &[], false);
    tb.push_str("\nvar vbias 1k\n\ncorner tt\n  temp 27\n\noutput vout \"at(v(OUT), 1n)\"\n\nspec vout\n  min 0.1\n\ncode tran spice\n  | .tran 1n 10n\n");
    f.insert("tb.shbench".into(), tb.into_bytes());
    f
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig { cases: 48, failure_persistence: None, ..Default::default() })]
    #[test]
    fn generated_projects_round_trip(seed in proptest::prelude::any::<u64>()) {
        let files = gen_project(seed);
        round_trip(&files, &format!("seed {seed}"));
    }
}

// ---------------------------------------------------------------------------------------
// Merge with existing files (§7.3)
// ---------------------------------------------------------------------------------------

fn file<'a>(files: &'a [(String, Vec<u8>)], path: &str) -> &'a str {
    std::str::from_utf8(
        &files
            .iter()
            .find(|f| f.0 == path)
            .unwrap_or_else(|| panic!("{path} missing"))
            .1,
    )
    .unwrap()
}

/// Inserts `extra` after the first line matching `after` (a crude local edit).
fn edit(text: &str, after: &str, extra: &str) -> String {
    let mut out = String::new();
    let mut done = false;
    for l in text.lines() {
        out.push_str(l);
        out.push('\n');
        if !done && l == after {
            out.push_str(extra);
            done = true;
        }
    }
    assert!(done, "no line `{after}` in\n{text}");
    out
}

#[test]
fn import_onto_existing_files_keeps_schemify_only_data() {
    let text = std::fs::read_to_string(fixtures().join("pdk.sfyx")).unwrap();
    let first = virtuoso::import(
        &text,
        &Files::new(),
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap();
    let dev = file(&first.files, "gpdk/nmos1v.shprim");
    // A teammate documents the device in Schemify: header doc, a bound, a spice format.
    let mut local = edit(
        dev,
        "  provenance \"virtuoso gpdk/nmos1v\"",
        "  doc \"thin-oxide nmos\"\n",
    );
    local = edit(&local, "  doc \"Total width\"", "  min 120n\n  max 100u\n");
    local = edit(
        &local,
        "impl spice spice",
        "  format spice \"X{name} {D} {G} {S} {B} nmos1v w={w} l={l}\"\n",
    );
    let local = canonical(local.as_bytes());
    let mut existing: Files = first.files.iter().cloned().collect();
    existing.insert("gpdk/nmos1v.shprim".into(), local.clone().into_bytes());
    // Unchanged in Virtuoso: the local file stays as it is.
    let again = virtuoso::import(
        &text,
        &existing,
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap();
    assert_eq!(
        strip_fp(file(&again.files, "gpdk/nmos1v.shprim")),
        strip_fp(&local)
    );
    // Changed in Virtuoso (a new default for w): the CDF wins, the bound, doc and spice
    // format stay.
    let changed = text.replace("\"w\" \"string\" \"120n\"", "\"w\" \"string\" \"240n\"");
    let m = virtuoso::import(
        &changed,
        &existing,
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap();
    let got = file(&m.files, "gpdk/nmos1v.shprim");
    assert!(got.contains("param w 240n"), "{got}");
    for kept in [
        "doc \"thin-oxide nmos\"",
        "  min 120n",
        "  max 100u",
        "format spice \"X{name}",
    ] {
        assert!(got.contains(kept), "{kept} lost:\n{got}");
    }
    assert!(
        m.notes
            .rows
            .iter()
            .any(|r| r.class == "updated" && r.cell == "gpdk/nmos1v.shprim")
    );
}

#[test]
fn stale_schemify_data_is_dropped_with_a_note() {
    let text = std::fs::read_to_string(fixtures().join("inverter.sfyx")).unwrap();
    let first = virtuoso::import(
        &text,
        &Files::new(),
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap();
    let tb = file(&first.files, "demo/tb_inv.shbench").to_owned();
    // Docs, a bench variable and a waiver on V0 survive; a waiver on a deleted instance
    // goes.
    let v0 = tb
        .lines()
        .find(|l| l.starts_with("inst ") && tb.contains("name V0"))
        .unwrap();
    let v0_id = tb
        .split("\ninst ")
        .find(|b| b.contains("name V0"))
        .and_then(|b| b.split_whitespace().next())
        .unwrap()
        .to_owned();
    let _ = v0;
    let local = format!(
        "{tb}\nwaive 0zzzzz {v0_id} vdc\n  reason \"supply is fixed\"\nwaive 1zzzzz 9zzzzz vdc\n  reason \"old instance\"\n\nprose 2zzzzz\n  | # The bench\n\nvar vdd 1.8\n"
    );
    let local = canonical(local.as_bytes());
    let mut existing: Files = first.files.iter().cloned().collect();
    existing.insert("demo/tb_inv.shbench".into(), local.into_bytes());
    let m = virtuoso::import(
        &text,
        &existing,
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap();
    let got = file(&m.files, "demo/tb_inv.shbench");
    assert!(got.contains("waive 0zzzzz"), "{got}");
    assert!(!got.contains("waive 1zzzzz"), "{got}");
    assert!(
        got.contains("prose 2zzzzz") && got.contains("var vdd 1.8"),
        "{got}"
    );
    let stale: Vec<_> = m
        .notes
        .rows
        .iter()
        .filter(|r| r.class == "dropped:stale-schemify-data")
        .collect();
    assert_eq!(stale.len(), 1, "{:?}", m.notes.rows);
    assert!(stale[0].detail.contains("1zzzzz"));
}

#[test]
fn reimporting_an_unchanged_dump_is_a_no_op() {
    for name in ["inverter", "bus", "symbol", "pdk", "netexpr"] {
        let text = std::fs::read_to_string(fixtures().join(format!("{name}.sfyx"))).unwrap();
        let first = virtuoso::import(
            &text,
            &Files::new(),
            &Mapping::default(),
            &ImportOptions::default(),
        )
        .unwrap();
        let existing: Files = first.files.iter().cloned().collect();
        let again = virtuoso::import(
            &text,
            &existing,
            &Mapping::default(),
            &ImportOptions::default(),
        )
        .unwrap();
        for (p, b) in &again.files {
            assert_eq!(
                String::from_utf8_lossy(b),
                String::from_utf8_lossy(&existing[p]),
                "{name}: {p}"
            );
        }
        assert!(
            !again.notes.rows.iter().any(|r| r.class == "updated"),
            "{name}: {:?}",
            again.notes.rows
        );
    }
    // Also for a project exported from Schemify and imported twice.
    for dir in fixture_projects() {
        let files = read_dir(&dir);
        let (text, _) = export_all(&files);
        let man: Files = files
            .iter()
            .filter(|(p, _)| *p == "schemify.project")
            .map(|(p, b)| (p.clone(), b.clone()))
            .collect();
        let first = virtuoso::import(&text, &man, &Mapping::default(), &import_opts()).unwrap();
        let mut existing = man.clone();
        existing.extend(first.files.iter().cloned());
        let again =
            virtuoso::import(&text, &existing, &Mapping::default(), &import_opts()).unwrap();
        for (p, b) in &again.files {
            assert_eq!(
                String::from_utf8_lossy(b),
                String::from_utf8_lossy(&existing[p]),
                "{}: {p}",
                dir.display()
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// Re-mastering keeps every net (§6.4)
// ---------------------------------------------------------------------------------------

/// Per instance id, per pin index, the set of (instance id, pin) it shares a net with:
/// the net partition over instance pins, independent of net names.
fn partition(
    d: &Design,
    ci: usize,
    pin_names: &dyn Fn(&str, usize) -> String,
) -> BTreeSet<BTreeSet<String>> {
    let g = interop::design::geom(d, ci);
    let n = interop::nets::extract(d, ci, &g);
    let mut by_net: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (k, pins) in n.inst_pin.iter().enumerate() {
        for (p, node) in pins.iter().enumerate() {
            if *node == interop::nets::NO_NODE {
                continue;
            }
            // Same-named nets are one net (elab connects labels by name).
            let net = &n.net_name[n.node_net[*node as usize] as usize];
            by_net.entry(net.clone()).or_default().insert(format!(
                "{}.{}",
                g.inst_id[k],
                pin_names(&g.inst_id[k], p)
            ));
        }
    }
    by_net.into_values().collect()
}

#[test]
fn remastered_devices_keep_every_net() {
    // A bench of core resistors and a source; `dev:res2` has the same pins elsewhere.
    let mut f = Files::new();
    f.insert(
        "schemify.project".into(),
        b"schemify 1 project\n\nlib dev path dev\n".to_vec(),
    );
    f.insert(
        "dev/res2.shprim".into(),
        b"schemify 1 shprim\n\npin a inout 0 -50\npin b inout 0 50\n\nparam r 1k\n\nimpl spice spice\n  format spice \"R{name} {a} {b} {r}\"\n\nrect -10 -40 10 40\n".to_vec(),
    );
    f.insert(
        "tb.shbench".into(),
        b"schemify 1 shbench\n\ninst aaaaa1 core:res\n  at 0 0 R0\n  name R1\ninst aaaaa2 core:res\n  at 0 60 R0\n  name R2\ninst aaaaa3 core:res\n  at 100 0 R90\n  name R3\ninst aaaaa4 core:vsource\n  at -100 0 R0\n  name V1\n\nwire bbbbb1 0 30 0 30.5\nwire bbbbb2 -100 -30 0 -30\nwire bbbbb3 -100 30 -100 120\nwire bbbbb4 -100 120 0 120\nwire bbbbb5 0 90 0 120\nwire bbbbb6 70 0 0 0\n\nlabel ccccc1 130 0 R0 out\nlabel ccccc2 -100 120 R0 0\n".to_vec(),
    );
    let d = design(&f);
    let ci = d.find("tb.shbench").unwrap();
    let g = interop::design::geom(&d, ci);
    let n = interop::nets::extract(&d, ci, &g);
    let res2 = interop::design::sym_pins(&d.cells[d.find("dev/res2.shprim").unwrap()]);
    let todo: Vec<interop::remap::Remaster> = (0..g.inst_id.len())
        .filter(|&k| d.cells[g.inst_master[k] as usize].name == "res")
        .map(|k| interop::remap::Remaster {
            inst: k,
            pins: vec![(0, "a".into()), (1, "b".into())],
            new_pins: Some(
                res2.name
                    .iter()
                    .cloned()
                    .zip(res2.at.iter().copied())
                    .collect(),
            ),
        })
        .collect();
    let e = interop::remap::apply(&g, &n, &todo);
    assert!(!e.stubs.is_empty() && !e.labels.is_empty());
    let masters: Vec<interop::remap::NewMaster> = todo
        .iter()
        .map(|t| (t.inst, "dev:res2".to_owned(), None))
        .collect();
    let mut k = 0;
    let doc = interop::remap::apply_to_doc(&d.cells[ci].doc, &g, &e, &masters, || {
        k += 1;
        format!("zz{k:04}")
    });
    let mut f2 = f.clone();
    f2.insert(
        "tb.shbench".into(),
        schemify_format::write(&doc).into_bytes(),
    );
    let d2 = design(&f2);
    let ci2 = d2.find("tb.shbench").unwrap();
    // Old pin p of a re-mastered res is new pin p of res2 (same index).
    let names = |_: &str, p: usize| p.to_string();
    assert_eq!(
        partition(&d, ci, &names),
        partition(&d2, ci2, &names),
        "\n{}",
        schemify_format::write(&doc)
    );
    // And the edits are what §6.4 says: every new pin got a stub, nothing was left
    // dangling at an old pin point.
    assert_eq!(e.stubs.len(), 2 * todo.len());
}

#[test]
fn analoglib_import_remasters_onto_core_cells_keeping_nets() {
    let text = std::fs::read_to_string(fixtures().join("inverter.sfyx")).unwrap();
    let plain = virtuoso::import(
        &text,
        &Files::new(),
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap();
    let opts = ImportOptions {
        analoglib: true,
        ..ImportOptions::default()
    };
    let core = virtuoso::import(&text, &Files::new(), &Mapping::default(), &opts).unwrap();
    let tb = file(&core.files, "demo/inv.shcomp");
    assert!(
        tb.contains("core:nmos4") && tb.contains("core:pmos4"),
        "{tb}"
    );
    assert!(core.notes.rows.iter().any(|r| r.class == "remapped"));
    let bench = file(&core.files, "demo/tb_inv.shbench");
    assert!(
        bench.contains("core:vsource") && bench.contains("value \"DC 1.8\""),
        "{bench}"
    );
    let pf: Files = plain.files.iter().cloned().collect();
    let cf: Files = core.files.iter().cloned().collect();
    let (dp, dc) = (design(&pf), design(&cf));
    for cell in ["demo/inv.shcomp", "demo/tb_inv.shbench"] {
        let names = |_: &str, p: usize| p.to_string();
        assert_eq!(
            partition(&dp, dp.find(cell).unwrap(), &names),
            partition(&dc, dc.find(cell).unwrap(), &names),
            "{cell}\n{}",
            file(&core.files, cell)
        );
    }
}

// ---------------------------------------------------------------------------------------
// Negative space: malformed dumps, unknown records, export refusals (§4, §8.2, §12)
// ---------------------------------------------------------------------------------------

#[test]
fn malformed_or_truncated_dumps_are_refused_with_the_line() {
    let head = "(\"sfyx\" 1 \"t\")\n(\"lib\" \"L\")\n(\"cell\" \"c\" \"design\")\n(\"cv\" \"schematic\" \"schematic\" 160)\n";
    for (body, line, why) in [
        (
            "(\"inst\" 1 \"I0\" \"a\" \"b\" \"symbol\" 0 0)\n",
            5,
            "fields",
        ),
        (
            "(\"inst\" 1 \"I0\" \"a\" \"b\" \"symbol\" 0 0 \"R45\")\n",
            5,
            "orientation",
        ),
        ("(\"wire\" 1 (0 0 10) 0)\n", 5, "even"),
        ("(\"wire\" 0 (0 0 10 0) 0)\n", 5, "positive"),
        (
            "(\"label\" 1 7 \"n\" 0 0 \"R0\" \"lowerLeft\" \"stick\" 10 \"normal\" \"wire\" \"label\")\n",
            3,
            "unknown ref",
        ),
        ("(\"prop\" 9 \"x\" \"string\" \"1\")\n", 3, "unknown ref"),
        ("(\"prop\" 0 \"x\" \"blob\" \"1\")\n", 5, "type"),
        (
            "(\"shape\" 1 \"blob\" \"text\" \"drawing\" (0 0 1 1) 0)\n",
            5,
            "kind",
        ),
        (
            "(\"wire\" 1 (0 0 10 0) 0)\n(\"wire\" 1 (0 0 0 10) 0)\n",
            3,
            "twice",
        ),
        (
            "(\"param\" \"w\" \"string\" \"1\" \"w\" \"\" () () \"\")\n",
            5,
            "cdf",
        ),
        ("(\"var\" \"x\" \"1\" \"\")\n", 5, "maestro"),
        ("(\"wire\" 1 (0 0 1.5 0) 0)\n", 5, "float"),
        ("(\"wire\" 1 (0 0 010 0) 0)\n", 5, "leading zero"),
    ] {
        let t = format!("{head}{body}(\"end\")\n");
        let e = virtuoso::import(
            &t,
            &Files::new(),
            &Mapping::default(),
            &ImportOptions::default(),
        )
        .expect_err(body);
        assert!(
            e.iter()
                .any(|m| m.contains(&format!("line {line}")) && m.to_lowercase().contains(why)),
            "{body}: {e:?}"
        );
    }
    let e = virtuoso::import(
        head,
        &Files::new(),
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .expect_err("truncated");
    assert!(e[0].contains("truncated"), "{e:?}");
    let e = virtuoso::import(
        "(\"inst\" 1)\n",
        &Files::new(),
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .expect_err("no header");
    assert!(e[0].contains("sfyx v1"), "{e:?}");
    assert!(virtuoso::import_targets(head).is_err());
    // An object outside any cellview.
    let t = "(\"sfyx\" 1 \"t\")\n(\"wire\" 1 (0 0 1 0) 0)\n(\"end\")\n";
    let e = virtuoso::import(
        t,
        &Files::new(),
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap_err();
    assert!(
        e[0].contains("line 2") && e[0].contains("outside a cellview"),
        "{e:?}"
    );
}

#[test]
fn unknown_records_are_reported_never_dropped_silently() {
    let t = "(\"sfyx\" 1 \"t\")\n(\"frob\" 1)\n(\"lib\" \"L\")\n(\"cell\" \"c\" \"design\")\n(\"cv\" \"schematic\" \"schematic\" 160)\n(\"wire\" 1 (0 0 10 0) 0)\n(\"inst\" 2 \"I0\" \"L\" \"x\" \"symbol\" 0 0 \"R0\")\n(\"blink\" 3 \"x\")\n(\"conn\" 2 \"A\" \"n1\")\n(\"intent\" 2 \"nmos\" ((\"gmid\" \"10\")))\n(\"shape\" 4 \"ellipse\" \"wire\" \"drawing\" (0 0 2 2) 0)\n(\"label\" 5 0 \"x\" 0 0 \"R0\" \"lowerLeft\" \"stick\" 10 \"normal\" \"instance\" \"label\")\n(\"prop\" 0 \"lastSchematicExtraction\" \"string\" \"t\")\n(\"end\")\n";
    let imp = virtuoso::import(
        t,
        &Files::new(),
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap();
    let c = imp.notes.counts();
    assert_eq!(
        c.get("dropped:unknown-record"),
        Some(&3),
        "{}",
        imp.notes.text()
    ); // frob, blink, intent
    assert!(
        imp.notes.text().contains("`blink` on line 8"),
        "{}",
        imp.notes.text()
    );
    assert!(imp.notes.text().contains("solder-dot"));
    assert!(imp.notes.text().contains("lastSchematicExtraction"));
    assert!(imp.notes.text().contains("conn A n1"));
    // The count conservation asserts inside the import held (it returned at all); the
    // wire and the instance landed.
    let f = file(&imp.files, "L/c.shbench");
    assert!(f.contains("wire ") && f.contains("inst "), "{f}");
}

#[test]
fn export_refuses_a_mapping_that_leaves_a_pin_unmapped() {
    let mut f = Files::new();
    f.insert(
        "schemify.project".into(),
        b"schemify 1 project\n\nlib dev path dev\n".to_vec(),
    );
    f.insert(
        "dev/nfet.shprim".into(),
        b"schemify 1 shprim\n\npin d inout 0 -30\npin g in -20 0\npin s inout 0 30\n\nparam W 1\n\nimpl spice spice\n\nline 0 -30 0 30\n".to_vec(),
    );
    f.insert(
        "tb.shbench".into(),
        b"schemify 1 shbench\n\ninst aaaaaa dev:nfet\n  at 0 0 R0\n  name M1\n  W 2\n".to_vec(),
    );
    let d = design(&f);
    let tops = [d.find("tb.shbench").unwrap()];
    let bad = Mapping::parse("map dev:nfet virtuoso pdk nch\n  pin d D\n  pin g G\n  param L l\n")
        .unwrap();
    let e = virtuoso::export(&d, &tops, &bad, &ExportOptions::default()).unwrap_err();
    assert!(
        e.iter().any(|m| m.contains("pin s has no `pin` row")),
        "{e:?}"
    );
    assert!(e.iter().any(|m| m.contains("no param L")), "{e:?}");
    // A complete row re-masters with `conn` records (the target's geometry is unknown)
    // and scales the width.
    let good = Mapping::parse("map dev:nfet virtuoso pdk nch\n  pin d D\n  pin g G\n  pin s S\n  param W w scale 1e-6\n  set mult 1\n").unwrap();
    let x = virtuoso::export(&d, &tops, &good, &ExportOptions::default()).unwrap();
    assert!(
        x.sfyx
            .contains("(\"inst\" 1 \"M1\" \"pdk\" \"nch\" \"symbol\" 0 0 \"R0\")"),
        "{}",
        x.sfyx
    );
    assert!(
        x.sfyx.contains("\"w\" \"string\" \"0.000002\"")
            || x.sfyx.contains("\"w\" \"string\" \"2e-6\""),
        "{}",
        x.sfyx
    );
    assert!(
        x.sfyx.contains("(\"prop\" 1 \"mult\" \"string\" \"1\")"),
        "{}",
        x.sfyx
    );
    assert_eq!(x.sfyx.matches("(\"conn\" 1 ").count(), 3, "{}", x.sfyx);
    assert!(x.notes.rows.iter().any(|r| r.class == "remapped"));
}

// ---------------------------------------------------------------------------------------
// §3.4 property: AEL translation preserves the value
// ---------------------------------------------------------------------------------------

fn eval(src: &str, w: f64) -> Option<f64> {
    use schemify_expr::{Mode, Program, Scratch, Value};
    let p = Program::compile(src, Mode::Scalar).ok()?;
    let mut inputs = Vec::new();
    p.bind(&mut inputs, |n| (n == "w").then_some(Value::Num(w)))
        .ok()?;
    match p.eval(&inputs, &mut Scratch::default()).ok()? {
        Value::Num(x) => Some(x),
        Value::Int(i) => Some(i as f64),
        _ => None,
    }
}

fn arb_expr() -> impl proptest::strategy::Strategy<Value = String> {
    use proptest::prelude::*;
    let num = (
        1u32..999,
        0u32..3,
        proptest::sample::select(vec![
            "", "f", "p", "n", "u", "m", "M", "k", "K", "meg", "MEG", "g", "t", "mil",
        ]),
    )
        .prop_map(|(m, d, s)| {
            if d == 0 {
                format!("{m}{s}")
            } else {
                format!("{}.{}{s}", m / 10 + 1, m % 10)
            }
        });
    let leaf = prop_oneof![num, Just("w".to_owned()), Just("pi".to_owned())];
    leaf.prop_recursive(4, 24, 3, |inner| {
        prop_oneof![
            (
                inner.clone(),
                proptest::sample::select(vec!["+", "-", "*", "/"]),
                inner.clone()
            )
                .prop_map(|(a, o, b)| format!("{a} {o} {b}")),
            inner.clone().prop_map(|a| format!("-({a})")),
            inner.clone().prop_map(|a| format!("({a})")),
            (inner.clone(), 1u32..3).prop_map(|(a, k)| format!("({a})^{k}")),
            (
                inner.clone(),
                proptest::sample::select(vec![
                    "abs", "sqrt", "exp", "ln", "sin", "cos", "atan", "floor", "ceil"
                ])
            )
                .prop_map(|(a, f)| format!("{f}(abs({a}) + 1)")),
            (inner.clone(), inner).prop_map(|(a, b)| format!("max({a}, {b})")),
        ]
    })
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig { cases: 512, failure_persistence: None, ..Default::default() })]
    #[test]
    fn ael_translation_preserves_values(e in arb_expr(), w in 1e-9f64..1e3) {
        use virtuoso::ael::{AelName, Name, from_ael, to_ael};
        let Some(want) = eval(&e, w) else { return Ok(()) };
        let a = to_ael(&e, |n| if n == "w" { Name::ParentParam } else { Name::Unknown }).unwrap();
        let back = from_ael(&a.text, |n| if n == "w" { AelName::Param } else { AelName::Unknown }, |_| None).unwrap();
        let got = eval(&back.text, w).unwrap_or_else(|| panic!("{e} → {} → {} does not evaluate", a.text, back.text));
        let tol = 1e-12 * want.abs().max(1e-300);
        proptest::prop_assert!((got - want).abs() <= tol, "{} → {} → {}: {} vs {}", e, a.text, back.text, want, got);
    }
}
