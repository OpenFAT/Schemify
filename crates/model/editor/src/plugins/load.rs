//! Loading the plugin set: the bundled default UI (D6), user packages (D35) and the
//! project's local plugins in configured order (D8), each with its content hash and fuel.
//!
//! Contents:
//! - `Host::new`
//! - `pub(super) fn load_installed`
//! - `pub(super) fn content_hash`

use std::collections::VecDeque;
use std::path::Path;

use schemify_cli::pack::{self, Installed};
use schemify_config::{Config, Request, Startup};
use schemify_doc::{Attr, Project};
use schemify_plugin::abi::FUEL_DEFAULT;
use schemify_plugin::{PluginEvents, PluginId, Plugins, RequestQueues, Snapshots};

use crate::platform;

use super::snapshot::Filled;
use super::{DEFAULT_UI_WASM, HASH_HEX, Host};

/// Bounds on a manifest `fuel` override (plugin ABI §2). A plugin above the default runs its
/// events on a worker thread; the ceiling still bounds how long one event may run there.
// ponytail: on the web (no threads) such events still run on the frame thread and block it;
// slice them with wasmi's resumable calls (`call_resumable`, out-of-fuel) if the web needs it.
const FUEL_MIN: u64 = 1_000_000;

const FUEL_MAX: u64 = 100_000_000_000;

impl Host {
    /// Runs the user config `startup` names, then loads, in composition order (config spec
    /// §6): the bundled default UI (unless `no_default_ui`), the user's eager packages, and
    /// every manifest plugin with a `path` source (a `.wasm` file, or a directory holding
    /// `<name>.wasm`), granting what the manifest declares. Load failures and config errors
    /// are returned as messages; plugin failures are also in the health report.
    ///
    /// # Panics
    ///
    /// Only on a broken internal invariant (a loaded plugin without its content hash).
    pub fn new(
        project: &Project,
        root: &Path,
        no_default_ui: bool,
        startup: &Startup,
    ) -> (Host, Vec<String>) {
        let mut plugins = Plugins::new();
        let mut notes = Vec::new();
        let mut hashes = Vec::new();
        // Through the platform: on the web `init.lua` lives in OPFS `/config` (web spec §5).
        let (user, startup_out) = Config::load_with(startup, |p| {
            platform::read(p)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
        });
        if !no_default_ui {
            match plugins.load("default-ui", DEFAULT_UI_WASM, &[], FUEL_DEFAULT) {
                Ok(id) => hashes.push((id, content_hash(DEFAULT_UI_WASM))),
                Err(c) => notes.push(format!("default-ui: {c:?}")),
            }
        }
        let mut lazy = Vec::new();
        if !user.table().packs.is_empty() {
            match pack::Paths::of(&user) {
                Ok(paths) => {
                    let (found, pack_notes) = pack::installed(user.table(), &paths);
                    notes.extend(pack_notes);
                    for p in found {
                        if !p.is_lazy() {
                            load_installed(&mut plugins, &mut hashes, &mut notes, &p);
                        } else {
                            lazy.push(p);
                        }
                    }
                }
                Err(e) => notes.push(format!("user packages: {e}")),
            }
        }
        for rec in project.plugins() {
            let name = rec.fields.first().map_or("", String::as_str);
            let (Some("path"), Some(path)) =
                (rec.fields.get(1).map(String::as_str), rec.fields.get(2))
            else {
                notes.push(format!("plugin {name}: only path sources load in M2"));
                continue;
            };
            let mut file = root.join(path);
            if platform::is_dir(&file) {
                file = file.join(format!("{name}.wasm"));
            }
            let grants: Vec<&str> = rec
                .attrs
                .iter()
                .filter(|a| a.key == "grant")
                .filter_map(|a| a.values.first().map(String::as_str))
                .collect();
            let fuel = match manifest_fuel(rec.attrs.iter().find(|a| a.key == "fuel")) {
                Ok(f) => f,
                Err(e) => {
                    notes.push(format!("plugin {name}: {e}; using the default fuel"));
                    FUEL_DEFAULT
                }
            };
            match platform::read(&file) {
                Ok(wasm) => match plugins.load(name, &wasm, &grants, fuel) {
                    Ok(id) => {
                        let _ = plugins.set_worker(id, fuel > FUEL_DEFAULT);
                        hashes.push((id, content_hash(&wasm)));
                    }
                    Err(c) => notes.push(format!("plugin {name}: {c:?}")),
                },
                Err(e) => notes.push(format!("plugin {name}: {e}")),
            }
        }
        let faults_seen = plugins.health().recorded();
        assert_eq!(
            hashes.len(),
            plugins.ids().len(),
            "one content hash per loaded plugin"
        );
        let mut host = Host {
            plugins,
            events: PluginEvents::new(),
            snapshots: Snapshots::new(),
            snapshot_key: None,
            filled: Filled::default(),
            queues: RequestQueues::default(),
            seen_epoch: None,
            hashes,
            args: String::new(),
            result: String::new(),
            commanded: false,
            children: Vec::new(),
            timers: Vec::new(),
            user,
            lazy,
            user_cmds: Vec::new(),
            user_logs: Vec::new(),
            user_ready: false,
            notes: VecDeque::new(),
            notes_seq: 0,
            // Load faults are in the startup notes already.
            faults_seen,
        };
        // Startup errors go out with the load notes (all of them reach the status line).
        for r in startup_out {
            match r {
                Request::Command { name, args } => host.user_cmds.push((name, args)),
                Request::Notify { text, .. } => notes.push(text),
            }
        }
        assert_eq!(host.hashes.len(), host.plugins.ids().len());
        (host, notes)
    }
}

