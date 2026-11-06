# User config and packages v1 (normative)

Implements D9 and D35 as amended 2026-10-11 (Lua user config, user packages, `stdpath` directories), with D7 (capability grants) and D8 (last wins). Implementation: `crates/model/config` (directories, the sandboxed Lua runtime and the user-config table it fills), `crates/model/cli` (`schemify pack`, the package manager), `crates/view/app` (the editor applies the table and loads user packages through the plugin host). Example: [`docs/examples/init.lua`](../examples/init.lua).

Design files, the project manifest and lockfiles stay line records ([format.md](format.md)); only the user's own config is a program. Nothing in this spec is reachable from a design file (D7).

## 0. Usage first

```lua
-- ~/.config/schemify/init.lua
schemify.opt.snap = 5                          -- schematic units
schemify.opt.light = true
schemify.keymap.set("Ctrl+Shift+N", "sim.netlist", { desc = "netlist" })
schemify.keymap.set("F5", function() schemify.cmd("sim.simulate") end)
schemify.sim("ngspice", { path = "/opt/ngspice/bin/ngspice", args = { "-b", "-r", "{raw}", "{netlist}" } })
schemify.sim("espice", { path = "/opt/espice/bin/espice", env = { ZIG = "/opt/zig-0.17/bin/zig" } })
schemify.opt.simulator = "espice"              -- Simulate and `schemify run` use ESPice
schemify.on("saved", function(ev) schemify.notify("saved document " .. ev.doc) end)
schemify.pack.add {
  { src = "https://github.com/someone/gmid-lut.git", version = "v1.2.0", grant = { "read_project", "spawn:ngspice" } },
  { url = "https://example.org/eye.wasm", sha256 = "9f2c…", lazy = { cmd = "eye.open" } },
}
```

`schemify pack sync` installs the two packages and writes `~/.config/schemify/schemify-pack.lock`; commit both files to your dotfiles, and `schemify pack restore` reproduces the exact install elsewhere.

## 1. Directories

Like Neovim's `stdpath()`. A pure function of the environment and the OS:

| Kind | Linux, macOS | Windows | Web (OPFS) |
|---|---|---|---|
| config | `$XDG_CONFIG_HOME/schemify`, else `$HOME/.config/schemify` | `%LOCALAPPDATA%\schemify` | `/config` |
| data | `$XDG_DATA_HOME/schemify`, else `$HOME/.local/share/schemify` | `%LOCALAPPDATA%\schemify-data` | `/data` |
| state | `$XDG_STATE_HOME/schemify`, else `$HOME/.local/state/schemify` | `%LOCALAPPDATA%\schemify-data` | `/state` |
| cache | `$XDG_CACHE_HOME/schemify`, else `$HOME/.cache/schemify` | `%TEMP%\schemify` | `/cache` |

- macOS uses the XDG layout too (`~/.config`, not `~/Library`), so one dotfiles tree serves both.
- An `XDG_*` variable counts only when it is an absolute path (the XDG spec says to ignore relative ones). Windows honours the `XDG_*` variables as well when they are set.
- `SCHEMIFY_APPNAME`, when set and non-empty, replaces `schemify` in every native path (`NVIM_APPNAME`): a separate config, data and lockfile for experiments. It must be one path segment of `A–Z a–z 0–9 . _ -`, not `.` or `..`. The web paths carry no name (OPFS is per origin).
- An invalid `SCHEMIFY_APPNAME`, or a missing or relative `HOME` (Linux, macOS) or `LOCALAPPDATA`/`TEMP` (Windows), leaves the directory undefined: no user config runs and `schemify pack` refuses (exit 2), with a message naming the variable.

Files:

| Path | What |
|---|---|
| `<config>/init.lua` | The user config (§2). |
| `<config>/lua/` | Root of `require` (§2). |
| `<config>/lua/settings.lua` | The user's settings file, written by the settings editor (§3.4). |
| `<config>/schemify-pack.lock` | The package lockfile (§5), next to `init.lua`. |
| `<data>/pack/<name>/` | An installed `git` or `url` package (§5). |

## 2. Startup

Both binaries (`schemify`, the core, and `schemify-gui`, the editor) read the user config the same way before doing anything else:

1. Resolve the directories (§1).
2. Unless `--clean`, apply the user's settings file `<root>/lua/settings.lua` (§3.4), then run the init file: `-u <file>` if given, else `<config>/init.lua` when it exists. Its directory is the **config root** `<root>` (normally `<config>`): `require`, the settings file and the package lockfile resolve against it.
3. Report every error as `<file>:<line>: <message>` (stderr for the core, the status line for the editor) and continue with what the script set before the error, like Neovim. A config can never stop the tool from starting.

