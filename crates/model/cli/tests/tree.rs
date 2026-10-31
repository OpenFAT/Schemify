//! `schemify tree` end to end (docs/guide/cli.md § tree) on the two-stage Miller op-amp
//! ladder of Analog-Skills' stage-1 reference (`skills/analog-design-flow/reference/
//! system-spec-veriloga.md` §2.5–§2.6): opamp2 → stage1 (→ diffpair, mirror), cs_stage, CC.
//! `tests/fixtures/tree/native` (cells and `opamp2_ac.shbench`) and `.../fallback` (the
//! `spice` + `.hdl` form) are the unedited output of that reference's verified generator,
//! `gen.py native|fallback` (Circuit-Skills 5eee978, 2026-10-12).
//!
//! Oracles:
//! - spec values: retiring the nodes leaf first (`default beh` → `default schematic`, in
//!   the reference's order) makes `count_va` read 5, 4, 3, 2, 1, 0;
//! - the netlister: in every state and every config of the bench, the elaborated
//!   Verilog-A nodes are the netlist's Verilog-A instance lines (`N…` naming a `beh_`
//!   model, the reference's own cross-check, §2.4; each cell has one instance), and an
//!   `inst` rule selecting `beh` for one instance raises both by one;
//! - metamorphic: the `.hdl` form gives the native form's tree, langs aside;
//! - golden: the ladder's `--json` and text (`tests/fixtures/tree/golden`, re-blessed with
//!   `SCHEMIFY_BLESS=1` and reviewed).

mod common;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use common::{core, scratch};
use schemify_cli::rpc::{Json, parse};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tree");

/// The nodes in the order the reference retires them, leaf first (§2.5).
const RETIRE: [&str; 5] = ["cs_stage", "mirror", "diffpair", "stage1", "opamp2"];

/// The bench's configs (§2.3) and `one`, added here: `beh` for one instance.
const CONFIGS: [&str; 6] = ["children", "current", "diagram", "golden", "xtor", "one"];
const ONE: &str = "config one\n  inst /XDUT/XS1/XDP beh\n\n";

// ---- private ----

/// Runs the core in `dir` with the user directories inside it; (exit code, stdout, stderr).
fn schemify(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = core(dir).args(args).output().expect("binary runs");
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let code = out.status.code().expect("not killed");
    (code, text(&out.stdout), text(&out.stderr))
}

/// The fixture form `form` copied into a scratch project, the bench given config `one`.
fn project(name: &str, form: &str) -> PathBuf {
    let dir = scratch(name);
    let from = fs::read_dir(Path::new(FIXTURE).join(form)).expect("the fixture exists");
    for e in from.flatten() {
        fs::copy(e.path(), dir.join(e.file_name())).expect("the scratch dir is writable");
    }
    fs::write(dir.join("schemify.project"), "schemify 1 project\n")
        .expect("the scratch dir is writable");
    let bench = dir.join("opamp2_ac.shbench");
    if let Ok(text) = fs::read_to_string(&bench) {
        let at = text.find("config xtor\n").expect("the bench's configs");
        fs::write(&bench, format!("{}{ONE}{}", &text[..at], &text[at..]))
            .expect("the scratch dir is writable");
    }
    dir
}

/// Retires `cell`: its default becomes its schematic.
fn retire(dir: &Path, cell: &str) {
    let path = dir.join(format!("{cell}.shcomp"));
    let text = fs::read_to_string(&path).expect("the cell exists");
    let retired = text.replacen("\n  default beh\n", "\n  default schematic\n", 1);
    assert_ne!(retired, text, "{cell} was behavioral");
    fs::write(&path, retired).expect("the scratch dir is writable");
}

/// `tree --json` of `file` in `dir`, one JSON value per view; asserts exit 0.
fn tree(dir: &Path, file: &str, configs: &[&str]) -> Vec<Json> {
    let mut args = vec!["--clean", "tree", file, "--json"];
    for c in configs {
        args.extend(["--config", c]);
    }
    let (code, out, err) = schemify(dir, &args);
    assert_eq!(code, 0, "{err}");
    let views: Vec<Json> = out
        .lines()
        .map(|l| parse(l).expect("one JSON value per line"))
        .collect();
    assert_eq!(views.len(), configs.len().max(1), "{out}");
    views
}

