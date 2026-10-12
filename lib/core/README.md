# lib/core: the built-in device library

The 23 primitives every project can place as `core:<cell>`, compiled into the binary
(`schemify_import::core_library`). Their pins, parameters and `impl` formats are the netlist
contract: **pin names, order and coordinates never move** (placed instances attach to them).
Only rank-2 graphics and `stext` may be redrawn, and graphics never reach a netlist.

## Drawing kit

The symbols are drawn to read like Razavi or Sedra-Smith figures, from one small kit
(`.claude/skills/schemify-symbols/SKILL.md`; evidence in `docs/research/symbol-conventions.md`).
G = 10 units, the pin pitch.

| Element | Rule |
|---|---|
| Leads (pin to body) | plain 1× stroke |
| Body | `width 2` (twice the wire); capacitor plates `width 3` |
| Arrowheads | filled (`fill true`), 0.5G × 0.375G, at the pin end |
| Fill | none on bodies |
| Labels | `size 0.14`, a column at body edge + 0.5G: name, value/model, then `{eng(w)}/{eng(l)}` |

Per device: MOS has a solid 2G channel, a gate plate 0.25G off it, the arrow on the source
(out for N, in for P), no PMOS bubble and the bulk as a thin lead; BJTs have no circle; the
resistor is a 3-cycle ANSI zigzag; `parax_cap` is the capacitor dashed and thin; independent
sources are 1.5G circles, controlled ones diamonds with +/− or an arrow and left control
leads marked +/−; `bsource` is a diamond tagged `f(·)`; switches are a 30° blade between
open poles; `gnd` is three bars, `vdd` a T bar.

The contact sheet `crates/app/tests/fixtures/symbols/sheet.shcomp` places every cell plus a
5-transistor OTA; `cargo test -p schemify-app --test symbols` checks its goldens
(`core_symbols_{dark,light}{,_4x}.png`).
