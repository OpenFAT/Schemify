//! The `schemify.*` API (config spec §3, §5): Lua source → the table written directly, the
//! requests of functions and hooks, and each call's negative space.
//!
//! Contents: no `pub` items.

use std::fs;
use std::path::PathBuf;

use schemify_config::{
    Config, Event, FnId, HOOKS_MAX, Hook, Keymaps, Level, Options, PackSpec, REQUESTS_MAX, Request,
    Sim, SimKind, Source, Startup, Table, Target, USER_LAYERS,
};

use crate::{SHA, errors, run};

// ---- private ----

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
schemify.sim("espice", {{ path = "/e/espice", env = {{ ZIG = "/z/zig", A = "1" }} }})
schemify.opt.simulator = "espice"
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
    let mut sims: [Option<Sim>; 7] = Default::default();
    sims[0] = Some(Sim {
        path: "/opt/ng/bin/ngspice".into(),
        args: ["-b", "-r", "{raw}", "{netlist}"]
            .map(String::from)
            .to_vec(),
        env: Vec::new(),
    });
    sims[1] = Some(Sim {
        path: "openvaf-r".into(),
        args: Vec::new(),
        env: Vec::new(),
    });
    // `env` sorted by name whatever Lua's table order.
    sims[3] = Some(Sim {
        path: "/e/espice".into(),
        args: Vec::new(),
        env: vec![("A".into(), "1".into()), ("ZIG".into(), "/z/zig".into())],
    });
    let spec = |name: &str, source: Source| PackSpec {
        name: name.into(),
        source,
        grant: Vec::new(),
        fuel: None,
        lazy_command: Vec::new(),
        lazy_event: Vec::new(),
    };
    let want = Table {
        opts: Options {
            snap: 5_000,
            grid: 25_500,
            show_grid: false,
            light: true,
            simulator: Some(SimKind::Espice),
            ..Options::default()
        },
        keymaps: Keymaps {
            chord: vec!["Ctrl+S".into(), "F5".into()],
            target: vec![
                Target::Command("file.save_as".into()),
                Target::Function(FnId(0)),
            ],
            description: vec![String::new(), String::new()],
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
                lazy_command: vec!["eye.open".into()],
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
        hooks: vec![Hook {
            event: Event::Saved,
            function: FnId(1),
        }],
    };
    assert_eq!(cfg.table(), &want);
    let lazy: Vec<bool> = cfg.table().packs.iter().map(PackSpec::is_lazy).collect();
    assert_eq!(lazy, [false, true, false, false], "only `eye` waits");
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
    assert_eq!(cfg.table().opts.snap, Options::default().snap);
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
        "assert(schemify.opt.simulator == nil)\n",
        "c[5] = '123456' -- a copy: sets nothing\n",
    ));
    assert!(out.is_empty(), "{out:?}");
    let o = cfg.table().opts;
    let mut want = [None; USER_LAYERS];
    want[1] = Some(0x0000_ccff);
    want[4] = Some(0x00ff_8000);
    want[21] = Some(0x0000_0001);
    assert_eq!(o.layer_colors, want);
    assert_eq!((o.dim, o.line_width_tenths), (25, 15));
    // Defaults: no colour, no dimming, one pixel.
    let d = Options::default();
    assert_eq!(
        (d.layer_colors, d.dim, d.line_width_tenths),
        ([None; USER_LAYERS], 0, 10)
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
        "schemify.opt.simulator = 'openvaf'",
        "schemify.opt.simulator = 'hspice'",
        "schemify.opt.simulator = true",
    ] {
        let (cfg, out) = run(bad);
        let e = errors(&out);
        assert_eq!(e.len(), 1, "{bad}: {out:?}");
        assert!(e[0].contains("bad value"), "{bad}: {}", e[0]);
        assert_eq!(cfg.table().opts, Options::default(), "{bad}");
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

/// `schemify.sim` `env` (ESPice's `ZIG`): a map of names to strings, else one located error
/// that leaves the row unset.
#[test]
fn sim_env_must_map_names_to_strings() {
    let (cfg, out) = run("schemify.sim('espice', { path = 'espice', env = { ZIG = '/z' } })");
    assert!(out.is_empty(), "{out:?}");
    let row = cfg.table().sim(SimKind::Espice).unwrap();
    assert_eq!(row.env, vec![("ZIG".to_owned(), "/z".to_owned())]);
    for bad in [
        "schemify.sim('espice', { path = 'espice', env = 'ZIG=/z' })",
        "schemify.sim('espice', { path = 'espice', env = { ZIG = true } })",
        "schemify.sim('espice', { path = 'espice', env = { '/z' } })",
        "schemify.sim('espice', { path = 'espice', env = { ['A=B'] = 'x' } })",
    ] {
        let (cfg, out) = run(bad);
        let e = errors(&out);
        assert_eq!(e.len(), 1, "{bad}: {out:?}");
        assert!(e[0].contains("`env` must map"), "{bad}: {}", e[0]);
        assert!(cfg.table().sim(SimKind::Espice).is_none());
    }
    for bad in [
        "schemify.sim('espice', { path = 'espice', args = '-b' })",
        "schemify.sim('espice', { path = 'espice', args = { true } })",
    ] {
        let (cfg, out) = run(bad);
        let e = errors(&out);
        assert_eq!(e.len(), 1, "{bad}: {out:?}");
        assert!(e[0].contains("`args` must be a list"), "{bad}: {}", e[0]);
        assert!(cfg.table().sim(SimKind::Espice).is_none());
    }
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
    // `std::env::temp_dir()` panics on WASI ("not supported by WASI yet"); the CI runner maps
    // the host's /tmp into the sandbox (`wasmtime run --dir=/tmp::/tmp`).
    #[cfg(not(target_os = "wasi"))]
    let tmp = std::env::temp_dir();
    #[cfg(target_os = "wasi")]
    let tmp = PathBuf::from("/tmp");
    let root = tmp.join(format!("schemify-config-req-{tag}"));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("lua/keys")).unwrap();
    fs::write(
        root.join("lua/keys/init.lua"),
        "count = (count or 0) + 1\nreturn { chord = 'F6' }",
    )
    .unwrap();
    fs::write(root.join("lua/loop.lua"), "require('loop')").unwrap();
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
    // `./` segments are dropped lexically (PathBuf equality ignores them, the lockfile and
    // `pack status` text does not); `..` stays, since only the filesystem can resolve it.
    let (cfg, out) = Config::from_source(
        "schemify.pack.add { { src = './repo', name = 'r' }, { path = './a/./plug' }, \
         { path = '../up' } }",
        Some(&root),
    );
    assert!(out.is_empty(), "{out:?}");
    let shown = |i: usize| match &cfg.table().packs[i].source {
        Source::Git { url, .. } => url.clone(),
        Source::Path(p) => p.display().to_string(),
        Source::Url { .. } => panic!("no url source here"),
    };
    // Joined per component, as the expansion does, so the separators match on Windows too.
    let want = |parts: &[&str]| {
        parts
            .iter()
            .fold(root.clone(), |p, c| p.join(c))
            .display()
            .to_string()
    };
    assert_eq!(shown(0), want(&["repo"]));
    assert_eq!(shown(1), want(&["a", "plug"]));
    assert_eq!(shown(2), want(&["..", "up"]));
    assert!((0..3).all(|i| !shown(i).contains("/./") && !shown(i).contains("\\.\\")));
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
    let _ = fs::remove_dir_all(&root);
}

