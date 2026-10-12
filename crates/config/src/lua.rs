//! The sandboxed Lua 5.4 state (spec §4) and the `schemify.*` API (spec §3), on omnilua: a
//! pure-Rust Lua 5.4, so native and web builds run the same interpreter (web spec §5).
//!
//! The API surface is a short Lua prelude (`prelude.lua`) over Rust functions (`raw`): each
//! Rust function validates its arguments and returns an error string instead of raising, and
//! the prelude raises it at the caller's line (`error(msg, 2)`), the way C Lua's `luaL_error`
//! locates errors. During a call the [`Table`] being written and the request list live in
//! [`St`], shared by the Rust functions; [`Runtime::with_state`] moves the table in before and
//! out after, so it has one writer and callers never see Lua values. Lua functions the config
//! keeps (keymap targets, hooks) live in [`St::fns`] indexed by [`FnId`].

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use omnilua::{
    Error as LuaError, Function, HostHooks, Lua, LuaError as RawError, LuaVersion, Sandbox,
    SandboxConfig, Table as LTable, TripReason, Value, Variadic,
};

use crate::{
    BUDGET, Event, FnId, HOOKS_MAX, KEYMAPS_MAX, Level, MEMORY_MAX, PACKS_MAX, PackSpec,
    REQUESTS_MAX, ReadFile, Request, SIM_ENV_MAX, SimKind, Source, Table, Target, USER_LAYERS,
    error,
};

/// Most functions one config may keep (every keymap and hook, plus rebinding headroom).
const FNS_MAX: usize = 8192;
/// VM instructions between budget and memory checks.
const CHECK_EVERY: u32 = 1000;
/// The API prelude (its chunk name is `schemify`, never a `.lua` file).
const PRELUDE: &str = include_str!("prelude.lua");
/// Globals the config never sees (spec §4): host access, code loading, the collector.
/// `require`, `print`, `pcall` and `xpcall` are replaced by the prelude's.
const REMOVED: [&str; 9] = [
    "io",
    "os",
    "debug",
    "package",
    "coroutine",
    "dofile",
    "loadfile",
    "load",
    "collectgarbage",
];

/// The Lua state of one config.
pub(crate) struct Runtime {
    lua: Lua,
    sandbox: Sandbox,
    st: Rc<RefCell<St>>,
}

/// What the Rust functions share: the table and outputs of the call in progress, the kept
/// functions, the config root.
struct St {
    table: Table,
    out: Vec<Request>,
    fns: Vec<Function>,
    root: Option<PathBuf>,
    read: ReadFile,
    /// The budget handle, for the protected calls; taken out on drop (it holds the state, the
    /// state holds the functions that hold this).
    sandbox: Option<Sandbox>,
}

type Shared = Rc<RefCell<St>>;

