# Command-line reference

Schemify ships two programs. `schemify` is the headless core: it netlists, checks, simulates, formats, diffs and merges files, manages your user packages, and serves a JSON-RPC and an MCP command stream. It opens no window. `schemify edit` starts the editor, `schemify-gui`. A third program, `schemify-interop`, converts designs to and from xschem and Cadence Virtuoso ([below](#schemify-interop)).

```
schemify [--clean | -u <init.lua>] <command> [args]
```

## Global flags

Global flags go before the command.

| Flag | Effect |
|---|---|
| `--clean` | Skip your `init.lua` and every user package. Project packages (the `plugin` records of `schemify.project`) still load. |
| `-u <file>` | Run `<file>` as the init file instead of `<config>/init.lua`. |

Give `--clean` or `-u` at most once, and not both. `schemify --help` (also `-h` or `help`) prints the usage and exits 0. With no command, or an unknown one, `schemify` prints the usage and exits 2.

```sh
schemify --clean netlist top.shcomp
schemify -u test.lua pack status
```

Without `--clean`, every command runs your user config first. Its notifications and errors print on stderr as `note: …`, `warning: …` or `error: <file>:<line>: …`, and the command goes on. See [config.md](config.md).

## Exit codes

Every command follows one rule:

| Code | Meaning |
|---|---|
| 0 | Success. |
| 1 | Design errors: check violations, files that differ (`diff`), merge conflicts, files not in canonical form (`fmt --check`), failed simulation jobs or specs (`run`), failed packages (`pack`). |
| 2 | Usage or I/O errors: a bad flag, a missing file, a file that cannot be written, no user config for `pack`. |

## Commands

| Command | Does |
|---|---|
| [`edit`](#edit) | Open the editor. |
| [`netlist`](#netlist) | Write a cell's netlist. |
| [`check`](#check) | Report violations. |
| [`run`](#run) | Simulate a bench and print its outputs and specs. |
| [`fmt`](#fmt) | Rewrite files in canonical form. |
| [`diff`](#diff) | Record-level diff of two files. |
| [`merge-driver`](#merge-driver) | Three-way merge for git. |
| [`git-setup`](#git-setup) | Register the merge driver in this clone. |
| [`pack`](#pack) | Manage user packages. |
| [`rpc`](#rpc) | JSON-RPC command stream. |
| [`mcp`](#mcp) | The same methods as MCP tools, for agents. |

### edit

```
schemify edit [<project-dir> | <cell-file>] [--no-default-ui] [--rpc <socket>]
```

Starts `schemify-gui` from the directory of the running `schemify`, else from `PATH`, passing the global flags and the remaining arguments. With a cell file it opens that cell in its project (the nearest directory with `schemify.project`); with a directory it opens the project with a new schematic.

| Flag | Effect |
|---|---|
| `--no-default-ui` | Run without the bundled UI plugin: window and canvas, mouse only. |
| `--rpc <socket>` | Serve the editor's command stream on a socket (see [rpc](#rpc)). |

```sh
schemify edit amp/top.shcomp
schemify edit amp/ --rpc /tmp/schemify.sock
```

`edit` returns the editor's exit code: 0 when it closes normally, 2 for usage or startup errors, and 2 when `schemify-gui` cannot be found. See [editor.md](editor.md).

### netlist

```
schemify netlist <cell> [--dialect D] [--config C] [--corner K] [--set VAR=EXPR]... [-o FILE] [--allow-errors]
```

Elaborates the cell, checks it, and writes its netlist to stdout or `FILE`.

| Flag | Effect |
|---|---|
| `--dialect D` | `ngspice`, `espice`, `xyce`, `cdl`, `spectre` or `verilog`. Default: `schemify.opt.simulator`, else `ngspice`. |
| `--config C` | Bench config. Default: the bench's `config` header attribute. |
| `--corner K` | Bench corner. Default: the bench's `corner` header attribute. |
| `--set VAR=EXPR` | Override a bench variable. Repeatable, later wins. |
| `-o FILE` | Write to `FILE` (atomically) instead of stdout. |
| `--allow-errors` | Write the netlist even when there are `E` violations. |

```sh
schemify netlist tb.shbench --dialect spectre --corner ff -o tb.scs
```

Violations print on stderr. Exit 1 when an error blocks the netlist. See [netlisting.md](netlisting.md).

### check

```
schemify check <cell> [--config C] [--corner K]
```

Elaborates the cell and prints each violation on stderr as `<code> <cell> [<path>] [<param>]: <detail>`. Exit 1 when any violation is an error (`E…`), else 0. The codes are listed in [netlisting.md](netlisting.md#check-a-design).

```sh
schemify check tb.shbench --corner ff
```

### run

```
schemify run <bench> [--config C]... [--corner K]... [--sweep S]... [--mc M] [--set VAR=EXPR]... [--jobs N] [--dialect D]
```

Simulates a `.shbench` and prints the results: one `job` record per job with its outputs and spec verdicts. Jobs are the product of the selected configs, corners, sweep points and Monte Carlo samples.

| Flag | Effect |
|---|---|
| `--config C` | Run this config. Repeatable. Default: the bench's. |
| `--corner K` | Run this corner. Repeatable. Default: the bench's. |
| `--sweep S` | Run the bench's `sweep S` record. Repeatable; the first is the outermost loop. |
| `--mc M` | Run the bench's `montecarlo M` record. |
| `--set VAR=EXPR` | Override a bench variable in every job. Repeatable. |
| `--jobs N` | Run at most `N` simulators at once (N > 0). Default: the number of cores. |
| `--dialect D` | `ngspice` or `espice`. Default: `schemify.opt.simulator`. Any other dialect exits 2. |

```sh
schemify run tb.shbench --corner tt --corner ff --sweep vdd_sweep --jobs 4
```

The simulator command line comes from `schemify.sim` in your [user config](config.md), never from a design file. Each run lives in `<project>/.schemify/runs/<hash>/`, with a `results` file and one directory per job (`netlist.cir`, `out.raw`, `stdout.log`, `stderr.log`, `command.log`). Exit 1 when a job failed or a spec failed; failed jobs print `job <n>: <message>` on stderr. `run` is not available in the web build. See [simulation.md](simulation.md).

### fmt

```
schemify fmt [--check] <file>...
```

Parses each file and rewrites it in canonical form, in place. With `--check` it rewrites nothing and prints `<file>: not in canonical form` for each file that would change (exit 1). A file that does not parse prints `<file>:<line>:<col>: …` and gives exit 1; the other files are still processed.

```sh
schemify fmt --check lib/*.shprim
```

### diff

```
schemify diff <A> <B>
```

Compares two files record by record and prints one line per added (`+`), removed (`-`) or changed (`~`) record. A changed record lists what changed: `[fields]`, `[text]`, `[comments]`, `[attrs: <keys>]`.

```
$ schemify diff a.shcomp b.shcomp
~ inst 1j446r [attrs: W]
```

Exit 0 when the files are the same, 1 when they differ, like `diff(1)`.

### merge-driver

```
schemify merge-driver <O> <A> <B> [<path>]
```

A semantic three-way merge of base `O`, ours `A` and theirs `B`; the result replaces `A`, as git expects. `<path>` is the name used in messages. Exit 1 when conflicting records remain (it prints `<path>: <n> conflicting record(s)`), or when a side does not parse (then `A` is left untouched for you to resolve by hand). You do not call this yourself: run `git-setup` and git calls it.

### git-setup

```
schemify git-setup
```

Run it once in each clone. It sets `merge.schemify.name` and `merge.schemify.driver` in the clone's git config and adds these lines to `.gitattributes` when missing:

```
*.shprim merge=schemify
*.shcomp merge=schemify
*.shbench merge=schemify
```

Commit `.gitattributes` so every clone shares it. Git never reads a driver command from the repository itself, so each clone still runs `git-setup`. Exit 2 outside a git clone.

### pack

```
schemify pack sync|update|restore|clean|status
```

Installs and pins the packages your `init.lua` declares with `schemify.pack.add`. Each command prints one `<name>: <what happened>` line per package. Exit 1 when a package failed, 2 for a usage error or when there is no user config (for example under `--clean`).

| Command | Does |
|---|---|
| `sync` | Install missing packages and write the lockfile. |
| `update` | Move every package to its newest allowed revision and rewrite the lockfile. |
| `restore` | Make every package exactly what the lockfile says. |
| `clean` | Delete installed packages no spec names. |
| `status` | One line per package: kind, resolved source, state, and denied or unused grants. |

Details in [config.md](config.md#packages).

### rpc

```
schemify rpc [--listen <socket>]
```

Serves a JSON-RPC 2.0 command stream, one JSON message per line. Without `--listen` it reads requests on stdin and answers on stdout until stdin ends (exit 0). With `--listen` it serves one client after another on a local socket: a Unix socket path, or a loopback TCP address such as `127.0.0.1:7777` (Windows has only the TCP form). Non-loopback addresses are refused.

| Method | Params | Result |
|---|---|---|
| `fmt`, `diff`, `netlist`, `check`, `run` | The command's arguments as an array of strings | `{"code", "stdout", "stderr"}`: the exit code and output of that command |
| `edit` | `{"file": <path>, "ops": <op text>, "label"?: <text>}` | `{"changed": true\|false}` |
| `rpc.methods` | none | The method names |

Each CLI method runs `schemify` itself as a child process, with the server's `--clean` or `-u` flags, so a failing command cannot stop the stream. `edit` applies edit ops ([edits.md](../spec/edits.md)) to a file as one transaction and writes it back in canonical form; it keeps no state between calls.

```sh
echo '{"jsonrpc":"2.0","id":1,"method":"netlist","params":["tb.shbench","--dialect","xyce"]}' | schemify rpc
echo '{"jsonrpc":"2.0","id":2,"method":"edit","params":{"file":"top.shcomp","ops":"set_attr inst m00001 W 3"}}' | schemify rpc
```

The editor serves a stream too: `schemify edit <path> --rpc <socket>`. There a method is any command name the editor knows (`file.save`, `sim.netlist`, `sim.dialect.xyce`, a plugin's command), params are the command's arguments as strings, and the answer is `{"status": <the status-line message>}`. Clients also receive `event` notifications (`doc_changed`, `saved`, `run_done`, …). Anyone who can connect to the socket can run what you can run, simulators included: do not share it with other users. The protocol is in [rpc.md](../spec/rpc.md).

### mcp

```
schemify mcp
```

A Model Context Protocol server on stdio for AI agents. Its tools are the headless methods above: `check`, `diff`, `edit`, `fmt`, `netlist` and `run`. The CLI tools take `{"args": [<string>…]}`; `edit` takes `{"file", "ops", "label"?}`. A tool's text result is `exit code <n>`, then its stdout and stderr. Register `schemify` with the argument `mcp` as a stdio server in your agent's configuration. It takes no flags of its own (the global flags still apply), and exposes no editor commands.

## schemify-interop

`schemify-interop` is the interop plugin's command line. It is a separate program, built from `plugins/interop`:

```sh
nix run .#interop -- xschem-import …
cargo run --manifest-path plugins/Cargo.toml -p interop --features cli --bin schemify-interop -- <command>
```

Exit codes follow the core: 0 ok, 1 an output failed its check or the conversion was refused (problems printed), 2 usage or I/O errors.

### xschem-import

```
schemify-interop xschem-import <lib-dir>... [--root <dir>]... [--models <file>[:<section>]]...
    [--spiceinit <file>] [--tech <file>] [--cdf <dump>] [--xschemrc <file>]... [--no-core] -o <out-dir>
```

Converts every `.sym` and `.sch` under each `<lib-dir>` into Schemify cells. The `<lib-dir>`s are the xschem library search path, in order. It writes `<lib>/<cell>.<ext>`, a `schemify.project` and `import-report.md`/`import-report.txt` into `<out-dir>`, prints a summary, then checks every output.

| Flag | Effect |
|---|---|
| `--root <dir>` | Add a search-path element whose files are only referenced, not converted (xschem's `xschem_library` parent, so `devices/res.sym` resolves). Repeatable. |
| `--models <file>[:<section>]` | Scan SPICE model cards (and their `.include`/`.lib` files) for device limits: model bins become `min`/`max` on `w` and `l`. Repeatable. |
| `--spiceinit <file>` | Read an ngspice `.spiceinit`: its `sourcepath` is where model includes are looked up. |
| `--tech <file>` | A Magic tech file (or a KLayout `.lyt`) for the manufacturing grid. |
| `--cdf <dump>` | A Virtuoso `cdfDump` file: cyclic parameters become `choices`. |
| `--xschemrc <file>` | PDK path variables and `drc` procs. Repeatable (pass `xschem-drc` too). |
| `--no-core` | Keep xschem's devices as converted cells instead of mapping them onto Schemify's core devices. |
| `-o <out-dir>` | Output directory. Required. |

```sh
schemify-interop xschem-import ~/xschem_library/devices ~/designs/amp --root ~/xschem_library -o amp-schemify
```

See [migrating.md](migrating.md) and [import.md](../spec/import.md).

### Virtuoso and xschem export

```
schemify-interop virtuoso-export <project-dir> <cell-file>... -o <file.sfyx> [--core-cells] [--retarget] [--target <map.retarget>] [--lib L=V]...
schemify-interop virtuoso-import <project-dir> <file.sfyx> [--analoglib] [--lib L=V]...
schemify-interop xschem-export <project-dir> <cell-file>... -o <out-dir>
schemify-interop install-skill <dir>
```

Cell files, `.sfyx` paths and output paths are relative to `<project-dir>`. Each conversion writes a report next to its output listing everything that did not translate one to one.

| Command or flag | Effect |
|---|---|
| `virtuoso-export` | Write the cells (and their testbench setups) to a `.sfyx` file that Virtuoso loads, plus OCEAN scripts under `ocean/`. |
| `--core-cells` | Export core devices as `schemify_core` mirror cells instead of their analogLib masters (the default). |
| `--retarget`, `--target <map.retarget>` | Carry gm/Id sizing intent so widths are solved on the target PDK in Virtuoso. |
| `--lib L=V` | Map Schemify library `L` to Virtuoso library `V` (import: the other way). Repeatable. |
| `virtuoso-import` | Read a `.sfyx` dump from Virtuoso into the project. |
| `--analoglib` | On import, move analogLib instances onto Schemify's core devices. |
| `xschem-export` | Write `.sch`/`.sym` files, core devices onto xschem's `devices/` symbols. |
| `install-skill` | Write `schemify.il` and `sfy_probe.il`, the SKILL files Virtuoso loads, into `<dir>`. |

The Virtuoso workflow, including the first-session probe, is in `plugins/interop/README.md` and [interop.md](../spec/interop.md).

See also: [netlisting.md](netlisting.md), [config.md](config.md), [simulation.md](simulation.md).
