//! Negative space: every code this crate emits, from the request that triggers it,
//! and the guarantee that a rejected transaction changes nothing.

mod common;

use common::*;
use schemify_doc::{
    Code, DocHandle, Edit, Mode, Op, OpError, Outcome, Project, RecordBuf, Target, Transaction,
    ops_from_text,
};
use schemify_format::{FileKind, RecordKind};

const CELL: &str = "schemify 1 shcomp\n  default sch\n\npin A in 0 0\npin B out 10 0\n\nparam W 1\n\nimpl sch schematic\nimpl beh spice\n\nline 0 0 1 1\n\nrect 0 0 2 2\n\ninst 0gsrp0 ./x\n  at 0 0 R0\n  name X1\n\nwire 0d2k1m 0 0 10 0\n\nwaive 1aaaaa 0gsrp0 W\n  reason ok\n\nvar g 1\n\nbody beh\n  | .subckt x\n";

fn rejected(p: &mut Project, h: DocHandle, ops: Vec<Op>) -> Vec<OpError> {
    let before = canon(p, h);
    let out = run(p, h, tx(ops));
    assert_eq!(
        canon(p, h),
        before,
        "a rejected transaction changes nothing"
    );
    match out {
        Outcome::Rejected(e) => e,
        o => panic!("expected a rejection, got {o:?}"),
    }
}

fn first(p: &mut Project, h: DocHandle, ops: Vec<Op>) -> Code {
    rejected(p, h, ops)[0].code
}

fn t(kind: RecordKind, key: &str) -> Target {
    Target::new(kind, key)
}

fn wire(id: &str, c: [&str; 4]) -> RecordBuf {
    RecordBuf::new(RecordKind::Wire)
        .field(id)
        .field(c[0])
        .field(c[1])
        .field(c[2])
        .field(c[3])
}

