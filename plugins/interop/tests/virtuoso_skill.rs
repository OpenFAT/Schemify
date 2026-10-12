//! The shipped SKILL (`interop::skill`) through the test interpreter and mock database
//! (spec `interop.md` §5).

mod skill;

use skill::mockdb::MockDb;
use skill::{Interp, V, read_all};

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
}
