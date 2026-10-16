//! Maestro ⇄ bench (spec `interop.md` §9): generated benches round trip through the
//! maestro records (Rust only, and through the SKILL loader/dumper on the mock maestro),
//! hand-written maestro dumps import to golden benches, malformed records are refused.

mod common;
mod skill;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use common::{load_tops, strip_stash};
use interop::design::{Design, Files};
use interop::map::Mapping;
use interop::notes::Notes;
use interop::sfyx::{self, Record, Value};
use interop::virtuoso::{self, ExportOptions, ImportOptions};
use skill::mockdb::MockDb;
use skill::{Interp, V};

// ---------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------

fn canonical(text: &str) -> String {
    let d = schemify_format::parse(text.as_bytes()).unwrap_or_else(|e| panic!("{e:?}\n{text}"));
    schemify_format::write(&d)
}

fn project(bench: &str) -> Files {
    let mut f = Files::new();
    f.insert("schemify.project".into(), b"schemify 1 project\n".to_vec());
    f.insert("tb.shbench".into(), canonical(bench).into_bytes());
    f
}

fn design(files: &Files) -> Design {
    load_tops(files, &["tb.shbench"])
}

/// The `.sfyx` export of `tb.shbench` and its notes.
fn export(files: &Files) -> (String, Notes) {
    let d = design(files);
    let tb = d.find("tb.shbench").expect("loaded");
    let opts = ExportOptions {
        producer: "test".into(),
        ..ExportOptions::default()
    };
    let e =
        virtuoso::export(&d, &[tb], &Mapping::default(), &opts).unwrap_or_else(|e| panic!("{e:?}"));
    (e.sfyx, e.notes)
}

fn maestro(text: &str) -> Vec<Record> {
    let recs = sfyx::read(text).unwrap();
    let k = recs
        .iter()
        .position(|r| r.tag == "maestro")
        .unwrap_or(recs.len() - 1);
    recs[k..recs.len() - 1]
        .iter()
        .map(|r| Record::new(&r.tag, r.fields.clone()))
        .collect()
}

/// The bench an import of `text` writes (`existing`: the files it merges with).
fn import(text: &str, existing: &Files) -> (String, Notes) {
    let imp = virtuoso::import(
        text,
        existing,
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{e:?}\n{text}"));
    let tb = imp
        .files
        .iter()
        .find(|(p, _)| p == "tb.shbench")
        .unwrap_or_else(|| {
            panic!(
                "no tb.shbench in {:?}",
                imp.files.iter().map(|f| &f.0).collect::<Vec<_>>()
            )
        });
    // Without the import fingerprints (`vsym`/`vsch` header lines, spec §7.5), which an
    // import adds by design.
    let text: String = String::from_utf8(tb.1.clone())
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("  vsym ") && !l.starts_with("  vsch "))
        .map(|l| format!("{l}\n"))
        .collect();
    (text, imp.notes)
}

/// Rank-4/6 records of a bench as `kind name` → record text (fields and attributes).
fn tables(text: &str) -> BTreeMap<String, String> {
    let d = schemify_format::parse(text.as_bytes()).unwrap();
    let mut out = BTreeMap::new();
    for r in 1..d.record_count() {
        let w = d.record_word(r);
        if !["var", "sweep", "corner", "output", "spec", "code"].contains(&w) {
            continue;
        }
        let mut t: Vec<String> = d.fields(r).map(str::to_owned).collect();
        for a in d.attrs(r) {
            t.push(format!(
                "{}={}",
                d.attr_key(a),
                d.attr_values(a).collect::<Vec<_>>().join(",")
            ));
        }
        for v in d.verbatim(r) {
            t.push(format!("|{v}"));
        }
        out.insert(format!("{w} {}", d.field(r, 0).unwrap_or("")), t.join(" "));
    }
    out
}

/// A splitmix64 stream: the generator's only randomness.
struct Gen(u64);

impl Gen {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn coin(&mut self) -> bool {
        self.next() & 1 == 1
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[(self.next() % xs.len() as u64) as usize]
    }
}

/// A generated bench: its text, the `kind name` keys §9 brings back exactly, and the
/// `kind name` keys a stash-less round trip loses (documented losses).
struct Bench {
    text: String,
    exact: Vec<String>,
    lost: Vec<String>,
}

