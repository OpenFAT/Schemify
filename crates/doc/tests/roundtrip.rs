//! Load/save: the spec example files round-trip byte-identically; load(save(x)) == x
//! over generated documents of every file kind and record kind; load-time semantics
//! (W001 re-id, read-only newer versions, E024/E042 load diagnostics).

mod common;

use common::*;
use schemify_doc::{Code, Edit, ObjId, Op, Outcome, Project, RecordBuf, Target};
use schemify_format::{Document as FormatDoc, FileKind, RecordKind, parse, write};

#[test]
fn spec_examples_round_trip_byte_identically() {
    for (name, text) in [
        ("ota5t.shcomp", OTA),
        ("nfet_01v8.shprim", NFET),
        ("ota_ac.shbench", BENCH),
    ] {
        let (p, h) = open(text, name);
        assert_eq!(canon(&p, h), text, "{name}");
        assert!(!p.is_dirty(h));
    }
}

#[test]
fn messy_input_canonicalizes_like_the_format() {
    let (p, h) = open(MESSY, "ota5t.shcomp");
    assert_eq!(canon(&p, h), OTA);
    assert!(!p.is_dirty(h), "canonicalization is not an edit");
}

#[test]
fn tables_hold_the_example_data() {
    let (p, h) = open(OTA, "ota5t.shcomp");
    let d = p.doc(h).unwrap();
    let s = d.strings();
    let pins: Vec<&str> = d.pins().name.iter().map(|n| s.get(*n)).collect();
    assert_eq!(pins, ["INP", "INN", "OUT", "IBIAS", "VDD", "VSS"]);
    assert_eq!(d.instances().id.len(), 5);
    let m2 = (0..5)
        .find(|&i| s.get(d.instances().name[i]) == "M2")
        .unwrap();
    assert_eq!(
        (d.instances().x[m2], d.instances().y[m2]),
        (120_000, -300_000)
    );
    assert_eq!(d.instances().orient[m2], schemify_format::Orient::MR0);
    let w = d.pool().find_attr(s, d.instances().attrs[m2], "W").unwrap();
    assert_eq!(s.get(w[0]), "Wp");
    assert_eq!(d.wires().x1.len(), 5);
    assert!(d.wires().x1.iter().zip(&d.wires().x2).all(|(a, b)| a <= b));
    assert_eq!(d.graphics().shape[0], RecordKind::Poly);
    assert_eq!(d.doc_blocks().kind, [RecordKind::Prose, RecordKind::Var]);
    let id = ObjId::find("0gsrp0", s).unwrap();
    assert!(!id.is_alias(), "6-char ids are their 30-bit value");
    assert_eq!(
        d.find_id(id).unwrap().table,
        schemify_doc::TableId::Instances
    );
    assert_eq!(d.header_value("category"), Some("amplifier"));
}

// ------------------------------------------------------------------ generated documents

fn ident(rng: &mut Mix, used: &mut Vec<String>) -> String {
    const A: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    loop {
        let len = if rng.chance(15) {
            1 + rng.below(13) as usize
        } else {
            6
        };
        let id: String = (0..len)
            .map(|_| A[rng.below(32) as usize] as char)
            .collect();
        if !used.contains(&id) {
            used.push(id.clone());
            return id;
        }
    }
}

fn text(rng: &mut Mix) -> &'static str {
    rng.word(&[
        "a",
        "W",
        "1u",
        "-3",
        "a b",
        "say \"hi\"",
        "back\\slash",
        "#x",
        "|y",
        "",
        "µ",
        "line\nbreak",
        "tab\tx",
    ])
}

fn c(rng: &mut Mix) -> String {
    coord((rng.below(2001) as i32 - 1000) * if rng.chance(20) { 1 } else { 1000 })
}

fn comment(rng: &mut Mix, d: &mut FormatDoc<'static>) {
    if rng.chance(15) {
        d.push_comment(rng.word(&[" note", "", "# double", " µ unicode"]));
    }
}

