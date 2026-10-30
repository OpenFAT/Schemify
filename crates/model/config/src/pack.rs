//! `schemify.pack.add` (config spec §5): Lua spec tables → validated [`PackSpec`] rows of the
//! user-config table. A bad spec is one located error request and is dropped; the others
//! stay. Cites: config spec §5 (the spec fields, names, local paths); plugin ABI §2 (fuel),
//! §5 (grants), §6 (event words).
//!
//! Contents:
//! - `pub(crate) fn pack_add(lua: &Lua, state: &Shared, (here, specs): (Value, Value))`

use std::env;
use std::path::{Component, Path, PathBuf};

use omnilua::{Lua, Table as LuaTable, Value};

use crate::lua::{self, Shared};
use crate::{Event, PACKS_MAX, PackSpec, Source};

/// The fields a spec table may have.
const SPEC_FIELDS: [&str; 10] = [
    "src", "url", "sha256", "path", "name", "rev", "version", "grant", "fuel", "lazy",
];
/// Longest package name, bytes (spec §5).
const NAME_MAX: usize = 64;
/// Longest grant or lazy command word, bytes.
const WORD_MAX: usize = 128;
/// Smallest per-event fuel override (plugin ABI §2).
const FUEL_MIN: i64 = 1_000_000;
/// Largest per-event fuel override (plugin ABI §2).
const FUEL_MAX: i64 = 100_000_000_000;

/// A spec's `lazy` table: what loads the plugin on first use.
#[derive(Default)]
struct Lazy {
    /// Commands.
    cmd: Vec<String>,
    /// Events.
    event: Vec<Event>,
}

/// `schemify.pack.add { spec, … }`: appends each valid spec, or replaces the row of the same
/// name; a bad spec is an error request located at `here` (the caller's `file:line: `).
/// Returns the error that fails the call: more than [`PACKS_MAX`] packages or too many
/// requests.
pub(crate) fn pack_add(
    _: &Lua,
    state: &Shared,
    (here, specs): (Value, Value),
) -> omnilua::Result<Option<String>> {
    let here = lua::string(&here).unwrap_or_default();
    let Value::Table(specs) = specs else {
        return Ok(Some(lua::bad_arg(1, "add", "table", &specs)));
    };
    let root = state.borrow().root.clone();
    for i in 1..=specs.len()? {
        let index = i64::try_from(i).expect("a Lua sequence index fits an integer");
        let spec = match pack_spec(specs.raw_get(index)?, root.as_deref()) {
            Ok(spec) => spec,
            Err(message) => {
                let request = crate::error(format!("{here}pack spec: {message}"));
                match lua::push(state, request) {
                    Some(e) => return Ok(Some(e)),
                    None => continue,
                }
            }
        };
        let mut state = state.borrow_mut();
        let packs = &mut state.table.packs;
        if let Some(row) = packs.iter_mut().find(|p| p.name == spec.name) {
            *row = spec;
            continue;
        }
        if packs.len() >= PACKS_MAX {
            return Ok(Some(format!("more than {PACKS_MAX} packages")));
        }
        packs.push(spec);
    }
    let state = state.borrow();
    let packs = &state.table.packs;
    assert!(packs.len() <= PACKS_MAX);
    if let Some(last) = packs.last() {
        let earlier = &packs[..packs.len() - 1];
        assert!(
            earlier.iter().all(|p| p.name != last.name),
            "a spec of a known name replaces its row"
        );
    }
    Ok(None)
}

// ---- private ----