#[test]
fn every_code_from_its_trigger() {
    let (mut p, h) = open(CELL, "x.shcomp");
    let ins = |rec: RecordBuf| Op::Insert { rec, at: None };
    // E022: a coordinate needing a fourth fractional digit.
    let e = first(
        &mut p,
        h,
        vec![Op::SetField {
            target: t(RecordKind::Wire, "0d2k1m"),
            index: 1,
            value: "0.0005".into(),
        }],
    );
    assert_eq!(e, Code::E022);
    // E024: a name taken, by insert and by rename; an override given twice.
    let pin = RecordBuf::new(RecordKind::Pin)
        .field("A")
        .field("in")
        .field("5")
        .field("5");
    assert_eq!(first(&mut p, h, vec![ins(pin)]), Code::E024);
    assert_eq!(
        first(
            &mut p,
            h,
            vec![Op::SetField {
                target: t(RecordKind::Pin, "B"),
                index: 0,
                value: "A".into()
            }]
        ),
        Code::E024
    );
    let twice = RecordBuf::new(RecordKind::Inst)
        .field("2bbbbb")
        .field("./x")
        .attr("at", &["1", "1", "R0"])
        .attr("name", &["X2"])
        .attr("W", &["1"])
        .attr("W", &["2"]);
    assert_eq!(first(&mut p, h, vec![ins(twice)]), Code::E024);
    // E025: removing the only schematic impl of a .shcomp; a second schematic impl.
    let del_sch = Op::Delete(t(RecordKind::Impl, "sch"));
    let e = rejected(&mut p, h, vec![del_sch.clone()]);
    assert!(e.iter().any(|x| x.code == Code::E025), "{e:?}");
    let sch2 = RecordBuf::new(RecordKind::Impl)
        .field("sch2")
        .field("schematic");
    assert_eq!(first(&mut p, h, vec![ins(sch2)]), Code::E025);
    // ... but a transaction may pass through an invalid state.
    let new_sch = RecordBuf::new(RecordKind::Impl)
        .field("sch")
        .field("schematic");
    assert!(matches!(
        run(&mut p, h, tx(vec![del_sch, ins(new_sch)])),
        Outcome::Committed { .. }
    ));
    // E026: a zero-length wire at the end of the transaction (not in between).
    assert_eq!(
        first(&mut p, h, vec![ins(wire("3ccccc", ["5", "5", "5", "5"]))]),
        Code::E026
    );
    let via_zero = vec![
        Op::SetField {
            target: t(RecordKind::Wire, "0d2k1m"),
            index: 3,
            value: "0".into(),
        },
        Op::SetField {
            target: t(RecordKind::Wire, "0d2k1m"),
            index: 4,
            value: "7".into(),
        },
    ];
    assert!(matches!(
        run(&mut p, h, tx(via_zero)),
        Outcome::Committed { .. }
    ));
    // E027: a kind the file kind does not allow.
    let corner = RecordBuf::new(RecordKind::Corner).field("tt");
    assert_eq!(first(&mut p, h, vec![ins(corner)]), Code::E027);
    // E028: an attribute key that is not a word; a comment with trailing whitespace.
    let bad_key = Op::SetAttr {
        target: t(RecordKind::Inst, "0gsrp0"),
        key: "2W".into(),
        values: vec![],
    };
    assert_eq!(first(&mut p, h, vec![bad_key]), Code::E028);
    let mut commented = wire("4ddddd", ["0", "0", "1", "0"]);
    commented.comments.push("trailing ".into());
    assert_eq!(first(&mut p, h, vec![ins(commented)]), Code::E028);
    // E029: removing a required attribute; a value of the wrong form.
    let no_at = Op::RemoveAttr {
        target: t(RecordKind::Inst, "0gsrp0"),
        key: "at".into(),
        first: None,
    };
    assert_eq!(first(&mut p, h, vec![no_at]), Code::E029);
    let bad_dir = Op::SetField {
        target: t(RecordKind::Pin, "A"),
        index: 1,
        value: "sideways".into(),
    };
    assert_eq!(first(&mut p, h, vec![bad_dir]), Code::E029);
    let no_reason = Op::RemoveAttr {
        target: t(RecordKind::Waive, "1aaaaa"),
        key: "reason".into(),
        first: None,
    };
    assert_eq!(first(&mut p, h, vec![no_reason]), Code::E029);
    // E040: a missing target; removing an attribute that is not there.
    assert_eq!(
        first(&mut p, h, vec![Op::Delete(t(RecordKind::Wire, "zzzzzz"))]),
        Code::E040
    );
    assert_eq!(
        first(&mut p, h, vec![Op::Delete(t(RecordKind::Line, "7"))]),
        Code::E040
    );
    assert_eq!(
        first(&mut p, h, vec![Op::Delete(t(RecordKind::Line, "1"))]),
        Code::E040,
        "row 1 is a rect"
    );
    let absent = Op::RemoveAttr {
        target: t(RecordKind::Inst, "0gsrp0"),
        key: "L".into(),
        first: None,
    };
    assert_eq!(first(&mut p, h, vec![absent]), Code::E040);
    // E041: an id in use, by any kind.
    assert_eq!(
        first(&mut p, h, vec![ins(wire("0gsrp0", ["0", "0", "1", "0"]))]),
        Code::E041
    );
    // E042: deleting a waived instance; a waiver for a missing instance; dropping the
    // impl a body or the header default names.
    assert_eq!(
        first(&mut p, h, vec![Op::Delete(t(RecordKind::Inst, "0gsrp0"))]),
        Code::E042
    );
    let orphan = RecordBuf::new(RecordKind::Waive)
        .field("5eeeee")
        .field("zzzzzz")
        .field("W")
        .attr("reason", &["x"]);
    assert_eq!(first(&mut p, h, vec![ins(orphan)]), Code::E042);
    assert_eq!(
        first(&mut p, h, vec![Op::Delete(t(RecordKind::Impl, "beh"))]),
        Code::E042
    );
    let rename = Op::SetField {
        target: t(RecordKind::Impl, "beh"),
        index: 0,
        value: "beh2".into(),
    };
    assert_eq!(first(&mut p, h, vec![rename.clone()]), Code::E042);
    // ... unless the same transaction fixes the references.
    let fix_body = Op::SetField {
        target: t(RecordKind::Body, "beh"),
        index: 0,
        value: "beh2".into(),
    };
    let fix_default = Op::SetAttr {
        target: Target::header(),
        key: "default".into(),
        values: vec!["sch".into()],
    };
    assert!(matches!(
        run(&mut p, h, tx(vec![rename, fix_body, fix_default])),
        Outcome::Committed { .. }
    ));
    // E043: ids are immutable; reorder or a position in an unordered table; the header
    // is not deleted or inserted; a position past the end.
    let re_id = Op::SetField {
        target: t(RecordKind::Wire, "0d2k1m"),
        index: 0,
        value: "9fffff".into(),
    };
    assert_eq!(first(&mut p, h, vec![re_id]), Code::E043);
    assert_eq!(
        first(
            &mut p,
            h,
            vec![Op::Reorder {
                target: t(RecordKind::Wire, "0d2k1m"),
                to: 0
            }]
        ),
        Code::E043
    );
    assert_eq!(
        first(
            &mut p,
            h,
            vec![Op::Insert {
                rec: wire("6ggggg", ["0", "0", "1", "0"]),
                at: Some(0)
            }]
        ),
        Code::E043
    );
    assert_eq!(
        first(&mut p, h, vec![Op::Delete(Target::header())]),
        Code::E043
    );
    assert_eq!(
        first(
            &mut p,
            h,
            vec![Op::Insert {
                rec: RecordBuf::new(RecordKind::Header),
                at: None
            }]
        ),
        Code::E043
    );
    assert_eq!(
        first(
            &mut p,
            h,
            vec![Op::Reorder {
                target: t(RecordKind::Pin, "A"),
                to: 2
            }]
        ),
        Code::E043
    );
    let put_re_id = Op::Put {
        target: t(RecordKind::Wire, "0d2k1m"),
        rec: wire("7hhhhh", ["0", "0", "1", "0"]),
    };
    assert_eq!(first(&mut p, h, vec![put_re_id]), Code::E043);
    assert_eq!(
        first(
            &mut p,
            h,
            vec![Op::SetField {
                target: t(RecordKind::Var, "g"),
                index: 2,
                value: "x".into()
            }]
        ),
        Code::E043
    );
}