/// A random, valid file of `kind` exercising every record kind it allows.
fn gen_file(rng: &mut Mix, kind: FileKind) -> String {
    let mut d = FormatDoc::new(kind);
    let mut ids = Vec::new();
    d.push_attr("doc");
    d.push_value(text(rng));
    if rng.chance(50) {
        d.push_attr("pdk");
        d.push_value("sky130");
    }
    let cell = kind != FileKind::Shbench;
    let mut impls = Vec::new();
    if cell {
        for i in 0..1 + rng.below(4) {
            d.begin_record(RecordKind::Pin);
            comment(rng, &mut d);
            d.push_field(&format!("P{i}"));
            d.push_field(rng.word(&["in", "out", "inout"]));
            d.push_field(&c(rng));
            d.push_field(&c(rng));
            if rng.chance(30) {
                d.push_attr("domain");
                d.push_value("digital");
            }
        }
        for i in 0..rng.below(4) {
            d.begin_record(RecordKind::Param);
            d.push_field(&format!("k{i}"));
            d.push_field(text(rng));
            if rng.chance(50) {
                d.push_attr("choices");
                d.push_value("a");
                d.push_value("b c");
            }
        }
        if kind == FileKind::Shcomp {
            impls.push("sch".to_owned());
            d.begin_record(RecordKind::Impl);
            d.push_field("sch");
            d.push_field("schematic");
        }
        for i in 0..(kind == FileKind::Shprim) as u64 + rng.below(2) {
            let name = format!("leaf{i}");
            d.begin_record(RecordKind::Impl);
            d.push_field(&name);
            d.push_field("spice");
            d.push_attr("format");
            d.push_value("spice");
            d.push_value("X{name} {D}");
            impls.push(name);
        }
        for g in 0..rng.below(6) {
            let k = *rng.pick(&[
                RecordKind::Line,
                RecordKind::Rect,
                RecordKind::Arc,
                RecordKind::Circle,
                RecordKind::Poly,
                RecordKind::Stext,
            ]);
            d.begin_record(k);
            let n = match k {
                RecordKind::Arc | RecordKind::Circle => 3,
                RecordKind::Stext => 2,
                RecordKind::Poly => 4 + 2 * rng.below(3) as usize,
                _ => 4,
            };
            for _ in 0..n {
                d.push_field(&c(rng));
            }
            match k {
                RecordKind::Arc => {
                    d.push_field("0");
                    d.push_field("90.5");
                }
                RecordKind::Stext => {
                    d.push_field("R90");
                    d.push_field(text(rng));
                    d.push_attr("hcenter");
                }
                _ => {}
            }
            if g % 2 == 0 {
                d.push_attr("layer");
                d.push_value("4");
            }
        }
    }
    let mut insts = Vec::new();
    if kind != FileKind::Shprim {
        for i in 0..rng.below(6) {
            let id = ident(rng, &mut ids);
            d.begin_record(RecordKind::Inst);
            comment(rng, &mut d);
            d.push_field(&id);
            d.push_field(rng.word(&["./ota5t", "sky130:nfet_01v8"]));
            d.push_attr("at");
            d.push_value(&c(rng));
            d.push_value(&c(rng));
            d.push_value("MR270");
            d.push_attr("name");
            d.push_value(&format!("X{i}"));
            if rng.chance(50) {
                d.push_attr("W");
                d.push_value(text(rng));
            }
            if rng.chance(30) {
                d.push_attr("inherit");
                d.push_value("VDD");
                d.push_value("vdd1");
            }
            insts.push(id);
        }
        for _ in 0..rng.below(6) {
            d.begin_record(RecordKind::Wire);
            d.push_field(&ident(rng, &mut ids));
            let (x, y) = (c(rng), (rng.below(2001) as i32 - 1000) * 125);
            d.push_field(&x);
            d.push_field(&coord(y));
            d.push_field(&x);
            d.push_field(&coord(y + 1000));
            if rng.chance(30) {
                d.push_attr("bus");
                d.push_value("true");
            }
        }
        let placed: &[RecordKind] = if kind == FileKind::Shcomp {
            &[RecordKind::Label, RecordKind::Port, RecordKind::Text]
        } else {
            &[RecordKind::Label, RecordKind::Text]
        };
        for _ in 0..rng.below(6) {
            d.begin_record(*rng.pick(placed));
            d.push_field(&ident(rng, &mut ids));
            d.push_field(&c(rng));
            d.push_field(&c(rng));
            d.push_field("R0");
            d.push_field(text(rng));
        }
        if rng.chance(50) {
            d.begin_record(RecordKind::Noconn);
            d.push_field(&ident(rng, &mut ids));
            d.push_field(&c(rng));
            d.push_field(&c(rng));
        }
        if !insts.is_empty() {
            d.begin_record(RecordKind::Constraint);
            d.push_field(&ident(rng, &mut ids));
            d.push_field("match");
            for m in &insts {
                d.push_attr("member");
                d.push_value(m);
            }
            d.push_attr("pattern");
            d.push_value("common-centroid");
            d.begin_record(RecordKind::Waive);
            d.push_field(&ident(rng, &mut ids));
            d.push_field(&insts[0]);
            d.push_field("W");
            d.push_attr("reason");
            d.push_value(text(rng));
        }
    }
    for i in 0..rng.below(4) {
        if rng.chance(50) {
            d.begin_record(RecordKind::Prose);
            d.push_field(&ident(rng, &mut ids));
            for l in ["# t", "", "| pipe", "  indented", "$$x^2$$"] {
                d.push_verbatim(l);
            }
        } else {
            d.begin_record(RecordKind::Var);
            d.push_field(&format!("v{i}"));
            d.push_field(text(rng));
            d.push_attr("unit");
            d.push_value("V");
        }
    }
    if cell {
        for name in &impls {
            if name != "sch" && rng.chance(60) {
                d.begin_record(RecordKind::Body);
                d.push_field(name);
                d.push_verbatim(".subckt x a b");
                d.push_verbatim("");
            }
        }
    }
    if kind == FileKind::Shbench {
        d.begin_record(RecordKind::Corner);
        d.push_field("tt");
        d.push_attr("lib");
        d.push_value("${PDK}/x.lib");
        d.push_value("tt");
        d.push_attr("temp");
        d.push_value("27");
        d.begin_record(RecordKind::Config);
        d.push_field("default");
        d.push_attr("order");
        d.push_value("schematic");
        d.push_value("spice");
        d.begin_record(RecordKind::Sweep);
        d.push_field("s");
        d.push_attr("lin");
        for v in ["1", "2", "5"] {
            d.push_value(v);
        }
        d.begin_record(RecordKind::Montecarlo);
        d.push_field("mc");
        d.push_attr("samples");
        d.push_value("100");
        d.begin_record(RecordKind::Optimize);
        d.push_field("opt");
        d.push_attr("goal");
        d.push_value("gbw");
        d.push_value("max");
        d.begin_record(RecordKind::Output);
        d.push_field("gbw");
        d.push_field(text(rng));
        d.begin_record(RecordKind::Spec);
        d.push_field("gbw");
        d.push_attr("min");
        d.push_value("10meg");
        d.begin_record(RecordKind::Code);
        d.push_field("control");
        d.push_field("spice");
        d.push_verbatim(".control");
        d.push_verbatim(".endc");
    }
    if impls.len() > 1 {
        // A cell with several impls names its default; the header takes it before
        // records, so rebuild with it.
        return gen_with_default(&d, &impls[0]);
    }
    if rng.chance(30) {
        d.push_eof_comment(" end");
    }
    write(&d)
}

