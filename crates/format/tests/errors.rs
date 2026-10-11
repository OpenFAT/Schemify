//! Negative space: every diagnostic code the reader declares, with its position.
//! (`E021` is tested in `reader.rs` with a small limit; a 1 GiB fixture is not.)

use schemify_format::{Code, NO_RECORD, parse};

/// Asserts `text` fails with `code` at `line:col` (col 0 = not checked) and that
/// every reported diagnostic is an error.
fn fails(text: &str, code: Code, line: u32, col: u32) {
    let err = parse(text.as_bytes()).expect_err(text);
    assert!(err.diagnostics.iter().all(|d| !d.code.is_warning()));
    let hit = err
        .diagnostics
        .iter()
        .find(|d| d.code == code)
        .unwrap_or_else(|| panic!("no {code:?} in:\n{err}\nfor:\n{text}"));
    assert_eq!(hit.line, line, "{err}");
    if col != 0 {
        assert_eq!(hit.col, col, "{err}");
    }
}

const BENCH: &str = "schemify 1 shbench\n";

#[test]
fn e022_coordinates() {
    fails(
        "schemify 1 shbench\nwire 0abc 0 0 1.0005 0\n",
        Code::E022,
        2,
        15,
    );
    fails(
        "schemify 1 shbench\nnoconn 0abc 2147484 0\n",
        Code::E022,
        2,
        13,
    );
    fails(
        "schemify 1 shbench\ninst 0abc ./x\n  at 0.1234 0 R0\n  name X\n",
        Code::E022,
        3,
        6,
    );
    assert!(parse(b"schemify 1 shbench\nnoconn 0abc 2147483.647 -2147483.648\n").is_ok());
    assert!(parse(b"schemify 1 shbench\nnoconn 0abc 1.5000 0\n").is_ok());
}

#[test]
fn e023_rank_order() {
    fails(
        "schemify 1 shcomp\nimpl s schematic\ninst 0abc ./x\n  at 0 0 R0\n  name X\npin A in 0 0\n",
        Code::E023,
        6,
        1,
    );
    // Any order inside a rank is fine.
    assert!(
        parse(b"schemify 1 shbench\nwire 1abc 0 0 1 0\ninst 0abc ./x\n  at 0 0 R0\n  name X\n")
            .is_ok()
    );
}

#[test]
fn e024_duplicate_names_and_attributes() {
    fails(
        "schemify 1 shprim\npin A in 0 0\npin A out 1 1\nimpl s spice\n",
        Code::E024,
        3,
        1,
    );
    fails(
        "schemify 1 shbench\ninst 0abc ./x\n  at 0 0 R0\n  at 1 1 R0\n  name X\n",
        Code::E024,
        4,
        3,
    );
    fails(
        "schemify 1 lock\nlocked lib a git x 0\nlocked lib a git y 1\n",
        Code::E024,
        3,
        1,
    );
    // Same name in different kinds, and repeatable attributes, are fine.
    assert!(
        parse(b"schemify 1 shbench\nvar W 1\noutput W 1\ncorner tt\n  lib m tt\n  lib m ff\n")
            .is_ok()
    );
    assert!(parse(b"schemify 1 lock\nlocked lib a git x 0\nlocked plugin a git x 0\n").is_ok());
}

#[test]
fn e025_kind_rules() {
    fails("schemify 1 shcomp\npin A in 0 0\n", Code::E025, 1, 1);
    fails(
        "schemify 1 shcomp\nimpl a schematic\nimpl b schematic\n",
        Code::E025,
        1,
        1,
    );
    fails(
        "schemify 1 shprim\npin A in 0 0\nimpl s schematic\n",
        Code::E025,
        1,
        1,
    );
    fails("schemify 1 shprim\nimpl s spice\n", Code::E025, 1, 1);
    fails("schemify 1 shprim\npin A in 0 0\n", Code::E025, 1, 1);
    fails(
        "schemify 1 shprim\npin A in 0 0\nimpl s spice\nimpl v veriloga\n",
        Code::E025,
        1,
        1,
    );
    assert!(
        parse(b"schemify 1 shprim\n  default s\npin A in 0 0\nimpl s spice\nimpl v veriloga\n")
            .is_ok()
    );
    assert!(parse(b"schemify 1 shcomp\nimpl s schematic\nimpl v spice\n  format spice x\n  format cdl y\n  default s\n").is_err());
}

#[test]
fn e026_zero_length_wire() {
    fails(
        "schemify 1 shbench\nwire 0abc 5 5 5.000 5\n",
        Code::E026,
        2,
        1,
    );
    assert!(parse(b"schemify 1 shbench\nwire 0abc 5 5 5 5.001\n").is_ok());
}

