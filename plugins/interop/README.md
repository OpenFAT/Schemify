# interop: Schemify ⇄ Cadence Virtuoso and xschem

Use Schemify as your schematic editor while your team stays in Cadence Virtuoso, and move
designs to and from xschem. One plugin, never part of the core (D24 as amended, D41). Normative
spec: [docs/spec/interop.md](../../docs/spec/interop.md); xschem import:
[docs/spec/import.md](../../docs/spec/import.md).

| Direction | What crosses |
|---|---|
| Virtuoso → Schemify | cells (symbol, schematic, CDF parameters, Verilog-A/Verilog views), PDK libraries (symbols, CDF, simInfo → netlist templates), maestro setups (variables, analyses, corners, outputs, specs) |
| Schemify → Virtuoso | the same, plus gm/Id sizing intent that is solved on the target PDK (PDK-agnostic flow) |
| xschem → Schemify | designs and PDK device libraries (`schemify-interop xschem-import`) |
| Schemify → xschem | `.sch`/`.sym`, core devices onto xschem's `devices/` symbols |

Nothing Cadence-made is linked or shipped. Virtuoso runs one SKILL file, `schemify.il`, that
dumps and loads a plain data file (`.sfyx`). All translation happens in Schemify, in tested
Rust code. The SKILL side only moves data, and its data is never evaluated (`lineread`, D7).

## Install

Add the plugin to the project manifest:

```
plugin interop path plugins/interop
  grant read_project
  grant write_project
  fuel 100000000000
```

`fuel` is the per-event instruction budget (plugin-abi §2). The default, 10^8, is too small for
a conversion of a real design (a Virtuoso export of a small project with a PDK needed about
10^11), so give the ceiling, 10^11: with more fuel than the default the plugin runs on a worker
thread and the editor does not freeze while it converts. A conversion that runs out says so in
the status line (`interop: command interop.virtuoso_export: out of fuel (budget N), …`).

In the editor the File menu gains **Export to Virtuoso (.sfyx)…**, **Import from Virtuoso
(.sfyx)…**, **Export to xschem…** and **Write Virtuoso SKILL bridge (schemify.il)**. Without
the editor, the same conversions run from the command line:

```
cargo run --manifest-path plugins/Cargo.toml -p interop --features cli --bin schemify-interop -- <command>

schemify-interop virtuoso-export <project> <cell-file>... -o <file.sfyx> [--lib L=V]... [--core-cells] [--retarget] [--target <map.retarget>]
schemify-interop virtuoso-import <project> <file.sfyx> [--lib L=V]... [--analoglib]
schemify-interop xschem-export   <project> <cell-file>... -o <dir>
schemify-interop xschem-import   <lib-dir>... -o <out-dir> [...]        (spec import.md)
schemify-interop install-skill   <dir>
```

Paths after `<project>` are relative to it. Each conversion writes a report next to its output
(`<name>.report.md`, `.report.txt`): everything that did not cross one to one, with a reason.

## First time with Virtuoso

1. `schemify-interop install-skill interop/` (or the File menu item) writes `schemify.il` and
   `sfy_probe.il`.
2. In Virtuoso's CIW: `load("/absolute/path/interop/schemify.il")`. Add the same line to
   `.cdsinit` to keep it loaded.
3. Run the self-test once on any cell you have, with a scratch library defined in `cds.lib`:
   `SfySelfTest("myLib" "someCell" "scratchLib")`. It dumps the cell, loads it into the scratch
   library, dumps it again, compares the two dumps, and runs the gm/Id solver checks. It
   prints `PASS` or the first lines that differ.
4. Optionally run `load("/absolute/path/interop/sfy_probe.il")` then `SfyProbe("scratchLib")`,
   and send back `/tmp/sfy_probe.txt`. It records how your Virtuoso version prints, orients,
   names buses and stores CDF data. Those facts settle the open assumptions listed in the spec
   (§13). The probe opens only `scratchLib/sfy_probe` and never saves it.

## Workflows

### Bring a PDK into Schemify (mirror)

In Virtuoso: `SfyExportLib("/tmp/gpdk045.sfyx" "gpdk045")`. In Schemify:
`schemify-interop virtuoso-import <project> gpdk045.sfyx`. You get a library `gpdk045/` with
one `.shprim` per device: the same symbol, pins and CDF parameters (as Schemify parameters
with units and choices), and Spectre and CDL templates built from simInfo. Designs that place
these cells export back to the real PDK cells by name, so geometry and parameters match exactly.

