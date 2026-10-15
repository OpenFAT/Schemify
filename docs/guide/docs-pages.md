# Docs pages

Every `.shcomp` and `.shbench` has a documentation page that sits beside its schematic. It is
part of the cell file, so it is versioned, diffed and merged with the circuit it describes. A page
holds three kinds of content:

- text in Markdown, with LaTeX math,
- variables: named expressions that evaluate live and that instances in the schematic can use,
- plots of simulation results.

Sizing math lives next to the circuit and drives it. Change `gm_id_in` on the OTA page below and
every input-pair width on the schematic follows in the same frame.

The full rules are in [docs-page.md](../spec/docs-page.md).

## Show and hide the page

A tab opens with its page shown when the file has at least one docs block. Toggle it with
`Alt+D`, **View → Docs page**, the **Docs** button over the canvas, or the page's `x` button.
Descending into an instance shows that cell's page.

The page's toolbar:

| Button | Effect |
|---|---|
| **Rendered** / **Source** | show text blocks as Markdown source, or rendered again (per tab; the file is untouched) |
| **+ Text** | add a text block after the selected block (at the end when none is selected) |
| **+ Var** | add a variable `v<k> 0` and focus its field |
| **Up**, **Down** | move the selected block |
| **Del** | delete the selected block |
| **x** | hide the page |

Each edit is one undo step.

## What a page looks like in the file

Docs blocks are `prose` and `var` records, kept in file order. This is part of the OTA fixture
(`crates/app/tests/fixtures/ota/ota.shcomp`):

```
inst m00001 dev:nfet_01v8
  at 60 300 R0
  name M1
  W W_in
  L L_in

prose d00001
  | # Five-transistor OTA, sized by gm/Id
  |
  | The input pair sets the gain-bandwidth. In strong inversion the square law gives
  | $g_m = \frac{2 I_D}{V_{ov}}$, so $g_m/I_D = 2/V_{ov}$. The **gm/Id method** reads every
  | ratio from simulated lookup tables instead, valid from weak to strong inversion: pick the
  | inversion level $g_m/I_D$ and the length $L$, and the `gmid` plugin returns the current
  | density $J_D = I_D/W$, hence the width
  |
  | $$W = \frac{I_D}{J_D\left(g_m/I_D,\ L\right)}$$
var Id_tail 20u
  unit A
  doc "tail current: sets the power"
var gm_id_in 15
  unit 1/V
  min 5
  max 25
  doc "input pair inversion level (higher is weaker inversion)"
var L_in 0.5u
  unit m
var id_in Id_tail/2
  unit A
var W_in "gmid.w(\"nfet_01v8\", gm_id_in, L_in, id_in)"
  unit m
  cached 3.8813910462992726e-6 6f77c86f9226a930
prose d00002
  | ## Speed and gain
  |
  | With the load $C_L$ on the output, $\mathrm{GBW} = \frac{g_{m1}}{2 \pi C_L}$, and the DC
  | gain $A_0 \approx \frac{g_{m1}}{g_{ds2} + g_{ds4}}$ is about half the intrinsic gain
  | $g_m/g_{ds}$ of the pair when the mirror is as long as the pair.
var CL 1p
  unit F
var gm_in "gmid.gm(\"nfet_01v8\", gm_id_in, L_in, id_in)"
  unit S
  cached 0.00015000000000000001 6f77c86f9226a930
var GBW gm_in/(2*pi*CL)
  unit Hz
  min 10meg
```

You rarely edit these records by hand: the page writes them for you. Open the fixture to see it
rendered:

```sh
schemify edit crates/app/tests/fixtures/ota/ota.shcomp
```

## Text blocks

A `prose` block is Markdown. Click a block to edit it: it turns into a text area with a live
**Preview** below it.

| Key | Effect |
|---|---|
| `Enter` | new line |
| `Ctrl+Enter`, or click elsewhere | apply |
| `Esc` | cancel |

Applying an empty block deletes it.

