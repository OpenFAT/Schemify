# User configuration

Your personal settings live in one Lua file, `init.lua`, in the style of Neovim. It sets editor options, binds keys, names your simulator programs, hooks events and declares user packages. Both `schemify` and the editor `schemify-gui` run it at startup.

Design files never hold user settings, and the config cannot reach into a design: a project you clone cannot change your keys or start a program on your machine.

## Where the config lives

| OS | Config directory |
|---|---|
| Linux, macOS | `$XDG_CONFIG_HOME/schemify`, else `~/.config/schemify` |
| Windows | `%LOCALAPPDATA%\schemify` |

macOS uses the XDG layout too (`~/.config`, not `~/Library`), so one dotfiles tree serves both. An `XDG_*` variable counts only when it is an absolute path; Windows honours the `XDG_*` variables when they are set.

| Path | What |
|---|---|
| `<config>/init.lua` | Your config. |
| `<config>/lua/` | Modules for `require`. |
| `<config>/schemify-pack.lock` | The package lockfile, next to `init.lua`. |
| `<data>/pack/<name>/` | An installed package. `<data>` is `$XDG_DATA_HOME/schemify`, else `~/.local/share/schemify` (Windows `%LOCALAPPDATA%\schemify-data`). |

`SCHEMIFY_APPNAME` replaces `schemify` in every one of these paths, so you can keep a separate config, package set and lockfile for experiments:

```sh
SCHEMIFY_APPNAME=schemify-test schemify edit amp/top.shcomp
```

It must be one path segment of `A-Z a-z 0-9 . _ -`. An invalid name, or a missing `HOME` (`LOCALAPPDATA` on Windows), means no user config runs and `schemify pack` exits 2 with a message naming the variable.

In the browser build the init file is `/config/init.lua` in the browser's storage, and its modules are under `/config/lua/` ([web.md](web.md)).

## Startup flags

Put these before the command, for `schemify` and `schemify edit` alike:

| Flag | Effect |
|---|---|
| `--clean` | Skip `init.lua` and every user package. Project packages (the `plugin` records of `schemify.project`) still load, because the design needs them. |
| `-u <file>` | Run `<file>` instead of `<config>/init.lua`. Its directory becomes the config root, so `require` and the lockfile resolve next to it. |

```sh
schemify --clean edit amp/top.shcomp
schemify -u ~/scratch/test.lua pack status
```

An error in the config is reported as `<file>:<line>: <message>` (on stderr for `schemify`, in the status line of the editor), and startup continues with whatever the script set before the error. A broken config never stops the tool from starting.

## A starting config

This is `docs/examples/init.lua`, shortened:

```lua
-- ~/.config/schemify/init.lua
schemify.opt.snap = 5
schemify.opt.light = true
schemify.opt.layer_colors = { [4] = "ff8000" }  -- symbol layer orange

schemify.keymap.set("Ctrl+Shift+N", "sim.netlist", { desc = "netlist the active cell" })
schemify.keymap.set("F5", function()
  schemify.cmd("sim.simulate")
  schemify.notify("simulating…")
end)

schemify.sim("ngspice", { path = "ngspice", args = { "-b", "-r", "{raw}", "{netlist}" } })

schemify.on("saved", function(ev)
  schemify.notify("saved document " .. ev.doc)
end)
```

## Options

