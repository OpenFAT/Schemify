//! The sandboxed Lua 5.4 state (spec §4) and the `schemify.*` API (spec §3).
//!
//! During a call the [`Table`] being written and the request list live in the state's app
//! data ([`State`]); [`Runtime::with_state`] moves them in before and out after, so the
//! table has one writer and the callers never see Lua values. Lua functions the config keeps
//! (keymap targets, hooks) live in a registry table indexed by [`FnId`].

use std::path::{Path, PathBuf};

use mlua::chunk::ChunkMode;
use mlua::debug::Debug;
use mlua::{
    Error as LuaError, Function, HookTriggers, Lua, LuaOptions, MultiValue, StdLib,
    Table as LTable, Value, VmState,
};

use crate::{
    BUDGET, Event, FnId, HOOKS_MAX, KEYMAPS_MAX, Level, MEMORY_MAX, PACKS_MAX, PackSpec,
    REQUESTS_MAX, Request, SimKind, Source, Table, Target, error,
};

/// Registry table of kept functions (`FnId` n is at index n + 1).
const FNS: &str = "schemify.fns";
/// Registry table of loaded modules (`require`).
const LOADED: &str = "schemify.loaded";
/// Most functions one config may keep (every keymap and hook, plus rebinding headroom).
const FNS_MAX: usize = 8192;
/// VM instructions between budget checks.
const HOOK_EVERY: u32 = 1000;

/// The Lua state of one config.
pub(crate) struct Runtime {
    lua: Lua,
}

/// The table and outputs of the call in progress (Lua app data).
struct State {
    table: Table,
    out: Vec<Request>,
}

/// Instructions left in the call in progress (Lua app data, read by the hook).
struct Budget {
    left: i64,
    exhausted: bool,
}

/// The config root `require` resolves against (Lua app data).
struct Root(Option<PathBuf>);

impl Runtime {
    /// Builds the sandbox and runs the init file `src` (chunk name `name`, root `root`).
    /// `None` when the state itself cannot be built (reported in the requests).
    pub(crate) fn start(
        src: &str,
        name: &str,
        root: Option<&Path>,
    ) -> (Option<Runtime>, Table, Vec<Request>) {
        let lua = match sandbox(root) {
            Ok(l) => l,
            Err(e) => {
                let msg = format!("{name}: cannot start Lua: {}", describe(&e));
                return (None, Table::default(), vec![error(msg)]);
            }
        };
        let rt = Runtime { lua };
        let chunk = format!("@{name}");
        let (table, out) = rt.with_state(Table::default(), name, |lua| {
            lua.load(src)
                .set_name(chunk)
                .set_mode(ChunkMode::Text)
                .exec()
        });
        (Some(rt), table, out)
    }

    /// Calls kept function `f` with an event table (`ev`) or no argument.
    pub(crate) fn call(
        &mut self,
        table: Table,
        f: FnId,
        ev: Option<Event>,
        fields: &[(&str, i64)],
    ) -> (Table, Vec<Request>) {
        let what = format!("function {}", f.0);
        self.with_state(table, &what, |lua| {
            let fns: LTable = lua.named_registry_value(FNS)?;
            let func: Function = fns.raw_get(usize::from(f.0) + 1)?;
            match ev {
                None => func.call::<()>(()),
                Some(e) => {
                    let t = lua.create_table()?;
                    t.raw_set("event", e.word())?;
                    for &(k, v) in fields {
                        t.raw_set(k, v)?;
                    }
                    func.call::<()>(t)
                }
            }
        })
    }

    /// Runs `run` with `table` and a fresh budget in the app data; returns the table and the
    /// requests, plus one error request when `run` fails (`what` names it when the error
    /// carries no location).
    fn with_state(
        &self,
        table: Table,
        what: &str,
        run: impl FnOnce(&Lua) -> mlua::Result<()>,
    ) -> (Table, Vec<Request>) {
        let lua = &self.lua;
        lua.set_app_data(State {
            table,
            out: Vec::new(),
        });
        lua.set_app_data(Budget {
            left: BUDGET as i64,
            exhausted: false,
        });
        let result = run(lua);
        let mut st = lua.remove_app_data::<State>().expect("set above");
        lua.remove_app_data::<Budget>();
        if let Err(e) = result {
            let msg = describe(&e);
            let located = msg.contains(".lua:") || msg.starts_with(what);
            let text = match (located, traceback_location(&e)) {
                (true, _) => msg,
                (false, Some(at)) => format!("{at}: {msg}"),
                (false, None) => format!("{what}: {msg}"),
            };
            st.out.push(error(text));
        }
        assert!(st.out.len() <= REQUESTS_MAX + 1);
        assert!(st.table.keymaps.chord.len() == st.table.keymaps.target.len());
        (st.table, st.out)
    }
}

