# A first session

This walk-through takes a CMOS inverter from the schematic to a waveform: you open a cell, build a testbench around it by placing and wiring sources, netlist it, simulate it with ngspice and read the results. A second part runs a finished RC bench from the command line and shows what outputs, specs and the optimizer add.

You need Schemify installed ([install.md](install.md)), ngspice on your `PATH`, and a checkout of the repository for the example files. The keys below are the default ones, which follow xschem. A letter key means the plain letter: `w` is lower-case w, `Shift+R` is upper-case R. Press `/` in the editor at any time for the full key list.

## Copy the example project

The examples live in the repository's test fixtures. Copy them so your edits do not touch the checkout:

```sh
mkdir -p ~/schemify-tour
cp -r crates/app/tests/fixtures/inv crates/app/tests/fixtures/rc ~/schemify-tour/
```

`~/schemify-tour/inv` is a project: a directory with a `schemify.project` file. It holds two cells and a small device library:

```
inv/
  schemify.project     the project manifest
  inv.shcomp           a CMOS inverter
  top.shcomp           a buffer made of two inverters
  dev/nmos.shprim      device symbols with their SPICE lines
  dev/pmos.shprim
```

The manifest declares the `dev` library, so cells can place `dev:nmos`:

```
schemify 1 project

lib dev path dev
```

The bundled device library (`core:res`, `core:cap`, `core:vsource`, `core:nmos4`, ...) is always available and needs no line in the manifest.

## Open a cell

```sh
schemify edit ~/schemify-tour/inv/top.shcomp
```

Schemify opens the cell and treats the nearest directory above it that holds `schemify.project` as the project. `top.shcomp` is a two-inverter buffer. Its file is short enough to read whole:

```
schemify 1 shcomp
  doc "Two-inverter buffer: an inverter cell used as a hierarchical sub-cell"
  prefix x

pin IN in -40 0
pin OUT out 40 0

impl sch schematic

rect -20 -20 20 20

line -40 0 -20 0
line 20 0 40 0

stext -20 -28 R0 {name}
  size 0.25
stext -14 -5 R0 buf2
  size 0.18

inst x00001 ./inv
  at 0 0 R0
  name x1
  wp 4
inst x00002 ./inv
  at 140 0 R0
  name x2

wire w00001 40 0 100 0
wire w00002 -80 0 -40 0
...
label n00001 70 0 R0 mid
label n00002 70 -70 R0 VDD
label n00003 70 70 R0 VSS

port p00001 -80 0 R0 IN
port p00002 240 0 R180 OUT
...
```

One `.shcomp` file holds the cell's symbol (the `pin`, `rect`, `line` and `stext` records), its schematic (`inst`, `wire`, `label`, `port`) and, optionally, a docs page. Each object is one record with a stable id, so moving a part changes one line in a diff. `./inv` places the cell `inv.shcomp` from the same directory; `wp 4` overrides that instance's `wp` parameter and leaves every other parameter at its default. The full format is in [files.md](files.md).

Look around:

| Key | Does |
|---|---|
| `f` | zoom to fit everything |
| `Shift+Z` / `Ctrl+Z` | zoom in / out |
| arrow keys | pan |
| click `x1`, then `e` | descend into the inverter's schematic |
| `Ctrl+E` or `BackSpace` | back to the parent |
| `i` | descend into the selected instance's symbol |
| `Alt+D` | show or hide the cell's docs page |
| `/` | key summary |

## Make a testbench

Only a `.shbench` file can be simulated. A component stays reusable; the bench around it holds the sources, analyses and measurements. Press `s` on `top.shcomp` and the status line tells you so.

Open the inverter itself with `Ctrl+O` and pick `inv.shcomp`, then choose **Simulation → New bench from this component**. Schemify writes `inv_tb.shbench` next to the cell and opens it in a new tab. The new file holds one instance of the inverter, a net label on each of its pins and an operating-point analysis (your ids differ):

