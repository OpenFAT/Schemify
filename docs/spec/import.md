# xschem import v1 (normative)

Implements D1, D14, D24, D33, D40. Readers: `crates/import` (`xschem`, `attr`, `symbol`, `spice`, `tech`, `cdf`). Translation: `import::translate_library` (pure: `(path, bytes)` in, `(path, bytes)` + report out), oracle `import::check_output`, CLI `tools/devicereader`. Clean-room: behaviour comes from `docs/research/` and from comparing against goldens produced by running xschem (`tests/corpus/`); never from xschem source.

**End-to-end oracle:** for every corpus bench, `import → schemify netlist` must equal xschem's golden under `tools/spicecmp` (SPICE and CDL). Differences are bugs or annotated XFAILs. `tests/corpus/import_all.sh` imports every corpus group and writes `benches.map` (bench → cell file) for that comparison.

## 1. What becomes what

| xschem input | Schemify output |
|---|---|
| `X.sym` (`type=subcircuit`) + its schematic | `X.shcomp`, one impl `schematic schematic` |
| `X.sym` (any other cell type) + its schematic | `X.shcomp`, impls `<leaf>` (default, xschem netlists these by `format`) and `schematic` |
| `X.sym` with no schematic | `X.shprim`, one leaf impl named after its lang: `spice`; `veriloga` when `device_model` holds `pre_osdi`; `verilog` for `d_cosim` |
| `X.sch` with no `.sym` (or a non-cell `.sym`) | `X.shbench` |
| a `.shcomp` schematic holding netlist text (code instances, `S {}`) | also `X__top.shbench`: a bench copy of the same schematic carrying that text (xschem netlists such schematics as test benches; 271 of the 301 corpus benches have a symbol) |
| `type` ∈ `label ipin opin iopin noconn` | not cells: label / port / noconn records (§3) |
| `type` ∈ `netlist_commands spice_parameters`, or a pinless symbol with a `format` (`k.sym`) | not cells: bench `code` text (§3) |
| `type` ∈ `launcher logo architecture arch_declarations attributes package port_attributes use timescale verilog_preprocessor generic annotator`, `show_label scope`, `probe` without `format` | not cells; instances dropped (non-electrical / display) and reported |
| `type` ∈ `bus_tap netlist_options`, symbol generators `x.tcl(args)`, `.sch` used as a symbol (LCC), embedded `[ … ]` bodies | `needs_review` (unsupported in v1) |

- **The schematic of a symbol:** `default_schematic=ignore` → none; `schematic=<file>` (with `@symname` replaced by the stem) resolved like a symbol reference; `schematic=tcleval([hierarchy_config @symname])` → its known default, the same-named `.sch` in the sibling `sch` tree of the symbol's `sym` directory (IHP std cells); otherwise `<stem>.sch` beside the `.sym`. A schematic consumed this way is not also emitted as a bench. `type=tcleval([symtype])` → its known default `subcircuit`.
- **Libraries:** one Schemify library per directory holding a cell, named after the directory, skipping the generic components `xschem`, `sym`, `sch` (`…/sg13g2_stdcell/sym/xschem` → `sg13g2_stdcell`); clashes (and `core`) get `_2`, `_3`, … in path order. Output: `<out>/<lib>/<cell>.<ext>`, `<out>/schemify.project` (`lib <name> path <name>` per library plus the PDK `set` entries of §5), `<out>/import-report.md` and `import-report.txt`.
- **Symbol references** resolve like `XSCHEM_LIBRARY_PATH`: the caller passes the search path (`Options::roots`); `r` → first `<root>/r` among the inputs, then `r` beside the referencing schematic; `./`, `../` relative to that schematic; files with `file_version` 1.0 (or none) get `.sym` appended to extension-less references.
- **Determinism:** inputs are processed in path order whatever their order; the same inputs give byte-identical outputs.

## 2. Symbol interface (`.sym` → rank 1)