`--clean` skips the init file **and** every user package; the project's init file and its packages (below) still load, because they belong to the design, not to the user.

### The project's init file

A project names its own Lua file in its manifest header: `schemify 1 project` with the attribute `init <relpath>` ([format.md](format.md) §4), any path inside the project directory (an absolute path or one climbing out with `..` is refused). It is how a project declares the plugins a design cannot work without (a gm/Id lookup its docs page calls, its PDK helpers): `schemify.pack.add` specs in that file are **project packages**; those in the user's `init.lua` are **user packages**, the user's preferences. A project manifest has no `plugin` records of its own (a package's manifest still does, §5 Layout); the editor names any it finds in a note and loads none.

The file runs after the user's init file, in the same Lua state, with the whole API: its options, keymaps, hooks and sims win over the user's (a later `set` replaces an earlier one). `require` and relative package paths resolve against the project directory (`<project>/lua/`), and its packages install under `<project>/.schemify/pack/<name>` with the lockfile `<project>/schemify-pack.lock` (§5), committed with the project.

The project's settings file `<project>/lua/settings.lua` (§3.4) is applied just before the file runs. A project with a settings file and no `init` is trusted the same way, with the settings file in the dialog in place of the init file.

A design file is inert (D7), so the file runs, its settings apply and its packages load only once the user **trusts** them. The editor computes a digest over the file, every `.lua` file under `<project>/lua/` (the settings file among them), and each project package's name, source, grants, fuel and installed module (its sha256, or "absent"), and keeps the digests the user trusted in `<state>/trusted-projects` (§1; at most 1024, oldest dropped). Opening or reloading (`project.reload`) a project whose digest is not trusted, or one of whose packages is not installed, loads none of them and opens the **trust dialog**: the file, each package with its source and grants, and whether it downloads. Before that the file runs only into a throwaway config, to list its packages; nothing it sets is applied. *Trust & download* installs the missing packages as `schemify pack sync` does (§5; `url` hashes checked, `git` at the locked commit), records the digest of what is then on disk, and reopens the project; *Skip* leaves its plugins off, and values they compute keep their cached results (D20). Any change to the file, a module, a grant or a package is a new digest and asks again. The dialog is the core-layer `core.trust` (plugin-abi §1): no plugin or theme can relabel, restyle or cover it, and its *Trust* and *Skip* commands (`core.trust.ok`, `core.trust.skip`) answer only a click on their buttons, never a key, `schemify.cmd` or the JSON-RPC stream. Headless, `schemify-gui cmd --trust …` trusts as the dialog would; without `--trust` the project's plugins stay off.

Flags go before the command: `schemify --clean netlist top.shcomp`, `schemify -u test.lua pack status`. `schemify edit [flags] [path]` starts `schemify-gui` (§7) with the same flags.

`require "a.b"` loads `<root>/lua/a/b.lua`, else `<root>/lua/a/b/init.lua`, once (the result is cached like Lua's `package.loaded`). A module name is dot-separated segments of `A–Z a–z 0–9 _ -`; anything else (`..`, `/`, an absolute path) is an error. There is no `package` table, no `package.path` and no C module.

## 3. The `schemify` API

The init file and every function it registers run in one Lua 5.4 state that only the config crate touches. Everything the API sets lands in the **user-config table** (§6); the binaries read that table and never the Lua state.

| Call | Effect |
|---|---|
| `schemify.opt.<key> = value` | Sets an option; reading `schemify.opt.<key>` returns its value. Keys and types below; an unknown key or a wrong type is an error. |
| `schemify.keymap.set(chord, target, opts?)` | Binds `chord` (the keymap record syntax, `Ctrl+Shift+N`, `F5`, `w`) to `target`: a command name ([plugin-abi.md](plugin-abi.md) §7, the D27 command table) or a Lua function. `opts.desc` is a description string. A later `set` of the same chord replaces the earlier one. |
| `schemify.cmd(name, args?)` | Queues command `name` with the argument string `args` (default empty). The editor runs queued commands in order at its next delivery; the core has no command table yet and ignores them. |
| `schemify.on(event, fn)` | Calls `fn(ev)` after each `event` the editor delivers to plugins; returns the autocommand id. Shorthand for `schemify.api.create_autocmd(event, { callback = fn })` (§3.3). Events (plugin-abi §6 wire words): `load` (the editor is ready), `doc_opened`, `doc_closed`, `doc_changed`, `selection_changed`, `saved`, `run_done`. `ev` holds the event's fields (`ev.doc`, `ev.epoch`, `ev.job`) plus `ev.event`, `ev.id` and `ev.match`. Hooks run in registration order. |
| `schemify.api.*` | Neovim's API under Neovim's names: editor reads (§3.1), user commands (§3.2), autocommands (§3.3). There is no `vim` global; `local vim = schemify` is the user's choice. |
| `schemify.sim(name, { path = , args = , env = })` | Only at the top level of the user's own `init.lua`: a project file (trusted or not) and any function either file keeps get an error and set no row, so a cloned project cannot choose which programs run. The external programs of run.md §5: `ngspice`, `espice`, `xyce`, `ltspice` and `spectre` (simulator command lines; `{netlist}` and `{raw}` are replaced by the job's files; without `args` the defaults of run.md §5), `openvaf`, `iverilog` (compilers; `args` unused). `path` is required (a local `path` with a directory part that names no file when a run starts is a warning, `sim_notes`, and the simulator's default program on `PATH` runs instead); `args` is a list of strings; `env` maps variable names (non-empty, no `=`) to strings, at most 64, set for the program on top of the inherited environment and stored sorted by name (ESPice's `ZIG`, the Zig 0.17 it compiles Verilog-A with). Another name, or a bad `args`/`env`, is an error that leaves the row unset. The core's runner and the editor's Simulate both use all seven; a PDK-pinned or detected simulator runs through the same row. With `host = "[user@]host"` (a simulator only; a compiler with a `host` is an error) the row is that simulator's command line on that ssh destination (run.md §5, remote jobs), kept beside the local row and keyed by (simulator, destination), a later row replacing an earlier one; there `path` is optional (absent: the simulator's default program on the host's `PATH`). The destination is `user` `[a-z_][a-z0-9._-]{0,31}` and `@`, then a host name or `~/.ssh/config` alias `[A-Za-z0-9][A-Za-z0-9.-]{0,252}`: no port, IPv6 literal or option (they live in `~/.ssh/config`), so it never passes for an ssh option (`valid_host`). A control character in its `path`, `args` or `env` is an error (the words reach the host's login shell on one line). At most 64 rows name a host. |
| `schemify.notify(msg, level?)` | Shows `msg`: `level` is `"error"`, `"warn"` or `"info"` (default). |
| `schemify.pack.add { spec, … }` | Declares user packages (§5). Each call appends; a later spec with the same name replaces the earlier one. |