impl Runtime {
    /// Builds the sandbox and runs the init file `src` (chunk name `name`, root `root`).
    /// `None` when the state itself cannot be built (reported in the requests).
    pub(crate) fn start(
        src: &str,
        name: &str,
        root: Option<&Path>,
        read: ReadFile,
    ) -> (Option<Runtime>, Table, Vec<Request>) {
        let st = Rc::new(RefCell::new(St {
            table: Table::default(),
            out: Vec::new(),
            fns: Vec::new(),
            root: root.map(Path::to_path_buf),
            read,
            sandbox: None,
        }));
        let (lua, sandbox) = match sandbox(&st) {
            Ok(l) => l,
            Err(e) => {
                let msg = format!("{name}: cannot start Lua: {}", first_line(&e));
                return (None, Table::default(), vec![error(msg)]);
            }
        };
        st.borrow_mut().sandbox = Some(sandbox.clone());
        let rt = Runtime { lua, sandbox, st };
        let chunk = format!("@{name}");
        let (table, out) = rt.with_state(Table::default(), name, |lua| {
            text_chunk(lua, src, &chunk)?.call::<_, ()>(())
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
        let func = self.st.borrow().fns.get(usize::from(f.0)).cloned();
        self.with_state(table, &what, |lua| {
            let func = func.ok_or_else(|| runtime(format!("no function {}", f.0)))?;
            match ev {
                None => func.call::<_, ()>(()),
                Some(e) => {
                    let t = lua.create_table()?;
                    t.raw_set("event", e.word())?;
                    for &(k, v) in fields {
                        t.raw_set(k, v)?;
                    }
                    func.call::<_, ()>(t)
                }
            }
        })
    }

    /// Runs `run` with `table` and a fresh budget; returns the table and the requests, plus
    /// one error request when `run` fails (`what` names it when the error carries no
    /// location).
    fn with_state(
        &self,
        table: Table,
        what: &str,
        run: impl FnOnce(&Lua) -> omnilua::Result<()>,
    ) -> (Table, Vec<Request>) {
        {
            let mut st = self.st.borrow_mut();
            st.table = table;
            st.out.clear();
        }
        arm(&self.lua, &self.sandbox, BUDGET);
        let result = run(&self.lua);
        let trip = self.sandbox.tripped();
        let mut st = self.st.borrow_mut();
        if let Err(e) = result {
            let msg = match trip {
                Some(TripReason::Instructions) => {
                    format!("instruction budget exhausted ({BUDGET} per call)")
                }
                Some(TripReason::Memory) => out_of_memory(),
                None => first_line(&e),
            };
            let located = msg.contains(".lua:") || msg.starts_with(what);
            let text = match (located, traceback_location(&e)) {
                (true, _) => msg,
                (false, Some(at)) => format!("{at}: {msg}"),
                (false, None) => format!("{what}: {msg}"),
            };
            st.out.push(error(text));
        }
        let out = std::mem::take(&mut st.out);
        let table = std::mem::take(&mut st.table);
        assert!(out.len() <= REQUESTS_MAX + 1);
        assert!(table.keymaps.chord.len() == table.keymaps.target.len());
        (table, out)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Break the cycle state → functions → `St` → sandbox → state.
        let mut st = self.st.borrow_mut();
        st.sandbox = None;
        st.fns.clear();
    }
}

/// Refills the instruction budget to `budget` and clears a trip.
fn arm(lua: &Lua, sandbox: &Sandbox, budget: u64) {
    sandbox.reset();
    let cfg = SandboxConfig {
        instruction_limit: Some(budget),
        memory_limit_bytes: Some(MEMORY_MAX),
        check_interval: CHECK_EVERY,
        remove_globals: Vec::new(),
    };
    lua.install_sandbox(cfg)
        .expect("re-arming the sandbox only sets limits");
}

/// A Lua state with only the safe libraries, the budget, the memory cap and the
/// `schemify` API.
fn sandbox(st: &Shared) -> omnilua::Result<(Lua, Sandbox)> {
    // No host hooks: no files, processes, environment or dynamic libraries.
    let lua = Lua::with_hooks_versioned(HostHooks::default(), LuaVersion::V54)?;
    lua.set_capture_tracebacks(true);
    let raw = api(&lua, st)?;
    let getinfo: Function = lua.globals().get::<_, LTable>("debug")?.get("getinfo")?;
    lua.load(PRELUDE)
        .set_name("=schemify")
        .into_function()?
        .call::<_, ()>((raw, getinfo))?;
    let cfg = SandboxConfig {
        instruction_limit: Some(BUDGET),
        memory_limit_bytes: Some(MEMORY_MAX),
        check_interval: CHECK_EVERY,
        remove_globals: REMOVED.iter().map(|g| g.as_bytes().to_vec()).collect(),
    };
    let sandbox = lua.install_sandbox(cfg)?;
    Ok((lua, sandbox))
}

/// Compiles `src` as a text chunk: precompiled bytecode (which can crash a VM) never loads.
fn text_chunk(lua: &Lua, src: &str, name: &str) -> omnilua::Result<Function> {
    if src.as_bytes().first() == Some(&0x1b) {
        return Err(runtime(format!(
            "{}: attempt to load a binary chunk (mode is 't')",
            name.trim_start_matches('@')
        )));
    }
    lua.load(src).set_name(name).into_function()
}

/// The `raw` table the prelude wraps.
fn api(lua: &Lua, st: &Shared) -> omnilua::Result<LTable> {
    let t = lua.create_table()?;
    macro_rules! raw {
        ($name:literal, $f:expr) => {{
            let st = st.clone();
            let f = $f;
            t.raw_set($name, lua.create_function(move |lua, a| f(lua, &st, a))?)?;
        }};
    }
    raw!("get_opt", get_opt);
    raw!("set_opt", set_opt);
    raw!("keymap_set", keymap_set);
    raw!("cmd", cmd);
    raw!("on", on);
    raw!("sim", sim);
    raw!("notify", notify);
    raw!("pack_add", pack_add);
    raw!("find_module", find_module);
    raw!("pcall", pcall);
    raw!("xpcall", xpcall);
    Ok(t)
}

// ------------------------------------------------------------------ helpers

fn runtime(msg: String) -> LuaError {
    RawError::runtime(format_args!("{msg}")).into()
}

/// The first line of an error's message.
fn first_line(e: &LuaError) -> String {
    let text = match &**e {
        RawError::Memory => out_of_memory(),
        other => other.message_lossy(),
    };
    text.lines().next().unwrap_or("").to_owned()
}

fn out_of_memory() -> String {
    format!("not enough memory ({} MiB cap)", MEMORY_MAX >> 20)
}

/// `file:line` of the innermost config frame (`*.lua`) in an error's traceback: budget and
/// memory trips carry no location of their own.
fn traceback_location(e: &LuaError) -> Option<String> {
    let tb = e.traceback_lossy()?;
    tb.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("[C]") && !l.starts_with("stack traceback"))
        .filter_map(|l| l.split_once(": in ").map(|(at, _)| at.to_owned()))
        .find(|at| at.contains(".lua:"))
}

/// Queues a request; past [`REQUESTS_MAX`] the call fails.
fn push(st: &Shared, r: Request) -> Option<String> {
    let mut st = st.borrow_mut();
    if st.out.len() >= REQUESTS_MAX {
        return Some(format!("more than {REQUESTS_MAX} requests in one call"));
    }
    st.out.push(r);
    None
}

/// One word: non-empty, no whitespace, at most `max` bytes.
fn is_word(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_whitespace)
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Nil => "nil",
        Value::Boolean(_) => "boolean",
        Value::Integer(_) | Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Table(_) => "table",
        Value::Function(_) => "function",
        Value::UserData(_) | Value::LightUserData(_) => "userdata",
        Value::Thread(_) => "thread",
    }
}

