# Netlist ratchet (xschem differential)

The `netlist` row of [TESTING.md](../../TESTING.md): every bench of
[benches.txt](benches.txt) imported with `schemify_import`, elaborated, emitted
(ngspice and cdl) and compared with xschem's golden under `schemify_spicecmp`.
Produced by `crates/netlist/tests/corpus.rs`:

```text
SCHEMIFY_CORPUS=tests/corpus/.cache cargo test -p schemify-netlist --release --test corpus -- --nocapture
```

(our netlists, the diffs and the imported files land in `.cache/netlist-out/`; the
second oracle, ngspice results of ours vs the golden, is the ignored test
`ngspice_results_agree`. The CLI path is `import_all.sh`, which netlists with
`schemify netlist` and runs `diff_all.sh`: raw comparison, no code stripping or binding.)

The count may only go up. Every remaining difference is annotated below with its root
cause and owner.

## Categories

- **agree**: equivalent under `spicecmp` (SPICE against the golden; the CDL column
  "agree, golden code stripped" against the golden without its `**** begin/end user
  architecture code` blocks: netlist spec §7.4 gives CDL no code blocks).
- **golden-unevaluated**: equal only with `spicecmp --bind-free`: xschem netlists a
  component as the top level with its parameters left as undefined names (`w=WP`,
  `res/2`, `VTH=VTH` passed down); ours evaluates them to the component defaults. The
  option binds each name the golden never defines to the value ours uses (one value per
  name, the whole netlist compared again under it); the bindings are printed per bench
  in `netlist-out/RESULTS.txt` (`cmos_inv: lln=3e-6 llp=3e-6 wn=1.5e-5 wp=4.5e-5`).
  Counted apart from agreement, never as agree.
- **differ**, **failed-to-elaborate** (blocked by the gate): below.

## Current numbers (2026-10-11, main @ fef9426 + the D17 amendment)

| set | mode | agree | golden-unevaluated | differ | (connectivity agrees) | cdl agree, golden code stripped | failed-to-import | failed-to-elaborate |
|---|---|---|---|---|---|---|---|---|
| xschem | spice | 72 | 18 | 10 | 3 | - | 0 | 10 |
| xschem | cdl | 31 | 19 | 50 | 17 | 68 | 0 | 10 |
| sky130 | spice | 63 | 5 | 7 | 0 | - | 0 | 1 |
| sky130 | cdl | 22 | 11 | 42 | 17 | 51 | 0 | 1 |
| gf180 | spice | 58 | 0 | 0 | 0 | - | 0 | 0 |
| gf180 | cdl | 0 | 0 | 58 | 8 | 58 | 0 | 0 |
| ihp | spice | 57 | 0 | 0 | 0 | - | 0 | 0 |
| ihp | cdl | 2 | 2 | 53 | 47 | 50 | 0 | 0 |
| **total** | | **305** | **55** | **220** | 92 | **227** | 0 | 22 |

For CDL, golden-unevaluated is judged against the code-stripped golden.

| set | SPICE agree before → after | CDL (code stripped) agree before → after |
|---|---|---|
| xschem (101) | 42 → **70** (+18 golden-unevaluated) | 37 → **68** (+19) |
| sky130 (76) | 43 → **61** (+5) | 33 → **51** (+11) |
| gf180 (58) | 58 → **58** | 58 → **58** |
| ihp (57) | 55 → **57** | 46 → **50** (+2) |
| **total (301)** | **198 → 246** (+23) | **174 → 227** (+32) |

Then 246 → **250** SPICE with the D17 amendment (component code inside its subckt); CDL
unchanged (CDL carries no code).

Gate-blocked: 20 → 22 per both modes together (`test_doublepin` unblocked; `sar_adc`
and `tb_sar_adc` now blocked by a real E011, see below).

History: first run 88 agree (73 netlist panics on non-bench tops, fixed); 118 after that;
SPICE 185 → 197 after the elab fixes (recursive `${NAME}`, unique `net_<id>` names,
m1 feb9899); 198 with precompiled Verilog-A bodies emitted; 246 with this change.

### What moved the numbers