fn gen_bench(seed: u64, with_param: bool) -> Bench {
    let mut g = Gen(seed);
    let mut t = String::from("schemify 1 shbench\n\n");
    let mut exact = Vec::new();
    let mut lost = Vec::new();
    // Vars (no attributes: those are Schemify-only).
    t.push_str("var vdd 1.8\n");
    exact.push("var vdd".into());
    let r = g.pick(&["1k", "2.2k", "1meg", "470"]);
    t.push_str(&format!("var R {r}\n"));
    exact.push("var R".into());
    let c = g.pick(&["1p", "10f", "2*vdd", "(vdd+1)/2"]);
    t.push_str(&format!("var C {c}\n"));
    exact.push("var C".into());
    t.push_str("var W 1u\n");
    match g.next() % 4 {
        0 => t.push_str("sweep W\n  var W\n  lin 1 3 3\n"),
        1 => t.push_str("sweep W\n  var W\n  log 1 100 3\n"),
        2 => t.push_str("sweep W\n  var W\n  values 1k 2k 4k\n"),
        _ => {
            exact.push("var W".into());
        }
    }
    if t.contains("sweep W") {
        exact.push("sweep W".into());
    }
    if g.coin() {
        t.push_str("sweep s2\n  var R\n  values 1 2\n"); // named after no var: not mapped
        lost.push("sweep s2".into());
    }
    // Corners: models in order, sets by var name, temp last (the import's order).
    t.push_str("corner ff\n  lib \"${PDK}/models.lib\" ff\n  include extra.inc\n  set vdd 1.95\n  temp -40\n");
    t.push_str("corner tt\n  lib models.lib tt\n  temp 27\n");
    if g.coin() {
        t.push_str("corner ss\n  set C 2p\n  set R 2k\n");
        exact.push("corner ss".into());
    }
    exact.extend(["corner ff".into(), "corner tt".into()]);
    // The analysis block, and one block the export cannot map.
    let mut lines = vec![];
    if g.coin() {
        lines.push(".include models/top.inc".to_owned());
    }
    if g.coin() {
        lines.push(".lib ${PDK}/m.lib tt".to_owned());
    }
    if g.coin() {
        lines.push(".temp 50".to_owned());
    }
    lines.push(
        g.pick(&[".tran 1n 1u", ".tran 1p 10n 0", ".tran 1n 1u 0 1n"])
            .to_owned(),
    );
    if g.coin() {
        lines.push(
            g.pick(&[".ac dec 10 1 1g", ".ac lin 100 1k 10meg"])
                .to_owned(),
        );
    }
    if g.coin() {
        lines.push(".dc V1 0 1.8 0.1".into());
    }
    if g.coin() {
        lines.push(".op".into());
    }
    if g.coin() {
        lines.push(".noise v(out) V1 dec 10 1 1meg".into());
    }
    if with_param {
        lines.push(".param k=2".into());
    }
    t.push_str("code sim spice\n");
    for l in &lines {
        t.push_str(&format!("  | {l}\n"));
    }
    t.push_str("code models spectre\n  | simulator lang=spectre\n  | include \"x.scs\"\n");
    exact.push("code models".into());
    if g.coin() {
        t.push_str("code zz spice\n  | .save all\n");
    }
    // Outputs and specs.
    t.push_str(
        "output vmax \"ymax(v(out))\"\noutput gain \"(ymax(v(out)) - ymin(v(out))) / vdd\"\n",
    );
    t.push_str("output tr \"delay(v(in), 0.9, v(out), 0.9)\"\n");
    t.push_str("spec vmax\n  min 1\n  max 1.7\nspec gain\n  max 2\nspec tr\n  max 1n\n");
    exact.extend([
        "output vmax".into(),
        "output gain".into(),
        "spec vmax".into(),
        "spec gain".into(),
    ]);
    lost.extend(["output tr".into(), "spec tr".into()]);
    if g.coin() {
        t.push_str("montecarlo mc\n  samples 10\n");
        lost.push("montecarlo mc".into());
    }
    Bench {
        text: canonical(&t),
        exact,
        lost,
    }
}

// ---------------------------------------------------------------------------------------
// (a) Rust round trip
// ---------------------------------------------------------------------------------------

