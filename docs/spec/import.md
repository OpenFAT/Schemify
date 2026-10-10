# xschem import v1 (normative)

Implements D1, D14, D24, D33, D40. Readers (done): `crates/import` (`xschem`, `attr`, `symbol`, `spice`, `tech`, `cdf`). This spec defines the **translation** into Schemify files. Clean-room: behaviour comes from `docs/research/` and from comparing against goldens produced by running xschem (`tests/corpus/`); never from xschem source.

**End-to-end oracle:** for every corpus bench, `import → schemify netlist` must equal xschem's golden under `tools/spicecmp` (SPICE and CDL). Differences are bugs or annotated XFAILs.

## 1. What becomes what

| xschem input | Schemify output |
|---|---|
| `X.sym` + `X.sch` (symbol `type=subcircuit` or a schematic exists) | `X.shcomp`: symbol interface + graphics from `.sym`, schematic from `.sch`, impl `schematic schematic` |
| `X.sym` with no schematic, a `format` (device) | `X.shprim` with one impl named after the lang (`spice` by default; `veriloga` when `device_model` holds `pre_osdi`/OSDI `.model`; `verilog` for `d_cosim`) |
| `X.sch` with no `.sym` | `X.shbench` |
| `type` ∈ `label`, `launcher`, `netlist_commands`, `annotator`, logo/title symbols | not cells; handled as below |

Library layout: one Schemify library per xschem library directory, same relative structure, written as `<out>/<lib>/<cell>.<ext>` plus `<out>/schemify.project` and `<out>/import-report.md` (+ `import-report.txt`, machine-readable).

## 2. Symbol interface (`.sym` → rank 1)

- **Pins:** `B 5` boxes → `pin <name> <dir> <cx> <cy>` (box center). Order: file order, or ascending `sim_pinnumber` when every pin has one (stable). `dir`: `in`/`out`/`inout` from the pin's `dir` attribute (missing → `inout`). Tiny bulk boxes (e.g. 0.156 wide) are real pins.
- **Params:** symbol `template` `k=v` pairs, except `name` (→ header `prefix` = its leading letter run) and `spiceprefix` (→ folded into the template, §4). Kind inference: SI number → `number`; integer → `int`; `true/false` → `bool`; listed in `available_models` → `choice` with `choices`; otherwise `string`. `expr('…')` values → `computed` (translated, §5).
- **Implicit pins:** names in `extra=` that are used as `@x` nets, and `@body`-style net params → `pin <name> inout` with `inherit <default-net>` (D29), default net = the template value.
- **Device limits (D14):** from model cards (`spice::scan`) → `min`/`max` on `L`/`W` (union of bins); from `drc=` procs → recognized min/max/nf rules; from tech grid → `grid` on `L`/`W`. Every unrecognized `drc=` → `needs_review`.
- **Impl and templates:** `format` → `impl <lang> <lang>` with `format spice <template>`; `lvs_format` → `format cdl <template>`; `spectre_format` → `format spectre`; `verilog_format` → `format verilog`. `device_model` → `body <impl>` verbatim (plus the lang detection of §1). `spice_ignore=true` on a symbol → impl attr `ignore true` (netlist §7).

## 3. Schematic (`.sch` → rank 3)

| xschem record / symbol | Schemify |
|---|---|
| `N` wire | `wire` (cached `lab=` dropped: names come from labels) |
| `C {lab_pin.sym}` / `lab_wire.sym` / `lab_show.sym` | `label` (anchor = symbol pin position after transform, net = `lab`) |
| `C {ipin.sym}` / `opin.sym` / `iopin.sym` | in a `.shcomp`: `port` bound to the same-named interface pin; in a `.shbench`: `label` |
| `C {noconn.sym}` | `noconn` |
| `C {code.sym}` / `code_shown.sym` | in a `.shbench`: `code <name> spice` (`value` text → verbatim lines; `only_toplevel` honoured); in a `.shcomp`: `needs_review` |
| `C {launcher.sym}`, `title.sym`, logo/image symbols, `B 2 … flags=graph`, `flags=image` | dropped; listed in the report (launchers are executable content, D7) |
| any other `C {sym}` | `inst <id> <cellref>` |
| `T` text | `text` |
| `L/B/P/A` graphics in a `.sch` | `text`-free graphics are dropped with a report line (schematic drawings are not part of v1 rank 3) |

