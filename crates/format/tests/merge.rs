//! TESTING.md `merge3` oracles: `merge(O, A, O) == A`, `merge(O, O, B) == B`,
//! non-conflicting edits commute, and model-based PBT over random edit scripts (move,
//! param change, insert, delete, label rename) against a naive model; plus `diff`.

mod common;

use common::{Rng, gen_doc};
use proptest::prelude::*;
use schemify_format::{
    ChangeKind, Document, FileKind, IdRng, Orient, Parts, RecordKind, diff, fresh_id, merge3,
    parse, write,
};
use std::collections::{BTreeMap, BTreeSet};

/// Cases of `merge_is_total` where all three sides parsed (checked to be non-vacuous).
static PARSED_ALL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[test]
fn merge_is_total_is_not_vacuous() {
    // proptest runs the cases of `merge_is_total` in its own test; run a few here.
    let before = PARSED_ALL.load(std::sync::atomic::Ordering::Relaxed);
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    runner
        .run(
            &(any::<u64>(), proptest::collection::vec(any::<u8>(), 0..24)),
            |(seed, junk)| {
                merge_is_total_body(seed, junk);
                Ok(())
            },
        )
        .unwrap();
    let hits = PARSED_ALL.load(std::sync::atomic::Ordering::Relaxed) - before;
    assert!(hits >= 64, "only {hits}/256 cases parsed all three sides");
}

// ---------------------------------------------------------------- naive model

#[derive(Clone, Debug, PartialEq)]
enum Obj {
    Inst {
        cell: String,
        x: i32,
        y: i32,
        orient: Orient,
        name: String,
        params: Vec<(String, String)>,
    },
    Wire {
        c: [i32; 4],
    },
    Label {
        x: i32,
        y: i32,
        net: String,
    },
}

/// A bench as the oracle sees it: a header doc line and objects by id.
#[derive(Clone, Debug)]
struct Model {
    doc: String,
    objs: BTreeMap<String, Obj>,
}

#[derive(Clone, Debug)]
enum Edit {
    Move(String, i32, i32),
    Param(String, String, String),
    Insert(String, Obj),
    Delete(String),
    Rename(String, String),
    MoveWire(String, [i32; 4]),
    Doc(String),
}

fn apply(m: &mut Model, e: &Edit) {
    match e {
        Edit::Move(id, nx, ny) => {
            if let Some(Obj::Inst { x, y, .. }) = m.objs.get_mut(id) {
                (*x, *y) = (*nx, *ny);
            }
        }
        Edit::Param(id, k, v) => {
            if let Some(Obj::Inst { params, .. }) = m.objs.get_mut(id) {
                match params.iter_mut().find(|p| p.0 == *k) {
                    Some(p) => p.1 = v.clone(),
                    None => params.push((k.clone(), v.clone())),
                }
            }
        }
        Edit::Insert(id, o) => {
            m.objs.insert(id.clone(), o.clone());
        }
        Edit::Delete(id) => {
            m.objs.remove(id);
        }
        Edit::Rename(id, n) => {
            if let Some(Obj::Label { net, .. }) = m.objs.get_mut(id) {
                *net = n.clone();
            }
        }
        Edit::MoveWire(id, nc) => {
            if let Some(Obj::Wire { c }) = m.objs.get_mut(id) {
                *c = *nc;
            }
        }
        Edit::Doc(text) => m.doc = text.clone(),
    }
}

/// Builds the document an editor would save for `m` (record push order is irrelevant:
/// the writer sorts rank 3 and normalizes wires).
fn render(m: &Model) -> Document<'static> {
    let mut d = Document::new(FileKind::Shbench);
    d.push_attr("doc");
    d.push_value(&m.doc);
    for (id, o) in &m.objs {
        match o {
            Obj::Inst {
                cell,
                x,
                y,
                orient,
                name,
                params,
            } => {
                d.begin_record(RecordKind::Inst);
                d.push_field(id);
                d.push_field(cell);
                d.push_attr("at");
                d.push_value_coord(*x);
                d.push_value_coord(*y);
                d.push_value(orient.as_str());
                d.push_attr("name");
                d.push_value(name);
                for (k, v) in params {
                    d.push_attr(k);
                    d.push_value(v);
                }
            }
            Obj::Wire { c } => {
                d.begin_record(RecordKind::Wire);
                d.push_field(id);
                for v in c {
                    d.push_field_coord(*v);
                }
            }
            Obj::Label { x, y, net } => {
                d.begin_record(RecordKind::Label);
                d.push_field(id);
                d.push_field_coord(*x);
                d.push_field_coord(*y);
                d.push_field("R0");
                d.push_field(net);
            }
        }
    }
    d
}