/// A Lua state with only the safe libraries, the budget hook, the memory cap and the
/// `schemify` API.
fn sandbox(root: Option<&Path>) -> mlua::Result<Lua> {
    let libs = StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::UTF8;
    let lua = Lua::new_with(libs, LuaOptions::new())?;
    lua.set_memory_limit(MEMORY_MAX)?;
    lua.set_app_data(Root(root.map(Path::to_path_buf)));
    lua.set_named_registry_value(FNS, lua.create_table()?)?;
    lua.set_named_registry_value(LOADED, lua.create_table()?)?;
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(HOOK_EVERY),
        |lua, debug| {
            let Some(mut b) = lua.app_data_mut::<Budget>() else {
                return Ok(VmState::Continue);
            };
            b.left -= i64::from(HOOK_EVERY);
            if b.left >= 0 {
                return Ok(VmState::Continue);
            }
            b.exhausted = true;
            drop(b);
            Err(LuaError::runtime(format!(
                "{}instruction budget exhausted ({BUDGET} per call)",
                location(debug)
            )))
        },
    )?;
    let g = lua.globals();
    // Everything that reads files, loads code or bypasses the budget.
    for name in ["dofile", "loadfile", "load", "collectgarbage"] {
        g.raw_set(name, Value::Nil)?;
    }
    g.raw_set("print", lua.create_function(print)?)?;
    g.raw_set("pcall", lua.create_function(pcall)?)?;
    g.raw_set("xpcall", lua.create_function(xpcall)?)?;
    g.raw_set("require", lua.create_function(require)?)?;
    g.raw_set("schemify", api(&lua)?)?;
    Ok(lua)
}

/// The `schemify` table.
fn api(lua: &Lua) -> mlua::Result<LTable> {
    let s = lua.create_table()?;
    let opt = lua.create_table()?;
    let mt = lua.create_table()?;
    mt.raw_set(
        "__index",
        lua.create_function(|lua, (_, k): (Value, String)| get_opt(lua, &k))?,
    )?;
    mt.raw_set(
        "__newindex",
        lua.create_function(|lua, (_, k, v): (Value, String, Value)| set_opt(lua, &k, v))?,
    )?;
    opt.set_metatable(Some(mt))?;
    s.raw_set("opt", opt)?;
    let keymap = lua.create_table()?;
    keymap.raw_set("set", lua.create_function(keymap_set)?)?;
    s.raw_set("keymap", keymap)?;
    s.raw_set("cmd", lua.create_function(cmd)?)?;
    s.raw_set("on", lua.create_function(on)?)?;
    s.raw_set("sim", lua.create_function(sim)?)?;
    s.raw_set("notify", lua.create_function(notify)?)?;
    let pack = lua.create_table()?;
    pack.raw_set("add", lua.create_function(pack_add)?)?;
    s.raw_set("pack", pack)?;
    Ok(s)
}

// ------------------------------------------------------------------ helpers

/// `file:line: ` of the Lua code that called the running callback (stack level 1).
fn here(lua: &Lua) -> String {
    lua.inspect_stack(1, location).unwrap_or_default()
}

fn location(d: &Debug) -> String {
    let src = d.source();
    match (src.short_src.as_deref(), d.current_line()) {
        (Some(s), Some(l)) => format!("{s}:{l}: "),
        (Some(s), None) => format!("{s}: "),
        _ => String::new(),
    }
}

/// An error at the calling Lua line.
fn fail<T>(lua: &Lua, msg: impl std::fmt::Display) -> mlua::Result<T> {
    Err(LuaError::runtime(format!("{}{msg}", here(lua))))
}

/// The first line of the innermost message of `e`.
fn describe(e: &LuaError) -> String {
    let text = match e {
        LuaError::CallbackError { cause, .. } => return describe(cause),
        LuaError::WithContext { cause, .. } => return describe(cause),
        LuaError::RuntimeError(s) => s.clone(),
        LuaError::SyntaxError { message, .. } => message.clone(),
        LuaError::MemoryError(_) => format!("not enough memory ({} MiB cap)", MEMORY_MAX >> 20),
        other => other.to_string(),
    };
    text.lines().next().unwrap_or("").to_owned()
}