/// A string argument (numbers convert, as Lua's `tostring` would for `luaL_checkstring`).
fn string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => s.to_str().ok(),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn bad_arg(n: u8, func: &str, want: &str, got: &Value) -> String {
    format!(
        "bad argument #{n} to '{func}' ({want} expected, got {})",
        type_name(got)
    )
}

fn keep(st: &Shared, f: Function) -> Result<FnId, String> {
    let mut st = st.borrow_mut();
    if st.fns.len() >= FNS_MAX {
        return Err(format!("more than {FNS_MAX} functions kept"));
    }
    st.fns.push(f);
    Ok(FnId(
        u16::try_from(st.fns.len() - 1).expect("FNS_MAX < 2^16"),
    ))
}

/// A table field (`nil` when absent).
fn field(t: &LTable, k: &str) -> Value {
    t.raw_get::<_, Value>(k).unwrap_or(Value::Nil)
}

// ------------------------------------------------------------------ pcall

/// `pcall` that cannot swallow budget exhaustion (a `while true do pcall(f) end` would
/// otherwise outlive any budget). A memory-cap error is caught like any error, and the cap is
/// re-armed with the budget left. The error value is its message string.
fn pcall(lua: &Lua, st: &Shared, a: Variadic<Value>) -> omnilua::Result<Variadic<Value>> {
    let mut a = a.into_vec();
    if a.is_empty() {
        return Err(runtime(
            "bad argument #1 to 'pcall' (value expected)".into(),
        ));
    }
    let f = a.remove(0);
    let failed = match protected(lua, st, f, a)? {
        Ok(v) => return Ok(v),
        Err(msg) => Value::String(lua.create_string(msg)?),
    };
    Ok(Variadic::from(vec![Value::Boolean(false), failed]))
}