- **Cell refs:** a symbol in the same xschem library → `./<cell>`; in another library → `<lib>:<cell>`; xschem's `devices` library → the bundled `core` library by name (§6).
- **Instance placement:** `C` x,y,rot,flip → `at x y <orient>`. The `(rot, flip)` → `R0…MR270` table is fixed by golden tests: an instance's transformed pin positions must reproduce xschem's connectivity on every corpus bench.
- **Instance params (D40):** keep `name` always; keep a param only if its (whitespace-normalized) value differs from the symbol template default. `spice_ignore=true|short|open` on an instance → inst attr `ignore <value>`. `schematic=<alt.sch>` on an instance → a generated cell `<cell>__<alt>.shcomp` (same symbol, alternate schematic) and the instance points at it; listed in the report.
- **Ids:** deterministic `IdRng::for_import(source path)` in record order, so re-importing unchanged input is a no-op diff.
- **Coordinates:** i32 milli-units; every rounding is listed in the report (D33).

## 4. Templates (`format` strings → expr templates)

| xschem | Schemify template |
|---|---|
| `@name` | `{name}` |
| `@spiceprefix@name` with `spiceprefix=X` | `X{name}` |
| `@model`, `@<param>` | `{model}`, `{<param>}` |
| `@pinlist` | `{pins}` |
| `@@P` | `{P}` (the net on pin `P`) |
| `@symname` | `{cell}` |
| `\\` after a token | token terminator: nothing emitted |
| `\$` | `$` |
| literal `{` / `}` | `{{` / `}}` |
| `'…'` SPICE expressions | verbatim (inside the template text) |
| `expr('…')`, `expr_eng(…)` | `{…}` with the body translated (§5) |
| unknown `@tok` | empty string (matches xschem), counted in the report |
| `tcleval(…)` | §5 |

## 5. Expressions and Tcl

- `expr()` bodies → expr-language syntax: `@x` → `x`, `^` stays power, `log` → `log10`, `ln` stays, `int` stays (trunc), `round` stays; numbers keep SI suffixes; trailing unit letters are stripped.
- Known Tcl idioms (PDK doc §5.4–5.6): `$::VAR`/`$VAR` → `${VAR}` path variables resolved from project `set` entries generated from the PDK's `xschemrc` (`set VAR ${PDK_ROOT}/…`); `[ev …]`/`[ev7 …]` → expression with `eng`/canonical formatting; `[abs_sym_path f]` → path relative to the cell file; `type=tcleval([symtype])` / `schematic=tcleval([hierarchy_config …])` → their known defaults.
- Display-only Tcl in labels (`@spice_get_*`, `ngspice::get_node`, `display_*_params`) → dropped from graphics text (OP annotation comes from the annotation overlay instead), counted.
- Anything else → kept verbatim in a `needs_review` record of the report and **never** guessed into a netlist; the affected field is left at its default and the cell is flagged.

## 6. The `core` library

A bundled library authored for Schemify from SPICE semantics (not copied from xschem): `res cap ind vsource isource vcvs vccs ccvs cccs bsource diode nmos4 pmos4 npn pnp switch vswitch tline ammeter gnd vdd` with pins in SPICE order, a `spice` impl template each, and our own graphics. The importer maps xschem `devices/*.sym` names to these cells (table in the importer); unmapped `devices/` symbols → converted like any other library.

## 7. Report

`import-report.md` / `.txt`: counts (files, records in, records out, records reported — they must sum: count conservation), every `needs_review` item with source file:line and reason, every dropped construct, every coordinate rounding, every unknown `@tok`, every generated alternate-schematic cell.
