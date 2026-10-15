//! The shipped SKILL (`interop::skill`) through the test interpreter and mock database
//! (spec `interop.md` §4, §5).
//!
//! Fixtures are mock libraries built by `tests/fixtures/skill/build.il` through the same
//! Cadence API the mock implements; dumps are compared with committed goldens in
//! `tests/fixtures/skill/*.sfyx` (re-bless with `SCHEMIFY_BLESS=1`).

mod skill;

use interop::sfyx;
use proptest::prelude::*;
use skill::mockdb::MockDb;
use skill::{Interp, V, read_all};

const BUILD: &str = include_str!("fixtures/skill/build.il");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/skill");

/// A Verilog-A module longer than one 4000-byte chunk, with tabs and a UTF-8 character.
fn va_text() -> String {
    let mut s =
        String::from("// \u{3bc}-model\n`include \"disciplines.vams\"\nmodule vamod(p, n);\n");
    for k in 0..300 {
        s.push_str(&format!(
            "\tparameter real r{k} = {k}.0; // r\u{e9}sistance\n"
        ));
    }
    s.push_str("endmodule\n");
    s
}

/// An interpreter with schemify.il loaded and the given libraries in cds.lib.
fn interp(libs: &[&str]) -> Interp<MockDb> {
    let mut i = Interp::new(MockDb::default());
    for l in libs {
        i.host.add_lib(l);
    }
    i.load(&interop::skill::schemify_il())
        .expect("schemify.il loads");
    i
}

/// An interpreter with every fixture library built, the text views included.
fn fixtures() -> Interp<MockDb> {
    let mut i = interp(&["analogLib", "basic", "pdk", "cb", "work", "scratch"]);
    i.load(BUILD).expect("build.il loads");
    i.load("(SfyFixAll)").expect("fixtures build");
    let va = i
        .host
        .add_text_file("work", "vamod", "veriloga", "veriloga.va");
    i.files.insert(va, va_text());
    let v = i
        .host
        .add_text_file("work", "vamod", "functional", "verilog.v");
    i.files
        .insert(v, "module vamod(inout p, inout n);\nendmodule\n".into());
    i
}

fn run(i: &mut Interp<MockDb>, src: &str) -> V {
    match i.load(src) {
        Ok(v) => v,
        Err(e) => panic!("{src}: {e}\nlog: {}", i.log),
    }
}

fn file(i: &Interp<MockDb>, path: &str) -> String {
    i.files
        .get(path)
        .cloned()
        .unwrap_or_else(|| panic!("no file {path}"))
}