fn text(m: &Model) -> String {
    write(&render(m))
}

fn grid(rng: &mut Rng) -> i32 {
    (rng.below(100) as i32 - 50) * 10_000
}

fn new_id(rng: &mut Rng, used: &mut BTreeSet<String>) -> String {
    let id = fresh_id(&mut IdRng::new(rng.next()), |c| used.contains(c))
        .as_str()
        .to_string();
    used.insert(id.clone());
    id
}

fn new_obj(rng: &mut Rng) -> Obj {
    match rng.below(3) {
        0 => {
            let mut params = vec![("W".to_string(), format!("{}u", rng.below(9) + 1))];
            if rng.chance(50) {
                params.push(("L".to_string(), "0.5".to_string()));
            }
            Obj::Inst {
                cell: rng.word(&["sky130:nfet_01v8", "./ota5t"]).to_string(),
                x: grid(rng),
                y: grid(rng),
                orient: Orient::R0,
                name: format!("X{}", rng.below(1000)),
                params,
            }
        }
        1 => {
            let (x, y) = (grid(rng), grid(rng));
            Obj::Wire {
                c: [x, y, x + 10_000 * (rng.below(9) as i32 + 1), y],
            }
        }
        _ => Obj::Label {
            x: grid(rng),
            y: grid(rng),
            net: format!("n{}", rng.below(50)),
        },
    }
}

/// An edit of one object that touches only `unit`: 0 = geometry/name, 1 = params.
fn edit_of(rng: &mut Rng, id: &str, o: &Obj, unit: u8) -> Edit {
    match (o, unit) {
        (Obj::Inst { .. }, 0) => Edit::Move(id.into(), grid(rng), grid(rng)),
        (Obj::Inst { params, .. }, _) => {
            let key = if rng.chance(50) {
                params[0].0.clone()
            } else {
                rng.word(&["nf", "m", "model"]).to_string()
            };
            Edit::Param(id.into(), key, format!("{}", rng.below(100)))
        }
        (Obj::Wire { c }, _) => {
            Edit::MoveWire(id.into(), [c[0], c[1] + 10_000, c[2], c[3] + 10_000])
        }
        (Obj::Label { .. }, _) => Edit::Rename(id.into(), format!("net{}", rng.below(50))),
    }
}

/// A base model and two edit scripts that never touch the same conflict unit (record
/// line, one attribute line, or a whole record for deletes).
fn scenario(seed: u64) -> (Model, Vec<Edit>, Vec<Edit>) {
    let mut rng = Rng::new(seed);
    let mut used = BTreeSet::new();
    let mut o = Model {
        doc: "bench".into(),
        objs: BTreeMap::new(),
    };
    for _ in 0..rng.below(25) {
        let id = new_id(&mut rng, &mut used);
        let obj = new_obj(&mut rng);
        o.objs.insert(id, obj);
    }
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for (id, obj) in &o.objs {
        match rng.below(7) {
            0 | 1 => {}
            2 | 3 => {
                let unit = rng.below(2) as u8;
                let e = edit_of(&mut rng, id, obj, unit);
                (if rng.chance(50) { &mut a } else { &mut b }).push(e);
            }
            4 => (if rng.chance(50) { &mut a } else { &mut b }).push(Edit::Delete(id.clone())),
            5 if matches!(obj, Obj::Inst { .. }) => {
                // Same record, different attribute lines: the driverless killer case.
                a.push(edit_of(&mut rng, id, obj, 0));
                b.push(edit_of(&mut rng, id, obj, 1));
            }
            _ => {
                // The identical change on both sides merges to that change.
                let e = if rng.chance(50) {
                    Edit::Delete(id.clone())
                } else {
                    edit_of(&mut rng, id, obj, 0)
                };
                a.push(e.clone());
                b.push(e);
            }
        }
    }
    for side in [&mut a, &mut b] {
        for _ in 0..rng.below(4) {
            let id = new_id(&mut rng, &mut used);
            side.push(Edit::Insert(id, new_obj(&mut rng)));
        }
    }
    match rng.below(4) {
        0 => a.push(Edit::Doc("ours".into())),
        1 => b.push(Edit::Doc("theirs".into())),
        _ => {}
    }
    (o, a, b)
}