/// `xpcall` with the same rule as [`pcall`]; the handler gets the message string.
fn xpcall(lua: &Lua, st: &Shared, a: Variadic<Value>) -> omnilua::Result<Variadic<Value>> {
    let mut a = a.into_vec();
    if a.len() < 2 {
        return Err(runtime(
            "bad argument #2 to 'xpcall' (value expected)".into(),
        ));
    }
    let f = a.remove(0);
    let handler = match a.remove(0) {
        Value::Function(h) => h,
        other => return Err(runtime(bad_arg(2, "xpcall", "function", &other))),
    };
    let failed: Value = match protected(lua, st, f, a)? {
        Ok(v) => return Ok(v),
        Err(msg) => handler.call(msg)?,
    };
    Ok(Variadic::from(vec![Value::Boolean(false), failed]))
}

/// Calls `f(args…)` for [`pcall`]/[`xpcall`]: `Ok(true, results…)`, or `Err(message)` for an
/// error a protected call may catch; budget exhaustion propagates as the outer `Err`.
fn protected(
    lua: &Lua,
    st: &Shared,
    f: Value,
    args: Vec<Value>,
) -> omnilua::Result<Result<Variadic<Value>, String>> {
    let result = match f {
        Value::Function(f) => f.call::<_, Variadic<Value>>(Variadic::from(args)),
        other => Err(runtime(format!(
            "attempt to call a {} value",
            type_name(&other)
        ))),
    };
    match result {
        Ok(v) => {
            let mut all = vec![Value::Boolean(true)];
            all.extend(v.into_vec());
            Ok(Ok(Variadic::from(all)))
        }
        Err(e) => caught(lua, st, e).map(Err),
    }
}

/// The message a protected call returns for `e`, or `e` itself when it must propagate (the
/// instruction budget). A memory trip is recoverable: the sandbox is re-armed with the
/// instructions this call has left, so a retry loop still ends.
fn caught(lua: &Lua, st: &Shared, e: LuaError) -> omnilua::Result<String> {
    let sandbox = st.borrow().sandbox.clone().expect("armed while Lua runs");
    match sandbox.tripped() {
        Some(TripReason::Instructions) => Err(e),
        Some(TripReason::Memory) => {
            let left = sandbox.instructions_remaining().unwrap_or(1).max(1);
            arm(lua, &sandbox, left);
            Ok(out_of_memory())
        }
        None => Ok(first_line(&e)),
    }
}

// ------------------------------------------------------------------ schemify.*

fn get_opt(lua: &Lua, st: &Shared, key: Value) -> omnilua::Result<(Value, Option<String>)> {
    let o = st.borrow().table.opts;
    let step = |milli: i32| Value::Number(f64::from(milli) / 1000.0);
    let key = string(&key).unwrap_or_default();
    Ok(match key.as_str() {
        "snap" => (step(o.snap), None),
        "grid" => (step(o.grid), None),
        "show_grid" => (Value::Boolean(o.show_grid), None),
        "light" => (Value::Boolean(o.light), None),
        "dim" => (Value::Number(f64::from(o.dim) / 100.0), None),
        "line_width" => (Value::Number(f64::from(o.line_width_tenths) / 10.0), None),
        "simulator" => (Value::String(lua.create_string(o.simulator.word())?), None),
        // A copy: assigning to one of its entries changes nothing (spec §3).
        "layer_colors" => {
            let t = lua.create_table()?;
            for (layer, c) in o.layer_colors.iter().enumerate() {
                if let Some(rgb) = c {
                    t.raw_set(layer as i64, format!("{rgb:06x}"))?;
                }
            }
            (Value::Table(t), None)
        }
        _ => (Value::Nil, Some(format!("unknown option `{key}`"))),
    })
}