/// `file:line` of the innermost Lua frame in a callback error's traceback (argument
/// conversion errors carry no location of their own).
fn traceback_location(e: &LuaError) -> Option<String> {
    let LuaError::CallbackError { traceback, cause } = e else {
        return None;
    };
    let frame = traceback
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("[C]") && !l.starts_with("stack traceback"))
        .find_map(|l| l.split_once(": in ").map(|(at, _)| at.to_owned()));
    frame.or_else(|| traceback_location(cause))
}

fn with<R>(lua: &Lua, f: impl FnOnce(&mut State) -> R) -> R {
    let mut st = lua
        .app_data_mut::<State>()
        .expect("API called inside a call");
    f(&mut st)
}

/// Queues a request; past [`REQUESTS_MAX`] the call fails.
fn push(lua: &Lua, r: Request) -> mlua::Result<()> {
    let full = with(lua, |st| {
        if st.out.len() >= REQUESTS_MAX {
            return true;
        }
        st.out.push(r);
        false
    });
    if full {
        return fail(
            lua,
            format!("more than {REQUESTS_MAX} requests in one call"),
        );
    }
    Ok(())
}

/// One word: non-empty, no whitespace, at most `max` bytes.
fn is_word(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_whitespace)
}

fn keep(lua: &Lua, f: Function) -> mlua::Result<FnId> {
    let fns: LTable = lua.named_registry_value(FNS)?;
    let n = fns.raw_len();
    if n >= FNS_MAX {
        return fail(lua, format!("more than {FNS_MAX} functions kept"));
    }
    fns.raw_set(n + 1, f)?;
    Ok(FnId(u16::try_from(n).expect("FNS_MAX < 2^16")))
}

fn exhausted(lua: &Lua) -> bool {
    lua.app_data_ref::<Budget>().is_some_and(|b| b.exhausted)
}

// ------------------------------------------------------------------ globals

/// `print(...)`: an info notification (no stdout from a config).
fn print(lua: &Lua, args: MultiValue) -> mlua::Result<()> {
    let parts: Vec<String> = args
        .iter()
        .map(|v| v.to_string())
        .collect::<mlua::Result<_>>()?;
    push(
        lua,
        Request::Notify {
            level: Level::Info,
            text: parts.join("\t"),
        },
    )
}

/// `pcall` that cannot swallow budget exhaustion (a `while true do pcall(f) end` would
/// otherwise outlive any budget). The error value is its message string.
fn pcall(lua: &Lua, (f, args): (Function, MultiValue)) -> mlua::Result<MultiValue> {
    match f.call::<MultiValue>(args) {
        Ok(v) => {
            let mut all = vec![Value::Boolean(true)];
            all.extend(v.into_vec());
            Ok(MultiValue::from_vec(all))
        }
        Err(e) if exhausted(lua) => Err(e),
        Err(e) => {
            let msg = Value::String(lua.create_string(describe(&e))?);
            Ok(MultiValue::from_vec(vec![Value::Boolean(false), msg]))
        }
    }
}

/// `xpcall` with the same rule as [`pcall`]; the handler gets the message string.
fn xpcall(
    lua: &Lua,
    (f, handler, args): (Function, Function, MultiValue),
) -> mlua::Result<MultiValue> {
    match f.call::<MultiValue>(args) {
        Ok(v) => {
            let mut all = vec![Value::Boolean(true)];
            all.extend(v.into_vec());
            Ok(MultiValue::from_vec(all))
        }
        Err(e) if exhausted(lua) => Err(e),
        Err(e) => {
            let handled: Value = handler.call(describe(&e))?;
            Ok(MultiValue::from_vec(vec![Value::Boolean(false), handled]))
        }
    }
}