Options, one row of `OPTIONS` in `crates/model/config/src/options.rs` each (that table also drives `schemify.opt` and the editor's options, so the three cannot drift; a test checks this table against it):

<!-- options: generated from OPTIONS (crates/model/config/src/options.rs) -->
| Key | Type | Default | Meaning |
|---|---|---|---|
| `snap` | number in [0.001, 1000000] | `10` | Snap step, schematic units (xschem `cadsnap`). |
| `grid` | number in [0.001, 1000000] | `20` | Grid step, schematic units. |
| `show_grid` | boolean | `true` | Draw the grid. |
| `grid_axes` | boolean | `false` | Draw the x = 0 and y = 0 axes. |
| `light` | boolean | `false` | Light canvas colours (View → Light); also on with `theme_mode = "light"`. |
| `theme` | name | `"default"` | The theme: `default`, `modern` (bundled) or the id of an installed theme package (§5). Kept across restarts when picked in the View menu. |
| `theme_mode` | `"dark"`, `"light"`, `"system"` | `"dark"` | Dark or light chrome and canvas, or follow the system's choice. Kept across restarts. |
| `ui_zoom` | number in [0.5, 3] | `1` | Scale of the whole UI over the system's (View → Zoom UI in, out, reset), stored in percent. Kept across restarts. |
| `layer_colors` | table `{ [layer] = "rrggbb" }` | `{}` | Canvas colour per layer `0`–`21` over the theme's (View → Change current layer color); `"#rrggbb"` is accepted. Assign the whole table: reading returns a copy, so changing one of its entries sets nothing. |
| `dim` | number in [0, 1] | `0` | Dims the canvas toward its background (View → Dim colors), stored in percent. |
| `line_width` | number in [0.1, 10] | `1` | Canvas line width in pixels (View → Set line width), stored in tenths of a pixel. |
| `variable_width` | boolean | `false` | Line width follows the zoom. |
| `layer` | number in [0, 21] | `4` | Current drawing layer of lines, rectangles, polygons and arcs. |
| `hidden` | number in [0, 4194303] | `0` | Hidden canvas layers, bit `l` for layer `l`. |
| `fill` | boolean | `true` | Filled layers are drawn filled. |
| `symbol_text` | boolean | `true` | Texts inside symbol instances are drawn. |
| `show_symbols` | `"graphics"`, `"cell_boxes"`, `"boxes"` | `"graphics"` | Instances as their symbol graphics, boxes for schematic cells, or boxes for all. |
| `crosshair` | boolean | `false` | Crosshair at the snapped pointer. |
| `crosshair_px` | number in [0, 10000] | `0` | Crosshair half-size in pixels; 0 spans the canvas. |
| `snap_cursor` | boolean | `false` | Diamond on the connection point nearest the pointer. |
| `manhattan` | `"straight"`, `"horizontal"`, `"vertical"` | `"straight"` | Manhattan mode while drawing and moving: straight, horizontal first or vertical first (Space cycles). |
| `constrain` | `"none"`, `"horizontal"`, `"vertical"` | `"none"` | Axis constraint while drawing and moving (`h`, `v`). |
| `orthogonal` | boolean | `false` | Pick the Manhattan mode from the dominant axis (Shift+L). |
| `stretch` | boolean | `false` | Plain moves stretch attached wires (xschem `enable_stretch`, `y`). |
| `persistent` | boolean | `false` | Wire and line modes continue from each committed segment. |
| `auto_trim` | boolean | `false` | Join and trim wires after every wire edit. |
| `auto_highlight` | boolean | `false` | Every selection change highlights the selection's nets. |
| `highlight_insts` | boolean | `false` | Instances with a pin on a highlighted net are outlined. |
| `only_probes` | boolean | `false` | Everything but the highlight is dimmed. |
| `incr_color` | boolean | `false` | Each highlighted net gets its own colour. |
| `highlight_plotted` | boolean | `false` | The nets plotted in the waves dock are highlighted. |
| `toolbar` | boolean | `true` | The toolbar is shown. |
| `fullscreen` | boolean | `false` | Canvas-only view: menus, toolbar, tabs and status hidden. |
| `flat` | boolean | `false` | Netlists are flat (SPICE dialects). |
| `symbol_width` | number in [10, 10000] | `150` | Box width Make symbol draws, schematic units. |
| `export_color` | boolean | `true` | Exports use layer colours (else black on white). |
| `export_transparent` | boolean | `false` | SVG exports have no background rectangle. |
| `export_page` | `"a4"`, `"a3"`, `"a5"`, `"letter"`, `"legal"`, `"tabloid"` | `"a4"` | Paper PDF and EPS exports are fitted to. |
| `export_portrait` | boolean | `false` | PDF and EPS pages are portrait (else landscape). |
| `png_scale` | number in [0.1, 16] | `1` | PNG export scale; 1 is one image pixel per logical pixel (96 dpi), stored in percent. |
| `simulator` | `"ngspice"`, `"espice"`, `"xyce"`, `"ltspice"`, `"spectre"` or `nil` | `nil` | The simulator Simulate and `schemify run` start, and the dialect `schemify netlist` and the editor's netlist dialect default to (run.md §5). `--dialect`, the Netlist dialect menu and a PDK's pinned simulator override it; unset, the first installed simulator is used (espice first), else espice. |
| `check_app_updates` | boolean | `false` | Ask for a new Schemify release once a day at startup (update.md §3). Off, the app asks only from Help → Check for updates; it never contacts anything at startup. Only the user's own `init.lua` sets it. |
<!-- /options -->

Steps are stored in milli-units (D33), rounded to the nearest; a step that rounds to 0 or exceeds 10^6 units is an error. A layer outside `0`–`21`, a colour that is not six hex digits, or a number out of its range is an error. The editor reads the options once, at startup; changing a colour, the dim factor or the line width from a menu lasts for the session (the settings editor, §3.4, keeps it). The view state is the exception: `theme`, `theme_mode`, `light` and `ui_zoom` as last picked in the editor are kept in `<state>/view` (§1; one `<key> <value>` line each, values as Lua would write them) and applied at the next start to each of them neither Lua nor the project's settings file assigned (it wins over the user's settings file, which the settings editor writes together with it).

