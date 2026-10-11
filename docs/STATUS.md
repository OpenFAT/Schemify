# Status (handoff)

Where the project stands. Update this file whenever a workstream lands or stops. Open items live in [BACKLOG.md](BACKLOG.md); decisions in [DECISIONS.md](DECISIONS.md).

## Branches

- `m1`: integration branch. Every merged workstream lives here; `main` has no commits yet.
- In-flight work happens in agent worktrees under `.claude/worktrees/agent-<id>` on branches `worktree-agent-<id>`, merged into `m1` by the integrator.

## Done on `m1`

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
| `app` | GUI editor (`schemify edit`), default-UI plugin, docs pane, CLI: `import netlist check run fmt diff merge-driver git-setup` |
| Plugins | `default-ui` (embedded), `gmid-lut` (gm/Id functions from ngspice-characterized LUTs), `net2schem` (SPICE → schematic; round trip spicecmp-equivalent on 297/301 goldens), examples `hello`/`bad-actor`/`gmid-stub` |
| Corpus | `tests/corpus/` fetch + 602 xschem goldens (`.cache/`, gitignored), `tools/spicecmp`, ratchet in `tests/corpus/RATCHET.md` (SPICE 246/301, CDL code-stripped 227/301; +23/+32 golden-unevaluated tracked separately) |
| Workspace tests | 526 passed, 0 failed (`cargo test --workspace --no-fail-fast`, 2026-10-10, after ratchet + net2schem merges) |
| Nix | `flake.nix`: `nix run` (wrapped editor, 23.0 MiB stripped/LTO/panic=abort, D39 ≤ 25 MB), `nix run .#devicereader`, `nix develop` (toolchain, simulators, xschem, GUI libs) |

## In flight (resumed 2026-10-10)

| Workstream | Branch | Scope |
|---|---|---|
| Editor integration | `worktree-agent-a17d8c60dd9156f29` | checks overlay + panel, net highlight `k`, netlist `n`, non-blocking Simulate `s`, results table, waves cross-probe, OP back-annotation, optimizer UI with staged edits |

## Next

1. Merge the three in-flight branches; run `cargo test --workspace --no-fail-fast`.
2. Editor parity gaps (BACKLOG "app / editor"): stretch/kissing move, poly/arc/circle, context menu, search, Save As, scrolling lists, system clipboard, crash-recovery prompt, toolbar icons.
3. Web build end to end (OPFS VFS, runner plugin), CI determinism job, nightly fuzzing.
4. Remaining BACKLOG items by crate.

## Run it

```sh
nix develop            # toolchain, simulators, xschem, GUI libs
cargo run -p schemify-app -- edit crates/app/tests/fixtures/ota/ota.shcomp   # docs pane + gm/Id
schemify netlist <cell> [--dialect cdl]    # headless
nix run                # packaged editor (once flake.nix is committed)
```
