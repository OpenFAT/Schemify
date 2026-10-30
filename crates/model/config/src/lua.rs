//! The sandboxed Lua 5.4 state (config spec §4) on omnilua, a pure-Rust Lua 5.4, so native and
//! web builds run the same interpreter (web spec §5): Lua source → the [`Table`] it writes
//! and the [`Request`]s it queues, under an instruction budget and a memory cap.
//!
//! During a call the table being written and the request list live in [`State`], shared with
//! the `schemify.*` functions (`api`); [`Runtime::start`] and [`Runtime::call`] move the
//! table in before and out after, so it has one writer and callers never see Lua values. Lua
//! functions the config keeps (keymap targets, hooks) live in [`State::functions`], indexed
//! by [`FnId`]. This file also holds the value readers the API shares and the budget-proof
//! `pcall`/`xpcall` (spec §4: they never catch budget exhaustion).
//!
//! Contents:
//! - `pub(crate) struct Runtime`, `pub(crate) struct Started`, `pub(crate) struct State`,
//!   `pub(crate) type Shared`
//! - `Runtime::start(source, name, root, read) -> Started`
//! - `Runtime::call(&mut self, table, f, event, fields) -> (Table, Vec<Request>)`
//! - `pub(crate) fn pcall`, `pub(crate) fn xpcall`, `pub(crate) fn text_chunk`
//! - value readers: `pub(crate) fn string`, `pub(crate) fn type_name`, `pub(crate) fn field`,
//!   `pub(crate) fn strings`, `pub(crate) fn bad_arg`, `pub(crate) fn is_word`
//! - `pub(crate) fn push`, `pub(crate) fn keep`, `pub(crate) fn first_line`

use std::cell::RefCell;
use std::mem;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use omnilua::{
    Error as LuaError, Function, HostHooks, Lua, LuaError as RawError, LuaVersion, Sandbox,
    SandboxConfig, Table as LuaTable, TripReason, Value, Variadic,
};

use crate::{BUDGET, Event, FnId, MEMORY_MAX, REQUESTS_MAX, ReadFile, Request, Table, Target};

/// The Lua state of one config.
pub(crate) struct Runtime {
    lua: Lua,
    state: Shared,
}

/// What [`Runtime::start`] returns.
pub(crate) struct Started {
    /// The state; `None`, with one error request in `out`, when it could not be built.
    pub(crate) runtime: Option<Runtime>,
    /// The table the init file wrote.
    pub(crate) table: Table,
    /// The requests the init file queued.
    pub(crate) out: Vec<Request>,
}

/// What the `schemify.*` functions share: the table and requests of the call in progress,
/// the kept functions, the config root.
pub(crate) struct State {
    /// The table being written; empty between calls.
    pub(crate) table: Table,
    /// Requests of the call in progress, at most [`REQUESTS_MAX`].
    pub(crate) out: Vec<Request>,
    /// Kept functions, at most [`FUNCTIONS_MAX`]; a [`FnId`] indexes them.
    pub(crate) functions: Vec<Function>,
    /// The config root `require` and local package paths resolve against.
    pub(crate) root: Option<PathBuf>,
    /// How `require` reads a module.
    pub(crate) read: ReadFile,
    /// The budget handle, for the protected calls. Taken out on drop: the state holds the
    /// functions that hold this, so the handle would keep the state alive.
    sandbox: Option<Sandbox>,
}

/// [`State`] as the Rust functions capture it. Shared ownership because omnilua's
/// `create_function` takes `'static` closures: every `schemify.*` function the state keeps
/// must own its way to the table, and the runtime needs the same table between calls.
pub(crate) type Shared = Rc<RefCell<State>>;

/// Most functions one config may keep (every keymap and hook, plus rebinding headroom).
const FUNCTIONS_MAX: usize = 8192;
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

const _: () = assert!(FUNCTIONS_MAX <= u16::MAX as usize + 1);
const _: () = assert!(crate::KEYMAPS_MAX + crate::HOOKS_MAX <= FUNCTIONS_MAX);
const _: () = assert!((CHECK_EVERY as u64) < BUDGET);

