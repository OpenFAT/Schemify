//! Technology classes (pdk spec §5, §6): `lib tech map`, the derived `tech` cells, the
//! E001 explanations, and `reload_manifest` (pdk spec §9.2).

use schemify_doc::{Code, LibSource, Project, ReloadError, TableId};

/// A stand-in for `core:nmos4`: pins d g s b, class params w l m, one graphic.
const NMOS4: &[u8] = b"schemify 1 shprim\n  prefix M\n\npin d inout 20 -30\npin g in -20 0\npin s inout 20 30\npin b in 20 0\n\nparam model nmos\n  kind string\nparam w 1u\nparam l 0.18u\nparam m 1\n\nimpl spice spice\n\nline -20 0 -10 0\n";
/// A PDK nfet with upper-case pins at other positions and PDK-unit params.
const NFET: &[u8] = b"schemify 1 shprim\n  prefix X\n\npin D inout 0 -40\npin G in -40 0\npin S inout 0 40\npin B in 0 0\n\nparam W 1\nparam L 0.15\nparam nf 1\nparam mult 1\n\nimpl spice spice\n  format spice \"X{name} {D} {G} {S} {B} nfet L={L} W={W} nf={nf} m={mult}\"\n";
const MAP: &[u8] = b"schemify 1 techmap\n  pdk fake130\n\nclass nmos core\n  cell pdk:nfet\n  param W w/1u\n  param L l/1u\n  param mult m\nclass nmos lvt\n  cell pdk:nfet_lvt\n";
const MANIFEST: &[u8] = b"schemify 1 project\nlib pdk path pdk\nlib tech map maps/fake.map\n";

fn indexed(manifest: &[u8], map: &[u8]) -> Project {
    let mut p = Project::new(Some(manifest), None, 1).unwrap();
    p.index_library("core", "<builtin>", &[("nmos4.shprim", NMOS4)])
        .unwrap();
    if p.libs().find("pdk").is_some() {
        p.index_library("pdk", "pdk", &[("nfet.shprim", NFET)])
            .unwrap();
    }
    if p.libs().find("tech").is_some() {
        p.index_library("tech", "maps/fake.map", &[("tech.map", map)])
            .unwrap();
    }
    p
}

#[test]
fn tech_cells_derive_from_the_map() {
    let p = indexed(MANIFEST, MAP);
    assert_eq!(
        p.libs().source[2],
        LibSource::Map {
            path: "maps/fake.map".into()
        }
    );
    let h = p.libs().find("tech").unwrap();
    let row = p.resolve_tech("nmos", "core").expect("derived");
    assert_eq!(p.cells().lib[row] as usize, h);
    let doc = p.interface(row).unwrap();
    // Pins: the PDK's names and order, the core symbol's positions.
    let pins = doc.pins();
    let names: Vec<&str> = pins.name.iter().map(|s| doc.strings().get(*s)).collect();
    assert_eq!(names, ["D", "G", "S", "B"]);
    assert_eq!((pins.x[0], pins.y[0]), (20_000, -30_000));
    assert_eq!((pins.x[1], pins.y[1]), (-20_000, 0));
    // Params: mapped ones computed, class params appended with the class defaults.
    let text = doc.canonical();
    assert!(text.contains("param W w/1u\n  computed w/1u\n"), "{text}");
    assert!(text.contains("param mult m\n  computed m\n"), "{text}");
    assert!(
        text.contains("param nf 1\n"),
        "nf: the PDK's param is the class param: {text}"
    );
    for class in [
        "param w 1u",
        "param l 0.18u",
        "param m 1",
        "param flavor core",
    ] {
        assert!(text.contains(class), "{class}: {text}");
    }
    assert!(
        text.contains("format spice \"X{name}"),
        "the PDK impl: {text}"
    );
    assert_eq!(doc.table_len(TableId::Graphics), 1, "the core drawing");
    // `tech:nmos` means the core flavor, through the ordinary resolver.
    assert_eq!(p.resolve_in(row, "tech:nmos"), Ok(row));
}