#[test]
fn generated_benches_round_trip_through_maestro_records() {
    for seed in 0..32 {
        let with_param = seed % 8 == 7;
        let b = gen_bench(seed, with_param);
        let files = project(&b.text);
        let (text, notes) = export(&files);
        assert_eq!(text, export(&files).0, "seed {seed}: deterministic");
        let rec = maestro(&text);
        assert_eq!(rec[0].tag, "maestro");
        assert_eq!(
            rec[0].str(0).unwrap(),
            "sim",
            "seed {seed}: the test is named after its block"
        );
        // Unmapped parts are reported.
        let classes: BTreeSet<&str> = notes.rows.iter().map(|n| n.class.as_str()).collect();
        assert!(
            classes.contains("needs_review:output"),
            "seed {seed}: {}",
            notes.text()
        );
        assert!(
            classes.contains("needs_review:spec"),
            "seed {seed}: {}",
            notes.text()
        );
        if b.text.contains("montecarlo") {
            assert!(classes.contains("needs_review:montecarlo"));
        }
        if b.text.contains("sweep s2") {
            assert!(classes.contains("needs_review:sweep"));
        }
        if b.text.contains("code zz") {
            assert!(notes.text().contains("code zz spice"), "{}", notes.text());
        }
        // Without the stash: what §9 maps comes back exactly, the rest is gone.
        let man: Files = files
            .iter()
            .filter(|(p, _)| *p == "schemify.project")
            .map(|(p, v)| (p.clone(), v.clone()))
            .collect();
        let (got, _) = import(&strip_stash(&text), &man);
        let (want_t, got_t) = (tables(&b.text), tables(&got));
        for k in &b.exact {
            assert_eq!(
                got_t.get(k),
                want_t.get(k),
                "seed {seed}: {k}\n--- want\n{}\n--- got\n{got}",
                b.text
            );
        }
        for k in &b.lost {
            assert!(!got_t.contains_key(k), "seed {seed}: {k} came back\n{got}");
        }
        if with_param {
            assert_eq!(
                got_t.get("var k").map(String::as_str),
                Some("k 2"),
                "a .param comes back as a var"
            );
        }
        // The analysis block comes back as the same maestro records (idempotence).
        let (text2, _) = export(&project(&got));
        assert_eq!(maestro(&text2), rec, "seed {seed}: re-export\n{got}");
        // With the stash: byte for byte (benches without `.param`, a documented loss).
        let (exact, _) = import(&text, &man);
        if !with_param {
            assert_eq!(exact, b.text, "seed {seed}: with the stash");
        }
        // Onto the existing file: unchanged.
        let mut existing = man.clone();
        existing.insert("tb.shbench".into(), b.text.clone().into_bytes());
        let (onto, _) = import(&text, &existing);
        if !with_param {
            assert_eq!(onto, b.text, "seed {seed}: onto the existing file");
        }
    }
}

#[test]
fn records_follow_the_spec_9_table() {
    let bench = "schemify 1 shbench\n\nvar vdd 1.8\nvar W 1u\nsweep W\n  var W\n  lin 1k 3k 3\n\
                 corner ff\n  lib \"${PDK}/m.lib\" ff\n  include x.inc\n  set vdd 1.95\n  temp -40\n\
                 code a spice\n  | .lib ${PDK}/t.lib tt\n  | .temp 50\n  | .tran 1n 1u 0 2n\n  | .ac dec 10 1 1g\n\
                 \x20 | .dc V1 0 1.8 0.1\n  | .op\n  | .noise v(out) V1 lin 5 1k 1meg\n\
                 code defs spectre\n  | parameters p=1\n\
                 output o \"ymax(v(out))\"\nspec o\n  min 1\n  max 1meg\n";
    let (text, _) = export(&project(bench));
    let got: Vec<String> = maestro(&text)
        .iter()
        .map(|r| {
            let mut s = String::new();
            sfyx::write_record(r, &mut s);
            s
        })
        .collect();
    let want = [
        r#"("maestro" "a" "schemify" "tb" "schematic" "spectre")"#,
        r#"("var" "W" "1000,2000,3000" "")"#,
        r#"("var" "temperature" "50" "")"#,
        r#"("var" "vdd" "1.8" "")"#,
        r#"("analysis" "ac" 1 (("start" "1") ("stop" "1G") ("incrType" "Logarithmic") ("stepTypeLog" "Points Per Decade") ("dec" "10")))"#,
        r#"("analysis" "dc" 1 (("sweep" "Component Parameter") ("dev" "/V1") ("param" "dc") ("start" "0") ("stop" "1.8") ("step" "0.1") ("saveOppoint" "t")))"#,
        r#"("analysis" "noise" 1 (("start" "1k") ("stop" "1M") ("incrType" "Linear") ("stepTypeLin" "Number of Steps") ("lin" "5") ("oprobe" "/out") ("iprobe" "/V1")))"#,
        r#"("analysis" "tran" 1 (("stop" "1u") ("start" "0") ("maxstep" "2n")))"#,
        r#"("model" "" "$PDK/t.lib" "tt")"#,
        r#"("corner" "ff" 1)"#,
        r#"("model" "ff" "$PDK/m.lib" "ff")"#,
        r#"("model" "ff" "x.inc" "")"#,
        r#"("var" "temperature" "-40" "ff")"#,
        r#"("var" "vdd" "1.95" "ff")"#,
        r#"("output" "o" "point" "ymax(VT(\"/out\"))")"#,
        r#"("spec" "o" "gt" "1")"#,
        r#"("spec" "o" "lt" "1M")"#,
        r#"("defn" "defs.scs" "parameters p=1\n")"#,
    ];
    assert_eq!(got, want.map(|s| format!("{s}\n")));
}

// ---------------------------------------------------------------------------------------
// (b) Hand-written maestro dumps → golden benches
// ---------------------------------------------------------------------------------------

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ade")
}