A key of two or more dot-separated segments of `A–Z a–z 0–9 _ -` (never `core.`) is a **plugin option**, `schemify.opt["gmid.points"] = 120`: a boolean, a number or a string (at most 4096 bytes, no control character but tab) kept for the plugin that declares it (§3.4); reading one returns the value the layers give, or `nil` when nobody assigned it.

Functions (keymap targets, hooks, user commands) run under the same sandbox and limits as the init file (§4), one budget per call. A function's error is reported as `<file>:<line>: <message>` and the editor carries on.

### 3.1 Editor reads

**When Lua runs.** The init file (and a trusted project file) runs once, before the editor has a document: it declares options, keymaps, hooks and commands. Everything else runs later, as a **call**: a hook, a keymap function, a user command. The editor queues each call and runs it at its next plugin delivery, right after it fills the plugin snapshot tables (plugin-abi §4), in the same frame; so a call reads exactly what the plugins of that delivery read. Reads are valid only inside a call: at the top level of an init file they raise `schemify.api.<name>: the editor is readable only inside a hook, a keymap function or a user command`. A read never changes anything and costs the call no budget beyond building its result.

```lua
schemify.api.create_user_command("Selected", function()
  for _, o in ipairs(schemify.api.doc_get_objects()) do
    if o.kind == "instance" then schemify.notify(o.name .. " " .. o.cell) end
  end
end)
```