/// The documented example config runs without an error.
#[test]
fn the_example_config_is_valid() {
    let (cfg, out) = run(include_str!(concat!(
        env!("SCHEMIFY_ROOT"),
        "/docs/examples/init.lua"
    )));
    assert!(errors(&out).is_empty(), "{out:?}");
    assert_eq!(cfg.table().packs.len(), 2);
    assert_eq!(cfg.table().keymaps.chord.len(), 2);
}

/// Each call's argument checks: one located error per bad call, nothing queued or set.
#[test]
fn bad_arguments_are_located_errors_that_set_nothing() {
    for (src, why) in [
        (
            "schemify.keymap.set('Ctrl X', 'file.save')",
            "bad chord `Ctrl X`",
        ),
        (
            "schemify.keymap.set('F5', 'file save')",
            "bad command name `file save`",
        ),
        (
            "schemify.keymap.set({}, 'file.save')",
            "bad argument #1 to 'set'",
        ),
        ("schemify.cmd('a b')", "bad command name `a b`"),
        (
            "schemify.cmd('x.y', {})",
            "bad argument #2 to 'cmd' (string expected, got table)",
        ),
        ("schemify.notify('hi', 'loud')", "unknown level `loud`"),
        ("schemify.notify(nil)", "bad argument #1 to 'notify'"),
        (
            "schemify.sim('ngspice', { args = {} })",
            "sim `ngspice`: `path` is required",
        ),
        (
            "schemify.pack.add('x')",
            "bad argument #1 to 'add' (table expected, got string)",
        ),
        ("local x = schemify.opt.bogus", "unknown option `bogus`"),
    ] {
        let (cfg, out) = run(src);
        let e = errors(&out);
        assert_eq!(e.len(), 1, "{src}: {out:?}");
        assert!(
            e[0].starts_with(&format!("init.lua:1: {why}")),
            "{src}: {}",
            e[0]
        );
        assert_eq!(out.len(), 1, "{src}: {out:?}");
        assert_eq!(cfg.table(), &Table::default(), "{src}");
    }
    // `opts.desc` is kept; a number converts to a command argument string.
    let (cfg, out) =
        run("schemify.keymap.set('w', 'x.y', { desc = 'wire' })\nschemify.cmd('x.y', 5)");
    assert_eq!(cfg.table().keymaps.description, vec!["wire".to_owned()]);
    let cmd = Request::Command {
        name: "x.y".into(),
        args: "5".into(),
    };
    assert_eq!(out, vec![cmd]);
}

