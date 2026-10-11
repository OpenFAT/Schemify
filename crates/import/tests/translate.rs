//! Translation oracles on a small hand-written xschem library (TESTING.md `import`
//! row): every output loads in `doc` and round-trips, every expression compiles
//! (`check_output`), count conservation (asserted inside `translate_library`),
//! byte-identical re-import, and no panic on mutated inputs.

use proptest::prelude::*;
use schemify_format::{Orient, parse};
use schemify_import::report::{Class, Drop};
use schemify_import::template::Reason;
use schemify_import::translate::{ORIENT_TABLE, orient, place};
use schemify_import::{Options, check_output, translate_library};

/// A minimal library in xschem's file format, written for these tests: a `devices`
/// directory (mapped to `core` by name and pin geometry) and a design library.
fn fixture() -> Vec<(String, String)> {
    let f = |p: &str, s: &str| (p.to_owned(), s.to_owned());
    vec![
        f(
            "x/devices/res.sym",
            "v {xschem version=3.4.7 file_version=1.2}\nK {type=resistor\nformat=\"@name @pinlist @value m=@m\"\ntemplate=\"name=R1 value=1k m=1 footprint=0603\"}\nB 5 -2.5 -32.5 2.5 -27.5 {name=P dir=inout}\nB 5 -2.5 27.5 2.5 32.5 {name=M dir=inout}\nL 4 0 -30 0 30 {}\n",
        ),
        f(
            "x/devices/nmos4.sym",
            "v {xschem version=3.4.7 file_version=1.2}\nK {type=nmos\nformat=\"@spiceprefix@name @pinlist @model w=@w l=@l @extra m=@m\"\ntemplate=\"name=M1 model=nmos w=5u l=0.18u m=1\"}\nB 5 17.5 -32.5 22.5 -27.5 {name=d dir=inout}\nB 5 -22.5 -2.5 -17.5 2.5 {name=g dir=in}\nB 5 17.5 27.5 22.5 32.5 {name=s dir=inout}\nB 5 17.5 -2.5 22.5 2.5 {name=b dir=in}\n",
        ),
        f(
            "x/devices/lab_pin.sym",
            "v {xschem version=3.4.7 file_version=1.2}\nK {type=label\nformat=\"*.alias @lab\"\ntemplate=\"name=p1 lab=xxx\"}\nB 5 -1.25 -1.25 1.25 1.25 {name=p dir=in}\nT {@lab} -2.5 -12.5 0 0 0.33 0.33 {}\n",
        ),
        f(
            "x/devices/gnd.sym",
            "v {xschem version=3.4.7 file_version=1.2}\nK {type=label\nglobal=true\nformat=\"*.alias @lab\"\ntemplate=\"name=l1 lab=GND\"}\nB 5 -2.5 -2.5 2.5 2.5 {name=p dir=inout}\n",
        ),
        f(
            "x/devices/ipin.sym",
            "v {xschem version=3.4.7 file_version=1.2}\nK {type=ipin\nformat=\"*.ipin @lab\"\ntemplate=\"name=p1 lab=xxx\"}\nB 5 -1.25 -1.25 1.25 1.25 {name=p dir=out}\n",
        ),
        f(
            "x/devices/opin.sym",
            "v {xschem version=3.4.7 file_version=1.2}\nK {type=opin\nformat=\"*.opin @lab\"\ntemplate=\"name=p1 lab=xxx\"}\nB 5 -1.25 -1.25 1.25 1.25 {name=p dir=in}\n",
        ),
        f(
            "x/devices/code.sym",
            "v {xschem version=3.4.7 file_version=1.2}\nK {type=netlist_commands\ntemplate=\"name=s1 value=blabla\"\nformat=\"\n@value\n\"}\n",
        ),
        f(
            "x/devices/launcher.sym",
            "v {xschem version=3.4.7 file_version=1.2}\nK {type=launcher\ntemplate=\"name=h1 url=x\"}\n",
        ),
        f(
            "x/lib/inv.sym",
            "v {xschem version=3.4.7 file_version=1.2}\nK {type=subcircuit\nformat=\"@name @pinlist @symname WN=@WN\"\ntemplate=\"name=x1 WN=1u\"}\nB 5 -42.5 -2.5 -37.5 2.5 {name=A dir=in}\nB 5 37.5 -2.5 42.5 2.5 {name=Z dir=out}\nL 4 -40 0 40 0 {}\nT {@name} 0 -20 0 0 0.2 0.2 {}\nT {@spice_get_voltage} 0 20 0 0 0.2 0.2 {layer=15}\n",
        ),
        f(
            "x/lib/inv.sch",
            "v {xschem version=3.4.7 file_version=1.2}\nN 0 -30 0 -50 {lab=Z}\nC {ipin.sym} -40 0 0 0 {name=p1 lab=A}\nC {opin.sym} 0 -50 0 0 {name=p2 lab=Z}\nC {devices/nmos4.sym} -20 0 0 0 {name=M1 w=WN l=0.2u}\nC {lab_pin.sym} 0 30 0 0 {name=l1 lab=0}\nC {lab_pin.sym} 0 0 0 0 {name=l2 lab=0}\nC {code.sym} 0 -200 0 0 {name=MOD value=\".param KP=2\n.model nch nmos kp=KP\"}\nC {code.sym} 0 -300 0 0 {name=STIM only_toplevel=true value=\"Vin A 0 1\"}\nL 4 0 0 10 10 {}\n",
        ),
        f(
            "x/lib/nand.sym",
            "v {xschem version=3.4.7 file_version=1.2}\nK {type=primitive\nformat=\"@name @@A @@B @VDD @VSS @@Y @prefix\\\\\\\\nand2\"\ntemplate=\"name=x1 VDD=VDD VSS=VSS prefix=pdk_\"\nextra=\"VDD VSS prefix\"}\nB 5 -42.5 -12.5 -37.5 -7.5 {name=A dir=in}\nB 5 -42.5 7.5 -37.5 12.5 {name=B dir=in}\nB 5 37.5 -2.5 42.5 2.5 {name=Y dir=out}\n",
        ),
        f(
            "x/lib/tb.sch",
            "v {xschem version=3.4.7 file_version=1.2}\nN 100 -30 100 -60 {lab=OUT}\nN 100 30 100 60 {}\nN 100 60 100 60 {}\nC {inv.sym} 60 0 1 1 {name=X1 WN=2u}\nC {nand.sym} 300 0 0 0 {name=x2 VSS=GND}\nC {res.sym} 100 0 0 0 {name=R1 value='VCC/2' footprint=0805}\nC {res.sym} 200 0 2 0 {name=R2 value=1k spice_ignore=short}\nC {gnd.sym} 100 60 0 0 {name=l1 lab=GND}\nC {lab_pin.sym} 100 -60 0 0 {name=l2 lab=OUT}\nC {code.sym} 0 -200 0 0 {name=SIM value=\".param VCC=5 HALF='VCC/2'\n.tran 1n 10n\n.include $::MODELS/x.lib\"}\nC {code.sym} 0 -300 0 0 {name=TCL format=\"tcleval(@value )\" value=\".lib $::MODELS/m.lib tt\"}\nC {launcher.sym} 0 -400 0 0 {name=h1}\nC {missing.sym} 0 -500 0 0 {name=U1}\nT {a note} 0 100 0 0 0.4 0.4 {}\nB 2 0 0 100 100 {flags=graph}\n",
        ),
    ]
}