| Call | Neovim | Returns |
|---|---|---|
| `schemify.api.list_docs()` | `nvim_list_bufs` | The open documents' ids (`Docs.handle`), in `Docs` order. |
| `schemify.api.get_current_doc()` | `nvim_get_current_buf` | The active document's id, or `nil` with none open. |
| `schemify.api.doc_get_info(doc?)` | `nvim_buf_get_name` and buffer options | `{ id, kind, dirty, path, cell }`: `kind` the plugin-abi `Docs.kind` number, `dirty` a boolean, `path` under the project root when inside it, `cell` the file's stem. |
| `schemify.api.doc_get_objects(doc?, kind?)` | `nvim_buf_get_lines` | Every instance, then wire, then label of the document, or only those of `kind` (`"instance"`, `"wire"`, `"label"`): each a table with `kind` and its plugin-abi columns but `doc` (`id, x, y, orient, cell, name`; `id, x1, y1, x2, y2, net`; `id, x, y, net, text`). Coordinates are integer milli-units (D33), as plugins read them; `net` is absent for an object on no net. |
| `schemify.api.get_selection()` | (visual selection) | The selected object ids of the active document. |

`doc` is a document id; `nil` (or absent) is the active document. An id not in `list_docs()` is an error (`invalid document id N`), as is a document other than the active one for `doc_get_objects`: only the active document's objects are in the snapshot. Options are read through `schemify.opt` (§3), the value the config holds.

### 3.2 User commands

```lua
schemify.api.create_user_command("Netlist2", function(o)
  schemify.cmd("sim.dialect", o.fargs[1])
  schemify.cmd("sim.netlist")
end, { nargs = 1, desc = "netlist in another dialect" })
```

`schemify.api.create_user_command(name, fn, opts?)` adds a row to the D27 command table, so a keymap (`schemify.keymap.set("F6", "Netlist2")`), a menu or UI node's `command`, `schemify.cmd` and the command stream run it like any other command; `del_user_command(name)` removes it (an error when there is none). `fn` gets `{ name, args, fargs }`: `args` the argument string, `fargs` its whitespace-separated words, or `args` whole for `nargs` `1` and `"?"` (Neovim's rule).

- **Names.** An uppercase ASCII letter, then letters, digits or `_`, at most 64 bytes (Neovim's rule). Core and plugin commands are lowercase `<menu>.<action>` words, so a user command never collides with a core command and never takes the reserved `core.` prefix; over a plugin command of the same name it wins (it applies last, §6).
- **`opts`:** `desc` (a string), `nargs` (`0` default, `1`, `"?"`, `"*"`, `"+"`; arguments that do not fit fail the call with an error), `force` (default `true`: a later create replaces the row; `false`: an existing name is an error). Another key is an error.
- **Consent.** A command queued by Lua (`schemify.cmd` from any call, or from the init file) never runs a command that stands for the user's consent (accepting staged edits, installing updates, trusting a project or a PDK host, deleting files): it is refused with `<name>: needs you (a click or a key), not a Lua call`. Only the user's own click or key runs those; a key bound to a user command does not lend it that consent.

### 3.3 Autocommands

```lua
local g = schemify.api.create_augroup("my_tb", { clear = true })
schemify.api.create_autocmd({ "saved", "doc_opened" }, {
  group = g, pattern = { "*_tb", "amp?" }, once = false,
  callback = function(ev) schemify.notify(ev.event .. " " .. ev.match) end,
})
```

| Call | Effect |
|---|---|
| `create_augroup(name, { clear = true })` | Returns the group's id (≥ 1). An existing group keeps its id; unless `clear = false` it loses its autocommands first, so re-running a config does not stack hooks. |
| `create_autocmd(event \| { event, … }, opts)` | One hook per event, all under the returned id. `opts`: `callback` (a function, required), `group` (a name or id from `create_augroup`), `pattern` (a cell-name glob or a list of them), `once`, `desc`. Another key is an error. |
| `del_autocmd(id)` | Removes every hook of autocommand `id`; an unknown id is an error. |

- **Patterns** match the whole cell name of the event's document (its `doc` field; for `selection_changed`, `run_done` and `load`, the active document): `*` any run, `?` one character, everything else itself. A hook with a pattern does not run when there is no document or no cell name. `ev.match` is that cell name.
- **`once`** deletes the autocommand (all its events) just before its first run.
- A hook another hook of the same event deleted does not run. Ids are never reused within a session.

### 3.4 Settings files and the settings editor

The editor's settings overlay (`view.settings`: Options → Settings…, the gear at the foot of the icon bar, Ctrl+,) shows every row of the option table above and every option a plugin declares (`contribute options`, [plugin-abi.md](plugin-abi.md) §7), by section (the table's group column, then one section per plugin), with a control per type: a checkbox, a number field that clamps into the row's range, a row of word buttons, a text field, a colour field with a swatch, one colour field per canvas layer for `layer_colors`. A search box filters by key, title, doc text and section; the theme picker on top lists the installed themes (§5 Themes): a click previews one, *Done* keeps it (written as `theme` to the shown scope), *Cancel* or Escape goes back to the theme the overlay opened with.