- **Pins:** `B 5` boxes → `pin <name> <dir> <cx> <cy>` (box centre). Order: file order, or ascending `sim_pinnumber` when every pin has one (stable). When the SPICE `format` names pins with `@@P` and has no `@pinlist`, pins are reordered by first `@@` appearance (others keep their order after): xschem's `.subckt` lists ports in that order (IHP std cells `@@Y @@A` over symbol order `A Y`). `dir`: `in`/`out`/`inout` from `dir=` (missing or other → `inout`). Tiny bulk boxes are real pins. A pin named like a parameter is renamed `<pin>_pin` (template pin names shadow parameters; xschem keeps the namespaces apart); empty or duplicate names → `pin<k>` / `<name>_<k>` + `needs_review`.
- **Params:** template `k=v` pairs (first occurrence wins), except `name` (→ header `prefix` = its leading letter run), `spiceprefix` (folded into the template, §4), `device_model`, `spice_ignore`, `lvs_ignore` and net parameters. Kind inference: `true`/`false` → `bool`; a SPICE number (SI suffix kept, unit letters stripped, any integer included: xschem cannot tell `W=1` from an integer) → `number`; `expr('…')`/`expr_eng(…)` → `number` whose default is the translated expression (overridable, as in xschem; not `computed`); a value listed in `available_models` → `choice`; anything else, including SPICE-quoted expressions (`'int((nf+1)/2)*W/nf*0.18u'`, evaluated by SPICE as in the goldens), → `string`, verbatim. A parameter that any instance in the inputs sets to text that is not a number expression (`value="port=1 z0=50 ac 1"`) is a `string` whatever its default. Keys that no `@token` can reach (`+`, `300`) are dropped and reported; names that collide with template builtins (`name cell pins params file`) or with inst attributes (`at ignore inherit`) → `needs_review`.
- **Unknown tokens:** an `@x` in a format that names no parameter, pin or builtin becomes a `string` parameter `x` with default `""`: it renders empty exactly as xschem renders an unresolved token, and instances can still set it (`@extra` on MOS symbols). Non-identifier tokens render empty. Both are counted in the report.
- **Implicit pins:** names in `extra=` (in that order), then `body`, that appear as a standalone `@x` word in `format` or `lvs_format` → `pin <x> inout 0 0` with `inherit <default-net>` (D29), default net = the template value (`0` if empty). Goldens append them to `.subckt` lines in that order. Words of `extra` used inside a token (`@prefix\a21o_1`) stay parameters.
- **Device limits (D14):** see §2.1.
- **Impl and templates:** `format` → `format spice`; `lvs_format` → `format cdl`; `spectre_format` → `format spectre`; `verilog_format` → `format verilog` (§4). A schematic impl gets the format with `k=@k` pairs of its own parameters removed (the netlister specialises the subcircuit, netlist §6). `device_model` (symbol or template level) and `spice_sym_def` → `body <impl>` (after §5). `spice_ignore=true|open|short` (symbol or template) → impl attr `ignore`. A leaf impl with no `format`, or whose `format` does not translate, gets `ignore true` (xschem does not netlist it; nothing is guessed) and the failure is reviewed.

### 2.1 Device limits (D14)

Limits go on parameters named `L`/`W` (any case) as `min`/`max`/`grid` expressions in the parameter's own unit: a default with an SI suffix or below 1e-3 is metres (gf180 `0.28u`), a plain default ≥ 1e-3 is micrometres (sky130 `L=0.15` with `.option scale=1e-6`).
- **Model cards:** the SPICE name is the `format` word holding `@model` with the template `model` substituted (`sky130_fd_pr__@model`); bins of every scanned model under that name (`spice::ModelScan::bins_of`) → the envelope `[min lmin, max lmax]` / `[min wmin, max wmax]` (`ponytail:` gaps between bins are not representable).
- **`drc=` procs:** the call's words map the proc's formal arguments to `@params`; the arm of `switch -regexp $model` matching the template's `model` default supplies rules `if { $v < N }` → `min`, `> N` → `max`. `regsub {u$} $v {} v` marks thresholds ≥ 1e-3 as micrometres (`0.13` → `0.13u` for a metre parameter); `set v [expr {double($v)/double($d)}]` makes the bound per finger (`0.42*nf`), skipped when a guarding `if {![regexp {RE} $symbol]}` matches the symbol file name. Any other line, a proc without a `switch`, or an unknown proc → `needs_review` (the recognised rules still apply).
- **Grid:** the tech file's manufacturing grid (KLayout default grid, Magic `cifoutput` `gridlimit`) → `grid` (`5n` or `0.005`).
- Several bounds on one attribute combine to the tightest (`max(…)` of minima).
- **CDF (`cdfDump`):** radio/cyclic parameters of a same-named cell set `choice` and `choices` on a same-named `string` parameter; callbacks → `needs_review`.

