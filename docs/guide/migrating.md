# Coming from xschem or Virtuoso

This page covers moving existing work into Schemify: importing xschem libraries, setting up a PDK, exchanging designs with Cadence Virtuoso, and turning a SPICE netlist into a schematic. It also lists what your xschem habits get you and where Schemify behaves differently.

All of the conversions live in plugins, not in the `schemify` binary. Some run only from the command line, some only in the editor:

| Task | Command line | Editor |
|---|---|---|
| Import an xschem library | `schemify-interop xschem-import` | no |
| Export cells to xschem | `schemify-interop xschem-export` | File → Export to xschem… |
| Export to Virtuoso (`.sfyx`) | `schemify-interop virtuoso-export` | File → Export to Virtuoso (.sfyx)… |
| Import a Virtuoso dump | `schemify-interop virtuoso-import` | File → Import from Virtuoso (.sfyx)… |
| Write the SKILL bridge | `schemify-interop install-skill` | File → Write Virtuoso SKILL bridge (schemify.il) |
| Install and switch PDKs | no | View → PDKs (the `pdk-manager` plugin) |
| SPICE netlist to schematic | no | File → Import SPICE netlist… (the `cktimg` plugin) |

## Getting `schemify-interop`

`schemify-interop` is the interop plugin's command-line tool, from its own repository, [Schemify-Interop](https://github.com/OmarSiwy/Schemify-Interop):

```sh
cargo install --git https://github.com/OmarSiwy/Schemify-Interop --features cli --bin schemify-interop
schemify-interop xschem-import --help            # prints the usage line
# or, from Schemify's submodule checkout:
cargo run --manifest-path examples/plugins/interop/Cargo.toml --features cli \
  --bin schemify-interop -- xschem-import ...
```

Every subcommand exits 0 on success, 1 when an output failed its check (or a conversion was refused), and 2 on a usage or I/O error.

## Importing an xschem library

Point the importer at the directories of your xschem library search path (`XSCHEM_LIBRARY_PATH`), in the same order, and give it an output directory:

```sh
XS=/usr/local/share/xschem/xschem_library      # wherever xschem installed it

schemify-interop xschem-import \
  "$XS/devices" --root "$XS" \
  ~/chips/amp/xschem \
  -o ~/chips/amp-schemify
```

Every `.sym` and `.sch` under each directory you name is converted. `--root` adds a search path element whose files are only referenced, not converted. Here it makes a reference such as `devices/res.sym` resolve, while `"$XS/devices"` is what actually gets converted.

| Flag | What it does |
|---|---|
| `<lib-dir>...` | Directories to convert, in search path order (at least one). |
| `--root <dir>` | A search path element used only to resolve references (repeatable). |
| `--xschemrc <file>` | Reads PDK path variables (`set SKYWATER_MODELS ${PDK_ROOT}/...`) and `drc=` procs. Repeatable: pass a PDK's `xschem-drc` file as well. |
| `--models <file>[:<section>]` | Scans SPICE model cards. Model bins give `L`/`W` parameters a `min`/`max` (repeatable). |
| `--spiceinit <file>` | Reads an ngspice `.spiceinit`. Its `sourcepath` is where model includes are looked up (`$VAR`s come from your environment). |
| `--tech <file>` | A Magic tech file or KLayout `.lyt`. Its manufacturing grid becomes the `grid` of `L`/`W`. |
| `--cdf <dump>` | A Virtuoso `cdfDump` file. Radio and cyclic CDF parameters become choice parameters. |
| `--no-core` | Keeps xschem's `devices/` symbols as their own library instead of mapping them onto Schemify's bundled `core` devices. |
| `-o <out-dir>` | Where to write (required). |

The output is one Schemify library per xschem symbol directory, plus a `schemify.project` that lists them and an import report:

```text
amp-schemify/
  schemify.project          lib records, and set records for PDK variables
  devices/ ...              only xschem devices that could not map to core
  amp/
    ota.shcomp              ota.sym + ota.sch
    ota__top.shbench        a bench copy, when ota.sch carries netlist text
    tb_ota.shbench          a schematic without a symbol
  import-report.md
  import-report.txt
```

A symbol with a schematic becomes a `.shcomp` and a symbol without one a `.shprim`. A schematic without a symbol becomes a `.shbench`, or a `.shcomp` when it has `ipin`/`opin`/`iopin` instances. The full mapping is in [the import spec](../spec/import.md). File kinds are explained in [files.md](files.md).

The tool prints a summary (inputs, outputs per kind, how many device instances landed on `core`, and the count of each report class), then checks every output file and prints `check: N problems`. Read `import-report.md` before anything else. Every item that did not cross one to one is there with its file and line:

- `needs_review:<reason>`: something the importer would not guess, such as a Tcl expression or a missing symbol. The field is left at its default, never filled with a guess.
- `dropped:<reason>`: something that has no meaning in Schemify (launchers, logos, display-only texts).
- `alt-cell`: a generated cell, such as a bench copy or a per-instance variant.
- `rounding`: a coordinate rounded onto Schemify's integer grid.

Then open the result and netlist a bench to compare with what xschem gave you:

```sh
schemify edit ~/chips/amp-schemify
schemify netlist ~/chips/amp-schemify/amp/tb_ota.shbench
```

### How close the netlists are