/// `require "a.b"`: `<root>/lua/a/b.lua` or `<root>/lua/a/b/init.lua`, once.
fn require(lua: &Lua, name: String) -> mlua::Result<Value> {
    let ok = !name.is_empty()
        && name.split('.').all(|seg| {
            !seg.is_empty()
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        });
    if !ok {
        return fail(lua, format!("bad module name `{name}`"));
    }
    let loaded: LTable = lua.named_registry_value(LOADED)?;
    match loaded.raw_get::<Value>(name.as_str())? {
        Value::Nil => {}
        Value::LightUserData(_) => return fail(lua, format!("loop requiring `{name}`")),
        v => return Ok(v),
    }
    let root = lua.app_data_ref::<Root>().and_then(|r| r.0.clone());
    let Some(root) = root else {
        return fail(lua, format!("module `{name}`: no config directory"));
    };
    let rel = name.replace('.', "/");
    let candidates = [format!("lua/{rel}.lua"), format!("lua/{rel}/init.lua")];
    let Some((file, src)) = candidates
        .iter()
        .find_map(|c| std::fs::read_to_string(root.join(c)).ok().map(|s| (c, s)))
    else {
        return fail(
            lua,
            format!("module `{name}` not found under {}/lua", root.display()),
        );
    };
    // A sentinel while the module runs, so a cycle fails instead of recursing.
    loaded.raw_set(
        name.as_str(),
        Value::LightUserData(mlua::LightUserData(std::ptr::null_mut())),
    )?;
    let chunk = lua
        .load(&src)
        .set_name(format!("@{file}"))
        .set_mode(ChunkMode::Text);
    let result = chunk.call::<Value>(name.as_str());
    let v = match result {
        Ok(Value::Nil) => Value::Boolean(true),
        Ok(v) => v,
        Err(e) => {
            loaded.raw_set(name.as_str(), Value::Nil)?;
            return Err(e);
        }
    };
    loaded.raw_set(name.as_str(), v.clone())?;
    Ok(v)
}

// ------------------------------------------------------------------ schemify.*

fn get_opt(lua: &Lua, key: &str) -> mlua::Result<Value> {
    let o = with(lua, |st| st.table.opts);
    let step = |milli: i32| Value::Number(f64::from(milli) / 1000.0);
    Ok(match key {
        "snap" => step(o.snap),
        "grid" => step(o.grid),
        "show_grid" => Value::Boolean(o.show_grid),
        "light" => Value::Boolean(o.light),
        _ => return fail(lua, format!("unknown option `{key}`")),
    })
}

fn set_opt(lua: &Lua, key: &str, v: Value) -> mlua::Result<()> {
    let step = |v: &Value| -> Option<i32> {
        let units = match *v {
            Value::Integer(i) => i as f64,
            Value::Number(n) => n,
            _ => return None,
        };
        let milli = (units * 1000.0).round();
        (1.0..=1e9).contains(&milli).then_some(milli as i32)
    };
    let flag = |v: &Value| match *v {
        Value::Boolean(b) => Some(b),
        _ => None,
    };
    let set = match key {
        "snap" => step(&v).map(|s| with(lua, |st| st.table.opts.snap = s)),
        "grid" => step(&v).map(|s| with(lua, |st| st.table.opts.grid = s)),
        "show_grid" => flag(&v).map(|b| with(lua, |st| st.table.opts.show_grid = b)),
        "light" => flag(&v).map(|b| with(lua, |st| st.table.opts.light = b)),
        _ => return fail(lua, format!("unknown option `{key}`")),
    };
    match set {
        Some(()) => Ok(()),
        None => fail(
            lua,
            format!("option `{key}`: bad value ({})", v.type_name()),
        ),
    }
}

fn keymap_set(
    lua: &Lua,
    (chord, target, opts): (String, Value, Option<LTable>),
) -> mlua::Result<()> {
    if !is_word(&chord, 64) {
        return fail(lua, format!("bad chord `{chord}`"));
    }
    let target = match target {
        Value::String(s) => {
            let s = s.to_str()?.to_owned();
            if !is_word(&s, 128) {
                return fail(lua, format!("bad command name `{s}`"));
            }
            Target::Command(s)
        }
        Value::Function(f) => Target::Function(keep(lua, f)?),
        other => {
            return fail(
                lua,
                format!(
                    "keymap target must be a command name or a function, not {}",
                    other.type_name()
                ),
            );
        }
    };
    let desc: String = match opts {
        Some(t) => t.raw_get::<Option<String>>("desc")?.unwrap_or_default(),
        None => String::new(),
    };
    let full = with(lua, |st| {
        let k = &mut st.table.keymaps;
        if let Some(i) = k.chord.iter().position(|c| *c == chord) {
            k.target[i] = target;
            k.desc[i] = desc;
            return false;
        }
        if k.chord.len() >= KEYMAPS_MAX {
            return true;
        }
        k.chord.push(chord);
        k.target.push(target);
        k.desc.push(desc);
        false
    });
    if full {
        return fail(lua, format!("more than {KEYMAPS_MAX} keymaps"));
    }
    Ok(())
}