#[test]
fn errors_list_every_failing_op_with_its_index() {
    let (mut p, h) = open(CELL, "x.shcomp");
    let ops = vec![
        Op::Delete(t(RecordKind::Wire, "zzzzzz")),
        Op::Insert {
            rec: wire("8jjjjj", ["0", "0", "1", "0"]),
            at: None,
        },
        Op::Reorder {
            target: t(RecordKind::Wire, "0d2k1m"),
            to: 0,
        },
        Op::Insert {
            rec: wire("8jjjjj", ["0", "0", "2", "0"]),
            at: None,
        },
    ];
    let e = rejected(&mut p, h, ops);
    assert_eq!(
        e,
        [
            OpError {
                op: 0,
                code: Code::E040
            },
            OpError {
                op: 2,
                code: Code::E043
            },
            OpError {
                op: 3,
                code: Code::E041
            },
        ]
    );
}

#[test]
fn refusals_for_stale_handles_stages_and_states() {
    let (mut p, h) = open(CELL, "x.shcomp");
    assert_eq!(
        run(&mut p, h, Edit::Accept(9)),
        Outcome::Refused(Code::E040)
    );
    assert_eq!(
        run(&mut p, h, Edit::Reject(9)),
        Outcome::Refused(Code::E040)
    );
    assert_eq!(run(&mut p, h, Edit::Undo), Outcome::Nothing);
    assert!(p.close(h).is_ok());
    assert_eq!(
        run(&mut p, h, Edit::Undo),
        Outcome::Refused(Code::E040),
        "stale handle"
    );
    assert_eq!(p.save(h), Err(Code::E040));
    assert!(p.doc(h).is_none());
    let h2 = p
        .open("", "x.shcomp", CELL.as_bytes(), None)
        .unwrap()
        .handle;
    assert_eq!(h2.index, h.index, "the slot is reused");
    assert_ne!(h2.generation, h.generation, "with a new generation");
    // At most 2^8 open stages.
    let stage = |i: u32| {
        Edit::Tx(Transaction {
            label: format!("s{i}"),
            mode: Mode::Stage,
            ops: vec![Op::SetAttr {
                target: Target::header(),
                key: "doc".into(),
                values: vec![i.to_string()],
            }],
        })
    };
    for i in 0..256 {
        assert!(matches!(run(&mut p, h2, stage(i)), Outcome::Staged { .. }));
    }
    assert_eq!(run(&mut p, h2, stage(256)), Outcome::Refused(Code::E043));
    assert_eq!(canon(&p, h2), CELL, "stages do not apply");
    // A created document is invalid until it has its kind's records: saving says so.
    let c = p.create("", "new.shprim", FileKind::Shprim).unwrap();
    assert!(p.is_dirty(c));
    assert_eq!(p.save(c), Err(Code::E025));
    assert_eq!(
        p.create("", "x.project", FileKind::Project),
        Err(Code::E027)
    );
}

#[test]
fn op_text_errors_carry_op_indices() {
    assert_eq!(
        ops_from_text("delete wire 0d2k1m\nfrob x y\n", FileKind::Shcomp),
        Err(vec![OpError {
            op: 1,
            code: Code::E027
        }])
    );
    let e = ops_from_text("delete wire \"0d2k1m\n", FileKind::Shcomp).unwrap_err();
    assert_eq!(e[0].code, Code::E028);
}