fn gen_with_default(d: &FormatDoc<'_>, default: &str) -> String {
    let mut out = FormatDoc::new(d.kind());
    for a in d.attrs(0) {
        out.push_attr(d.attr_key(a));
        for v in d.attr_values(a) {
            out.push_value(v);
        }
    }
    out.push_attr("default");
    out.push_value(default);
    for r in 1..d.record_count() {
        out.copy_record(d, r);
    }
    write(&out)
}

fn round_trip(seed: u64) {
    let mut rng = Mix::new(seed);
    for kind in [FileKind::Shprim, FileKind::Shcomp, FileKind::Shbench] {
        let text = gen_file(&mut rng, kind);
        let model = parse(text.as_bytes())
            .unwrap_or_else(|e| panic!("seed {seed}: generator broke: {e}\n{text}"));
        assert_eq!(write(&model), text, "the generator writes canonical text");
        let (p, h) = open(&text, "gen.x");
        assert_eq!(
            canon(&p, h),
            text,
            "save(load(x)) != x, seed {seed}\n{text}"
        );
        let (q, g) = open(&canon(&p, h), "gen.x");
        assert_eq!(canon(&q, g), text, "load(save(x)) != x, seed {seed}");
        // Every record reads back as the same record through the table read path.
        let d = p.doc(h).unwrap();
        let rows: usize = schemify_doc::TableId::ALL
            .iter()
            .map(|t| d.table_len(*t))
            .sum();
        assert_eq!(rows as u32 + 1, model.record_count(), "seed {seed}");
    }
}

