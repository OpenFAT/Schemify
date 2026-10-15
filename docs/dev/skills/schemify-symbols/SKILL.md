---
name: schemify-symbols
description: Drawing rules and redesign loop for Schemify device symbols (`devices/*.shprim` and any `.shprim`/`.shcomp` symbol view). Use when drawing, redrawing or reviewing a device symbol, its labels or op-point slots, or when asked to make the device library look better than xschem's.
---

The target is **textbook quality**: a symbol should read like a figure in Razavi or Sedra-Smith, built from one small kit (stroke weights, one arrowhead, one label column), and use nothing a reader of those books wouldn't recognise. CircuitLab abandoned its invented MOSFET because nobody could read it.

## Frozen: pins

**Pin names, order and coordinates never move.** Every placed instance and wire in every user design is attached to them, so moving a pin silently breaks connectivity. A redesign changes only rank-2 graphics and `stext`. If a pin truly must move, that is a new cell, or a decision for the user with a migration plan.

Grid: **G = 10 units**, the pin pitch. Every pin endpoint is on G. Body detail may use G/8 sub-grid points (integers, D33). The current pins give two-terminal parts a 6G span (±30).

## The kit

| Element | Rule |
|---|---|
| Body stroke | 2 × wire stroke. Leads (pin to body) stay 1 × so the body reads as "the device" |
| Joins and caps | round |
| Arrowhead | filled, one size everywhere: 0.5G long × 0.375G wide, at the pin end of its lead (0.35G × 0.25G vanished at fit zoom on the contact sheet) |
| Fill | none. Bodies are open outlines (KLC S3.3; the "puristic" look users praised in Qucs) |
| Text | 0.5G cap height, i.e. `size 0.14` (a 0.7G em; floor 0.3G for pin names), in a **label column** right of the body at body edge + 0.5G: name on top, then value/model, then W/L/nf |
| Op-point slots | up to 5 rows continuing the label column, plus one voltage slot per pin 0.25G off its lead; shown in the annotation colour and hidden first when zoomed out |
| Variants | data, not new shapes: thick-oxide = 2× gate stroke; IEC resistor box and "lazy MOS" are alternate glyphs of the same cell |

## Per device

- **MOS (nmos4/pmos4):** solid channel 2G, gate plate parallel with a 0.25G gap, gate lead 1G and centred. Arrow on the **source** lead (out for N, in for P), no PMOS bubble. Bulk is a short 1× lead from the channel's back centre, so a 4-terminal device still reads as the textbook 3-terminal one.
- **BJT:** no envelope circle (the circle means a discrete package). Emitter arrow at the pin end, same C–E span as the MOS.
- **Resistor:** ANSI zigzag with 3 full cycles; body 3G × 1G (3 cycles on the G/8 grid need 2.25G or 3G; 3G reads better); peaks on the G/8 grid. The IEC box is the alternate.
- **Inductor:** 3–4 semicircular humps over the same body length as the resistor.
- **Capacitor:** two straight plates 1.5G long with a 0.4G gap; the plates are drawn heavier than the body. **parax_cap:** same shape, dashed or half weight, muted, so it reads as "not a real device".
- **Sources:** independent = circle 1.5G across, with +/− for V or an arrow for I. Controlled = diamond ~17% larger, and the four kinds differ only in the inner mark. **bsource** = diamond plus a small `f(·)` tag. Control leads are short, on the left, with their own +/−.
- **Switch:** open blade at ~30° between two small open-circle poles; vswitch adds control leads like the controlled sources.
- **gnd:** three shrinking bars on a 1G stem, pointing down. **vdd:** a T bar on a 1G stem, pointing up, labelled with its net.
- **Junction dots** (canvas, not symbols): 0.35G across, only where 3+ wire ends meet, never smaller than 4 px.

## Renderer support

Every rank-2 graphic takes `width <1-4>` (multiples of the wire stroke), `dash <n>` and `fill true` (format.md §4, stroke and fill attributes). Weights are screen-space: a body is 2 px over a 1 px wire at every zoom. Mirrored instances right-align their symbol texts, so the label column flips to the left of the body. The sheet is `crates/app/tests/fixtures/symbols/sheet.shcomp`, rendered by `crates/app/tests/symbols.rs`; its ignored `symbol_crops` test writes an 8× crop per cell to `$SCHEMIFY_SHOTS` for review.

## Loop

The same loop as `schemify-ui-loop`, rendered on a **contact sheet**: one schematic placing every `devices/` cell, each wired to short stubs, plus one small reference circuit (a 5-transistor OTA with sources). Rendered as golden `core_symbols` in both themes and at two zooms (fit and 4×).

1. **State the pass:** which cells, and which kit rules they serve. Done when the cells are named.
2. **Redraw** only the rank-2 records and `stext` of those cells. Done when `git diff` shows no `pin` line changed (`git diff devices | grep '^[-+]pin'` is empty).
3. **Render** the sheet with `SCHEMIFY_BLESS=1`. Done when the PNGs are rewritten.
4. **Look**: compare old and new at both zooms. Give each touched cell a verdict against every row of *The kit* and its *Per device* entry. Then ask the recognisability question: would a Razavi reader name it at a glance? Done when every cell has a verdict. A fail sends you back to step 2.
5. **Prove** it with `cargo test -p schemify-app`, plus the netlist goldens (`cargo test -p schemify-netlist`): graphics never reach the netlist, so the netlist goldens must be byte-identical. Done when both pass.
6. **Report** the before/after, the verdicts and the change radius (cells touched, pins untouched).
