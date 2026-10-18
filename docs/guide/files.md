# Projects and files

A Schemify design is a directory of plain text files. Every file is line-based, one object per
record, so a change to one part or one parameter shows up as one changed line in `git diff`.
This page covers the project layout, the three cell file kinds and how to keep designs in
version control. The full grammar is in [../spec/format.md](../spec/format.md).

## Project layout

```
my_project/
  schemify.project        manifest: libraries, plugins, settings
  schemify.lock           pinned library hashes (tool-written, when present)
  .gitattributes          merge driver lines (from schemify git-setup)
  top.shcomp              cells of the project itself
  inv.shcomp
  tb_inv.shbench
  dev/                    a library declared in the manifest
    nmos.shprim
    pmos.shprim
  .schemify/              generated: journals, netlists, runs, exports
```

Schemify finds the project from any file you open: it is the nearest directory above the file
that holds `schemify.project`. Without one, the file's own directory is the project and only
the bundled `core` library is available.

## The manifest: `schemify.project`

The smallest manifest is the header line alone:

```
schemify 1 project
```

A project that uses a local device library and a plugin:

```
schemify 1 project

lib dev path dev

plugin gmid path plugins/gmid_lut.wasm
  grant read_project
  grant write_project
  grant spawn:ngspice

set PDK_ROOT /opt/pdk
```

### `lib`: libraries

A library is a directory of cell files. You give it a name and a source:

| Record | Meaning |
|---|---|
| `lib <name> path <dir>` | A directory, relative to the project root |
| `lib <name> git <url> <rev>` | A git repository at a branch, tag or commit |
| `lib <name> url <url> <sha256>` | An archive with its expected hash |
| `lib tech map <file>` | The technology map of a PDK ([../spec/pdk.md](../spec/pdk.md)) |

Any of the first three takes an indented `subdir <dir>` when the cells sit below the source's
root. Only `path` libraries load in this release: `git` and `url` libraries are read from the
manifest but not fetched, and Schemify reports a note saying so (on stderr, or in the
editor's status line).

`core`, the bundled device library, is always there without a `lib` line. A `lib core ...`
line of your own replaces it.

### `plugin`: project plugins

`plugin <name> path <file>` loads a WASM plugin for everyone who opens the project. The path is a
`.wasm` file or a directory holding `<name>.wasm`. A plugin can do nothing outside the sandbox
unless the manifest grants it, one `grant` line per capability:

| Grant | Lets the plugin |
|---|---|
| `read_project` | Read files of the project |
| `write_project` | Write files of the project |
| `spawn:<program>` | Run that one program, found on `PATH` (`spawn:ngspice`) |

`fuel <n>` raises the plugin's work budget per event (a whole number from 1000000 to
100000000000; the default is 100000000) for plugins that do heavy work, such as importing a
large netlist. Only `path` plugins load in this release. Plugins you want in every project
belong in your user config instead ([config.md](config.md)). The capability list is in
[../spec/plugin-abi.md](../spec/plugin-abi.md).

### `set`: project variables

`set <name> <value>` defines a variable for `${NAME}` in model and include paths of a bench's
corners. Project variables win over environment variables of the same name:

```
corner tt
  lib ${PDK_ROOT}/sky130A/libs.tech/ngspice/sky130.lib.spice tt
```

## The lockfile: `schemify.lock`

The lockfile pins each fetched library to the exact commit or archive it resolved to, with a
content hash, so everyone simulates the same models:

```
schemify 1 lock

locked lib <name> <git|url|path> <resolved commit, url or path> <sha256>
```

It is written by tools, never by hand; commit it when your project has one. Schemify reads it
on load. Because `git` and `url` libraries are not fetched yet, no `schemify` command writes a
lockfile in this release, and most projects have none.

## Cells and file kinds

A cell is one file. Its name is the file stem: `inv.shcomp` is the cell `inv`. One library may
not hold two files with the same stem and different kinds. Every file starts with the header
`schemify 1 <kind>`; indented lines below a record belong to it; `#` lines are comments and
stay attached to the record that follows.

