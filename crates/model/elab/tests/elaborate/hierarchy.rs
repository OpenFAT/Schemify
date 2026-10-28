//! Specialization naming and its stability, path rules, recursion and inherited
//! connections.

use schemify_elab::{Dialect, Request, Target, elaborate, no_funcs};

use super::{AMP, RES, conn_names, elab_cfg, param, row, target};
use crate::common::{codes, elab, ngspice, project};

/// FNV-1a 64 + 6 Crockford base32 digits, written independently of elab.
fn spec_hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    let a = b"0123456789abcdefghjkmnpqrstvwxyz";
    (0..6)
        .map(|i| a[((h >> (59 - 5 * i)) & 31) as usize] as char)
        .collect()
}

#[test]
fn specialization_names_are_stable_and_shared() {
    let bench = "schemify 1 shbench\n\ninst 0x0001 ./amp\n  at 0 0 R0\n  name X1\ninst 0x0002 ./amp\n  at 100 0 R0\n  name X2\n  r 2k\ninst 0x0003 ./amp\n  at 200 0 R0\n  name X3\n  r 2000\ninst 0x0004 ./amp\n  at 300 0 R0\n  name X4\n  r 1000\ninst 0x0005 ./amp\n  at 400 0 R0\n  name X5\n  r 3k\n";
    let e = elab_cfg(bench, None, Dialect::Ngspice);
    let names: Vec<String> = ["X1", "X2", "X3", "X4", "X5"]
        .iter()
        .map(|x| target(&e, row(&e, e.top, x)))
        .collect();
    let two = format!("def:amp__{}", spec_hash("r=2000"));
    let three = format!("def:amp__{}", spec_hash("r=3000"));
    assert_eq!(
        names,
        [
            "def:amp",
            two.as_str(),
            two.as_str(),
            "def:amp",
            three.as_str()
        ]
    );
    let amp_defs = e
        .defs
        .cell
        .iter()
        .filter(|&&c| e.names.get(e.cells[c as usize].stem) == "amp")
        .count();
    assert_eq!(amp_defs, 3, "identical tuples share one definition");
    // Inside the specialization the value is concrete.
    let d = match e.insts.target[row(&e, e.top, "X2")] {
        Target::Def(d) => d,
        Target::Leaf(_) => panic!(),
    };
    assert_eq!(param(&e, row(&e, d, "R1"), "R"), "2000");
    // Stable across unrelated edits and record order: reverse the instances, add one.
    let shuffled = "schemify 1 shbench\n\ninst 0x0005 ./amp\n  at 0 0 R0\n  name X5\n  r 3k\ninst 0x0002 ./amp\n  at 9 9 R90\n  name X2\n  r 2k\ninst 0x0009 ./res\n  at 500 0 R0\n  name R9\n";
    let e2 = elab_cfg(shuffled, None, Dialect::Ngspice);
    assert_eq!(target(&e2, row(&e2, e2.top, "X2")), two);
    assert_eq!(target(&e2, row(&e2, e2.top, "X5")), three);
}

#[test]
fn path_rules_split_definitions_with_equal_params() {
    // Two `mid` instances with equal params; a config `inst` rule changes the impl of
    // one leaf under /M2 only, so the two `mid` definitions differ and must not share
    // a name.
    let mid = "schemify 1 shcomp\n\npin P inout 0 0\n\nimpl sch schematic\n\ninst 0a0001 ./amp\n  at 0 0 R0\n  name A1\n\nport 0p0001 0 0 R0 P\n";
    let bench = "schemify 1 shbench\n\ninst 0x0001 ./mid\n  at 0 0 R0\n  name M1\ninst 0x0002 ./mid\n  at 100 0 R0\n  name M2\n\nconfig c\n  inst /M2/A1 beh\n";
    let (p, hs) = project(&[
        ("res.shprim", RES),
        ("amp.shcomp", AMP),
        ("mid.shcomp", mid),
        ("tb.shbench", bench),
    ]);
    let req = Request {
        config: Some("c"),
        ..ngspice()
    };
    let e = elaborate(&p, hs[3], &req, &mut no_funcs);
    let (m1, m2) = (
        target(&e, row(&e, e.top, "M1")),
        target(&e, row(&e, e.top, "M2")),
    );
    assert_ne!(m1, m2, "different children, different definitions");
    assert!(
        m1.starts_with("def:mid__") && m2.starts_with("def:mid__"),
        "{m1} {m2}"
    );
    let names: Vec<&str> = e.defs.name.iter().map(|n| e.names.get(*n)).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        names.len(),
        "definition names are unique: {names:?}"
    );
}

