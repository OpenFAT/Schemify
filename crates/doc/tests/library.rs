//! Project manifest, lockfile, library index, cell resolution (netlist spec §1), and
//! the edit semantics that need the instantiated cell: grid snapping (D13), `E030`,
//! and the D40 redundant-override report.

mod common;

use common::*;
use schemify_doc::{
    Code, LibSource, LockStatus, Op, Outcome, Project, RecordBuf, Target, apply_edits, hex, sha256,
};
use schemify_format::{FileKind, RecordKind};

const DEV: &str = "schemify 1 shprim\n\npin D inout 0 0\n\nparam L 0.15\n  grid 0.005\nparam W 1\n  step 0.5\n  min 0.25\nparam ad 0\n  computed W*0.29\n\nimpl spice spice\n";

fn project() -> Project {
    Project::new(Some(MANIFEST.as_bytes()), Some(LOCK.as_bytes()), 3).unwrap()
}

#[test]
fn manifest_and_lockfile_parse_into_rows() {
    let p = project();
    let libs = p.libs();
    assert_eq!(libs.name, ["", "sky130", "analog", "mine", "core"]);
    assert_eq!(libs.source[0], LibSource::Project);
    assert_eq!(
        libs.source[1],
        LibSource::Git {
            url: "https://github.com/example/sky130-schemify".into(),
            rev: "v1.2.0".into()
        }
    );
    assert_eq!(libs.subdir[1].as_deref(), Some("libs/sky130"));
    assert!(matches!(libs.source[2], LibSource::Url { .. }));
    assert_eq!(
        libs.source[3],
        LibSource::Path {
            path: "./lib".into()
        }
    );
    assert_eq!(libs.source[4], LibSource::Builtin);
    assert_eq!(p.plugins().len(), 1);
    assert_eq!(
        p.settings()[0],
        ("PDK_ROOT".to_owned(), "/opt/pdk".to_owned())
    );
    assert_eq!(p.lock().name, ["analog", "sky130", "gmid-lut"]);
    assert_eq!(
        p.write_lock(),
        LOCK,
        "an unchanged lockfile writes back byte-identically"
    );
    let bare = Project::new(None, None, 0).unwrap();
    assert_eq!(bare.libs().name, ["", "core"]);
    assert!(
        Project::new(Some(LOCK.as_bytes()), None, 0).is_err(),
        "a lockfile is not a manifest"
    );
}

#[test]
fn indexing_hashes_content_and_checks_the_lock() {
    let mut p = project();
    let files: &[(&str, &[u8])] = &[("nfet_01v8.shprim", NFET.as_bytes()), ("README.md", b"x")];
    let (status, diags) = p.index_library("sky130", "4e1c2a9", files).unwrap();
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        status,
        LockStatus::Mismatch {
            locked: "0b5d3c7f1e9a2846".into()
        }
    );
    p.lock_library("sky130").unwrap();
    let lock = p.write_lock();
    let hash = hex(&p.libs().hash[1].unwrap());
    assert!(
        lock.contains(&format!("locked lib sky130 git 4e1c2a9 {hash}\n")),
        "{lock}"
    );
    let mut q = Project::new(Some(MANIFEST.as_bytes()), Some(lock.as_bytes()), 3).unwrap();
    let (status, _) = q.index_library("sky130", "4e1c2a9", files).unwrap();
    assert_eq!(status, LockStatus::Match);
    let changed: &[(&str, &[u8])] = &[("nfet_01v8.shprim", DEV.as_bytes())];
    assert!(matches!(
        q.index_library("sky130", "4e1c2a9", changed).unwrap().0,
        LockStatus::Mismatch { .. }
    ));
    // The content hash is defined over sorted (path, file hash), independent of listing order.
    let swapped: &[(&str, &[u8])] = &[("README.md", b"x"), ("nfet_01v8.shprim", NFET.as_bytes())];
    q.index_library("sky130", "4e1c2a9", swapped).unwrap();
    assert_eq!(hex(&q.libs().hash[1].unwrap()), hash);
    assert_eq!(
        q.cells().lib.len(),
        1,
        "re-indexing replaces the library's rows"
    );
    assert_eq!(q.cells().hash[0], sha256(NFET.as_bytes()));
    assert_eq!(p.index_library("nolib", "", files), Err(Code::E001));
    assert_eq!(p.lock_library("core"), Err(Code::E043));
    assert_eq!(p.lock_library("mine"), Err(Code::E001), "never indexed");
}

