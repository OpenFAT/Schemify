//! The sandboxed Lua 5.4 state (config spec §4) on omnilua, a pure-Rust Lua 5.4, so native and
//! web builds run the same interpreter (web spec §5): Lua source → the [`Table`] it writes
//! and the [`Request`]s it queues, under an instruction budget and a memory cap.
//!
//! During a call the table being written and the request list live in [`St`], shared with
//! the `schemify.*` functions (`api`); [`Runtime::start`] and [`Runtime::call`] move the
//! table in before and out after, so it has one writer and callers never see Lua values. Lua
//! functions the config keeps (keymap targets, hooks) live in [`St::fns`], indexed by
//! [`FnId`]. This file also holds the value readers the API shares and the budget-proof
//! `pcall`/`xpcall` (spec §4: they never catch budget exhaustion).
//!
//! Contents:
//! - `pub(crate) struct Runtime`, `pub(crate) struct St`, `pub(crate) type Shared`
//! - `Runtime::start(src, name, root, read) -> (Option<Runtime>, Table, Vec<Request>)`
//! - `Runtime::call(&mut self, table, f, ev, fields) -> (Table, Vec<Request>)`
//! - `pub(crate) fn pcall`, `pub(crate) fn xpcall`, `pub(crate) fn text_chunk`
//! - value readers: `pub(crate) fn string`, `pub(crate) fn type_name`, `pub(crate) fn field`,
//!   `pub(crate) fn strings`, `pub(crate) fn bad_arg`, `pub(crate) fn is_word`
//! - `pub(crate) fn push`, `pub(crate) fn keep`, `pub(crate) fn first_line`

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use omnilua::{
    Error as LuaError, Function, HostHooks, Lua, LuaError as RawError, LuaVersion, Sandbox,
    SandboxConfig, Table as LTable, TripReason, Value, Variadic,
};

use crate::{BUDGET, Event, FnId, MEMORY_MAX, REQUESTS_MAX, ReadFile, Request, Table, error};

/// The Lua state of one config.
pub(crate) struct Runtime {
    lua: Lua,
    st: Shared,
}

/// What the `schemify.*` functions share: the table and requests of the call in progress,
/// the kept functions, the config root.
pub(crate) struct St {
    /// The table being written; empty between calls.
    pub(crate) table: Table,
    /// Requests of the call in progress, at most [`REQUESTS_MAX`].
    pub(crate) out: Vec<Request>,
    /// Kept functions, at most [`FNS_MAX`]; a [`FnId`] indexes them.
    pub(crate) fns: Vec<Function>,
    /// The config root `require` and local package paths resolve against.
    pub(crate) root: Option<PathBuf>,
    /// How `require` reads a module.
    pub(crate) read: ReadFile,
    /// The budget handle, for the protected calls. Taken out on drop: the state holds the
    /// functions that hold this, so the handle would keep the state alive.
    sandbox: Option<Sandbox>,
}

/// [`St`] as the Rust functions capture it.
pub(crate) type Shared = Rc<RefCell<St>>;

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

const _: () = assert!(FNS_MAX <= u16::MAX as usize + 1);
const _: () = assert!(crate::KEYMAPS_MAX + crate::HOOKS_MAX <= FNS_MAX);

impl Runtime {
    /// Builds the sandbox and runs init file `src` (chunk name `name`; `require` reads through
    /// `read` under `root`). Returns the state, the table the run wrote and its requests; the
    /// state is `None`, with one error request, when it cannot be built.
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
        st.borrow_mut().sandbox = Some(sandbox);
        let rt = Runtime { lua, st };
        let chunk = format!("@{name}");
        let (table, out) = rt.with_state(Table::default(), name, |lua| {
            text_chunk(lua, src, &chunk)?.call::<_, ()>(())
        });
        (Some(rt), table, out)
    }

    /// Calls kept function `f` on `table` under a fresh budget, with an event table (`event`
    /// = `ev`'s word plus `fields`) or no argument; returns the table and the call's requests.
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
            let Some(e) = ev else {
                return func.call::<_, ()>(());
            };
            let t = lua.create_table()?;
            t.raw_set("event", e.word())?;
            for &(k, v) in fields {
                t.raw_set(k, v)?;
            }
            func.call::<_, ()>(t)
        })
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