/// Property over generated inputs: 256 seeds drawn from one fixed stream, so a failure
/// replays from the printed seed.
#[test]
fn load_save_round_trips_generated_documents() {
    let mut seeds = Mix::new(0x5eed);
    for _ in 0..256 {
        let seed = seeds.next();
        round_trip(seed);
    }
}

// ------------------------------------------------------------------ load semantics

#[test]
fn duplicate_ids_are_reassigned_deterministically() {
    let text = "schemify 1 shbench\n\ninst 0gsrp0 ./a\n  at 0 0 R0\n  name X1\ninst 0gsrp0 ./a\n  at 10 0 R0\n  name X2\n\nwire 0gsrp0 0 0 10 0\n\nwaive 1aaaaa 0gsrp0 W\n  reason ok\n";
    let mut p = Project::new(None, None, 1).unwrap();
    let opened = p.open("", "b.shbench", text.as_bytes(), None).unwrap();
    let w001 = opened.diags.iter().filter(|d| d.code == Code::W001).count();
    assert_eq!(w001, 2);
    let h = opened.handle;
    assert!(p.is_dirty(h), "re-ids must be saved");
    let first = canon(&p, h);
    let (q, g) = open(text, "b.shbench");
    assert_eq!(canon(&q, g), first, "re-iding is deterministic");
    let d = p.doc(h).unwrap();
    let s = d.strings();
    let ids: Vec<String> = d
        .instances()
        .id
        .iter()
        .chain(&d.wires().id)
        .map(|i| i.text(s))
        .collect();
    let mut uniq = ids.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 3, "{ids:?}");
    let x1 = (0..2)
        .find(|&i| s.get(d.instances().name[i]) == "X1")
        .unwrap();
    assert_eq!(
        d.instances().id[x1].text(s),
        "0gsrp0",
        "the first occurrence keeps its id"
    );
    assert!(
        opened.diags.iter().all(|d| d.code != Code::E042),
        "the waiver still resolves"
    );
    // Saving clears the fix-up state.
    let req = p.save(h).unwrap().unwrap();
    p.saved(h, req.bytes.clone()).unwrap();
    assert!(!p.is_dirty(h));
}

#[test]
fn newer_versions_open_read_only() {
    let text = "schemify 2 shbench\n\nfrobnicate 0abc 1 2\n  extra x\n\nwire 0d2k1m 0 0 10 0\n";
    let (mut p, h) = open(text, "new.shbench");
    let d = p.doc(h).unwrap();
    assert!(d.read_only());
    assert_eq!(d.wires().x1.len(), 1, "known records are still readable");
    let del = Op::Delete(Target::new(RecordKind::Wire, "0d2k1m"));
    assert_eq!(run(&mut p, h, tx(vec![del])), Outcome::Refused(Code::E044));
    assert_eq!(run(&mut p, h, Edit::Undo), Outcome::Refused(Code::E044));
    assert_eq!(p.save(h), Err(Code::E044));
}

#[test]
fn inherit_is_keyed_by_pin_on_load() {
    // One `inherit` line per pin is legal (format spec §4); the same pin twice is E024.
    let ok = "schemify 1 shbench\n\ninst 0gsrp0 ./a\n  at 0 0 R0\n  name X1\n  inherit VPWR vdd\n  inherit VGND 0\n";
    let mut p = Project::new(None, None, 1).unwrap();
    let opened = p.open("", "ok.shbench", ok.as_bytes(), None).unwrap();
    assert!(opened.diags.is_empty(), "{:?}", opened.diags);
    let twice = ok.replace("inherit VGND 0", "inherit VPWR vss");
    let opened = p.open("", "twice.shbench", twice.as_bytes(), None).unwrap();
    let codes: Vec<Code> = opened.diags.iter().map(|d| d.code).collect();
    assert_eq!(codes, [Code::E024]);
}