fn cmd(lua: &Lua, (name, args): (String, Option<String>)) -> mlua::Result<()> {
    if !is_word(&name, 128) {
        return fail(lua, format!("bad command name `{name}`"));
    }
    push(
        lua,
        Request::Command {
            name,
            args: args.unwrap_or_default(),
        },
    )
}

fn on(lua: &Lua, (event, f): (String, Function)) -> mlua::Result<()> {
    let Some(ev) = Event::from_word(&event) else {
        return fail(lua, format!("unknown event `{event}`"));
    };
    if with(lua, |st| st.table.hooks.len()) >= HOOKS_MAX {
        return fail(lua, format!("more than {HOOKS_MAX} hooks"));
    }
    let id = keep(lua, f)?;
    with(lua, |st| st.table.hooks.push((ev, id)));
    Ok(())
}

fn sim(lua: &Lua, (name, spec): (String, LTable)) -> mlua::Result<()> {
    let Some(kind) = SimKind::ALL.into_iter().find(|k| k.word() == name) else {
        return fail(
            lua,
            format!("unknown program `{name}` (ngspice, openvaf, iverilog)"),
        );
    };
    let path: Option<String> = spec.raw_get("path")?;
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        return fail(lua, format!("sim `{name}`: `path` is required"));
    };
    let args: Vec<String> = match spec.raw_get::<Option<LTable>>("args")? {
        Some(t) => t.sequence_values::<String>().collect::<mlua::Result<_>>()?,
        None => Vec::new(),
    };
    with(lua, |st| {
        st.table.sims[kind as usize] = Some(crate::Sim { path, args });
    });
    Ok(())
}

fn notify(lua: &Lua, (msg, level): (String, Option<String>)) -> mlua::Result<()> {
    let level = match level.as_deref() {
        None | Some("info") => Level::Info,
        Some("warn") => Level::Warn,
        Some("error") => Level::Error,
        Some(other) => return fail(lua, format!("unknown level `{other}`")),
    };
    push(lua, Request::Notify { level, text: msg })
}

/// `schemify.pack.add { spec, … }`: a bad spec is reported and dropped, the rest stay.
fn pack_add(lua: &Lua, specs: LTable) -> mlua::Result<()> {
    let root = lua.app_data_ref::<Root>().and_then(|r| r.0.clone());
    for v in specs.sequence_values::<Value>() {
        let spec = match pack_spec(v?, root.as_deref()) {
            Ok(s) => s,
            Err(msg) => {
                push(lua, error(format!("{}pack spec: {msg}", here(lua))))?;
                continue;
            }
        };
        let full = with(lua, |st| {
            let packs = &mut st.table.packs;
            if let Some(i) = packs.iter().position(|p| p.name == spec.name) {
                packs[i] = spec;
                return false;
            }
            if packs.len() >= PACKS_MAX {
                return true;
            }
            packs.push(spec);
            false
        });
        if full {
            return fail(lua, format!("more than {PACKS_MAX} packages"));
        }
    }
    Ok(())
}

const SPEC_FIELDS: [&str; 10] = [
    "src", "url", "sha256", "path", "name", "rev", "version", "grant", "fuel", "lazy",
];