/// Checks `text` is canonical `.sfyx` (Rust reads it, writes it back byte for byte) and
/// equals the golden `name` (blessed with `SCHEMIFY_BLESS=1`).
fn golden(name: &str, text: &str) {
    let recs = sfyx::read(text).unwrap_or_else(|e| panic!("{name}: {e}\n{text}"));
    sfyx::require_complete(&recs).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(
        sfyx::write(&recs),
        text,
        "{name}: the dump is not canonical .sfyx"
    );
    let path = format!("{FIXTURES}/{name}");
    if std::env::var_os("SCHEMIFY_BLESS").is_some() {
        std::fs::write(&path, text).expect("bless");
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{path}: run with SCHEMIFY_BLESS=1"));
    assert_eq!(text, want, "{name} differs from its golden");
}

#[test]
fn every_shipped_line_reads_in_the_prefix_subset() {
    for (name, text) in interop::skill::PARTS {
        if let Err(e) = read_all(text) {
            panic!("{name}: {e}");
        }
    }
    read_all(interop::skill::PROBE).expect("sfy_probe.il reads");
    read_all(&interop::skill::schemify_il()).expect("schemify.il reads");
}

#[test]
fn the_reader_refuses_what_must_not_ship() {
    for bad in [
        "(f(a))",
        "(setq a b+c)",
        "(x~>y)",
        "(setq a 1u)",
        "(setq a 007)",
        "(list 1.)",
    ] {
        assert!(read_all(bad).is_err(), "{bad} must not read");
    }
}

#[test]
fn interpreter_core_semantics() {
    let mut i = Interp::new(MockDb::default());
    let v = i
        .load(
            r#"
(procedure (SfyT x @key (k 2))
  (let ((acc nil))
    (foreach e (list 1 2 3) (setq acc (cons (times e k) acc)))
    (list (reverse acc) (substring "abcdef" 2 4) (case x ("a" 1) (t 0)))))
(SfyT "a" ?k 3)
"#,
        )
        .unwrap();
    let mut s = String::new();
    skill::print(&v, &mut s);
    assert_eq!(s, "((3 6 9) \"bcde\" 1)");
    let p =
        i.load(r#"(let ((p (outfile "/t/x" "w"))) (fprintf p "(%s %d)\n" "\"a\"" -5) (close p))"#);
    assert!(p.is_ok());
    assert_eq!(i.files["/t/x"], "(\"a\" -5)\n");
    let r = i
        .load(r#"(let ((p (infile "/t/x"))) (list (lineread p) (lineread p)))"#)
        .unwrap();
    let mut s = String::new();
    skill::print(&r, &mut s);
    assert_eq!(s, "(((\"a\" -5)) nil)");
    assert!(matches!(
        i.load("(errset (error \"boom\"))").unwrap(),
        V::Nil
    ));
    // %g follows C, as SKILL's printf does.
    let g = i
        .load(r#"(list (sprintf nil "%.17g" 0.1) (sprintf nil "%.17g" 1.5) (sprintf nil "%g" 1e-06) (sprintf nil "%.17g" 1e22))"#)
        .unwrap();
    let mut s = String::new();
    skill::print(&g, &mut s);
    assert_eq!(s, "(\"0.10000000000000001\" \"1.5\" \"1e-06\" \"1e+22\")");
}

// ---------------------------------------------------------------------- escaper

fn skill_write_str(i: &mut Interp<MockDb>, s: &str) -> String {
    i.set_global("sfyTestStr", V::str(s));
    run(
        i,
        r#"(let ((p (outfile "/t/esc" "w"))) (SfyPutStr p sfyTestStr) (close p))"#,
    );
    file(i, "/t/esc")
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, .. ProptestConfig::default() })]

    /// The SKILL escaper writes what `sfyx::write_str` writes, and SKILL's `lineread` of
    /// that line gives the string back (spec §4.1, §4.3). SKILL strings cannot hold NUL.
    #[test]
    fn skill_escaper_matches_rust(s in "[^\u{0}]{0,64}") {
        let mut i = interp(&[]);
        let got = skill_write_str(&mut i, &s);
        let mut want = String::new();
        sfyx::write_str(&s, &mut want);
        prop_assert_eq!(&got, &want);
        i.files.insert("/t/line".into(), format!("({got})\n"));
        let back = run(&mut i, r#"(let ((p (infile "/t/line")) (r nil)) (setq r (car (car (lineread p)))) (close p) r)"#);
        prop_assert_eq!(back.as_str(), Some(s.as_str()));
    }

    /// Text fields chunk exactly as `sfyx::text`, whatever the segments (lines) are.
    #[test]
    fn skill_text_chunks_match_rust(parts in proptest::collection::vec("[a\u{e9}\u{3bc}\u{1f600}\n\t\"]{0,400}", 0..30)) {
        let s: String = parts.concat();
        let mut i = interp(&[]);
        i.set_global("sfyTestSegs", V::list(parts.iter().map(|p| V::str(p)).collect()));
        run(&mut i, r#"(let ((p (outfile "/t/text" "w")) (sfyCountTab (makeTable "c" nil)) (sfyCountKeys nil))
                         (SfyPut p "x" (list (SfyText sfyTestSegs))) (close p))"#);
        let want = sfyx::write(&[sfyx::Record::new("x", vec![sfyx::text(&s)])]);
        prop_assert_eq!(file(&i, "/t/text"), want);
    }
}

#[test]
fn escaper_covers_every_control_byte() {
    let mut i = interp(&[]);
    let all: String = (1u8..=127)
        .map(char::from)
        .chain("\u{e9}\u{10ffff}".chars())
        .collect();
    let got = skill_write_str(&mut i, &all);
    let mut want = String::new();
    sfyx::write_str(&all, &mut want);
    assert_eq!(got, want);
    assert!(got.contains("\\001") && got.contains("\\177") && got.contains("\\n"));
}

// ---------------------------------------------------------------------- export

/// (golden name, export call) for every fixture dump.
const DUMPS: &[(&str, &str)] = &[
    (
        "inv.sfyx",
        r#"(SfyExport "/t/inv.sfyx" "work" (list "inv"))"#,
    ),
    (
        "bus.sfyx",
        r#"(SfyExport "/t/bus.sfyx" "work" (list "bus"))"#,
    ),
    (
        "symkit.sfyx",
        r#"(SfyExport "/t/symkit.sfyx" "work" (list "symkit"))"#,
    ),
    (
        "netx.sfyx",
        r#"(SfyExport "/t/netx.sfyx" "work" (list "netx"))"#,
    ),
    (
        "vamod.sfyx",
        r#"(SfyExport "/t/vamod.sfyx" "work" (list "vamod"))"#,
    ),
    ("pdk.sfyx", r#"(SfyExportLib "/t/pdk.sfyx" "pdk")"#),
];

#[test]
fn exports_are_canonical_and_match_their_goldens() {
    let mut i = fixtures();
    for (name, call) in DUMPS {
        let ok = run(&mut i, call);
        let path = format!("/t/{name}");
        let log = file(&i, &format!("{path}.log"));
        assert!(ok.truthy() && log.ends_with("PASS\n"), "{name}: {log}");
        golden(name, &file(&i, &path));
    }
}

#[test]
fn export_dumps_the_expected_objects() {
    let mut i = fixtures();
    run(&mut i, DUMPS[0].1);
    let t = file(&i, "/t/inv.sfyx");
    // ref cells first (analogLib, every master the schematic uses, pins excluded), then work.
    let cells: Vec<&str> = t.lines().filter(|l| l.starts_with("(\"cell\"")).collect();
    assert_eq!(
        cells,
        [
            "(\"cell\" \"gnd\" \"ref\")",
            "(\"cell\" \"nmos4\" \"ref\")",
            "(\"cell\" \"pmos4\" \"ref\")",
            "(\"cell\" \"vdd\" \"ref\")",
            "(\"cell\" \"inv\" \"design\")"
        ]
    );
    assert!(
        t.contains(
            "(\"prop\" 0 \"schemifyRecords1\" \"string\" \"prose \\\"inverter\\\"\\nvar k 2\")"
        ),
        "{t}"
    );
    assert!(
        !t.contains("author") && !t.contains("lastSchematicExtraction") && !t.contains("instance#")
    );
    assert!(
        t.contains("\"spin\" ")
            && t.contains("\"in\" \"input\" \"basic\" \"ipin\" \"symbol\" -80000 0 \"R0\""),
        "{t}"
    );
    assert!(t.contains("\"w\" \"string\" \"2u\"") && t.contains("\"l\" \"string\" \"200n\""));
    let log = file(&i, "/t/inv.sfyx.log");
    assert!(
        log.contains("count inst ") && log.contains("count wire 8"),
        "{log}"
    );
    // A symbol with two pins on one terminal, every shape and label kind.
    run(&mut i, DUMPS[2].1);
    let t = file(&i, "/t/symkit.sfyx");
    assert_eq!(t.matches("(\"pinfig\"").count(), 3);
    assert!(
        t.contains("\"arc\" \"device\" \"drawing\" (-10000 -30000 10000 -10000 0 1570796) 0"),
        "{t}"
    );
    assert!(t.contains("\"path\" \"device\" \"drawing1\" (30000 0 40000 0) 5000"));
    assert!(t.contains("\"a\\\"b\\\\c\\td\\001 \u{3bc}A\""), "{t}");
    for kind in ["\"normal\"", "\"NLP\"", "\"IL\""] {
        assert!(t.contains(kind), "{kind}");
    }
    // A long text view is chunked.
    run(&mut i, DUMPS[4].1);
    let recs = sfyx::read(&file(&i, "/t/vamod.sfyx")).unwrap();
    let tv: Vec<&sfyx::Record> = recs.iter().filter(|r| r.tag == "textview").collect();
    assert_eq!(tv.len(), 2);
    assert_eq!(tv[1].str(0).unwrap(), "veriloga");
    assert!(matches!(tv[1].fields[2], sfyx::Value::List(_)));
    assert_eq!(tv[1].text(2).unwrap(), va_text());
}

#[test]
fn export_lib_skips_cells_without_a_symbol_and_keeps_every_cdf_type() {
    let mut i = fixtures();
    run(&mut i, DUMPS[5].1);
    let t = file(&i, "/t/pdk.sfyx");
    assert!(!t.contains("padonly"));
    for p in [
        "(\"param\" \"model\" \"string\" \"nch_mac\" \"Model\" \"\" (\"storeDefault\" \"hidden\") () \"\")",
        "(\"param\" \"nf\" \"int\" \"1\" \"Fingers\" \"\" () () \"\")",
        "(\"param\" \"mult\" \"float\" \"1.5\" \"Multiplier\" \"\" () () \"\")",
        "(\"param\" \"bodytie\" \"boolean\" \"nil\" \"Body tie\" \"\" () () \"\")",
        "(\"param\" \"layout\" \"cyclic\" \"a\" \"Layout\" \"\" () (\"a\" \"b\") \"\")",
        "(\"param\" \"region\" \"radio\" \"on\" \"Region\" \"\" (\"dontSave\") (\"on\" \"off\") \"\")",
        "(\"param\" \"refresh\" \"button\" \"\" \"Refresh\" \"\" () () \"\")",
        "(\"param\" \"sub\" \"netSet\" \"gnd!\" \"Substrate\" \"\" (\"storeDefault\") () \"\")",
    ] {
        assert!(t.contains(p), "{p}\n{t}");
    }
    assert!(
        t.contains("(\"siminfo\" \"auCdl\" ((\"componentName\" \"nmos\") (\"dollarParams\" ())"),
        "{t}"
    );
    assert!(
        t.contains(
            "(\"termMapping\" ((\"D\" \":d\") (\"B\" \"(FUNCTION minus(root(\\\"D\\\")))\")))"
        ),
        "{t}"
    );
}

// ---------------------------------------------------------------------- import

#[test]
fn imported_goldens_dump_back_identically() {
    for (name, _) in &DUMPS[..5] {
        let golden_text = std::fs::read_to_string(format!("{FIXTURES}/{name}")).expect("golden");
        let mut i = interp(&["analogLib", "basic", "pdk", "cb", "scratch"]);
        i.load(BUILD).unwrap();
        run(&mut i, "(SfyFixStdlib)");
        i.files.insert("/t/in.sfyx".into(), golden_text.clone());
        let ok = run(&mut i, r#"(SfyImport "/t/in.sfyx" ?lib "scratch")"#);
        let log = file(&i, "/t/in.sfyx.log");
        assert!(ok.truthy() && log.ends_with("PASS\n"), "{name}: {log}");
        let cell = name.trim_end_matches(".sfyx");
        run(
            &mut i,
            &format!(r#"(SfyExport "/t/out.sfyx" "scratch" (list "{cell}"))"#),
        );
        let back = file(&i, "/t/out.sfyx");
        assert_eq!(
            back,
            golden_text.replace("\"work\"", "\"scratch\""),
            "{name} round trip"
        );
    }
}

#[test]
fn import_log_lists_counts_and_pass() {
    let mut i = fixtures();
    run(&mut i, DUMPS[0].1);
    run(&mut i, r#"(SfyImport "/t/inv.sfyx" ?lib "scratch")"#);
    let log = file(&i, "/t/inv.sfyx.log");
    for line in [
        "count inst 4\n",
        "count spin 2\n",
        "count wire 8\n",
        "count label 8\n",
        "count param 2\n",
        "count skipped:ref cell 4\n",
        "check scratch/inv/schematic: 0 errors 0 warnings\n",
        "PASS\n",
    ] {
        assert!(log.contains(line), "{line:?} missing from\n{log}");
    }
    let cv = i
        .host
        .cellview("scratch", "inv", "schematic")
        .expect("saved");
    assert!(i.host.is_saved(cv));
    // Without ?overwrite an existing view is refused, and nothing else is touched.
    let ok = run(&mut i, r#"(SfyImport "/t/inv.sfyx" ?lib "scratch")"#);
    let log = file(&i, "/t/inv.sfyx.log");
    assert!(
        !ok.truthy() && log.contains("exists; pass ?overwrite t") && log.ends_with("FAIL\n"),
        "{log}"
    );
    assert!(
        run(
            &mut i,
            r#"(SfyImport "/t/inv.sfyx" ?lib "scratch" ?overwrite t)"#
        )
        .truthy()
    );
}

#[test]
fn selftest_passes_on_the_mock() {
    for cell in ["inv", "bus", "symkit", "netx", "vamod"] {
        let mut i = fixtures();
        let ok = run(
            &mut i,
            &format!(r#"(SfySelfTest "work" "{cell}" "scratch")"#),
        );
        assert!(
            ok.truthy() && i.log.ends_with("SfySelfTest: PASS\n"),
            "{cell}: {}",
            i.log
        );
    }
}

/// A design cell instantiating cb/dev with parameters a, b, c (file order) and n.
fn cb_dump(n: &str) -> String {
    format!(
        "(\"sfyx\" 1 \"test\")\n(\"lib\" \"cb\")\n(\"cell\" \"top\" \"design\")\n(\"cv\" \"schematic\" \"schematic\" 160)\n\
         (\"inst\" 1 \"I0\" \"cb\" \"dev\" \"symbol\" 0 0 \"R0\")\n\
         (\"prop\" 1 \"a\" \"string\" \"1\")\n(\"prop\" 1 \"b\" \"string\" \"3\")\n(\"prop\" 1 \"c\" \"string\" \"2\")\n\
         (\"prop\" 1 \"d\" \"string\" \"7\")\n(\"prop\" 1 \"n\" \"int\" \"{n}\")\n(\"prop\" 1 \"x\" \"string\" \"free\")\n(\"end\")\n"
    )
}

#[test]
fn callbacks_run_in_cdf_order_and_failures_are_logged_not_fatal() {
    let mut i = fixtures();
    i.set_global("sfyCbOrder", V::Nil);
    i.files.insert("/t/cb.sfyx".into(), cb_dump("0"));
    let open_before = run(&mut i, "(length (dbGetOpenCellViews))");
    let ok = run(&mut i, r#"(SfyImport "/t/cb.sfyx" ?callbacks t)"#);
    let log = file(&i, "/t/cb.sfyx.log");
    assert!(ok.truthy() && log.ends_with("PASS\n"), "{log}");
    let order = run(&mut i, "(reverse sfyCbOrder)");
    let mut s = String::new();
    skill::print(&order, &mut s);
    assert_eq!(s, "(\"c\" \"a\" \"b\")");
    assert!(log.contains("callback failed line 8: I0 c: "), "{log}");
    assert!(log.contains("clamped line 7: I0 b sent 3 now 5"), "{log}");
    assert!(
        log.contains("count default 1\n"),
        "n equal to its default is not set: {log}"
    );
    let cv = i.host.cellview("cb", "top", "schematic").unwrap();
    let inst = match i.host.attr(cv, "instances").items() {
        [V::Obj(o)] => *o,
        other => panic!("{other:?}"),
    };
    assert_eq!(i.host.attr(inst, "a").as_str(), Some("1"));
    assert_eq!(i.host.attr(inst, "b").as_str(), Some("5"));
    assert_eq!(i.host.attr(inst, "x").as_str(), Some("free"));
    assert!(!i.host.attr(inst, "n").truthy());
    // As the form runs them: every value is set before the first callback (a, before b in
    // CDF order, sees b's new value), and cdfgData is the cell CDF (id: the cell).
    let seen = run(&mut i, "sfyCbSeen");
    let mut s = String::new();
    skill::print(&seen, &mut s);
    assert_eq!(s, "(\"3\" \"ddCell\")");
    // The read-only d is left to the callbacks: b's callback derives it, the file's stale
    // 7 is not sent.
    assert_eq!(i.host.attr(inst, "d").as_str(), Some("9"));
    assert!(log.contains("count skipped:read-only param 1\n"), "{log}");
    // Read back after the callbacks: what the PDK derived differs from the file's value.
    assert!(log.contains("derived line 9: I0 d file 7 now 9"), "{log}");
    // The cell CDF gets its own values back.
    let b = run(
        &mut i,
        r#"(getSGq (getSGq (cdfGetCellCDF (ddGetObj "cb" "dev")) b) value)"#,
    );
    assert_eq!(b.as_str(), Some("0"));
    // The masters the import opened are closed again (the build left its own open).
    assert_eq!(
        run(&mut i, "(length (dbGetOpenCellViews))").as_int(),
        open_before.as_int()
    );
    // Callbacks are opt-in.
    let mut i = fixtures();
    i.set_global("sfyCbOrder", V::Nil);
    i.files.insert("/t/cb.sfyx".into(), cb_dump("4"));
    assert!(run(&mut i, r#"(SfyImport "/t/cb.sfyx")"#).truthy());
    assert!(matches!(run(&mut i, "sfyCbOrder"), V::Nil));
    let cv = i.host.cellview("cb", "top", "schematic").unwrap();
    let inst = match i.host.attr(cv, "instances").items() {
        [V::Obj(o)] => *o,
        other => panic!("{other:?}"),
    };
    assert!(matches!(i.host.attr(inst, "n"), V::Int(4)));
    assert_eq!(i.host.attr(inst, "b").as_str(), Some("3"));
    // Without callbacks the file's d is the only source, so it is sent; the log says which
    // callbacks did not run.
    assert_eq!(i.host.attr(inst, "d").as_str(), Some("7"));
    let log = file(&i, "/t/cb.sfyx.log");
    assert!(log.contains("count callback not run 3\n"), "{log}");
}

#[test]
fn malformed_or_truncated_input_is_refused_and_nothing_is_saved() {
    let good = std::fs::read_to_string(format!("{FIXTURES}/inv.sfyx")).expect("golden");
    let lines: Vec<&str> = good.lines().collect();
    let inst = lines
        .iter()
        .position(|l| l.starts_with("(\"inst\""))
        .unwrap();
    let with = |k: usize, l: &str| -> String {
        let mut v = lines.clone();
        v[k] = l;
        v.join("\n") + "\n"
    };
    let cases: Vec<(String, usize, &str)> = vec![
        (
            lines[..lines.len() - 1].join("\n") + "\n",
            lines.len() - 1,
            "truncated",
        ),
        (
            with(
                inst,
                "(\"inst\" 1 \"P0\" \"analogLib\" \"pmos4\" \"symbol\" 0 1.5 \"R0\")",
            ),
            inst + 1,
            "1.5 is not a string, integer or list",
        ),
        (
            with(
                inst,
                "(\"inst\" 1 \"P0\" \"analogLib\" \"pmos4\" \"symbol\" 0 word \"R0\")",
            ),
            inst + 1,
            "is not a string",
        ),
        (
            with(inst, "(\"inst\" 1 \"P0\" \"analogLib\")"),
            inst + 1,
            "inst takes 8 fields",
        ),
        (
            with(inst, "(\"instance\" 1)"),
            inst + 1,
            "unknown record instance",
        ),
        (
            with(
                inst,
                "(\"inst\" 1 \"P0\" \"analogLib\" \"pmos4\" \"symbol\" \"0\" 0 \"R0\")",
            ),
            inst + 1,
            "field 6 is not an integer",
        ),
        (
            with(
                inst,
                "(\"inst\" 1 \"P0\" \"analogLib\" \"pmos4\" \"symbol\" 0 0 \"R0\"",
            ),
            inst + 1,
            "not readable",
        ),
        (with(0, "(\"sfyx\" 2 \"x\")"), 1, "not an .sfyx v1 file"),
        (
            with(1, "(\"cv\" \"schematic\" \"schematic\" 160)"),
            2,
            "cv outside a cell",
        ),
    ];
    for (text, line, what) in cases {
        let mut i = interp(&["analogLib", "basic", "scratch"]);
        i.load(BUILD).unwrap();
        run(&mut i, "(SfyFixStdlib)");
        i.files.insert("/t/bad.sfyx".into(), text);
        let ok = run(&mut i, r#"(SfyImport "/t/bad.sfyx" ?lib "scratch")"#);
        let log = file(&i, "/t/bad.sfyx.log");
        assert!(!ok.truthy(), "{what}: accepted\n{log}");
        assert!(
            log.contains(&format!("refused line {line}: ")) && log.contains(what),
            "{what}: {log}"
        );
        assert!(log.ends_with("FAIL\n"));
        for view in ["schematic", "symbol"] {
            assert!(
                i.host.cellview("scratch", "inv", view).is_none(),
                "{what}: {view} was created"
            );
        }
    }
}

#[test]
fn schematic_with_check_errors_is_not_saved() {
    let text = "(\"sfyx\" 1 \"t\")\n(\"lib\" \"work\")\n(\"cell\" \"dangling\" \"design\")\n\
                (\"cv\" \"schematic\" \"schematic\" 160)\n(\"wire\" 1 (0 0 20 0) 0)\n\
                (\"label\" 2 0 \"x\" 40 0 \"R0\" \"lowerLeft\" \"stick\" 10 \"normal\" \"wire\" \"label\")\n(\"end\")\n";
    let mut i = interp(&["scratch"]);
    i.files.insert("/t/d.sfyx".into(), text.into());
    assert!(!run(&mut i, r#"(SfyImport "/t/d.sfyx" ?lib "scratch")"#).truthy());
    let log = file(&i, "/t/d.sfyx.log");
    assert!(
        log.contains("check scratch/dangling/schematic: 1 errors")
            && log.contains("not saved: 1 check errors"),
        "{log}"
    );
    assert!(
        i.host
            .cellview("scratch", "dangling", "schematic")
            .is_none()
    );
    assert!(
        run(
            &mut i,
            r#"(SfyImport "/t/d.sfyx" ?lib "scratch" ?check nil)"#
        )
        .truthy()
    );
    assert!(
        i.host
            .cellview("scratch", "dangling", "schematic")
            .is_some()
    );
}

#[test]
fn conn_records_draw_a_labelled_stub_at_the_real_pin() {
    let text = "(\"sfyx\" 1 \"t\")\n(\"lib\" \"work\")\n(\"cell\" \"remap\" \"design\")\n\
                (\"cv\" \"schematic\" \"schematic\" 160)\n(\"inst\" 1 \"R0\" \"analogLib\" \"res\" \"symbol\" 0 0 \"R90\")\n\
                (\"conn\" 1 \"PLUS\" \"out\")\n(\"conn\" 1 \"MINUS\" \"gnd!\")\n(\"end\")\n";
    let mut i = interp(&["analogLib", "basic", "scratch"]);
    i.load(BUILD).unwrap();
    run(&mut i, "(SfyFixStdlib)");
    i.files.insert("/t/c.sfyx".into(), text.into());
    assert!(
        run(&mut i, r#"(SfyImport "/t/c.sfyx" ?lib "scratch")"#).truthy(),
        "{}",
        file(&i, "/t/c.sfyx.log")
    );
    run(
        &mut i,
        r#"(SfyExport "/t/c2.sfyx" "scratch" (list "remap") ?ref nil)"#,
    );
    let t = file(&i, "/t/c2.sfyx");
    // R90 turns PLUS (0 40) to (-40 0): the stub runs one grid step further out, labelled.
    assert!(t.contains("(\"wire\" 2 (-50000 0 -40000 0) 0)"), "{t}");
    assert!(t.contains("\"out\" -50000 0"), "{t}");
    assert!(t.contains("\"gnd!\" 50000 0"), "{t}");
    assert!(t.contains("\"schemifyStub\" \"string\" \"1\""), "{t}");
}

#[test]
fn the_probe_runs_on_the_mock() {
    let mut i = fixtures();
    i.load(interop::skill::PROBE).expect("probe loads");
    assert!(
        run(
            &mut i,
            r#"(SfyProbe "scratch" ?pdkLib "pdk" ?pdkCell "nch")"#
        )
        .truthy()
    );
    let t = file(&i, "/tmp/sfy_probe.txt");
    assert!(!t.contains("ERROR"), "{t}");
    for want in [
        "version \"sfy test interpreter\"",
        "not callable nil",
        "errmsg SfyErrMsg",
        "orient MYR90 (-2.0 -1.0)",
        "escaper PASS",
        "schCreatePin returns objType=\"inst\"",
        "boolean prop reads \"TRUE\"",
        "mode w-open \"w\"",
        "(\"sfyNet\" \"netSet\" \"VDDA\")",
        // The fixture's w callback calls an undefined pdkWidthCB: reported, not fatal.
        "pdk callbacks of \"w\" -> failed (\"w\")",
        "mae getAnalysis tran (((\"anaName\" \"tran\")",
        "mae getSimOption temp (\"55\")",
        "mae output name=(\"o1\") expression=(\"ymax(VT(\\\"/n1\\\"))\")",
        "mae reopen r",
        "end\n",
    ] {
        assert!(t.contains(want), "{want:?} missing from\n{t}");
    }
    // The scratch cellview was purged, never saved.
    assert!(
        i.host
            .cellview("scratch", "sfy_probe", "schematic")
            .is_none()
    );
}

#[test]
fn netset_overrides_dump_and_load_with_their_type() {
    let mut i = fixtures();
    // An inherited-connection override as Virtuoso stores it (dbReplaceProp … "netSet").
    run(
        &mut i,
        r#"(let ((cv (dbOpenCellViewByType "cb" "ns" "schematic" "schematic" "w")) (x nil))
             (setq x (schCreateInst cv (dbOpenCellViewByType "cb" "dev" "symbol" nil "r") "I0"
                       (list 0.0 0.0) "R0"))
             (dbReplaceProp x "bulk" "netSet" "VDDA")
             (dbSave cv) (dbClose cv))"#,
    );
    let ok = run(
        &mut i,
        r#"(SfyExport "/t/ns.sfyx" "cb" (list "ns") ?ref nil)"#,
    );
    let log = file(&i, "/t/ns.sfyx.log");
    let t = file(&i, "/t/ns.sfyx");
    assert!(ok.truthy() && !log.contains("skipped:prop"), "{log}");
    assert!(t.contains("\"bulk\" \"netSet\" \"VDDA\")"), "{t}");
    // Rust reads the type (a net override below the master's own pins is reviewed).
    interop::sfyx::read(&t).expect("canonical");
    // Loaded back, the property keeps its type.
    let ok = run(&mut i, r#"(SfyImport "/t/ns.sfyx" ?lib "scratch")"#);
    assert!(ok.truthy(), "{}", file(&i, "/t/ns.sfyx.log"));
    let ty = run(
        &mut i,
        r#"(let ((cv (dbOpenCellViewByType "scratch" "ns" "schematic" nil "r")))
             (car (setof p (getSGq (car (getSGq cv instances)) prop)
                    (equal (getSGq p name) "bulk"))))"#,
    );
    let V::Obj(p) = ty else { panic!("{ty:?}") };
    assert_eq!(i.host.attr(p, "valueType").as_str(), Some("netSet"));
}

#[test]
fn an_unbound_master_is_refused_and_the_rest_dumps() {
    let mut i = fixtures();
    run(
        &mut i,
        r#"(let ((cv (dbOpenCellViewByType "cb" "ub" "schematic" "schematic" "w")))
             (schCreateInst cv (dbOpenCellViewByType "cb" "dev" "symbol" nil "r") "I0"
               (list 0.0 0.0) "R0")
             (dbSave cv) (dbClose cv))"#,
    );
    // The master's library leaves cds.lib after the schematic was drawn.
    i.host.forget_cell("cb", "dev");
    let ok = run(&mut i, r#"(SfyExport "/t/ub.sfyx" "cb" (list "ub"))"#);
    let log = file(&i, "/t/ub.sfyx.log");
    assert!(!ok.truthy() && log.ends_with("FAIL\n"), "{log}");
    assert!(log.contains("refused: master cb/dev not found"), "{log}");
    let t = file(&i, "/t/ub.sfyx");
    assert!(
        t.ends_with("(\"end\")\n") && t.contains("(\"cell\" \"ub\" \"design\")"),
        "{t}"
    );
}

#[test]
fn a_target_open_for_editing_is_refused() {
    let mut i = fixtures();
    run(&mut i, DUMPS[0].1);
    let t = file(&i, "/t/inv.sfyx");
    i.files.insert("/t/in.sfyx".into(), t);
    assert!(run(&mut i, r#"(SfyImport "/t/in.sfyx" ?lib "scratch")"#).truthy());
    // The user opens the schematic in an editor window, then imports again.
    run(
        &mut i,
        r#"(setq sfyWin (dbOpenCellViewByType "scratch" "inv" "schematic" nil "a"))"#,
    );
    let ok = run(
        &mut i,
        r#"(SfyImport "/t/in.sfyx" ?lib "scratch" ?overwrite t)"#,
    );
    let log = file(&i, "/t/in.sfyx.log");
    assert!(!ok.truthy(), "{log}");
    assert!(
        log.contains("scratch/inv/schematic is open for editing in Virtuoso; close it first"),
        "{log}"
    );
    // Read-only opens (another view's master, a viewer) do not block.
    run(&mut i, "(dbClose sfyWin)");
    run(
        &mut i,
        r#"(setq sfyWin (dbOpenCellViewByType "scratch" "inv" "schematic" nil "r"))"#,
    );
    assert!(
        run(
            &mut i,
            r#"(SfyImport "/t/in.sfyx" ?lib "scratch" ?overwrite t)"#
        )
        .truthy(),
        "{}",
        file(&i, "/t/in.sfyx.log")
    );
}

#[test]
fn set_params_refuses_locked_params_and_prints_numbers_without_noise() {
    let mut i = fixtures();
    run(
        &mut i,
        r#"(let ((cv (dbOpenCellViewByType "cb" "sp" "schematic" "schematic" "w")))
             (setq sfyInst (schCreateInst cv (dbOpenCellViewByType "cb" "dev" "symbol" nil "r")
                             "I0" (list 0.0 0.0) "R0")))"#,
    );
    // 0.18e-6 is 1.7999999999999999e-07 in binary: a string parameter gets 1.8e-07.
    assert!(
        run(
            &mut i,
            r#"(SfySetParams sfyInst (list (list "a" 1.8e-7)) nil)"#
        )
        .truthy()
    );
    let inst = match run(&mut i, "sfyInst") {
        V::Obj(o) => o,
        v => panic!("{v:?}"),
    };
    assert_eq!(i.host.attr(inst, "a").as_str(), Some("1.8e-07"));
    // d is read-only in the PDK: refused with the reason, not written.
    let why = run(
        &mut i,
        r#"(SfySetParams sfyInst (list (list "d" "1")) nil)"#,
    );
    assert!(
        why.as_str().is_some_and(|w| w.contains("d is read-only")),
        "{why:?}"
    );
    assert!(!i.host.attr(inst, "d").truthy());
    // A failing callback is a "callback:" answer: the value is set, the device kept.
    let why = run(&mut i, r#"(SfySetParams sfyInst (list (list "c" "1")) t)"#);
    assert!(
        why.as_str()
            .is_some_and(|w| w.starts_with("callback: the CDF callback of c")),
        "{why:?}"
    );
    assert_eq!(i.host.attr(inst, "c").as_str(), Some("1"));
}

#[test]
fn a_bad_field_writes_no_half_record() {
    let mut i = fixtures();
    let r = run(
        &mut i,
        r#"(let ((p (outfile "/t/half.sfyx" "w")) (r nil))
             (setq r (errset (SfyPut p "inst" (list 1 "a" 1.5))))
             (close p)
             r)"#,
    );
    assert!(matches!(r, V::Nil), "a float field signals");
    assert_eq!(
        file(&i, "/t/half.sfyx"),
        "",
        "nothing written before the check"
    );
}

#[test]
fn a_skill_error_costs_one_cellview_and_the_log_still_closes() {
    let mut i = fixtures();
    run(&mut i, DUMPS[0].1);
    let t = file(&i, "/t/inv.sfyx");
    i.files.insert("/t/in.sfyx".into(), t);
    // A release whose label call signals (an unexpected argument shape, say).
    run(&mut i, "(procedure (SfyLoadLabel lr) (error \"boom\"))");
    let ok = run(&mut i, r#"(SfyImport "/t/in.sfyx" ?lib "scratch")"#);
    let log = file(&i, "/t/in.sfyx.log");
    assert!(!ok.truthy() && log.ends_with("FAIL\n"), "{log}");
    assert!(log.contains("SKILL error") && log.contains("boom"), "{log}");
    // Each cellview with a label is refused on its own line (symbol, then schematic), the
    // rest of the import went on (instances, wires); nothing is left open for writing.
    assert_eq!(log.matches("SKILL error").count(), 2, "{log}");
    assert!(log.contains("count wire "), "{log}");
    let writing = run(
        &mut i,
        r#"(setof cv (dbGetOpenCellViews) (equal (getSGq cv mode) "w"))"#,
    );
    assert!(matches!(writing, V::Nil), "{writing:?}");
    // Export: the same backstop ends the log with the message.
    run(
        &mut i,
        "(procedure (SfyDumpTextViews port lib cell) (error \"bang\"))",
    );
    let ok = run(&mut i, DUMPS[0].1);
    let log = file(&i, "/t/inv.sfyx.log");
    assert!(
        !ok.truthy() && log.ends_with("FAIL\n") && log.contains("bang"),
        "{log}"
    );
}