| Kind | Holds | Can be placed | Can be simulated |
|---|---|---|---|
| `.shprim` | A symbol and one or more device definitions (SPICE template, Verilog-A, Verilog, SPICE text) | yes | no |
| `.shcomp` | A symbol, its schematic and a docs page | yes | no |
| `.shbench` | A schematic, a docs page and the simulation setup | no | yes |

Coordinates are in xschem units with y pointing down; the bundled symbols put pins on a
10-unit pitch. Orientations are `R0`, `R90`, `R180`, `R270` and their mirrored forms `MR0` to `MR270`.

### `.shprim`: a primitive

The bundled resistor, `crates/doc/devices/res.shprim`:

```
schemify 1 shprim
  doc "Resistor: SPICE R element"
  prefix R
  category passive

pin p inout 0 -30
pin n inout 0 30

param value 1k
  kind string
  unit Ohm
  doc "resistance: SPICE value text (a number, an expression, or a value plus device parameters)"
param m 1
  doc "parallel multiplier"

impl spice spice
  format spice "{name} {p} {n} {value} m={m}"
  format spectre "{name} ({p} {n}) resistor r={num(value)} m={m}"

line 0 -30 0 -15
line 0 15 0 30

poly 0 -15 5 -12.5 -5 -7.5 5 -2.5 -5 2.5 5 7.5 -5 12.5 0 15
  width 2

stext 10 -9 R0 {name}
  size 0.14
stext 10 0 R0 {value}
  size 0.14
```

- The header attributes: `doc` is the one-line description, `prefix` starts new instance names
  (`R1`, `R2`), `category` groups the cell in the library browser.
- `pin` records are the interface, in port order: name, direction (`in`, `out`, `inout`),
  position on the symbol.
- `param` records declare parameters with their defaults. `kind`, `unit`, `min`, `max` and
  `doc` describe them; the editor's properties dialog shows the defaults.
- `impl spice spice` is the device definition: one `format` template per netlist dialect, with
  `{name}`, pins and parameters substituted. The `spice` template also serves ESPice, Xyce and
  CDL netlists unless the cell gives a more specific one.
- `line`, `poly`, `stext` and the other shape records draw the symbol. `stext` prints the
  instance name and parameter values next to it.

### `.shcomp`: a component with a schematic

An inverter, shortened from `crates/app/tests/fixtures/inv/inv.shcomp`:

```
schemify 1 shcomp
  doc "CMOS inverter (editor fixture)"
  prefix x

pin A in -40 0
pin Y out 40 0
pin VDD inout 0 -30
pin VSS inout 0 30

param wp 2
  unit um
param wn 1
  unit um

impl sch schematic

poly -20 -20 15 0 -20 20 -20 -20
  closed true

inst m00001 dev:pmos
  at 0 -50 R0
  name M1
  W wp
inst m00002 dev:nmos
  at 0 50 R0
  name M2
  W wn

wire w00001 20 -20 20 20
wire w00002 20 0 80 0

label n00001 50 -50 R0 VDD
label n00002 50 50 R0 VSS

port p00001 -60 0 R0 A
port p00002 80 0 R180 Y
port p00003 20 -110 R90 VDD
port p00004 20 110 R270 VSS

text t00001 -60 -150 R0 "CMOS inverter"
  size 0.4
```

