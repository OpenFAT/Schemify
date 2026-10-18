# Schemify documentation

Start with [Install](guide/install.md), then work through [A first session](guide/tutorial.md). It takes an inverter from schematic to waveform and runs a finished RC bench.

## User guide

| Page | Read it when you want to |
|---|---|
| [Install](guide/install.md) | get the editor and the command line running, and know which simulators to install |
| [A first session](guide/tutorial.md) | learn the editor by building and simulating a testbench |
| [The editor](guide/editor.md) | find a key, a menu command or a panel |
| [Projects and files](guide/files.md) | understand `schemify.project`, the three file kinds, libraries, and Schemify under git |
| [Simulation](guide/simulation.md) | write a bench (corners, sweeps, Monte Carlo, outputs, specs, the optimizer), run it and read the waveforms |
| [Docs pages](guide/docs-pages.md) | write a cell's documentation page with math, live variables, plots and gm/Id sizing |
| [Netlists and checks](guide/netlisting.md) | pick a netlist dialect (ngspice, Xyce, CDL, Spectre, Verilog, ESPice) and fix check violations |
| [Configuration and plugins](guide/config.md) | set options and keys in `init.lua`, point at your simulators, and install plugins |
| [Coming from xschem or Virtuoso](guide/migrating.md) | import an xschem library, set up a PDK, move designs to and from Virtuoso, or turn a SPICE netlist into a schematic |
| [Command line](guide/cli.md) | look up a `schemify` or `schemify-interop` command and its flags |
| [In the browser](guide/web.md) | build and use the web version |

## Writing plugins

[plugins/](plugins/README.md) is for plugin authors: the [repository layout](plugins/repository.md) every plugin repository follows, [testing](plugins/testing.md) locally and in CI (against a pinned Schemify and nightly against `main`), and [publishing](plugins/publishing.md). Four example plugins are submodules under `examples/plugins/`.

## Reference

The specs in [spec/](spec/) are the exact rules the code follows. They are written for implementers, so the guide links to the relevant section instead of repeating it.

| Spec | Covers |
|---|---|
| [format.md](spec/format.md) | the design file format: records, attributes, the project manifest and lockfile |
| [expr.md](spec/expr.md) | the expression language, the waveform calculator and netlist templates |
| [netlist.md](spec/netlist.md) | elaboration, hierarchy and each netlist dialect |
| [run.md](spec/run.md) | job expansion, Monte Carlo, results files, the runner and the optimizer |
| [waves.md](spec/waves.md) | the waveform viewer |
| [docs-page.md](spec/docs-page.md) | docs pages |
| [edits.md](spec/edits.md) | edits, undo, staged edits and crash recovery |
| [config.md](spec/config.md) | the Lua user config and the package manager |
| [plugin-abi.md](spec/plugin-abi.md) | the plugin wasm ABI, capabilities and contributions |
| [rpc.md](spec/rpc.md) | the JSON-RPC interface |
| [import.md](spec/import.md), [interop.md](spec/interop.md), [pdk.md](spec/pdk.md) | xschem import, Virtuoso interop and PDK installation |
| [web.md](spec/web.md) | the browser build |

Examples: [examples/init.lua](examples/init.lua) (a user config) and [examples/pdk/](examples/pdk/README.md) (a PDK project).

## Contributing

[dev/contributing.md](dev/contributing.md) has the rules for changing the code. The rest of [dev/](dev/) is for contributors: the [architecture](dev/architecture.md), the [design decisions](dev/decisions.md), the [test plan](dev/testing.md), [performance notes](dev/perf.md), the [status](dev/status.md) and [backlog](dev/backlog.md), and the UI guides in [dev/skills/](dev/skills/).
