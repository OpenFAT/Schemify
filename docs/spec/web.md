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
- No user config yet: see §5.

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

Not run on the web in this version (config spec §4). Evaluation (2026-10-11):

- *C Lua 5.4 for `wasm32-unknown-unknown`*: `lua-src` 551.0.2 (mlua's `vendored` build) refuses the target (`don't know how to build Lua for wasm32-unknown-unknown`; only `-emscripten` and `-wasi` have branches, and `mlua-sys` panics without `vendored` on wasm32). A build would need a C toolchain for the target, a libc shim (the `printf` family with exact float formatting, `strtod`, string and math functions) and `setjmp`/`longjmp`, which clang lowers to the wasm exception-handling proposal only with a runtime (`__wasm_setjmp`, `__wasm_longjmp`) that wasi-libc ships and this target lacks — C code outside the `forbid(unsafe_code)` rule, plus a patched `lua-src` in the root manifest.
- *Pure-Rust Lua*: `piccolo` 0.3.3 (the candidate named in BACKLOG) lacks most of the standard library the config API and real configs use (`tonumber`, `string.rep/format/find/gsub/match/byte/char`, `table.insert/concat/remove/sort`, `utf8`), and its `rand` dependency pulls `getrandom`, which does not build for this target without a JS backend feature. `omnilua` 0.7.1 passes the PUC-Rio 5.4 suite and builds for the target, but it is four months old, has one maintainer and churned 17 releases in six weeks, its API is not mlua's (the 770-line runtime would be rewritten), and identical behaviour on both targets would mean running it natively too, replacing the vendored C Lua: a dependency and runtime decision for the user (AGENTS rule 7), not part of this change.

## 6. Simulation

D16: runner plugins. Not wired yet (the editor's run spine is native-only); see BACKLOG.

## 7. Rendering

WebGPU through vello, as natively. Without an adapter (headless Chrome in CI) the canvas stays blank; a CPU fallback is pending (BACKLOG).

## 8. Tests

- `crates/cli/src/platform/vfs.rs`: the table against `std::fs` on random operation scripts (reads, existence, directories, listings with hidden and skipped subtrees), the flush order, the byte budget, import planning (top directory, names, collisions, unsafe paths).
- `crates/cli/src/zip.rs`: an archive written by CPython's `zipfile` (deflated + stored + a directory entry), round trips and determinism, the CRC check value, truncation and byte flips (errors, never a panic), the output cap.
- `crates/app/tests/parity.rs` `export_project_zips_the_listed_files`: the archive equals the listed files byte for byte, hidden entries out, same bytes twice.
- `tests/web/smoke.sh` + `drive.py` (CI `wasm` job, headless Chrome over the DevTools protocol): the editor starts without a trap; a wire drawn and saved reaches OPFS and survives a reload, and the reloaded editor sees it (its next new cell is `untitled2`); Ctrl+C reaches the system clipboard and, after the reload, Ctrl+V pastes from it.