```
schemify 1 shbench

inst 3f9c1a ./inv
  at 0 0 R0
  name x1

label 0d51c8 40 0 R0 Y
label 52aa17 0 30 R0 VSS
label 7b20e4 -40 0 R0 A
label 9e33f0 0 -30 R0 VDD

code op spice
  | .op
```

Each label names the net on its pin after the pin, so the nets are `A`, `Y`, `VDD` and `VSS`.

## Place and wire the sources

**Ground the inverter.** Double-click the `VSS` label (or select it and press `q`), change the net name to `0` and press OK. Net `0` is SPICE ground.

**Add the supply.** Press `Insert` (or `Shift+I`) to open the insert-symbol chooser and type `vsource`. When the filter leaves one match it is picked for you, and the symbol follows the pointer. Click to the right of the inverter to drop it; it is named `V1`. Press `q` with `V1` selected and set `value` to `1.8`. Fields you leave empty keep the cell's default.

**Label the supply.** Press `Alt+L`, type `VDD`, press OK, and click on `V1`'s top pin. Press `Alt+L` again, type `0`, and click on its bottom pin. Two labels with the same name are the same net, so `V1` now drives the inverter's `VDD`.

**Add the input and wire it.** Insert another `vsource` to the left of the inverter (`V2`) and set its `value` to:

```
pulse(0 1.8 1n 100p 100p 5n 10n)
```