/// Validates one spec (spec §5). A bare string is `{ src = … }`.
fn pack_spec(v: Value, root: Option<&Path>) -> Result<PackSpec, String> {
    let t = match v {
        Value::String(s) => {
            let url = s.to_str().map_err(|e| e.to_string())?.to_owned();
            return finish_spec(
                Source::Git { url, rev: None },
                None,
                Vec::new(),
                None,
                (Vec::new(), Vec::new()),
            );
        }
        Value::Table(t) => t,
        other => return Err(format!("a spec is a table, not {}", other.type_name())),
    };
    let s = |k: &str| -> Result<Option<String>, String> {
        t.raw_get::<Option<String>>(k)
            .map_err(|_| format!("`{k}` must be a string"))
    };
    for pair in t.pairs::<Value, Value>() {
        let (k, _) = pair.map_err(|e| e.to_string())?;
        let known = match &k {
            Value::String(k) => k.to_str().is_ok_and(|k| SPEC_FIELDS.contains(&&*k)),
            _ => false,
        };
        if !known {
            return Err(format!(
                "unknown field {}",
                k.to_string().unwrap_or_default()
            ));
        }
    }
    let (src, url, path) = (s("src")?, s("url")?, s("path")?);
    let (rev, version) = (s("rev")?, s("version")?);
    if rev.is_some() && version.is_some() {
        return Err("give `rev` or `version`, not both".into());
    }
    let rev = rev.or(version);
    let source = match (src, url, path) {
        (Some(url), None, None) => {
            // A local repository path is relative to the config, not to where git runs.
            let local = url.starts_with("~/") || url.starts_with("./") || url.starts_with("../");
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
    let grant: Vec<String> = match t.raw_get::<Option<LTable>>("grant") {
        Ok(Some(g)) => g
            .sequence_values::<String>()
            .collect::<mlua::Result<_>>()
            .map_err(|_| "`grant` must be a list of strings".to_owned())?,
        Ok(None) => Vec::new(),
        Err(_) => return Err("`grant` must be a list of strings".into()),
    };
    if let Some(g) = grant.iter().find(|g| !is_word(g, 128)) {
        return Err(format!("bad grant `{g}`"));
    }
    let fuel = match t.raw_get::<Value>("fuel").map_err(|e| e.to_string())? {
        Value::Nil => None,
        Value::Integer(n) if (1_000_000..=100_000_000_000).contains(&n) => Some(n as u64),
        _ => return Err("`fuel` must be a whole number in [10^6, 10^11]".into()),
    };
    let lazy = match t.raw_get::<Value>("lazy").map_err(|e| e.to_string())? {
        Value::Nil => (Vec::new(), Vec::new()),
        Value::Table(l) => {
            let cmds = words(&l, "cmd")?;
            let mut events = Vec::new();
            for w in words(&l, "event")? {
                events.push(Event::from_word(&w).ok_or(format!("unknown event `{w}`"))?);
            }
            if cmds.is_empty() && events.is_empty() {
                return Err("`lazy` needs `cmd` or `event`".into());
            }
            (cmds, events)
        }
        _ => return Err("`lazy` must be a table".into()),
    };
    finish_spec(source, s("name")?, grant, fuel, lazy)
}

/// `cmd = "x"` or `cmd = { "x", "y" }`.
fn words(t: &LTable, key: &str) -> Result<Vec<String>, String> {
    let bad = || format!("`lazy.{key}` must be a word or a list of words");
    let v: Vec<String> = match t.raw_get::<Value>(key).map_err(|e| e.to_string())? {
        Value::Nil => Vec::new(),
        Value::String(s) => vec![s.to_str().map_err(|_| bad())?.to_owned()],
        Value::Table(l) => l
            .sequence_values::<String>()
            .collect::<mlua::Result<_>>()
            .map_err(|_| bad())?,
        _ => return Err(bad()),
    };
    if v.iter().any(|w| !is_word(w, 128)) {
        return Err(bad());
    }
    Ok(v)
}

fn finish_spec(
    source: Source,
    name: Option<String>,
    grant: Vec<String>,
    fuel: Option<u64>,
    (lazy_cmd, lazy_event): (Vec<String>, Vec<Event>),
) -> Result<PackSpec, String> {
    let name = match name {
        Some(n) => n,
        None => {
            let from = match &source {
                Source::Git { url, .. } | Source::Url { url, .. } => url.clone(),
                Source::Path(p) => p.display().to_string(),
            };
            let last = from
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\', ':'])
                .next()
                .unwrap_or("");
            let last = last
                .strip_suffix(".git")
                .or_else(|| last.strip_suffix(".wasm"))
                .unwrap_or(last);
            last.to_owned()
        }
    };
    let ok = !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !ok {
        return Err(format!("bad package name `{name}` (give `name`)"));
    }
    Ok(PackSpec {
        name,
        source,
        grant,
        fuel,
        lazy_cmd,
        lazy_event,
    })
}

/// `~/x` against `HOME`, a relative path against the config root.
fn expand(p: &str, root: Option<&Path>) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    let path = PathBuf::from(p);
    match root {
        Some(r) if path.is_relative() => r.join(path),
        _ => path,
    }
}