fn applied(m: &Model, edits: &[Edit]) -> Model {
    let mut m = m.clone();
    edits.iter().for_each(|e| apply(&mut m, e));
    m
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn clean_merge_matches_the_naive_model(seed in any::<u64>()) {
        let (o, ea, eb) = scenario(seed);
        let (ma, mb) = (applied(&o, &ea), applied(&o, &eb));
        let expected = text(&applied(&ma, &eb));
        let (to, ta, tb) = (text(&o), text(&ma), text(&mb));
        let (po, pa, pb) = (parse(to.as_bytes()).unwrap(), parse(ta.as_bytes()).unwrap(), parse(tb.as_bytes()).unwrap());
        let ab = merge3(&po, &pa, &pb);
        prop_assert_eq!(ab.conflicts, 0, "\n{}", ab.text);
        prop_assert_eq!(&ab.text, &expected);
        let ba = merge3(&po, &pb, &pa);
        prop_assert_eq!(&ba.text, &ab.text, "not commutative");
    }

    #[test]
    fn merge_with_an_unchanged_side_is_the_other_side(seed in any::<u64>()) {
        let mut rng = Rng::new(seed);
        let kind = if seed.is_multiple_of(2) { FileKind::Shcomp } else { FileKind::Shbench };
        let (to, ta) = (write(&gen_doc(&mut rng, kind)), write(&gen_doc(&mut rng, kind)));
        let (o, a) = (parse(to.as_bytes()).unwrap(), parse(ta.as_bytes()).unwrap());
        for (x, y, z) in [(&o, &a, &o), (&o, &o, &a), (&o, &a, &a), (&a, &a, &a)] {
            let m = merge3(x, y, z);
            prop_assert_eq!(m.conflicts, 0);
            prop_assert_eq!(&m.text, &ta);
        }
    }

    /// The `merge3` fuzz oracle on stable: unrelated and mutated documents (duplicate
    /// ids, unknown records, conflicts everywhere) never panic, and an unchanged side
    /// still yields the other side exactly.
    #[test]
    fn merge_is_total(seed in any::<u64>(), junk in proptest::collection::vec(any::<u8>(), 0..24)) {
        merge_is_total_body(seed, junk);
    }

    #[test]
    fn diff_reports_exactly_the_edited_records(seed in any::<u64>()) {
        let (o, ea, _) = scenario(seed);
        let ma = applied(&o, &ea);
        let (to, ta) = (text(&o), text(&ma));
        let (po, pa) = (parse(to.as_bytes()).unwrap(), parse(ta.as_bytes()).unwrap());
        let d = diff(&po, &pa);
        let changed: BTreeSet<String> = ma.objs.iter().filter(|(id, x)| o.objs.get(*id).is_some_and(|y| y != *x)).map(|(id, _)| id.clone()).collect();
        let added: BTreeSet<String> = ma.objs.keys().filter(|id| !o.objs.contains_key(*id)).cloned().collect();
        let removed: BTreeSet<String> = o.objs.keys().filter(|id| !ma.objs.contains_key(*id)).cloned().collect();
        let of = |kind: ChangeKind| -> BTreeSet<String> {
            d.changes.iter().filter(|c| c.kind == kind && c.a != 0 && c.b != 0).map(|c| {
                let (doc, r) = if c.b != schemify_format::NONE { (&pa, c.b) } else { (&po, c.a) };
                doc.field(r, 0).unwrap().to_string()
            }).collect()
        };
        prop_assert_eq!(of(ChangeKind::Changed), changed);
        prop_assert_eq!(of(ChangeKind::Added), added);
        prop_assert_eq!(of(ChangeKind::Removed), removed);
        prop_assert!(diff(&pa, &pa).changes.is_empty());
    }
}

