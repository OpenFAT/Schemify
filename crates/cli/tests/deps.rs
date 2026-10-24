//! D39: the headless core links no GPU, window, text-shaping, accessibility or LaTeX code.
//! Checked on the resolved dependency graph for every target platform, so a new dependency
//! (or a feature that drags one in) fails here instead of in a size measurement. Not
//! `--offline`: `--target all` needs every platform's crate sources, and a host build only
//! fetched its own (CI); cargo fetches the rest.

use std::process::Command;

/// Crates that belong to the editor only.
const EDITOR_ONLY: &[&str] = &[
    "schemify-app",
    "schemify-ui",
    "schemify-render",
    "wgpu",
    "wgpu-core",
    "wgpu-hal",
    "naga",
    "ash",
    "glow",
    "vello",
    "vello_cpu",
    "vello_encoding",
    "winit",
    "smithay-client-toolkit",
    "x11rb",
    "parley",
    "fontique",
    "harfrust",
    "skrifa",
    "read-fonts",
    "swash",
    "taffy",
    "accesskit",
    "accesskit_winit",
    "accesskit_unix",
    "zbus",
    "ratex-parser",
    "ratex-layout",
    "ratex-font",
    "regex",
    "fancy-regex",
    "rustfft",
];

/// The package names `pkg` links on any target (normal and build edges).
fn linked(pkg: &str) -> Vec<String> {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            pkg,
            "-e",
            "normal,build",
            "--target",
            "all",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_owned))
        .collect();
    assert!(
        names.iter().any(|n| n == pkg),
        "{pkg} missing from its own tree: {names:?}"
    );
    names
}

#[test]
fn the_core_links_no_editor_crate() {
    let names = linked("schemify-cli");
    assert!(names.iter().any(|n| n == "omnilua"), "{names:?}");
    let bad: Vec<&String> = names
        .iter()
        .filter(|n| EDITOR_ONLY.contains(&n.as_str()))
        .collect();
    assert!(bad.is_empty(), "the core depends on editor crates: {bad:?}");
}

/// The canvas tables are filled headless by the editor and drawn by `render`: they link
/// geometry (kurbo) and nothing of the GPU, window, text or UI stack.
#[test]
fn the_canvas_tables_link_no_editor_crate() {
    let names = linked("schemify-canvas");
    assert!(names.iter().any(|n| n == "kurbo"), "{names:?}");
    let bad: Vec<&String> = names
        .iter()
        .filter(|n| EDITOR_ONLY.contains(&n.as_str()))
        .collect();
    assert!(
        bad.is_empty(),
        "the canvas tables depend on editor crates: {bad:?}"
    );
}