#[test]
fn e027_unknown_record() {
    fails(
        "schemify 1 shbench\nhologram 1 2\n  knob 3\n",
        Code::E027,
        2,
        1,
    );
    fails("schemify 1 shbench\npin A in 0 0\n", Code::E027, 2, 1); // known word, wrong file kind
    fails("schemify 1 config\ninst 0abc ./x\n", Code::E027, 2, 1);
    // Its attribute lines are skipped, not reported again or attached elsewhere.
    let err = parse(b"schemify 1 shbench\nhologram 1 2\n  knob 3\n  | x\n").unwrap_err();
    assert_eq!(err.diagnostics.len(), 1);
    assert!(parse(b"schemify 2 shbench\nhologram 1 2\n  knob 3\n").is_ok());
}

#[test]
fn e028_lexical() {
    fails("\u{feff}schemify 1 shbench\n", Code::E028, 1, 1);
    fails("", Code::E028, 1, 1);
    fails("schematic 1 shbench\n", Code::E028, 1, 1);
    fails("schemify one shbench\n", Code::E028, 1, 10);
    fails("schemify 0 shbench\n", Code::E028, 1, 10);
    fails("schemify 1 shthing\n", Code::E028, 1, 12);
    fails("schemify 1 shbench extra\n", Code::E028, 1, 20);
    fails("schemify1 shbench\n", Code::E028, 1, 1);
    fails("schemify 1 shbench\nschemify 1 shbench\n", Code::E028, 2, 1);
    fails("schemify 1 shbench\n1wire 0abc 0 0 1 0\n", Code::E028, 2, 1);
    fails("schemify 1 shbench\nwi$re 0abc 0 0 1 0\n", Code::E028, 2, 3);
    fails(
        "schemify 1 shbench\nvar W \"unterminated\n",
        Code::E028,
        2,
        7,
    );
    fails(
        "schemify 1 shbench\nvar W \"bad \\q escape\"\n",
        Code::E028,
        2,
        12,
    );
    fails(
        "schemify 1 shbench\nvar W \"\\u{d800}\"\n",
        Code::E028,
        2,
        8,
    );
    fails(
        "schemify 1 shbench\nvar W \"\\u{1234567}\"\n",
        Code::E028,
        2,
        8,
    );
    fails("schemify 1 shbench\nvar W \"a\"b\n", Code::E028, 2, 10);
    fails("schemify 1 shbench\nvar W a\"b\n", Code::E028, 2, 8);
    fails("schemify 1 shbench\nvar W #x\n", Code::E028, 2, 7);
    fails("schemify 1 shbench\nvar W |x\n", Code::E028, 2, 7);
    fails("schemify 1 shbench\nvar W a\rb\n", Code::E028, 2, 8);
    fails("schemify 1 shbench\nvar W a\u{1}b\n", Code::E028, 2, 8);
    fails(
        "schemify 1 shbench\nprose 0abc\n  |text\n",
        Code::E028,
        3,
        4,
    );
    fails(
        "schemify 1 shbench\nprose 0abc\n  | a\rb\n",
        Code::E028,
        3,
        6,
    );
    fails("schemify 1 shbench\n# a\rb\nprose 0abc\n", Code::E028, 2, 4);
    fails(
        "schemify 1 shbench\nprose 0abc\n  \"key\" x\n",
        Code::E028,
        3,
        3,
    );
    let invalid = b"schemify 1 shbench\nvar W \xff\n";
    let err = parse(invalid).unwrap_err();
    assert_eq!(
        (
            err.diagnostics[0].code,
            err.diagnostics[0].line,
            err.diagnostics[0].col
        ),
        (Code::E028, 2, 7)
    );
    assert_eq!(err.diagnostics[0].record, NO_RECORD);
    // Accepted lexical variants.
    assert!(parse(b"schemify 1 shbench\r\nvar W \"\\u{1F600}\\t\"\r\n").is_ok());
    assert!(parse(b"schemify\t01\tshbench  \nvar W 1").is_ok());
    assert!(parse(b"schemify 1 shbench\nprose 0abc\n  |\n  | x\n\t|  y\n").is_ok());
    assert!(parse(b"schemify 1 shbench\nvar W \"a\\\"b\" c#d e|f\n").is_err()); // arity, not lexing
}

