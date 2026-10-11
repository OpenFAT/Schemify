//! End-to-end checks of the headless CLI spines (exit codes are the contract, D27).

use std::path::{Path, PathBuf};
use std::process::Command;

const CANONICAL: &str = include_str!("../../format/tests/fixtures/ota5t.shcomp");
const MESSY: &str = include_str!("../../format/tests/fixtures/ota5t.messy.shcomp");

fn schemify(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_schemify"))
        .args(args)
        .current_dir(dir)
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
        "{}/tests/fixtures/inv/top.shcomp",
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
