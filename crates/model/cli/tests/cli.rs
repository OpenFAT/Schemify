//! End-to-end checks of the headless CLI spines (exit codes are the contract, D27).

mod common;

use common::{schemify, scratch};
use std::env;
use std::fs;

const CANONICAL: &str = include_str!(concat!(
    env!("SCHEMIFY_ROOT"),
    "/crates/protocol/format/tests/fixtures/ota5t.shcomp"
));
const MESSY: &str = include_str!(concat!(
    env!("SCHEMIFY_ROOT"),
    "/crates/protocol/format/tests/fixtures/ota5t.messy.shcomp"
));

// ---- private ----

/// Replaces the first attribute line starting with `attr` under the
/// `nth_inst`-th `inst` record.
fn edit_attr(text: &str, nth_inst: usize, attr: &str, new_line: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let inst = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("inst "))
        .nth(nth_inst)
        .map(|(i, _)| i)
        .expect("fixture has instances");
    let at = (inst + 1..lines.len())
        .find(|&i| lines[i].trim_start().starts_with(attr))
        .expect("attribute present");
    lines[at] = new_line.to_owned();
    format!("{}\n", lines.join("\n"))
}

#[test]
fn fmt_check_and_rewrite() {
    let dir = scratch("fmt");
    fs::write(dir.join("a.shcomp"), CANONICAL).unwrap();
    fs::write(dir.join("b.shcomp"), MESSY).unwrap();
    assert_eq!(schemify(&dir, &["fmt", "--check", "a.shcomp"]).0, 0);
    assert_eq!(schemify(&dir, &["fmt", "--check", "b.shcomp"]).0, 1);
    assert_eq!(schemify(&dir, &["fmt", "b.shcomp"]).0, 0);
    assert_eq!(fs::read_to_string(dir.join("b.shcomp")).unwrap(), CANONICAL);
    assert_eq!(schemify(&dir, &["fmt", "--check", "b.shcomp"]).0, 0);
}

#[test]
fn fmt_reports_parse_errors_with_path() {
    let dir = scratch("fmt-err");
    fs::write(dir.join("bad.shcomp"), "schemify 1 shcomp\nwire zz 0 0\n").unwrap();
    let (code, text) = schemify(&dir, &["fmt", "bad.shcomp"]);
    assert_eq!(code, 1);
    assert!(text.starts_with("bad.shcomp:"), "{text}");
}

#[test]
fn merge_driver_merges_independent_edits_and_flags_conflicts() {
    let dir = scratch("merge");
    let moved = edit_attr(CANONICAL, 0, "at ", "  at 999 -300 R0");
    let resized = edit_attr(CANONICAL, 1, "W ", "  W 7");
    fs::write(dir.join("O"), CANONICAL).unwrap();
    fs::write(dir.join("A"), &moved).unwrap();
    fs::write(dir.join("B"), &resized).unwrap();
    assert_eq!(
        schemify(&dir, &["merge-driver", "O", "A", "B", "x.shcomp"]).0,
        0
    );
    let merged = fs::read_to_string(dir.join("A")).unwrap();
    assert!(merged.contains("  at 999 -300 R0") && merged.contains("  W 7"));

    // Same attribute changed differently on both sides: conflict, exit 1.
    fs::write(dir.join("A"), edit_attr(CANONICAL, 1, "W ", "  W 3")).unwrap();
    let (code, text) = schemify(&dir, &["merge-driver", "O", "A", "B", "x.shcomp"]);
    assert_eq!(code, 1, "{text}");
    assert!(
        fs::read_to_string(dir.join("A"))
            .unwrap()
            .contains("<<<<<<<")
    );
}

