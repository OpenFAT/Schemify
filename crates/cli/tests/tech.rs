//! Technology classes end to end (pdk spec §5, §6, §10): one `tech:` inverter netlisted
//! under the shipped sky130 and gf180mcu maps (`plugins/pdk-manager/maps/`) against tiny
//! stand-in PDK libraries yields each PDK's device lines with the mapped params (golden
//! netlists, re-blessed with `SCHEMIFY_BLESS=1`); without `lib tech`, with an unknown class
//! or an unknown flavor it is refused with the reason.

use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tech");
const MAPS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../plugins/pdk-manager/maps"
);

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

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_tree(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// The fixture project with the shipped maps under `maps/` and `manifest` as its manifest.
fn project(name: &str, manifest: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("schemify-tech-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy_tree(Path::new(FIXTURE), &dir);
    std::fs::remove_dir_all(dir.join("golden")).ok();
    copy_tree(Path::new(MAPS), &dir.join("maps"));
    std::fs::write(dir.join("schemify.project"), manifest).unwrap();
    dir
}

fn manifest(family: &str, lib: &str) -> String {
    format!(
        "schemify 1 project\nlib {lib} path pdk/{family}/{lib}\nlib tech map maps/{family}.map\n"
    )
}

fn golden(name: &str, text: &str) {
    let path = Path::new(FIXTURE).join("golden").join(name);
    if std::env::var_os("SCHEMIFY_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{name}: bless it"));
    assert_eq!(text, want, "{name}");
}

#[test]
fn one_inverter_two_technologies() {
    for (family, lib) in [("sky130", "sky130_fd_pr"), ("gf180mcu", "gf180mcu_fd_pr")] {
        let dir = project(family, &manifest(family, lib));
        let (code, text) = schemify(&dir, &["netlist", "inv.shcomp"]);
        assert_eq!(code, 0, "{family}: {text}");
        golden(&format!("inv.{family}.spice"), &text);
    }
}

#[test]
fn shipped_maps_parse() {
    for family in ["sky130", "gf180mcu", "ihp-sg13g2"] {
        let bytes = std::fs::read(Path::new(MAPS).join(format!("{family}.map"))).unwrap();
        let map = schemify_doc::TechMap::parse(&bytes).expect(family);
        assert_eq!(map.pdk, family);
        for class in ["nmos", "pmos"] {
            assert!(map.find(class, "core").is_some(), "{family} {class}");
        }
    }
}

#[test]
fn refused_without_technology_or_class() {
    // No `lib tech`: E001 "no technology selected".
    let dir = project(
        "none",
        "schemify 1 project\nlib sky130_fd_pr path pdk/sky130/sky130_fd_pr\n",
    );
    let (code, text) = schemify(&dir, &["netlist", "inv.shcomp"]);
    assert_ne!(code, 0, "{text}");
    assert!(
        text.contains("E001") && text.contains("no technology selected"),
        "{text}"
    );
    // A flavor the map lacks.
    let dir = project("flavor", &manifest("sky130", "sky130_fd_pr"));
    let inv = std::fs::read_to_string(dir.join("inv.shcomp")).unwrap();
    let lvt = inv.replacen("  nf 2\n", "  nf 2\n  flavor lvt\n", 1);
    std::fs::write(dir.join("inv.shcomp"), lvt).unwrap();
    let (code, text) = schemify(&dir, &["netlist", "inv.shcomp"]);
    assert_ne!(code, 0, "{text}");
    assert!(text.contains("sky130 has no lvt pmos"), "{text}");
    // Not a technology class.
    let bjt = inv.replacen("tech:nmos", "tech:npn", 1);
    std::fs::write(dir.join("inv.shcomp"), bjt).unwrap();
    let (code, text) = schemify(&dir, &["netlist", "inv.shcomp"]);
    assert_ne!(code, 0, "{text}");
    assert!(text.contains("`npn` is not a technology class"), "{text}");
}