#[test]
fn maestro_dumps_import_to_their_golden_benches() {
    for name in ["spice", "native"] {
        let text = std::fs::read_to_string(fixtures().join(format!("{name}.sfyx"))).unwrap();
        let (got, notes) = import(&text, &Files::new());
        let out = format!("{got}\n--- report\n{}", notes.text());
        let path = fixtures().join(format!("{name}.expected"));
        if std::env::var_os("SCHEMIFY_BLESS").is_some() {
            std::fs::write(&path, &out).unwrap();
        }
        let want = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{path:?}: bless"));
        assert_eq!(out, want, "{name} (re-bless with SCHEMIFY_BLESS=1)");
        // The imported bench loads.
        design(&project(&got));
    }
}

// ---------------------------------------------------------------------------------------
// (c) Through the SKILL loader and dumper on the mock maestro
// ---------------------------------------------------------------------------------------

fn interp() -> Interp<MockDb> {
    skill::virtuoso(&["schemify"])
}

fn run(i: &mut Interp<MockDb>, src: &str) -> V {
    match i.load(src) {
        Ok(v) => v,
        Err(e) => panic!("{src}: {e}\nlog: {}", i.log),
    }
}

fn s(v: &V) -> String {
    v.as_str()
        .unwrap_or_else(|| panic!("{v:?} is not a string"))
        .to_owned()
}