#[test]
fn recursion_is_reported_not_followed() {
    let a = "schemify 1 shcomp\n\npin P inout 0 0\n\nimpl sch schematic\n\ninst 0a0001 ./b\n  at 0 0 R0\n  name XB\n\nport 0p0001 0 0 R0 P\n";
    let b = "schemify 1 shcomp\n\npin P inout 0 0\n\nimpl sch schematic\n\ninst 0b0001 ./a\n  at 0 0 R0\n  name XA\n\nport 0p0002 0 0 R0 P\n";
    let selfie = "schemify 1 shcomp\n\npin P inout 0 0\n\nimpl sch schematic\n\ninst 0c0001 ./selfie\n  at 0 0 R0\n  name XS\n\nport 0p0003 0 0 R0 P\n";
    let bench = "schemify 1 shbench\n\ninst 0x0001 ./a\n  at 0 0 R0\n  name X1\ninst 0x0002 ./selfie\n  at 100 0 R0\n  name X2\n";
    let (p, hs) = project(&[
        ("a.shcomp", a),
        ("b.shcomp", b),
        ("selfie.shcomp", selfie),
        ("tb.shbench", bench),
    ]);
    let e = elab(&p, hs[3]);
    let paths: Vec<&str> = e
        .violations
        .with(schemify_elab::Code::E014)
        .map(|v| e.names.get(v.path))
        .collect();
    assert_eq!(paths, ["/X1/XB/XA", "/X2/XS"]);
}

const INV: &str = "schemify 1 shprim\n\npin A in 0 0\npin Y out 20 0\npin VDD inout 10 -10\n  inherit vdd!\npin VSS inout 10 10\n  inherit 0\n\nimpl spice spice\n";

#[test]
fn inherited_connections_resolve_up_the_path() {
    // buf: I1 overrides VSS locally; I2 inherits both. So buf takes VDD and VSS as
    // propagated inherited ports (sorted), and the top resolves them per instance.
    let buf = "schemify 1 shcomp\n\npin A in 0 0\npin Y out 100 0\n\nimpl sch schematic\n\ninst 0j0001 ./inv\n  at 0 0 R0\n  name I1\n  inherit VSS gnd_local\ninst 0j0002 ./inv\n  at 20 0 R0\n  name I2\n\nport 0p0001 0 0 R0 A\nport 0p0002 40 0 R0 Y\n";
    let bench = "schemify 1 shbench\n\ninst 0x0001 ./buf\n  at 0 0 R0\n  name X1\ninst 0x0002 ./buf\n  at 200 0 R0\n  name X2\n  inherit VDD vccb\n\nlabel 0n0001 0 0 R0 in1\nlabel 0n0002 100 0 R0 out1\nlabel 0n0003 200 0 R0 in2\nlabel 0n0004 300 0 R0 out2\n";
    let (p, hs) = project(&[
        ("inv.shprim", INV),
        ("buf.shcomp", buf),
        ("tb.shbench", bench),
    ]);
    let e = elab(&p, hs[2]);
    assert!(!e.violations.blocking(), "{:?}", codes(&e));
    let x1 = row(&e, e.top, "X1");
    assert_eq!(conn_names(&e, x1), ["in1", "out1", "vdd!", "0"]);
    assert_eq!(
        conn_names(&e, row(&e, e.top, "X2")),
        ["in2", "out2", "vccb", "0"]
    );
    let Target::Def(d) = e.insts.target[x1] else {
        panic!()
    };
    let ports: Vec<&str> = e.ports(d).iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(ports, ["A", "Y", "VDD", "VSS"]);
    assert_eq!(
        conn_names(&e, row(&e, d, "I1")),
        // A net of instance pins only is `net_<inst id>_<pin index>` (spec §3).
        ["A", "net_0j0001_1", "VDD", "gnd_local"]
    );
    assert_eq!(
        conn_names(&e, row(&e, d, "I2")),
        ["net_0j0001_1", "Y", "VDD", "VSS"]
    );
    let globals: Vec<&str> = e.globals.iter().map(|n| e.names.get(*n)).collect();
    assert_eq!(globals, ["0", "vdd!"]);
    // A cell netlisted on its own: its declared/propagated pins are bound to defaults.
    let e = elab(&p, hs[1]);
    assert_eq!(conn_names(&e, row(&e, e.top, "I2"))[2..], ["vdd!", "0"]);
}