#[test]
fn cell_references_resolve_per_netlist_spec() {
    let mut p = project();
    p.index_library("sky130", "r", &[("nfet_01v8.shprim", NFET.as_bytes())])
        .unwrap();
    let files: &[(&str, &[u8])] = &[
        ("bench/ota5t.shcomp", OTA.as_bytes()),
        ("bench/ota_ac.shbench", BENCH.as_bytes()),
        (
            "bench/twin.shcomp",
            b"schemify 1 shcomp\n\nimpl sch schematic\n",
        ),
        ("bench/twin.shprim", DEV.as_bytes()),
        (
            "bench/broken.shcomp",
            b"schemify 1 shcomp\n\npin A sideways 0 0\n",
        ),
    ];
    let (_, diags) = p.index_library("", "", files).unwrap();
    let codes: Vec<(String, Code)> = diags.iter().map(|(f, d)| (f.clone(), d.code)).collect();
    assert!(
        codes.contains(&("bench/twin.shprim".into(), Code::E020)),
        "{codes:?}"
    );
    let broken = codes.iter().find(|(f, _)| f == "bench/broken.shcomp");
    assert!(
        broken.is_some_and(|(_, c)| !c.is_warning()),
        "an interface that does not parse is reported: {codes:?}"
    );
    let h = p
        .open("", "bench/ota_ac.shbench", BENCH.as_bytes(), None)
        .unwrap()
        .handle;
    let ota = p.resolve(h, "./ota5t").unwrap();
    assert_eq!(p.cells().kind[ota], FileKind::Shcomp);
    let iface = p.interface(ota).unwrap();
    assert_eq!(
        iface.pins().name.len(),
        6,
        "the interface is ranks 0-1 only"
    );
    assert_eq!(iface.instances().id.len(), 0);
    let nfet = p.resolve(h, "sky130:nfet_01v8").unwrap();
    assert_eq!(p.cells().kind[nfet], FileKind::Shprim);
    assert_eq!(
        p.cells().kind[p.resolve(h, "./twin").unwrap()],
        FileKind::Shcomp,
        ".shcomp before .shprim"
    );
    for bad in [
        "./ota_ac",
        "nolib:x",
        "sky130:missing",
        "analog:x",
        "ota5t",
        "./bench/ota5t",
        ":x",
    ] {
        assert_eq!(p.resolve(h, bad), Err(Code::E001), "{bad}");
    }
    // An open cell is its own interface.
    let oh = p
        .open("", "bench/ota5t.shcomp", OTA.as_bytes(), None)
        .unwrap()
        .handle;
    let del = Op::Delete(Target::new(RecordKind::Pin, "VSS"));
    assert!(matches!(
        run(&mut p, oh, tx(vec![del])),
        Outcome::Committed { .. }
    ));
    assert_eq!(p.interface(ota).unwrap().pins().name.len(), 5);
    assert!(matches!(
        p.open("", "bench/ota5t.shcomp", OTA.as_bytes(), None),
        Err(schemify_doc::OpenError::AlreadyOpen(_))
    ));
    assert!(matches!(
        p.open("nolib", "x.shcomp", OTA.as_bytes(), None),
        Err(schemify_doc::OpenError::Code(Code::E001))
    ));
    assert!(matches!(
        p.open("", "x.lock", LOCK.as_bytes(), None),
        Err(schemify_doc::OpenError::Code(Code::E027))
    ));
}

fn bench_with_device() -> (Project, schemify_doc::DocHandle) {
    let mut p = Project::new(None, None, 5).unwrap();
    p.index_library("", "", &[("dev.shprim", DEV.as_bytes())])
        .unwrap();
    let bench = "schemify 1 shbench\n\ninst 0gsrp0 ./dev\n  at 0 0 R0\n  name M1\n  W 1\n";
    let h = p
        .open("", "b.shbench", bench.as_bytes(), None)
        .unwrap()
        .handle;
    (p, h)
}

fn attr(p: &Project, h: schemify_doc::DocHandle, key: &str) -> Option<String> {
    let d = p.doc(h).unwrap();
    let v = d
        .pool()
        .find_attr(d.strings(), d.instances().attrs[0], key)?;
    Some(d.strings().get(v[0]).to_owned())
}

fn set(key: &str, value: &str) -> Op {
    Op::SetAttr {
        target: Target::new(RecordKind::Inst, "0gsrp0"),
        key: key.into(),
        values: vec![value.into()],
    }
}