/// A Lua number (integer or float).
fn number(v: &Value) -> Option<f64> {
    match *v {
        Value::Integer(i) => Some(i as f64),
        Value::Number(n) if n.is_finite() => Some(n),
        _ => None,
    }
}

/// `{ [layer] = "rrggbb" | "#rrggbb", … }` with layers in `[0, USER_LAYERS)`; `None` for
/// any other key, value or a table of more than [`USER_LAYERS`] entries.
fn layer_colors(v: &Value) -> Option<[Option<u32>; USER_LAYERS]> {
    let Value::Table(t) = v else {
        return None;
    };
    let mut out = [None; USER_LAYERS];
    let mut n = 0usize;
    for pair in t.pairs().ok()? {
        let (k, c) = pair.ok()?;
        n += 1;
        if n > USER_LAYERS {
            return None;
        }
        let layer = match k {
            Value::Integer(i) => usize::try_from(i).ok().filter(|&l| l < USER_LAYERS)?,
            _ => return None,
        };
        let Value::String(c) = c else {
            return None;
        };
        let text = c.to_str().ok()?;
        let hex = text.strip_prefix('#').unwrap_or(&text);
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        out[layer] = Some(u32::from_str_radix(hex, 16).ok()?);
    }
    assert!(n <= USER_LAYERS);
    Some(out)
}

fn set_opt(_: &Lua, st: &Shared, (key, v): (Value, Value)) -> omnilua::Result<Option<String>> {
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
    let key = string(&key).unwrap_or_default();
    let mut s = st.borrow_mut();
    let o = &mut s.table.opts;
    let set = match key.as_str() {
        "snap" => step(&v).map(|x| o.snap = x),
        "grid" => step(&v).map(|x| o.grid = x),
        "show_grid" => flag(&v).map(|b| o.show_grid = b),
        "light" => flag(&v).map(|b| o.light = b),
        "dim" => number(&v)
            .filter(|f| (0.0..=1.0).contains(f))
            .map(|f| o.dim = (f * 100.0).round() as u8),
        "line_width" => number(&v).filter(|w| (0.1..=10.0).contains(w)).map(|w| {
            o.line_width_tenths = (w * 10.0).round().clamp(1.0, 100.0) as u8;
        }),
        "layer_colors" => layer_colors(&v).map(|c| o.layer_colors = c),
        "simulator" => match &v {
            // A Lua string only: `string` would also take a number.
            Value::String(_) => [SimKind::Ngspice, SimKind::Espice]
                .into_iter()
                .find(|k| string(&v).as_deref() == Some(k.word()))
                .map(|k| o.simulator = k),
            _ => None,
        },
        _ => return Ok(Some(format!("unknown option `{key}`"))),
    };
    Ok(match set {
        Some(()) => None,
        None => Some(format!("option `{key}`: bad value ({})", type_name(&v))),
    })
}