| owner | change | benches |
|---|---|---|
| spicecmp | parameter names in scope inside unevaluable expressions read as their values (`v(pn,mn)/ron`, `'VTH-0.1'`); numbers in canonical form there | the old 34-row bucket (B/E/G sources in passed-parameter subckts) |
| spicecmp | `.save` vectors are a set, spacing inside `v( x )` ignored; with matching connectivity, saved nets and devices compare by graph position (unnamed nets, saves in specialized subckt copies) | the old 29-row and 4-row `.save` buckets, scoped saves (counter, keeper, optimize_delay) |
| spicecmp | control nodes of B/E/G sources (`v(a)` in expressions, `pwl(N)`/`poly(N)`/`and(N)`… nodes) are terminals, not text; CDL multi-terminal R/C/D | sr_flop, IHP svaricap/esd CDL |
| spicecmp | `--bind-free` (golden-unevaluated category) | 55 rows moved out of "differ" |
| import | `@param` in an instance value is the parent parameter (`model=@modeln`) | bandgap family, test_comparator |
| import | netlist-text instances with `spice_ignore=true` dropped; Tcl in SPICE comment lines dropped instead of the block | ihp-inv_mc_tb, inv_sweep_tb, pv_ngspice, solar_panel |
| import | bench copies: component defaults only for referenced parameters, a bench `.param` replaces them | pv_ngspice, lvnand |
| import | a mapped device's *template* `device_model` is not emitted (goldens with `model=` overrides lack it) | flip_flop, counter, keeper, led_driver |
| import | a schematic as a symbol: derived interface from its `ipin`/`opin`/`iopin` (symbol-less schematics get LVS ports; `C {x.sch}` LCC instances) | 20 CDL tops, LCC_instances, poweramp_lcc, bus_keeper |
| import | xschem bracket-less buses `A[3..0]` → `A3,A2,A1,A0` | solar_panel (`G2[9..0]`) |
| import | `#pfx#@name` name prefixes; `@@P` naming no pin renders empty; escaped newline joins format lines; pinless symbols whose format uses `extra` nets are devices | autozero_comp, sky130 gates (nand4b, fill/tap CDL), test_stdcells sources |
| import + elab + netlist | same-named pins are one port: `alias` pins (format §4 rank 1, netlist §3) | test_short_option ×4, xcross, doublepin, test_doublepin unblocked |
| import | `only_toplevel=true` devices live in the bench copy; `$USER_CONF_DIR` → `${HOME}/.xschem` | test_analog, tb_diff_amp ×2 |
| elab + netlist | schematic-impl `body` (instance `device_model` in a component) emitted once, not in CDL | charge_pump_ideal, tb_charge_pump |
| netlist | XSPICE bus bits respelled `d[0]` → `d_0` (spec §7.2) | test_ngspice (`Y_NAND_A[4]`) |
| format + elab + netlist + import | D17 amendment: `code` in a `.shcomp`, written inside each of its definitions (netlist §7.6.15); the importer keeps a component's code instances and `S {}` text there | MSA-2643 (Q1 model cards), test_extracted_netlist, test_ngspice_flop, test_stdcells |

## Remaining differences by root cause

Rows are `bench (mode)`; SPICE unless marked. 17 SPICE and 31 CDL (code stripped)
differences remain outside golden-unevaluated and the gate.

