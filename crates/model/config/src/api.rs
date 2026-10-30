//! The `schemify.*` API (config spec §3) as the Rust functions under `prelude.lua`: Lua
//! arguments → rows of the [`Table`](crate::Table) in [`State`](crate::lua::State), or
//! requests. Each function validates its arguments and returns an error string instead of
//! raising; the prelude raises it at the caller's line (`error(msg, 2)`), the way C Lua's
//! `luaL_error` locates errors. Cites: config spec §2 (`require`), §3 (calls, options).
//!
//! Contents:
//! - `pub(crate) fn table(lua: &Lua, state: &Shared) -> omnilua::Result<LuaTable>`: the `raw`
//!   table the prelude wraps

use omnilua::{Function, Lua, Table as LuaTable, Value};

use crate::lua::{self, Shared};
use crate::table::{LINE_WIDTH_TENTHS_MAX, LINE_WIDTH_TENTHS_MIN, STEP_MAX, STEP_MIN};
use crate::{
    Event, HOOKS_MAX, KEYMAPS_MAX, Level, Request, SIM_ENV_MAX, Sim, SimKind, Target, USER_LAYERS,
};

/// An error string for the prelude to raise, or `None` on success.
type Raised = omnilua::Result<Option<String>>;

/// Builds the `raw` table: one Lua function per API entry point, each capturing `state`.
pub(crate) fn table(lua: &Lua, state: &Shared) -> omnilua::Result<LuaTable> {
    let t = lua.create_table()?;
    macro_rules! raw {
        ($name:literal, $f:expr) => {{
            let state = state.clone();
            let f = $f;
            t.raw_set($name, lua.create_function(move |lua, a| f(lua, &state, a))?)?;
        }};
    }
    raw!("get_opt", get_opt);
    raw!("set_opt", set_opt);
    raw!("keymap_set", keymap_set);
    raw!("cmd", cmd);
    raw!("on", on);
    raw!("sim", sim);
    raw!("notify", notify);
    raw!("pack_add", crate::pack::pack_add);
    raw!("find_module", find_module);
    raw!("pcall", lua::pcall);
    raw!("xpcall", lua::xpcall);
    Ok(t)
}

// ---- private ----

/// `schemify.opt.<key>`: the value in Lua units, or `nil` and the error.
fn get_opt(lua: &Lua, state: &Shared, key: Value) -> omnilua::Result<(Value, Option<String>)> {
    let options = state.borrow().table.opts;
    options.assert_in_domain();
    let step = |milli: i32| Value::Number(f64::from(milli) / 1000.0);
    let key = lua::string(&key).unwrap_or_default();
    let v = match key.as_str() {
        "snap" => step(options.snap),
        "grid" => step(options.grid),
        "show_grid" => Value::Boolean(options.show_grid),
        "light" => Value::Boolean(options.light),
        "dim" => Value::Number(f64::from(options.dim) / 100.0),
        "line_width" => Value::Number(f64::from(options.line_width_tenths) / 10.0),
        "simulator" => match options.simulator {
            Some(k) => Value::String(lua.create_string(k.word())?),
            None => Value::Nil,
        },
        // A copy: assigning to one of its entries changes nothing (spec §3).
        "layer_colors" => {
            let t = lua.create_table()?;
            for (layer, color) in options.layer_colors.iter().enumerate() {
                if let Some(rgb) = color {
                    let layer = i64::try_from(layer).expect("layer < USER_LAYERS");
                    t.raw_set(layer, format!("{rgb:06x}"))?;
                }
            }
            Value::Table(t)
        }
        _ => return Ok((Value::Nil, Some(format!("unknown option `{key}`")))),
    };
    Ok((v, None))
}