- `impl sch schematic` says the cell's body is its schematic. A `.shcomp` has exactly one.
- `inst <id> <cell>` places a cell. `at` is the position and orientation, `name` the instance
  name; every other attribute overrides a parameter of the placed cell (here `W` gets the
  expression `wp`, the inverter's own parameter). Parameters left at their default are not
  written.
- `wire`, `label`, `port`, `noconn` and `text` are the rest of the schematic. Labels and ports
  name nets; a `port` ties a net to the pin of the same name.
- The first value after `inst`, `wire`, `label` and the other schematic record words is an id.
  The editor makes up random six-character ids; diff and merge use them to match objects
  between versions, so keep them when you edit a file by hand and give a new object an unused
  one (1 to 13 characters of digits and lowercase letters other than `i`, `l`, `o`, `u`).

A `.shcomp` can also carry a docs page (`prose` and `var` records, see
[docs-pages.md](docs-pages.md)) and `code` blocks of SPICE text that go into its subcircuit
definition ([netlisting.md](netlisting.md)).

### `.shbench`: a testbench

The RC bench from `crates/app/tests/fixtures/rc/tb.shbench`:

```
schemify 1 shbench
  doc "RC low-pass step response: a 1 V step charges C through R"
  corner tt

inst 0aaaaa ./res
  at 0 0 R0
  name R1
  R R
inst 0aaaab ./cap
  at 0 90 R0
  name C1
  C C

wire 0ccccc 0 30 0 60

label 0bbbba 0 -30 R0 in
label 0bbbbb 0 30 R0 out
label 0bbbbd 0 120 R0 0

prose 0ddddd
  | # RC low-pass
  |
  | A 1 V step charges $C$ through $R$: $v_{out} = 1 - e^{-t/\tau}$ with $\tau = RC$.

var R 3.3k
  unit Ohm
  min 100
  max 10k
var C 1u
  unit F

corner tt
  set C 1u

output tau "cross(v(out), 0.6321205588285577)"
output v1m "at(v(out), 1m)"

spec v1m
  min 0.3
  max 0.9

code tran spice
  | V1 in 0 1
  | .ic v(out)=0
  | .op
  | .tran 1u 5m 0 1u uic
```

- A bench has no pins or symbol: it is a top level and the only file kind you can simulate.
- `var` records are the bench variables, used by instance parameters (`R R`), sweeps and the
  optimizer. `corner`, `sweep`, `montecarlo` and `optimize` set up the jobs.
- `output` records measure one number per job from the waveforms; `spec` sets pass limits.
- `code` blocks hold SPICE text (sources, analyses, `.ic`) that goes into the netlist as
  written. Lines starting with `|` are kept verbatim.

Running benches is covered in [simulation.md](simulation.md).

## Cell references

Inside a file an instance names its cell in one of two ways:

| Form | Resolves to |
|---|---|
| `lib:cell` | Cell `cell` at the top directory of library `lib`, as declared in `schemify.project` (`core:res`, `dev:nmos`) |
| `./cell` | Cell `cell` in the same directory as the file that places it |
| `tech:<class>` | The current PDK's device for a technology class ([../spec/pdk.md](../spec/pdk.md)) |

Schemify looks for `<cell>.shcomp` first, then `<cell>.shprim`; a `.shbench` cannot be placed.
The project's own cells have no library name, so they place each other with `./cell`. A cell in
a subdirectory is reachable only with `./cell` from files in that same directory; declare the
subdirectory as a library of its own (`lib analog path analog`) to place its cells from
anywhere as `analog:cell`.

Messages and the checks panel show a cell's full name as `lib:dir/stem`
(`dev:nmos`, `core:res`), and a project cell without the `lib:` part (`inv`, `blocks/bias`).

## The bundled core library

These 23 devices ship inside the binary as the `core` library. Place them as `core:<cell>`. They
open read-only in the editor.

| Cell | Device | Pins |
|---|---|---|
| `res` | Resistor (R) | p n |
| `cap` | Capacitor (C) | p n |
| `ind` | Inductor (L) | p n |
| `parax_cap` | Parasitic capacitor to an inherited reference net (C) | p gnd |
| `diode` | Diode (D) | p n |
| `nmos4`, `pmos4` | 4-terminal MOSFETs (M, or X with `spiceprefix X` for subcircuit models) | d g s b |
| `npn`, `pnp` | Bipolar transistors (Q) | c b e |
| `vsource` | Voltage source (V); `value` is the SPICE source text | p n |
| `isource` | Current source (I) | p n |
| `vcvs`, `vccs` | Voltage-controlled voltage and current sources (E, G) | p n cp cn |
| `ccvs`, `cccs` | Current-controlled voltage and current sources (H, F) | p n |
| `bsource` | Behavioural source (B) | p n |
| `ammeter` | 0 V source used to measure current | p n |
| `switch` | Current-controlled switch (W) | p n |
| `vswitch` | Voltage-controlled switch (S) | p n cp cn |
| `tline` | Lossless transmission line (T) | p1 n1 p2 n2 |
| `probe` | Voltage probe: adds a `.save` for its net | p |
| `gnd`, `vdd` | Ground and supply symbols | p |

`gnd` and `vdd` emit nothing into the netlist. To name a ground net, place a label `0` (as the
RC bench above does). The sources live in [`crates/doc/devices/`](../../crates/doc/devices/) if you want to read a
cell's parameters and templates.

## The `.schemify/` directory

Schemify writes everything it generates under `.schemify/` in the project root:

| Path | Contents |
|---|---|
| `journal/<cell path>.journal` | Edit journal per cell: undo history and crash recovery |
| `netlist/<cell>.<ext>` | Netlists written by the editor's `n` |
| `runs/<design hash>/` | Simulation jobs: netlists, logs, raw files, results |
| `build/` | Cache of compiled models |
| `export/` | Image exports and project zips |
| `pdks/` | PDKs installed by the PDK manager plugin |

Do not commit it. Add it to `.gitignore`:

```sh
echo '.schemify/' >> .gitignore
```

Deleting it loses unsaved-edit recovery, undo history from earlier sessions, old simulation
results and installed PDKs. Your design files are not in it.

## Version control

### Canonical form: `schemify fmt`

The editor always saves files in one canonical form: records in a fixed order, schematic
objects sorted by id, coordinates in their shortest spelling, two-space indentation. The same
design always gives the same bytes, so diffs show only real changes. After editing a file by
hand, normalize it:

```sh
schemify fmt inv.shcomp top.shcomp       # rewrite in place
schemify fmt --check *.shcomp *.shbench  # CI: exit 1 if any file is not canonical
```

### Comparing two versions: `schemify diff`

`schemify diff A B` lists records added (`+`), removed (`-`) and changed (`~`), by object rather
than by text line, with the attributes that changed:

```sh
git show HEAD:top.shcomp > /tmp/top-old.shcomp
schemify diff /tmp/top-old.shcomp top.shcomp
```

```
~ inst x00001 [attrs: wp]
+ wire 7hq2mx
```

A moved object shows up as `[attrs: at]`; a reordered file with the same content shows nothing.
The exit code is 0 when the files are the same and 1 when they differ, like `diff`.

### Merging branches: `schemify git-setup`

Git merges text line by line, which can break a design file in ways that still look like text.
Schemify ships a merge driver that merges per object instead. Register it once in each clone:

```sh
cd my_project
schemify git-setup
git add .gitattributes
git commit -m "Use the Schemify merge driver"
```

`git-setup` adds two settings to the clone's `.git/config` (`merge.schemify.name` and
`merge.schemify.driver`, which runs `schemify merge-driver %O %A %B %P`) and these lines to
`.gitattributes`:

```
*.shprim merge=schemify
*.shcomp merge=schemify
*.shbench merge=schemify
```

Commit `.gitattributes` so everyone shares it. Git never reads driver commands from the
repository itself, so every collaborator runs `schemify git-setup` in their own clone, and
`schemify` must be on `PATH` when git merges.

With the driver, two people can edit different objects of the same cell, or different
attributes of the same object, and the merge is clean. An instance moved on one branch and
resized on the other keeps both changes.

### Conflicts

A conflict happens when both branches change the same part of one object differently. The
driver writes the merged file with each conflicting record twice, between markers, and git
reports the file as conflicted:

```
<<<<<<< ours
inst x00001 ./inv
  at 0 0 R0
  name x1
  wp 4
=======
inst x00001 ./inv
  at 0 0 R0
  name x1
  wp 8
>>>>>>> theirs
```

The rest of the file is merged. To resolve it:

1. Open the file in a text editor and keep one version of each record (or combine them by
   hand). Delete the marker lines.
2. Run `schemify fmt` on the file. It fails on a leftover marker, so this also checks that
   you got them all.
3. `git add` the file and finish the merge.

A file with markers does not open in the editor: resolve it first. If one side of a merge does
not parse at all, the driver leaves your version of the file in place and git marks it as
conflicted; merge that file by hand.

See also: [editor.md](editor.md), [cli.md](cli.md), [migrating.md](migrating.md),
[../spec/format.md](../spec/format.md).
