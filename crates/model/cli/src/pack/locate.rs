//! Installed packages → what the plugin host loads (config spec §5, plugin-abi §5, D7): the
//! module file of each spec's package directory and the grants it gets, requested ∩ granted.
//!
//! Contents:
//! - `pub const READ_ANY: &str`
//! - `pub struct Installed { name, wasm, grants, denied, unused, fuel, lazy_cmd, lazy_event }`
//! - `pub fn locate(&PackSpec, &Paths) -> Result<Installed, String>`
//! - `pub fn installed(&Table, &Paths) -> (Vec<Installed>, Vec<String>)`
//! - for `pack`: `struct Module`, `Installed::grant_notes`, `fn package_dir`, `fn locate_in`

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use schemify_config::{Event, PackSpec, Source, Table};
use schemify_doc::Project;

use super::{MANIFEST, Paths};

/// The package request that takes every `read:<NAME>` grant the user gives (config spec §5).
pub const READ_ANY: &str = "read:*";

/// An installed package, ready for the plugin host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// Package name (the plugin's name in the host).
    pub name: String,
    /// The module.
    pub wasm: PathBuf,
    /// What the plugin gets: requested ∩ granted, in request order (D7).
    pub grants: Vec<String>,
    /// Requested but not granted.
    pub denied: Vec<String>,
    /// Granted but not requested.
    pub unused: Vec<String>,
    /// Spec `fuel`, else the manifest's, else `None` (the host default).
    pub fuel: Option<u64>,
    /// Commands that load it lazily.
    pub lazy_cmd: Vec<String>,
    /// Events that load it lazily.
    pub lazy_event: Vec<Event>,
}

/// A module found in a package directory: its file, requested grants (`None` without a
/// manifest) and manifest fuel.
pub(super) struct Module {
    pub(super) wasm: PathBuf,
    requested: Option<Vec<String>>,
    fuel: Option<u64>,
}

/// Finds the installed module of `spec` and computes its grants (config spec §5).
///
/// # Errors
///
/// Not installed (run `schemify pack sync`), or a manifest that does not parse.
///
/// # Panics
///
/// Never: the asserts check the grant arithmetic here.
pub fn locate(spec: &PackSpec, paths: &Paths) -> Result<Installed, String> {
    let m = locate_in(&package_dir(spec, paths), &spec.name)?;
    if !m.wasm.is_file() {
        return Err(format!(
            "not installed (no {}); run `schemify pack sync`",
            m.wasm.display()
        ));
    }
    let requested = m.requested.unwrap_or_else(|| spec.grant.clone());
    // A request `read:*` takes every `read:<NAME>` the user grants (plugin-abi §5): an
    // importer cannot know which variables a user will point it at, the user names each one.
    let covers = |req: &str, g: &str| req == g || (req == READ_ANY && g.starts_with("read:"));
    let mut grants: Vec<String> = Vec::with_capacity(spec.grant.len());
    for req in &requested {
        let covered = spec.grant.iter().filter(|g| covers(req, g));
        for g in covered {
            if !grants.contains(g) {
                grants.push(g.clone());
            }
        }
    }
    let denied: Vec<String> = requested
        .iter()
        .filter(|r| *r != READ_ANY && !spec.grant.contains(r))
        .cloned()
        .collect();
    let unused: Vec<String> = spec
        .grant
        .iter()
        .filter(|g| !requested.iter().any(|r| covers(r, g)))
        .cloned()
        .collect();
    assert!(grants.len() <= spec.grant.len());
    // D7: a plugin gets only what the user granted, and never what it was denied.
    for g in &grants {
        assert!(
            spec.grant.contains(g),
            "{g}: granted without the user's grant"
        );
        assert!(!denied.contains(g), "{g}: both granted and denied");
    }
    Ok(Installed {
        name: spec.name.clone(),
        wasm: m.wasm,
        grants,
        denied,
        unused,
        fuel: spec.fuel.or(m.fuel),
        lazy_cmd: spec.lazy_cmd.clone(),
        lazy_event: spec.lazy_event.clone(),
    })
}