/// `schemify.opt.<key> = v`: converts to the table's units; a value outside the key's domain
/// sets nothing.
fn set_opt(_: &Lua, state: &Shared, (key, v): (Value, Value)) -> Raised {
    let step = |v: &Value| -> Option<i32> {
        let units = match *v {
            Value::Integer(i) => i as f64,
            Value::Number(n) => n,
            _ => return None,
        };
        let milli = (units * 1000.0).round();
        (f64::from(STEP_MIN)..=f64::from(STEP_MAX))
            .contains(&milli)
            .then_some(milli as i32)
    };
    let flag = |v: &Value| match *v {
        Value::Boolean(b) => Some(b),
        _ => None,
    };
    let dim = |v: &Value| {
        let fraction = number(v).filter(|f| (0.0..=1.0).contains(f))?;
        Some((fraction * 100.0).round() as u8)
    };
    let line_width = |v: &Value| {
        let pixels = number(v).filter(|w| (0.1..=10.0).contains(w))?;
        let tenths = (pixels * 10.0).round().clamp(
            f64::from(LINE_WIDTH_TENTHS_MIN),
            f64::from(LINE_WIDTH_TENTHS_MAX),
        );
        Some(tenths as u8)
    };
    // A Lua string only: `string` would also take a number.
    let simulator = |v: &Value| match v {
        Value::String(_) => {
            let word = lua::string(v);
            (SimKind::SIMULATORS.into_iter()).find(|k| word.as_deref() == Some(k.word()))
        }
        _ => None,
    };
    let key = lua::string(&key).unwrap_or_default();
    let mut state = state.borrow_mut();
    let options = &mut state.table.opts;
    let set = match key.as_str() {
        "snap" => assign(&mut options.snap, step(&v)),
        "grid" => assign(&mut options.grid, step(&v)),
        "show_grid" => assign(&mut options.show_grid, flag(&v)),
        "light" => assign(&mut options.light, flag(&v)),
        "dim" => assign(&mut options.dim, dim(&v)),
        "line_width" => assign(&mut options.line_width_tenths, line_width(&v)),
        "layer_colors" => assign(&mut options.layer_colors, layer_colors(&v)),
        "simulator" => assign(&mut options.simulator, simulator(&v).map(Some)),
        _ => return Ok(Some(format!("unknown option `{key}`"))),
    };
    options.assert_in_domain();
    if set {
        return Ok(None);
    }
    Ok(Some(format!(
        "option `{key}`: bad value ({})",
        lua::type_name(&v)
    )))
}

/// Writes `value` into `slot` when there is one; returns whether it did.
fn assign<T>(slot: &mut T, value: Option<T>) -> bool {
    let Some(value) = value else {
        return false;
    };
    *slot = value;
    true
}

/// A finite Lua number (integer or float).
fn number(v: &Value) -> Option<f64> {
    match *v {
        Value::Integer(i) => Some(i as f64),
        Value::Number(n) if n.is_finite() => Some(n),
        _ => None,
    }
}

/// `{ [layer] = "rrggbb" | "#rrggbb", … }` with layers in `[0, USER_LAYERS)`; `None` for any
/// other key or value, or more than [`USER_LAYERS`] entries.
fn layer_colors(v: &Value) -> Option<[Option<u32>; USER_LAYERS]> {
    let Value::Table(t) = v else {
        return None;
    };
    let mut out = [None; USER_LAYERS];
    let pairs = t.pairs().ok()?;
    for (n, pair) in pairs.enumerate() {
        if n >= USER_LAYERS {
            return None;
        }
        let (Value::Integer(layer), Value::String(color)) = pair.ok()? else {
            return None;
        };
        let layer = usize::try_from(layer).ok().filter(|&l| l < USER_LAYERS)?;
        let text = color.to_str().ok()?;
        let hex = text.strip_prefix('#').unwrap_or(&text);
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        out[layer] = Some(u32::from_str_radix(hex, 16).ok()?);
    }
    assert!(
        out.iter().flatten().all(|&rgb| rgb <= 0x00ff_ffff),
        "six hex digits"
    );
    Some(out)
}