impl Runtime {
    /// Builds the sandbox and runs init file `source` (chunk name `name`; `require` reads
    /// through `read` under `root`).
    pub(crate) fn start(source: &str, name: &str, root: Option<&Path>, read: ReadFile) -> Started {
        assert!(!name.is_empty(), "errors are located by the chunk name");
        let state = Rc::new(RefCell::new(State {
            table: Table::default(),
            out: Vec::new(),
            functions: Vec::new(),
            root: root.map(Path::to_path_buf),
            read,
            sandbox: None,
        }));
        let (lua, sandbox) = match sandbox(&state) {
            Ok(built) => built,
            Err(e) => {
                let message = format!("{name}: cannot start Lua: {}", first_line(&e));
                return Started {
                    runtime: None,
                    table: Table::default(),
                    out: vec![crate::error(message)],
                };
            }
        };
        state.borrow_mut().sandbox = Some(sandbox);
        let runtime = Runtime { lua, state };
        let chunk = format!("@{name}");
        let (table, out) = runtime.with_state(Table::default(), name, |lua| {
            text_chunk(lua, source, &chunk)?.call::<_, ()>(())
        });
        Started {
            runtime: Some(runtime),
            table,
            out,
        }
    }

    /// Calls kept function `f` on `table` under a fresh budget, with an event table (`event`
    /// = `event`'s word plus `fields`) or no argument; returns the table and the call's
    /// requests.
    pub(crate) fn call(
        &mut self,
        table: Table,
        f: FnId,
        event: Option<Event>,
        fields: &[(&str, i64)],
    ) -> (Table, Vec<Request>) {
        let what = format!("function {}", f.0);
        let function = self.state.borrow().functions.get(usize::from(f.0)).cloned();
        self.with_state(table, &what, |lua| {
            let function = function.ok_or_else(|| runtime(format!("no function {}", f.0)))?;
            let Some(event) = event else {
                return function.call::<_, ()>(());
            };
            let argument = lua.create_table()?;
            argument.raw_set("event", event.word())?;
            for &(name, value) in fields {
                argument.raw_set(name, value)?;
            }
            function.call::<_, ()>(argument)
        })
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Break the cycle state → functions → `State` → sandbox → state.
        let mut state = self.state.borrow_mut();
        state.sandbox = None;
        state.functions.clear();
    }
}

/// `pcall` that cannot swallow budget exhaustion (a `while true do pcall(f) end` would
/// otherwise outlive any budget). The error value is its message string.
pub(crate) fn pcall(
    lua: &Lua,
    state: &Shared,
    arguments: Variadic<Value>,
) -> omnilua::Result<Variadic<Value>> {
    let mut arguments = arguments.into_vec();
    if arguments.is_empty() {
        return Err(runtime(
            "bad argument #1 to 'pcall' (value expected)".to_owned(),
        ));
    }
    let f = arguments.remove(0);
    let failed = match protected(lua, state, f, arguments)? {
        Ok(results) => return Ok(results),
        Err(message) => Value::String(lua.create_string(message)?),
    };
    Ok(Variadic::from(vec![Value::Boolean(false), failed]))
}

/// `xpcall` with the rule of [`pcall`]; the handler gets the message string.
pub(crate) fn xpcall(
    lua: &Lua,
    state: &Shared,
    arguments: Variadic<Value>,
) -> omnilua::Result<Variadic<Value>> {
    let mut arguments = arguments.into_vec();
    if arguments.len() < 2 {
        return Err(runtime(
            "bad argument #2 to 'xpcall' (value expected)".to_owned(),
        ));
    }
    let f = arguments.remove(0);
    let handler = match arguments.remove(0) {
        Value::Function(handler) => handler,
        other => return Err(runtime(bad_arg(2, "xpcall", "function", &other))),
    };
    let failed: Value = match protected(lua, state, f, arguments)? {
        Ok(results) => return Ok(results),
        Err(message) => handler.call(message)?,
    };
    Ok(Variadic::from(vec![Value::Boolean(false), failed]))
}

