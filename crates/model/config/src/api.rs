//! The `schemify.*` API (config spec §3) as the Rust functions under `prelude.lua`: Lua
//! arguments → rows of the [`Table`](crate::Table) in [`St`](crate::lua::St), or requests.
//! Each function validates its arguments and returns an error string instead of raising;
//! the prelude raises it at the caller's line (`error(msg, 2)`), the way C Lua's
//! `luaL_error` locates errors. Cites: config spec §2 (`require`), §3 (calls, options).
//!
//! Contents:
//! - `pub(crate) fn table(lua: &Lua, st: &Shared) -> omnilua::Result<LTable>`: the `raw`
//!   table the prelude wraps

use omnilua::{Function, Lua, Table as LTable, Value};

use crate::lua::{
    Shared, bad_arg, field, first_line, is_word, keep, push, string, strings, text_chunk, type_name,
};
use crate::{
    Event, HOOKS_MAX, KEYMAPS_MAX, Level, Request, SIM_ENV_MAX, Sim, SimKind, Target, USER_LAYERS,
};

/// An error string for the prelude to raise, or `None` on success.
type Raised = omnilua::Result<Option<String>>;

/// Builds the `raw` table: one Lua function per API entry point, each capturing `st`.
pub(crate) fn table(lua: &Lua, st: &Shared) -> omnilua::Result<LTable> {
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
    raw!("pack_add", crate::pack::pack_add);
    raw!("find_module", find_module);
    raw!("pcall", crate::lua::pcall);
    raw!("xpcall", crate::lua::xpcall);
    Ok(t)
}

// ---- private ----