/// `schemify.keymap.set(chord, target, opts)`: a later chord replaces its row in place.
fn keymap_set(_: &Lua, state: &Shared, (chord, target, opts): (Value, Value, Value)) -> Raised {
    let Some(chord) = lua::string(&chord) else {
        return Ok(Some(lua::bad_arg(1, "set", "string", &chord)));
    };
    if !lua::is_word(&chord, 64) {
        return Ok(Some(format!("bad chord `{chord}`")));
    }
    let target = match target {
        Value::String(s) => {
            let s = s.to_str()?;
            if !lua::is_word(&s, 128) {
                return Ok(Some(format!("bad command name `{s}`")));
            }
            Target::Command(s)
        }
        Value::Function(f) => match lua::keep(state, f) {
            Ok(id) => Target::Function(id),
            Err(e) => return Ok(Some(e)),
        },
        other => {
            return Ok(Some(format!(
                "keymap target must be a command name or a function, not {}",
                lua::type_name(&other)
            )));
        }
    };
    let description = match &opts {
        Value::Table(t) => lua::string(&lua::field(t, "desc")).unwrap_or_default(),
        _ => String::new(),
    };
    let mut state = state.borrow_mut();
    let keymaps = &mut state.table.keymaps;
    assert_eq!(keymaps.chord.len(), keymaps.target.len());
    assert_eq!(keymaps.chord.len(), keymaps.desc.len());
    if let Some(i) = keymaps.chord.iter().position(|c| *c == chord) {
        keymaps.target[i] = target;
        keymaps.desc[i] = description;
        return Ok(None);
    }
    if keymaps.chord.len() >= KEYMAPS_MAX {
        return Ok(Some(format!("more than {KEYMAPS_MAX} keymaps")));
    }
    keymaps.chord.push(chord);
    keymaps.target.push(target);
    keymaps.desc.push(description);
    assert_eq!(keymaps.chord.len(), keymaps.target.len());
    assert_eq!(keymaps.chord.len(), keymaps.desc.len());
    assert!(keymaps.chord.len() <= KEYMAPS_MAX);
    Ok(None)
}

/// `schemify.cmd(name, args)`: queues a command request.
fn cmd(_: &Lua, state: &Shared, (name, args): (Value, Value)) -> Raised {
    let Some(name) = lua::string(&name) else {
        return Ok(Some(lua::bad_arg(1, "cmd", "string", &name)));
    };
    if !lua::is_word(&name, 128) {
        return Ok(Some(format!("bad command name `{name}`")));
    }
    let args = match &args {
        Value::Nil => String::new(),
        v => match lua::string(v) {
            Some(s) => s,
            None => return Ok(Some(lua::bad_arg(2, "cmd", "string", v))),
        },
    };
    Ok(lua::push(state, Request::Command { name, args }))
}

/// `schemify.on(event, f)`: appends a hook row.
fn on(_: &Lua, state: &Shared, (event, f): (Value, Value)) -> Raised {
    let Some(event) = lua::string(&event) else {
        return Ok(Some(lua::bad_arg(1, "on", "string", &event)));
    };
    let Value::Function(f) = f else {
        return Ok(Some(lua::bad_arg(2, "on", "function", &f)));
    };
    let Some(event) = Event::from_word(&event) else {
        return Ok(Some(format!("unknown event `{event}`")));
    };
    if state.borrow().table.hooks.len() >= HOOKS_MAX {
        return Ok(Some(format!("more than {HOOKS_MAX} hooks")));
    }
    let id = match lua::keep(state, f) {
        Ok(id) => id,
        Err(e) => return Ok(Some(e)),
    };
    let mut state = state.borrow_mut();
    let hooks = &mut state.table.hooks;
    hooks.push((event, id));
    assert!(hooks.len() <= HOOKS_MAX);
    Ok(None)
}

/// `schemify.sim(name, { path, args, env })`: sets one program row; any bad field leaves the
/// row as it was.
fn sim(_: &Lua, state: &Shared, (name, spec): (Value, Value)) -> Raised {
    let Some(name) = lua::string(&name) else {
        return Ok(Some(lua::bad_arg(1, "sim", "string", &name)));
    };
    let Some(kind) = SimKind::ALL.into_iter().find(|k| k.word() == name) else {
        return Ok(Some(format!(
            "unknown program `{name}` (ngspice, espice, xyce, ltspice, spectre, openvaf, iverilog)"
        )));
    };
    let Value::Table(spec) = spec else {
        return Ok(Some(lua::bad_arg(2, "sim", "table", &spec)));
    };
    let Some(path) = lua::string(&lua::field(&spec, "path")).filter(|p| !p.is_empty()) else {
        return Ok(Some(format!("sim `{name}`: `path` is required")));
    };
    let args = match lua::field(&spec, "args") {
        Value::Nil => Some(Vec::new()),
        Value::Table(t) => lua::strings(&t),
        _ => None,
    };
    let Some(args) = args else {
        return Ok(Some(format!(
            "sim `{name}`: `args` must be a list of strings"
        )));
    };
    let env = match lua::field(&spec, "env") {
        Value::Nil => Some(Vec::new()),
        Value::Table(t) => env_pairs(&t),
        _ => None,
    };
    let Some(env) = env else {
        return Ok(Some(format!(
            "sim `{name}`: `env` must map at most {SIM_ENV_MAX} names to strings"
        )));
    };
    state.borrow_mut().table.sims[kind as usize] = Some(Sim { path, args, env });
    Ok(None)
}

