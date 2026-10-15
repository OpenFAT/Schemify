# Schemify in the browser

The browser build is the same editor as the desktop one, compiled to WebAssembly. You draw, edit, check and netlist schematics, and your files stay in the browser's private storage. Simulation is the large gap: it does not run in the browser yet (see [What is missing](#what-is-missing-compared-with-the-desktop)).

## Getting the bundle

Release builds include a `schemify-<version>-web.tar.gz`. Unpack it anywhere: it holds `index.html` and a `pkg/` directory with the WebAssembly module and its loader.

To build it yourself from a checkout, you need the `wasm32-unknown-unknown` Rust target and a `wasm-bindgen` CLI whose version equals the `wasm-bindgen` crate in `Cargo.lock` (`nix develop` provides both):

```sh
rustup target add wasm32-unknown-unknown
grep -A1 '^name = "wasm-bindgen"$' Cargo.lock      # the version your CLI must have
cargo install wasm-bindgen-cli --version <that version>

cargo build --release --target wasm32-unknown-unknown -p schemify-app --bin schemify-gui
wasm-bindgen --target web --no-typescript --out-name schemify --out-dir crates/app/web/pkg \
  target/wasm32-unknown-unknown/release/schemify-gui.wasm
```

The bundle is now `crates/app/web/` (`index.html` plus `pkg/`).

## Serving it

Any static file server works. The page needs no special headers:

```sh
python3 -m http.server 8000 --bind 127.0.0.1 --directory crates/app/web
```

Open `http://127.0.0.1:8000/`. On another machine, serve it over HTTPS: browsers give file storage and the clipboard only to secure contexts (HTTPS, or `localhost` and `127.0.0.1`).

## Browser requirements

- WebGPU draws the canvas when the browser offers it. Without a usable WebGPU device, the editor draws every frame on the CPU instead and logs `gpu: <reason>; drawing on the CPU` once in the browser console. Everything else works the same; the CPU path costs more per frame, and the editor redraws only on input.
- The Origin Private File System (OPFS) stores your projects. A browser without it, or one that refuses it, still opens the editor, but edits live in memory only: the console says so and nothing survives a reload.
- The asynchronous Clipboard API carries copy and paste between the editor and other applications. The browser may ask for permission the first time you paste.

## Opening a project

The part of the URL after `#` is the editor's only argument, as a path in browser storage:

```text
http://127.0.0.1:8000/                              the default project, /projects/default
http://127.0.0.1:8000/#/projects/inv                the project /projects/inv
http://127.0.0.1:8000/#/projects/inv/top.shcomp     that cell, in its project
```

One page holds one project, as one desktop process does.

## Where your files live

Everything is in the site's OPFS storage, laid out like this:

| Path | Holds |
|---|---|
| `/projects/<name>/` | one project per directory; `default` is the start project |
| `/config/` | your user config: `init.lua` and its modules in `/config/lua/` |
| `/data/`, `/state/`, `/cache/` | the same data, state and cache directories the desktop editor keeps (`/state/recent` lists recent files) |

The editor reads the whole storage once when the page loads, then works on that copy. A save is visible to the editor at once and reaches OPFS a few milliseconds later. While writes are still pending, or after a write failed, closing or reloading the tab asks you to confirm first. A failed write is reported in the browser console only, not in the status line, so open the console if a save seems not to stick.

Storage is capped at 1 GiB and 262,144 files in total. Past that, loading storage fails: the console says `web storage: ...; edits stay in this page (nothing persists)` and the editor runs without saving anything. Storage belongs to the site's origin: clearing site data for it deletes your projects. Export a zip before you do.

## Projects in and out

Projects move in and out of the browser as zip files or folders, all from the File menu:

| Menu item | Effect |
|---|---|
| File → Import project (zip) | Pick a `.zip`; it is unpacked into `/projects/<name>` and the page reloads on it. |
| File → Import project folder | Pick a folder; same as the zip. |
| File → Export project (zip) | Downloads `<name>.zip` of the open project. |
| File → Image export → PNG Export, SVG Export, PDF/PS Export | Downloads the image. Nothing is written to browser storage. |

On import, a top directory that every entry shares is stripped and names the project; otherwise the zip's file name does. The name keeps only letters, digits, `.`, `_` and `-` (others become `_`). If `/projects/<name>` exists, `-2`, `-3` and so on are appended, so an import never overwrites a project. `__MACOSX/` entries are dropped. An entry with an absolute path or a `..` segment refuses the whole import, and nothing is written. One import takes at most 65,536 files and 512 MiB. Zips must use stored or deflated entries; zip64 and encrypted archives are refused.

The exported zip holds the files the editor lists in the project, under a top directory named after it. Hidden entries such as `.schemify/` (netlists, run results, the undo journal) and `.git` are left out. The same project gives the same zip bytes on every platform, so you can export from the browser, unzip, and open the directory with `schemify edit` on the desktop, or zip a desktop project and import it here.

## Copy and paste

`Ctrl+C` and `Ctrl+X` put the selection on the system clipboard as Schemify text, so you can paste it into another tab, the desktop editor or a text file. `Ctrl+V` reads the system clipboard. The first paste may wait for the browser's permission prompt: the editor pastes as soon as the answer arrives. If the clipboard holds something that is not Schemify text, the editor pastes its own last copy instead.

## Your Lua config

The browser runs the same Lua 5.4 config as the desktop, from `/config/init.lua` in browser storage, so one `init.lua` behaves the same everywhere. [config.md](config.md) documents the API.

The editor has no command to create or edit that file. Write it once with the browser's own storage API, from the developer console of the Schemify tab:

```js
const root = await navigator.storage.getDirectory();
const dir = await root.getDirectoryHandle("config", { create: true });
const file = await dir.getFileHandle("init.lua", { create: true });
const w = await file.createWritable();
await w.write('schemify.keymap.set("F9", "file.save")\n');
await w.close();
```

Then reload the page. The editor reads storage only at load, so a change you make this way takes effect on the next reload.

## Simulation on the web

Pressing `s` in the browser shows `simulate: no simulator on the web (D16: runner plugins)` in the status line. ngspice is not compiled into the page, and the browser cannot start programs. The planned route is a runner plugin that sends each job's netlist to a simulation server you run (a machine with ngspice and your PDK) and brings the raw file back. That plugin and the editor support for it are designed but not built yet.

Until then, netlist in the browser (`n` works, and the checks panel works) and simulate on the desktop: export the project as a zip, then run it with `schemify run` or the desktop editor ([simulation.md](simulation.md)).

## What is missing compared with the desktop

- Simulation, and everything built on its results: the results table, the waves dock fed by a run, operating-point annotation and the optimizer.
- Command-line options. The URL fragment is the only argument: there is no `--clean`, `-u <init.lua>` or `--no-default-ui`.
- The JSON-RPC command stream (`schemify rpc`) and the MCP bridge.
- User packages. `schemify pack` is a desktop command, so packages named in `init.lua` cannot be installed from the browser. Plugins listed in a project's `schemify.project` with a `path` source load from the project's files as on the desktop.
- Plugins that start programs. Anything that needs a `spawn:` grant fails in the browser, which rules out the `pdk-manager` plugin (it runs `ciel` and `schemify-interop`).
- Responsiveness for heavy plugin jobs. A plugin with a raised `fuel` budget runs on a worker thread on the desktop, but in the browser it runs on the page's only thread, so a large `cktimg` import freezes the tab until it finishes.
- Storage error reporting in the editor itself: OPFS failures show only in the console.

See also: [install.md](install.md), [config.md](config.md), [files.md](files.md), [the web spec](../spec/web.md).