The nightly tests import 301 benches (xschem's own examples, sky130, GF180 and IHP designs), netlist them and compare with the netlists xschem itself writes. On the current run, 255 of 301 SPICE netlists are equivalent to xschem's. For CDL, 235 of 301 are equivalent once the inline code blocks are left out of the comparison, since Schemify's CDL carries none. Another 24 SPICE benches differ only because xschem leaves a parameter unevaluated where Schemify evaluates its default. Each remaining difference is listed with its cause in [examples/plugins/interop/tests/corpus/RATCHET.md](../../tests/corpus/RATCHET.md).

### What does not come across

- Anything that needs a Tcl interpreter: `tcleval(...)` with commands, symbol generators (`x.tcl(...)`), procs called from a `format`, `[xschem ...]` calls. These are reported, not run.
- Drawings in a schematic (`L`, `B`, `P`, `A` records), graphs, images, launchers and logos are dropped. Symbol drawings are kept.
- Floater texts and display-only texts (`@spice_get_*`, `ngspice::get_node`).
- Bus taps, `netlist_options` symbols and parametric bus widths such as `ADD[dim-1:0]`.
- Repeated instance names. xschem netlists both; Schemify reports `E009` and refuses to netlist until you rename them. Press `#` to highlight duplicates and `Ctrl+#` to rename them.
- XSPICE inverted-net labels like `~D[5..0]` (the `~` is not yet accepted in net names).

## Setting up a PDK

There are two routes. The `pdk-manager` plugin installs a PDK per project and keeps the manifest in sync. A manual import gives you more control, for example model-bin limits.

### With the PDK manager

The plugin installs sky130, gf180mcu and ihp-sg13g2 through [ciel](https://github.com/fossi-foundation/ciel), and the academic kits freepdk45 and asap7 from pinned downloads. You need `ciel` and `schemify-interop` on your `PATH`. Add the plugin to your project's `schemify.project`, pointing `path` at your copy of `pdk_manager.wasm` (it is in `examples/plugins/pdk-manager/` of a checkout):

```text
plugin pdk-manager path plugins/pdk-manager/pdk_manager.wasm
  grant read_project
  grant write_project
  grant spawn:ciel
  grant spawn:schemify-interop
```

freepdk45 also needs `grant spawn:curl` and `grant spawn:tar`; asap7 needs `grant spawn:git`.

Open **View → PDKs**, pick a family, then **Install** on a version and **Use** once it finishes. The same actions are plugin commands you can bind in `init.lua` or send over JSON-RPC:

```text
pdk.list_remote sky130
pdk.install sky130 1e3554706ddbb2529193bdabbc7128296f3cb44a
pdk.use sky130 1e3554706ddbb2529193bdabbc7128296f3cb44a
```

The install runs `ciel`, then `schemify-interop xschem-import` on the PDK's xschem library, into `.schemify/pdks/<family>/<version>/`. `pdk.use` rewrites your manifest, and the editor reloads the project without a restart:

```text
lib sky130_fd_pr path .schemify/pdks/sky130/1e35.../lib/sky130_fd_pr
lib tech map .schemify/pdks/sky130/1e35.../tech.map
set PDK_ROOT .schemify/pdks/sky130/1e35.../root
set PDK sky130A
set PDK_MODELS ${PDK_ROOT}/sky130A/libs.tech/combined/sky130.lib.spice
```

`pdk.remove <family> <version>` deletes a version that is neither active nor pinned by `schemify.lock`. `pdk.prune` lists all such versions and `pdk.prune confirm` deletes them. The plugin does not add `--models`, so a PDK installed this way has no model-bin limits on `L`/`W`.

### Technology-neutral devices (`tech:`)

Place `tech:nmos` and `tech:pmos` instead of a PDK's own transistor cells, and size them with `w`, `l`, `nf`, `m` in metres, plus `flavor` (`core`, `io`, `lvt` or `hvt`):

```text
inst m00001 tech:pmos
  at 0 -50 R0
  name M1
  w 2u
  l 0.5u
  nf 2
```

The active `tech.map` turns each one into the PDK's cell at netlist time (sky130's `W = w/1u`, for example). Switching PDK with `pdk.use` retargets every `tech:` instance with no edit to the drawing. Cells placed directly from a PDK library, such as `sky130_fd_pr:nfet_01v8`, do not retarget: they turn into unresolved references (`E001`) once their library leaves the manifest. A flavor the PDK lacks is an error, never a silent substitute ("sky130 has no lvt pmos").

### Model corners

Point bench corners at `${PDK_MODELS}` so they follow the active PDK:

```text
corner tt
  lib ${PDK_MODELS} tt
```

Section names are the PDK's own: `tt ff ss sf fs` for sky130, `typical ff ss sf fs` for gf180mcu, `mos_tt mos_ff ...` for ihp-sg13g2, `tt ff ss` for freepdk45 and asap7. Schemify does not map one family's corner names onto another's yet, so edit the `lib` line when you switch between sectioned kits.

### Manual import with model-bin limits

To get `min`/`max` on `L` and `W` from the model bins, import the PDK's xschem library yourself and pass its model file:

```sh
schemify-interop xschem-import \
  "$PDK_ROOT/sky130A/libs.tech/xschem" \
  --xschemrc "$PDK_ROOT/sky130A/libs.tech/xschem/xschemrc" \
  --models "$PDK_ROOT/sky130A/libs.tech/combined/sky130.lib.spice:tt" \
  -o pdk/sky130
```

The summary lists every limit it found (`limit <file> L: min ... max ...`). An instance sized outside its limits then reports `E005` in the checks panel and in `schemify check`. Add the resulting libraries to your project's manifest with `lib` records.

The [PDK example walkthrough](../examples/pdk/README.md) goes from installing sky130 to switching to gf180mcu, freepdk45 and asap7. The full rules are in [the PDK spec](../spec/pdk.md).

## Working with Cadence Virtuoso

Schemify links nothing from Cadence. Virtuoso loads one SKILL file, `schemify.il`, which dumps cells to a plain data file (`.sfyx`) and loads them back. All translation happens in Schemify.

No Virtuoso runs in Schemify's CI. The SKILL side is tested against a mock built from the manuals, and the `cdfDump` reader has only been checked against the documented format. Run the self-test below before you trust a round trip on your release and PDK.

To check everything at once, run the live test kit on the Virtuoso machine: `examples/plugins/interop/live/run.sh` from your Virtuoso project directory. It loads, dumps and round-trips a set of fixture designs and your own cells, netlists and simulates them, and writes a report with a verdict. See [examples/plugins/interop/live/README.md](../../examples/plugins/interop/live/README.md).

### Load the plugin in the editor

The editor's File menu items appear once the interop plugin is in your manifest. Copy `interop.wasm` (from `examples/plugins/interop/` of a checkout) into your project:

```text
plugin interop path plugins/interop.wasm
  grant read_project
  grant write_project
  fuel 100000000000
```

The default per-event budget is too small for a real design. With the higher `fuel`, the conversion also runs on a worker thread, so the editor keeps drawing. Each menu item opens a path panel; type project-relative paths and press Enter.

### Install and check the bridge

```sh
schemify-interop install-skill interop        # writes interop/schemify.il and interop/sfy_probe.il
```

Copy both files to the Virtuoso machine. Create an empty, writable scratch library (`scratchLib` below), close the schematic and maestro windows of the cells you will use, then in the CIW:

```text
load("/abs/path/schemify.il")
isCallable('SfySelfTest)                                  ; prints t
load("/abs/path/sfy_probe.il")
SfyProbe("scratchLib" ?pdkLib "yourPdk" ?pdkCell "nch")
SfySelfTest("myLib" "someCell" "scratchLib")
```

`SfySelfTest` dumps a cell, loads it into the scratch library, dumps it again and compares. It prints `PASS` or the first differing lines and writes `/tmp/sfy_selftest.txt`. The probe writes `/tmp/sfy_probe.txt`, which records how your release behaves. The plugin's [README](../../examples/plugins/interop/README.md) has the full first-run checklist and what to send back if something fails.

### Moving cells each way

| Goal | In Virtuoso (CIW) | In Schemify |
|---|---|---|
| Mirror a PDK library | `SfyExportLib("/tmp/gpdk045.sfyx" "gpdk045")` | `schemify-interop virtuoso-import <project> gpdk045.sfyx` |
| Bring a design over | `SfyExport("/tmp/amp.sfyx" "myLib" list("amp" "amp_tb"))` | `schemify-interop virtuoso-import <project> amp.sfyx` |
| Send edits back | `SfyImport("/abs/amp.sfyx" ?overwrite t)` | `schemify-interop virtuoso-export <project> lib/amp.shcomp -o amp.sfyx` |

Paths after `<project>` are relative to it. The command-line options:

```text
schemify-interop virtuoso-export <project-dir> <cell-file>... -o <file.sfyx>
    [--core-cells] [--lib L=V]...
schemify-interop virtuoso-import <project-dir> <file.sfyx> [--analoglib] [--lib L=V]...
schemify-interop xschem-export   <project-dir> <cell-file>... -o <out-dir>
schemify-interop install-skill   <dir>
```

- `--lib L=V` renames Schemify library `L` to Virtuoso library `V` (on import, the reverse).
- Export places Schemify's core sources and passives as analogLib cells; `--core-cells` keeps them as `schemify_core` cells instead.
- `virtuoso-import --analoglib` moves analogLib instances onto Schemify's core devices instead of an analogLib mirror library.

`SfyImport` adds `?callbacks t` to run the PDK's CDF callbacks as the Edit Properties form does, and logs every value the PDK clamped. Each schematic is checked with `schCheck` and saved only when the check is clean. A testbench (`.shbench`) also exports as a maestro view and an OCEAN script, `ocean/<cell>.ocn`, beside the `.sfyx`.

Re-importing merges with your files. Virtuoso owns the drawing, instance parameters, CDF parameters and the maestro setup. Schemify keeps what Virtuoso has no field for: docs, variables, parameter bounds, waivers, ngspice formats, sweeps, Monte Carlo and optimizer records. Every conversion writes `<name>.report.md` beside its output with everything that did not cross one to one.

When the two sides name the same devices differently (an xschem-imported sky130 against a Virtuoso sky130 kit), write an `interop.map` at the project root to map cells, pins and parameters, including unit scaling. The syntax is in the mapping file section of [the interop spec](../spec/interop.md).

Mirrors of a commercial PDK contain its symbols and parameter names. Keep those libraries out of public repositories.

## Turning a SPICE netlist into a schematic

The `cktimg` plugin reads a SPICE deck and draws it: supplies at the top, grounds at the bottom, signal flowing left to right. Add it to the manifest (the wasm is `examples/plugins/cktImg/cktimg.wasm` in a checkout):

```text
plugin cktimg path plugins/cktimg.wasm
  grant read_project
  grant write_project
  fuel 100000000000
```

Press `Ctrl+Shift+I` or use **File → Import SPICE netlist…**, type a project-relative `.sp`, `.cir` or `.spice` path and press Enter. Each `.subckt` becomes a `.shcomp` and the top level becomes a `.shbench`, written next to the netlist. The plugin never overwrites: if an output exists, nothing is written and the status line names the files in the way.

- Start the file with a `*` comment or `.title`. The first line is not treated as a title otherwise.
- `R C L V I E G F H S W T` cards, and `M` with literal `w`/`l`, become core devices. Other cards (a diode without `area=`, `J`, `B`, behavioural sources, XSPICE `A`) become generated `spice_*` leaf cells that re-emit the card verbatim.
- An `X` call of a subckt the file does not define (a PDK device) becomes a stub cell named after it.
- Top-level dot cards and `.control` blocks go into the bench's `code` block unchanged.
- One import takes at most 200 element cards. Large flat cells need the higher `fuel` above; without it they stop with `E_FUEL` and write nothing. While a large cell is laid out the editor stalls.

## Your xschem keys still work

The default keymap is xschem's. `w` draws a wire, `Shift+I` or `Insert` places a symbol, `q` edits properties, `m` moves, `c` copies, `e` and `Ctrl+E` descend and return, `k` highlights a net, `n` netlists, `s` simulates, `u` and `Shift+U` undo and redo, `f` zooms to fit. Press `/` in the editor for the full list. [editor.md](editor.md) covers editing in detail.

Differences worth knowing:

| xschem | Schemify |
|---|---|
| `x` starts a new xschem process | `x` opens a new tab; one process holds every tab |
| Netlist directory set per session | Netlists go to `.schemify/netlist/<cell>.<ext>` in the project |
| Netlist format menu (SPICE, Verilog, VHDL, tEDAx) | Dialects ngspice, ESPice, Xyce, CDL, Spectre, Verilog; `Ctrl+Shift+V` cycles them |
| LVS netlist options | The `cdl` dialect is the LVS netlist |
| Keys `0` to `4` drive the logic simulator | Not present; simulation goes through ngspice or Xyce |
| Tools → Execute TCL command | No Tcl anywhere; scripting is Lua in `init.lua` ([config.md](config.md)) |
| Configure simulators in the GUI | Simulators come from your user config |
| Undo buffer in memory or on disk | Undo always lives in an on-disk journal, so it survives a crash |
| Duplicate instance names allowed | `E009`; `#` highlights them, `Ctrl+#` renames |
| Floater texts | Instance texts come from the symbol's text templates |
| Images in schematics, Print grabs a screen area | No images in the file format; Print exports a PNG of the canvas |

New keys with no xschem counterpart include `Alt+D` (the docs pane) and `Ctrl+Shift+I` (SPICE import, with the cktimg plugin loaded).

See also: [files.md](files.md), [netlisting.md](netlisting.md), [simulation.md](simulation.md), [cli.md](cli.md), [the import spec](../spec/import.md), [the interop spec](../spec/interop.md).