Lua stays the source of truth. The overlay never writes an init file; it writes one tool-owned file per scope, rewritten whole on every change:

| Scope | File | Applied |
|---|---|---|
| User | `<root>/lua/settings.lua` (`<root>` the config root, §2) | before the user's init file runs |
| Project | `<project>/lua/settings.lua` | before the project's init file runs, once the project is trusted (§2) |

A settings file is a strict subset of Lua that the config crate reads without running it: a header comment, then one assignment per line in key order, `schemify.opt.<key> = <literal>` for a core option and `schemify.opt["<plugin>.<key>"] = <literal>` for a plugin's; a literal is `true`, `false`, `nil`, a decimal number, a double-quoted string (escapes `\"`, `\\`, `\t` only) or the colour table `{ [3] = "ff0000" }`. Lua running the same file gives the same values. A line outside this grammar, an unknown key or a value outside its row's domain is skipped with a warning naming its line; the other lines still apply. Do not `require "settings"` from `init.lua`: it is already applied, and a value the init file sets counts as the init file's.

Precedence, lowest first: the default (or the plugin's `option` record) < the user's settings file < the user's Lua (`init.lua`, its modules, the functions it keeps) < the project's settings file < the project's init file. The overlay shows each scope's own file: a row the scope's file holds is marked *Modified* and has *Reset* (drops the row; the option falls back to the next layer down); a value an init file of that scope sets is locked (*set in init.lua*, *set in the project's init file*), and a row a higher scope sets says so. Every change applies at once.

