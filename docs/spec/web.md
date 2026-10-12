# Web platform v1 (normative)

Implements D2 (generated glue only), D31 (OPFS storage, projects in and out by zip or folder), D32 (web is tier 1) and D16 (simulation through runner plugins). The editor and the core are the same Rust code as natively; only the OS boundary differs: `crates/cli/src/platform.rs` (public API, both targets), `platform/web.rs` (browser glue: web-sys/js-sys, no hand-written JS), `platform/vfs.rs` (the in-memory file table, pure, tested natively), `crates/cli/src/zip.rs` (archives). The page is `crates/app/web/index.html`: a one-line loader of the wasm-bindgen bundle.

## 0. Usage first

```
https://host/schemify/                               the default project /projects/default
https://host/schemify/#/projects/inv                 the project /projects/inv
https://host/schemify/#/projects/inv/top.shcomp      that cell in its project
File → Import project (zip) | Import project folder  pick → /projects/<name> → reload on it
File → Export project (zip)                          download <name>.zip
File → Image export → …                              download top.svg / top.pdf / …
Ctrl+C / Ctrl+V                                      the system clipboard
```

## 1. Start

- The URL fragment is the editor's one argument (the page has no command line): a project directory or a cell file, percent-decoded (`platform::args`). No fragment opens `/projects/default` (`platform::WEB_ROOT`).
- Before the editor starts, the storage is mounted (§2) and an unload guard installed; then the editor runs exactly as natively (`Editor::new` → `render::run`).
- One project per page, as one per native process: importing a project reloads the page on it (§3).
- The clock is the page's (`Date.now`): journal stamps, file times, double clicks and plugin timers use it. Object-id seeds mix `Math.random` and the clock (ids need uniqueness, not secrecy).
- The user config is `/config/init.lua` (§5).

## 2. Storage (OPFS)

Layout of the origin's OPFS root:

| Path | What |
|---|---|
| `/projects/<name>/` | one project per directory (`default` is the start project) |
| `/config`, `/data`, `/state`, `/cache` | the config spec §1 directories (`/state/recent`: recent files) |

**The boundary.** OPFS is asynchronous on the main thread, while every spine (load, save, journal, netlist, export) calls the platform synchronously. The options were:

