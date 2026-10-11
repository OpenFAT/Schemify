# net2schem

Imports a SPICE netlist into Schemify schematics (D26). Each `.subckt` becomes a `.shcomp`
component, a top level (devices, sources, analyses, `.control` blocks) becomes a `.shbench`, and
devices are placed on the bundled `core` cells, then wired. The schematic it draws may be ugly,
but it netlists to the same circuit: the tests check every fixture through `elab`, the netlister
and `tools/spicecmp`.

## Usage

Add the plugin to the project manifest and grant it file access:

```
plugin net2schem path plugins/net2schem
  grant read_project
  grant write_project
```

Then use **File ▸ Import SPICE netlist…** or `Ctrl+Shift+I`. A panel asks for a
project-relative path (`.sp`, `.cir` or `.spice`); press Enter to import. The command also takes
the path as its argument (`net2schem.import n2s/opamp.sp`).

The files are written next to the netlist: `<subckt>.shcomp` for each subckt, `<file>.shbench`
for the top level (`<file>_2` when a subckt has the same name), and the generated leaf cells
described below. The log reports cells, devices, wires, labels, how many nets are fully wired,
and every line it kept as code or changed.

| Capability | Why |
|---|---|
| `read_project` | read the netlist; check whether output names exist |
| `write_project` | write the new cells |

The plugin never overwrites. If any output cell already exists, nothing is written and the log
names the files in the way. Generated leaves (`spice_*.shprim`, stubs) that already exist are kept,
since earlier imports into the same directory share them.

## What becomes what

| SPICE | Schemify |
|---|---|
| `R C L V I E G F H S W T`, `M` with literal `w`/`l`, `D`/`Q` with `area=` | instance of `core:res`, `core:nmos4`, … (pins in SPICE order, values as overrides) |
| any other card, e.g. `D1 a k dmod` (the core diode always prints `area=`), `J`, `B`, behavioural `E`/`G`, 4-node `Q` | instance of a generated leaf `spice_<kind>.shprim` whose template `{name} {pins} {args}` re-emits the card verbatim |
| `X` of a subckt in the file | instance of `./<subckt>`; call parameters become overrides |
| `X` of an undefined subckt (PDK device, external DUT) | instance of a stub leaf named after it, `X…` card re-emitted |
| `.subckt` ports, params, in-body `.param` | `pin` + `port` records; `param` records (kind `string`), re-declared in the netlist by a `.param` line inside the definition |
| `.global` | labels with `global true` |
| top-level dot cards, `K`, `.control … .endc`, unsupported letters | the bench's `code netlist spice` block, verbatim |
| dot cards inside a subckt (`.model`, `.save`, …) | a `spice_code` instance inside the component, so they stay in its scope |

Placement follows the cktImg heuristics: signal flows left to right by distance from the inputs,
supplies sit on top and ground at the bottom, series passives lie horizontal, and transistors face
their gates toward the devices that drive them. Wires are Manhattan paths grown per net from the
leftmost pin. A connection the router cannot draw without touching another net gets a label;
since equal labels connect, that always keeps the circuit. Rails (`vdd*`, `vss*`, `gnd`, `0`, …)
are labelled at every pin. Every net keeps its SPICE name as a label, so `.control` text that
names `v(out)` still works. Ids and bytes are deterministic, so importing the same file twice
gives identical files.

## Limitations

- The first line is not treated as a title. Start a netlist with `*` or `.title`.
- At most 200 element cards per import, and at most 80 devices are drawn with wires (cells in
  file order). This keeps a conversion inside the plugin's default fuel budget of 10^8. Bigger
  cells are drawn with labels only. The native library (`net2schem::convert`) has no card limit.
- `X` cards with no nodes are kept as code text and not drawn.
- `A`, `N`, `P`, `Y` (XSPICE and similar) and `K` cards are kept as code in the cell they
  appear in. They are netlisted but not drawn.
- Subckt parameters are `string` parameters. A literal `w`/`l` maps onto the core MOSFET;
  anything parametric goes through a `spice_*` leaf.
- A stray token on a `.subckt` line (`delay=200 ps`) is dropped with a warning.
- Node names containing `,`, `*` or `:` are renamed with `_`, since those characters mean bundle,
  repeat and range in a net expression.

## Tests

`cargo test --manifest-path plugins/Cargo.toml -p net2schem` runs these checks:

- `tests/roundtrip.rs`: netlist → files → `doc` + `elab` + netlister → SPICE, compared with the
  input by `tools/spicecmp`. The inputs are the salvaged fixtures (`tests/fixtures/*/`, generated
  from the old PySpice scripts by `tests/fixtures/gen.py`), hand fixtures, and, when
  `SCHEMIFY_CORPUS` points at a fetched `tests/corpus/.cache`, xschem golden netlists used as
  input only. The same test checks that files load cleanly and round-trip, that no wires
  overlap, that every pin is wired or labelled, that output is deterministic, and that mutated
  netlists never panic. It prints layout metrics. `--ignored` adds the whole corpus.
- `tests/guest.rs`: runs the built `net2schem.wasm` (from `plugins/build.sh`) through the real
  host. It checks contributions and the import flow, that wasm output is byte-identical to
  native output, and the no-overwrite refusal. `--ignored` prints the fuel report.