#[test]
fn unresolved_tech_cells_explain_why() {
    // Negative space: no technology, a flavor the map lacks, a row whose PDK cell is gone,
    // not a class, and a manifest misusing the reserved name or the map source.
    let none = indexed(b"schemify 1 project\nlib pdk path pdk\n", MAP);
    assert_eq!(none.resolve_tech("nmos", "core"), Err(Code::E001));
    assert!(
        none.explain_tech("nmos", "core")
            .starts_with("no technology selected")
    );
    let p = indexed(MANIFEST, MAP);
    assert_eq!(p.resolve_tech("nmos", "hvt"), Err(Code::E001));
    assert_eq!(p.explain_tech("nmos", "hvt"), "fake130 has no hvt nmos");
    assert_eq!(p.resolve_tech("nmos", "lvt"), Err(Code::E001));
    assert!(
        p.explain_tech("nmos", "lvt")
            .contains("`pdk:nfet_lvt` not found")
    );
    assert_eq!(p.resolve_tech("pmos", "core"), Err(Code::E001));
    assert_eq!(p.explain_tech("pmos", "core"), "fake130 has no core pmos");
    assert!(
        p.explain_tech("bjt", "core")
            .contains("not a technology class")
    );
    let broken = indexed(MANIFEST, b"schemify 1 techmap\nclass nmos core\n");
    assert!(
        broken
            .explain_tech("nmos", "core")
            .contains("does not load")
    );
    let shadow = indexed(
        MANIFEST,
        b"schemify 1 techmap\nclass nmos core\n  cell pdk:nfet\n  param nf 2*nf\n",
    );
    assert!(
        shadow
            .explain_tech("nmos", "core")
            .contains("is a class param")
    );
    let missing = indexed(
        MANIFEST,
        b"schemify 1 techmap\nclass nmos core\n  cell pdk:nfet\n  param Z w\n",
    );
    assert!(
        missing
            .explain_tech("nmos", "core")
            .contains("not a param of pdk:nfet")
    );
    for bad in [
        "schemify 1 project\nlib tech path x\n",
        "schemify 1 project\nlib pdk map x.map\n",
    ] {
        assert!(
            Project::new(Some(bad.as_bytes()), None, 0).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn reload_manifest_keeps_open_documents() {
    let mut p = indexed(b"schemify 1 project\nlib pdk path pdk\n", MAP);
    let h = p
        .open("pdk", "nfet.shprim", NFET, None)
        .expect("opens")
        .handle;
    assert!(p.resolve_tech("nmos", "core").is_err());
    // Selecting a technology: the new `lib tech` row resolves after re-indexing.
    p.reload_manifest(Some(MANIFEST), None).expect("reloads");
    assert!(p.cells().lib.is_empty(), "the caller re-indexes");
    p.index_library("core", "<builtin>", &[("nmos4.shprim", NMOS4)])
        .unwrap();
    p.index_library("pdk", "pdk", &[("nfet.shprim", NFET)])
        .unwrap();
    p.index_library("tech", "maps/fake.map", &[("tech.map", MAP)])
        .unwrap();
    assert!(p.resolve_tech("nmos", "core").is_ok());
    assert!(p.doc(h).is_some(), "the open document survives");
    let row = p.resolve(h, "pdk:nfet").unwrap();
    assert_eq!(p.open_handle(row), Some(h), "and still knows its library");
    // A manifest dropping an open document's library is refused, unchanged.
    let err = p.reload_manifest(Some(b"schemify 1 project\n"), None);
    assert!(matches!(err, Err(ReloadError::LibraryInUse(l)) if l == "pdk"));
    assert!(p.resolve_tech("nmos", "core").is_ok());
    assert!(matches!(
        p.reload_manifest(Some(b"schemify 1 lock\n"), None),
        Err(ReloadError::Parse(_))
    ));
}