fn opts<'a>(roots: &'a [String], vars: &'a [(String, String)]) -> Options<'a> {
    Options {
        roots,
        pdk_vars: vars,
        ..Options::default()
    }
}

fn run(files: &[(String, String)]) -> (Vec<(String, Vec<u8>)>, schemify_import::Report) {
    let roots = vec!["x/devices".to_owned(), "x".to_owned()];
    let vars = vec![("MODELS".to_owned(), "${PDK_ROOT}/models".to_owned())];
    let inputs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_bytes()))
        .collect();
    translate_library(&inputs, &opts(&roots, &vars))
}

fn file<'a>(out: &'a [(String, Vec<u8>)], path: &str) -> &'a str {
    let f = out
        .iter()
        .find(|f| f.0 == path)
        .unwrap_or_else(|| panic!("no {path}"));
    std::str::from_utf8(&f.1).expect("utf-8")
}

#[test]
fn fixture_translates_and_every_output_checks() {
    let (out, report) = run(&fixture());
    let problems = check_output(&out);
    assert!(problems.is_empty(), "{problems:#?}");
    let paths: Vec<&str> = out.iter().map(|f| f.0.as_str()).collect();
    for p in [
        "lib/inv.shcomp",
        "lib/nand.shprim",
        "lib/tb.shbench",
        "schemify.project",
        "import-report.md",
        "import-report.txt",
    ] {
        assert!(paths.contains(&p), "{p} missing from {paths:?}");
    }
    // Mapped devices and non-cells produce no files of their own.
    assert!(
        !paths.iter().any(|p| p.starts_with("devices/")),
        "{paths:?}"
    );
    let tb = file(&out, "lib/tb.shbench");
    // Core mapping, D40 (footprint/default dropped, value kept verbatim), ignore.
    assert!(
        tb.contains("core:res\n  at 100 0 R0\n  name R1\n  value 'VCC/2'\n"),
        "{tb}"
    );
    assert!(tb.contains("  name R2\n  ignore short\n"), "{tb}");
    assert!(!tb.contains("footprint"), "{tb}");
    // Orientation (rot 1, flip 1) and an override of a subcircuit parameter.
    assert!(
        tb.contains("./inv\n  at 60 0 MR90\n  name X1\n  WN 2u\n"),
        "{tb}"
    );
    // Inherited pins: explicit nets on every instance (override or template default).
    assert!(
        tb.contains("  inherit VDD VDD\n  inherit VSS GND\n"),
        "{tb}"
    );
    // Labels; gnd.sym is a global label keeping xschem's name.
    assert!(tb.contains(" GND\n  global true\n"), "{tb}");
    assert!(tb.contains(" OUT\n"), "{tb}");
    // Code: `.param` became bench vars; PDK paths became ${VAR}; Tcl format honoured.
    assert!(
        tb.contains("var VCC 5\n") && tb.contains("var HALF VCC/2\n"),
        "{tb}"
    );
    assert!(
        tb.contains("  | .include $$::MODELS/x.lib\n"),
        "plain text keeps a literal $: {tb}"
    );
    assert!(tb.contains("  | .lib ${MODELS}/m.lib tt"), "{tb}");
    assert!(file(&out, "schemify.project").contains("set MODELS ${PDK_ROOT}/models\n"));
    // Component: ports bound to the symbol pins, parameter passed down as an expression.
    let inv = file(&out, "lib/inv.shcomp");
    assert!(
        inv.contains("impl schematic schematic\n  format spice \"{name} {pins} {cell}\"\n"),
        "{inv}"
    );
    assert!(
        inv.contains(" A\n") && inv.contains("port ") && inv.contains("  w WN\n"),
        "{inv}"
    );
    assert!(inv.contains("stext 0 -20 R0 {name}\n"), "{inv}");
    // Primitive with extra pins: inherited pins and a terminated token.
    let nand = file(&out, "lib/nand.shprim");
    assert!(
        nand.contains("pin VDD inout 0 0\n  inherit VDD\n"),
        "{nand}"
    );
    assert!(
        nand.contains("format spice \"{name} {A} {B} {VDD} {VSS} {Y} {prefix}nand2\""),
        "{nand}"
    );
    // Report: needs_review for the missing symbol, drops for the rest.
    let has = |c: Class| report.items.iter().any(|i| i.class == c);
    assert!(has(Class::Review(Reason::MissingSymbol)));
    assert!(has(Class::Dropped(Drop::NonElectrical)) && has(Class::Dropped(Drop::Graphic)));
    assert!(has(Class::Dropped(Drop::ZeroWire)) && has(Class::Dropped(Drop::Display)));
    assert_eq!((report.core_instances, report.device_instances), (3, 3));
    // D17 as amended: a component's netlist text is its own `code` (its `.param` lines
    // stay in it, xschem writes them inside the subckt); `only_toplevel` text belongs to
    // the bench copy alone.
    assert!(
        inv.contains("code MOD spice\n  | .param KP=2\n  | .model nch nmos kp=KP\n"),
        "{inv}"
    );
    assert!(!inv.contains("STIM") && !inv.contains("var KP"), "{inv}");
    let copy = file(&out, "lib/inv__top.shbench");
    assert!(copy.contains("code STIM spice\n  | Vin A 0 1\n"), "{copy}");
    assert!(
        copy.contains("var KP 2\n"),
        "the bench copy's .param: {copy}"
    );
    assert!(has(Class::Dropped(Drop::BenchCopy)));
    assert!(
        !report
            .items
            .iter()
            .any(|i| matches!(i.class, Class::Review(_)) && i.detail.contains("MOD")),
        "component code is translated, not reviewed"
    );
    report.assert_conservation();
}

