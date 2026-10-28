//! docs/dev/testing.md `merge3` oracles: `merge(O, A, O) == A`, `merge(O, O, B) == B`,
//! non-conflicting edits commute, and model-based PBT over random edit scripts (move,
//! param change, insert, delete, label rename) against a naive model; plus `diff`.

mod common;

use common::Rng;
use common::docgen;
use proptest::prelude::*;
use schemify_format as format;
use schemify_format::{ChangeKind, Document, FileKind, IdRng, Orient, RecordKind};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU32, Ordering};

/// Cases of `merge_is_total` where all three sides parsed (checked to be non-vacuous).
static PARSED_ALL: AtomicU32 = AtomicU32::new(0);

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

// ---- private ----

#[test]
fn merge_is_total_is_not_vacuous() {
    // proptest runs the cases of `merge_is_total` in its own test; run a few here.
    let before = PARSED_ALL.load(Ordering::Relaxed);
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
    let hits = PARSED_ALL.load(Ordering::Relaxed) - before;
    assert!(hits >= 64, "only {hits}/256 cases parsed all three sides");
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
    format::write(&render(m))
}

fn grid(rng: &mut Rng) -> i32 {
    (rng.below(100) as i32 - 50) * 10_000
}

fn new_id(rng: &mut Rng, used: &mut BTreeSet<String>) -> String {
    let id = format::fresh_id(&mut IdRng::new(rng.next()), |c| used.contains(c))
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
    for e in edits {
        apply(&mut m, e);
    }
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
        let (po, pa, pb) = (format::parse(to.as_bytes()).unwrap(), format::parse(ta.as_bytes()).unwrap(), format::parse(tb.as_bytes()).unwrap());
        let ab = format::merge3(&po, &pa, &pb);
        prop_assert_eq!(ab.conflicts, 0, "\n{}", ab.text);
        prop_assert_eq!(&ab.text, &expected);
        let ba = format::merge3(&po, &pb, &pa);
        prop_assert_eq!(&ba.text, &ab.text, "not commutative");
    }

    #[test]
    fn merge_with_an_unchanged_side_is_the_other_side(seed in any::<u64>()) {
        let mut rng = Rng::new(seed);
        let kind = if seed.is_multiple_of(2) { FileKind::Shcomp } else { FileKind::Shbench };
        let (to, ta) = (format::write(&docgen::gen_doc(&mut rng, kind)), format::write(&docgen::gen_doc(&mut rng, kind)));
        let (o, a) = (format::parse(to.as_bytes()).unwrap(), format::parse(ta.as_bytes()).unwrap());
        for (x, y, z) in [(&o, &a, &o), (&o, &o, &a), (&o, &a, &a), (&a, &a, &a)] {
            let m = format::merge3(x, y, z);
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
        let (po, pa) = (format::parse(to.as_bytes()).unwrap(), format::parse(ta.as_bytes()).unwrap());
        let d = format::diff(&po, &pa);
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
        prop_assert!(format::diff(&pa, &pa).changes.is_empty());
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
    let mut texts: Vec<String> = (0..3)
        .map(|_| format::write(&docgen::gen_doc(&mut rng, kind)))
        .collect();
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
        .filter_map(|t| format::parse(t.as_bytes()).ok())
        .collect();
    if docs.len() == 3 {
        PARSED_ALL.fetch_add(1, Ordering::Relaxed);
        let (o, a, b) = (&docs[0], &docs[1], &docs[2]);
        let m = format::merge3(o, a, b);
        assert_eq!(m.conflicts > 0, m.text.contains("\n=======\n"));
        assert_eq!(format::merge3(o, a, o).text, format::write(a));
        assert_eq!(format::merge3(o, o, b).text, format::write(b));
        assert!(format::diff(a, a).changes.is_empty());
    }
}
