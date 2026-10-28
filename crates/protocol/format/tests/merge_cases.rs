//! docs/dev/testing.md `merge3` oracles, case by case: conflicts (same attribute,
//! delete against modify, verbatim runs, kind rules), id collisions and re-ids, graphics
//! blocks, order merges, unknown records, and diff3 over verbatim lines.

mod common;

use common::Rng;
use proptest::prelude::*;
use schemify_format as format;
use schemify_format::{ChangeKind, Document, Parts, RecordKind};

const INST: &str = "schemify 1 shbench\n\ninst 0abc ./ota\n  at 0 0 R0\n  name X1\n  W 1\n";

// ---- private ----

fn p(text: &str) -> Document<'_> {
    format::parse(text.as_bytes()).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

#[test]
fn same_attribute_changed_differently_conflicts() {
    let a = INST.replace("W 1", "W 2");
    let b = INST.replace("W 1", "W 3");
    let m = format::merge3(&p(INST), &p(&a), &p(&b));
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
    let m = format::merge3(&p(INST), &p(deleted), &p(&moved));
    assert_eq!(m.conflicts, 1);
    assert!(
        m.text.contains("<<<<<<< ours\n=======\ninst 0abc"),
        "{}",
        m.text
    );
    let m = format::merge3(&p(INST), &p(&moved), &p(deleted));
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
    let m = format::merge3(&p(o), &p(a), &p(b));
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
    assert_eq!(
        format::merge3(&p(o), &p(a), &p(b)),
        m,
        "re-id is deterministic"
    );
    // Identical additions are one record, not a collision.
    assert_eq!(format::merge3(&p(o), &p(a), &p(a)).text, a);
}

#[test]
fn graphics_merge_as_one_block() {
    let o = "schemify 1 shprim\n\npin A in 0 0\n\nimpl sp spice\n\nline 0 0 1 1\n\nrect 0 0 2 2\n";
    let a = o.replace("line 0 0 1 1", "line 0 0 1 5");
    let b = o.replace("rect 0 0 2 2", "rect 0 0 2 9");
    let pin = o.replace("pin A in 0 0", "pin A in 5 5");
    assert_eq!(format::merge3(&p(o), &p(&a), &p(&pin)).conflicts, 0);
    assert_eq!(format::merge3(&p(o), &p(&a), &p(&a)).text, a);
    let m = format::merge3(&p(o), &p(&a), &p(&b));
    assert_eq!(m.conflicts, 1);
    assert!(m.text.contains("<<<<<<< ours\nline 0 0 1 5\n\nrect 0 0 2 2\n=======\nline 0 0 1 1\n\nrect 0 0 2 9\n>>>>>>> theirs\n"), "{}", m.text);
}

#[test]
fn concurrent_named_adds_keep_file_order_and_commute() {
    let o = "schemify 1 shprim\n\npin A in 0 0\npin Z in 0 9\n\nimpl sp spice\n";
    let a = o.replace("pin Z", "pin M in 0 5\npin Z");
    let b = o.replace("pin Z", "pin K in 0 4\npin Z");
    let ab = format::merge3(&p(o), &p(&a), &p(&b));
    let ba = format::merge3(&p(o), &p(&b), &p(&a));
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
    assert_eq!(format::merge3(&p(o), &p(ra), &p(rb)).conflicts, 1);
    // One side reordering wins.
    assert_eq!(format::merge3(&p(o), &p(ra), &p(o)).text, ra);
}

#[test]
fn concurrent_param_adds_on_one_instance_commute() {
    let a = INST.replace("  W 1\n", "  W 1\n  nf 2\n");
    let b = INST.replace("  W 1\n", "  W 1\n  L 5\n");
    let ab = format::merge3(&p(INST), &p(&a), &p(&b));
    assert_eq!(ab.conflicts, 0);
    assert_eq!(ab.text, format::merge3(&p(INST), &p(&b), &p(&a)).text);
    assert!(ab.text.ends_with("  W 1\n  L 5\n  nf 2\n"), "{}", ab.text);
    // Same key, different values: conflict.
    let c = INST.replace("  W 1\n", "  W 1\n  nf 3\n");
    assert_eq!(format::merge3(&p(INST), &p(&a), &p(&c)).conflicts, 1);
}