fn keymap_set(
    _: &Lua,
    st: &Shared,
    (chord, target, opts): (Value, Value, Value),
) -> omnilua::Result<Option<String>> {
    let Some(chord) = string(&chord) else {
        return Ok(Some(bad_arg(1, "set", "string", &chord)));
    };
    if !is_word(&chord, 64) {
        return Ok(Some(format!("bad chord `{chord}`")));
    }
    let target = match target {
        Value::String(s) => {
            let s = s.to_str()?;
            if !is_word(&s, 128) {
                return Ok(Some(format!("bad command name `{s}`")));
            }
            Target::Command(s)
        }
        Value::Function(f) => match keep(st, f) {
            Ok(id) => Target::Function(id),
            Err(e) => return Ok(Some(e)),
        },
        other => {
            return Ok(Some(format!(
                "keymap target must be a command name or a function, not {}",
                type_name(&other)
            )));
        }
    };
    let desc = match &opts {
        Value::Table(t) => string(&field(t, "desc")).unwrap_or_default(),
        _ => String::new(),
    };
    let mut s = st.borrow_mut();
    let k = &mut s.table.keymaps;
    if let Some(i) = k.chord.iter().position(|c| *c == chord) {
        k.target[i] = target;
        k.desc[i] = desc;
        return Ok(None);
    }
    if k.chord.len() >= KEYMAPS_MAX {
        return Ok(Some(format!("more than {KEYMAPS_MAX} keymaps")));
    }
    k.chord.push(chord);
    k.target.push(target);
    k.desc.push(desc);
    Ok(None)
}

fn cmd(_: &Lua, st: &Shared, (name, args): (Value, Value)) -> omnilua::Result<Option<String>> {
    let Some(name) = string(&name) else {
        return Ok(Some(bad_arg(1, "cmd", "string", &name)));
    };
    if !is_word(&name, 128) {
        return Ok(Some(format!("bad command name `{name}`")));
    }
    let args = match &args {
        Value::Nil => String::new(),
        v => match string(v) {
            Some(s) => s,
            None => return Ok(Some(bad_arg(2, "cmd", "string", v))),
        },
    };
    Ok(push(st, Request::Command { name, args }))
}

fn on(_: &Lua, st: &Shared, (event, f): (Value, Value)) -> omnilua::Result<Option<String>> {
    let Some(event) = string(&event) else {
        return Ok(Some(bad_arg(1, "on", "string", &event)));
    };
    let Value::Function(f) = f else {
        return Ok(Some(bad_arg(2, "on", "function", &f)));
    };
    let Some(ev) = Event::from_word(&event) else {
        return Ok(Some(format!("unknown event `{event}`")));
    };
    if st.borrow().table.hooks.len() >= HOOKS_MAX {
        return Ok(Some(format!("more than {HOOKS_MAX} hooks")));
    }
    match keep(st, f) {
        Ok(id) => {
            st.borrow_mut().table.hooks.push((ev, id));
            Ok(None)
        }
        Err(e) => Ok(Some(e)),
    }
}

fn sim(_: &Lua, st: &Shared, (name, spec): (Value, Value)) -> omnilua::Result<Option<String>> {
    let Some(name) = string(&name) else {
        return Ok(Some(bad_arg(1, "sim", "string", &name)));
    };
    let Some(kind) = SimKind::ALL.into_iter().find(|k| k.word() == name) else {
        return Ok(Some(format!(
            "unknown program `{name}` (ngspice, espice, openvaf, iverilog)"
        )));
    };
    let Value::Table(spec) = spec else {
        return Ok(Some(bad_arg(2, "sim", "table", &spec)));
    };
    let Some(path) = string(&field(&spec, "path")).filter(|p| !p.is_empty()) else {
        return Ok(Some(format!("sim `{name}`: `path` is required")));
    };
    let args = match field(&spec, "args") {
        Value::Nil => Vec::new(),
        Value::Table(t) => match strings(&t) {
            Some(a) => a,
            None => {
                return Ok(Some(format!(
                    "sim `{name}`: `args` must be a list of strings"
                )));
            }
        },
        _ => {
            return Ok(Some(format!(
                "sim `{name}`: `args` must be a list of strings"
            )));
        }
    };
    let env = match field(&spec, "env") {
        Value::Nil => Vec::new(),
        Value::Table(t) => match env_pairs(&t) {
            Some(e) => e,
            None => {
                return Ok(Some(format!(
                    "sim `{name}`: `env` must map at most {SIM_ENV_MAX} names to strings"
                )));
            }
        },
        _ => {
            return Ok(Some(format!(
                "sim `{name}`: `env` must map at most {SIM_ENV_MAX} names to strings"
            )));
        }
    };
    st.borrow_mut().table.sims[kind as usize] = Some(crate::Sim { path, args, env });
    Ok(None)
}