#[test]
fn skill_loader_and_dumper_round_trip_on_the_mock_maestro() {
    let b = gen_bench(5, false);
    let base: String = b
        .text
        .lines()
        .filter(|l| !l.starts_with("  | .include") && !l.starts_with("  | .lib"))
        .map(|l| format!("{l}\n"))
        .collect();
    let bench = canonical(&base.replace(
        "code sim spice\n",
        "code sim spice\n  | .include models/top.inc\n  | .lib ${PDK}/m.lib tt\n",
    ));
    let files = project(&bench);
    let (text, _) = export(&files);
    let mut i = interp();
    i.files.insert("/t/in.sfyx".into(), text.clone());
    let ok = run(&mut i, r#"(SfyImport "/t/in.sfyx")"#);
    let log = i.files["/t/in.sfyx.log"].clone();
    assert!(ok.truthy(), "{log}");
    assert!(log.contains("count maestro 1"), "{log}");

    // The mock maestro holds what §9.1 says.
    let su = i.host.mae.saved[&("schemify".to_owned(), "tb".to_owned())].clone();
    assert_eq!(su.tests.len(), 1);
    let t = &su.tests[0];
    assert_eq!(t.name, "sim");
    assert_eq!(
        t.design,
        ("schemify".into(), "tb".into(), "schematic".into())
    );
    assert_eq!(t.simulator, "spectre");
    let tran = t.analyses.iter().find(|a| a.0 == "tran").expect("tran");
    assert!(tran.1);
    assert_eq!(tran.2[0].0, "stop");
    let var = |n: &str| su.vars.iter().find(|v| v.0 == n).map(|v| v.1.clone());
    assert_eq!(var("vdd").as_deref(), Some("1.8"));
    // The test temperature is the simulator option temp, not a design variable (which
    // would set nothing); saveOppoint is the boolean t, not the string "t".
    assert!(
        t.sim
            .iter()
            .any(|o| o.0 == "temp" && o.1.as_str() == Some("50")),
        "{:?}",
        t.sim
    );
    assert!(var("temperature").is_none());
    let dc = t.analyses.iter().find(|a| a.0 == "dc").expect("dc");
    assert!(
        dc.2.iter()
            .any(|o| o.0 == "saveOppoint" && matches!(o.1, V::T)),
        "{:?}",
        dc.2
    );
    let ff = su.corners.iter().find(|c| c.name == "ff").expect("ff");
    assert!(ff.enabled);
    assert_eq!(
        ff.models,
        vec![
            ("models.lib".into(), "$PDK/models.lib".into(), "ff".into()),
            ("extra.inc".into(), "extra.inc".into(), String::new()),
        ]
    );
    assert!(ff.vars.contains(&("vdd".into(), "1.95".into())));
    assert!(ff.vars.contains(&("temperature".into(), "-40".into())));
    let vmax = t.outputs.iter().find(|o| o.name == "vmax").expect("vmax");
    assert_eq!(vmax.kind, "point");
    assert_eq!(vmax.expr.as_deref(), Some("ymax(VT(\"/out\"))"));
    assert_eq!(vmax.spec[0].0, "range");
    assert_eq!(
        vmax.spec[0].1.items().iter().map(s).collect::<Vec<_>>(),
        ["1", "1.7"]
    );
    let gain = t.outputs.iter().find(|o| o.name == "gain").expect("gain");
    assert_eq!(
        (gain.spec[0].0.as_str(), s(&gain.spec[0].1)),
        ("lt", "2".into())
    );
    let env = |n: &str| {
        t.env
            .iter()
            .find(|e| e.0 == n)
            .map(|e| e.1.clone())
            .unwrap_or(V::Nil)
    };
    let mf: Vec<Vec<String>> = env("modelFiles")
        .items()
        .iter()
        .map(|e| e.items().iter().map(s).collect())
        .collect();
    assert_eq!(
        mf,
        vec![
            vec!["models/top.inc".to_owned(), String::new()],
            vec!["$PDK/m.lib".into(), "tt".into()]
        ]
    );
    let defs: Vec<String> = env("definitionFiles").items().iter().map(s).collect();
    assert_eq!(defs, ["/mock/schemify/tb/maestro/models.scs"]);
    assert_eq!(
        i.files[&defs[0]],
        "simulator lang=spectre\ninclude \"x.scs\"\n"
    );
    assert!(
        i.host.mae.sessions.iter().all(Option::is_none),
        "every session closed"
    );

    // Dumped back: the same maestro records, and the bench comes back.
    run(
        &mut i,
        r#"(SfyExport "/t/out.sfyx" "schemify" (list "tb"))"#,
    );
    let dumped = i.files["/t/out.sfyx"].clone();
    assert!(
        i.files["/t/out.sfyx.log"].contains("PASS"),
        "{}",
        i.files["/t/out.sfyx.log"]
    );
    // IC23.1 (and the mock) echo form fields nothing set: anaName on every analysis, the
    // ac sweep chooser. The dump carries them; the import drops them (§9.2).
    let echoed = |r: &Record| {
        let Ok(n) = r.str(0) else { return r.clone() };
        if r.tag != "analysis" {
            return r.clone();
        }
        let mut f = r.fields.clone();
        if let Value::List(opts) = &mut f[2] {
            assert_eq!(
                opts.first().map(|o| o.items().to_vec()),
                Some(vec![Value::Str("anaName".into()), Value::Str(n.into())]),
                "{n}: anaName echoed first"
            );
            opts.retain(|o| match o.items() {
                [Value::Str(k), Value::Str(v)] => {
                    !((k == "anaName" && v == n) || (k == "sweep" && v == "Frequency"))
                }
                _ => true,
            });
        }
        Record::new(&r.tag, f)
    };
    assert_eq!(
        maestro(&dumped).iter().map(echoed).collect::<Vec<_>>(),
        maestro(&text),
        "SKILL dump vs Rust export"
    );
    let man: Files = files
        .iter()
        .filter(|(p, _)| *p == "schemify.project")
        .map(|(p, v)| (p.clone(), v.clone()))
        .collect();
    let (got, _) = import(&dumped, &man);
    assert_eq!(got, bench, "with the stash: exact");
    let (bare, _) = import(&strip_stash(&dumped), &man);
    let (want_t, got_t) = (tables(&bench), tables(&bare));
    for k in &b.exact {
        assert_eq!(got_t.get(k), want_t.get(k), "{k} without the stash");
    }

    // A second load is refused without ?overwrite and replaces the test with it.
    let again = run(&mut i, r#"(SfyImport "/t/in.sfyx")"#);
    assert!(!again.truthy());
    assert!(i.files["/t/in.sfyx.log"].contains("maestro exists; pass ?overwrite t"));
    let over = run(&mut i, r#"(SfyImport "/t/in.sfyx" ?overwrite t)"#);
    assert!(over.truthy(), "{}", i.files["/t/in.sfyx.log"]);
    assert_eq!(
        i.host.mae.saved[&("schemify".to_owned(), "tb".to_owned())]
            .tests
            .len(),
        1
    );
    assert!(i.host.mae.calls.iter().any(|c| c == "maeDeleteTest"));
}

/// A session that gives every new test default model files (a PDK's libInit, live
/// run): a bench without models loads with none, and one with models with exactly its own.
#[test]
fn the_exported_model_list_replaces_the_session_defaults() {
    let defaults = || {
        vec![(
            "modelFiles".to_owned(),
            V::list(vec![V::list(vec![
                V::str("/pdk/models.lib.scs"),
                V::str("tt"),
            ])]),
        )]
    };
    let models_of = |text: &str| -> Vec<String> {
        sfyx::read(text)
            .unwrap()
            .into_iter()
            .filter(|r| r.tag == "model" && r.str(0) == Ok(""))
            .map(|r| r.str(1).unwrap().to_owned())
            .collect()
    };
    let base: String = gen_bench(5, false)
        .text
        .lines()
        .filter(|l| !l.starts_with("  | .include") && !l.starts_with("  | .lib"))
        .map(|l| format!("{l}\n"))
        .collect();
    for (bench, want) in [
        (canonical(&base), vec![]),
        (
            canonical(&base.replace(
                "code sim spice\n",
                "code sim spice\n  | .lib ${PDK}/m.lib tt\n",
            )),
            vec!["$PDK/m.lib".to_owned()],
        ),
    ] {
        let (text, _) = export(&project(&bench));
        let mut i = interp();
        i.host.mae.default_env = defaults();
        i.files.insert("/t/in.sfyx".into(), text);
        let ok = run(&mut i, r#"(SfyImport "/t/in.sfyx")"#);
        let log = i.files["/t/in.sfyx.log"].clone();
        assert!(ok.truthy(), "{log}");
        run(
            &mut i,
            r#"(SfyExport "/t/out.sfyx" "schemify" (list "tb"))"#,
        );
        assert_eq!(models_of(&i.files["/t/out.sfyx"]), want, "{log}");
    }
}

#[test]
fn test_variables_override_globals_and_one_view_takes_several_tests() {
    let (text, _) = export(&project(&canonical(&gen_bench(5, false).text)));
    // The same test twice under two names, in one maestro view.
    let k = text.find("(\"maestro\" \"sim\"").expect("a maestro test");
    let end = text.rfind("(\"end\")").expect("end");
    let second = text[k..end].replacen("(\"maestro\" \"sim\"", "(\"maestro\" \"sim2\"", 1);
    let two = format!("{}{second}(\"end\")\n", &text[..end]);
    let mut i = interp();
    i.files.insert("/t/two.sfyx".into(), two);
    let ok = run(&mut i, r#"(SfyImport "/t/two.sfyx")"#);
    let log = i.files["/t/two.sfyx.log"].clone();
    assert!(ok.truthy() && log.contains("count maestro 2"), "{log}");
    // A test-level vdd in sim (set in the GUI, say) overrides the global 1.8 there only.
    let key = ("schemify".to_owned(), "tb".to_owned());
    let su = i.host.mae.saved.get_mut(&key).expect("saved");
    let sim = su.tests.iter_mut().find(|t| t.name == "sim").expect("sim");
    sim.vars.push(("vdd".into(), "2.5".into()));
    run(
        &mut i,
        r#"(SfyExport "/t/out.sfyx" "schemify" (list "tb"))"#,
    );
    let mut test = String::new();
    let mut vdd = Vec::new();
    for r in sfyx::read(&i.files["/t/out.sfyx"]).unwrap() {
        match r.tag.as_str() {
            "maestro" => test = r.str(0).unwrap().to_owned(),
            "var" if r.str(0) == Ok("vdd") && r.str(2) == Ok("") => {
                vdd.push((test.clone(), r.str(1).unwrap().to_owned()));
            }
            _ => {}
        }
    }
    assert_eq!(
        vdd,
        [
            ("sim".to_owned(), "2.5".to_owned()),
            ("sim2".into(), "1.8".into())
        ]
    );
}

#[test]
fn an_unnamed_maestro_output_is_skipped_not_fatal() {
    let (text, _) = export(&project(&canonical(&gen_bench(5, false).text)));
    let mut i = interp();
    i.files.insert("/t/in.sfyx".into(), text);
    assert!(run(&mut i, r#"(SfyImport "/t/in.sfyx")"#).truthy());
    // ADE allows a saved or plotted signal without a name.
    let key = ("schemify".to_owned(), "tb".to_owned());
    let su = i.host.mae.saved.get_mut(&key).expect("saved");
    su.tests[0].outputs.push(skill::mockdb::mae::Output {
        name: String::new(),
        kind: "net".into(),
        signal: Some("/out".into()),
        ..Default::default()
    });
    let ok = run(
        &mut i,
        r#"(SfyExport "/t/out.sfyx" "schemify" (list "tb"))"#,
    );
    let log = i.files["/t/out.sfyx.log"].clone();
    assert!(ok.truthy(), "{log}");
    assert!(
        log.contains("skipped: maestro sim output without a name"),
        "{log}"
    );
    // The dump still imports.
    let mut man = Files::new();
    man.insert("schemify.project".into(), b"schemify 1 project\n".to_vec());
    import(&i.files["/t/out.sfyx"].clone(), &man);
}

#[test]
fn a_maestro_skill_error_is_logged_and_the_session_released() {
    let (text, _) = export(&project(&canonical(&gen_bench(5, false).text)));
    let mut i = interp();
    i.load("(procedure (SfyAdeFill test tlib cell sess) (error \"boom\"))")
        .unwrap();
    i.files.insert("/t/in.sfyx".into(), text);
    let ok = run(&mut i, r#"(SfyImport "/t/in.sfyx")"#);
    let log = i.files["/t/in.sfyx.log"].clone();
    assert!(!ok.truthy() && log.ends_with("FAIL\n"), "{log}");
    // The message survives the close (a completing errset clears errset.errset).
    assert!(
        log.contains("maestro sim: SKILL error") && log.contains("boom"),
        "{log}"
    );
    // The schematic before it landed; the session is closed (no maestro lock left).
    assert!(log.contains("count saved schematic"), "{log}");
    assert!(i.host.mae.sessions.iter().all(Option::is_none));
}

#[test]
fn colon_sweeps_and_echoed_form_fields_import() {
    let text = format!(
        "{HEAD}(\"var\" \"VDD\" \"0.81:0.09:0.99\" \"\")\n\
         (\"analysis\" \"ac\" 1 ((\"anaName\" \"ac\") (\"sweep\" \"Frequency\") (\"start\" \"1\") (\"stop\" \"1G\") (\"incrType\" \"Logarithmic\") (\"stepTypeLog\" \"Points Per Decade\") (\"dec\" \"10\")))\n\
         (\"analysis\" \"tran\" 1 ((\"anaName\" \"tran\") (\"stop\" \"1n\") (\"incrType\" \"Linear\") (\"stepTypeLin\" \"Step Size\")))\n(\"end\")\n"
    );
    let (got, _) = import(&text, &Files::new());
    // ADE's start:step:stop is a lin sweep of its points.
    assert!(got.contains("var VDD 0.81\n"), "{got}");
    assert!(
        got.contains("sweep VDD\n  var VDD\n  lin 0.81 0.99 3\n"),
        "{got}"
    );
    // tran by step size has no SPICE form, so the block stays Spectre: its lines carry
    // neither the echoed anaName nor a form-only field.
    assert!(got.contains("  | ac ac start=1 stop=1G dec=10\n"), "{got}");
    assert!(got.contains("  | tran tran stop=1n\n"), "{got}");
    // Alone, the echoed ac still maps to SPICE.
    let ac_only: String = text
        .lines()
        .filter(|l| !l.contains("\"tran\""))
        .map(|l| format!("{l}\n"))
        .collect();
    let (got, _) = import(&ac_only, &Files::new());
    assert!(got.contains(".ac dec 10 1 1g"), "{got}");
    // A colon form that does not land on stop is no sweep.
    let bad = format!("{HEAD}(\"var\" \"VDD\" \"0.8:0.07:1\" \"\")\n(\"end\")\n");
    let (got, notes) = import(&bad, &Files::new());
    assert!(!got.contains("sweep VDD"), "{got}");
    assert!(notes.text().contains("0.8:0.07:1"), "{}", notes.text());
}

// ---------------------------------------------------------------------------------------
// (d) Negative space
// ---------------------------------------------------------------------------------------

const HEAD: &str = "(\"sfyx\" 1 \"t\")\n(\"lib\" \"schemify\")\n(\"cell\" \"tb\" \"design\")\n(\"cv\" \"schematic\" \"schematic\" 160)\n(\"maestro\" \"t\" \"schemify\" \"tb\" \"schematic\" \"spectre\")\n";

#[test]
fn malformed_maestro_records_are_refused_with_their_line() {
    for (rec, why) in [
        (r#"("analysis" "tran" 2 ())"#, "enable"),
        (r#"("analysis" "tran" 1 (("stop")))"#, "option"),
        (r#"("analysis" "tran" 1 (("stop" 1)))"#, "option"),
        (r#"("var" "a" "1" "nocorner")"#, "no such corner"),
        (r#"("model" "nocorner" "f" "")"#, "no such corner"),
        (r#"("output" "o" "wave" "x")"#, "kind"),
        (r#"("spec" "o" "gt" "1")"#, "not declared"),
        (r#"("var" "a" "1")"#, "fields"),
        (r#"("corner" "c" "1")"#, "integer"),
        (r#"("defn" "../x.scs" "t")"#, "path"),
    ] {
        let text = format!("{HEAD}{rec}\n(\"end\")\n");
        let e = virtuoso::import(
            &text,
            &Files::new(),
            &Mapping::default(),
            &ImportOptions::default(),
        )
        .err()
        .unwrap_or_else(|| panic!("{rec} must be refused"));
        let msg = e.join("; ");
        assert!(msg.contains("line 6"), "{rec}: {msg}");
        assert!(msg.contains(why), "{rec}: {msg}");
    }
    let dup = format!("{HEAD}(\"corner\" \"c\" 1)\n(\"corner\" \"c\" 1)\n(\"end\")\n");
    let e = virtuoso::import(
        &dup,
        &Files::new(),
        &Mapping::default(),
        &ImportOptions::default(),
    )
    .unwrap_err();
    assert!(e.join(";").contains("line 7"), "{e:?}");
}

#[test]
fn unknown_analyses_are_reported_and_kept_as_spectre() {
    let text = format!(
        "{HEAD}(\"analysis\" \"pss\" 1 ((\"fund\" \"1G\") (\"harms\" \"10\")))\n(\"analysis\" \"tran\" 1 ((\"stop\" \"1u\")))\n(\"end\")\n"
    );
    let (got, notes) = import(&text, &Files::new());
    assert!(
        notes.text().contains("analysis pss has no SPICE form"),
        "{}",
        notes.text()
    );
    assert!(
        got.contains("code t spectre\n  | pss pss fund=1G harms=10\n  | tran tran stop=1u\n"),
        "{got}"
    );
    // And a Spectre block exports back to the same analyses.
    let (again, _) = export(&project(&got));
    let a: Vec<Record> = maestro(&again)
        .into_iter()
        .filter(|r| r.tag == "analysis")
        .collect();
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].str(0).unwrap(), "pss");
    // The native golden's block (with output comments) exports as analyses, not a defn.
    let native = std::fs::read_to_string(fixtures().join("native.sfyx")).unwrap();
    let (bench, _) = import(&native, &Files::new());
    let rec = maestro(&export(&project(&bench)).0);
    assert_eq!(rec[0].str(0).unwrap(), "AC");
    assert_eq!(rec.iter().filter(|r| r.tag == "analysis").count(), 2);
    assert_eq!(
        rec.iter().filter(|r| r.tag == "defn").count(),
        1,
        "only stimuli.scs"
    );
}

#[test]
fn skill_loader_refuses_bad_options_with_their_line() {
    let mut i = interp();
    let bad = "(\"sfyx\" 1 \"t\")\n(\"lib\" \"schemify\")\n(\"cell\" \"tb\" \"design\")\n(\"cv\" \"schematic\" \"schematic\" 160)\n(\"maestro\" \"t\" \"schemify\" \"tb\" \"schematic\" \"spectre\")\n(\"analysis\" \"tran\" 1 ((\"stop\")))\n(\"end\")\n";
    i.files.insert("/t/bad.sfyx".into(), bad.into());
    let ok = run(&mut i, r#"(SfyImport "/t/bad.sfyx")"#);
    let log = &i.files["/t/bad.sfyx.log"];
    assert!(!ok.truthy());
    assert!(log.contains("refused line 6: maestro t: analysis"), "{log}");
}

/// Exercise 2 forms (§9.1): one test holding the op table and the ac outputs (`analysis`),
/// a plot (`vs`), spec bounds over bench vars folded, a bound over a swept var reported,
/// `.options errpreset` on the tran analysis, `slope`-style column conditions reported.
#[test]
fn op_and_ac_outputs_share_one_test() {
    let bench = "schemify 1 shbench\n\nvar A0_spec 40\nvar GBW_spec 50meg\nvar R 1k\n\
                 sweep R\n  var R\n  values 1k 2k\n\
                 code sim spice\n  | .options errpreset=conservative\n  | .op\n  | .ac dec 50 1 10G\n  | .tran 10p 200n\n\
                 output A0 at(mag(v(out)),10)\n  analysis ac\n\
                 output Vout v(out)\n  analysis op\n\
                 output gm \"op(\\\"M1\\\",\\\"gm\\\")\"\n\
                 output gmid \"op(\\\"M1\\\",\\\"gm\\\")/op(\\\"M1\\\",\\\"id\\\")\"\n  vs Vout\n\
                 output slope ymin(if(db(v(out))>=0,deriv(db(v(out))),0))\n\
                 output bad v(out)\n  analysis noise\n\
                 spec A0\n  min A0_spec/(1+A0_spec)\n  max GBW_spec\n\
                 spec Vout\n  min R/1k\n";
    let (text, notes) = export(&project(bench));
    let got: Vec<String> = maestro(&text)
        .iter()
        .map(|r| {
            let mut s = String::new();
            sfyx::write_record(r, &mut s);
            s.trim_end().to_owned()
        })
        .collect();
    for want in [
        r#"("analysis" "tran" 1 (("stop" "200n") ("errpreset" "conservative")))"#,
        r#"("output" "A0" "point" "value(mag(VF(\"/out\")) 10)")"#,
        r#"("output" "Vout" "point" "VDC(\"/out\")")"#,
        r#"("output" "gm" "point" "OP(\"/M1\" \"gm\")")"#,
        r#"("output" "gmid" "point" "OP(\"/M1\" \"gm\")/OP(\"/M1\" \"id\")")"#,
        r#"("spec" "A0" "gt" "0.975609756097561")"#,
        r#"("spec" "A0" "lt" "50000000")"#,
    ] {
        assert!(got.iter().any(|l| l == want), "{want}\n{got:#?}");
    }
    let rows = notes.text();
    for want in [
        "output slope: `ymin(if(",
        "output bad: analysis `noise` is not one the test runs",
        "spec Vout min `R/1k`: depends on swept var R",
        "output gmid vs Vout: plotted by the OCEAN script only",
    ] {
        assert!(rows.contains(want), "{want}\n{rows}");
    }
    assert!(!rows.contains("errpreset"), "{rows}");
    assert!(
        !got.iter().any(|l| l.starts_with(r#"("spec" "Vout""#)),
        "{got:#?}"
    );
}