/// `{ NAME = "value", … }` as pairs sorted by name, so the row does not depend on Lua's
/// iteration order; `None` for a key that is not a non-empty string without `=` or NUL, a
/// value that is not a string, or more than [`SIM_ENV_MAX`] entries.
fn env_pairs(t: &LuaTable) -> Option<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    let pairs = t.pairs().ok()?;
    for pair in pairs {
        let (k, v): (Value, Value) = pair.ok()?;
        let Value::String(_) = k else { return None };
        let k = lua::string(&k).filter(|k| !k.is_empty() && !k.contains(['=', '\0']))?;
        out.push((k, lua::string(&v)?));
        if out.len() > SIM_ENV_MAX {
            return None;
        }
    }
    out.sort_unstable();
    assert!(out.len() <= SIM_ENV_MAX);
    assert!(
        out.windows(2).all(|w| w[0].0 < w[1].0),
        "table keys are unique"
    );
    Some(out)
}

/// `schemify.notify(msg, level)`: queues a notification.
fn notify(_: &Lua, state: &Shared, (message, level): (Value, Value)) -> Raised {
    let Some(text) = lua::string(&message) else {
        return Ok(Some(lua::bad_arg(1, "notify", "string", &message)));
    };
    let level = match lua::string(&level).as_deref() {
        None | Some("info") => Level::Info,
        Some("warn") => Level::Warn,
        Some("error") => Level::Error,
        Some(other) => return Ok(Some(format!("unknown level `{other}`"))),
    };
    Ok(lua::push(state, Request::Notify { level, text }))
}

/// `require "a.b"`'s file: `<root>/lua/a/b.lua` or `<root>/lua/a/b/init.lua`, compiled (the
/// prelude runs it once and caches the result). Returns the chunk, or `nil` and the error.
fn find_module(lua: &Lua, state: &Shared, name: Value) -> omnilua::Result<(Value, Option<String>)> {
    let name = lua::string(&name).unwrap_or_default();
    let segment = |segment: &str| {
        !segment.is_empty()
            && (segment.bytes()).all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    };
    if name.is_empty() || !name.split('.').all(segment) {
        return Ok((Value::Nil, Some(format!("bad module name `{name}`"))));
    }
    let (root, read) = {
        let state = state.borrow();
        (state.root.clone(), state.read)
    };
    let Some(root) = root else {
        let message = format!("module `{name}`: no config directory");
        return Ok((Value::Nil, Some(message)));
    };
    let relative = name.replace('.', "/");
    // Segments of `A–Z a–z 0–9 _ -` keep a module under `<root>/lua`.
    assert!(
        !relative.contains(".."),
        "`{relative}` climbs out of <root>/lua"
    );
    assert!(!relative.starts_with('/'), "`{relative}` is absolute");
    let candidates = [
        format!("lua/{relative}.lua"),
        format!("lua/{relative}/init.lua"),
    ];
    let found = (candidates.iter()).find_map(|c| read(&root.join(c)).map(|source| (c, source)));
    let Some((file, source)) = found else {
        let message = format!("module `{name}` not found under {}/lua", root.display());
        return Ok((Value::Nil, Some(message)));
    };
    let chunk: omnilua::Result<Function> = lua::text_chunk(lua, &source, &format!("@{file}"));
    Ok(match chunk {
        Ok(f) => (Value::Function(f), None),
        Err(e) => (Value::Nil, Some(lua::first_line(&e))),
    })
}