/// Body of `merge_is_total`, shared with its non-vacuity check.
fn merge_is_total_body(seed: u64, junk: Vec<u8>) {
    let mut rng = Rng::new(seed);
    let kind = if seed.is_multiple_of(2) {
        FileKind::Shcomp
    } else {
        FileKind::Shbench
    };
    let mut texts: Vec<String> = (0..3).map(|_| write(&gen_doc(&mut rng, kind))).collect();
    // Sometimes splice junk into one side; duplicate a wire (same id, W001) in another.
    if seed.is_multiple_of(3) {
        let at = rng.below(texts[1].len() as u64) as usize;
        let at = (0..=at)
            .rev()
            .find(|&i| texts[1].is_char_boundary(i))
            .unwrap_or(0);
        texts[1].insert_str(at, &String::from_utf8_lossy(&junk));
    }
    if let Some(line) = texts[2]
        .lines()
        .find(|l| l.starts_with("wire "))
        .map(str::to_string)
    {
        texts[2] = texts[2].replacen(&line, &format!("{line}\n{line}"), 1);
    }
    let docs: Vec<_> = texts
        .iter()
        .filter_map(|t| parse(t.as_bytes()).ok())
        .collect();
    if docs.len() == 3 {
        PARSED_ALL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (o, a, b) = (&docs[0], &docs[1], &docs[2]);
        let m = merge3(o, a, b);
        assert_eq!(m.conflicts > 0, m.text.contains("\n=======\n"));
        assert_eq!(merge3(o, a, o).text, write(a));
        assert_eq!(merge3(o, o, b).text, write(b));
        assert!(diff(a, a).changes.is_empty());
    }
}

// ---------------------------------------------------------------- conflicts and edge cases

fn p(text: &str) -> Document<'_> {
    parse(text.as_bytes()).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

const INST: &str = "schemify 1 shbench\n\ninst 0abc ./ota\n  at 0 0 R0\n  name X1\n  W 1\n";

#[test]
fn same_attribute_changed_differently_conflicts() {
    let a = INST.replace("W 1", "W 2");
    let b = INST.replace("W 1", "W 3");
    let m = merge3(&p(INST), &p(&a), &p(&b));
    assert_eq!(m.conflicts, 1);
    let ours = "inst 0abc ./ota\n  at 0 0 R0\n  name X1\n  W 2\n";
    let theirs = "inst 0abc ./ota\n  at 0 0 R0\n  name X1\n  W 3\n";
    assert_eq!(
        m.text,
        format!("schemify 1 shbench\n\n<<<<<<< ours\n{ours}=======\n{theirs}>>>>>>> theirs\n")
    );
}

#[test]
fn delete_against_modify_conflicts() {
    let deleted = "schemify 1 shbench\n";
    let moved = INST.replace("at 0 0", "at 10 0");
    let m = merge3(&p(INST), &p(deleted), &p(&moved));
    assert_eq!(m.conflicts, 1);
    assert!(
        m.text.contains("<<<<<<< ours\n=======\ninst 0abc"),
        "{}",
        m.text
    );
    let m = merge3(&p(INST), &p(&moved), &p(deleted));
    assert!(
        m.text.contains("  W 1\n=======\n>>>>>>> theirs\n"),
        "{}",
        m.text
    );
}

