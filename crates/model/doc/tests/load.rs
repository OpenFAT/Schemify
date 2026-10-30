//! Load-time semantics: `W001` duplicate ids re-assigned deterministically, newer
//! format versions open read-only, `inherit` keyed by pin, the `E024`/`E042` load
//! diagnostics, `at`/`name` written first, schematic shapes through load, edit and undo,
//! and the record read API against the file.

mod common;

use common::OTA;
use schemify_doc::{Code, Edit, ObjId, Op, Outcome, Project, RecordBuf, Target};
use schemify_format::RecordKind;

// ---- private ----

#[test]
fn duplicate_ids_are_reassigned_deterministically() {
    let text = "schemify 1 shbench\n\ninst 0gsrp0 ./a\n  at 0 0 R0\n  name X1\ninst 0gsrp0 ./a\n  at 10 0 R0\n  name X2\n\nwire 0gsrp0 0 0 10 0\n\nwaive 1aaaaa 0gsrp0 W\n  reason ok\n";
    let mut p = Project::new(None, None, 1).unwrap();
    let opened = p.open("", "b.shbench", text.as_bytes(), None).unwrap();
    let w001 = opened.diags.iter().filter(|d| d.code == Code::W001).count();
    assert_eq!(w001, 2);
    let h = opened.handle;
    assert!(p.is_dirty(h), "re-ids must be saved");
    let first = common::canon(&p, h);
    let (q, g) = common::open(text, "b.shbench");
    assert_eq!(common::canon(&q, g), first, "re-iding is deterministic");
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
    let (mut p, h) = common::open(text, "new.shbench");
    let d = p.doc(h).unwrap();
    assert!(d.read_only());
    assert_eq!(d.wires().x1.len(), 1, "known records are still readable");
    let del = Op::Delete(Target::new(RecordKind::Wire, "0d2k1m"));
    assert_eq!(
        common::run(&mut p, h, common::tx(vec![del])),
        Outcome::Refused(Code::E044)
    );
    assert_eq!(
        common::run(&mut p, h, Edit::Undo),
        Outcome::Refused(Code::E044)
    );
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
    assert_eq!(common::canon(&p, opened.handle), text);
}

#[test]
fn instance_at_and_name_are_written_first() {
    let text = "schemify 1 shbench\n\ninst 0gsrp0 ./a\n  name X1\n  W 1\n  at 0 0 R0\n";
    let (p, h) = common::open(text, "b.shbench");
    assert_eq!(
        common::canon(&p, h),
        "schemify 1 shbench\n\ninst 0gsrp0 ./a\n  at 0 0 R0\n  name X1\n  W 1\n"
    );
}

/// Rank-3 `shape` rows (spec §4): load, find by id, edit (insert, put, delete) through
/// transactions with undo restoring the bytes, and the per-form arity check on edits.
#[test]
fn schematic_shapes_load_edit_and_undo() {
    let text = "schemify 1 shbench\n\nshape 0abc arc 0 0 5 0 90\n  layer 7\nshape 0abd poly 0 0 10 0 10 10\n  closed true\n";
    let (mut p, h) = common::open(text, "b.shbench");
    assert_eq!(common::canon(&p, h), text, "shapes round-trip");
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
    let out = common::run(
        &mut p,
        h,
        common::tx(vec![Op::Insert {
            rec: line,
            at: None,
        }]),
    );
    assert!(matches!(out, Outcome::Committed { .. }), "{out:?}");
    assert!(common::canon(&p, h).contains("shape 0abe line 0 0 20 0\n"));
    let short = RecordBuf::new(RecordKind::Shape)
        .field("0abf")
        .field("line")
        .field("0")
        .field("0");
    let out = common::run(
        &mut p,
        h,
        common::tx(vec![Op::Insert {
            rec: short,
            at: None,
        }]),
    );
    assert!(matches!(out, Outcome::Rejected(_)), "{out:?}");
    let out = common::run(
        &mut p,
        h,
        common::tx(vec![Op::Delete(Target::new(RecordKind::Shape, "0abd"))]),
    );
    assert!(matches!(out, Outcome::Committed { .. }), "{out:?}");
    assert!(!common::canon(&p, h).contains("0abd"));
    common::run(&mut p, h, Edit::Undo);
    common::run(&mut p, h, Edit::Undo);
    assert_eq!(common::canon(&p, h), text, "undo restores the file");
}

#[test]
fn record_read_api_matches_the_file() {
    let (p, h) = common::open(OTA, "ota5t.shcomp");
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
