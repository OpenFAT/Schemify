//! Shared test helpers: projects from file texts, ids, coordinates, elaboration.

#![allow(dead_code)] // each test binary uses a subset

use schemify_doc::{DocHandle, Project};
use schemify_elab::{Dialect, Elab, Request, elaborate, no_funcs};
use std::path::Path;

/// The 6-char id of `n` (a bijection on 30 bits, so ids are unique and scattered).
pub fn id(n: u64) -> String {
    const A: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let v = n.wrapping_mul(0x9e37_79b1) & ((1 << 30) - 1);
    (0..6)
        .rev()
        .map(|k| A[((v >> (5 * k)) & 31) as usize] as char)
        .collect()
}

/// Coordinate text of milli-units.
pub fn c(milli: i64) -> String {
    let mut s = String::new();
    schemify_format::format_coord(milli as i32, &mut s);
    s
}

/// A project with every file indexed in the project library and opened, in the given
/// order.
pub fn project(files: &[(&str, &str)]) -> (Project, Vec<DocHandle>) {
    let mut p = Project::new(None, None, 11).unwrap();
    let listing: Vec<(&str, &[u8])> = files.iter().map(|(n, t)| (*n, t.as_bytes())).collect();
    let (_, diags) = p.index_library("", "", &listing).unwrap();
    assert!(diags.is_empty(), "index diags: {diags:?}");
    let handles = files
        .iter()
        .map(|(n, t)| {
            p.open("", n, t.as_bytes(), None)
                .unwrap_or_else(|e| panic!("{n}: {e:?}"))
                .handle
        })
        .collect();
    (p, handles)
}

pub fn request(dialect: Dialect) -> Request<'static> {
    Request {
        config: None,
        corner: None,
        overrides: &[],
        env: &[],
        dialect,
    }
}

pub fn elab(p: &Project, top: DocHandle, dialect: Dialect) -> Elab {
    elaborate(p, top, &request(dialect), &mut no_funcs)
}

/// The hand-written fixtures (`tests/fixtures/*.sh*`), file name and text, sorted.
pub fn fixtures() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("shprim" | "shcomp" | "shbench")
            )
        })
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .collect();
    out.sort();
    out
}

/// The fixture project and the handle of `top`.
pub fn fixture(files: &[(String, String)], top: &str) -> (Project, DocHandle) {
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    let (p, hs) = project(&refs);
    let k = files
        .iter()
        .position(|f| f.0 == top)
        .expect("fixture exists");
    (p, hs[k])
}
