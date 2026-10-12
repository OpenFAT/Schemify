//! End-to-end checks of the headless CLI spines (exit codes are the contract, D27).

use std::path::{Path, PathBuf};
use std::process::Command;

const CANONICAL: &str = include_str!("../../format/tests/fixtures/ota5t.shcomp");
const MESSY: &str = include_str!("../../format/tests/fixtures/ota5t.messy.shcomp");

/// Runs the core in `dir` with the user directories inside it, so the developer's own
/// `~/.config/schemify/init.lua` never runs here.
fn schemify(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_schemify"))
        .args(args)
        .current_dir(dir)
        .env("XDG_CONFIG_HOME", dir.join(".xdg/config"))
        .env("XDG_DATA_HOME", dir.join(".xdg/data"))
        .env_remove("SCHEMIFY_APPNAME")
        .output()
        .expect("binary runs");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code().expect("not killed"), text)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("schemify-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

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
    lines.join("\n") + "\n"
}

#[test]
fn fmt_check_and_rewrite() {
    let dir = scratch("fmt");
    std::fs::write(dir.join("a.shcomp"), CANONICAL).unwrap();
    std::fs::write(dir.join("b.shcomp"), MESSY).unwrap();
    assert_eq!(schemify(&dir, &["fmt", "--check", "a.shcomp"]).0, 0);
    assert_eq!(schemify(&dir, &["fmt", "--check", "b.shcomp"]).0, 1);
    assert_eq!(schemify(&dir, &["fmt", "b.shcomp"]).0, 0);
    assert_eq!(
        std::fs::read_to_string(dir.join("b.shcomp")).unwrap(),
        CANONICAL
    );
    assert_eq!(schemify(&dir, &["fmt", "--check", "b.shcomp"]).0, 0);
}

#[test]
fn fmt_reports_parse_errors_with_path() {
    let dir = scratch("fmt-err");
    std::fs::write(dir.join("bad.shcomp"), "schemify 1 shcomp\nwire zz 0 0\n").unwrap();
    let (code, text) = schemify(&dir, &["fmt", "bad.shcomp"]);
    assert_eq!(code, 1);
    assert!(text.starts_with("bad.shcomp:"), "{text}");
}

#[test]
fn merge_driver_merges_independent_edits_and_flags_conflicts() {
    let dir = scratch("merge");
    let moved = edit_attr(CANONICAL, 0, "at ", "  at 999 -300 R0");
    let resized = edit_attr(CANONICAL, 1, "W ", "  W 7");
    std::fs::write(dir.join("O"), CANONICAL).unwrap();
    std::fs::write(dir.join("A"), &moved).unwrap();
    std::fs::write(dir.join("B"), &resized).unwrap();
    assert_eq!(
        schemify(&dir, &["merge-driver", "O", "A", "B", "x.shcomp"]).0,
        0
    );
    let merged = std::fs::read_to_string(dir.join("A")).unwrap();
    assert!(merged.contains("  at 999 -300 R0") && merged.contains("  W 7"));

    // Same attribute changed differently on both sides: conflict, exit 1.
    std::fs::write(dir.join("A"), edit_attr(CANONICAL, 1, "W ", "  W 3")).unwrap();
    let (code, text) = schemify(&dir, &["merge-driver", "O", "A", "B", "x.shcomp"]);
    assert_eq!(code, 1, "{text}");
    assert!(
        std::fs::read_to_string(dir.join("A"))
            .unwrap()
            .contains("<<<<<<<")
    );
}

#[test]
fn diff_lists_changed_records_and_exit_code() {
    let dir = scratch("diff");
    std::fs::write(dir.join("a.shcomp"), CANONICAL).unwrap();
    std::fs::write(dir.join("b.shcomp"), edit_attr(CANONICAL, 1, "W ", "  W 7")).unwrap();
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
}