/// [`locate`] for every spec of `table`, in spec order: the installed packages and one
/// message per package that cannot load or is not granted everything it requests.
///
/// # Panics
///
/// As [`locate`].
#[must_use]
pub fn installed(table: &Table, paths: &Paths) -> (Vec<Installed>, Vec<String>) {
    let mut ok = Vec::with_capacity(table.packs.len());
    let mut notes = Vec::new();
    for spec in &table.packs {
        match locate(spec, paths) {
            Ok(i) => {
                if !i.denied.is_empty() {
                    notes.push(format!("{}: not granted: {}", i.name, i.denied.join(", ")));
                }
                ok.push(i);
            }
            Err(e) => notes.push(format!("{}: {e}", spec.name)),
        }
    }
    assert!(ok.len() <= table.packs.len());
    assert!(
        notes.len() >= table.packs.len() - ok.len(),
        "every package that cannot load has its note"
    );
    (ok, notes)
}

impl Installed {
    /// ` denied: a,b unused: c` for the status line; empty when every grant matches.
    pub(super) fn grant_notes(&self) -> String {
        let mut notes = String::new();
        if !self.denied.is_empty() {
            write!(notes, " denied: {}", self.denied.join(",")).expect("writing to a String");
        }
        if !self.unused.is_empty() {
            write!(notes, " unused: {}", self.unused.join(",")).expect("writing to a String");
        }
        notes
    }
}

/// The directory a spec's package lives in: its own path, else `<data>/pack/<name>`.
pub(super) fn package_dir(spec: &PackSpec, paths: &Paths) -> PathBuf {
    match &spec.source {
        Source::Path(p) => p.clone(),
        Source::Git { .. } | Source::Url { .. } => {
            let dir = paths.pack.join(&spec.name);
            assert!(
                dir.starts_with(&paths.pack),
                "{}: a package name",
                spec.name
            );
            dir
        }
    }
}

/// The module of the package in `dir`: the manifest's `plugin <name> path <file>.wasm`
/// record with its `grant` and `fuel` attributes, else `<dir>/<name>.wasm` with no requests.
pub(super) fn locate_in(dir: &Path, name: &str) -> Result<Module, String> {
    let manifest = dir.join(MANIFEST);
    let Ok(bytes) = fs::read(&manifest) else {
        return Ok(Module {
            wasm: dir.join(format!("{name}.wasm")),
            requested: None,
            fuel: None,
        });
    };
    let project = Project::new(Some(&bytes), None, 0)
        .map_err(|e| format!("{}: {:?}", manifest.display(), e.diagnostics.first()))?;
    let Some(rec) = project.plugins().first() else {
        return Err(format!("{}: no `plugin` record", manifest.display()));
    };
    let rel = match (rec.fields.get(1).map(String::as_str), rec.fields.get(2)) {
        (Some("path"), Some(rel)) if !rel.contains("..") && !Path::new(rel).is_absolute() => rel,
        _ => {
            return Err(format!(
                "{}: the plugin record must be `plugin <name> path <file>.wasm`",
                manifest.display()
            ));
        }
    };
    let requested: Vec<String> = rec
        .attrs
        .iter()
        .filter(|a| a.key == "grant")
        .filter_map(|a| a.values.first().cloned())
        .collect();
    let fuel: Option<u64> = rec
        .attrs
        .iter()
        .find(|a| a.key == "fuel")
        .and_then(|a| a.values.first()?.parse().ok());
    let wasm = dir.join(rel);
    // The record's path was refused above when absolute or climbing out with `..`.
    assert!(
        wasm.starts_with(dir),
        "{}: outside its package",
        wasm.display()
    );
    Ok(Module {
        wasm,
        requested: Some(requested),
        fuel,
    })
}