Supported Markdown: headings, paragraphs, `*italic*`, `**bold**`, `` `code` ``, fenced code
blocks, bulleted and numbered lists (nested by two spaces), `>` quotes, `---` rules, pipe tables
(up to 16 columns), `[text](url)` links (shown, not followed), footnotes (`[^1]` and
`[^1]: text`) and backslash escapes. An underscore inside a word stays literal, so `gm_id_in`
needs no escaping.

Images: `![alt](path)` on a line of its own shows a PNG at its pixel size, scaled down to the page
width. The path is relative to the project root and cannot leave the project (no leading `/`, no
`..`). Files over 16 MiB or 4096 px a side are refused. A missing or refused image shows
`[image: alt]`.

### Math

`$…$` is inline math; `$$…$$` is a centred display formula, on one line or on lines of its own.
The renderer covers the common LaTeX math subset (fractions, sub- and superscripts, Greek,
`\left`/`\right`, `\mathrm`, and so on). A formula it cannot render shows as its source text,
never disappears.

A `$` opens math only before a non-space character, and a single `$` closes only when no digit
follows it, so `$5 to $10` stays plain text.

## Variables

A `var` block shows as one row:

```
W_in = [gmid.w("nfet_01v8", gm_id_in, L_in, id_in)] = 3.881µm
```

The middle part is an editable field. Type an expression and press `Enter`. The value follows
with 4 significant digits and the SI prefix glued to the var's `unit` (`20µA`, `23.87MHz`).
Hovering the name shows the var's `doc` text; clicking the name selects the block.

Expressions use the [expression language](../spec/expr.md). A var can use the cell's own
parameters and the other vars, in any order; Schemify sorts the dependencies and reports a cycle
as `X005` naming it (`a -> b -> a`). Calculator functions that need simulation data (`v()`,
`op()`, `cross()`, …) do not work in a var (`X006`); put those in a bench `output`.

The value's colour tells you its state:

| Shown | Meaning |
|---|---|
| green value | evaluates, and lies within `min` and `max` |
| red value with `above max 25` or `below min 5` | evaluates, outside a bound |
| red error code and message (`X010 gmid.w: plugin reports Range`) | the expression fails |
| `(an input has an error)` | a var it reads fails |
| amber `unverified (cached; plugin not loaded)` | the value comes from a cache (see below) |

Any text is accepted and stored. Errors show live and do not block saving.

### Vars drive the schematic

Instances use a var by name in a parameter. In the OTA, M1 and M2 have `W W_in` and `L L_in`.
When you change `gm_id_in` on the page, `W_in` recomputes, both instances take the new width, and
the symbol text `W={eng(W)}` on the canvas shows it (`W=3.88u`). The netlist uses the same value.

The var is the single source of the number: you never copy a width from a calculator into an
instance.

In a bench, the vars are the ones corners, sweeps, Monte Carlo and the optimizer change (see
[simulation.md](simulation.md)).

## Plots

A fenced block with the language `plot` inside a text block becomes a graph:

````
```plot
db(v(out))
```
````

The fence holds one calculator expression, as in the waveform viewer. The graph draws from the
editor's current simulation results when the page belongs to the bench that ran, to a cell the
bench instantiates, or to any cell below it among the open documents. It draws one trace per
finished job (every sweep point, corner and Monte Carlo sample, up to 16 jobs), the selected job
first. After an optimizer run it shows the selected job only.

Until a bench has run, the slot reads `no results yet: run the bench to fill this graph`. An
expression that fails, or that reduces to a single number, shows the reason instead of a graph.

The RC bench in `crates/app/tests/fixtures/rc/tb.shbench` has a `v(out)` plot on its page. The
OTA fixture has a `db(v(out))` plot but no bench yet, so it stays empty.