## 3. Schematic (`.sch` → rank 3)

| xschem record / symbol | Schemify |
|---|---|
| `N` wire | `wire` (`bus true` from `bus=`; cached `lab=` dropped); zero-length → dropped |
| `type=label` (`lab_pin`, `lab_wire`, `gnd`, `vdd`, `sub`, …) | `label` at the symbol pin's placed position, net = `lab` (instance, else template); `global true` when the instance or symbol has `global=true` (xschem `.GLOBAL`; netlist §3) |
| `ipin`/`opin`/`iopin` | in a `.shcomp`: `port` bound to the same-named interface pin (renamed pins follow); a label that names no pin stays a `label` + `needs_review`. In a `.shbench`: `label` |
| interface pin with no port | a `port` at a same-named label's anchor (xschem binds subcircuit ports by net name; IHP std cells' `VDD`/`VSS`), else at a free point outside the drawing |
| `noconn` | `noconn` at the pin position |
| netlist text (`code`, `code_shown`, `simulator_commands`, `corner`, `param`, pinless devices) | in a `.shbench`: `code <instance name> <lang>`: the `format` (instance first) rendered with instance-then-template values, `tcleval(…)` per §5, verbatim lines; lang = the `simulator` value when the format tests `sim_is_@simulator`, else `spice`. In a `.shcomp`: dropped when a bench copy carries it, else `needs_review` |
| `S {…}` text | in a `.shbench`: `code schematic_spice spice`; in a `.shcomp`: as netlist text |
| `G`/`V`/`E` text, launchers, logos, display probes, `L B P A` drawings, graphs, images | dropped; listed in the report (drawings as one line per file) |
| `T` text | `text` + `size` (x scale); floaters (`name=`), `tcleval`, `@spice_get*`, `@#` texts → dropped as display-only |
| any other `C {sym}` | `inst <id> <cellref>` |

- **Cell refs:** same library → `./<cell>`; another library → `<lib>:<cell>`; a mapped xschem `devices/` symbol → `core:<cell>` (§6).
- **Instance placement:** `C` x,y,rot,flip → `at x y <orient>`, by the table below.
- **Instance params (D40):** `name` always (missing → `x<line>` + `needs_review`). A parameter is kept only if its whitespace-normalised value differs from the symbol template default (for `core` targets: if the translated effective value — instance, else xschem template — differs from the core default). Values: `number` kinds as expressions (SPICE number, or `'…'`/`{…}`/bare expression translated per §5 with identifiers matched case-insensitively to the parent cell's parameters and the bench variables), else `needs_review`; `bool` from `true/false/1/0/on/off/yes/no`; `string`/`choice` as template text with literal braces doubled and words naming a parameter of the containing cell turned into holes (`w='wp*2'` → `'{WP}*2'`: the netlister specialises subcircuits, so parent parameters must be evaluated in the parent).
- **Inherited pins:** every instance gets `inherit <pin> <net>` for every implicit pin of its target (instance value, else template default): xschem resolves `@VDD` in the instance's own scope, which an explicit inherit reproduces.
- `spice_ignore=true|short|open` → `ignore <value>`; `tcleval` → `needs_review`. Instance `format`/`lvs_format`/`spice_sym_def` different from the symbol's, and a folded `spiceprefix` changed per instance → `needs_review`. Instance `device_model` (and a mapped device's template `device_model`) → a bench `code model_<name>` block (identical texts once), or, in a component, its schematic impl's `body`.
- `schematic=<alt>` on an instance → a generated cell `<cell>__<alt>.shcomp` (same symbol, alternate schematic) the instance points at; listed in the report; unresolvable → `needs_review`.
- Attributes that are neither parameters nor netlist controls (`footprint`, `sig_type`, …) are dropped and counted per key.
- **Bench variables:** `.param k=v …` lines of bench code (outside `.control`, not continued) become `var k <expr>` records when every value translates and names only variables defined the same way; the line leaves the code. A bench copy also defines the component's `number` parameters as variables (xschem leaves them undefined at top level; these are its defaults).
- **Ids:** `IdRng::for_import("<lib>/<source file>", line·8 + k)` with collision redraw, so re-importing unchanged input is a no-op diff.
- **Coordinates:** i32 milli-units; every rounding is listed in the report (D33).

