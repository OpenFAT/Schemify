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
`ngspice_results_agree`: 10/10 benches agree.)

The count may only go up. Every remaining difference is annotated below with its root
cause and owner.

## Current numbers (2026-10-10, m1 @ 789743b + netlist)

| set | mode | agree | differ | differ, connectivity agrees | cdl agree, golden code stripped | failed-to-import | failed-to-elaborate |
|---|---|---|---|---|---|---|---|
| xschem | spice | 42 | 59 | 14 | - | 0 | 9 |
| xschem | cdl | 18 | 83 | 14 | 37 | 0 | 9 |
| sky130 | spice | 43 | 32 | 9 | - | 0 | 1 |
| sky130 | cdl | 15 | 60 | 21 | 33 | 0 | 1 |
| gf180 | spice | 58 | 0 | 0 | - | 0 | 0 |
| gf180 | cdl | 0 | 58 | 8 | 58 | 0 | 0 |
| ihp | spice | 55 | 2 | 2 | - | 0 | 0 |
| ihp | cdl | 2 | 55 | 45 | 46 | 0 | 0 |
| **total** | | **233** | **349** | 113 | **174** | 0 | 20 |

SPICE: **198 / 301** agree. CDL: 35 / 301 raw, **174 / 301** against the golden without
its `**** begin/end user architecture code` blocks: netlist spec §7.4 gives CDL no code
blocks while xschem's LVS netlist keeps them, so raw CDL comparisons differ by design
(owner: spec, accepted). CDL netlists the component when one exists (LVS checks the cell
and its ports), SPICE the bench copy (`<stem>__top.shbench`, import §1).

History: first run 88 agree (73 netlist panics on non-bench tops, fixed); 118 after that;
SPICE 185 → 197 after the elab fixes (recursive `${NAME}`, unique `net_<id>` names,
m1 feb9899); 198 with precompiled Verilog-A bodies emitted.

## Remaining differences by root cause

Benches without a suffix are SPICE; `(cdl)` rows are those still differing after the
code blocks are stripped. No remaining difference is owned by the netlister alone.