/// Validates one spec; a bare string is `{ src = … }`.
fn pack_spec(v: Value, root: Option<&Path>) -> Result<PackSpec, String> {
    let t = match v {
        Value::String(s) => {
            let url = s.to_str().map_err(|e| e.to_string())?;
            let source = Source::Git { url, rev: None };
            return named(source, None, Vec::new(), None, Lazy::default());
        }
        Value::Table(t) => t,
        other => return Err(format!("a spec is a table, not {}", lua::type_name(&other))),
    };
    let pairs = t.pairs().map_err(|e| e.to_string())?;
    for pair in pairs {
        let (k, _): (Value, Value) = pair.map_err(|e| e.to_string())?;
        let known = match &k {
            Value::String(k) => k.to_str().is_ok_and(|k| SPEC_FIELDS.contains(&k.as_str())),
            _ => false,
        };
        if !known {
            let shown = lua::string(&k).unwrap_or_else(|| lua::type_name(&k).to_owned());
            return Err(format!("unknown field {shown}"));
        }
    }
    let s = |k: &str| -> Result<Option<String>, String> {
        match lua::field(&t, k) {
            Value::Nil => Ok(None),
            Value::String(v) => v.to_str().map(Some).map_err(|e| e.to_string()),
            _ => Err(format!("`{k}` must be a string")),
        }
    };
    let (src, url, path) = (s("src")?, s("url")?, s("path")?);
    let (rev, version) = (s("rev")?, s("version")?);
    if rev.is_some() && version.is_some() {
        return Err("give `rev` or `version`, not both".into());
    }
    let rev = rev.or(version);
    let source = match (src, url, path) {
        (Some(url), None, None) => {
            // A local repository path is relative to the config, not to where git runs.
            let local = ["~/", "./", "../"].iter().any(|p| url.starts_with(p));
            let url = if local {
                expand(&url, root).display().to_string()
            } else {
                url
            };
            Source::Git {
                url,
                rev: rev.clone(),
            }
        }
        (None, Some(url), None) => {
            let sha = s("sha256")?.ok_or("a `url` source needs `sha256`")?;
            if sha.len() != 64 || !sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                return Err("`sha256` must be 64 lowercase hex digits".into());
            }
            Source::Url { url, sha256: sha }
        }
        (None, None, Some(p)) => Source::Path(expand(&p, root)),
        _ => return Err("give exactly one of `src`, `url`, `path`".into()),
    };
    if rev.is_some() && !matches!(source, Source::Git { .. }) {
        return Err("`rev`/`version` apply to `src` (git) only".into());
    }
    let grant = match lua::field(&t, "grant") {
        Value::Nil => Vec::new(),
        Value::Table(g) => lua::strings(&g).ok_or("`grant` must be a list of strings")?,
        _ => return Err("`grant` must be a list of strings".into()),
    };
    if let Some(g) = grant.iter().find(|g| !lua::is_word(g, WORD_MAX)) {
        return Err(format!("bad grant `{g}`"));
    }
    let fuel = match lua::field(&t, "fuel") {
        Value::Nil => None,
        Value::Integer(n) if (FUEL_MIN..=FUEL_MAX).contains(&n) => {
            Some(u64::try_from(n).expect("FUEL_MIN > 0"))
        }
        _ => return Err("`fuel` must be a whole number in [10^6, 10^11]".into()),
    };
    let lazy = match lua::field(&t, "lazy") {
        Value::Nil => Lazy::default(),
        Value::Table(l) => {
            let cmd = words(&l, "cmd")?;
            let mut event = Vec::new();
            for w in words(&l, "event")? {
                event.push(Event::from_word(&w).ok_or(format!("unknown event `{w}`"))?);
            }
            if cmd.is_empty() && event.is_empty() {
                return Err("`lazy` needs `cmd` or `event`".into());
            }
            Lazy { cmd, event }
        }
        _ => return Err("`lazy` must be a table".into()),
    };
    named(source, s("name")?, grant, fuel, lazy)
}

/// `~/x` against `HOME`, a relative path against the config root; `.` segments are dropped
/// lexically (the lockfile and `pack status` print the result), `..` is kept because only the
/// filesystem can resolve it past a symlink.
fn expand(p: &str, root: Option<&Path>) -> PathBuf {
    let joined = if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = env::var_os("HOME")
    {
        PathBuf::from(home).join(rest)
    } else {
        let path = PathBuf::from(p);
        match root {
            Some(r) if path.is_relative() => r.join(path),
            _ => path,
        }
    };
    // `components()` skips every `.` except a leading one.
    let clean: PathBuf = joined.components().collect();
    debug_assert!(clean.components().skip(1).all(|c| c != Component::CurDir));
    clean
}

/// `cmd = "x"` or `cmd = { "x", "y" }` of a `lazy` table: words of at most 128 bytes.
fn words(t: &LuaTable, key: &str) -> Result<Vec<String>, String> {
    assert!(matches!(key, "cmd" | "event"), "the `lazy` fields");
    let bad = || format!("`lazy.{key}` must be a word or a list of words");
    let v: Vec<String> = match lua::field(t, key) {
        Value::Nil => Vec::new(),
        Value::String(s) => vec![s.to_str().map_err(|_| bad())?],
        Value::Table(l) => lua::strings(&l).ok_or_else(bad)?,
        _ => return Err(bad()),
    };
    if v.iter().any(|w| !lua::is_word(w, WORD_MAX)) {
        return Err(bad());
    }
    Ok(v)
}

/// The spec with its name: `name`, else the source's last segment without `.git`/`.wasm`.
fn named(
    source: Source,
    name: Option<String>,
    grant: Vec<String>,
    fuel: Option<u64>,
    lazy: Lazy,
) -> Result<PackSpec, String> {
    let name = name.unwrap_or_else(|| {
        let from = match &source {
            Source::Git { url, .. } | Source::Url { url, .. } => url.clone(),
            Source::Path(p) => p.display().to_string(),
        };
        let last = (from.trim_end_matches(['/', '\\']).rsplit(['/', '\\', ':']))
            .next()
            .unwrap_or("");
        let last = (last.strip_suffix(".git"))
            .or_else(|| last.strip_suffix(".wasm"))
            .unwrap_or(last);
        last.to_owned()
    });
    let ok = !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= NAME_MAX
        && (name.bytes()).all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !ok {
        return Err(format!("bad package name `{name}` (give `name`)"));
    }
    let spec = PackSpec {
        name,
        source,
        grant,
        fuel,
        lazy_command: lazy.cmd,
        lazy_event: lazy.event,
    };
    // `pack_spec` checked each field; the row this crate stores holds them.
    if let Some(fuel) = spec.fuel {
        assert!(fuel >= FUEL_MIN.unsigned_abs());
        assert!(fuel <= FUEL_MAX.unsigned_abs());
    }
    assert!(spec.grant.iter().all(|g| lua::is_word(g, WORD_MAX)));
    assert!(spec.lazy_command.iter().all(|c| lua::is_word(c, WORD_MAX)));
    Ok(spec)
}