/// The per-call request capacity (spec §6) and the hook capacity: the first call past it
/// fails, located, and what came before stays.
#[test]
fn capacities_fail_the_call_past_their_limit() {
    let (_, out) = run(&format!(
        "for i = 1, {} do schemify.cmd('x.y') end",
        REQUESTS_MAX + 1
    ));
    assert_eq!(out.len(), REQUESTS_MAX + 1);
    let e = errors(&out);
    assert_eq!(e, vec!["init.lua:1: more than 256 requests in one call"]);
    let (cfg, out) = run(&format!(
        "for i = 1, {} do schemify.on('saved', function() end) end",
        HOOKS_MAX + 1
    ));
    assert_eq!(errors(&out), vec!["init.lua:1: more than 1024 hooks"]);
    assert_eq!(cfg.table().hooks.len(), HOOKS_MAX);
}

/// A function the config never returned, a config without Lua, and an unreadable `-u` file
/// are each one error request, never a panic.
#[test]
fn calls_without_a_function_or_a_file_are_errors() {
    let (mut cfg, out) = run("schemify.on('load', function() end)");
    assert!(out.is_empty(), "{out:?}");
    let e = cfg.call(FnId(9));
    assert!(errors(&e)[0].contains("no function 9"), "{e:?}");
    assert_eq!(e.len(), 1);
    let (mut clean, _) = Config::load(&Startup::Clean);
    assert_eq!(
        errors(&clean.call(FnId(0))),
        vec!["function 0 called without a config"]
    );
    assert!(clean.fire(Event::Load, &[]).is_empty());
    let missing = PathBuf::from("/nonexistent/schemify/init.lua");
    let (cfg, out) = Config::load(&Startup::File(missing.clone()));
    let want = format!("{}: cannot read the file", missing.display());
    assert_eq!(errors(&out), vec![want.as_str()]);
    assert!(cfg.root().is_none());
    assert_eq!(cfg.table(), &Table::default());
}