| count | owner | root cause | benches |
|---|---|---|---|
| 45 | golden | xschem leaves a component's params as undefined SPICE identifiers at top level (`w=WP`, `res/2`); the bench copy defines them as vars, so ours evaluates (expected, integrator-noted) | xs-ex-cmos_inv, xs-ex-cmos_inv (cdl), xs-ex-diode_1, xs-ex-diode_1 (cdl), xs-ex-dlatch, xs-ex-dlatch (cdl), xs-ex-rcline, xs-ex-rcline (cdl), xs-ng-and3_ngspice, xs-ng-and3_ngspice (cdl), xs-ng-and_ngspice, xs-ng-and_ngspice (cdl), xs-ng-boost (cdl), xs-ng-buck_boost (cdl), xs-ng-nand_ngspice, xs-ng-nand_ngspice (cdl), xs-ng-nor_ngspice, xs-ng-nor_ngspice (cdl), xs-ng-or_ngspice, xs-ng-or_ngspice (cdl), xs-ng-pv_ngspice, xs-ng-pv_ngspice (cdl), xs-ng-xnor_ngspice, xs-ng-xnor_ngspice (cdl), xs-ng-xor_ngspice, xs-ng-xor_ngspice (cdl), sky130-lvnand, sky130-lvnand (cdl), sky130-lvtnot, sky130-lvtnot (cdl), sky130-not, sky130-not (cdl), sky130-passgate_nlvt, sky130-passgate_nlvt (cdl), sky130-test_customplot (cdl), sky130-test_mos_binning (cdl), sky130-test_nmos_sizes (cdl), sky130-test_sweep_mos_w (cdl), sky130-test_sweep_res_l (cdl), ihp-ac_svaricap_test (cdl), ihp-dc_esd_diodes (cdl), ihp-inv_sweep_tb (cdl), ihp-isolbox_sweep_tb (cdl), ihp-sp_svaricap_test (cdl), ihp-tran_svaricap_test (cdl) |
| 34 | spicecmp | B/E/G expression text keeps the subckt params xschem passes (`v(pn,mn)/ron`); ours specializes them to numbers. spicecmp should substitute params in scope inside unevaluable expressions | xs-ex-classD_amp, xs-ex-classD_amp (cdl), xs-ex-flop, xs-ex-flop (cdl), xs-ex-sr_flop, xs-ex-sr_flop (cdl), xs-ex-test_lvs_ignore, xs-ex-test_lvs_ignore (cdl), xs-ex-test_short_option, xs-ex-test_short_option (cdl), xs-ng-boost, xs-ng-buck_boost, xs-ng-buf_ngspice, xs-ng-buf_ngspice (cdl), xs-ng-comp_ngspice, xs-ng-comp_ngspice (cdl), xs-ng-counter_6bit_ngspice, xs-ng-counter_6bit_ngspice (cdl), xs-ng-diode_ngspice, xs-ng-diode_ngspice (cdl), xs-ng-flip_flop_ngspice, xs-ng-flip_flop_ngspice (cdl), xs-ng-inv_ngspice, xs-ng-inv_ngspice (cdl), xs-ng-keeper_ngspice, xs-ng-keeper_ngspice (cdl), xs-ng-solar_panel, xs-ng-solar_panel (cdl), sky130-charge_pump_ideal, sky130-charge_pump_ideal (cdl), sky130-diode_ngspice, sky130-diode_ngspice (cdl), sky130-tb_charge_pump, sky130-tb_charge_pump (cdl) |
| 29 | import or spicecmp | `.save v( x )` (core `probe` template keeps xschem's literal spaces) vs xschem's `.save v(x)`; fix the core template or normalize whitespace inside `v( )` in spicecmp | xs-ex-lightning, xs-ex-mos_power_ampli, xs-ex-mos_power_ampli (cdl), xs-ex-poweramp_xyce, xs-ex-poweramp_xyce (cdl), xs-ex-test_extracted_netlist (cdl), xs-ex-TwoStageAmp, xs-ng-amp_xschem, xs-ng-comp_65nm, xs-ng-comp_65nm (cdl), xs-ng-led_driver, xs-ng-led_driver (cdl), xs-ng-opamp_65nm, xs-ng-opamp_65nm (cdl), xs-iss-comp3, xs-iss-comp3 (cdl), xs-iss-comp_65nm, xs-iss-comp_65nm (cdl), sky130-bandgap_opamp, sky130-bandgap_opamp (cdl), sky130-logic_ngspice, sky130-logic_ngspice (cdl), sky130-optimize_delay, sky130-optimize_delay (cdl), sky130-sky130_oscillator, sky130-tb_ft_test_2, sky130-tb_ft_test_2 (cdl), sky130-test_lvs_ignore, sky130-test_lvs_ignore (cdl) |
| 20 | import | devices missing in ours: `.sch` used as a symbol (LCC), leaf impls left `ignore true` (format not translated), or netlist text in a component dropped | xs-ex-bus_keeper, xs-ex-LCC_instances, xs-ex-LCC_instances (cdl), xs-ex-loading, xs-ex-loading (cdl), xs-ex-poweramp_lcc, xs-ex-poweramp_lcc (cdl), xs-ex-tb_symbol_include, xs-ex-tesla (cdl), xs-ex-test_extracted_netlist, xs-ng-autozero_comp, xs-ng-autozero_comp_xyce, sky130-all_sky130_gates, sky130-all_sky130_gates (cdl), sky130-gates_sky130, sky130-gates_sky130 (cdl), sky130-test_generators, sky130-test_generators (cdl), sky130-test_nand2_noschematic, sky130-test_ngspice_flop |
| 20 | import | a symbol-less schematic becomes a `.shbench` (labels, no ports); xschem uses its ipin/opin as the CDL top ports (or the bench copy is netlisted) | xs-ex-bus_keeper (cdl), xs-ex-lightning (cdl), xs-ex-symbol_include2 (cdl), xs-ex-tb_symbol_include (cdl), xs-ex-TwoStageAmp (cdl), xs-ng-amp_xschem (cdl), xs-ng-autozero_comp (cdl), xs-ng-autozero_comp_xyce (cdl), xs-ng-colpitts_xschem (cdl), xs-ng-hpf_xschem (cdl), xs-iss-comp3_empty (cdl), xs-iss-comp3_parax (cdl), xs-iss-comp_65nm_parax (cdl), sky130-simulate_ff (cdl), sky130-sky130_oscillator (cdl), sky130-test_nand2_noschematic (cdl), sky130-test_ngspice_flop (cdl), sky130-test_nmos (cdl), sky130-test_pmos (cdl), sky130-test_res (cdl) |
| 12 | import | instance value `@param` translated to `@{param}` (the `@` must go): `model @{modeln}` | sky130-bandgap, sky130-bandgap (cdl), sky130-gain_stage, sky130-gain_stage (cdl), sky130-passgate, sky130-passgate (cdl), sky130-tb_bandgap_opamp, sky130-tb_bandgap_opamp (cdl), sky130-tb_bandgap, sky130-tb_bandgap (cdl), sky130-test_comparator, sky130-test_comparator (cdl) |
| 8 | import | cosim cells: model cards in component code dropped, `D[5..0]` bus syntax | xs-cosim-ainv, xs-cosim-ainv (cdl), xs-cosim-ccap, xs-cosim-ccap (cdl), xs-cosim-sar_adc, xs-cosim-sar_adc (cdl), xs-cosim-tb_sar_adc, xs-cosim-tb_sar_adc (cdl) |
| 6 | import | alternate schematic per instance (`schematic=`) / instance format override not reproduced | xs-ex-test_backannotated_subckt, xs-ex-test_backannotated_subckt (cdl), xs-iss-inst_sch_select, xs-iss-inst_sch_select (cdl), sky130-test_format_override, sky130-test_format_override (cdl) |
| 6 | import | include/lib or params from code blocks selected differently (simulator-specific code, `only_toplevel`) | xs-ex-test_ne555, xs-ex-test_ne555 (cdl), sky130-test_hvl_cells (cdl), ihp-inv_mc_tb, ihp-inv_sweep_tb, ihp-sc-tran_inv (cdl) |
| 5 | import | duplicate pin names (xschem shorts them: `short.sym`, `xcross`, `doublepin`) renamed `<pin>_2` | xs-ex-doublepin, xs-ex-doublepin (cdl), xs-ex-xcross (cdl), sky130-test_short_option, sky130-test_short_option (cdl) |
| 4 | import | extra devices in ours: simulator-specific or non-SPICE code (VHDL/Verilog) kept as `spice` code | xs-ex-greycnt, sky130-sky130_mismatch (cdl), sky130-test_analog, sky130-test_analog (cdl) |
| 4 | spicecmp | `.save` with several vectors / unnamed nets in directives: compared as text (`v(net1)` vs `v(net_0mea9e)`) | sky130-montecarlo_mismatch_sim, sky130-montecarlo_mismatch_sim (cdl), sky130-test_bipolar, sky130-test_bipolar (cdl) |
| 4 | golden | xschem's CDL keeps `expr_eng(...)` unevaluated (and an untranslated rsil `lvs_format`: import) | ihp-ac_lv_nmosrf (cdl), ihp-dc_res_temp (cdl), ihp-iso_dc_res (cdl), ihp-mc_res_op (cdl) |
| 3 | import | `.model` text in a component schematic dropped (`needs_review: code-in-component`) | xs-ex-MSA-2643, xs-ex-MSA-2643 (cdl), xs-cosim-tgate |
| 2 | import | two code blocks define the same `.param`/source (duplicate code text in the bench copy) | xs-ex-poweramp, xs-ex-poweramp (cdl) |
| 2 | import | instance `device_model` (subckt/model text) missing from the bench copy | xs-ng-tb_diff_amp, sky130-tb_diff_amp |
| 2 | import + netlist | XSPICE templates with `~`/vector tokens not translated; bus-bit nets inside `[ ]` (netlist §7.6 open issue) | xs-logic-test_ngspice, xs-logic-test_ngspice (cdl) |
| 2 | import | bus taps (`bus_tap`, needs_review) not translated | sky130-test_inv, sky130-test_inv (cdl) |
| 2 | import | xschem `\\` + newline in a format joins lines; translated as a newline (sqwsource) | sky130-test_stdcells, sky130-test_stdcells (cdl) |

## Blocked by the gate (failed-to-elaborate)

| mode | first blocking finding | benches |
|---|---|---|
| cdl | E008 X004: unknown identifier `WPRECH` | xs-ex-test_bus_tap |
| cdl | E009 instance name `m2` is used twice | xs-ex-nand2, xs-ex-test2, xs-ex-test |
| cdl | E011 pin `ADD[dim-1:0]` (1 bits × 1 | xs-logic-ram_tb |
| cdl | E011 pin `A[3:0]_2` (1 bits × 1 | xs-ex-test_doublepin |
| cdl | E011 pin `d` (1 bits × 1 | xs-logic-testbench |
| cdl | E011 pin `p` (1 bits × 1 | xs-ng-adc, xs-ng-delta_sigma |
| cdl | E011 pin `p` (1 bits × 256 | sky130-test_carry_lookahead |
| spice | E008 X004: unknown identifier `WPRECH` | xs-ex-test_bus_tap |
| spice | E009 instance name `m2` is used twice | xs-ex-nand2, xs-ex-test2, xs-ex-test |
| spice | E011 pin `ADD[dim-1:0]` (1 bits × 1 | xs-logic-ram_tb |
| spice | E011 pin `A[3:0]_2` (1 bits × 1 | xs-ex-test_doublepin |
| spice | E011 pin `d` (1 bits × 1 | xs-logic-testbench |
| spice | E011 pin `p` (1 bits × 1 | xs-ng-adc, xs-ng-delta_sigma |
| spice | E011 pin `p` (1 bits × 256 | sky130-test_carry_lookahead |

E009 (duplicate instance names; xschem netlists both, the netlists agree once waived):
elab gates by design (spec §8). E011 bus-width findings come from imported bus syntax
(`ADD[dim-1:0]`, broadcast pins of probes and bus taps): import. E008 `WPRECH`: a
parameter defined only in xschem's bus-tap test code: import.