#[test]
fn reimport_is_byte_identical_whatever_the_input_order() {
    let files = fixture();
    let (a, _) = run(&files);
    let mut rev = files.clone();
    rev.reverse();
    let (b, _) = run(&rev);
    assert_eq!(a, b);
}

/// The table matches xschem's documented placement (feature inventory §5.6: flip x
/// first, then rot 1 maps (x, y) → (−y, x), clockwise on screen with y down).
#[test]
fn orientation_table_matches_documented_xschem_transform() {
    let xschem = |rot: i16, flip: i16, p: [i32; 2]| {
        let (mut x, mut y) = (p[0], p[1]);
        if flip != 0 {
            x = -x;
        }
        for _ in 0..rot {
            (x, y) = (-y, x);
        }
        [x, y]
    };
    for rot in 0..4 {
        for flip in 0..2 {
            for p in [[20, -30], [-20, 0], [7, 13]] {
                assert_eq!(
                    place(p, orient(rot, flip), [0, 0]),
                    Some(xschem(rot, flip, p)),
                    "rot {rot} flip {flip}"
                );
            }
        }
    }
    assert_eq!(ORIENT_TABLE[1][1], Orient::MR90);
    assert_eq!(orient(-1, 5), Orient::MR270);
}

/// A byte-level mutation of one fixture file.
fn mutate(s: &str, ops: &[(u16, u8, u8)]) -> String {
    let mut b = s.as_bytes().to_vec();
    for &(at, op, byte) in ops {
        let i = usize::from(at) % (b.len() + 1);
        let table = b"{}\"\\@ =\n.;[]$0aZ-5";
        let byte = table[usize::from(byte) % table.len()];
        match op % 3 {
            0 if i < b.len() => b[i] = byte,
            1 => b.insert(i, byte),
            _ if i < b.len() => {
                b.remove(i);
            }
            _ => {}
        }
    }
    String::from_utf8_lossy(&b).into_owned()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Mutated real-shaped files: the whole translate path never panics, conserves
    /// records (asserted inside) and still produces only valid files.
    #[test]
    fn mutated_inputs_translate_to_valid_outputs(
        which in 0usize..12,
        ops in prop::collection::vec((any::<u16>(), any::<u8>(), any::<u8>()), 1..12),
    ) {
        let mut files = fixture();
        let k = which % files.len();
        files[k].1 = mutate(&files[k].1, &ops);
        let (out, report) = run(&files);
        report.assert_conservation();
        let problems = check_output(&out);
        prop_assert!(problems.is_empty(), "{problems:#?}\n{}", files[k].1);
    }
}

#[test]
fn parse_of_every_output_cell_is_canonical() {
    let (out, _) = run(&fixture());
    for (p, b) in out.iter().filter(|f| {
        f.0.ends_with(".shbench") || f.0.ends_with(".shcomp") || f.0.ends_with(".shprim")
    }) {
        let d = parse(b).unwrap_or_else(|e| panic!("{p}: {e:?}"));
        assert_eq!(schemify_format::write(&d).as_bytes(), b.as_slice(), "{p}");
    }
}
