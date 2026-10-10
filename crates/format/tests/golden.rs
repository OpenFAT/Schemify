//! Golden fixtures: the research §5 examples adapted to spec v1 (coordinates, `impl`
//! records, bench tables), one per file kind. Each is canonical, so it must be a fixed
//! point of parse → write; the messy variant must canonicalize to its twin.

use schemify_format::{FileKind, RecordKind, parse, parse_coord, parse_prefix, write};

const FIXTURES: &[(&str, &str, FileKind)] = &[
    (
        "ota5t.shcomp",
        include_str!("fixtures/ota5t.shcomp"),
        FileKind::Shcomp,
    ),
    (
        "nfet_01v8.shprim",
        include_str!("fixtures/nfet_01v8.shprim"),
        FileKind::Shprim,
    ),
    (
        "ota_ac.shbench",
        include_str!("fixtures/ota_ac.shbench"),
        FileKind::Shbench,
    ),
    (
        "schemify.project",
        include_str!("fixtures/schemify.project"),
        FileKind::Project,
    ),
    (
        "schemify.lock",
        include_str!("fixtures/schemify.lock"),
        FileKind::Lock,
    ),
    ("config", include_str!("fixtures/config"), FileKind::Config),
];

#[test]
fn fixtures_are_canonical_fixed_points() {
    for (name, text, kind) in FIXTURES {
        let doc = parse(text.as_bytes()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(doc.kind(), *kind, "{name}");
        assert!(doc.warnings().is_empty(), "{name}");
        assert_eq!(write(&doc), *text, "{name} is not canonical");
    }
}

#[test]
fn messy_input_canonicalizes() {
    let canonical = include_str!("fixtures/ota5t.shcomp");
    let messy = include_str!("fixtures/ota5t.messy.shcomp");
    assert_eq!(write(&parse(messy.as_bytes()).unwrap()), canonical);
    let crlf = messy.replace('\n', "\r\n");
    assert_eq!(write(&parse(crlf.as_bytes()).unwrap()), canonical);
    let unterminated = messy.trim_end_matches('\n');
    assert_eq!(write(&parse(unterminated.as_bytes()).unwrap()), canonical);
}

#[test]
fn ota_reads_as_expected() {
    let text = include_str!("fixtures/ota5t.shcomp");
    let parsed = parse(text.as_bytes()).unwrap();
    let doc = &parsed;
    let of = |k: RecordKind| (0..doc.record_count()).filter(move |&r| doc.record_kind(r) == k);
    let pins: Vec<&str> = of(RecordKind::Pin)
        .map(|r| doc.field(r, 0).unwrap())
        .collect();
    assert_eq!(pins, ["INP", "INN", "OUT", "IBIAS", "VDD", "VSS"]);
    let mut ports: Vec<&str> = of(RecordKind::Port)
        .map(|r| doc.field(r, 4).unwrap())
        .collect();
    ports.sort_unstable();
    let mut sorted_pins = pins.clone();
    sorted_pins.sort_unstable();
    assert_eq!(ports, sorted_pins, "every pin has exactly one port");
    let m2 = of(RecordKind::Inst)
        .find(|&r| doc.find_attr(r, "name").and_then(|a| doc.attr_value(a, 0)) == Some("M2"))
        .unwrap();
    let at = doc.find_attr(m2, "at").unwrap();
    assert_eq!(doc.attr_value(at, 0).map(parse_coord), Some(Ok(120_000)));
    assert_eq!(doc.attr_value(at, 2), Some("MR0"));
    assert_eq!(doc.comments(1).count(), 2);
    let prose = of(RecordKind::Prose).next().unwrap();
    assert_eq!(
        doc.verbatim(prose).collect::<Vec<_>>(),
        [
            "# 5T OTA",
            "",
            "Gain is $g_m r_o / 2$, with $r_o$ from M2 || M4."
        ]
    );
    assert_eq!(of(RecordKind::Inst).count(), 5);
    assert_eq!(of(RecordKind::Wire).count(), 5);
}

#[test]
fn symbol_interface_reads_without_the_schematic() {
    let text = include_str!("fixtures/ota5t.shcomp");
    let iface = parse_prefix(text.as_bytes(), 1).unwrap();
    let kinds: Vec<RecordKind> = (1..iface.record_count())
        .map(|r| iface.record_kind(r))
        .collect();
    assert_eq!(kinds.iter().filter(|k| **k == RecordKind::Pin).count(), 6);
    assert_eq!(kinds.iter().filter(|k| **k == RecordKind::Param).count(), 2);
    assert_eq!(*kinds.last().unwrap(), RecordKind::Impl);
    assert!(kinds.iter().all(|k| k.rank() <= 1));
    // The rank-2 comment before the cut has nowhere to attach but end of file.
    assert_eq!(iface.eof_comments().count(), 1);
}

#[test]
fn bench_values_unescape() {
    let doc = parse(include_str!("fixtures/ota_ac.shbench").as_bytes()).unwrap();
    let output = (0..doc.record_count())
        .find(|&r| doc.record_kind(r) == RecordKind::Output)
        .unwrap();
    assert_eq!(
        doc.field(output, 1),
        Some("meas(\"ac\", \"when vdb(out)=0\")")
    );
    let code = (0..doc.record_count())
        .find(|&r| doc.record_kind(r) == RecordKind::Code)
        .unwrap();
    assert_eq!(doc.verbatim(code).nth(1), Some("  ac dec 20 1 1G"));
}