/// Compiles `source` as a text chunk named `name` (`@file`): precompiled bytecode (which can
/// crash a VM) never loads.
pub(crate) fn text_chunk(lua: &Lua, source: &str, name: &str) -> omnilua::Result<Function> {
    assert!(
        name.starts_with('@'),
        "chunk names are `@file`, got `{name}`"
    );
    if source.as_bytes().first() == Some(&0x1b) {
        return Err(runtime(format!(
            "{}: attempt to load a binary chunk (mode is 't')",
            name.trim_start_matches('@')
        )));
    }
    lua.load(source).set_name(name).into_function()
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

/// Field `key` of `t` without metamethods; `nil` when absent.
pub(crate) fn field(t: &LuaTable, key: &str) -> Value {
    assert!(!key.is_empty(), "field names are this crate's literals");
    t.raw_get::<_, Value>(key).unwrap_or(Value::Nil)
}

/// The sequence part of `t` as strings; `None` if an element is not a string or a number.
pub(crate) fn strings(t: &LuaTable) -> Option<Vec<String>> {
    let n = t.len().ok()?;
    let mut out = Vec::new();
    for i in 1..=n {
        let index = i64::try_from(i).ok()?;
        out.push(string(&t.raw_get::<_, Value>(index).ok()?)?);
    }
    assert_eq!(out.len() as u64, n);
    Some(out)
}

/// `luaL_argerror`'s message.
pub(crate) fn bad_arg(n: u8, function: &str, want: &str, got: &Value) -> String {
    assert!(n >= 1, "Lua arguments count from 1");
    assert!(!function.is_empty());
    assert!(!want.is_empty());
    format!(
        "bad argument #{n} to '{function}' ({want} expected, got {})",
        type_name(got)
    )
}

/// One word: non-empty, no whitespace, at most `max` bytes.
pub(crate) fn is_word(s: &str, max: usize) -> bool {
    assert!(max > 0, "an empty word is never valid");
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_whitespace)
}

/// Queues a request; past [`REQUESTS_MAX`] returns the error that fails the call.
pub(crate) fn push(state: &Shared, request: Request) -> Option<String> {
    // The partner of `cmd`'s check, on the side that stores the row.
    if let Request::Command { name, .. } = &request {
        assert!(is_word(name, 128), "`schemify.cmd` checked the name");
    }
    let mut state = state.borrow_mut();
    if state.out.len() >= REQUESTS_MAX {
        return Some(format!("more than {REQUESTS_MAX} requests in one call"));
    }
    state.out.push(request);
    assert!(state.out.len() <= REQUESTS_MAX);
    None
}

/// Keeps `f` for later calls; past [`FUNCTIONS_MAX`] returns the error that fails the call.
pub(crate) fn keep(state: &Shared, f: Function) -> Result<FnId, String> {
    let mut state = state.borrow_mut();
    if state.functions.len() >= FUNCTIONS_MAX {
        return Err(format!("more than {FUNCTIONS_MAX} functions kept"));
    }
    state.functions.push(f);
    let id = u16::try_from(state.functions.len() - 1).expect("FUNCTIONS_MAX <= 2^16");
    assert!(usize::from(id) < FUNCTIONS_MAX);
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
fn sandbox(state: &Shared) -> omnilua::Result<(Lua, Sandbox)> {
    // No host hooks: no files, processes, environment or dynamic libraries.
    let lua = Lua::with_hooks_versioned(HostHooks::default(), LuaVersion::V54)?;
    lua.set_capture_tracebacks(true);
    let raw = crate::api::table(&lua, state)?;
    // The prelude keeps `debug.getinfo` private before the sandbox removes `debug`.
    let getinfo: Function = lua.globals().get::<_, LuaTable>("debug")?.get("getinfo")?;
    (lua.load(PRELUDE).set_name("=schemify").into_function()?).call::<_, ()>((raw, getinfo))?;
    let removed: Vec<Vec<u8>> = REMOVED.iter().map(|g| g.as_bytes().to_vec()).collect();
    let sandbox = lua.install_sandbox(SandboxConfig {
        instruction_limit: Some(BUDGET),
        memory_limit_bytes: Some(MEMORY_MAX),
        check_interval: CHECK_EVERY,
        remove_globals: removed,
    })?;
    // Negative space: what the config must never reach is gone, the API it needs is there.
    for global in REMOVED {
        let value: Value = lua.globals().raw_get(global)?;
        assert!(
            matches!(value, Value::Nil),
            "the sandbox left `{global}` visible"
        );
    }
    let api: Value = lua.globals().raw_get("schemify")?;
    assert!(
        matches!(api, Value::Table(_)),
        "the prelude sets `schemify`"
    );
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
        assert!(!what.is_empty());
        let sandbox = {
            let mut state = self.state.borrow_mut();
            assert!(
                state.out.is_empty(),
                "requests are drained after every call"
            );
            state.table = table;
            state.sandbox.clone().expect("armed for the state's life")
        };
        arm(&self.lua, &sandbox, BUDGET);
        let result = run(&self.lua);
        let mut state = self.state.borrow_mut();
        if let Err(e) = result {
            let message = match sandbox.tripped() {
                Some(TripReason::Instructions) => {
                    format!("instruction budget exhausted ({BUDGET} per call)")
                }
                Some(TripReason::Memory) => out_of_memory(),
                None => first_line(&e),
            };
            let located = message.contains(".lua:") || message.starts_with(what);
            let text = match (located, traceback_location(&e)) {
                (true, _) => message,
                (false, Some(at)) => format!("{at}: {message}"),
                (false, None) => format!("{what}: {message}"),
            };
            state.out.push(crate::error(text));
        }
        let out = mem::take(&mut state.out);
        let table = mem::take(&mut state.table);
        assert!(out.len() <= REQUESTS_MAX + 1);
        table.assert_invariants();
        // Handle bounds: every function the table names was kept by this state.
        let kept = state.functions.len();
        assert!(kept <= FUNCTIONS_MAX);
        for target in &table.keymaps.target {
            if let Target::Function(f) = target {
                assert!(usize::from(f.0) < kept, "keymap function {} not kept", f.0);
            }
        }
        for &(_, f) in &table.hooks {
            assert!(usize::from(f.0) < kept, "hook function {} not kept", f.0);
        }
        (table, out)
    }
}