### 3.1 Orientation

`(rot, flip)` → `[R0, MR0], [R90, MR90], [R180, MR180], [R270, MR270]` (indexed `[rot mod 4][flip ≠ 0]`): xschem flips x, then rotates clockwise on screen in quarter turns, which is format spec §3's `M` then `R`.

Evidence (`crates/import/tests/translate_corpus.rs`, derived independently of the table): for 1,289 device instances of the 301 golden benches whose golden line lists ≥ 2 label-named nets, every candidate orientation places the pins and votes when all checked pins land on the named nets.

| rot flip | R0 | R90 | R180 | R270 | MR0 | MR90 | MR180 | MR270 | winner |
|---|---|---|---|---|---|---|---|---|---|
| 0 0 | **962** | 0 | 0 | 0 | 409 | 0 | 44 | 0 | R0 |
| 0 1 | 67 | 0 | 6 | 0 | **144** | 0 | 0 | 0 | MR0 |
| 1 0 | 0 | **55** | 0 | 0 | 0 | 49 | 0 | 3 | R90 |
| 1 1 | 0 | 4 | 0 | 0 | 0 | **14** | 0 | 0 | MR90 |
| 2 0 | 0 | 0 | **24** | 0 | 0 | 0 | **24** | 0 | tie (only x-symmetric devices observed) |
| 2 1 | 0 | 0 | 4 | 0 | 0 | 0 | **19** | 0 | MR180 |
| 3 0 | 0 | 0 | 0 | **32** | 0 | 0 | 0 | 25 | R270 |
| 3 1 | 0 | 0 | 0 | 11 | 0 | 0 | 0 | **14** | MR270 |

The same test checks interface pin order: 170 golden `.subckt` lines list ports exactly in the translated pin order (XFAIL: `sar_adc` uses the `D[5..0]` bus form outside the netexpr grammar; `xcross`/`doublepin` use duplicate pin names).

## 4. Templates (`format` strings → expr templates)