/// The editor's fixture project: a two-inverter buffer over a hierarchical `inv`.
fn inv_top() -> String {
    format!(
        "{}/../app/tests/fixtures/inv/top.shcomp",
        env!("CARGO_MANIFEST_DIR")
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
    let cdl = std::fs::read_to_string(dir.join("top.cdl")).unwrap();
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
    std::fs::write(dir.join("schemify.project"), "schemify 1 project\n").unwrap();
    std::fs::write(
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
    std::fs::write(dir.join("a.shcomp"), CANONICAL).unwrap();
    std::fs::write(
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
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("init.lua"), "print('from init')").unwrap();
    let (_, text) = schemify(&dir, &["fmt", "--check", "a.shcomp"]);
    assert!(text.contains("note: from init"), "{text}");

    // pack: a local package, synced and locked next to init.lua.
    let pkg = dir.join("pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("demo.wasm"), b"demo").unwrap();
    let init = format!(
        "schemify.pack.add {{ {{ path = [==[{}]==], name = 'demo' }} }}",
        pkg.display()
    );
    std::fs::write(cfg.join("init.lua"), init).unwrap();
    let (code, text) = schemify(&dir, &["pack", "status"]);
    assert_eq!((code, text.trim()), (0, "demo: path - unlocked"));
    let (code, text) = schemify(&dir, &["pack", "sync"]);
    assert_eq!((code, text.trim()), (0, "demo: local"));
    let lock = std::fs::read_to_string(cfg.join("schemify-pack.lock")).unwrap();
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

/// A bench whose two configs reach different cells: `beh` makes `amp` a leaf (no `res`),
/// `sch` (the default) reaches `res` through amp's schematic.
const AMP: &str = "schemify 1 shcomp\n  default sch\n\npin IN in 0 0\npin OUT out 0 20\n\n\
impl sch schematic\nimpl beh spice\n  format spice \"R{name} {IN} {OUT} 2k\"\n\n\
inst 0r0000 ./res\n  at 0 0 R0\n  name R1\n\nport 0p0001 0 0 R0 IN\nport 0p0002 0 20 R0 OUT\n";
const RES: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\n\
impl spice spice\n  format spice \"R{name} {A} {B} 1k\"\n";
const BENCH: &str = "schemify 1 shbench\n\ninst 0x0001 ./amp\n  at 0 0 R0\n  name X1\n\n\
label 0n0001 0 0 R0 a\nlabel 0n0002 0 20 R0 0\n\n\
config beh\n  cell ./amp beh\nconfig sch\n  order sch\n\ncode op spice\n  | V1 a 0 1\n  | .op\n";

/// Writes an ASCII raw file (one operating point) for the netlist `$1` into `$2`; with
/// `$WANT` set, refuses a netlist whose header is not `(schemify $WANT)` and, with `$ZIG`
/// unset, an espice one.
const FAKE: &str = r#"if [ -n "$WANT" ]; then head -1 "$1" | grep -q "(schemify $WANT)" || { echo "not $WANT" >&2; exit 3; }; fi
if head -1 "$1" | grep -q espice && [ -z "$ZIG" ]; then echo 'no ZIG' >&2; exit 4; fi
printf 'Title: fake\nPlotname: Operating Point\nFlags: real\nNo. Variables: 1\nNo. Points: 1\nVariables:\n 0 v(a) voltage\nValues:\n0 1\n' > "$2"
"#;

fn run_project(name: &str) -> PathBuf {
    let dir = scratch(name);
    std::fs::write(dir.join("schemify.project"), "schemify 1 project\n").unwrap();
    std::fs::write(dir.join("amp.shcomp"), AMP).unwrap();
    std::fs::write(dir.join("res.shprim"), RES).unwrap();
    std::fs::write(dir.join("tb.shbench"), BENCH).unwrap();
    dir
}

/// `init.lua` lines naming the fake simulator for `sim` (with `extra` fields).
fn fake_sim(sim: &str, extra: &str) -> String {
    format!(
        "schemify.sim('{sim}', {{ path = 'sh', args = {{ '-c', [==[{FAKE}]==], 'fake', \
         '{{netlist}}', '{{raw}}' }}{extra} }})\n"
    )
}

/// Bug fix: `run` with several `--config`s opens the cells every config reaches, not only
/// the first's (the second config's job used to fail "cells not open").
#[test]
fn run_opens_the_cells_of_every_selected_config() {
    let dir = run_project("run-configs");
    std::fs::write(dir.join("init.lua"), fake_sim("ngspice", "")).unwrap();
    for order in [["beh", "sch"], ["sch", "beh"]] {
        let (code, text) = schemify(
            &dir,
            &[
                "-u",
                "init.lua",
                "run",
                "tb.shbench",
                "--config",
                order[0],
                "--config",
                order[1],
            ],
        );
        assert_eq!(code, 0, "{order:?}: {text}");
        assert_eq!(text.matches(" done\n").count(), 2, "{text}");
        assert!(!text.contains("not open"), "{text}");
    }
    // Negative space: a config the bench lacks still refuses the run.
    let (code, text) = schemify(
        &dir,
        &["-u", "init.lua", "run", "tb.shbench", "--config", "nope"],
    );
    assert_ne!(code, 0, "{text}");
}

/// `run --dialect espice` and `schemify.opt.simulator = "espice"` netlist for espice and start
/// the `espice` sim row with its `env`; `netlist --dialect espice` writes the espice text;
/// a dialect no simulator reads is a usage error for `run`.
#[test]
fn run_and_netlist_select_espice() {
    let dir = run_project("run-espice");
    let init = fake_sim("espice", ", env = { ZIG = '/z/zig', WANT = 'espice' }")
        + &fake_sim("ngspice", ", env = { WANT = 'ngspice' }");
    std::fs::write(dir.join("init.lua"), &init).unwrap();
    let run = |args: &[&str]| {
        let mut all = vec!["-u", "init.lua", "run", "tb.shbench"];
        all.extend_from_slice(args);
        schemify(&dir, &all)
    };
    let (code, text) = run(&["--dialect", "espice"]);
    assert_eq!((code, text.matches(" done\n").count()), (0, 1), "{text}");
    let (code, text) = run(&[]);
    assert_eq!(code, 0, "ngspice by default: {text}");
    std::fs::write(
        dir.join("init.lua"),
        init.clone() + "schemify.opt.simulator = 'espice'\n",
    )
    .unwrap();
    let (code, text) = run(&[]);
    assert_eq!(code, 0, "the configured simulator: {text}");
    let (code, text) = run(&["--dialect", "ngspice"]);
    assert_eq!(code, 0, "--dialect wins: {text}");
    assert_eq!(run(&["--dialect", "xyce"]).0, 2);
    assert_eq!(run(&["--dialect", "bogus"]).0, 2);
    let (code, text) = schemify(&dir, &["netlist", "tb.shbench", "--dialect", "espice"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.starts_with("* tb (schemify espice)\n"), "{text}");
}