/// `schemify.opt.<key>`: the value in Lua units, or `nil` and the error.
fn get_opt(lua: &Lua, st: &Shared, key: Value) -> omnilua::Result<(Value, Option<String>)> {
    let o = st.borrow().table.opts;
    let step = |milli: i32| Value::Number(f64::from(milli) / 1000.0);
    let key = string(&key).unwrap_or_default();
    let v = match key.as_str() {
        "snap" => step(o.snap),
        "grid" => step(o.grid),
        "show_grid" => Value::Boolean(o.show_grid),
        "light" => Value::Boolean(o.light),
        "dim" => Value::Number(f64::from(o.dim) / 100.0),
        "line_width" => Value::Number(f64::from(o.line_width_tenths) / 10.0),
        "simulator" => match o.simulator {
            Some(k) => Value::String(lua.create_string(k.word())?),
            None => Value::Nil,
        },
        // A copy: assigning to one of its entries changes nothing (spec §3).
        "layer_colors" => {
            let t = lua.create_table()?;
            for (layer, c) in o.layer_colors.iter().enumerate() {
                if let Some(rgb) = c {
                    t.raw_set(layer as i64, format!("{rgb:06x}"))?;
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
fn set_opt(_: &Lua, st: &Shared, (key, v): (Value, Value)) -> Raised {
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
        "dim" => (number(&v).filter(|f| (0.0..=1.0).contains(f)))
            .map(|f| o.dim = (f * 100.0).round() as u8),
        "line_width" => number(&v).filter(|w| (0.1..=10.0).contains(w)).map(|w| {
            o.line_width_tenths = (w * 10.0).round().clamp(1.0, 100.0) as u8;
        }),
        "layer_colors" => layer_colors(&v).map(|c| o.layer_colors = c),
        // A Lua string only: `string` would also take a number.
        "simulator" => match &v {
            Value::String(_) => (SimKind::SIMULATORS.into_iter())
                .find(|k| string(&v).as_deref() == Some(k.word()))
                .map(|k| o.simulator = Some(k)),
            _ => None,
        },
        _ => return Ok(Some(format!("unknown option `{key}`"))),
    };
    assert!(o.dim <= 100);
    assert!((1..=100).contains(&o.line_width_tenths));
    Ok(match set {
        Some(()) => None,
        None => Some(format!("option `{key}`: bad value ({})", type_name(&v))),
    })
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
    for (n, pair) in t.pairs().ok()?.enumerate() {
        if n >= USER_LAYERS {
            return None;
        }
        let (Value::Integer(layer), Value::String(c)) = pair.ok()? else {
            return None;
        };
        let layer = usize::try_from(layer).ok().filter(|&l| l < USER_LAYERS)?;
        let text = c.to_str().ok()?;
        let hex = text.strip_prefix('#').unwrap_or(&text);
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        out[layer] = Some(u32::from_str_radix(hex, 16).ok()?);
    }
    assert!(out.iter().flatten().all(|&rgb| rgb <= 0x00ff_ffff));
    Some(out)
}

/// `schemify.keymap.set(chord, target, opts)`: a later chord replaces its row in place.
fn keymap_set(_: &Lua, st: &Shared, (chord, target, opts): (Value, Value, Value)) -> Raised {
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
    assert_eq!(k.chord.len(), k.target.len());
    assert_eq!(k.chord.len(), k.desc.len());
    Ok(None)
}

/// `schemify.cmd(name, args)`: queues a command request.
fn cmd(_: &Lua, st: &Shared, (name, args): (Value, Value)) -> Raised {
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

/// `schemify.on(event, f)`: appends a hook row.
fn on(_: &Lua, st: &Shared, (event, f): (Value, Value)) -> Raised {
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
    let id = match keep(st, f) {
        Ok(id) => id,
        Err(e) => return Ok(Some(e)),
    };
    let mut s = st.borrow_mut();
    let hooks = &mut s.table.hooks;
    hooks.push((ev, id));
    assert!(hooks.len() <= HOOKS_MAX);
    Ok(None)
}

/// `schemify.sim(name, { path, args, env })`: sets one program row; any bad field leaves the
/// row as it was.
fn sim(_: &Lua, st: &Shared, (name, spec): (Value, Value)) -> Raised {
    let Some(name) = string(&name) else {
        return Ok(Some(bad_arg(1, "sim", "string", &name)));
    };
    let Some(kind) = SimKind::ALL.into_iter().find(|k| k.word() == name) else {
        return Ok(Some(format!(
            "unknown program `{name}` (ngspice, espice, xyce, ltspice, spectre, openvaf, iverilog)"
        )));
    };
    let Value::Table(spec) = spec else {
        return Ok(Some(bad_arg(2, "sim", "table", &spec)));
    };
    let Some(path) = string(&field(&spec, "path")).filter(|p| !p.is_empty()) else {
        return Ok(Some(format!("sim `{name}`: `path` is required")));
    };
    let args = match field(&spec, "args") {
        Value::Nil => Some(Vec::new()),
        Value::Table(t) => strings(&t),
        _ => None,
    };
    let Some(args) = args else {
        return Ok(Some(format!(
            "sim `{name}`: `args` must be a list of strings"
        )));
    };
    let env = match field(&spec, "env") {
        Value::Nil => Some(Vec::new()),
        Value::Table(t) => env_pairs(&t),
        _ => None,
    };
    let Some(env) = env else {
        return Ok(Some(format!(
            "sim `{name}`: `env` must map at most {SIM_ENV_MAX} names to strings"
        )));
    };
    st.borrow_mut().table.sims[kind as usize] = Some(Sim { path, args, env });
    Ok(None)
}

/// `{ NAME = "value", … }` as pairs sorted by name, so the row does not depend on Lua's
/// iteration order; `None` for a key that is not a non-empty string without `=` or NUL, a
/// value that is not a string, or more than [`SIM_ENV_MAX`] entries.
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

/// `schemify.notify(msg, level)`: queues a notification.
fn notify(_: &Lua, st: &Shared, (msg, level): (Value, Value)) -> Raised {
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

/// `require "a.b"`'s file: `<root>/lua/a/b.lua` or `<root>/lua/a/b/init.lua`, compiled (the
/// prelude runs it once and caches the result). Returns the chunk, or `nil` and the error.
fn find_module(lua: &Lua, st: &Shared, name: Value) -> omnilua::Result<(Value, Option<String>)> {
    let name = string(&name).unwrap_or_default();
    let segment = |seg: &str| {
        !seg.is_empty()
            && (seg.bytes()).all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    };
    if name.is_empty() || !name.split('.').all(segment) {
        return Ok((Value::Nil, Some(format!("bad module name `{name}`"))));
    }
    let (root, read) = {
        let s = st.borrow();
        (s.root.clone(), s.read)
    };
    let Some(root) = root else {
        let msg = format!("module `{name}`: no config directory");
        return Ok((Value::Nil, Some(msg)));
    };
    let rel = name.replace('.', "/");
    let candidates = [format!("lua/{rel}.lua"), format!("lua/{rel}/init.lua")];
    let found = (candidates.iter()).find_map(|c| read(&root.join(c)).map(|src| (c, src)));
    let Some((file, src)) = found else {
        let msg = format!("module `{name}` not found under {}/lua", root.display());
        return Ok((Value::Nil, Some(msg)));
    };
    let chunk: omnilua::Result<Function> = text_chunk(lua, &src, &format!("@{file}"));
    Ok(match chunk {
        Ok(f) => (Value::Function(f), None),
        Err(e) => (Value::Nil, Some(first_line(&e))),
    })
}
