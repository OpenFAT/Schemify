# Status (handoff)

Where the project stands. Update this file whenever a workstream lands or stops. Open items live in [BACKLOG.md](BACKLOG.md); decisions in [DECISIONS.md](DECISIONS.md).

## Branches

- `main`: integration branch, pushed to `origin` (git@github.com:OmarSiwy/Schemify.git, public); `m1` is its pre-remote history.
- In-flight work happens in agent worktrees under `.claude/worktrees/agent-<id>` on branches `worktree-agent-<id>`, merged into `main` by the integrator.

## Done on `main`

| Area | State |
|---|---|
| Research + decisions | `docs/research/*`, `docs/xschem-ui/` (42 xschem captures), D1–D40 + amendments, `ARCHITECTURE.md`, `TESTING.md`, `AGENTS.md` |
| Specs | `docs/spec/`: format, expr, netlist, edits, import, run, plugin-abi, docs-page |
| `format` | line-record codec, canonical writer, `merge3`, `diff` |
| `expr` | the expression language, scalar + calculator, templates, deterministic `libm` math |
| `doc` | design tables, edits/undo/redo, journal + crash recovery, external merge, project/libraries/lockfile |
| `elab` | connectivity, hierarchy elaboration + impl selection + subckt specialization, params + gating, checks, annotations |
| `netlist` | ngspice, xyce, cdl, spectre, verilog emitters + name map |
| `import` + `lib/core` + `tools/devicereader` | clean-room xschem translation, bundled `core` devices, PDK limits (model bins, `drc=`, tech grid, cdfDump) |
| `run` | job expansion, ngspice runner, results, sweep/corners/MC/optimizer |
| `wave` | `.raw` reader (ngspice/Xyce/LTspice), calculator binding, viewer tables, SIMD decimation |
| `abi` + `plugin` | wasmi host, capabilities, fuel, last-wins composition, guest SDK |
| `ui` + `render` | UI-as-data toolkit (taffy/parley/AccessKit/RaTeX), vello renderer, waves window |
| `app` | GUI editor (`schemify edit`): xschem editing, default-UI plugin, docs pane, live checks + panel, net highlight, netlist (`n`), non-blocking simulate (`s`) with results table, waves dock + cross-probe, OP annotation, optimizer → staged edit; CLI: `import netlist check run fmt diff merge-driver git-setup` (shared `spines` module) |
| Plugins | `default-ui` (embedded), `gmid-lut` (gm/Id functions from ngspice-characterized LUTs), `cktimg` (SPICE → schematic laid out by cktImg, `extern/cktimg`; round trip spicecmp-equivalent on 79/80 fixtures and 296/301 goldens, + annotated xfails; wasm guest byte-identical to native, 69/80 fixtures within the default fuel), examples `hello`/`bad-actor`/`gmid-stub` |
| Corpus | `tests/corpus/` fetch + 602 xschem goldens (`.cache/`, gitignored), `tools/spicecmp`, ratchet in `tests/corpus/RATCHET.md` (SPICE 246/301, CDL code-stripped 227/301; +23/+32 golden-unevaluated tracked separately) |
| `cli` + `config` | Headless core `schemify` (2.57 MB; D39 budget 2 MB) and editor `schemify-gui` (16.0 MB; budget 10 MB), split so the core links no GPU/window/text crate (`cli/tests/deps.rs`); Lua 5.4 user config (`init.lua`, sandboxed `schemify.*` API, Neovim `stdpath` dirs) and `schemify pack sync\|update\|restore\|clean\|status` (config spec). No Lua on the web yet |
| xschem parity | Every default-UI command resolves (`app/tests/parity.rs`: 198 → 0; xschem-only items dropped with reasons in BACKLOG); `shape` records (line/rect/arc/circle/poly); PNG/SVG/PDF export; stretch moves; context menu; search/replace; system clipboard; crash-recovery prompt |
| CI | `ci.yml` (fmt/clippy/tests on Linux/macOS/Windows, web bundle + headless-Chrome smoke, cross-OS + WASI determinism, plugins via Nix, flake, non-blocking size gate), `nightly.yml` (corpus ratchet, fuzz, perf gate), `mutants.yml` (weekly), `release.yml` (`v*` tags) |
| Workspace tests | 620 passed, 0 failed (`cargo test --workspace --no-fail-fast`, 2026-10-11, after the CI, parity and core/editor merges) |
| Nix | `flake.nix`: `nix run` (wrapped editor, 23.0 MiB stripped/LTO/panic=abort, D39 ≤ 25 MB), `nix run .#devicereader`, `nix develop` (toolchain, simulators, xschem, GUI libs) |
| CI (D32, D37) | `.github/workflows/`: `ci.yml` (merge tier: fmt/clippy/tests on Linux, macOS, Windows; wasm32 web build + bundle + headless-Chrome smoke test; WASI CLI; byte-identical CLI outputs across the four; Nix plugin build with size limits; `nix flake check` + `nix build`), `nightly.yml` (xschem goldens + differential ratchet against `RATCHET.md`, cargo-fuzz on the four harnesses, perf tests + 10% regression gate on `tests/perf/baseline.txt`), `mutants.yml` (weekly), `release.yml` (tag `v*`: three native archives + web bundle on a draft release with the checklist). Toolchain pinned to 1.95.0 (= MSRV = flake.lock rustc). Details: TESTING.md § Tiers |

## In flight (resumed 2026-10-10)

| Workstream | Branch | Scope |
|---|---|---|
| Performance (D39 frames) | `worktree-agent-a5a13d952a4c2592a` | world-space scene + camera transform, tile culling/LOD, incremental connectivity and checks, flat 1M netlist |

## Next

0. Push `main` (blocked: ssh-agent down; see the session log) and watch the first full CI run; seed `tests/perf/baseline.txt` from two nightly runs.
1. Upstream the cktImg patches in `docs/upstream/` (user), bump the submodule; set `fuel` for cktimg in project manifests that import large decks.
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