| xschem | Schemify template |
|---|---|
| `@name` | `{name}` |
| `@spiceprefix` with template `spiceprefix=X` | `X` (literal); without one: `{spiceprefix}` if that parameter exists, else nothing |
| `@model`, `@<param>`, `@<implicit pin>` | `{model}`, `{<param>}`, `{<pin>}` |
| `@pinlist` | `{pins}`; spelled out `{a} {b} …` (ordinary pins) when the cell has implicit pins |
| `@@P`, `@#n` / `@#n:net_name` / `@#P:net_name` | `{P}` (renamed pins follow) |
| `@symname` | `{cell}` |
| `@savecurrent` (a parameter) | `{savecurrent ? "\n.save i(" + name + ")" : ""}` |
| `%x` | `{x}` for a parameter, else the literal `x` |
| `\x` (attribute-level escape, incl. the token terminator `\` and `\$`) | `x` |
| literal `{` / `}` | `{{` / `}}` |
| `'…'` SPICE expressions | verbatim (tokens inside converted) |
| `expr(…)`, `expr_eng(…)`, `expr_eng4(…)` | `{…}`, `{eng(…, 5)}`, `{eng(…, 4)}` with the body translated (§5) |
| `tcleval(… [ev {…}] …)`, `[ev7 {…}]` | `{eng(…, 4)}`, `{eng(…, 7)}` (Tcl `%.4g`/`%.7g`) |
| an `@x` naming a pin (not an attribute) | nothing (xschem renders an unresolved attribute empty) |
| other unknown `@tok` | a `""` string parameter (§2), counted |
| `@path`, `@schname`, `@topschname`, `@symname_ext`, `@prop_ptr`, `@sch_last_modified`, `@spice_get_*`, `@tcleval(…)`, other `[cmd]`, `$VAR` in a cell template | `needs_review` |

Symbol `T` texts become `stext` templates the same way; only `name`, `cell` and parameters may appear (other tokens render empty); display-only texts are dropped.

## 5. Expressions and Tcl

- `expr()` bodies → expr syntax: `@x` → `x`, `^` power, `**` → `^`, `log` → `log10` (xschem) but `ln` in SPICE values and Tcl, `ln int round sqrt exp sin cos tan asin acos atan atan2 sinh cosh tanh abs floor ceil min max pow` kept, Tcl `double(x)` → `(x)`; xschem constants `pi e k h echarge abszero c` → `pi exp(1) phys.k phys.h phys.q (-phys.kelvin0) phys.c`; numbers keep SI suffixes (`meg`, `mil`, `a` → `e-18`), trailing unit letters are stripped (`1.6V` → `1.6`). The result must compile (`schemify_expr::Program::compile`) or the construct is reviewed.
- **PDK variables:** `set NAME value` lines of the PDK `xschemrc` whose value references a variable and has no command substitution (`set SKYWATER_MODELS ${PDK_ROOT}/${PDK}/libs.tech/combined`) → project `set NAME value` (first definition wins; `XSCHEM_*` and `$env(…)` reads skipped). In `tcleval(…)` netlist text (code, `device_model`, `spice_sym_def`): `$NAME`, `$::NAME`, `${NAME}` → `${NAME}` when `NAME` is such a variable or `PDK_ROOT`/`PDK` (the PDK's own globals, from the environment); `\c` → `c`; every literal `$` → `$$` (netlist §7.1 expansion syntax). Netlist text outside `tcleval` keeps its `$` literally (doubled). Any `[command]` in netlist text, or another variable, → `needs_review`.
- `[abs_sym_path f]`, `[sim_is_*]` outside the `simulator_commands` idiom, `symtype`/`hierarchy_config` outside `type=`/`schematic=` → `needs_review`.
- Display-only Tcl in texts (`@spice_get_*`, `ngspice::get_node`, `display_*_params`) → dropped, counted.
- Anything else → kept verbatim in a `needs_review` item and **never** guessed into a netlist: the field stays at its default (an untranslatable leaf format makes the impl `ignore true`).

## 6. The `core` library

`lib/core/*.shprim`, bundled through `import::core_library()` (`include_bytes!`, no file system), authored from SPICE semantics: `ammeter bsource cap cccs ccvs diode gnd ind isource nmos4 npn parax_cap pmos4 pnp probe res switch tline vccs vcvs vdd vsource vswitch`. Pins are in SPICE order at the positions xschem's devices use (imported schematics connect by geometry); two-terminal cells name their pins `p`/`n` so `m` stays the multiplier. Values that are SPICE text (`res`/`cap`/`ind`/source `value`, `bsource` `func`, `model` names) are `string` parameters rendered verbatim; geometric and count parameters (`w l area m z0 td`) are `number`. `gnd`/`vdd` are net-label shorthands (`category label`, impl `ignore true`): placing one means a `label` record with `lab` (global); the importer writes labels for xschem's `gnd.sym`/`vdd.sym` (goldens: `.GLOBAL GND`, `.GLOBAL VDD`, names kept).

`import::DEVICE_MAP` maps `devices/<name>.sym` → core: `ammeter bsource(VAR→src, FUNC→func) capa capa-2→cap cccs ccvs delay_line→tline diode ind isource nmos4 npn parax_cap pmos4 pnp res spice_probe→probe switch_ngspice→vswitch vccs vcvs vsource`, with pin renames. A row applies only when the xschem symbol found has exactly the row's pins at the core positions; otherwise (and for unmapped `devices/` symbols) the symbol is converted like any other library. Coverage on the corpus: 1,832 of 1,917 instances of `devices/` cell symbols (95.6 %) map to `core`.

## 7. Report

`import-report.md` (tables) and `import-report.txt` (tab-separated: `class file line records detail`; `file` lines give records in/out per input). Item classes: `needs_review:<reason>` (`tcl expression pin-token builtin-token drc code-in-component instance-text missing-symbol unsupported name port`), `dropped:<reason>` (`malformed not-a-cell mapped-to-core schematic-graphic display-only zero-length-wire non-electrical other-netlister-text symbol-wire-or-instance layer-out-of-range embedded-symbol in-bench-copy`), `rounding`, `unknown-token`, `alt-cell` (alternate and bench-copy cells); plus unused instance attributes by key and the `devices`→`core` instance counts.

**Count conservation** (asserted per input file): records in (top level plus embedded bodies) = records out (emitted as a record, or folded into one: `v`, `K`, comments, pin boxes) + records reported (each item states how many records it accounts for; field-level items account for none). Re-translations for alternate cells and bench copies are not counted twice.

## 8. Resolved questions (golden evidence)

- **Empty `K {}`:** no corpus file has a non-empty `G` together with an empty `K`, so no golden distinguishes the two readings in the research (`feature-inventory` §5.8 says both "K resets to NULL" and "K if non-empty, else G"). Kept: `K` if non-empty, else the first non-empty `G`; the 23 `devices/` symbols with only `G` (e.g. `code.sym`) netlist their `G` attributes in the goldens.
- **`@symname` inside `[hierarchy_config …]`:** replaced by the symbol name before the proc runs: golden `ihp-sc-tran_inv` expands `sch/xschem/sg13g2_inv_1.sch` (its `** sch_path:` line) for symbol `sg13g2_inv_1.sym`, the proc's default path for that name.
- **`sim_pinnumber` ties:** none in the corpus (every all-numbered symbol uses distinct numbers), so ties keep file order (stable sort) by rule, without evidence. Reordering itself is exercised by `nmoscl_2`/`nmoscl_4` (numbers 2, 1 over file order `vss vdd`).
- **Layers ≥ 22:** no corpus record uses one. xschem's default `cadlayers` (22) does not load them, so they are dropped and reported (`layer-out-of-range`).

## 9. Amendments (2026-10-10, import translation)

Against the first version of this spec: integer defaults are `number`, not `int` (§2); `expr()` defaults are overridable `number` parameters, not `computed` (§2); SPICE-quoted defaults are `string`, verbatim (§2); instance usage can force `string` (§2); unknown `@tokens` become `""` parameters (§2, §4); explicit-`@@` formats fix pin order (§2); colliding pins are renamed (§2); implicit pins follow `extra` order and every instance carries explicit `inherit` lines (§2, §3); leaf impls without a usable format are `ignore true` (§2); bench copies `<cell>__top.shbench` and the non-subcircuit `.shcomp` with a leaf default (§1); library naming (§1); labels carry `global true` (§3, with format/netlist specs); ports for name-bound interface pins (§3); instance `device_model` → bench code or schematic-impl body (§3); `.param` → bench `var` extraction (§3); the orientation table and its evidence (§3.1); `@savecurrent`, `%x`, `[ev]`/`[ev7]`, pin-named attribute tokens (§4); `$$` escaping and `PDK_ROOT`/`PDK` (§5, with netlist §7.1); `core` pin names `p`/`n`, string value parameters, `gnd`/`vdd` as label shorthands, the map and its coverage (§6); report classes and per-file conservation (§7); resolved questions (§8).