#[test]
fn diff_lists_changed_records_and_exit_code() {
    let dir = scratch("diff");
    fs::write(dir.join("a.shcomp"), CANONICAL).unwrap();
    fs::write(dir.join("b.shcomp"), edit_attr(CANONICAL, 1, "W ", "  W 7")).unwrap();
    assert_eq!(
        schemify(&dir, &["diff", "a.shcomp", "a.shcomp"]),
        (0, String::new())
    );
    let (code, text) = schemify(&dir, &["diff", "a.shcomp", "b.shcomp"]);
    assert_eq!(code, 1);
    assert_eq!(text, "~ inst 1j446r [attrs: W]\n");
}

#[test]
fn usage_errors_exit_2() {
    let dir = scratch("usage");
    assert_eq!(schemify(&dir, &[]).0, 2);
    assert_eq!(schemify(&dir, &["frobnicate"]).0, 2);
    assert_eq!(schemify(&dir, &["merge-driver", "O"]).0, 2);
    assert_eq!(schemify(&dir, &["diff", "missing-a", "missing-b"]).0, 2);
    // Every command refuses arguments it does not take, before doing any work.
    for args in [
        &["fmt"][..],
        &["fmt", "--check"],
        &["git-setup", "extra"],
        &["rpc", "--bogus"],
        &["rpc", "--listen"],
        &["mcp", "extra"],
        &["publish", "."],
        &["publish", "-o", "site"],
        &["check"],
        &["run", "--jobs", "0", "tb.shbench"],
        &["netlist", "a.shcomp", "b.shcomp"],
        &["netlist", "a.shcomp", "--set", "noequals"],
    ] {
        let (code, text) = schemify(&dir, args);
        assert_eq!(code, 2, "{args:?}: {text}");
        assert!(text.contains("usage: schemify"), "{args:?}: {text}");
    }
    assert!(!dir.join("site").exists(), "publish wrote nothing");
    let (code, text) = schemify(&dir, &["--version"]);
    assert_eq!(code, 0);
    assert_eq!(text, format!("schemify {}\n", env!("CARGO_PKG_VERSION")));
    let (code, text) = schemify(&dir, &["help"]);
    assert_eq!(code, 0);
    assert!(text.starts_with("usage: schemify"), "{text}");
}

/// The editor's fixture project: a two-inverter buffer over a hierarchical `inv`.
fn inv_top() -> String {
    format!(
        "{}/crates/view/app/tests/fixtures/inv/top.shcomp",
        env!("SCHEMIFY_ROOT")
    )
}

#[test]
fn netlist_and_check_a_project_cell() {
    let dir = scratch("netlist");
    let top = inv_top();
    assert_eq!(schemify(&dir, &["check", &top]).0, 0);
    let (code, text) = schemify(&dir, &["netlist", &top]);
    assert_eq!(code, 0, "{text}");
    // x1 overrides a param: its subckt is specialized; x2 uses the default definition.
    assert!(text.contains(".subckt inv A Y VDD VSS"), "{text}");
    assert!(text.contains("inv__"), "{text}");
    assert!(text.trim_end().ends_with(".end"), "{text}");
    let (code, cdl) = schemify(
        &dir,
        &["netlist", &top, "--dialect", "cdl", "-o", "top.cdl"],
    );
    assert_eq!(code, 0, "{cdl}");
    let cdl = fs::read_to_string(dir.join("top.cdl")).unwrap();
    assert!(
        cdl.contains(".SUBCKT top IN OUT") && cdl.contains("*.PININFO"),
        "{cdl}"
    );
    // Determinism: the same input netlists to the same bytes.
    assert_eq!(schemify(&dir, &["netlist", &top]).1, text);
}

