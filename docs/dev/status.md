# Status (handoff)

Where the project stands. Update this file whenever a workstream lands or stops. Open items live in [docs/dev/backlog.md](backlog.md); decisions in [docs/dev/decisions.md](decisions.md).

## Branches

- `main`: integration branch, pushed to `origin` (git@github.com:OmarSiwy/Schemify.git, public); `m1` is its pre-remote history.
- In-flight work happens on branches, merged into `main` by the integrator (no long-lived worktrees).

## Done on `main`

| Area | State |
|---|---|
| Research + decisions | D1–D40 + amendments, `docs/dev/architecture.md`, `docs/dev/testing.md`, `docs/dev/contributing.md` |
| Specs | `docs/spec/`: format, expr, netlist, edits, import, run, plugin-abi, docs-page |
| `format` | line-record codec, canonical writer, `merge3`, `diff` |
| `expr` | the expression language, scalar + calculator, templates, deterministic `libm` math |
| `doc` | design tables, edits/undo/redo, journal + crash recovery, external merge, project/libraries/lockfile, the bundled `devices/` devices |
| `elab` | connectivity, hierarchy elaboration + impl selection + subckt specialization, params + gating, checks, annotations |
| `netlist` | ngspice, xyce, cdl, spectre, verilog emitters + name map |
| `run` | job expansion, ngspice runner, results, sweep/corners/MC/optimizer |
| `wave` | `.raw` reader (ngspice/Xyce/LTspice), calculator binding, viewer tables, SIMD decimation |
| `abi` + `plugin` | wasmi host, capabilities, fuel, last-wins composition, guest SDK |
| `ui` + `render` | UI-as-data toolkit (taffy/parley/AccessKit/RaTeX), vello renderer, waves window |
| `app` | GUI editor (`schemify edit`): xschem editing, default-UI plugin, docs pane, live checks + panel, net highlight, netlist (`n`), non-blocking simulate (`s`) with results table, waves dock + cross-probe, OP annotation, optimizer → staged edit; CLI: `netlist check run fmt diff merge-driver git-setup` (shared `spines` module) |
| Plugins | `default-ui` (embedded); the rest are standalone repositories since 2026-10-18 (submodules in `examples/plugins/`, docs/plugins/): `interop` (2026-10-12: the clean-room xschem importer moved out of the core: xschem translation, PDK limits (model bins, `drc=`, tech grid, cdfDump); native CLI `schemify-interop xschem-import`; wasm guest builds, no commands yet), `gmid-lut` (gm/Id functions from ngspice-characterized LUTs), `cktimg` (SPICE → schematic laid out by cktImg's Zig engine; round trip spicecmp-equivalent on 79/80 fixtures and 296/301 goldens, + annotated xfails; wasm guest byte-identical to native, 69/80 fixtures within the default fuel), examples `hello`/`bad-actor`/`gmid-stub` |
| Corpus | moved with the interop plugin (Schemify-Interop `tests/corpus/`): fetch + 602 xschem goldens (`.cache/`, gitignored), `tools/spicecmp`, ratchet in its `RATCHET.md` (SPICE 246/301, CDL code-stripped 227/301; +23/+32 golden-unevaluated tracked separately) |
| `cli` + `config` | Headless core `schemify` (2.57 MB; D39 budget 2 MB) and editor `schemify-gui` (16.0 MB; budget 10 MB), split so the core links no GPU/window/text crate (`cli/tests/deps.rs`); Lua 5.4 user config (`init.lua`, sandboxed `schemify.*` API, Neovim `stdpath` dirs) and `schemify pack sync\|update\|restore\|clean\|status` (config spec). No Lua on the web yet |
| xschem parity | Every default-UI command resolves (`app/tests/parity.rs`: 198 → 0; xschem-only items dropped with reasons in BACKLOG); `shape` records (line/rect/arc/circle/poly); PNG/SVG/PDF export; stretch moves; context menu; search/replace; system clipboard; crash-recovery prompt |
| CI | `ci.yml` (fmt/clippy/tests on Linux/macOS/Windows, web bundle + headless-Chrome smoke, cross-OS + WASI determinism, core guests via Nix, flake, non-blocking size gate), `nightly.yml` (fuzz, perf gate), `mutants.yml` (weekly), `release.yml` (`v*` tags) |
| Workspace tests | 620 passed, 0 failed (`cargo test --workspace --no-fail-fast`, 2026-10-11, after the CI, parity and core/editor merges) |
| Nix | `flake.nix`: `nix run` (wrapped editor, 23.0 MiB stripped/LTO/panic=abort, D39 ≤ 25 MB), `nix develop` (toolchain, simulators, GUI libs) |
| CI (D32, D37) | `.github/workflows/`: `ci.yml` (merge tier: fmt/clippy/tests on Linux, macOS, Windows; wasm32 web build + bundle + headless-Chrome smoke test; WASI CLI; byte-identical CLI outputs across the four; Nix build of the core guests (ui/build.sh) with size limits; `nix flake check` + `nix build`), `nightly.yml` (cargo-fuzz on the core harnesses, perf tests + 10% regression gate on `tests/perf/baseline.txt`), `mutants.yml` (weekly), `release.yml` (tag `v*`: three native archives + web bundle on a draft release with the checklist). Toolchain pinned to 1.95.0 (= MSRV = flake.lock rustc). Details: docs/dev/testing.md § Tiers |

## In flight (2026-10-11): every gap in docs/dev/backlog.md has an owner; PR agents build only in CI

| Workstream | Branch | Scope |
|---|---|---|
| Performance (local) | `pr/perf-*` | merge of main, tiles + LOD (p99 ≤ 2 ms), app-side edit cost ≤ 0.5 ms, render text cache / exit hook |
| Incremental elab (local) | `pr/elab-incremental` | per-net connectivity + checks (≤ 0.5 ms/edit), flat 1M netlist ≤ 0.5 s, elab correctness + xschem bus broadcast |
| web (PR) | `pr/*` | OPFS, Lua on the web, clipboard/exports, web runner, CPU render fallback |
| api (PR) | `pr/*` | JSON-RPC (D27), plugin `eval`, plugin events on a worker, plugin correctness |
| netlist (PR) | `pr/*` | D17 code in `.shcomp`, import/netlist corpus gaps, spicecmp, Verilog oracle |
| editor (PR) | `pr/*` | ui widgets, edit gestures, exports, make-subcircuit, join/trim, packages view |
| run (PR) | `pr/*` | MC seeds, Verilog-A/Verilog bodies, optimizer corners, results table, staged edits |
| docs (PR) | `pr/*` | plot overlays, write-back, prose, incremental cell state, gmid-lut |
| waves (PR) | `pr/*` | cursors, digital/bus, histogram, properties, FFT axis |
| core (PR) | `pr/*` | doc/expr API requests, merge checks, reproducible plugins, fuzz corpora, coverage, sky130 oracle, release |
| cktimg (PR) | `pr/*` + upstream PRs | cktImg patches upstream, block geometry, XSPICE cards, one cell per event |

## Next

0. Push `main` (blocked: ssh-agent down; see the session log) and watch the first full CI run; seed `tests/perf/baseline.txt` from two nightly runs.
1. Upstream the cktImg patches in `docs/dev/upstream/` (user), bump the submodule; set `fuel` for cktimg in project manifests that import large decks.
2. Editor parity gaps (BACKLOG "app / editor"): stretch/kissing move, poly/arc/circle, context menu, search, Save As, scrolling lists, system clipboard, crash-recovery prompt, toolbar icons.
3. Web build end to end: OPFS VFS (D31; the web build edits in memory today), runner plugin.
4. Remaining BACKLOG items by crate.

## Run it

```sh
nix develop            # toolchain, simulators, xschem, GUI libs
cargo run -p schemify-app -- edit crates/app/tests/fixtures/ota/ota.shcomp   # docs pane + gm/Id
schemify netlist <cell> [--dialect cdl]    # headless
nix run                # packaged editor (once flake.nix is committed)
```