#[test]
fn repeatable_attributes_merge_per_first_value() {
    let o = "schemify 1 shbench\n\ncorner tt\n  lib m.lib tt\n  set vdd 1.8\n";
    let a = o.replace("set vdd 1.8", "set vdd 1.8\n  set temp 27");
    let b = o.replace("set vdd 1.8", "set vdd 1.9");
    let m = format::merge3(&p(o), &p(&a), &p(&b));
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
    let m = format::merge3(&p(o), &p(&a), &p(&b));
    assert_eq!(m.conflicts, 0);
    assert_eq!(
        m.text,
        "schemify 1 shbench\n  doc e\n  category amp\n\n# better note\nwire 0abc 0 0 1 0\n\n# end\n"
    );
    let ea = o.replace("# end", "# a");
    let eb = o.replace("# end", "# b");
    let m = format::merge3(&p(o), &p(&ea), &p(&eb));
    assert_eq!(m.conflicts, 1);
    assert!(
        m.text
            .ends_with("\n<<<<<<< ours\n# a\n=======\n# b\n>>>>>>> theirs\n"),
        "{}",
        m.text
    );
}

#[test]
fn adjacent_verbatim_line_changes_conflict() {
    // No line both sides keep separates the two changes: one changed run, changed twice.
    let o = "schemify 1 shbench\n\ncode ctl spice\n  | .op\n  | .end\n";
    let a = o.replace(".op", ".tran 1n 1u");
    let b = o.replace(".end", ".endc");
    assert_eq!(format::merge3(&p(o), &p(&a), &p(o)).text, a);
    assert_eq!(format::merge3(&p(o), &p(&a), &p(&b)).conflicts, 1);
}

#[test]
fn verbatim_lines_merge_line_by_line() {
    let o = "schemify 1 shbench\n\ncode ctl spice\n  | .op\n  | * keep\n  | .end\n";
    let a = o.replace(".op", ".tran 1n 1u");
    let b = o.replace(".end", ".endc");
    let m = format::merge3(&p(o), &p(&a), &p(&b));
    assert_eq!(m.conflicts, 0, "{}", m.text);
    assert_eq!(m.text, a.replace(".end", ".endc"));
    assert_eq!(format::merge3(&p(o), &p(&b), &p(&a)).text, m.text);
    // An insertion and a deletion in different runs, and the same change on both sides.
    let ins = o.replace("  | .op\n", "  | .op\n  | .save all\n");
    let del = o.replace("  | .end\n", "");
    let m = format::merge3(&p(o), &p(&ins), &p(&del));
    assert_eq!(m.conflicts, 0, "{}", m.text);
    assert_eq!(m.text, ins.replace("  | .end\n", ""));
    assert_eq!(format::merge3(&p(o), &p(&a), &p(&a)).text, a);
    // Negative space: one line changed differently, or different lines inserted at one
    // place, conflicts; the same insertion on both sides is taken once.
    let mine = o.replace("* keep", "* mine");
    let yours = o.replace("* keep", "* yours");
    assert_eq!(format::merge3(&p(o), &p(&mine), &p(&yours)).conflicts, 1);
    let other = o.replace("  | .op\n", "  | .op\n  | .save v(out)\n");
    assert_eq!(format::merge3(&p(o), &p(&ins), &p(&other)).conflicts, 1);
    let both = format!("{}  | .print\n", ins.trim_end_matches("  | .end\n"));
    let both = format!("{both}  | .end\n");
    let m = format::merge3(&p(o), &p(&ins), &p(&both));
    assert_eq!((m.conflicts, m.text.as_str()), (0, both.as_str()));
}