#[test]
fn netlist_gate_and_usage_errors() {
    let dir = scratch("netlist-gate");
    // A cell referencing a missing library cell: E001 blocks the netlist (exit 1).
    fs::write(dir.join("schemify.project"), "schemify 1 project\n").unwrap();
    fs::write(
        dir.join("bad.shbench"),
        "schemify 1 shbench\n\ninst 0aaaaa ./nowhere\n  at 0 0 R0\n  name X1\n",
    )
    .unwrap();
    let (code, text) = schemify(&dir, &["netlist", "bad.shbench"]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("E001"), "{text}");
    assert_eq!(schemify(&dir, &["check", "bad.shbench"]).0, 1);
    assert_eq!(
        schemify(&dir, &["netlist", "bad.shbench", "--allow-errors"]).0,
        0
    );
    assert_eq!(
        schemify(&dir, &["netlist", &inv_top(), "--dialect", "bogus"]).0,
        2
    );
    assert_eq!(schemify(&dir, &["netlist"]).0, 2);
}

/// Startup flags and the user config (config spec §2): errors are reported with `file:line`
/// and never stop a command; `--clean` skips the config; `pack` works end to end.
#[test]
fn user_config_flags_and_pack_commands() {
    let dir = scratch("config");
    fs::write(dir.join("a.shcomp"), CANONICAL).unwrap();
    fs::write(
        dir.join("bad.lua"),
        "schemify.opt.snap = 5\nschemify.opt.nope = 1\n",
    )
    .unwrap();
    let (code, text) = schemify(&dir, &["-u", "bad.lua", "fmt", "--check", "a.shcomp"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("error: bad.lua:2: unknown option `nope`"),
        "{text}"
    );
    assert_eq!(
        schemify(&dir, &["--clean", "fmt", "--check", "a.shcomp"]),
        (0, String::new())
    );
    assert_eq!(schemify(&dir, &["--clean", "-u", "x.lua", "fmt"]).0, 2);
    // The default config is <XDG_CONFIG_HOME>/schemify/init.lua.
    let cfg = dir.join(".xdg/config/schemify");
    fs::create_dir_all(&cfg).unwrap();
    fs::write(cfg.join("init.lua"), "print('from init')").unwrap();
    let (_, text) = schemify(&dir, &["fmt", "--check", "a.shcomp"]);
    assert!(text.contains("note: from init"), "{text}");

    // pack: a local package, synced and locked next to init.lua.
    let pkg = dir.join("pkg");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("demo.wasm"), b"demo").unwrap();
    let init = format!(
        "schemify.pack.add {{ {{ path = [==[{}]==], name = 'demo' }} }}",
        pkg.display()
    );
    fs::write(cfg.join("init.lua"), init).unwrap();
    let (code, text) = schemify(&dir, &["pack", "status"]);
    assert_eq!((code, text.trim()), (0, "demo: path - unlocked"));
    let (code, text) = schemify(&dir, &["pack", "sync"]);
    assert_eq!((code, text.trim()), (0, "demo: local"));
    let lock = fs::read_to_string(cfg.join("schemify-pack.lock")).unwrap();
    assert!(
        lock.starts_with("schemify 1 lock\n") && lock.contains("locked plugin demo path"),
        "{lock}"
    );
    let (code, text) = schemify(&dir, &["pack", "status"]);
    assert_eq!(code, 0);
    assert!(text.trim().ends_with(" ok"), "{text}");
    assert_eq!(schemify(&dir, &["pack"]).0, 2);
    assert_eq!(schemify(&dir, &["pack", "frobnicate"]).0, 2);
    assert_eq!(
        schemify(&dir, &["--clean", "pack", "status"]).0,
        2,
        "no config, no packages"
    );
}

/// `--version` (and `-V`, `version`) prints the package version and exits 0: what the distro
/// packages' smoke tests and `nix run …#latest -- --version` check.
#[test]
fn version_prints_the_package_version() {
    let dir = env::temp_dir();
    for flag in ["--version", "-V", "version"] {
        let (code, text) = schemify(&dir, &[flag]);
        assert_eq!(code, 0, "{flag}: {text}");
        assert_eq!(
            text,
            format!("schemify {}\n", env!("CARGO_PKG_VERSION")),
            "{flag}"
        );
    }
}