/// Refills the instruction budget to `budget` and clears a trip.
fn arm(lua: &Lua, sandbox: &Sandbox, budget: u64) {
    assert!(budget > 0);
    assert!(budget <= BUDGET);
    sandbox.reset();
    let config = SandboxConfig {
        instruction_limit: Some(budget),
        memory_limit_bytes: Some(MEMORY_MAX),
        check_interval: CHECK_EVERY,
        remove_globals: Vec::new(),
    };
    (lua.install_sandbox(config)).expect("re-arming the sandbox only sets limits");
    assert!(
        sandbox.tripped().is_none(),
        "a fresh budget starts untripped"
    );
    assert_eq!(sandbox.instructions_remaining(), Some(budget));
}

fn runtime(message: String) -> LuaError {
    assert!(!message.is_empty());
    RawError::runtime(format_args!("{message}")).into()
}

fn out_of_memory() -> String {
    format!("not enough memory ({} MiB cap)", MEMORY_MAX >> 20)
}

/// `file:line` of the innermost config frame (`*.lua`) in an error's traceback: budget and
/// memory trips carry no location of their own.
fn traceback_location(e: &LuaError) -> Option<String> {
    let traceback = e.traceback_lossy()?;
    (traceback.lines().map(str::trim))
        .filter(|l| !l.starts_with("[C]") && !l.starts_with("stack traceback"))
        .filter_map(|l| l.split_once(": in ").map(|(at, _)| at.to_owned()))
        .find(|at| at.contains(".lua:"))
}

/// Calls `f(arguments…)` for [`pcall`]/[`xpcall`]: `Ok(true, results…)`, or `Err(message)`
/// for an error a protected call may catch; budget exhaustion propagates as the outer `Err`.
fn protected(
    lua: &Lua,
    state: &Shared,
    f: Value,
    arguments: Vec<Value>,
) -> omnilua::Result<Result<Variadic<Value>, String>> {
    let result = match f {
        Value::Function(f) => f.call::<_, Variadic<Value>>(Variadic::from(arguments)),
        other => Err(runtime(format!(
            "attempt to call a {} value",
            type_name(&other)
        ))),
    };
    let e = match result {
        Ok(results) => {
            let mut all = vec![Value::Boolean(true)];
            all.extend(results.into_vec());
            return Ok(Ok(Variadic::from(all)));
        }
        Err(e) => e,
    };
    // A memory trip is recoverable: re-arm with the instructions this call has left, so a
    // retry loop still ends.
    let sandbox = state
        .borrow()
        .sandbox
        .clone()
        .expect("armed while Lua runs");
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