#[test]
fn plain_number_overrides_snap_to_grid_and_step_and_are_journaled_snapped() {
    let (mut p, h) = bench_with_device();
    assert!(matches!(
        run(&mut p, h, tx(vec![set("L", "0.1534")])),
        Outcome::Committed { .. }
    ));
    assert_eq!(attr(&p, h, "L").as_deref(), Some("0.155"));
    assert!(matches!(
        run(&mut p, h, tx(vec![set("W", "1.6")])),
        Outcome::Committed { .. }
    ));
    assert_eq!(
        attr(&p, h, "W").as_deref(),
        Some("1.75"),
        "step 0.5 from min 0.25"
    );
    assert!(matches!(
        run(&mut p, h, tx(vec![set("L", "lmin*2")])),
        Outcome::Committed { .. }
    ));
    assert_eq!(
        attr(&p, h, "L").as_deref(),
        Some("lmin*2"),
        "expressions are left for elab"
    );
    let journal = p.take_journal(h).unwrap().text;
    assert!(
        journal.contains("set_attr inst 0gsrp0 L 0.155"),
        "{journal}"
    );
    assert!(journal.contains("set_attr inst 0gsrp0 W 1.75"), "{journal}");
    // Inserted instances snap every override.
    let inst = RecordBuf::new(RecordKind::Inst)
        .field("1aaaaa")
        .field("./dev")
        .attr("at", &["10", "0", "R0"])
        .attr("name", &["M2"])
        .attr("L", &["0.151"]);
    assert!(matches!(
        run(
            &mut p,
            h,
            tx(vec![Op::Insert {
                rec: inst,
                at: None
            }])
        ),
        Outcome::Committed { .. }
    ));
    let d = p.doc(h).unwrap();
    let row = (0..2)
        .find(|&i| d.strings().get(d.instances().name[i]) == "M2")
        .unwrap();
    let l = d
        .pool()
        .find_attr(d.strings(), d.instances().attrs[row], "L")
        .unwrap();
    assert_eq!(d.strings().get(l[0]), "0.15");
    // Moving the instance never touches its overrides.
    let mv = Op::SetAttr {
        target: Target::new(RecordKind::Inst, "0gsrp0"),
        key: "at".into(),
        values: vec!["5".into(), "5".into(), "R0".into()],
    };
    assert!(matches!(
        run(&mut p, h, tx(vec![mv])),
        Outcome::Committed { .. }
    ));
    assert_eq!(attr(&p, h, "W").as_deref(), Some("1.75"));
}

#[test]
fn computed_params_cannot_be_overridden() {
    let (mut p, h) = bench_with_device();
    let out = run(&mut p, h, tx(vec![set("ad", "1")]));
    assert!(
        matches!(&out, Outcome::Rejected(e) if e[0].code == Code::E030),
        "{out:?}"
    );
    // Switching the cell re-checks existing overrides against the new cell.
    p.index_library(
        "",
        "",
        &[
            ("dev.shprim", DEV.as_bytes()),
            (
                "other.shprim",
                DEV.replace("param W 1", "param W 1\n  computed 2")
                    .as_bytes(),
            ),
        ],
    )
    .unwrap();
    let retarget = Op::SetField {
        target: Target::new(RecordKind::Inst, "0gsrp0"),
        index: 1,
        value: "./other".into(),
    };
    let out = run(&mut p, h, tx(vec![retarget]));
    assert!(
        matches!(&out, Outcome::Rejected(e) if e[0].code == Code::E030),
        "{out:?}"
    );
    // An unresolved cell gets no parameter semantics (elab reports E001).
    let unresolved = Op::SetField {
        target: Target::new(RecordKind::Inst, "0gsrp0"),
        index: 1,
        value: "./nowhere".into(),
    };
    assert!(matches!(
        run(&mut p, h, tx(vec![unresolved, set("ad", "1")])),
        Outcome::Committed { .. }
    ));
}

#[test]
fn overrides_equal_to_defaults_are_kept_but_reported() {
    let (mut p, h) = bench_with_device();
    assert_eq!(p.redundant_overrides(h).len(), 1, "W 1 equals the default");
    assert_eq!(
        attr(&p, h, "W").as_deref(),
        Some("1"),
        "kept as written (D40)"
    );
    let canon_tx = p.canonicalize_overrides(h);
    assert_eq!(canon_tx.ops.len(), 1);
    let out = apply_edits(&mut p, &[req(h, schemify_doc::Edit::Tx(canon_tx))]);
    assert!(matches!(out[0], Outcome::Committed { .. }));
    assert!(p.redundant_overrides(h).is_empty());
    assert_eq!(attr(&p, h, "W"), None);
}
