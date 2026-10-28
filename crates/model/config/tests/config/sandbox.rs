//! The sandbox (config spec §4): absent libraries, the instruction budget, the memory cap,
//! and errors located at `file:line` with earlier settings kept (spec §2).
//!
//! Contents: no `pub` items.

use std::time::{Duration, Instant};

use schemify_config::Table;

use crate::{errors, run};

// ---- private ----

#[test]
fn sandbox_has_no_io_os_debug_or_loaders() {
    let absent = [
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
    for name in absent {
        let (_, out) = run(&format!("assert({name} == nil, '{name} is present')"));
        assert!(out.is_empty(), "{name}: {out:?}");
    }
    let (_, out) = run("os.execute('touch /tmp/schemify-sandbox-escape')");
    let e = errors(&out);
    assert!(
        e[0].starts_with("init.lua:1:") && e[0].contains("os"),
        "{e:?}"
    );
    assert!(!std::path::Path::new("/tmp/schemify-sandbox-escape").exists());
    let (_, out) = run("package.loadlib('libc.so.6', 'system')");
    assert!(errors(&out)[0].contains("package"), "{out:?}");
    let (_, out) = run("require('os')");
    assert!(errors(&out)[0].contains("no config directory"), "{out:?}");
    // Precompiled bytecode (which can crash the VM) never loads, not even as the init file.
    let (_, out) = run("\x1bLuaT\0");
    assert!(errors(&out)[0].contains("binary chunk"), "{out:?}");
}

/// The ceiling C Lua had (spec §4, BACKLOG): one library call ran between budget checks. The
/// pattern matcher now charges the budget as it backtracks, so a pathological match stops.
#[test]
fn a_pathological_pattern_is_killed_by_the_budget() {
    let t = Instant::now();
    let (_, out) = run("local s = string.rep('a', 4000)\nstring.find(s, '.-.-.-.-.-.-b')");
    assert!(
        t.elapsed() < Duration::from_secs(20),
        "took {:?}",
        t.elapsed()
    );
    let e = errors(&out);
    assert_eq!(e.len(), 1, "{out:?}");
    assert!(e[0].contains("instruction budget exhausted"), "{e:?}");
    // Negative space: a cheap match in the same state runs.
    let (_, out) = run("assert(string.find(string.rep('a', 4000) .. 'b', 'a-b') == 1)");
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn an_infinite_loop_is_killed_by_the_budget() {
    for src in [
        "local x = 0\nwhile true do x = x + 1 end",
        // pcall cannot swallow the budget error and keep looping.
        "while true do pcall(function() while true do end end) end",
        "while true do xpcall(function() while true do end end, function(m) return m end) end",
    ] {
        let t = Instant::now();
        let (_, out) = run(src);
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "{src}: took {:?}",
            t.elapsed()
        );
        let e = errors(&out);
        assert_eq!(e.len(), 1, "{src}: {out:?}");
        assert!(e[0].contains("instruction budget exhausted"), "{e:?}");
        assert!(e[0].starts_with("init.lua:"), "located: {e:?}");
    }
}

#[test]
fn a_memory_bomb_is_killed_by_the_cap() {
    for src in [
        "local s = string.rep('x', 1 << 30)",
        "local t = {}\nfor i = 1, 1e9 do t[i] = string.rep('x', 4096) .. i end",
    ] {
        let (cfg, out) = run(src);
        let e = errors(&out);
        assert_eq!(e.len(), 1, "{src}: {out:?}");
        assert!(e[0].contains("not enough memory"), "{e:?}");
        assert_eq!(cfg.table(), &Table::default());
    }
    // pcall may catch it (the state stays usable), but the budget still bounds a retry loop.
    let (_, out) = run("for i = 1, 3 do assert(not pcall(string.rep, 'x', 1 << 30)) end");
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn errors_are_located_and_earlier_settings_stay() {
    let (cfg, out) =
        run("schemify.opt.snap = 5\n\nschemify.opt.bogus = 1\nschemify.opt.light = true");
    assert_eq!(errors(&out), vec!["init.lua:3: unknown option `bogus`"]);
    assert_eq!(cfg.table().opts.snap, 5_000, "set before the error");
    assert!(!cfg.table().opts.light, "after the error: not run");
    let (_, out) = run("\nerror('boom')");
    assert_eq!(errors(&out), vec!["init.lua:2: boom"]);
    let (_, out) = run("schemify.opt.snap = 'wide'");
    assert!(
        errors(&out)[0].starts_with("init.lua:1: option `snap`"),
        "{out:?}"
    );
    let (_, out) = run("schemify.opt.snap = 0");
    assert!(errors(&out)[0].contains("bad value"), "{out:?}");
    let (_, out) = run("local x = = 1");
    assert!(errors(&out)[0].starts_with("init.lua:1:"), "{out:?}");
    let (_, out) = run("schemify.on('bogus_event', function() end)");
    assert!(errors(&out)[0].contains("unknown event"), "{out:?}");
    let (_, out) = run("schemify.sim('hspice', { path = 'x' })");
    assert!(errors(&out)[0].contains("unknown program"), "{out:?}");
    let (_, out) = run("schemify.keymap.set('Ctrl+X', 42)");
    assert!(errors(&out)[0].starts_with("init.lua:1:"), "{out:?}");
    // Argument conversion errors are located through the traceback.
    let (_, out) = run("\nschemify.on('saved', 5)");
    assert!(errors(&out)[0].starts_with("init.lua:2:"), "{out:?}");
}