/// `{ NAME = "value", … }` as pairs sorted by name (so the table does not depend on Lua's
/// iteration order); `None` for a key that is not a non-empty string without `=`, a value
/// that is not a string, or more than [`SIM_ENV_MAX`] entries.
fn env_pairs(t: &LTable) -> Option<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    for pair in t.pairs().ok()? {
        let (k, v): (Value, Value) = pair.ok()?;
        let Value::String(_) = k else { return None };
        let k = string(&k).filter(|k| !k.is_empty() && !k.contains(['=', '\0']))?;
        out.push((k, string(&v)?));
        if out.len() > SIM_ENV_MAX {
            return None;
        }
    }
    out.sort_unstable();
    assert!(
        out.windows(2).all(|w| w[0].0 < w[1].0),
        "table keys are unique"
    );
    Some(out)
}

/// The sequence part of `t` as strings; `None` if an element is not a string.
fn strings(t: &LTable) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for i in 1..=t.len().ok()? {
        out.push(string(&t.raw_get::<_, Value>(i as i64).ok()?)?);
    }
    Some(out)
}

fn notify(_: &Lua, st: &Shared, (msg, level): (Value, Value)) -> omnilua::Result<Option<String>> {
    let Some(text) = string(&msg) else {
        return Ok(Some(bad_arg(1, "notify", "string", &msg)));
    };
    let level = match string(&level).as_deref() {
        None | Some("info") => Level::Info,
        Some("warn") => Level::Warn,
        Some("error") => Level::Error,
        Some(other) => return Ok(Some(format!("unknown level `{other}`"))),
    };
    Ok(push(st, Request::Notify { level, text }))
}

/// `schemify.pack.add { spec, … }`: a bad spec is reported (at `here`, the caller's
/// `file:line: `) and dropped; the rest stay.
fn pack_add(
    _: &Lua,
    st: &Shared,
    (here, specs): (Value, Value),
) -> omnilua::Result<Option<String>> {
    let here = string(&here).unwrap_or_default();
    let Value::Table(specs) = specs else {
        return Ok(Some(bad_arg(1, "add", "table", &specs)));
    };
    let root = st.borrow().root.clone();
    let n = specs.len()?;
    for i in 1..=n {
        let v: Value = specs.raw_get(i as i64)?;
        let spec = match pack_spec(v, root.as_deref()) {
            Ok(s) => s,
            Err(msg) => {
                if let Some(e) = push(st, error(format!("{here}pack spec: {msg}"))) {
                    return Ok(Some(e));
                }
                continue;
            }
        };
        let mut s = st.borrow_mut();
        let packs = &mut s.table.packs;
        if let Some(i) = packs.iter().position(|p| p.name == spec.name) {
            packs[i] = spec;
            continue;
        }
        if packs.len() >= PACKS_MAX {
            return Ok(Some(format!("more than {PACKS_MAX} packages")));
        }
        packs.push(spec);
    }
    Ok(None)
}