To simulate with ngspice in Schemify, give each mirror device a `format spice` template, or add
`spice "…"` to its row in `interop.map`. Formats you add survive later re-imports. A commercial
PDK's mirror holds its symbols and parameter names: keep that library out of public repositories.

### Bring a design from Virtuoso

`SfyExport("/tmp/amp.sfyx" "myLib" list("amp" "amp_tb"))` dumps the cells. The default
`?ref t` adds every master they use from other libraries, such as PDK and analogLib devices.
`?maestro t`, also the default, adds the testbenches' maestro setups. Then run
`schemify-interop virtuoso-import <project> amp.sfyx`.

Re-importing later merges with your files. Virtuoso owns the drawing, the instance parameters,
the CDF parameters and the maestro setup. Schemify keeps what Virtuoso has no field for:

- docs and variables,
- parameter bounds,
- waivers and constraints,
- ngspice formats,
- configs, sweeps, Monte Carlo and optimizer records.

A waiver whose instance is gone is dropped, and the report says so.

### Send your edits to Virtuoso

`schemify-interop virtuoso-export <project> lib/amp.shcomp -o amp.sfyx`, then in Virtuoso:
`SfyImport("/abs/amp.sfyx" ?overwrite t)`. Add `?callbacks t` to run the PDK's CDF callbacks
after setting parameters, as the Edit Properties form does. The loader then logs every value
the PDK clamped.

Each schematic is checked with `schCheck` and saved only when the check is clean.
`<file>.sfyx.log` lists every created object, every refusal and the result (`PASS`/`FAIL`).

Cellviews you did not change in Schemify since they came from Virtuoso are left exactly as
Virtuoso drew them, including fonts, label justification and instance-label styles. Only what
you edited is redrawn. Object identity rides on `schemifyId` properties, so round trips give
clean Schemify diffs.

### Testbenches and ADE (maestro)

A `.shbench` exports as the testbench schematic plus a maestro view with one test (Spectre):

| Schemify bench | Maestro |
|---|---|
| `var` | design variable (values in Spectre syntax: `1meg` → `1M`) |
| a `sweep` named after its var | the variable's comma list of points |
| `corner` (`lib`, `include`, `set`, `temp`) | corner with model files and sections, corner variables, `temperature` |
| `.tran .ac .dc .op .noise` lines of the analysis code block | analyses with their options |
| `.options errpreset=conservative` | the tran analysis's `errpreset` |
| `.temp`, `.param`, `.include`, `.lib` | variables and test model files |
| `code … spectre` blocks | definition files |
| `output` (`v()`, `i()`, `at`, `re`/`im`, `x()`, `deriv`, `max`, `cross`, `settle`, `phase_margin`, `op("M1","gm")`, …) | outputs (`VS("/net")`, `IF("/V1/PLUS")`, `value`, `real`/`imag`, `xval`, `cross`, `settlingTime`, `phaseMargin`, `OP("/M1" "gm")`, …); attribute `analysis op` picks the dataset per output |
| `spec` `min`/`max` | spec `>`/`<` (bounds over bench vars folded to numbers) |
| core sources, passives, ammeters | their analogLib cells (`vdc`, `idc`, `cap`, `iprobe`, …; `--core-cells` keeps the `schemify_core` mirrors); `dc {VGS} ac 1` → `vdc` `VGS`, `acm` `1` |

Each test also gets an OCEAN script, `ocean/<test>.ocn` beside the `.sfyx`, that runs the same
setup headless and prints every output to `<test>/<output>.txt` (sweeps as `paramAnalysis`; an
output with `vs <other output>` is also printed as a `waveVsWave` plot):

```
SfyImport("/abs/ex.sfyx")                      ; in the CIW (and SfyRetarget, below)
cd ocean && ocean -nograph < dc_vgs.ocn         ; in a shell
```

Importing goes the other way. A comma list comes back as a `lin`/`log`/`values` sweep, and the
analyses come back as one code block named after the test. Monte Carlo and optimizer records,
configs, and anything without a maestro form stay on the Schemify side; the report lists them.

### PDK-agnostic sizing (gm/Id retarget)

Size transistors in Schemify on a sample PDK with the `gmid` plugin. Write widths as
`gmid.w("nfet", gm_id, L, Id)`, directly or through a docs variable. Export with
`--retarget`: those transistors travel as *intent* (gm/Id, L, Id, VDS, fingers) instead of a
width, as placeholders with their connections.

On the machine with the real PDK, write a target map `tsmc.retarget` that binds each class to
a PDK cell and gives its Spectre model line, supply and grids (spec §10.2). Then run:

```
SfyImport("/abs/amp.sfyx")
SfyRetarget("/abs/amp.sfyx" "/abs/tsmc.retarget" ?callbacks t)
```

`SfyRetarget` characterizes each class with Spectre (through OCEAN) at the lengths the design
uses, then solves each width by the same gm/Id lookup the `gmid` plugin uses. It places the
real devices with those widths and draws their connections. `<file>.retarget.log` records the
solved W, fingers, L, VGS and the gm/Id residual for each device. The target PDK never leaves
that machine.

Every other `tech:nmos`/`tech:pmos` instance travels as a fixed-size placeholder (its `w l nf
m`, as design-variable expressions) that `SfyRetarget` places on the same PDK cell. The map's
`fingerw_max`, `fingerw_min` and `split_above` set the finger rule (`fingerw_max 10u fingerw_min
5u split_above 10u`: W > 10 µm splits into equal fingers of at least 5 µm). A `corner tt` record
in the map (`lib /pdk/models.scs tt_lib`) gives the server's model files; pass the map to the
export with `--target` so maestro and the OCEAN scripts use them.

### Different libraries for the same devices (`interop.map`)

When the Schemify library and the Virtuoso library name the same devices differently (an
xschem-imported sky130 against a Virtuoso sky130 kit), write `interop.map` at the project
root. It maps cell, pins and parameters, including value scaling. Such instances are
re-mastered: their connections ride on labels, so no net changes even though the two symbols
differ in shape (spec §6.4, §8.2).

### xschem

`schemify-interop xschem-export <project> top.shbench -o xs/` writes `xs/<lib>/<cell>.sym|.sch`
and an `xschemrc` fragment. Core devices land on xschem's own `devices/*.sym`, and cells first
imported from xschem point back at their original symbols. The test suite netlists the export
with a real xschem and compares it with Schemify's netlist.

## What does not cross

Every conversion lists its losses in the report. The main ones:

- Virtuoso config views (hierarchy editor), and maestro Monte Carlo and optimizer setups. They
  are kept on the Schemify side.
- PDK CDF callbacks inside Schemify. Derived parameters (`ad`, `as`, finger widths) are
  computed by the PDK when you load with `?callbacks t`.
- Cosmetic Virtuoso fields on cellviews you redraw: label fonts, justification,
  `cdsName()` versus `[@instanceName]` labels.
- xschem: non-default corners, sweeps, outputs, specs, optimizer records, waivers.

## How this is tested without Virtuoso

No Cadence tool runs here, so:

- **The SKILL subset.** The SKILL is written in plain prefix form `(f a b)`, which Virtuoso
  reads everywhere. A test-only interpreter (`tests/skill/`) executes it exactly and refuses
  anything outside the subset: C-style calls, infix operators, scale-suffixed or zero-padded
  numbers.
- **The mock Virtuoso.** It implements every Cadence function the scripts call, from the
  manuals' signatures (arity and types checked). A lint fails on any call it does not know.
- **Both sides together** (`tests/virtuoso_e2e.rs`):
  - A Rust export loaded by the shipped SKILL, dumped by it, and imported by Rust is
    byte-identical.
  - A Virtuoso-made dump imported, exported and reloaded into the original library dumps
    identically.
- **The gm/Id solver** is written in both SKILL and Rust, and the two agree bit for bit on
  random vectors. The Rust one is within 2 % of an analytic device.
- **xschem export** is checked against real xschem netlists.
- **What cannot be proven here** is listed as numbered assumptions in spec §13. `SfySelfTest`
  and the probe settle them on the first real run.

## Code

| File | Does |
|---|---|
| `src/design.rs`, `src/nets.rs`, `src/profile.rs` | Schemify files as tables, connectivity, coordinate/orientation/name rules |
| `src/sfyx.rs` | the exchange-file codec |
| `src/virtuoso/` | `write` (export), `read` (import), `ael` (values), `devices` (analogLib table), `ade` (maestro) |
| `src/merge.rs`, `src/remap.rs`, `src/map.rs` | keeping Schemify-only data, re-mastering, `interop.map` |
| `src/retarget.rs` | gm/Id intent and the reference solver |
| `src/xschem_out.rs`, `src/translate.rs` … | xschem export and import |
| `src/job.rs` | one conversion as pure steps, driven by `src/plugin.rs` (editor) and `src/cli.rs` |
| `skill/*.il` | the parts of `schemify.il` |
| `tests/` | `virtuoso_rust`, `virtuoso_skill`, `skill_lint`, `virtuoso_e2e`, `retarget`, `ade`, `exercise1` (a course's benches: maestro + OCEAN goldens), `xschem_export`, and the xschem import suites |