#[test]
fn colliding_new_ids_reid_theirs_and_its_references() {
    let o = "schemify 1 shbench\n";
    let a = "schemify 1 shbench\n\nwire 7zzzzz 0 0 10 0\n";
    let b = "schemify 1 shbench\n\ninst 7zzzzz ./x\n  at 0 0 R0\n  name X1\n\nwaive 1wv000 7zzzzz W\n  reason ok\n";
    let m = merge3(&p(o), &p(a), &p(b));
    assert_eq!(m.conflicts, 0, "{}", m.text);
    let merged = p(&m.text);
    let inst = (0..merged.record_count())
        .find(|&r| merged.record_kind(r) == RecordKind::Inst)
        .unwrap();
    let waive = (0..merged.record_count())
        .find(|&r| merged.record_kind(r) == RecordKind::Waive)
        .unwrap();
    let new_id = merged.field(inst, 0).unwrap();
    assert_ne!(new_id, "7zzzzz");
    assert_eq!(
        merged.field(waive, 1),
        Some(new_id),
        "reference follows the re-id"
    );
    assert!(m.text.contains("wire 7zzzzz"));
    assert_eq!(merge3(&p(o), &p(a), &p(b)), m, "re-id is deterministic");
    // Identical additions are one record, not a collision.
    assert_eq!(merge3(&p(o), &p(a), &p(a)).text, a);
}

#[test]
fn graphics_merge_as_one_block() {
    let o = "schemify 1 shprim\n\npin A in 0 0\n\nimpl sp spice\n\nline 0 0 1 1\n\nrect 0 0 2 2\n";
    let a = o.replace("line 0 0 1 1", "line 0 0 1 5");
    let b = o.replace("rect 0 0 2 2", "rect 0 0 2 9");
    let pin = o.replace("pin A in 0 0", "pin A in 5 5");
    assert_eq!(merge3(&p(o), &p(&a), &p(&pin)).conflicts, 0);
    assert_eq!(merge3(&p(o), &p(&a), &p(&a)).text, a);
    let m = merge3(&p(o), &p(&a), &p(&b));
    assert_eq!(m.conflicts, 1);
    assert!(m.text.contains("<<<<<<< ours\nline 0 0 1 5\n\nrect 0 0 2 2\n=======\nline 0 0 1 1\n\nrect 0 0 2 9\n>>>>>>> theirs\n"), "{}", m.text);
}

#[test]
fn concurrent_named_adds_keep_file_order_and_commute() {
    let o = "schemify 1 shprim\n\npin A in 0 0\npin Z in 0 9\n\nimpl sp spice\n";
    let a = o.replace("pin Z", "pin M in 0 5\npin Z");
    let b = o.replace("pin Z", "pin K in 0 4\npin Z");
    let ab = merge3(&p(o), &p(&a), &p(&b));
    let ba = merge3(&p(o), &p(&b), &p(&a));
    assert_eq!(ab.conflicts, 0);
    assert_eq!(ab.text, ba.text);
    assert!(
        ab.text
            .contains("pin A in 0 0\npin K in 0 4\npin M in 0 5\npin Z in 0 9\n"),
        "{}",
        ab.text
    );
    // Both reordering differently is a conflict over the rank.
    let ra = "schemify 1 shprim\n\npin Z in 0 9\npin A in 0 0\n\nimpl sp spice\n";
    let rb = "schemify 1 shprim\n\nimpl sp spice\n\npin A in 0 0\npin Z in 0 9\n";
    assert_eq!(merge3(&p(o), &p(ra), &p(rb)).conflicts, 1);
    // One side reordering wins.
    assert_eq!(merge3(&p(o), &p(ra), &p(o)).text, ra);
}

#[test]
fn concurrent_param_adds_on_one_instance_commute() {
    let a = INST.replace("  W 1\n", "  W 1\n  nf 2\n");
    let b = INST.replace("  W 1\n", "  W 1\n  L 5\n");
    let ab = merge3(&p(INST), &p(&a), &p(&b));
    assert_eq!(ab.conflicts, 0);
    assert_eq!(ab.text, merge3(&p(INST), &p(&b), &p(&a)).text);
    assert!(ab.text.ends_with("  W 1\n  L 5\n  nf 2\n"), "{}", ab.text);
    // Same key, different values: conflict.
    let c = INST.replace("  W 1\n", "  W 1\n  nf 3\n");
    assert_eq!(merge3(&p(INST), &p(&a), &p(&c)).conflicts, 1);
}