#[test]
fn load_reports_what_the_format_cannot_see() {
    let text = "schemify 1 shbench\n\ninst 0gsrp0 ./a\n  at 0 0 R0\n  name X1\n  W 1\n  W 2\n\nwaive 1aaaaa zzzzzz W\n  reason ok\n";
    let mut p = Project::new(None, None, 1).unwrap();
    let opened = p.open("", "b.shbench", text.as_bytes(), None).unwrap();
    let codes: Vec<Code> = opened.diags.iter().map(|d| d.code).collect();
    assert!(codes.contains(&Code::E024), "{codes:?}");
    assert!(codes.contains(&Code::E042), "{codes:?}");
    // Kept as written: the file still round-trips.
    assert_eq!(canon(&p, opened.handle), text);
}

#[test]
fn instance_at_and_name_are_written_first() {
    let text = "schemify 1 shbench\n\ninst 0gsrp0 ./a\n  name X1\n  W 1\n  at 0 0 R0\n";
    let (p, h) = open(text, "b.shbench");
    assert_eq!(
        canon(&p, h),
        "schemify 1 shbench\n\ninst 0gsrp0 ./a\n  at 0 0 R0\n  name X1\n  W 1\n"
    );
}

/// Rank-3 `shape` rows (spec §4): load, find by id, edit (insert, put, delete) through
/// transactions with undo restoring the bytes, and the per-form arity check on edits.
#[test]
fn schematic_shapes_load_edit_and_undo() {
    let text = "schemify 1 shbench\n\nshape 0abc arc 0 0 5 0 90\n  layer 7\nshape 0abd poly 0 0 10 0 10 10\n  closed true\n";
    let (mut p, h) = open(text, "b.shbench");
    assert_eq!(canon(&p, h), text, "shapes round-trip");
    let d = p.doc(h).unwrap();
    assert_eq!(d.shapes().form, [RecordKind::Arc, RecordKind::Poly]);
    let loc = d
        .find_id(ObjId::find("0abc", d.strings()).unwrap())
        .unwrap();
    assert_eq!(
        d.record(loc),
        RecordBuf::new(RecordKind::Shape)
            .field("0abc")
            .field("arc")
            .field("0")
            .field("0")
            .field("5")
            .field("0")
            .field("90")
            .attr("layer", &["7"])
    );
    let line = RecordBuf::new(RecordKind::Shape)
        .field("0abe")
        .field("line")
        .field("0")
        .field("0")
        .field("20")
        .field("0");
    let out = run(
        &mut p,
        h,
        tx(vec![Op::Insert {
            rec: line,
            at: None,
        }]),
    );
    assert!(matches!(out, Outcome::Committed { .. }), "{out:?}");
    assert!(canon(&p, h).contains("shape 0abe line 0 0 20 0\n"));
    let short = RecordBuf::new(RecordKind::Shape)
        .field("0abf")
        .field("line")
        .field("0")
        .field("0");
    let out = run(
        &mut p,
        h,
        tx(vec![Op::Insert {
            rec: short,
            at: None,
        }]),
    );
    assert!(matches!(out, Outcome::Rejected(_)), "{out:?}");
    let out = run(
        &mut p,
        h,
        tx(vec![Op::Delete(Target::new(RecordKind::Shape, "0abd"))]),
    );
    assert!(matches!(out, Outcome::Committed { .. }), "{out:?}");
    assert!(!canon(&p, h).contains("0abd"));
    run(&mut p, h, Edit::Undo);
    run(&mut p, h, Edit::Undo);
    assert_eq!(canon(&p, h), text, "undo restores the file");
}

#[test]
fn record_read_api_matches_the_file() {
    let (p, h) = open(OTA, "ota5t.shcomp");
    let d = p.doc(h).unwrap();
    let loc = d
        .find_id(ObjId::find("9vdm2q", d.strings()).unwrap())
        .unwrap();
    let rec = d.record(loc);
    let want = RecordBuf::new(RecordKind::Inst)
        .field("9vdm2q")
        .field("sky130:nfet_01v8")
        .attr("at", &["30", "-160", "R0"])
        .attr("name", &["M5"])
        .attr("W", &["4"])
        .attr("L", &["1"]);
    assert_eq!(rec, want);
}
