# Simulation

You simulate a **bench**: a `.shbench` file that instantiates the circuit under test, adds
stimulus, and lists what to measure. A `.shcomp` cell never simulates by itself. Pressing `s`
on one tells you to make a bench for it first.

This page covers what a bench holds, running it from the editor and from the command line,
picking a simulator, where results go, and the waveform viewer. Full rules are in
[run.md](../spec/run.md); the record syntax is in [format.md](../spec/format.md).

## Make a bench

Open the cell and choose **Simulation → New bench from this component**. Schemify writes
`<stem>_tb.shbench` beside the cell with one instance of it at the origin, a label on every pin
(the net is named after the pin) and a `code` block holding `.op`. Add sources and loads on the
schematic, then the records below.

## What a bench holds

A bench is a schematic plus records. This is the RC fixture
(`crates/app/tests/fixtures/rc/tb.shbench`), trimmed to the simulation part:

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

The header's `corner tt` makes `tt` the default corner. A `config <name>` header attribute does
the same for configs.

### Variables

`var <name> <expr>` declares a bench variable. Instances use it by name (`R R` sets the
resistor's `R` parameter to the bench var `R`). Vars are what corners, sweeps, Monte Carlo and the
optimizer change. `min` and `max` bound the value; the optimizer also takes its search range from
them. Expressions use the [expression language](../spec/expr.md): SI suffixes (`3.3k`, `1u`,
`2meg`), arithmetic, `if`, `sqrt`, and so on.

### Code blocks

Schemify adds no analysis of its own. You write the analyses, sources and options in `code`
blocks, as verbatim lines:

```
code tran spice
  | V1 in 0 1
  | .ic v(out)=0
  | .op
  | .tran 1u 5m 0 1u uic
```

The second word is the language: `spice` goes to every SPICE dialect, `ngspice`, `xyce` or
`spectre` to that dialect only. Put `.op` in a block when an output reads operating-point values
(see `op()` below).

### Corners

A corner sets model sections, include files, var overrides and the temperature:

```
corner tt
  lib ${PDK_MODELS} tt
  set C 1u
corner ff
  lib ${PDK_MODELS} ff
  set C 0.5u
  temp 85
```

`lib <path> <section>`, `include <path>` and `set <var> <expr>` can repeat. `${NAME}` expands
from the project manifest's `set` entries (the PDK manager writes `PDK_MODELS`), then from the
environment. A relative path names a file beside the bench; a path that starts with a project
`${NAME}` is relative to the project root. Nothing in a path executes.

### Configs

A config picks which implementation of a cell to netlist (a behavioural Verilog-A model instead of
the transistor schematic, for example):

```
config behavioural
  cell ./amp va
  order beh sch
```

`cell <cellref> <impl>` and `inst <path> <impl>` pin one cell or one instance; `order` is the
fallback preference list.

### Sweeps

```
sweep rsw
  var R
  values 1k 2k 3k
sweep fsw
  var C
  log 1p 1n 7
```

Each sweep names one var and exactly one of `values <expr>…`, `lin <from> <to> <n>` or
`log <from> <to> <n>`. `lin` and `log` bounds are constants and include both ends; `n` is 1 to
65536. Several sweeps nest, the first one outermost.

### Monte Carlo

Give each var that should vary a `dist`, then add a `montecarlo` record:

```
var R 3.3k
  dist gauss 50
var VDD 1.8
  dist uniform 0.05 rel

montecarlo mc
  samples 200
  seed 11
```

`dist gauss <spread>` draws a normal value with standard deviation `spread`; `dist uniform
<spread>` draws uniformly over plus or minus `spread`. `rel` makes the spread relative to the
value. `samples` is required (up to 2^20); `seed` defaults to 0.

Draws are reproducible: sample `k` of a var is the same in every corner, sweep point and config of
the run, and adding a var does not change the others' draws. Each sample also fixes the
simulator's own random seed (the netlist gets `.option seed=<n>`), so a corner whose `lib` section is
the PDK's Monte Carlo section draws the same PDK statistics for the same sample every time.

### Outputs

An output is a calculator expression evaluated on each job's raw file. It must reduce to one
number:

```
output tau "cross(v(out), 0.6321205588285577)"
output v1m "at(v(out), 1m)"
output ugf "cross(db(v(out)), 0)"
output pm  "phase_margin(v(out))"
output gm1 "op(M1, gm)"
```

Signals are `v(net)` and `i(dev)`; bench vars are visible by name. The reductions you will use
most are `at`, `cross`, `ymax`, `ymin`, `pp`, `avg`, `rms`, `delay`, `settle`, `gain_margin` and
`phase_margin`. The full list is in [expr.md](../spec/expr.md) §5.

`op(inst, param)` reads a device's DC operating point: `op(M1, gm)`, `op("/X1/M2", "id")`.
The path is the instance path from the bench. For a PDK device wrapped in a model subcircuit,
continue the path into the wrapper with the inner device name, for example
`op("M1/msky130_fd_pr__nfet_01v8", "gm")`. The bench must run `.op` in a `code` block; without
it the output reports `X010 … add .op to the bench` rather than a 0. Values are in SI units.
`op()` works with ngspice only.

### Specs

```
spec v1m
  min 0.3
  max 0.9
```

A spec names an output and bounds it. Either bound may be left out. A job passes when the value
lies within the bounds; a missing value is *unknown*.

### Optimize

```
optimize fit
  algo nelder-mead
  goal tau target 1m
  vary R
  budget 12
```

`goal <output> min|max|target <value> [weight <w>]` repeats; `vary <var>` repeats and takes its
range from the var's constant `min` and `max`. `algo` is `nelder-mead` (default) or `random`;
`budget` is the evaluation limit (default 100); `seed` defaults to 1. Add `config <name>` and
`corner <name>` lines to evaluate every candidate at each pair; the worst job per goal counts.

## Run from the editor

Open the bench and press `s` (**Simulation → Simulate**). The editor stays responsive while jobs
run, and the results panel opens at the bottom.

`s` selects every corner and every sweep of the bench and its first Monte Carlo record, then
runs the product: a bench with two corners, a three-point sweep and four Monte Carlo samples runs
2 × 3 × 4 = 24 jobs. Before any job starts, the design is checked; errors block the run
and show in the checks panel.

| Action | How |
|---|---|
| Simulate | `s`, **Simulation → Simulate**, toolbar **Simulate** |
| Cancel | **Simulation → Cancel simulation**, or **Cancel** in the results bar |
| Optimize | **Simulation → Optimize** (the bench's first `optimize` record) |
| Show or hide results | **Simulation → Results table** |
| Netlist | `n` (whole hierarchy), `Shift+N` (this level only) |
| Simulator | **Simulation → Netlist dialect** |
| Full logs | **Simulation → View last job data**, **View last job errors** |

You can start runs of several benches at once. A second Simulate of a bench that is still running
waits until its current run finishes.

### The results table

Each row is one job: its index, its key (config, corner, sample and var assignments), its state
(`pending`, `queued`, `running`, `done`, `failed`, `cancelled`), one column per output and one
per spec. Click a column header to sort, again to reverse. The filter field keeps rows that
contain every word you type (Enter applies), so `fail ff` lists the failing jobs of corner `ff`.

With Monte Carlo samples and at least one spec, an **MC** row on top shows each output's sample mean and each spec's
yield with its 95% confidence interval. Failed jobs and missing values count as failures.

Clicking a job's index selects it. The selected job drives the waveform viewer, the plots on the
docs page and the operating-point annotation. Below the table you see that job's run log: the
simulator command line and the last lines of its stdout and stderr, so a failing simulator's
message is visible without opening a file. Earlier runs of the bench (up to 8) are listed under
the table as **run** buttons.

### Operating-point annotation

**Op Annotate** (Waves menu, or **OP** in the results bar) is on by default. After a run with an
operating point, the schematic shows node voltages and, per MOSFET, `id`, `gm`, `gds`, `vgs`,
`vds` and `vdsat`. While it is on, Simulate adds `.save` lines to each ngspice netlist so those
quantities exist in the raw file. Descend into a subcircuit to see its devices.

### Optimizer results

When **Optimize** finishes, the best candidate becomes a staged edit on the bench: nothing in the
file changes yet. The docs page shows a banner with **Accept** and **Reject**, and the new value
beside each varied var. The canvas outlines what the edit changes. With several staged edits,
`<` and `>` on the banner step through them. **Edit → Accept staged edit** and **Reject staged
edit** do the same.

The results panel shows a **best** row with the best candidate's job and varied values.

### Stale results

Results belong to the exact design they came from. When you edit the bench or any cell it reaches,
the results title gains `[stale: the design changed since this run]`. Opening a bench loads its
newest run, marked stale if the design has changed since.

### The netlist tab

`n` writes the netlist to `.schemify/netlist/` and shows it in a read-only text tab. Type in its
find field and press Enter to list the matching lines with their line numbers; clear the field to
see the text again. Very large netlists show the first 2^14 lines; the file has all of it.

## Run from the command line

```sh
schemify run crates/app/tests/fixtures/rc/tb.shbench
```

| Flag | Effect |
|---|---|
| `--config C` | run config `C` (repeatable) |
| `--corner K` | run corner `K` (repeatable) |
| `--sweep S` | run sweep `S` (repeatable; nested in the order given) |
| `--mc M` | run Monte Carlo record `M` |
| `--set VAR=EXPR` | override a bench var (repeatable, applied last) |
| `--jobs N` | at most `N` simulators at once (default: the number of cores) |
| `--dialect D` | `ngspice` or `espice` |

Unlike `s` in the editor, the CLI runs only what you name: without flags it runs one job at the
default config and corner, with no sweep and no Monte Carlo. Naming a corner or sweep the bench
does not have is an error. There is no optimize flag; optimize from the editor.

```sh
schemify run tb.shbench --corner tt --corner ff --sweep rsw --mc mc --jobs 8
schemify run tb.shbench --set R=1k
```

`schemify run` prints the results file on stdout and each error row on stderr. It exits 0 when
every job succeeded and no spec failed, 1 on a failed job or spec, 2 on a usage error. The RC
fixture exits 1: with `R = 3.3k` the output `v1m` is about 0.26, below its spec minimum of 0.3.

## Choose a simulator

Schemify runs ngspice (the default) or ESPice.

- Command line: `--dialect ngspice` or `--dialect espice`.
- Editor: **Simulation → Netlist dialect**. ESPice runs ESPice; any other dialect runs ngspice.
- Default for both: `schemify.opt.simulator` in your `init.lua`.

Command lines come from your user config, never from a design file:

```lua
schemify.sim("ngspice", { path = "/opt/ngspice/bin/ngspice", args = { "-b", "-r", "{raw}", "{netlist}" } })
schemify.sim("espice", { path = "/opt/espice/bin/espice", env = { ZIG = "/opt/zig-0.17/bin/zig" } })
schemify.opt.simulator = "espice"
```

Without a `schemify.sim` row the defaults are `ngspice -b -r {raw} {netlist}` and
`espice -b --tokenizer=ngspice -r {raw} {netlist}`. See [config.md](config.md).

Differences that matter:

| | ngspice | ESPice |
|---|---|---|
| `op(inst, param)` outputs | yes | no (`X006`) |
| Device values in Op Annotate (`gm`, `id`, …) | yes | no |
| Verilog-A | compiled with OpenVAF to OSDI | read as source; compiled by ESPice with Zig 0.17 (`ZIG` or `zig` on `PATH`) |

A simulator still running after one hour is killed and its job fails.

## Verilog-A and Verilog models

A leaf cell can carry a Verilog-A or Verilog implementation, either as a file or inline:

```
impl va veriloga
  model ampm
  file va/amp.va
```

`file` is relative to the cell's directory. An inline source goes in a `body va` record instead
of `file`. Before the first job, Schemify compiles each source that is missing or newer than its
output:

- Verilog-A (ngspice): `openvaf <src> -o .schemify/build/<stem>.osdi`
- Verilog (ngspice): `iverilog -o .schemify/build/<stem>.vvp <src>`

Point Schemify at the compilers with `schemify.sim("openvaf", { path = … })` and
`schemify.sim("iverilog", { path = … })`. OpenVAF-Reloaded installs `openvaf-r`; name that as
the path. Files a Verilog-A source pulls in with `` `include `` are copied next to the job too.
Compiler output goes to `.schemify/build/<out>.log`; a failed compile fails every job that needs
it and names the source.

ESPice runs compile nothing ahead of time: the Verilog-A source is copied into the job directory,
so you need no OpenVAF.

Verilog co-simulation through ngspice `d_cosim` is compiled and wired, but not yet tested end to
end.

## Where results live

```
<project>/.schemify/
  runs/<first 16 hex digits of the design hash>/
    results            outputs, specs, var values per job
    bench              the bench's name
    0/                 one directory per job
      netlist.cir
      out.raw
      stdout.log
      stderr.log
      command.log      the simulator command line
  build/               compiled Verilog-A / Verilog
  netlist/             netlists written by `n`
```

`.schemify/` is git-ignored. The `results` file is plain text:

```
schemify 1 results
  design cef819755aace97c0019c295f1cec286d58686251414eee778f41f515dff6eeb

output tau
output v1m

spec v1m

var R
var C

job 0 done
  corner tt
  var R 3300
  var C 1e-6
  value tau 0.003299999981230017
  value v1m 0.26142328534313897
  pass v1m fail
```

The same design always gives the same run directory, so runs of an unchanged design land in one
place, and a changed design never mixes with old results.

Simulation does not run in the web build yet (see [web.md](web.md)).

## The waveform viewer

The viewer docks under the schematic. Open it by selecting a job in the results table and clicking
**Waves**, or from the **Waves** menu (**External viewer**, or **Op**, **Dc**, **Ac**, **Tran**,
… to pick a raw file). It reads ngspice, Xyce and LTspice raw files, binary or ASCII. `Ctrl+W`
closes it.

When you pick another job or a run finishes, the dock keeps its traces and panes and re-reads the
new raw file. A trace that no longer evaluates is dropped and named in the status line.

### Adding traces

- Double-click a signal in the browser on the left, or select it and click **Add**. The filter
  field above the list narrows it as you type; `<` and `>` step through the datasets
  (analyses) of the file.
- Type a calculator expression in the **Expression** field and press Enter or click **Plot**:
  `db(v(out))`, `v(out) - avg(v(out))`, `deriv(v(out))`, `fft(v(out))`.
- Cross-probe: with the dock open, click a net on the schematic to add `v(net)`, or an instance
  pin to add the current into that pin. When the raw file lacks that current, you get the pin's
  net voltage instead.

A bus trace is written `{v(d3), v(d2), v(d1), v(d0)}`, MSB first. `op()` is for outputs only and
does not work in a trace.

### Panes

**+ Pane** and **- Pane** add and remove panes (up to 16). **Next pane** moves the selected trace,
**Bold** thickens it, **Delete** (or `Del`) removes it. **Log X** and **Log Y** switch the axes.

Panes share one x axis by default. Double-click a plot to open its properties: mode (`analog`,
`digital`, `histogram`), logic threshold, bus radix (`hex` or `dec`), histogram bins, the sweep
step to show, unlock x, the axis limits, cursor positions and the trace colour. Fields accept SI
suffixes (`1.5u`, `2meg`).

Results on their own axis (an `fft`, or a per-step measurement of a swept run) go to a separate
unlocked pane, since they do not share the time axis.

### Mouse and keys

| Input | Effect |
|---|---|
| Wheel | zoom x about the pointer |
| `Shift`+wheel, or wheel over the y axis | zoom y |
| Left drag | pan |
| Right drag | box zoom |
| Double-click | pane properties |
| `f` | fit everything |
| `a`, `b` | place vertical cursor a or b at the pointer |
| `s` | swap cursors a and b |
| `Shift+A`, `Shift+B` | horizontal cursors in the pane under the pointer |
| `m` | measurement tooltip (`x=`, `y=` at the pointer) |
| `t` | track the sweep step closest to the pointer; again for all steps |
| Left, Right | pan |
| Up, Down | zoom in, zoom out |

Drag a cursor within 6 px to move it. The **Cursors** readout lists each trace's value at a and b
and their difference, one row per sweep step.

See also: [docs-pages.md](docs-pages.md) for plots on a docs page, [netlisting.md](netlisting.md),
[waves.md](../spec/waves.md), [run.md](../spec/run.md).