Security (D7): the project's file is project content, covered by the project's trust digest (§2), so it applies only once the user trusts the project; the overlay edits it only while the project is trusted or has nothing to trust yet, and trusts the digest of what it just wrote (the user's click made it). It never holds a user-only option: such a row is locked in the Project tab, and a project file that names one has that line skipped with a warning. Writing a settings file (`settings.set`, `settings.toggle`, `settings.pick`, `settings.color`, `settings.reset`, `settings.done`) runs from the window and the dev command stream, never from a plugin's `command` record or `schemify.cmd`; a user-only option changes only from the window. No overlay command is a consent command (`Cmd::needs_user`).

## 4. Sandbox

One interpreter on every target: omnilua, a pure-Rust Lua 5.4 (`crates/model/config/src/lua.rs`; the user's decision of 2026-10-12, web.md §5), built without its `io`, `os` and `package` libraries.

- **Present:** the base library without `dofile`, `loadfile`, `load`, `collectgarbage` and `require` (replaced by §2's); `string`, `table`, `math`, `utf8`. `print(...)` is an info notification (`schemify.notify`). `pcall` and `xpcall` return the error as its message string and never catch budget exhaustion (otherwise `while true do pcall(f) end` would outlive any budget).
- **Absent:** `io`, `os`, `debug`, `package` (so no `package.loadlib`), `coroutine`, binary chunks. Nothing in the config can open a file other than its own modules, start a process or reach the network (D7); packages are wasm and get capabilities only through grants (§5).
- **Instruction budget:** 10^8 Lua VM instructions per call (the init file with everything it requires, or one function call), charged every 1000 instructions, like plugin fuel; the pattern matcher charges it as it backtracks, so one library call cannot outrun it. Exhaustion stops the call with `instruction budget exhausted`, located at the running line.
- **Memory cap:** 64 MiB of collected memory for the whole Lua state, sampled with the budget; a buffer of known size (`string.rep`, `table.concat`, `string.pack`) is checked before it is built. Past the cap the call stops with `not enough memory`; `pcall` may catch that one (the state stays usable and the call keeps the budget it has left).
- **Locations:** errors read `<file>:<line>: <message>`. The API functions raise at the calling line, as `luaL_error` does; a small prelude, compiled before `debug` is removed, keeps `debug.getinfo` private for that.

A runaway config is reported and stopped; it never hangs the tool.

On the web the init file is OPFS `/config/init.lua` and its modules `/config/lua/` (web.md §2, §5); everything else is as natively.

## 5. Packages

A package is a prebuilt wasm plugin ([plugin-abi.md](plugin-abi.md)). Packages never run native code: there are no build hooks.

### Spec

`schemify.pack.add` takes a list of specs (tables), lazy.nvim-style:

| Field | Meaning |
|---|---|
| `src` | Git repository: anything `git clone` accepts; a path starting `./`, `../` or `~/` is relative to the config root (or the home directory). Local paths (here and in `path`) are cleaned lexically: `.` segments are dropped, `..` is kept (no filesystem lookup), so the lockfile and `pack status` print `<root>/plugins/x`, never `<root>/./plugins/x`. |
| `url` + `sha256` | One `.wasm` file fetched with `curl`; `sha256` (64 lowercase hex digits) is required and checked before anything is installed. |
| `path` | A local package directory used in place (development); never copied. Relative to the config root, or the home directory with `~/`. |
| `name` | Package name; default: the last segment of the source without `.git`/`.wasm`. One word of `A–Z a–z 0–9 . _ -`. |
| `rev` / `version` | Git only: the branch, tag or commit to check out (`version` is a tag; give one of the two). Default: the remote's default branch. |
| `grant` | List of capabilities the user grants (plugin-abi §5). |
| `fuel` | Per-event instruction budget, whole number in [10^6, 10^11] (plugin-abi §2); default 10^8. |
| `lazy` | `{ cmd = "x.run" or {…}, event = "doc_opened" or {…} }`: load the plugin on the first matching command or event (§3 event words) instead of at startup. Default: eager. |

Exactly one of `src`, `url`, `path`. A spec error (missing source, bad name, unknown field, bad `sha256`) is reported with its `file:line` and that spec is dropped. At most 128 user packages.

### Layout

A package directory holds the module and, optionally, its manifest:

```
<package>/
  schemify.project      # optional: one `plugin` record (format.md §4, plugin-abi §5)
  <name>.wasm
```

`schemify.project` is the project-manifest format with a single `plugin <name> path <file>.wasm` record whose `grant` attributes are the capabilities the plugin **requests** and whose `fuel` is its suggested budget (the spec's `fuel` wins). Without a manifest the module is `<name>.wasm` and it requests exactly what the user grants. A `url` package is a single `.wasm` file installed as `<data>/pack/<name>/<name>.wasm`.

### Themes

A **theme package** is records only: nothing in it runs, so it is as inert as a design file. Its manifest holds one record instead of `plugin`:

```
schemify 1 project
theme <id> <dark|light> path <file>.records
```

`<id>` is one word of `A–Z a–z 0–9 . _ -`; the file lies inside the package (no `..`, not absolute) and is installed, locked and hashed like a module. Its body is ordinary UI records (plugin-abi §7: `node`, `set`, `remove`, `style`, and the tokens `color <name> <hex>` and `size <name> <px>`), so a theme may restyle and rearrange the whole UI. The editor composes the active theme after every plugin and before the user's keymaps (D8 last wins); any edit of it that touches `core.*` is dropped and counted, as a plugin's is (plugin-abi §1). A theme grants nothing and requests nothing.

- **Bundled:** `default` (the default UI alone) and `modern` (`ui/themes/modern/`, the same package layout).
- **Choice:** `schemify.opt.theme = "<id>"` (§3), or View → Modern theme (`view.theme.<id>`; picking the active theme again goes back to `default`). An id no theme has leaves `default` and says so on the status line.
- **Tokens:** a style value `@<name>` takes the value of the last `color`/`size` token of that name in any contribution, parsed for the prop that references it, once when the UI is composed; an unknown token, or a colour token on a length, leaves the prop as it was and is a health row.
- **Light chrome:** while the canvas is light (`light`, `theme_mode`), every selector `X.light` lays its props over `X`.
- **Canvas roles:** `canvas.bg`, `canvas.wire`, `canvas.text`, `canvas.symbol`, `canvas.pin`, `canvas.grid` and `canvas.select` (`fg`, with `.light` variants) colour layers 0, 1, 3, 4 and 5, the grid and the selection over the numbered `canvas.<dark|light>.<layer>` selectors.

### Capabilities

A loaded package gets the **intersection** of what it requests and what the user grants (D7). `schemify pack status` reports both differences, requested but not granted (`denied`) and granted but not requested (`unused`); the editor's load notes name the denied ones.

### Lockfile

`<root>/schemify-pack.lock`, next to the init file (a project's: `<project>/schemify-pack.lock`, §2), in the lockfile row form of [format.md](format.md) §4: `locked plugin <name> <kind> <resolved> <sha256>`, sorted by name. `<kind>` is `git`, `url` or `path`; `<resolved>` is the commit, the url or the absolute path; `<sha256>` is the sha256 of the package's `.wasm`. Tool-written only; meant to be committed with the init file.

### Commands

`schemify pack <command>`; exit 0 on success, 1 if any package failed, 2 on a usage error or an unusable config.

| Command | Does |
|---|---|
| `sync` | Installs missing packages — at the locked commit when the lockfile has one, else at `rev`/`version` or the default branch — and writes the lockfile rows of every listed package. Installed packages are not moved. |
| `update` | Fetches every `git` package and moves it to `rev`/`version` (or the default branch's latest commit), re-hashes every package, rewrites the lockfile. |
| `restore` | Makes every listed package exactly what the lockfile says: checks out the locked commit, re-fetches a `url` whose file is missing; a hash that still differs from the lock is an error. Packages without a lock row are installed as by `sync`. |
| `clean` | Deletes every directory under `<data>/pack/` that no spec names and drops lockfile rows for unlisted packages. |
| `status` | One line per package: name, kind, resolved source, `ok` / `missing` / `changed` (hash ≠ lock) / `unlocked`, and the `denied` and `unused` grants. The editor shows the same lines in a read-only `packages` tab (command `tools.package_status`), with a `schemify pack sync` hint when a package is missing. |

Fetching: `git` sources by running `git` (`clone`, `fetch`, `checkout`, `rev-parse`), `url` sources by running `curl -fsSL -o <tmp> <url>`, as lazy.nvim and `vim.pack` do; no HTTP library is linked. A downloaded file whose sha256 differs from the spec is deleted and the package fails with nothing installed.

### Updates (`outdated`)

Nothing checks packages at startup (decision 2026-11-06). `pack::outdated` is the query a caller runs on demand (the marketplace marks an installed item *Update available*, marketplace Phase 3): for each `git` package, one `git ls-remote` (no clone) compares the remote with the lock row. A spec without `rev`, or whose `rev` names a branch, has an update when that head moved; a `rev` naming a tag (`version`) is the user's pin, and a newer version tag (same text before the first digit, numerically greater, no pre-release part) is reported but not applied; a commit `rev` never moves. `url` and `path` packages have no upstream to ask; an unreachable remote is skipped silently. `update` (above) is what moves a package.

## 6. Load order and the user-config table

Plugin composition order (D8, last wins): **bundled default UI < user packages (spec order) < project packages (the project init file's spec order)**, then the keymaps set in Lua (`schemify.keymap.set`, the user's then the project file's) last.

Why: D8 makes the more specific contribution win. User packages are the user's general preferences, so they override the bundled defaults; a project's packages are what that design needs (its netlister, its PDK helpers), so they override the user's general set, the way a project's `.editorconfig` overrides an editor's defaults. Keymaps typed in `init.lua` are the user's explicit muscle memory and are applied after every plugin, as Neovim users expect from mappings made after plugins load.

The user-config table (owner: `crates/model/config`; readers: both binaries):

| Table | Rows | Capacity |
|---|---|---|
| options | one per row of §3, with the layer that assigned it (§3.4) | fixed |
| plugin options | key, layer, literal (§3.4) | 256 keys |
| keymaps | chord, target (command name or function id), description | 4096 |
| sims | `ngspice`, `openvaf`, `iverilog`, `espice`: path, args, env (≤ 64 each) | 4 |
| hosts | simulator, ssh destination, path, args, env (`sim` rows with a `host`) | 64 |
| packages | the specs of §5, each marked user or project (§2) | 128 |
| hooks | autocommand id, event word, function id, group, cell patterns (≤ 64 of ≤ 256 bytes), once | 1024 |
| groups | autocommand group name | 256 |
| commands | user command name, function id, description, nargs | 1024 |
| requests (output of each call) | queued commands, notifications, errors | 256 per call |

## 7. Binaries

`schemify` is the headless core: CLI, config, package manager. It links no GPU, window, text-shaping, accessibility or LaTeX code (D39, checked by `crates/model/cli/tests/deps.rs`). `schemify edit …` runs `schemify-gui` from the directory of the running `schemify`, else from `PATH`, passing the remaining arguments; when neither exists it exits 2 saying so.

`schemify-gui [flags] [edit] [<project-dir>|<cell-file>] [--no-default-ui] [--rpc <socket>]`: after the flags (§2) an optional word `edit`, then at most one path, `--no-default-ui` and `--rpc <socket>` in any order (the word after `--rpc` is the socket, whatever it looks like). `--rpc` without a socket, a second `--rpc`, any other word starting with `-` or a second path prints the usage text and exits 2; a bad flag prints its message first. The config crate parses the line (`Launch::parse`) and holds the usage text (`GUI_USAGE`).