#[test]
fn repeatable_attributes_merge_per_first_value() {
    let o = "schemify 1 shbench\n\ncorner tt\n  lib m.lib tt\n  set vdd 1.8\n";
    let a = o.replace("set vdd 1.8", "set vdd 1.8\n  set temp 27");
    let b = o.replace("set vdd 1.8", "set vdd 1.9");
    let m = merge3(&p(o), &p(&a), &p(&b));
    assert_eq!(m.conflicts, 0, "{}", m.text);
    assert!(
        m.text
            .ends_with("  lib m.lib tt\n  set vdd 1.9\n  set temp 27\n"),
        "{}",
        m.text
    );
}

#[test]
fn header_comments_and_eof_merge() {
    let o = "schemify 1 shbench\n  doc d\n\n# note\nwire 0abc 0 0 1 0\n\n# end\n";
    let a = o.replace("doc d", "doc e");
    let b = o
        .replace("  doc d\n", "  doc d\n  category amp\n")
        .replace("# note", "# better note");
    let m = merge3(&p(o), &p(&a), &p(&b));
    assert_eq!(m.conflicts, 0);
    assert_eq!(
        m.text,
        "schemify 1 shbench\n  doc e\n  category amp\n\n# better note\nwire 0abc 0 0 1 0\n\n# end\n"
    );
    let ea = o.replace("# end", "# a");
    let eb = o.replace("# end", "# b");
    let m = merge3(&p(o), &p(&ea), &p(&eb));
    assert_eq!(m.conflicts, 1);
    assert!(
        m.text
            .ends_with("\n<<<<<<< ours\n# a\n=======\n# b\n>>>>>>> theirs\n"),
        "{}",
        m.text
    );
}

#[test]
fn verbatim_blocks_merge_as_a_unit() {
    let o = "schemify 1 shbench\n\ncode ctl spice\n  | .op\n  | .end\n";
    let a = o.replace(".op", ".tran 1n 1u");
    let b = o.replace(".end", ".endc");
    assert_eq!(merge3(&p(o), &p(&a), &p(o)).text, a);
    assert_eq!(merge3(&p(o), &p(&a), &p(&b)).conflicts, 1);
}

#[test]
fn unknown_records_merge_in_newer_versions() {
    let o = "schemify 2 shbench\n\ninst 0abc ./x\n  at 0 0 R0\n  name X1\n\nhologram h1 1\n";
    let a = o.replace("hologram h1 1", "hologram h1 2");
    let b = o
        .replace("name X1", "name X2")
        .replace("hologram h1 1\n", "hologram h1 1\n\nhologram h2 0\n");
    let m = merge3(&p(o), &p(&a), &p(&b));
    assert_eq!(m.conflicts, 0, "{}", m.text);
    assert!(
        m.text
            .ends_with("  name X2\n\nhologram h1 2\nhologram h2 0\n"),
        "{}",
        m.text
    );
}

#[test]
fn duplicate_ids_merge_by_occurrence() {
    let o = "schemify 1 shbench\n\nwire 0abc 0 0 1 0\n\nlabel 0abc 0 0 R0 n\n";
    let a = o.replace("R0 n", "R0 m");
    let m = merge3(&p(o), &p(&a), &p(o));
    assert_eq!((m.conflicts, m.text.as_str()), (0, a.as_str()));
}

#[test]
fn diff_names_changed_attribute_keys() {
    let a = "schemify 1 shbench\n\ninst 0abc ./ota\n  at 0 0 R0\n  name X1\n  W 1\n  L 2\n";
    let b = "schemify 1 shbench\n\ninst 0abc ./ota\n  at 0 0 R0\n  name X1\n  W 3\n  nf 2\n";
    let (pa, pb) = (p(a), p(b));
    let d = diff(&pa, &pb);
    assert_eq!(d.changes.len(), 1);
    let c = &d.changes[0];
    assert_eq!((c.kind, c.parts), (ChangeKind::Changed, Parts::ATTRS));
    assert_eq!(
        d.keys[c.keys.start as usize..c.keys.end as usize],
        ["L", "W", "nf"]
    );
}
