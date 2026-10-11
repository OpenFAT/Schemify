//! D39: the headless core links no GPU, window, text-shaping, accessibility or LaTeX code.
//! Checked on the resolved dependency graph for every target platform, so a new dependency
//! (or a feature that drags one in) fails here instead of in a size measurement.

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

#[test]
fn the_core_links_no_editor_crate() {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--offline",
            "-p",
            "schemify-cli",
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
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let names: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert!(
        names.contains(&"schemify-cli") && names.contains(&"mlua"),
        "{names:?}"
    );
    let bad: Vec<&&str> = names.iter().filter(|n| EDITOR_ONLY.contains(n)).collect();
    assert!(bad.is_empty(), "the core depends on editor crates: {bad:?}");
}