1. *Synchronous access handles in a worker* (`createSyncAccessHandle`). Rejected: the handle exists only in dedicated workers ([Exposed=DedicatedWorker]). The editor must run on the main thread (winit's web backend drives the DOM canvas and its input events), so it would have to block on the worker's answer, and the main thread cannot block: `Atomics.wait` throws there, and the `SharedArrayBuffer` it needs requires cross-origin isolation headers (COOP/COEP) that static hosting cannot set. A worker also needs its own JS entry script, which D2 forbids.
2. *The smallest async boundary* (chosen): the page keeps every OPFS file in memory. The mount reads the tree once, before the editor starts; then reads, writes, listings and deletes are synchronous on the table, and changes go back to OPFS asynchronously.

Rules:

- **Mount.** Every file under the root, depth ≤ 32, is loaded (bytes and `lastModified`); Chrome's `*.crswap` temporaries are skipped. Limits: 2^18 files, 1 GiB in total (`vfs::FILES_MAX`, `BYTES_MAX`); past them the mount stops with an error. A browser without OPFS (or one that refuses it) leaves the page editing in memory: the console says so and nothing persists.
- **Semantics** equal the native platform's for every call the spines make (the oracle: `vfs` tests run random scripts against `std::fs`): writes create parent directories; a write onto a directory, a directory under a file, and the removal of a missing file or a directory fail; listings visit each directory's children in name order, skip hidden names (`.x`) and `target`, and stop at the native bounds. Paths are absolute; a relative path is taken from `/`.
- **Write-back.** Each change marks its path stale; one flusher task drains the stale paths in the order of each path's *last* change and writes the path's current bytes (`createWritable` → `write` → `close`, which swaps the file in whole) or removes it. A path changed again while queued moves to the back, so OPFS sees the final states in the order they were made: a save that writes a cell and then drops its journal reaches OPFS in that order, and a crash between the two leaves the journal (recovery), never a lost save.
- **Durability.** A write is visible to the page at once and reaches OPFS milliseconds later. While changes are pending, or after any write failed this session, closing or reloading the page asks first (`beforeunload`). A failed write is reported on the console.
- Ceiling (`ponytail`): the whole tree lives in memory, and a changed file is rewritten whole (a journal append rewrites the journal). Upgrades: mount only the open project with `/config` and `/state`; append through `createWritable({keepExistingData})` + `seek`.

## 3. Projects in and out; exports

**Import** (`file.import_project` zip, `file.import_project.folder` folder; web only, inside the menu click's user activation): the browser's file picker (`<input type=file>`, `webkitdirectory` for a folder) → the files are read; a zip is unpacked (stored and deflated entries, every CRC checked; no zip64 or encryption) →

- `__MACOSX/` entries are dropped;
- an entry path that is absolute or has an empty, `.` or `..` segment, a `\`, `:` or NUL refuses the whole import (a trust boundary: nothing is written);
- a top directory shared by every entry is stripped and names the project; otherwise the zip's file stem does;
- the name is reduced to `A–Z a–z 0–9 . _ -` (others become `_`, leading dots dropped, ≤ 64 characters, empty → `project`); when `/projects/<name>` exists, `-2` … `-99` is appended;
- limits: 65 536 files and 512 MiB per import;
- the files are written to `/projects/<name>/…`; when all reached OPFS (≤ 30 s) the page reloads on `#/projects/<name>`. Natively the command only reports (a native project is a directory: open it).

**Export** (`file.export_project`): every file `list_files` lists under the project root (so `.schemify/`, `.git` and other hidden entries stay out), as `<name>/<relative path>` in list order, stored with the DOS epoch as time stamp, so the same project gives the same bytes on every platform. Natively written to `<root>/.schemify/export/<name>.zip`; on the web downloaded as `<name>.zip`.

**Image exports** (PNG/SVG/PDF/EPS) and the project zip go through `platform::export`: natively a file write, on the web a download named after the file (`Blob` + object URL + `<a download>`; the previous download's URL is released by the next) and nothing is written to OPFS.

## 4. Clipboard

The Clipboard API, secure contexts only (HTTPS or localhost):

- **Copy / cut** write the fragment text (`clip` module: a complete `.shcomp` fragment) with `navigator.clipboard.writeText`; a refusal is a console warning and the editor keeps its own copy.
- **Paste** needs `readText`, which answers later (and may ask the user for permission). The first `clipboard_get` starts the read and returns `CLIPBOARD_PENDING`; the editor's paste then does nothing. When the answer arrives `clipboard_ready()` is true once, the editor queues `edit.paste` again, and that paste takes the answer: the system text when it parses as a fragment, else the editor's own clipboard (as natively). While the read is in flight the editor asks for a tick every 20 ms (`platform::wake_ms`); otherwise it sleeps (D39).
- Text is capped at 64 MiB (`CLIPBOARD_BYTES_MAX`).

## 5. User config (Lua)

The init file is OPFS `/config/init.lua`, its modules `/config/lua/` (config spec §1–§4), read through the platform (`Config::load_with`). The interpreter is omnilua, a pure-Rust Lua 5.4, on the web and natively alike (user decision 2026-10-12), so a config behaves the same everywhere; CI runs the config tests on `wasm32-wasip1` too, and the browser smoke test binds a key in `/config/init.lua` and uses it.

Why omnilua (evaluation of 2026-10-11):

- *C Lua 5.4 for `wasm32-unknown-unknown`*: `lua-src` 551.0.2 (mlua's `vendored` build) refuses the target; a build would need a libc shim (the `printf` family, `strtod`, string and math functions) and `setjmp`/`longjmp` over the wasm exception-handling proposal with a runtime this target lacks: C outside the `forbid(unsafe_code)` rule and a patched `lua-src`.
- *piccolo* 0.3.3 lacks most of the standard library real configs use (`tonumber`, most of `string` and `table`, `utf8`) and pulls `getrandom`, which needs a JS backend on this target.
- *omnilua* 0.7.1 passes the PUC-Rio 5.4 suite, has no C and no third-party dependency (eight `lua-*` crates of its own), and builds for every target. Its sandbox covers the config spec's contract: an instruction budget, a memory ceiling, stripped globals. Risk accepted with it: a young crate (four months, one maintainer); the config API sits behind `crates/config/src/lua.rs` and the prelude, so another runtime replaces one file.

## 6. Simulation

D16: runner plugins. **Designed here, not implemented yet**: the editor's run spine (`app::sim`, `cli::spines::Run`) is native-only, and the steps below fall in the run, plugin and app crates that open PRs (#5, #6, #7) are changing; BACKLOG "web" lists them by owner, in order.

Options:

1. *ngspice compiled to wasm, in the page.* Rejected. An emscripten build ships emscripten's JS runtime (D2). A `wasm32-wasi` build would need a WASI host inside the page: a preview1 shim of our own under wasmi, over the OPFS table. wasmi interprets (D3's ceiling: heavy compute stays in external tools), and a transient analysis is compute-bound. A simulator binary of several MB would also share D39's 8 MB compressed budget with the editor. D16 itself says no ngspice port.
2. *A runner plugin with `network`* (chosen; D16's example): the plugin sends the job's netlist to a simulation endpoint the user runs (a machine with ngspice and the PDK) and hands back the raw file.

Design (each job of a run, exactly where the native runner would start ngspice, run spec §5):

- **Runner choice.** The plugin whose composed `runners` contribution claims lang `spice` (last wins, D8). None: the run is refused at start, `no runner plugin for spice (D16)`.
- **Hand-off** (plugin-abi §6 amendment). The netlist is written to the job directory `.schemify/runs/<design>/<job>/netlist.cir` (OPFS) as natively, and the plugin gets `RunJob { job, lang, dir }` with the netlist bytes as the event's NUL payload (as `io_done`). Model files the netlist names (`.lib`, `.include`) are the endpoint's to resolve (its PDK install) or the plugin's to upload (`read_project`).
- **Transport.** The plugin's `fetch` (plugin-abi §5, grant `network`): natively a `curl` child process (as `pack` fetches), on the web `window.fetch`; `IoDone.status` = the HTTP status, or −`E_GONE` without a response; `bytes` = the body.
- **Result** (plugin-abi §7 amendment). The plugin writes the raw file to `<dir>/out.raw` (`fs_write`, grant `write_project`) and returns `job <job> done`, or `job <job> failed <message>`. The host takes `job` records after the delivery's file writes; `done` sends the raw file through `Results::record`, as a native job's, and `failed` (or the plugin trapping or unloading) fails the job with the message.
- **Limits.** At most `workers` jobs handed off at once (default 4 on the web) and the run's timeout (1 h) per job. Cancel fails every handed-off job (`cancelled`); a later `job` record for it is ignored.
- **Runner in the spine.** On the web `Run` holds a plugin runner with the native `Runner`'s five calls (`submit`, `poll`, `busy`, `cancel_all`, `save`), selected by `cfg` (ARCHITECTURE "Platform split"), so `sim` keeps one spine for both targets.
- **Reference plugin** `plugins/remote-runner`: the endpoint URL comes from its manifest record (`plugin remote-runner … endpoint <url>`). It POSTs the netlist to the endpoint and expects `200` with the ngspice raw file as the body. The smoke test serves a fake endpoint that returns a canned raw file.

## 7. Rendering

WebGPU through vello, as natively, unless WebGPU is unusable; then every frame is rasterized by `CpuPainter` (vello_cpu, the painter of the native goldens) and put with `putImageData` on a 2D canvas (`id="schemify-cpu"`, `pointer-events: none`) laid over winit's canvas, which keeps the input (`crates/render/src/cpu_view.rs`). A separate canvas because winit's already holds the `webgpu` context of the failed attempt. Cost: a full raster per frame; the editor redraws on input only (D39).

WebGPU is unusable when (a) the browser offers no adapter or device (browsers without WebGPU), (b) the device is lost for any reason but our own drop, or (c) the first presented frame does not reach the page: 250 ms after it, one pixel at the canvas centre read back through a 2D canvas (the WebGPU canvas readback returns the last presented frame) has alpha 0, though every frame starts from the opaque background. (c) is what headless Chrome does on SwiftShader (no error at all) and on Vulkan (the swap-chain image fails to allocate): every WebGPU call succeeds and the page stays blank. A readback that itself fails counts as shown (the probe switches only on evidence). The switch is one-way for the page and logged once on the console as `gpu: <reason>; drawing on the CPU`; the GPU objects are dropped. Input, storage and clipboard never depend on which painter draws.

The editor fits a cell's camera to the first non-empty canvas: the web canvas has no size until winit's first `Resized`, so the first updates see an empty viewport.

## 8. Tests

- `crates/cli/src/platform/vfs.rs`: the table against `std::fs` on random operation scripts (reads, existence, directories, listings with hidden and skipped subtrees), the flush order, the byte budget, import planning (top directory, names, collisions, unsafe paths).
- `crates/cli/src/zip.rs`: an archive written by CPython's `zipfile` (deflated + stored + a directory entry), round trips and determinism, the CRC check value, truncation and byte flips (errors, never a panic), the output cap.
- `crates/app/tests/parity.rs` `export_project_zips_the_listed_files`: the archive equals the listed files byte for byte, hidden entries out, same bytes twice.
- `tests/web/smoke.sh` + `drive.py` (CI `wasm` job, headless Chrome over the DevTools protocol): the editor starts without a trap; the `inv` fixture written into OPFS by the browser's own API opens, and when the editor draws on the CPU its frame matches the native golden `editor_dark.png` under the native test's tolerance (≤ 0.1% of pixels off by more than 2 levels); whichever painter draws, the page screenshot is within 5% of pixels of that golden (a blank page, the CI failure before the §7 probe, is ~100% off); a wire drawn and saved reaches OPFS and survives a reload; Ctrl+C of it reaches the system clipboard and, after the reload, Ctrl+V pastes it from there, and the next save holds the reloaded file plus the pasted wire. The page viewport is pinned to 1280×800 (`Emulation.setDeviceMetricsOverride`): `--window-size` includes the headless browser's own UI. CI's SwiftShader adapter shows no frames, so CI exercises the (c) fallback of §7.
- `crates/app/tests/editor.rs` `fits_when_the_first_viewport_is_empty`: updates at an empty viewport, then at 1280×800, give the camera of a session sized from the start.