Set an option with `schemify.opt.<key> = value`; read it back the same way. An unknown key or a value of the wrong type is an error.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `snap` | number > 0 | 10 | Snap step, in schematic units (xschem's `cadsnap`). |
| `grid` | number > 0 | 20 | Grid step, in schematic units. |
| `show_grid` | boolean | `true` | Draw the grid. |
| `light` | boolean | `false` | Light canvas colours. |
| `layer_colors` | table `{ [layer] = "rrggbb" }` | `{}` | Canvas colour per layer `0` to `21`, over the theme's. `"#rrggbb"` is accepted. |
| `dim` | number in [0, 1] | 0 | Dim the canvas toward its background (View → Dim colors). |
| `line_width` | number in [0.1, 10] | 1 | Canvas line width in pixels (View → Set line width). |
| `simulator` | `"ngspice"` or `"espice"` | `"ngspice"` | The simulator Simulate and `schemify run` start, and the default netlist dialect of `schemify netlist` and the editor. `--dialect` and Simulation → Netlist dialect override it. |

Assign `layer_colors` as a whole table: reading it returns a copy, so changing one entry of that copy sets nothing. A step that rounds to zero, a layer outside `0` to `21`, a colour that is not six hex digits, or a number out of range is an error.

The editor reads the options once, at startup. A colour, dim factor or line width you change from the View menu lasts for the session; put it in `init.lua` to keep it.

## Keymaps

```lua
schemify.keymap.set(chord, target, opts)
```

`chord` uses the keymap syntax of the editor: `w`, `F5`, `Ctrl+Shift+N`, `Alt+X`. Modifiers are `Ctrl`, `Shift`, `Alt` and `Super` (or `Cmd`). Letters ignore case, so `w` and `W` are the same key; write `Shift+W` for the capital. `target` is a command name (the same names the menus and default keys use, such as `sim.netlist`, `sim.simulate`, `file.save`, `view.zoom_full`, or a plugin's command) or a Lua function. `opts.desc` is an optional description.

```lua
schemify.keymap.set("Ctrl+Shift+N", "sim.netlist", { desc = "netlist" })
schemify.keymap.set("F6", function() schemify.cmd("sim.dialect", "xyce") end)
```

A later `set` of the same chord replaces the earlier one. Your keymaps apply after every plugin's, so they win over the default keys. Press `/` in the editor for a summary of the current keys; [editor.md](editor.md) lists the defaults. At most 4096 keymaps.

## Simulators and compilers

```lua
schemify.sim(name, { path = "...", args = { ... }, env = { ... } })
```

Simulator and compiler command lines live only here, never in a design file. `name` is one of:

| Name | Used for | Default command line |
|---|---|---|
| `ngspice` | Simulating in the `ngspice` dialect | `ngspice -b -r {raw} {netlist}` |
| `espice` | Simulating in the `espice` dialect | `espice -b --tokenizer=ngspice -r {raw} {netlist}` |
| `openvaf` | Compiling Verilog-A for ngspice | `openvaf` (`args` unused) |
| `iverilog` | Compiling Verilog for ngspice's `d_cosim` | `iverilog` (`args` unused) |

`path` is required. `args` is a list of strings in which `{netlist}` and `{raw}` are replaced by the job's files; leave it out to keep the default arguments. `env` sets environment variables for the program on top of the inherited ones (at most 64). The editor's Simulate and `schemify run` both read these rows.

```lua
schemify.sim("ngspice", { path = "/opt/ngspice/bin/ngspice" })
schemify.sim("espice", { path = "/opt/espice/bin/espice", env = { ZIG = "/opt/zig-0.17/bin/zig" } })
schemify.sim("openvaf", { path = "openvaf-r" })   -- OpenVAF-Reloaded installs openvaf-r
schemify.opt.simulator = "espice"
```

ESPice compiles Verilog-A with Zig 0.17: set `ZIG` in `env` as above, or have `zig` on `PATH`. Another `name`, or a bad `args` or `env`, is an error and leaves that row unset. See [simulation.md](simulation.md).

## Events, commands and messages

| Call | Effect |
|---|---|
| `schemify.on(event, fn)` | Call `fn(ev)` after each `event` in the editor. Hooks run in registration order. |
| `schemify.cmd(name, args)` | Queue command `name` with an optional argument string. The editor runs queued commands in order; `schemify` ignores them. |
| `schemify.notify(msg, level)` | Show `msg`. `level` is `"error"`, `"warn"` or `"info"` (default). |
| `print(...)` | Same as an info notification. |

| Event | When | Fields of `ev` |
|---|---|---|
| `load` | The editor is ready. | |
| `doc_opened` | A document opened. | `ev.doc` |
| `doc_closed` | A document closed. | `ev.doc` |
| `doc_changed` | A document was edited. | `ev.doc`, `ev.epoch` |
| `selection_changed` | The selection changed. | |
| `saved` | A document was saved. | `ev.doc` |
| `run_done` | A simulation job finished. | `ev.job` |

`schemify` (the core) shows notifications on stderr as `note: …`, `warning: …` and `error: …`; the editor shows them in its status line. An error inside a hook or keymap function is reported with its `file:line` and the editor carries on.

## Splitting the config into modules

`require "keys"` loads `<config>/lua/keys.lua`; `require "my.keys"` loads `lua/my/keys.lua`, else `lua/my/keys/init.lua`. Each module runs once and its result is cached. Module names are dot-separated segments of `A-Z a-z 0-9 _ -`: no `..`, no `/`, no absolute paths. There is no `package` table and no C module.

## What the config can do

The config runs in a sandboxed Lua 5.4. It has the base library, `string`, `table`, `math` and `utf8`. It has no `io`, `os`, `debug`, `package` or `coroutine`, and no `load`, `loadfile`, `dofile` or `collectgarbage`: it cannot open files other than its own modules, start a process or reach the network.

Each call (the init file with everything it requires, or one hook or keymap function) gets a budget of 10^8 Lua instructions and the whole Lua state is capped at 64 MiB. A runaway loop stops with `instruction budget exhausted`, located at the running line, and the tool goes on. Details in [config.md](../spec/config.md#4-sandbox).

## Packages

A user package is a prebuilt wasm plugin you install for yourself, in every project. Packages never run native code and have no build step. To write one, see [Writing a plugin](../plugins/README.md). Declare them with `schemify.pack.add`, then install them with `schemify pack sync`:

```lua
schemify.pack.add {
  { src = "https://github.com/someone/schemify-gmid-lut.git", version = "v1.2.0",
    grant = { "read_project", "spawn:ngspice" } },
  { url = "https://example.org/eye-diagram.wasm", sha256 = "<64 hex digits>",
    lazy = { cmd = "eye.open" } },
  { path = "~/src/my-plugin" },
}
```

Each spec takes exactly one source, `src`, `url` or `path`:

| Field | Meaning |
|---|---|
| `src` | A git repository (anything `git clone` accepts). A path starting `./`, `../` or `~/` is relative to the config root or your home directory. |
| `url` + `sha256` | One `.wasm` file fetched with `curl`. `sha256` (64 lowercase hex digits) is required and checked before anything is installed. |
| `path` | A local package directory used in place, never copied. For developing a plugin. |
| `name` | Package name. Default: the last segment of the source without `.git` or `.wasm`. |
| `rev` / `version` | Git only: a branch, tag or commit (`rev`), or a tag (`version`). Give one of the two. Default: the remote's default branch. |
| `grant` | The capabilities you grant the package (below). |
| `fuel` | Instruction budget per event, a whole number in [10^6, 10^11]. Default 10^8. |
| `lazy` | `{ cmd = "x.run" or {…}, event = "doc_opened" or {…} }`: load the package on its first matching command or event instead of at startup. |

Each `add` call appends; a later spec with the same name replaces the earlier one. A bad spec (no source, a bad name, an unknown field, a bad `sha256`) is reported with its `file:line` and dropped. At most 128 user packages.

### Grants

A package can use a capability only when it requests it and you grant it. It gets the intersection of the two.

| Capability | Lets the package |
|---|---|
| `read_project` | Read files of the open project. |
| `write_project` | Write files of the open project; delete only inside `.schemify/`. |
| `spawn:<binary>` | Run that program, found on `PATH` (for example `spawn:ngspice`). Native builds only. |
| `network` | Fetch URLs. |
| `clipboard` | Read and write the clipboard. |

A package declares what it requests in an optional `schemify.project` in its directory, with one `plugin <name> path <file>.wasm` record and its `grant` lines. Without that file the module is `<name>.wasm` and it requests exactly what you grant. `schemify pack status` shows requested but not granted capabilities as `denied` and granted but not requested ones as `unused`.

### Managing packages

| Command | Does |
|---|---|
| `schemify pack sync` | Install missing packages (at the locked commit when the lockfile has one) and write the lockfile. Installed packages are not moved. |
| `schemify pack update` | Fetch every git package, move it to its `rev`/`version` or the default branch's latest commit, re-hash everything and rewrite the lockfile. |
| `schemify pack restore` | Make every package exactly what the lockfile says: check out the locked commit, re-fetch a missing `url` file. A hash that still differs is an error. |
| `schemify pack clean` | Delete installed packages that no spec names and drop their lockfile rows. |
| `schemify pack status` | One line per package: name, kind (`git`, `url`, `path`), resolved source, and `ok`, `missing`, `changed` (hash differs from the lock) or `unlocked`, followed by any `denied` and `unused` grants. |

```
$ schemify pack status
demo: path - unlocked
```

`pack` exits 0 on success, 1 if any package failed, and 2 for a usage error or when there is no user config (`--clean`, or no config directory). Git sources are fetched by running `git`, `url` sources by running `curl`. The editor shows the same status lines with the command `tools.package_status`.

The lockfile `schemify-pack.lock` records each package's resolved commit, URL or path and the sha256 of its `.wasm`. Commit it with `init.lua` in your dotfiles; `schemify pack restore` then reproduces the exact install on another machine.

## Load order

When several sources bind the same key or contribute the same thing, the later one wins:

1. the bundled default UI,
2. your user packages, in spec order,
3. the project's packages (`plugin` records of `schemify.project`), in manifest order,
4. your `schemify.keymap.set` calls.

A project's packages override your general ones because they are what that design needs. Your own keymaps come last, so your muscle memory always holds.

The full contract is in [config.md](../spec/config.md); the capabilities in [plugin-abi.md](../spec/plugin-abi.md#5-capabilities-d7).

See also: [cli.md](cli.md), [simulation.md](simulation.md), [editor.md](editor.md).