/// Loads one installed user package with its effective grants and fuel; returns its id, or
/// `None` with a note.
pub(super) fn load_installed(
    plugins: &mut Plugins,
    hashes: &mut Vec<(PluginId, String)>,
    notes: &mut Vec<String>,
    p: &Installed,
) -> Option<PluginId> {
    assert!(!p.name.is_empty(), "an installed package is named");
    let hashed = hashes.len();
    let wasm = match platform::read(&p.wasm) {
        Ok(w) => w,
        Err(e) => {
            notes.push(format!("plugin {}: {e}", p.name));
            return None;
        }
    };
    let grants: Vec<&str> = p.grants.iter().map(String::as_str).collect();
    let fuel = p.fuel.unwrap_or(FUEL_DEFAULT).clamp(FUEL_MIN, FUEL_MAX);
    match plugins.load(&p.name, &wasm, &grants, fuel) {
        Ok(id) => {
            let _ = plugins.set_worker(id, fuel > FUEL_DEFAULT);
            hashes.push((id, content_hash(&wasm)));
            assert_eq!(hashes.len(), hashed + 1);
            Some(id)
        }
        Err(c) => {
            notes.push(format!("plugin {}: {c:?}", p.name));
            None
        }
    }
}

/// `HASH_HEX` lowercase hex digits of the sha256 of `bytes`.
pub(super) fn content_hash(bytes: &[u8]) -> String {
    let mut h = schemify_doc::hex(&schemify_doc::sha256(bytes));
    h.truncate(HASH_HEX);
    assert_eq!(h.len(), HASH_HEX, "a sha256 has 64 hex digits");
    assert!(
        !h.contains(|c: char| c.is_ascii_uppercase()),
        "lowercase hex"
    );
    h
}

// ---- private ----

/// The per-plugin fuel budget from a manifest `fuel <n>` attribute, or the default when
/// absent.
///
/// # Errors
///
/// The value is not a whole number in `[FUEL_MIN, FUEL_MAX]`; the caller reports it and
/// falls back to the default rather than trusting a project file's number.
fn manifest_fuel(attr: Option<&Attr>) -> Result<u64, String> {
    let Some(attr) = attr else {
        return Ok(FUEL_DEFAULT);
    };
    let text = attr.values.first().map_or("", String::as_str);
    let fuel: u64 = text
        .parse()
        .map_err(|_| format!("fuel `{text}` is not a whole number"))?;
    if !(FUEL_MIN..=FUEL_MAX).contains(&fuel) {
        return Err(format!("fuel {fuel} is outside [{FUEL_MIN}, {FUEL_MAX}]"));
    }
    assert!(fuel > 0);
    Ok(fuel)
}

#[cfg(test)]
mod fuel_tests {
    use super::*;
    use crate::plugins::fn_result;
    use schemify_elab::Val;

    fn attr(v: &str) -> schemify_doc::Attr {
        schemify_doc::Attr {
            key: "fuel".into(),
            values: vec![v.into()],
        }
    }

    /// Plugin results: numbers, bools and quoted strings (escapes undone); a bare word, two
    /// values or an unterminated quote are not results.
    #[test]
    fn fn_results_are_numbers_bools_or_quoted_strings() {
        assert_eq!(fn_result("2.5e-6"), Some(Val::Num(2.5e-6)));
        assert_eq!(fn_result("true"), Some(Val::Bool(true)));
        assert_eq!(
            fn_result("\"nfet_01v8\""),
            Some(Val::Str("nfet_01v8".into()))
        );
        assert_eq!(fn_result(r#""a \"b\"""#), Some(Val::Str("a \"b\"".into())));
        assert_eq!(
            fn_result("\"12\""),
            Some(Val::Str("12".into())),
            "quoted: a string"
        );
        for bad in ["nfet", "\"a\" \"b\"", "\"open", "1 2", ""] {
            assert_eq!(fn_result(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn manifest_fuel_defaults_parses_and_rejects() {
        assert_eq!(manifest_fuel(None), Ok(FUEL_DEFAULT));
        assert_eq!(
            manifest_fuel(Some(&attr("50000000000"))),
            Ok(50_000_000_000)
        );
        assert_eq!(manifest_fuel(Some(&attr("1000000"))), Ok(FUEL_MIN));
        assert!(manifest_fuel(Some(&attr("999999"))).is_err());
        assert!(manifest_fuel(Some(&attr("100000000001"))).is_err());
        assert!(manifest_fuel(Some(&attr("1e9"))).is_err());
        assert!(manifest_fuel(Some(&attr("-5"))).is_err());
        assert!(manifest_fuel(Some(&attr(""))).is_err());
    }
}