/// `pcall` that cannot swallow budget exhaustion (a `while true do pcall(f) end` would
/// otherwise outlive any budget). The error value is its message string.
pub(crate) fn pcall(
    lua: &Lua,
    st: &Shared,
    a: Variadic<Value>,
) -> omnilua::Result<Variadic<Value>> {
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

/// `xpcall` with the rule of [`pcall`]; the handler gets the message string.
pub(crate) fn xpcall(
    lua: &Lua,
    st: &Shared,
    a: Variadic<Value>,
) -> omnilua::Result<Variadic<Value>> {
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

/// Compiles `src` as a text chunk named `name`: precompiled bytecode (which can crash a VM)
/// never loads.
pub(crate) fn text_chunk(lua: &Lua, src: &str, name: &str) -> omnilua::Result<Function> {
    if src.as_bytes().first() == Some(&0x1b) {
        return Err(runtime(format!(
            "{}: attempt to load a binary chunk (mode is 't')",
            name.trim_start_matches('@')
        )));
    }
    lua.load(src).set_name(name).into_function()
}

/// A string argument; numbers convert, as `luaL_checkstring` would.
pub(crate) fn string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => s.to_str().ok(),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Lua's `type(v)`.
pub(crate) fn type_name(v: &Value) -> &'static str {
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

/// Field `k` of `t` without metamethods; `nil` when absent.
pub(crate) fn field(t: &LTable, k: &str) -> Value {
    t.raw_get::<_, Value>(k).unwrap_or(Value::Nil)
}

/// The sequence part of `t` as strings; `None` if an element is not a string or a number.
pub(crate) fn strings(t: &LTable) -> Option<Vec<String>> {
    let n = t.len().ok()?;
    let mut out = Vec::new();
    for i in 1..=n {
        out.push(string(&t.raw_get::<_, Value>(i as i64).ok()?)?);
    }
    assert_eq!(out.len() as u64, n);
    Some(out)
}

/// `luaL_argerror`'s message.
pub(crate) fn bad_arg(n: u8, func: &str, want: &str, got: &Value) -> String {
    assert!(n >= 1);
    format!(
        "bad argument #{n} to '{func}' ({want} expected, got {})",
        type_name(got)
    )
}

/// One word: non-empty, no whitespace, at most `max` bytes.
pub(crate) fn is_word(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_whitespace)
}

/// Queues a request; past [`REQUESTS_MAX`] returns the error that fails the call.
pub(crate) fn push(st: &Shared, r: Request) -> Option<String> {
    let mut st = st.borrow_mut();
    if st.out.len() >= REQUESTS_MAX {
        return Some(format!("more than {REQUESTS_MAX} requests in one call"));
    }
    st.out.push(r);
    assert!(st.out.len() <= REQUESTS_MAX);
    None
}

/// Keeps `f` for later calls; past [`FNS_MAX`] returns the error that fails the call.
pub(crate) fn keep(st: &Shared, f: Function) -> Result<FnId, String> {
    let mut st = st.borrow_mut();
    if st.fns.len() >= FNS_MAX {
        return Err(format!("more than {FNS_MAX} functions kept"));
    }
    st.fns.push(f);
    let id = u16::try_from(st.fns.len() - 1).expect("FNS_MAX <= 2^16");
    assert!(usize::from(id) < FNS_MAX);
    Ok(FnId(id))
}

/// The first line of an error's message.
pub(crate) fn first_line(e: &LuaError) -> String {
    let text = match &**e {
        RawError::Memory => out_of_memory(),
        other => other.message_lossy(),
    };
    text.lines().next().unwrap_or("").to_owned()
}

// ---- private ----

/// A Lua state with only the safe libraries, the budget, the memory cap and the `schemify`
/// API.
fn sandbox(st: &Shared) -> omnilua::Result<(Lua, Sandbox)> {
    // No host hooks: no files, processes, environment or dynamic libraries.
    let lua = Lua::with_hooks_versioned(HostHooks::default(), LuaVersion::V54)?;
    lua.set_capture_tracebacks(true);
    let raw = crate::api::table(&lua, st)?;
    // The prelude keeps `debug.getinfo` private before the sandbox removes `debug`.
    let getinfo: Function = lua.globals().get::<_, LTable>("debug")?.get("getinfo")?;
    (lua.load(PRELUDE).set_name("=schemify").into_function()?).call::<_, ()>((raw, getinfo))?;
    let sandbox = lua.install_sandbox(SandboxConfig {
        instruction_limit: Some(BUDGET),
        memory_limit_bytes: Some(MEMORY_MAX),
        check_interval: CHECK_EVERY,
        remove_globals: REMOVED.iter().map(|g| g.as_bytes().to_vec()).collect(),
    })?;
    Ok((lua, sandbox))
}

impl Runtime {
    /// Runs `run` on `table` with a fresh budget; returns the table and the requests, plus one
    /// error request when `run` fails (`what` names it when the error carries no location).
    fn with_state(
        &self,
        table: Table,
        what: &str,
        run: impl FnOnce(&Lua) -> omnilua::Result<()>,
    ) -> (Table, Vec<Request>) {
        let sandbox = {
            let mut st = self.st.borrow_mut();
            assert!(st.out.is_empty(), "requests are drained after every call");
            st.table = table;
            st.sandbox.clone().expect("armed for the state's life")
        };
        arm(&self.lua, &sandbox, BUDGET);
        let result = run(&self.lua);
        let mut st = self.st.borrow_mut();
        if let Err(e) = result {
            let msg = match sandbox.tripped() {
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
        assert_eq!(table.keymaps.chord.len(), table.keymaps.target.len());
        (table, out)
    }
}

/// Refills the instruction budget to `budget` and clears a trip.
fn arm(lua: &Lua, sandbox: &Sandbox, budget: u64) {
    assert!(budget > 0);
    assert!(budget <= BUDGET);
    sandbox.reset();
    let cfg = SandboxConfig {
        instruction_limit: Some(budget),
        memory_limit_bytes: Some(MEMORY_MAX),
        check_interval: CHECK_EVERY,
        remove_globals: Vec::new(),
    };
    (lua.install_sandbox(cfg)).expect("re-arming the sandbox only sets limits");
}

fn runtime(msg: String) -> LuaError {
    RawError::runtime(format_args!("{msg}")).into()
}

fn out_of_memory() -> String {
    format!("not enough memory ({} MiB cap)", MEMORY_MAX >> 20)
}

/// `file:line` of the innermost config frame (`*.lua`) in an error's traceback: budget and
/// memory trips carry no location of their own.
fn traceback_location(e: &LuaError) -> Option<String> {
    let tb = e.traceback_lossy()?;
    (tb.lines().map(str::trim))
        .filter(|l| !l.starts_with("[C]") && !l.starts_with("stack traceback"))
        .filter_map(|l| l.split_once(": in ").map(|(at, _)| at.to_owned()))
        .find(|at| at.contains(".lua:"))
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
    let e = match result {
        Ok(v) => {
            let mut all = vec![Value::Boolean(true)];
            all.extend(v.into_vec());
            return Ok(Ok(Variadic::from(all)));
        }
        Err(e) => e,
    };
    // A memory trip is recoverable: re-arm with the instructions this call has left, so a
    // retry loop still ends.
    let sandbox = st.borrow().sandbox.clone().expect("armed while Lua runs");
    match sandbox.tripped() {
        Some(TripReason::Instructions) => Err(e),
        Some(TripReason::Memory) => {
            let left = sandbox.instructions_remaining().unwrap_or(1).max(1);
            arm(lua, &sandbox, left);
            Ok(Err(out_of_memory()))
        }
        None => Ok(Err(first_line(&e))),
    }
}
