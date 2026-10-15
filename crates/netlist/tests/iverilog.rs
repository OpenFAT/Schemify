//! Second oracle for the structural Verilog dialect (docs/dev/testing.md, netlist row): xschem has
//! no Verilog golden, so Icarus Verilog checks that our netlists are Verilog. Every fixture
//! cell that elaborates for verilog without a blocking finding is compiled by `iverilog`
//! (parse + elaboration: every instantiated module must exist, ports must match by name).
//!
//! Runs when `SCHEMIFY_IVERILOG` names the binary (CI installs Icarus on Linux); otherwise
//! it prints a skip note and passes.

mod common;

use common::*;
use schemify_elab::Dialect;
use schemify_netlist::emit;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Compiles `text` as `<dir>/<name>.v`; `Err(stderr)` when Icarus rejects it.
fn icarus(bin: &Path, dir: &Path, name: &str, text: &str) -> Result<(), String> {
    let src = dir.join(format!("{name}.v"));
    std::fs::write(&src, text).unwrap();
    let out = Command::new(bin)
        .arg("-o")
        .arg(dir.join(format!("{name}.vvp")))
        .arg(&src)
        .output()
        .expect("iverilog runs");
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

#[test]
fn icarus_parses_and_elaborates_the_verilog_dialect() {
    let Some(bin) = std::env::var_os("SCHEMIFY_IVERILOG").map(PathBuf::from) else {
        eprintln!("SCHEMIFY_IVERILOG unset: Icarus Verilog oracle skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!("schemify-iverilog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Negative space: Icarus elaborates, so an undefined module fails (the oracle is not
    // a mere parser), and so does a port our module does not have.
    let undefined = "module t ();\n  wire a;\n  missing u1 (.A(a));\nendmodule\n";
    assert!(icarus(&bin, &dir, "neg_undefined", undefined).is_err());
    let bad_port = "module t ();\n  wire a;\n  b u1 (.Q(a));\nendmodule\nmodule b (A);\n  input A;\nendmodule\n";
    assert!(icarus(&bin, &dir, "neg_port", bad_port).is_err());

    let files = fixtures();
    let mut checked = Vec::new();
    for (name, _) in files
        .iter()
        .filter(|f| f.0.ends_with(".shbench") || f.0.ends_with(".shcomp"))
    {
        let (p, top) = fixture(&files, name);
        let e = elab(&p, top, Dialect::Verilog);
        if e.violations.blocking() {
            continue; // leaves without a Verilog view (E013): no netlist to check
        }
        let text = emit(&e, Dialect::Verilog).text;
        let stem = name.replace('.', "_");
        if let Err(why) = icarus(&bin, &dir, &stem, &text) {
            panic!("{name}: Icarus Verilog rejects our netlist:\n{why}\n{text}");
        }
        checked.push(stem);
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        checked.iter().any(|c| c == "dig_tb_shbench"),
        "the digital bench is checked: {checked:?}"
    );
}