| count | owner | root cause | benches |
|---|---|---|---|
| 9 | import | instance `format`/`lvs_format` overrides (needs_review): pin order or terms change per instance (`format="@name @pinlist @model"`, `lvs_format=" "`) | sky130-test_format_override, sky130-test_format_override (cdl), xs-cosim-ainv, xs-cosim-ainv (cdl), xs-cosim-ccap, xs-cosim-ccap (cdl), xs-ng-boost (cdl), xs-ng-buck_boost (cdl), sky130-sky130_mismatch (cdl) |
| 6 | golden | xschem resolves an `extra` parameter (`@modelp`) inside a subcircuit from the symbol default and ignores the instance's value (`modelp=pfet_01v8_lvt` on x5/x6); ours honours the instance | sky130-bandgap, sky130-bandgap (cdl), sky130-tb_bandgap, sky130-tb_bandgap (cdl), sky130-tb_bandgap_opamp, sky130-tb_bandgap_opamp (cdl) |
| 4 | golden | a top-level component renders an unresolved `@modeln` as an empty model name (`sky130_fd_pr__`): not bindable (a name, not a value) | sky130-gain_stage, sky130-gain_stage (cdl), sky130-passgate, sky130-passgate (cdl) |
| 4 | golden | unquoted values with a space in xschem's `.subckt` line (`delay=200 ps`, `del=2 ns`): invalid SPICE, the stray token remains | xs-ex-doublepin, xs-ex-doublepin (cdl), xs-ex-loading, xs-ex-loading (cdl) |
| 5 | golden (spec §7.4) | xschem's LVS netlist keeps `.include`/`.lib` lines of netlist-text symbols outside the architecture block; CDL has no corners (test_stdcells also passes `vcc` from a stripped `.param`) | sky130-optimize_delay (cdl), sky130-tb_charge_pump (cdl), sky130-test_hvl_cells (cdl), ihp-sc-tran_inv (cdl), sky130-test_stdcells (cdl) |
| 4 | golden | xschem's CDL keeps `expr_eng(...)` unevaluated (IHP `rsil`/`rppd`/`rhigh` `lvs_format`) | ihp-ac_lv_nmosrf (cdl), ihp-dc_res_temp (cdl), ihp-iso_dc_res (cdl), ihp-mc_res_op (cdl) |
| 4 | import | alternate schematic per instance (`schematic=`), back-annotated subcircuit values | xs-ex-test_backannotated_subckt, xs-ex-test_backannotated_subckt (cdl), xs-iss-inst_sch_select, xs-iss-inst_sch_select (cdl) |
| 2 | import | parametric bus widths in symbols (`A[max:0]`, `nd2.sym`): XSPICE gates dropped | xs-logic-test_ngspice, xs-logic-test_ngspice (cdl) |
| 2 | import | symbol generators `x.tcl(@value)` need a Tcl interpreter | sky130-test_generators, sky130-test_generators (cdl) |
| 2 | import | `spice_sym_def=".include symbol_include.cir"`: xschem orders `@pinlist` by the included `.subckt` line | xs-ex-tb_symbol_include, xs-ex-tb_symbol_include (cdl) |
| 2 | golden | `$netlist_dir` (xschem's runtime output directory) in a `spice_sym_def` include | xs-ex-test_ne555, xs-ex-test_ne555 (cdl) |
| 2 | elab | an unnamed net on an iterated instance's bus pins: xschem sizes it pin width × elements (`x22[1:0]` → `net7[7:0]`), elab §9.3 by the widest pin (changing it breaks the sky130 adders: needs a rule from more evidence) | xs-ex-test_doublepin, xs-ex-test_doublepin (cdl) |
| 1 | spec (CDL has no code) | a `k.sym` coupling is netlist text in Schemify; xschem's LVS netlist keeps the `K` line | xs-ex-tesla (cdl) |
| 1 | golden | xschem's LVS netlist of a component includes its `only_toplevel` stimuli (it is the top); ours keeps them in the bench copy | sky130-n_diffamp (cdl) |

## Blocked by the gate (failed-to-elaborate)

| count | first blocking finding | owner / cause | benches (both modes) |
|---|---|---|---|
| 6 | E009 instance name `m2` is used twice | elab by design (spec §8); the schematics repeat names, xschem netlists both (`test`, `test2` agree once waived) | xs-ex-nand2, xs-ex-test2, xs-ex-test |
| 4 | E011 1-bit probe pin on a 6-bit bus | elab: xschem replicates a 1-pin instance per bus bit (`.save v(count[5])`…); spec §4 broadcasts only 1-bit nets | xs-ng-adc, xs-ng-delta_sigma |
| 4 | E011 pin `D5,…,D0` (6 bits) on a 1-bit net | import: XSPICE inverted-net labels `~D[5..0]` are outside the net grammar (`~` is not an identifier character) | xs-cosim-sar_adc, xs-cosim-tb_sar_adc |
| 4 | E011 pin `ADD[dim-1:0]` / `d` | import: parametric bus widths | xs-logic-ram_tb, xs-logic-testbench |
| 2 | E011 pin `p` (1 bits × 256) on a 32-bit net | elab/import: bus taps and replicated probes | sky130-test_carry_lookahead |
| 2 | E008 X004: unknown identifier `WPRECH` | import: a parameter defined only in xschem's bus-tap test code; bus taps unsupported | xs-ex-test_bus_tap |