/// Edits a run of base lines (keep, delete, replace, insert before), writing fresh
/// lines `<tag><n>` that equal no base line and no line of the other side.
fn edit_lines(rng: &mut Rng, lines: &[String], tag: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut fresh = 0;
    let mut new_line = |out: &mut Vec<String>| {
        fresh += 1;
        out.push(format!("{tag}{fresh}"));
    };
    for line in lines {
        match rng.below(6) {
            0 => {}                  // delete
            1 => new_line(&mut out), // replace
            2 => {
                new_line(&mut out); // insert before
                out.push(line.clone());
            }
            _ => out.push(line.clone()),
        }
    }
    if rng.chance(20) {
        new_line(&mut out); // insert at the end of the run
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// diff3 oracle: edits of `a` and `b` separated by a base line both keep merge
    /// cleanly to "both edits applied", symmetrically.
    #[test]
    fn separated_verbatim_edits_merge_cleanly_and_commute(seed: u64, n in 1usize..40, cut in 0usize..40) {
        let mut rng = Rng::new(seed);
        let base: Vec<String> = (0..n).map(|i| format!("o{i}")).collect();
        let k = cut % n;
        let ea = edit_lines(&mut rng, &base[..k], 'a');
        let eb = edit_lines(&mut rng, &base[k + 1..], 'b');
        let join = |head: &[String], tail: &[String]| {
            let mut v = head.to_vec();
            v.push(base[k].clone());
            v.extend_from_slice(tail);
            v
        };
        let doc = |lines: &[String]| {
            let mut t = String::from("schemify 1 shbench\n\nprose 0abc\n");
            for l in lines {
                t.push_str("  | ");
                t.push_str(l);
                t.push('\n');
            }
            t
        };
        let (to, ta, tb) = (doc(&base), doc(&join(&ea, &base[k + 1..])), doc(&join(&base[..k], &eb)));
        let (po, pa, pb) = (p(&to), p(&ta), p(&tb));
        let m = format::merge3(&po, &pa, &pb);
        prop_assert_eq!(m.conflicts, 0, "{}", m.text);
        prop_assert_eq!(&m.text, &doc(&join(&ea, &eb)));
        prop_assert_eq!(&format::merge3(&po, &pb, &pa).text, &m.text);
    }
}

#[test]
fn a_clean_merge_that_breaks_kind_rules_conflicts_on_the_interface() {
    // Each side replaces the schematic impl: both results have one, the merge has two.
    let o = "schemify 1 shcomp\n\npin A in 0 0\n\nimpl sch schematic\n";
    let a = o.replace("impl sch", "impl main");
    let b = o.replace("impl sch", "impl top");
    let m = format::merge3(&p(o), &p(&a), &p(&b));
    assert_eq!(m.conflicts, 1, "{}", m.text);
    assert_eq!(
        m.text,
        "schemify 1 shcomp\n\n<<<<<<< ours\npin A in 0 0\n\nimpl main schematic\n=======\npin A in 0 0\n\nimpl top schematic\n>>>>>>> theirs\n"
    );
    assert_eq!(format::merge3(&p(o), &p(&b), &p(&a)).conflicts, 1);
    // Each side deletes the other's last-but-one pin: no pin is left.
    let o = "schemify 1 shprim\n\npin A in 0 0\npin B out 1 0\n\nimpl s spice\n";
    let a = o.replace("pin A in 0 0\n", "");
    let b = o.replace("pin B out 1 0\n", "");
    let m = format::merge3(&p(o), &p(&a), &p(&b));
    assert_eq!(m.conflicts, 1, "{}", m.text);
    assert!(
        m.text.contains("<<<<<<< ours\npin B out 1 0\n"),
        "{}",
        m.text
    );
    // Positive space: rank-1 changes that keep the rules stay clean.
    let c = o.replace("pin B out 1 0\n", "pin B out 1 0\npin C out 2 0\n");
    let m = format::merge3(&p(o), &p(&a), &p(&c));
    assert_eq!(m.conflicts, 0, "{}", m.text);
    assert!(format::parse(m.text.as_bytes()).is_ok());
}

#[test]
fn unknown_records_merge_in_newer_versions() {
    let o = "schemify 2 shbench\n\ninst 0abc ./x\n  at 0 0 R0\n  name X1\n\nhologram h1 1\n";
    let a = o.replace("hologram h1 1", "hologram h1 2");
    let b = o
        .replace("name X1", "name X2")
        .replace("hologram h1 1\n", "hologram h1 1\n\nhologram h2 0\n");
    let m = format::merge3(&p(o), &p(&a), &p(&b));
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
    let m = format::merge3(&p(o), &p(&a), &p(o));
    assert_eq!((m.conflicts, m.text.as_str()), (0, a.as_str()));
}

#[test]
fn an_added_id_duplicated_in_theirs_is_re_ided_once() {
    // Fuzz crash ded040dc: both sides add id 0abc, theirs twice (W001). Theirs' records
    // are renamed together, ours is kept, and the merge is clean.
    let o = "schemify 1 shbench\n";
    let a = "schemify 1 shbench\n\nwire 0abc 0 0 1 0\n";
    let b = "schemify 1 shbench\n\nwire 0abc 0 0 2 0\n\nlabel 0abc 0 0 R0 n\n";
    let m = format::merge3(&p(o), &p(a), &p(b));
    assert_eq!(m.conflicts, 0, "{}", m.text);
    let merged = p(&m.text);
    assert_eq!(merged.record_count(), 4);
    assert_eq!(
        merged.warnings().len(),
        1,
        "theirs' two records share the new id"
    );
    assert!(m.text.contains("wire 0abc 0 0 1 0\n"), "{}", m.text);
    assert!(!m.text.contains("label 0abc"), "{}", m.text);
}

#[test]
fn diff_names_changed_attribute_keys() {
    let a = "schemify 1 shbench\n\ninst 0abc ./ota\n  at 0 0 R0\n  name X1\n  W 1\n  L 2\n";
    let b = "schemify 1 shbench\n\ninst 0abc ./ota\n  at 0 0 R0\n  name X1\n  W 3\n  nf 2\n";
    let (pa, pb) = (p(a), p(b));
    let d = format::diff(&pa, &pb);
    assert_eq!(d.changes.len(), 1);
    let c = &d.changes[0];
    assert_eq!((c.kind, c.parts), (ChangeKind::Changed, Parts::ATTRS));
    assert_eq!(
        d.keys[c.keys.start as usize..c.keys.end as usize],
        ["L", "W", "nf"]
    );
}