/// `require "a.b"`'s file: `<root>/lua/a/b.lua` or `<root>/lua/a/b/init.lua`, compiled (the
/// prelude runs it once and caches the result). Returns the chunk, or `nil` and the error.
fn find_module(lua: &Lua, st: &Shared, name: Value) -> omnilua::Result<(Value, Option<String>)> {
    let name = string(&name).unwrap_or_default();
    let ok = !name.is_empty()
        && name.split('.').all(|seg| {
            !seg.is_empty()
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        });
    if !ok {
        return Ok((Value::Nil, Some(format!("bad module name `{name}`"))));
    }
    let (root, read) = {
        let s = st.borrow();
        (s.root.clone(), s.read)
    };
    let Some(root) = root else {
        return Ok((
            Value::Nil,
            Some(format!("module `{name}`: no config directory")),
        ));
    };
    let rel = name.replace('.', "/");
    let candidates = [format!("lua/{rel}.lua"), format!("lua/{rel}/init.lua")];
    let Some((file, src)) = candidates
        .iter()
        .find_map(|c| read(&root.join(c)).map(|s| (c, s)))
    else {
        return Ok((
            Value::Nil,
            Some(format!(
                "module `{name}` not found under {}/lua",
                root.display()
            )),
        ));
    };
    match text_chunk(lua, &src, &format!("@{file}")) {
        Ok(f) => Ok((Value::Function(f), None)),
        Err(e) => Ok((Value::Nil, Some(first_line(&e)))),
    }
}

const SPEC_FIELDS: [&str; 10] = [
    "src", "url", "sha256", "path", "name", "rev", "version", "grant", "fuel", "lazy",
];

/// Validates one spec (spec §5). A bare string is `{ src = … }`.
fn pack_spec(v: Value, root: Option<&Path>) -> Result<PackSpec, String> {
    let t = match v {
        Value::String(s) => {
            let url = s.to_str().map_err(|e| e.to_string())?;
            return finish_spec(
                Source::Git { url, rev: None },
                None,
                Vec::new(),
                None,
                (Vec::new(), Vec::new()),
            );
        }
        Value::Table(t) => t,
        other => return Err(format!("a spec is a table, not {}", type_name(&other))),
    };
    let s = |k: &str| -> Result<Option<String>, String> {
        match field(&t, k) {
            Value::Nil => Ok(None),
            Value::String(v) => v.to_str().map(Some).map_err(|e| e.to_string()),
            _ => Err(format!("`{k}` must be a string")),
        }
    };
    for pair in t.pairs().map_err(|e| e.to_string())? {
        let (k, _) = pair.map_err(|e| e.to_string())?;
        let known = match &k {
            Value::String(k) => k.to_str().is_ok_and(|k| SPEC_FIELDS.contains(&k.as_str())),
            _ => false,
        };
        if !known {
            let shown = string(&k).unwrap_or_else(|| type_name(&k).to_owned());
            return Err(format!("unknown field {shown}"));
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
    let grant: Vec<String> = match field(&t, "grant") {
        Value::Nil => Vec::new(),
        Value::Table(g) => strings(&g).ok_or("`grant` must be a list of strings")?,
        _ => return Err("`grant` must be a list of strings".into()),
    };
    if let Some(g) = grant.iter().find(|g| !is_word(g, 128)) {
        return Err(format!("bad grant `{g}`"));
    }
    let fuel = match field(&t, "fuel") {
        Value::Nil => None,
        Value::Integer(n) if (1_000_000..=100_000_000_000).contains(&n) => Some(n as u64),
        _ => return Err("`fuel` must be a whole number in [10^6, 10^11]".into()),
    };
    let lazy = match field(&t, "lazy") {
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
    let v: Vec<String> = match field(t, key) {
        Value::Nil => Vec::new(),
        Value::String(s) => vec![s.to_str().map_err(|_| bad())?],
        Value::Table(l) => strings(&l).ok_or_else(bad)?,
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

/// `~/x` against `HOME`, a relative path against the config root; `.` segments are dropped
/// lexically (the lockfile and `pack status` print the result), `..` is kept because only
/// the filesystem can resolve it past a symlink.
fn expand(p: &str, root: Option<&Path>) -> PathBuf {
    let joined = if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
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
    debug_assert!(
        clean
            .components()
            .skip(1)
            .all(|c| c != std::path::Component::CurDir)
    );
    clean
}