fn num(v: &Json) -> u32 {
    match v {
        Json::Num(x) => *x as u32,
        _ => panic!("not a number: {v:?}"),
    }
}

fn nodes<'j>(v: &'j Json, out: &mut Vec<&'j Json>) {
    let Some(Json::Arr(kids)) = v.get("roots").or(v.get("children")) else {
        panic!("no roots or children: {v:?}");
    };
    for k in kids {
        out.push(k);
        nodes(k, out);
    }
}

/// The Verilog-A nodes of a view that its netlist instantiates.
fn elaborated_va(view: &Json) -> usize {
    let mut all = Vec::new();
    nodes(view, &mut all);
    let yes = |n: &Json, k: &str| n.get(k) == Some(&Json::Bool(true));
    all.iter()
        .filter(|n| yes(n, "veriloga") && yes(n, "elaborated"))
        .count()
}

/// Verilog-A instance lines of an ngspice netlist: an `N` element naming a `beh_` model.
fn va_lines(netlist: &str) -> usize {
    let behavioral = |l: &str| l.split_whitespace().skip(1).any(|t| t.starts_with("beh_"));
    netlist
        .lines()
        .filter(|l| l.starts_with(['N', 'n']) && behavioral(l))
        .count()
}

/// Drops every `lang` member (the one difference of the `.hdl` form).
fn without_lang(v: &Json) -> Json {
    match v {
        Json::Arr(items) => Json::Arr(items.iter().map(without_lang).collect()),
        Json::Obj(members) => Json::Obj(
            members
                .iter()
                .filter(|(k, _)| k != "lang")
                .map(|(k, v)| (k.clone(), without_lang(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn golden(name: &str, text: &str) {
    let path = Path::new(FIXTURE).join("golden").join(name);
    if env::var_os("SCHEMIFY_BLESS").is_some() {
        fs::create_dir_all(Path::new(FIXTURE).join("golden")).expect("the fixture is writable");
        fs::write(&path, text).expect("the fixture is writable");
    }
    let want = fs::read_to_string(&path).unwrap_or_else(|_| panic!("{name}: bless it"));
    assert_eq!(text, want, "{name}");
}

/// The bench's ngspice netlist under `config`; asserts exit 0.
fn netlist(dir: &Path, config: &str) -> String {
    let args = [
        "--clean",
        "netlist",
        "opamp2_ac.shbench",
        "--dialect",
        "ngspice",
    ];
    let (code, text, err) = schemify(dir, &[&args[..], &["--config", config]].concat());
    assert_eq!(code, 0, "{config}: {err}");
    text
}

/// Asserts that, for every config, the bench's tree elaborates the Verilog-A nodes its
/// netlist instantiates.
fn same_as_netlist(dir: &Path, state: &str) {
    let views = tree(dir, "opamp2_ac.shbench", &CONFIGS);
    for (config, view) in CONFIGS.iter().zip(&views) {
        assert_eq!(view.get("config"), Some(&Json::Str((*config).to_owned())));
        let text = netlist(dir, config);
        assert_eq!(
            elaborated_va(view),
            va_lines(&text),
            "{config}, {state}:\n{text}"
        );
    }
}

/// Gate: `count_va` is the burn-down count, 5, 4, 3, 2, 1, 0, for the cell and for its
/// golden bench (whose `config current` has no rules), and it never fails the command.
/// The cell's `--json` and text through the ladder are the goldens.
#[test]
fn count_va_burns_down_leaf_first() {
    let dir = project("ladder", "native");
    let (mut json, mut text) = (String::new(), String::new());
    for k in 0..=RETIRE.len() {
        if k > 0 {
            retire(&dir, RETIRE[k - 1]);
        }
        let want = (RETIRE.len() - k) as u32;
        let (code, out, err) = schemify(&dir, &["--clean", "tree", "opamp2.shcomp", "--json"]);
        assert_eq!((code, err.as_str()), (0, ""), "exit 0 with Verilog-A left");
        let cell = parse(out.trim_end()).expect("one JSON line");
        let bench = &tree(&dir, "opamp2_ac.shbench", &[])[0];
        for view in [&cell, bench] {
            assert_eq!(
                num(view.get("count_va").unwrap()),
                want,
                "after {k} retired"
            );
        }
        json.push_str(&out);
        let (code, out, _) = schemify(&dir, &["--clean", "tree", "opamp2.shcomp"]);
        assert_eq!(code, 0);
        let retired = if k == 0 {
            "none".to_owned()
        } else {
            RETIRE[..k].join(" ")
        };
        text.push_str(&format!("# retired: {retired}\n{out}"));
    }
    golden("ladder.json", &json);
    golden("ladder.txt", &text);
}

/// Gate: in every state and every config, the tree's elaborated Verilog-A nodes are the
/// netlist's Verilog-A lines, an ignored instance left out of both; once every node is
/// retired, `inst … beh` for one instance raises `count_va` and the netlist by one.
#[test]
fn elaborated_va_nodes_are_the_netlists() {
    let dir = project("netlist", "native");
    for k in 0..=RETIRE.len() {
        if k > 0 {
            retire(&dir, RETIRE[k - 1]);
        }
        same_as_netlist(&dir, &format!("{k} retired"));
    }
    let count = |c| {
        num(tree(&dir, "opamp2_ac.shbench", &[c])[0]
            .get("count_va")
            .unwrap())
    };
    assert_eq!((count("current"), count("one")), (0, 1));
    let lines = |c| va_lines(&netlist(&dir, c));
    assert_eq!((lines("current"), lines("one")), (0, 1));
    // `children` elaborates cs_stage's model; `ignore true` leaves it out of the netlist.
    assert_eq!(lines("children"), 2);
    let path = dir.join("opamp2.shcomp");
    let text = fs::read_to_string(&path).unwrap();
    let ignored = text.replacen("  name XS2\n", "  name XS2\n  ignore true\n", 1);
    assert_ne!(ignored, text);
    fs::write(&path, ignored).unwrap();
    assert_eq!(lines("children"), 1);
    same_as_netlist(&dir, "XS2 ignored");
}

/// A `spice` impl whose body loads an `.hdl` is Verilog-A: the reference's fallback form
/// burns down exactly like the native one, node for node.
#[test]
fn the_hdl_form_is_veriloga() {
    let (native, hdl) = (project("native", "native"), project("hdl", "fallback"));
    for k in 0..=RETIRE.len() {
        if k > 0 {
            retire(&native, RETIRE[k - 1]);
            retire(&hdl, RETIRE[k - 1]);
        }
        let a = without_lang(&tree(&native, "opamp2.shcomp", &[])[0]);
        let b = without_lang(&tree(&hdl, "opamp2.shcomp", &[])[0]);
        assert_eq!(a, b, "after {k} retired");
    }
}

/// Usage and design errors keep the CLI's codes: 2 for arguments, missing files and
/// primitives, 1 for an elaboration error (the tree still prints).
#[test]
fn usage_and_design_errors() {
    let dir = project("errors", "native");
    let (code, out, _) = schemify(&dir, &["--clean", "--help"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("tree <cell|bench> [--config C]... [--json]"),
        "{out}"
    );
    for args in [
        &["tree"][..],
        &["tree", "opamp2.shcomp", "extra.shcomp"],
        &["tree", "opamp2.shcomp", "--dialect", "xyce"],
        &["tree", "opamp2.shcomp", "--config"],
        &["tree", "nowhere.shcomp"],
        &["tree", "mos.shprim"],
    ] {
        let (code, _, err) = schemify(&dir, &[&["--clean"][..], args].concat());
        assert_eq!(code, 2, "{args:?}: {err}");
    }
    let (code, out, err) = schemify(
        &dir,
        &["--clean", "tree", "opamp2_ac.shbench", "--config", "nope"],
    );
    assert_eq!(code, 1, "{out}");
    assert!(err.contains("E035"), "{err}");
    // An unresolved cell is the netlister's E001; the rest of the tree still prints.
    let text = fs::read_to_string(dir.join("stage1.shcomp")).unwrap();
    fs::write(
        dir.join("stage1.shcomp"),
        text.replace("./mirror", "./nowhere"),
    )
    .unwrap();
    retire(&dir, "opamp2");
    retire(&dir, "stage1");
    let (code, out, err) = schemify(&dir, &["--clean", "tree", "opamp2.shcomp"]);
    assert_eq!(code, 1, "{out}");
    assert!(err.contains("E001"), "{err}");
    assert!(
        out.contains("XDP diffpair") && !out.contains("mirror"),
        "{out}"
    );
}
