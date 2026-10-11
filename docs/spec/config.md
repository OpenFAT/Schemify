# User config and packages v1 (normative)

Implements D9 and D35 as amended 2026-10-11 (Lua user config, user packages, `stdpath` directories), with D7 (capability grants) and D8 (last wins). Implementation: `crates/config` (directories, the sandboxed Lua runtime and the user-config table it fills), `crates/cli` (`schemify pack`, the package manager), `crates/app` (the editor applies the table and loads user packages through the plugin host). Example: [`docs/examples/init.lua`](../examples/init.lua).

Design files, the project manifest and lockfiles stay line records ([format.md](format.md)); only the user's own config is a program. Nothing in this spec is reachable from a design file (D7).

## 0. Usage first

```lua
-- ~/.config/schemify/init.lua
schemify.opt.snap = 5                          -- schematic units
schemify.opt.light = true
schemify.keymap.set("Ctrl+Shift+N", "sim.netlist", { desc = "netlist" })
schemify.keymap.set("F5", function() schemify.cmd("sim.simulate") end)
schemify.sim("ngspice", { path = "/opt/ngspice/bin/ngspice", args = { "-b", "-r", "{raw}", "{netlist}" } })
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
| `<config>/schemify-pack.lock` | The package lockfile (§5), next to `init.lua`. |
| `<data>/pack/<name>/` | An installed `git` or `url` package (§5). |

## 2. Startup

Both binaries (`schemify`, the core, and `schemify-gui`, the editor) read the user config the same way before doing anything else:

1. Resolve the directories (§1).
2. Unless `--clean`, run the init file: `-u <file>` if given, else `<config>/init.lua` when it exists. Its directory is the **config root** (normally `<config>`): `require` and the package lockfile resolve against it.
3. Report every error as `<file>:<line>: <message>` (stderr for the core, the status line for the editor) and continue with what the script set before the error, like Neovim. A config can never stop the tool from starting.

`--clean` skips the init file **and** every user package; project packages (the project manifest's `plugin` records) still load, because they belong to the design, not to the user.

Flags go before the command: `schemify --clean netlist top.shcomp`, `schemify -u test.lua pack status`. `schemify edit [flags] [path]` starts `schemify-gui` (§7) with the same flags.

`require "a.b"` loads `<root>/lua/a/b.lua`, else `<root>/lua/a/b/init.lua`, once (the result is cached like Lua's `package.loaded`). A module name is dot-separated segments of `A–Z a–z 0–9 _ -`; anything else (`..`, `/`, an absolute path) is an error. There is no `package` table, no `package.path` and no C module.

## 3. The `schemify` API

The init file and every function it registers run in one Lua 5.4 state that only the config crate touches. Everything the API sets lands in the **user-config table** (§6); the binaries read that table and never the Lua state.

| Call | Effect |
|---|---|
| `schemify.opt.<key> = value` | Sets an option; reading `schemify.opt.<key>` returns its value. Keys and types below; an unknown key or a wrong type is an error. |
| `schemify.keymap.set(chord, target, opts?)` | Binds `chord` (the keymap record syntax, `Ctrl+Shift+N`, `F5`, `w`) to `target`: a command name ([plugin-abi.md](plugin-abi.md) §7, the D27 command table) or a Lua function. `opts.desc` is a description string. A later `set` of the same chord replaces the earlier one. |
| `schemify.cmd(name, args?)` | Queues command `name` with the argument string `args` (default empty). The editor runs queued commands in order at its next delivery; the core has no command table yet and ignores them. |
| `schemify.on(event, fn)` | Calls `fn(ev)` after each `event` the editor delivers to plugins. Events (plugin-abi §6 wire words): `load` (the editor is ready), `doc_opened`, `doc_closed`, `doc_changed`, `selection_changed`, `saved`, `run_done`. `ev` holds the event's fields (`ev.doc`, `ev.epoch`, `ev.job`). Hooks run in registration order. |
| `schemify.sim(name, { path = , args = })` | The external programs of [run.md](run.md) §5: `ngspice` (the simulator command line; `{netlist}` and `{raw}` are replaced by the job's files), `openvaf`, `iverilog` (compilers; `args` unused). `path` is required; `args` is a list of strings. Another name is an error. |
| `schemify.notify(msg, level?)` | Shows `msg`: `level` is `"error"`, `"warn"` or `"info"` (default). |
| `schemify.pack.add { spec, … }` | Declares user packages (§5). Each call appends; a later spec with the same name replaces the earlier one. |

Options:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `snap` | number > 0 | 10 | Snap step, schematic units (xschem `cadsnap`). |
| `grid` | number > 0 | 20 | Grid step, schematic units. |
| `show_grid` | boolean | true | Draw the grid. |
| `light` | boolean | false | Light canvas colours. |

Steps are stored in milli-units (D33), rounded to the nearest; a step that rounds to 0 or exceeds 10^6 units is an error. The editor reads the options once, at startup.

Functions (keymap targets, hooks) run under the same sandbox and limits as the init file (§4), one budget per call. A function's error is reported as `<file>:<line>: <message>` and the editor carries on.

## 4. Sandbox

- **Present:** the base library without `dofile`, `loadfile`, `load`, `collectgarbage` and `require` (replaced by §2's); `string`, `table`, `math`, `utf8`. `print(...)` is an info notification (`schemify.notify`). `pcall` and `xpcall` return the error as its message string and never catch budget exhaustion (otherwise `while true do pcall(f) end` would outlive any budget).
- **Absent:** `io`, `os`, `debug`, `package` (so no `package.loadlib`), `coroutine`, binary chunks. Nothing in the config can open a file other than its own modules, start a process or reach the network (D7); packages are wasm and get capabilities only through grants (§5).
- **Instruction budget:** 10^8 Lua VM instructions per call (the init file with everything it requires, or one function call), counted by a hook every 1000 instructions, like plugin fuel. Exhaustion stops the call with `instruction budget exhausted`.
- **Memory cap:** 64 MiB for the whole Lua state (allocator limit). An allocation past it fails with `not enough memory` and the call stops.

A runaway config is reported and stopped; it never hangs the tool. Known ceiling: a single C library call (a pattern match over a huge string) runs between hook checks; the memory cap bounds the string it can build.

On the web the config is not run in this version (no Lua build for `wasm32-unknown-unknown` yet); see `docs/BACKLOG.md`.

## 5. Packages

A package is a prebuilt wasm plugin ([plugin-abi.md](plugin-abi.md)). Packages never run native code: there are no build hooks.

### Spec

`schemify.pack.add` takes a list of specs (tables), lazy.nvim-style:

| Field | Meaning |
|---|---|
| `src` | Git repository: anything `git clone` accepts; a path starting `./`, `../` or `~/` is relative to the config root (or the home directory). |
| `url` + `sha256` | One `.wasm` file fetched with `curl`; `sha256` (64 lowercase hex digits) is required and checked before anything is installed. |
| `path` | A local package directory used in place (development); never copied. |
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

### Capabilities

A loaded package gets the **intersection** of what it requests and what the user grants (D7). `schemify pack status` reports both differences, requested but not granted (`denied`) and granted but not requested (`unused`); the editor's load notes name the denied ones.

### Lockfile

`<root>/schemify-pack.lock`, next to the init file, in the lockfile row form of [format.md](format.md) §4: `locked plugin <name> <kind> <resolved> <sha256>`, sorted by name. `<kind>` is `git`, `url` or `path`; `<resolved>` is the commit, the url or the absolute path; `<sha256>` is the sha256 of the package's `.wasm`. Tool-written only; meant to be committed with the init file.

### Commands

`schemify pack <command>`; exit 0 on success, 1 if any package failed, 2 on a usage error or an unusable config.

| Command | Does |
|---|---|
| `sync` | Installs missing packages — at the locked commit when the lockfile has one, else at `rev`/`version` or the default branch — and writes the lockfile rows of every listed package. Installed packages are not moved. |
| `update` | Fetches every `git` package and moves it to `rev`/`version` (or the default branch's latest commit), re-hashes every package, rewrites the lockfile. |
| `restore` | Makes every listed package exactly what the lockfile says: checks out the locked commit, re-fetches a `url` whose file is missing; a hash that still differs from the lock is an error. Packages without a lock row are installed as by `sync`. |
| `clean` | Deletes every directory under `<data>/pack/` that no spec names and drops lockfile rows for unlisted packages. |
| `status` | One line per package: name, kind, resolved source, `ok` / `missing` / `changed` (hash ≠ lock) / `unlocked`, and the `denied` and `unused` grants. |

Fetching: `git` sources by running `git` (`clone`, `fetch`, `checkout`, `rev-parse`), `url` sources by running `curl -fsSL -o <tmp> <url>`, as lazy.nvim and `vim.pack` do; no HTTP library is linked. A downloaded file whose sha256 differs from the spec is deleted and the package fails with nothing installed.

## 6. Load order and the user-config table

Plugin composition order (D8, last wins): **bundled default UI < user packages (spec order) < project packages (manifest order)**, then the user's own keymaps (`schemify.keymap.set`) last.

Why: D8 makes the more specific contribution win. User packages are the user's general preferences, so they override the bundled defaults; a project's packages are what that design needs (its netlister, its PDK helpers), so they override the user's general set, the way a project's `.editorconfig` overrides an editor's defaults. Keymaps typed in `init.lua` are the user's explicit muscle memory and are applied after every plugin, as Neovim users expect from mappings made after plugins load.

The user-config table (owner: `crates/config`; readers: both binaries):

| Table | Rows | Capacity |
|---|---|---|
| options | the four keys of §3 | fixed |
| keymaps | chord, target (command name or function id), description | 4096 |
| sims | `ngspice`, `openvaf`, `iverilog`: path, args | 3 |
| packages | the specs of §5 | 128 |
| hooks | event word, function id | 1024 |
| requests (output of each call) | queued commands, notifications, errors | 256 per call |

## 7. Binaries

`schemify` is the headless core: CLI, config, package manager. It links no GPU, window, text-shaping, accessibility or LaTeX code (D39, checked by `crates/cli/tests/deps.rs`). `schemify edit …` runs `schemify-gui` from the directory of the running `schemify`, else from `PATH`, passing the remaining arguments; when neither exists it exits 2 saying so.