The page shows no results or spec table of its own: that is the results panel under the
schematic ([simulation.md](simulation.md#the-results-table)).

## Staged edits on the page

When the optimizer finishes, or a plugin proposes a change, the page shows a banner with the
edit's label, **Accept** and **Reject**, and the proposed expression (`→ <expr>`) beside each var
it would change. Nothing is written until you accept. With several staged edits, `<` and `>` on
the banner step through them.

## Size with gm/Id

The `gmid-lut` plugin adds `gmid.*` functions backed by lookup tables of simulated device data.
Vars call them and instances use the vars, so the schematic is sized from inversion-level targets.

### Set up a project

1. Put the plugin module in the project, for example `plugins/gmid_lut.wasm`. `plugins/build.sh`
   in the Schemify repository builds it (it writes one into the OTA fixture).
2. Declare it in `schemify.project`:

   ```
   plugin gmid path plugins/gmid_lut.wasm
     grant read_project
     grant write_project
     grant spawn:ngspice
   ```

   `read_project` is enough to use existing tables. `write_project` and `spawn:ngspice` (or
   `spawn:espice`) are needed only to characterize devices.
3. List the devices in `gmid.chars` at the project root. With ready-made tables:

   ```
   device nfet_01v8
     lut gmid/nfet_01v8.glut

   device pfet_01v8
     lut gmid/pfet_01v8.glut
   ```

### Characterize a device

To build a table yourself, give the device a SPICE deck (model includes and options), the
instance line, the OP vector name, the supply and the lengths to simulate:

```
device nfet_01v8
  deck models/sky130_nfet_tt.spice
  inst "X{n} d g s b sky130_fd_pr__nfet_01v8 W={w} L={l} nf=1"
  op "@m.x{n}.msky130_fd_pr__nfet_01v8"
  vdd 1.8
  l 0.15 0.18 0.25 0.5 1 2 4 8
  note "sky130_fd_pr tt, 27 C"
```

Lengths and widths are in µm. Add `polarity p` for a PMOS, and `sim espice` to characterize with
ESPice instead of ngspice (at most 8 lengths). Then run **Tools → gm/Id: characterize devices**.
The plugin writes its work files under `.schemify/gmid/work/`, sweeps VGS and VDS at every
length, and saves the table to the `lut` path (default `.schemify/gmid/<device>.glut`). After you
edit `gmid.chars`, run **Tools → gm/Id: reload lookup tables**. Simulator errors go to the log;
the table already loaded stays in use.

The full attribute list and the sweep details are in `plugins/gmid-lut/README.md`.

### Functions

All arguments and results are SI. `dev` is a device name from `gmid.chars` (a string), `gm_id`
is in 1/V, `l` in metres, `id` in amperes.

| Function | Result |
|---|---|
| `gmid.w(dev, gm_id, l, id)` | width (m) |
| `gmid.jd(dev, gm_id, l)` | current density Id/W (A/m) |
| `gmid.vgs(dev, gm_id, l)` | \|VGS\| (V) |
| `gmid.gm(dev, gm_id, l, id)` | gm (S) |
| `gmid.gds(dev, gm_id, l, id)` | gds (S) |
| `gmid.gain(dev, gm_id, l)` | intrinsic gain gm/gds (V/V) |
| `gmid.ft(dev, gm_id, l)` | transit frequency (Hz) |
| `gmid.l(dev, l)` | `l` clamped into the table's length range (m) |

Each function except `gmid.l` has an `_at` variant with a VDS magnitude as the last argument
(`gmid.w_at(dev, gm_id, l, id, vds)`); the plain ones use the table's nominal VDS. A length
outside the table, a `gm_id` the device cannot reach, an unknown device or `id ≤ 0` fail with
`X010`. The functions do not extrapolate.

### Cached values

When the plugin computes a var, Schemify stores the result in the file:

```
var W_in "gmid.w(\"nfet_01v8\", gm_id_in, L_in, id_in)"
  unit m
  cached 3.8813910462992726e-6 6f77c86f9226a930
```

The second number identifies the plugin build that computed it. The cache is written as part of
your edit, so it adds no extra undo step.

The cache lets the file work without the plugin. A colleague who opens the cell without
`gmid-lut` sees the cached value, marked `unverified (cached; plugin not loaded)`, and so do the
vars that depend on it. The netlist uses the cached values. Changing the expression drops its
cache, and the var shows `X006` until the plugin is available again. When a different plugin
build loads, every cache is recomputed and rewritten.

See also: [simulation.md](simulation.md), [docs-page.md](../spec/docs-page.md),
[expr.md](../spec/expr.md), [files.md](files.md).