Now draw a wire from `V2`'s top pin to the inverter's input: put the pointer on the pin, press `w`, then click on the end of the `A` label (the inverter's `A` pin). `Space` cycles the corner routing while you draw; `Shift+W` starts the wire at the nearest pin to the pointer instead. Finish `V2` with a `0` label on its bottom pin.

Useful while placing:

| Key | Does |
|---|---|
| `Shift+R` | rotate (also while a symbol follows the pointer) |
| `m` | move the selection |
| `Del` | delete the selection |
| `u` / `Shift+U` | undo / redo |
| `Esc` | abort the current operation and unselect |

**Add the analyses.** Analyses and model cards are SPICE text in a `code` block. Press `Alt+Q` (*Edit file*) to edit the bench as text and replace the `code op spice` block at the end with:

```
output y_at_4n "at(v(y), 4n)"

code op spice
  | .model nmos nmos level=1 vto=0.5 kp=100u
  | .model pmos pmos level=1 vto=-0.5 kp=40u
  | .op
  | .tran 10p 20n
```

The two `.model` cards give the fixture's `nmos` and `pmos` devices something to simulate (their SPICE lines name the models `nmos` and `pmos`). The `output` record measures `v(y)` at 4 ns, while the input is high; it appears as a column in the results. Apply with OK (or `Ctrl+Enter`); the whole change is one undo step.

Press `Ctrl+S` to save.

## Netlist

Press `n`. Schemify checks the design, writes the netlist to `.schemify/netlist/inv_tb.spice` in the project and shows it in a read-only tab, with the inverter as a subcircuit and your code block at the top level. `Ctrl+W` closes the tab; `Ctrl+Left` / `Ctrl+Right` switch tabs. If the check finds errors, nothing is written and the checks panel lists them; click a row to jump to the object.

The default dialect is ngspice. `Ctrl+Shift+V` cycles the dialect, or pick one under **Simulation → Netlist dialect**. The same netlist from the command line:

```sh
cd ~/schemify-tour/inv
schemify netlist inv_tb.shbench                       # to standard output
schemify netlist inv_tb.shbench --dialect cdl -o inv_tb.cdl
schemify check inv_tb.shbench                         # only the checks
```

`.schemify/` holds generated files (netlists, runs, build caches). Add it to your `.gitignore`; Schemify does not do that for you.

## Simulate

Press `s` with the bench open. The run starts in the background, so you can keep editing; the results panel opens below the canvas and fills in when ngspice finishes. It has one row per job (here one), with a column for `y_at_4n`. Expect a value close to 0 V: at 4 ns the input is high and the output low.

Because the bench runs `.op`, the schematic is annotated with the operating point: node voltages on the nets and `gm`, `id` and other quantities beside each MOSFET once you descend into `x1`. The **OP** button in the results panel (or **Waves → Op Annotate**) turns the annotation off and on.

Each run lives in `.schemify/runs/<hash>/` with one directory per job holding the netlist, the raw file and the simulator's logs. When a simulation fails, **Simulation → View last job errors** shows what ngspice printed.

## Look at waveforms

Click **Waves** in the results panel to open the selected job's waveforms in the waves dock. With the dock open, click a net in the schematic to plot its voltage: click the `A` and `Y` nets to see the input pulse and the inverted output. A click on an instance pin plots the current into it. To send several nets at once, select them, press `k` to highlight them and then `Alt+G`.

In the dock, `f` fits the traces. The viewer has panes, cursors, digital and histogram modes; [simulation.md](simulation.md) covers it.

From the command line the same simulation is:

```sh
schemify run inv_tb.shbench
```

It prints the results (one `job` block per job with a `value` line per output) and exits with 0, or 1 when a job fails or a spec fails.

## A complete bench: RC low-pass

The `rc` project is a bench that uses most of what a `.shbench` can hold. Here it is whole:

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

text 0eeeee -60 -80 R0 "RC low-pass: V1 in 0 1 (code block), tau = R C"
  size 0.3

prose 0ddddd
  | # RC low-pass
  |
  | A 1 V step charges $C$ through $R$: $v_{out} = 1 - e^{-t/\tau}$ with $\tau = RC$.
  |
  | ```plot
  | v(out)
  | ```

var R 3.3k
  unit Ohm
  min 100
  max 10k
var C 1u
  unit F

corner tt
  set C 1u

optimize fit
  algo nelder-mead
  goal tau target 1m
  vary R
  budget 12

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

What each part does:

| Record | Meaning |
|---|---|
| `R R`, `C C` on the instances | the resistor's `R` and the capacitor's `C` are the bench variables `R` and `C` |
| `var` | bench variables with units and bounds; you sweep, optimize and override them |
| `corner tt` | a process corner; it can load model sections, set variables and the temperature. The header's `corner tt` makes it the default |
| `output` | a measurement over each job's waveforms, written in the calculator language ([simulation.md](simulation.md)) |
| `spec` | pass/fail limits on an output |
| `optimize` | a search: vary `R` until `tau` reaches 1 ms, in at most 12 evaluations |
| `prose` | the docs page: Markdown with LaTeX math and an embedded plot of `v(out)` |
| `code` | SPICE text placed in the netlist: the step source and the analyses |

Run it from the command line:

```sh
cd ~/schemify-tour/rc
schemify run tb.shbench
```

With R = 3.3 kΩ and C = 1 µF the time constant is 3.3 ms, so `v1m` is about 0.26 V, below the spec's 0.3 minimum. The job's block ends with `pass v1m fail` and `schemify run` exits with 1. Override the variable for one run:

```sh
schemify run tb.shbench --set R=1k
```

Now τ is 1 ms, `v1m` is about 0.63 V and the spec passes. `--corner`, `--sweep` and `--mc` pick corners, sweeps and Monte Carlo records the same way; see [cli.md](cli.md).

Open the bench in the editor (`schemify edit tb.shbench`), press `Alt+D` for the docs page and `s` to simulate. The plot block on the docs page draws `v(out)` from the run, and the results table shows `tau`, `v1m` and the spec verdict. Choose **Simulation → Optimize** to run the `fit` search: the best `R` it finds is not written straight into the file but offered on the docs page beside the variable, with Accept and Reject. Accept it, save, and simulate again.

If you change the schematic or a variable after a run, the results panel marks the results stale until you simulate again.

See also: [editor.md](editor.md), [files.md](files.md), [simulation.md](simulation.md), [netlisting.md](netlisting.md), [docs-pages.md](docs-pages.md).
