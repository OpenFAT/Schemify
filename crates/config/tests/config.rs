//! Oracles for the user config (TESTING.md, config spec §2–§5): the Lua API against the table
//! written directly, and the sandbox's negative space (absent libraries, budget, memory cap,
//! errors located at `file:line`).
//!
//! The same interpreter runs natively and on the web (web spec §5), so CI also runs this file
//! for `wasm32-wasip1` under wasmtime. The browser target has no test harness.

#![cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use schemify_config::{
    Config, Event, FnId, Keymaps, Level, Opts, PackSpec, Request, Sim, Source, Startup, Table,
    Target,
};

fn run(src: &str) -> (Config, Vec<Request>) {
    Config::from_source(src, None)
}

fn errors(out: &[Request]) -> Vec<&str> {
    out.iter()
        .filter_map(|r| match r {
            Request::Notify {
                level: Level::Error,
                text,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

const SHA: &str = "9f2c000000000000000000000000000000000000000000000000000000000001";

/// Setting options, maps, sims, hooks and packages through Lua equals writing the table.
#[test]
fn lua_api_equals_the_table_written_directly() {
    let src = format!(
        r#"
schemify.opt.snap = 5
schemify.opt.grid = 25.5
schemify.opt.show_grid = false
schemify.opt.light = true
schemify.keymap.set("Ctrl+S", "file.save", {{ desc = "save" }})
schemify.keymap.set("F5", function() schemify.cmd("simulation.simulate") end)
schemify.keymap.set("Ctrl+S", "file.save_as")
schemify.sim("ngspice", {{ path = "/opt/ng/bin/ngspice", args = {{ "-b", "-r", "{{raw}}", "{{netlist}}" }} }})
schemify.sim("openvaf", {{ path = "openvaf-r" }})
schemify.on("saved", function(ev) end)
schemify.pack.add {{
  {{ src = "https://example.org/u/gmid-lut.git", version = "v1.2.0", grant = {{ "read_project", "spawn:ngspice" }} }},
  {{ url = "https://example.org/eye.wasm", sha256 = "{SHA}", lazy = {{ cmd = "eye.open", event = {{ "doc_opened" }} }} }},
  {{ path = "/dev/plug", name = "dev", fuel = 2000000000 }},
  "https://example.org/u/short.git",
}}
"#
    );
    let (cfg, out) = run(&src);
    assert!(out.is_empty(), "{out:?}");
    let mut sims: [Option<Sim>; 3] = [None, None, None];
    sims[0] = Some(Sim {
        path: "/opt/ng/bin/ngspice".into(),
        args: ["-b", "-r", "{raw}", "{netlist}"]
            .map(String::from)
            .to_vec(),
    });
    sims[1] = Some(Sim {
        path: "openvaf-r".into(),
        args: Vec::new(),
    });
    let spec = |name: &str, source: Source| PackSpec {
        name: name.into(),
        source,
        grant: Vec::new(),
        fuel: None,
        lazy_cmd: Vec::new(),
        lazy_event: Vec::new(),
    };
    let want = Table {
        opts: Opts {
            snap: 5_000,
            grid: 25_500,
            show_grid: false,
            light: true,
            ..Opts::default()
        },
        keymaps: Keymaps {
            chord: vec!["Ctrl+S".into(), "F5".into()],
            target: vec![
                Target::Command("file.save_as".into()),
                Target::Function(FnId(0)),
            ],
            desc: vec![String::new(), String::new()],
        },
        sims,
        packs: vec![
            PackSpec {
                grant: vec!["read_project".into(), "spawn:ngspice".into()],
                ..spec(
                    "gmid-lut",
                    Source::Git {
                        url: "https://example.org/u/gmid-lut.git".into(),
                        rev: Some("v1.2.0".into()),
                    },
                )
            },
            PackSpec {
                lazy_cmd: vec!["eye.open".into()],
                lazy_event: vec![Event::DocOpened],
                ..spec(
                    "eye",
                    Source::Url {
                        url: "https://example.org/eye.wasm".into(),
                        sha256: SHA.into(),
                    },
                )
            },
            PackSpec {
                fuel: Some(2_000_000_000),
                ..spec("dev", Source::Path(PathBuf::from("/dev/plug")))
            },
            spec(
                "short",
                Source::Git {
                    url: "https://example.org/u/short.git".into(),
                    rev: None,
                },
            ),
        ],
        hooks: vec![(Event::Saved, FnId(1))],
    };
    assert_eq!(cfg.table(), &want);
    assert_eq!(
        cfg.table().keymaps.records(),
        "key Ctrl+S file.save_as\nkey F5 user.lua.0\n"
    );
}

#[test]
fn options_read_back_and_defaults_hold() {
    let (cfg, out) = run(
        "assert(schemify.opt.snap == 10 and schemify.opt.light == false)\n\
         schemify.opt.grid = 40\nassert(schemify.opt.grid == 40)",
    );
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(cfg.table().opts.grid, 40_000);
    assert_eq!(cfg.table().opts.snap, Opts::default().snap);
    // Clean: nothing runs, nothing is set.
    let (clean, out) = Config::load(&Startup::Clean);
    assert!(out.is_empty() && clean.root().is_none());
    assert_eq!(clean.table(), &Table::default());
}

/// The canvas options (`layer_colors`, `dim`, `line_width`) read back what was set, in the
/// table's units; every value outside its domain is an error that sets nothing.
#[test]
fn canvas_options_set_read_back_and_reject_bad_values() {
    let (cfg, out) = run(concat!(
        "schemify.opt.layer_colors = { [4] = 'ff8000', [1] = '#00CCFF', [21] = '000001' }\n",
        "schemify.opt.dim = 0.25\n",
        "schemify.opt.line_width = 1.5\n",
        "local c = schemify.opt.layer_colors\n",
        "assert(c[4] == 'ff8000' and c[1] == '00ccff' and c[21] == '000001' and c[0] == nil)\n",
        "assert(schemify.opt.dim == 0.25 and schemify.opt.line_width == 1.5)\n",
        "c[5] = '123456' -- a copy: sets nothing\n",
    ));
    assert!(out.is_empty(), "{out:?}");
    let o = cfg.table().opts;
    let mut want = [None; schemify_config::USER_LAYERS];
    want[1] = Some(0x0000_ccff);
    want[4] = Some(0x00ff_8000);
    want[21] = Some(0x0000_0001);
    assert_eq!(o.layer_colors, want);
    assert_eq!((o.dim, o.line_width_tenths), (25, 15));
    // Defaults: no colour, no dimming, one pixel.
    let d = Opts::default();
    assert_eq!(
        (d.layer_colors, d.dim, d.line_width_tenths),
        ([None; schemify_config::USER_LAYERS], 0, 10)
    );
    // Negative space: each bad value is one located error, and the options keep their
    // defaults.
    for bad in [
        "schemify.opt.layer_colors = { [22] = 'ff0000' }",
        "schemify.opt.layer_colors = { [-1] = 'ff0000' }",
        "schemify.opt.layer_colors = { [3] = 'red' }",
        "schemify.opt.layer_colors = { [3] = 'ff00000' }",
        "schemify.opt.layer_colors = { x = 'ff0000' }",
        "schemify.opt.layer_colors = 'ff0000'",
        "schemify.opt.dim = 1.5",
        "schemify.opt.dim = -0.1",
        "schemify.opt.dim = 'half'",
        "schemify.opt.line_width = 0",
        "schemify.opt.line_width = 11",
    ] {
        let (cfg, out) = run(bad);
        let e = errors(&out);
        assert_eq!(e.len(), 1, "{bad}: {out:?}");
        assert!(e[0].contains("bad value"), "{bad}: {}", e[0]);
        assert_eq!(cfg.table().opts, Opts::default(), "{bad}");
    }
}

#[test]
fn functions_and_hooks_return_their_requests() {
    let (mut cfg, out) = run(
        "schemify.keymap.set('F5', function() schemify.cmd('simulation.simulate', 'fast') end)\n\
         schemify.on('doc_opened', function(ev) schemify.notify('doc ' .. ev.doc, 'warn') end)\n\
         schemify.on('doc_opened', function(ev) print('second', ev.event) end)\n\
         schemify.on('saved', function() while true do end end)",
    );
    assert!(out.is_empty(), "{out:?}");
    let Target::Function(f) = cfg.table().keymaps.target[0] else {
        panic!("a function keymap");
    };
    assert_eq!(
        cfg.call(f),
        vec![Request::Command {
            name: "simulation.simulate".into(),
            args: "fast".into()
        }]
    );
    assert_eq!(
        cfg.fire(Event::DocOpened, &[("doc", 7)]),
        vec![
            Request::Notify {
                level: Level::Warn,
                text: "doc 7".into()
            },
            Request::Notify {
                level: Level::Info,
                text: "second\tdoc_opened".into()
            },
        ]
    );
    // A runaway hook is stopped and reported; the config keeps working afterwards.
    let out = cfg.fire(Event::Saved, &[("doc", 1)]);
    assert!(
        errors(&out)[0].contains("instruction budget exhausted"),
        "{out:?}"
    );
    assert_eq!(cfg.call(f).len(), 1);
    assert!(
        cfg.fire(Event::RunDone, &[]).is_empty(),
        "no hook, no requests"
    );
}

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
    let (_, out) = run("schemify.sim('spectre', { path = 'x' })");
    assert!(errors(&out)[0].contains("unknown program"), "{out:?}");
    let (_, out) = run("schemify.keymap.set('Ctrl+X', 42)");
    assert!(errors(&out)[0].starts_with("init.lua:1:"), "{out:?}");
    // Argument conversion errors are located through the traceback.
    let (_, out) = run("\nschemify.on('saved', 5)");
    assert!(errors(&out)[0].starts_with("init.lua:2:"), "{out:?}");
}

#[test]
fn bad_pack_specs_are_reported_and_dropped() {
    let (cfg, out) = run(&format!(
        "schemify.pack.add {{\n\
           {{ name = 'nosource' }},\n\
           {{ url = 'https://x/a.wasm', sha256 = 'abc' }},\n\
           {{ src = 'https://x/b.git', rev = 'r', version = 'v' }},\n\
           {{ path = '/p', rev = 'main' }},\n\
           {{ src = 'https://x/c.git', colour = 'red' }},\n\
           {{ src = 'https://x/d.git', fuel = 5 }},\n\
           {{ src = 'https://x/e.git', lazy = {{ event = 'never' }} }},\n\
           {{ url = 'https://x/ok.wasm', sha256 = '{SHA}' }},\n\
         }}"
    ));
    let e = errors(&out);
    assert_eq!(e.len(), 7, "{e:?}");
    assert!(
        e.iter().all(|m| m.starts_with("init.lua:1: pack spec:")),
        "{e:?}"
    );
    assert_eq!(cfg.table().packs.len(), 1);
    assert_eq!(cfg.table().packs[0].name, "ok");
}

#[test]
fn require_reads_config_modules_once() {
    // WASI has no process ids: a clock stamp keeps parallel runs apart there.
    #[cfg(not(target_os = "wasi"))]
    let tag = std::process::id();
    #[cfg(target_os = "wasi")]
    let tag = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let root = std::env::temp_dir().join(format!("schemify-config-req-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("lua/keys")).unwrap();
    std::fs::write(
        root.join("lua/keys/init.lua"),
        "count = (count or 0) + 1\nreturn { chord = 'F6' }",
    )
    .unwrap();
    std::fs::write(root.join("lua/loop.lua"), "require('loop')").unwrap();
    let (cfg, out) = Config::from_source(
        "local k = require('keys')\nlocal again = require('keys')\n\
         assert(k == again and count == 1)\n\
         schemify.keymap.set(k.chord, 'file.save')",
        Some(&root),
    );
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(cfg.table().keymaps.chord, vec!["F6".to_owned()]);
    // Local package paths resolve against the config root.
    let (cfg, out) = Config::from_source(
        "schemify.pack.add { { src = '../repo', name = 'r' }, { path = 'plug' } }",
        Some(&root),
    );
    assert!(out.is_empty(), "{out:?}");
    let Source::Git { url, .. } = &cfg.table().packs[0].source else {
        panic!("a git source");
    };
    assert_eq!(PathBuf::from(url), root.join("../repo"));
    assert_eq!(cfg.table().packs[1].source, Source::Path(root.join("plug")));
    for (name, why) in [
        ("../etc/passwd", "bad module name"),
        ("/abs", "bad module name"),
        ("missing", "not found"),
        ("loop", "loop requiring"),
    ] {
        let (_, out) = Config::from_source(&format!("require('{name}')"), Some(&root));
        assert!(
            errors(&out).iter().any(|m| m.contains(why)),
            "{name}: {out:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The documented example config runs without an error.
#[test]
fn the_example_config_is_valid() {
    let (cfg, out) = run(include_str!("../../../docs/examples/init.lua"));
    assert!(errors(&out).is_empty(), "{out:?}");
    assert_eq!(cfg.table().packs.len(), 2);
    assert_eq!(cfg.table().keymaps.chord.len(), 2);
}

#[test]
fn startup_flags_parse() {
    let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        Startup::parse(&args(&["netlist", "x"])),
        Ok((Startup::User, 0))
    );
    assert_eq!(
        Startup::parse(&args(&["--clean", "fmt"])),
        Ok((Startup::Clean, 1))
    );
    assert_eq!(
        Startup::parse(&args(&["-u", "t.lua", "pack", "sync"])),
        Ok((Startup::File("t.lua".into()), 2))
    );
    assert!(Startup::parse(&args(&["-u"])).is_err());
    assert!(Startup::parse(&args(&["--clean", "-u", "x"])).is_err());
    for s in [Startup::Clean, Startup::User, Startup::File("a.lua".into())] {
        let mut flags = s.flags();
        flags.push("edit".into());
        assert_eq!(Startup::parse(&flags), Ok((s.clone(), flags.len() - 1)));
    }
}
