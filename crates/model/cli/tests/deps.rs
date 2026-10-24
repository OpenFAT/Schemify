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

/// The editor's model and frame spine run headless (`schemify-gui cmd`, the JSON-RPC
/// server's tables, frame tests): the GPU, window and accessibility-backend crates belong
/// to `schemify-app` and `schemify-render` only. Text shaping, layout and LaTeX are the UI
/// tables' (`schemify-ui`), so they stay.
#[test]
fn the_editor_links_no_gpu_or_window_crate() {
    const GPU_OR_WINDOW: &[&str] = &[
        "schemify-app",
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
        "accesskit_winit",
        "accesskit_unix",
        "zbus",
    ];
    let names = linked("schemify-editor");
    assert!(names.iter().any(|n| n == "schemify-ui"), "{names:?}");
    let bad: Vec<&String> = names
        .iter()
        .filter(|n| GPU_OR_WINDOW.contains(&n.as_str()))
        .collect();
    assert!(
        bad.is_empty(),
        "the editor depends on GPU or window crates: {bad:?}"
    );
}

/// The crate folders are layers of the GUI architecture (docs/dev/architecture.md § Crates):
/// `protocol` (wire and file formats) ← `plugins` (the plugin host) ← `model` (owned tables,
/// their transforms, the frame spine, all headless) ← `view` (pixels and the window). A crate
/// depends only on crates of its own layer or a lower one, so the model never reaches the
/// view. Checked on every member's `[dependencies]` and `[build-dependencies]`.
#[test]
fn crate_folders_depend_only_downward() {
    const LAYERS: [&str; 4] = ["protocol", "plugins", "model", "view"];
    let crates = std::path::Path::new(concat!(env!("SCHEMIFY_ROOT"), "/crates"));
    let mut layer_of: Vec<(String, usize)> = Vec::new();
    let mut manifests: Vec<(String, usize, String)> = Vec::new();
    for (rank, layer) in LAYERS.iter().enumerate() {
        for e in std::fs::read_dir(crates.join(layer)).expect("layer folder") {
            let dir = e.expect("entry").path();
            let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
                continue;
            };
            let name = format!("schemify-{}", dir.file_name().unwrap().to_string_lossy());
            assert!(
                text.contains(&format!("name = \"{name}\"")),
                "{name}: folder = crate name"
            );
            layer_of.push((name.clone(), rank));
            manifests.push((name, rank, text));
        }
    }
    assert!(
        manifests.len() >= 16,
        "every crate sits in a layer: {layer_of:?}"
    );
    let mut bad = Vec::new();
    for (name, rank, text) in &manifests {
        let mut section = "";
        for line in text.lines() {
            if line.starts_with('[') {
                section = line;
                continue;
            }
            let normal = section.ends_with("dependencies]") && !section.contains("dev-");
            let Some(dep) = line
                .split(['=', '.', ' '])
                .next()
                .filter(|d| d.starts_with("schemify-"))
            else {
                continue;
            };
            let Some(&(_, dep_rank)) = layer_of.iter().find(|(n, _)| n == dep) else {
                continue;
            };
            if normal && dep_rank > *rank {
                bad.push(format!(
                    "{name} ({}) → {dep} ({})",
                    LAYERS[*rank], LAYERS[dep_rank]
                ));
            }
        }
    }
    assert!(bad.is_empty(), "upward dependencies: {bad:?}");
}

/// Paths name their anchor: a crate's own files from `CARGO_MANIFEST_DIR`, any other
/// repository file from `SCHEMIFY_ROOT` (`.cargo/config.toml`), never a `../` chain that
/// breaks when a crate moves. `SCHEMIFY_ROOT` stays out of the sources the plugin
/// repositories compile as git dependencies (they do not read this repository's cargo
/// config): only tests and the `ui`, `editor` and `app` crates may name it.
#[test]
fn paths_are_anchored_at_the_crate_or_the_root() {
    const ROOT_IN_SRC: [&str; 3] = [
        "crates/model/ui/",
        "crates/model/editor/",
        "crates/view/app/",
    ];
    let root = std::path::Path::new(env!("SCHEMIFY_ROOT"));
    let mut stack = vec![root.join("crates")];
    let (mut bad, mut files) = (Vec::new(), 0);
    while let Some(dir) = stack.pop() {
        assert!(files < 100_000, "a bounded tree");
        for e in std::fs::read_dir(&dir).expect("dir") {
            let p = e.expect("entry").path();
            let name = p.file_name().unwrap().to_string_lossy();
            if p.is_dir() {
                if !matches!(name.as_ref(), "target" | "corpus" | "artifacts") {
                    stack.push(p);
                }
                continue;
            }
            if !name.ends_with(".rs") {
                continue;
            }
            files += 1;
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel.ends_with("cli/tests/deps.rs") {
                continue; // this test spells the patterns it looks for
            }
            let test = rel.contains("/tests/") || rel.contains("/fuzz/");
            let text = std::fs::read_to_string(&p).expect("utf-8 source");
            for (n, line) in text.lines().enumerate() {
                let chain = (line.contains("include") && line.contains("!(\"../"))
                    || (line.contains("CARGO_MANIFEST_DIR") && line.contains("/../"));
                let rooted = line.contains("SCHEMIFY_ROOT")
                    && !test
                    && !ROOT_IN_SRC.iter().any(|c| rel.starts_with(c));
                if chain || rooted {
                    bad.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                }
            }
        }
    }
    assert!(files > 100, "the scan saw the sources: {files}");
    assert!(bad.is_empty(), "unanchored or misplaced paths: {bad:#?}");
}
