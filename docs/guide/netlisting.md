# Netlisting and checks

Schemify turns a cell into netlist text for a simulator or an LVS tool. The editor and the `schemify` command line use the same netlister, so a netlist written on your laptop is byte-identical to the one CI writes: the header carries no date, no path and no version.

Before any text is written, Schemify elaborates the design (resolves cells, connects nets, evaluates every parameter) and checks it. Errors stop the netlist; warnings are reported and the netlist is still written.

## Netlist a cell

In the editor, press `n` (Simulation → Netlist) to netlist the tab's top cell, or `Shift+N` (Simulation → Netlist this level) to netlist the cell shown at the current hierarchy level. The netlist is written to `.schemify/netlist/<cell>.<ext>` in the project and opens in a tab. `.schemify/` is git-ignored.

From a shell:

```sh
schemify netlist amp.shcomp                   # print to stdout, default dialect
schemify netlist tb.shbench --dialect xyce -o tb.cir
schemify netlist tb.shbench --corner ff --set vdd=1.95
```

The argument is a cell file. Its project is the nearest directory above it that holds `schemify.project`, and the file must be inside that project (a file outside every library of the project is refused). Any cell kind can be netlisted: a `.shbench`, a `.shcomp` or a `.shprim`.

| Flag | Effect |
|---|---|
| `--dialect D` | `ngspice`, `espice`, `xyce`, `cdl`, `spectre` or `verilog`. Default: `schemify.opt.simulator` from your [user config](config.md), else `ngspice`. |
| `--config C` | The bench config that selects implementations. Default: the bench's `config` header attribute. |
| `--corner K` | The bench corner. Default: the bench's `corner` header attribute. |
| `--set VAR=EXPR` | Override a bench variable (repeatable; later wins). An unknown name adds the variable. |
| `-o FILE` | Write to `FILE` instead of stdout. |
| `--allow-errors` | Write the netlist even when the checks found errors. |

When the checks find an error, `schemify netlist` prints the findings, then `not netlisted: fix the errors above or pass --allow-errors`, and exits 1. With `--allow-errors` an instance whose line cannot be written becomes a comment, `* error in <name>: <message>` (`//` for Spectre and Verilog), and an unconnected pin is written `?`.

## Pick a dialect

| Dialect | Use it for | Editor file |
|---|---|---|
| `ngspice` | Simulating with ngspice. The default. | `.spice` |
| `espice` | Simulating with ESPice. The ngspice text, except that Verilog-A sources load with `.hdl "<stem>.va"` lines and default Verilog-A instance lines carry their parameters (`m` written `mfactor`). | `.sp` |
| `xyce` | Xyce. `.temp` is written `.options device temp=…`; a `.control` block anywhere is an error (`E033`). | `.cir` |
| `cdl` | LVS. No code blocks, corners, `.param` or `.control`. The netlisted cell is the top `.SUBCKT` with a `*.PININFO` line per subcircuit, keywords in capitals. | `.cdl` |
| `spectre` | Spectre. `subckt`/`ends`, `parameters`, `include "<file>" section=<s>`. Names with characters outside `A-Z a-z 0-9 _` are escaped (`vdd\!`, `d\[3\]`). | `.scs` |
| `verilog` | Structural Verilog: one `module` per schematic cell, a `wire` per internal net, named port connections. | `.v` |

Choose the dialect in one of three places:

- On the command line, `--dialect`.
- In the editor, Simulation → Netlist dialect (ngspice, ESPice, Xyce, CDL, Spectre, Verilog). Options → Netlist format / Symbol mode offers the Spice, ESPice and Verilog entries of the same setting.
- As the default for both, `schemify.opt.simulator = "espice"` (or `"ngspice"`) in `init.lua`. See [config.md](config.md).

Simulate and `schemify run` use the dialect too: ESPice when it is `espice`, ngspice for any other dialect in the editor, and `schemify run` refuses anything but `ngspice` and `espice`. Schemify runs no Spectre, Xyce or LVS tool itself; it writes the file for you to hand to one. See [simulation.md](simulation.md).

Every core device has a Spectre line except `bsource`, `switch` and `vswitch`, which report `E013` for Spectre. The core analog devices have no Verilog line, so a schematic that places them reports `E013` in the `verilog` dialect: use it for digital cells that have a `format verilog` or a Verilog body.

## Where device lines come from

A device (`.shprim`) carries one `format` line per dialect in its `impl`. This is the bundled resistor, `devices/res.shprim`:

```
param value 1k
  kind string
  unit Ohm
param m 1

impl spice spice
  format spice "{name} {p} {n} {value} m={m}"
  format spectre "{name} ({p} {n}) resistor r={num(value)} m={m}"
```

A template is text with `{expr}` holes; `{{` and `}}` are literal braces. Each hole is an expression ([expr.md](../spec/expr.md)) and can use these names:

| Name | Value |
|---|---|
| `name` | The instance name, spelled for the dialect (`R1`, `X1[3]`). |
| `cell` | The subcircuit name for a schematic, the cell's file stem for a device. |
| `model` | The impl's `model` attribute, else the `model` parameter. |
| `pins` | Every connected net, in pin order, space-separated. |
| a pin name (`p`, `d`, `g`) | The net on that pin. A bus pin gives its bits space-separated. |
| `in_pins`, `out_pins` | The nets of `in` pins, and of every other pin. |
| `params` | `k=v` for each parameter whose value differs from the cell default. |
| `file` | The impl's `file` attribute as written, relative to the cell file. |
| a parameter name | Its evaluated value. A pin of the same name wins. |

Schemify evaluates every parameter to a number before writing it: the netlist holds `W=2u`, never a SPICE expression over design parameters. Functions such as `num()` and `spectre_source()` convert SPICE value text for Spectre, whose suffixes differ (`1M` is milli in SPICE and mega in Spectre).

A `format spice` line serves `ngspice`, `espice`, `xyce` and `cdl`, unless the device also has `format espice`, `format xyce` or `format cdl`, which wins for that dialect (a PDK's LVS line goes in `format cdl`). Spectre and Verilog never fall back to the SPICE line.

An impl without a `format` line for the dialect gets a default:

| Impl language | ngspice, ESPice, Xyce, CDL |
|---|---|
| `schematic`, `spice` text body | `X{name} {pins} {cell}` |
| `veriloga` | `N{name} {pins} {model}` (ngspice loads the compiled OSDI file; not available for Xyce or CDL) |
| `verilog` | `A{name} [{in_pins}] [{out_pins}] {model}` (ngspice `d_cosim`; ngspice only) |

The device letter of a default template is added only when the instance name does not already start with it: `X1` stays `X1`, `M1` becomes `XM1`. A dialect the selected impl cannot write is `E013`. A template that does not parse, or names something no pin, parameter or built-in provides, is `E008`.

## Hierarchy and subcircuits

Each `.shcomp` schematic becomes one subcircuit, `.subckt <cell> <ports in pin order>` … `.ends <cell>`. Instances refer to cells as `lib:cell` (a library from `schemify.project`) or `./cell` (the same directory). A missing library or cell is `E001`; a cell that instantiates itself at any depth is `E014`.

Schemify writes one definition per distinct set of parameter values. The set equal to the cell's defaults is named after the cell; every other set gets `<cell>__<6 characters>` from a stable hash. Instances with the same values share a definition, and a specialized subcircuit takes no parameters. From a test bench with two sizes of inverter chain:

```
X1 in out vdd! chain
X2 out out2 vdd! chain__h4shwd
.subckt chain IN OUT VDD
X1 IN net_0w0001 VDD inv
X2 net_0w0001 OUT VDD inv__8hc2h8
.ends chain
```

A cell can have several implementations (a schematic, a Verilog-A model, a SPICE body). A bench `config` record picks one per instance:

```
config beh
  cell ./amp veriloga
  inst /X1/X3 sch
```

For each instance the first match wins: an `inst <path> <impl>` rule, then a `cell <cellref> <impl>` rule, then the first name in `order` that the cell has, then the cell's `default`. Pass `--config beh` or set the bench's `config` header attribute. A rule naming an impl the cell lacks is `E031`; an `inst` rule whose path matches no instance is `W037`.

Other things that shape the hierarchy:

- Global nets are `0`, any name ending in `!`, and any label with `global true`. They are the same net everywhere, are never subcircuit ports, and get one `.global` line (ngspice).
- An inherited pin (`pin VDD inout 0 -40` with `inherit vdd!`) takes its net from the nearest ancestor instance with `inherit VDD <net>`, else its default. Inherited pins become ports after the ordinary ones.
- An instance with `ignore true` is not written. `ignore open` also leaves its pins unconnected; `ignore short` merges all its pin nets into one. Ignored instances are still checked.
- Labels carry buses (`d[3:0]`, `2*a,b`) and instance names iterate (`C[1:0]` writes `C[0]` and `C[1]`). Widths that do not fit are `E011`.

## Net and instance names

A net takes the name of its port, else its label. Two different names on one net are `E003`: the netlist uses the port name, else the bytewise smallest label. An unnamed net is `net_<id>`, from an id of one of its wires, labels or ports, so a generated name never collides with another net.

Instance and net names inside a subcircuit are local. Schemify keeps a name map between schematic paths and netlist names: instance `/X1/M1` is `X1.XM1`, net `/X1/n1` is `X1.n1`, and a global is its bare name at every depth. Lookups ignore case for the SPICE dialects, because ngspice lowercases names. Cross-probing and the operating-point annotation use this map ([simulation.md](simulation.md)).

In ngspice, a bus bit wired to an XSPICE vector port (a `verilog` impl's digital pins) is spelled `d_0` instead of `d[0]` throughout that netlist, because ngspice splits `d[0]` inside `[ … ]`. Every other bus bit keeps `d[0]`.

## Code blocks

A bench holds raw simulator text in `code` records. This is the RC fixture's transient setup:

```
code tran spice
  | V1 in 0 1
  | .ic v(out)=0
  | .op
  | .tran 1u 5m 0 1u uic
```

The language word decides which dialects receive the block:

| `code` language | Written for |
|---|---|
| `spice` | ngspice, ESPice, Xyce |
| `ngspice` | ngspice, ESPice |
| `espice` | ESPice |
| `xyce` | Xyce |
| `spectre` | Spectre |
| `verilog` | Verilog |

CDL takes no code blocks. Bench blocks are written after the top-level instance lines, sorted by name. A `code` record in a `.shcomp` is the component's own text (model cards, `.param` lines, XSPICE gates): it goes inside every definition of that cell, before `.ends`. A `.shprim` cannot hold `code`.

`${NAME}` in a code block, an impl body or a corner path expands from the project manifest's `set` entries, then the environment. An undefined name is `E034`; `$$` is a literal `$`. Nothing is executed.

## Check a design

```sh
schemify check tb.shbench
schemify check tb.shbench --config beh --corner ff
```

`schemify check` elaborates the cell and prints one line per finding on stderr, in the form `<code> <cell> [<path>] [<param>]: <detail>`. It exits 1 when any finding is an error, 0 otherwise. It takes `--config` and `--corner` like `netlist`. It checks against the ngspice dialect, so a device that has no line for another dialect (`E013`) shows up when you netlist with `--dialect`.

In the editor the same checks run live as you edit: open Simulation → Checks panel to see them.

The first letter of a code is its severity. `E` is an error: it blocks Simulate and `schemify netlist`. `W` is a warning, reported only. `I` is information.

| Code | Meaning | Usual fix |
|---|---|---|
| E001 | Missing library or cell, or a bench placed as an instance | Check the `lib:cell` name and the `lib` lines of `schemify.project`. |
| E002 | A schematic's ports and the cell's pins disagree | Add or rename the `port` or the `pin`. |
| E003 | Two different names on one net | Remove or rename one label. |
| W004 | Instance pin connected to nothing | Wire it, or place a noconn. |
| E005 | Parameter outside its `min`/`max` | Change the value, or waive it. |
| E006 | Parameter not one of its `choices` | Pick a listed value, or waive it. |
| W007 | Override of a parameter the cell does not have | Remove it or fix the spelling (it is kept, not written). |
| E008 | Expression error, or a `format` template that does not parse or names something unknown | Fix the expression; the detail carries the expression error. |
| E009 | Duplicate instance name in a cell (after iterated names expand) | Rename one instance. |
| W010 | A `real` pin on an analog or digital net | Check the pin domain. |
| E011 | Bus width mismatch | Make the label, pin and instance widths agree. |
| E012 | A `port` with no matching pin, or a pin with no `port` | Add the missing one. |
| E013 | The selected impl cannot write this dialect | Add a `format <dialect>` line, or select another impl. |
| E014 | Hierarchy recursion | Break the cycle. |
| W016 | A net with only one connection | Usually a dangling wire or a typo in a label. |
| E030 | Override of a computed parameter | Remove the override. |
| E031 | A config names an impl the cell lacks | Fix the `cell`/`inst` rule. |
| E032 | Parameter off its `grid`/`step` | Snap the value, or waive it. |
| E033 | `.control` in a code block, netlisting for Xyce | Move it to an `ngspice` block. |
| E034 | Undefined `${NAME}` | Add a `set NAME …` to `schemify.project`, or export the variable. |
| E035 | Unknown corner or config requested | Check `--corner`/`--config` and the bench header. |
| E036 | Inherited pins of one name with different defaults in one definition | Give them one default net. |
| W037 | A config `inst` rule matches no instance | Fix the path (it starts with `/`). |
| I005 | A waived violation | Nothing; it is listed for review. |

Range, choice and grid violations (`E005`, `E006`, `E032`) can be waived per instance and parameter. Put a `waive` record in the cell that holds the instance:

```
waive 0w0001 m00001 w
  reason "sized by hand for matching"
```

The waived finding is reported as `I005` and no longer blocks anything. Other errors cannot be waived.

## Exit codes

`schemify netlist` and `schemify check` exit with:

| Code | Meaning |
|---|---|
| 0 | Success. Warnings may have been printed. |
| 1 | Design errors: an `E` finding (without `--allow-errors`), or a file that does not parse. |
| 2 | Usage or I/O errors: a bad flag, an unknown dialect, a file that cannot be read or written. |

The full rules, including every emission decision per dialect, are in [netlist.md](../spec/netlist.md).

See also: [cli.md](cli.md), [simulation.md](simulation.md), [files.md](files.md).