#[test]
fn e029_shapes() {
    fails("schemify 1 shbench\nwire 0abc 0 0 1\n", Code::E029, 2, 1);
    fails(
        "schemify 1 shbench\nwire 0abc 0 0 1 0 9\n",
        Code::E029,
        2,
        1,
    );
    fails(
        "schemify 1 shbench\nwire 0abc 0 0 1u 0\n",
        Code::E029,
        2,
        15,
    );
    fails("schemify 1 shbench\nwire 0ABC 0 0 1 0\n", Code::E029, 2, 6);
    fails(
        "schemify 1 shbench\nwire 0abcdefghjkmnp 0 0 1 0\n",
        Code::E029,
        2,
        6,
    );
    fails(
        "schemify 1 shbench\nlabel 0abc 0 0 R45 vin\n",
        Code::E029,
        2,
        16,
    );
    fails(
        "schemify 1 shprim\npin A sideways 0 0\nimpl s spice\n",
        Code::E029,
        2,
        7,
    );
    fails(
        "schemify 1 shprim\npin A in 0 0\nimpl s spice\npoly 0 0 1 1 2\n",
        Code::E029,
        4,
        1,
    );
    fails(
        "schemify 1 shprim\npin A in 0 0\nimpl s spice\nrect 0 0 1 1\n  fill yes\n",
        Code::E029,
        5,
        8,
    );
    fails(
        "schemify 1 shprim\npin A in 0 0\nimpl s spice\nstext 0 0 R0 x\n  hcenter true\n",
        Code::E029,
        5,
        3,
    );
    fails(
        "schemify 1 shbench\ninst 0abc ./x\n  name X\n",
        Code::E029,
        2,
        1,
    );
    fails(
        "schemify 1 shbench\ninst 0abc ./x\n  at 0 0\n  name X\n",
        Code::E029,
        3,
        3,
    );
    fails("schemify 1 shbench\nwaive 0abc 1abc W\n", Code::E029, 2, 1);
    fails(
        "schemify 1 shbench\nconstraint 0abc match\n  member X1\n",
        Code::E029,
        3,
        10,
    );
    fails(
        "schemify 1 shbench\nmontecarlo mc\n  samples many\n",
        Code::E029,
        3,
        11,
    );
    fails("schemify 1 shbench\nvar W 1\n  | text\n", Code::E029, 2, 1);
    fails("schemify 1 shbench\n  | text\n", Code::E029, 1, 1);
    fails("schemify 1 project\nlib core svn x\n", Code::E029, 2, 10);
    // Unknown attributes are kept, not errors; open records accept any key.
    assert!(parse(b"schemify 1 shbench\nwire 0abc 0 0 1 0\n  future 1 2 3\ninst 1abc ./x\n  at 0 0 R0\n  name X\n  anything goes here\n").is_ok());
}

#[test]
fn w001_duplicate_ids_warn_and_point_at_the_later_record() {
    let doc =
        parse(b"schemify 1 shbench\nwire 0abc 0 0 1 0\nlabel 0abc 0 0 R0 n\nprose 0abc\n").unwrap();
    let w = doc.warnings();
    assert_eq!(w.len(), 2);
    assert!(w.iter().all(|d| d.code == Code::W001));
    assert_eq!((w[0].line, w[0].record), (3, 2));
    assert_eq!((w[1].line, w[1].record), (4, 3));
    assert!(parse(BENCH.as_bytes()).unwrap().warnings().is_empty());
}

#[test]
fn errors_are_collected_in_line_order_and_capped() {
    let mut text = String::from(BENCH);
    for i in 0..200 {
        text.push_str(&format!("wire 0a{i} 0 0 0 0\n"));
    }
    let err = parse(text.as_bytes()).unwrap_err();
    assert_eq!(err.diagnostics.len(), 64);
    assert!(err.diagnostics.windows(2).all(|w| w[0].line <= w[1].line));
    let shown = err.to_string();
    assert!(shown.starts_with("2:1: E026 zero-length wire: "), "{shown}");
}

/// Rank-3 `shape` (spec §4): the form fixes the value count; a wrong count, an unknown form
/// or a non-coordinate value is `E029`; `shape` is not a symbol (rank-2) kind.
#[test]
fn shape_forms_and_arity() {
    let ok = "schemify 1 shbench\n\nshape 0abc line 0 0 10 0\n  layer 7\nshape 0abd rect 0 0 10 5\nshape 0abe arc 0 0 5 0 90\nshape 0abf circle 0 0 5\nshape 0abg poly 0 0 10 0 10 10\n  closed true\n";
    let doc = parse(ok.as_bytes()).expect("every form parses");
    assert_eq!(schemify_format::write(&doc), ok, "canonical round trip");
    for bad in [
        "shape 0abc line 0 0 10\n",
        "shape 0abc rect 0 0 10 5 1\n",
        "shape 0abc arc 0 0 5 0\n",
        "shape 0abc circle 0 0\n",
        "shape 0abc poly 0 0 10\n",
        "shape 0abc poly 0 0\n",
    ] {
        fails(&format!("{BENCH}{bad}"), Code::E029, 2, 1);
    }
    fails(&format!("{BENCH}shape 0abc oval 0 0 5\n"), Code::E029, 2, 0);
    fails(
        &format!("{BENCH}shape 0abc line 0 0 x 0\n"),
        Code::E029,
        2,
        0,
    );
    fails(
        "schemify 1 shprim\npin A in 0 0\nimpl s spice\nshape 0abc line 0 0 1 0\n",
        Code::E027,
        4,
        0,
    );
}
