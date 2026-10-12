# Inspector, tables and op-point annotation

Read when building property editing, simulation setup tables (outputs, corners, sweeps, specs, results) or on-schematic annotation.

## Property inspector

- **Docked panel**, non-modal, which keeps the canvas live while open. Virtuoso's Property Editor assistant and ADS 2024's docked panel replaced modal forms.
- **Multi-select** shows the shared fields. A field whose values differ across the selection is marked *varies* (Virtuoso uses bold orange); typing into it sets every selected object.
- **Default vs overridden** look different (Virtuoso: blue italic for defaults), with one action to reset a field to its default. This matches the param schema: instances store only overrides.
- **Expression and value** side by side: `W = 2*Wn` shows `2*Wn` and `→ 1.2 µm`. Errors show inline in the row, never in a pop-up.
- **Quick edit**: double-click a value label on the canvas, or hover it and press a key, to edit in place near the cursor (xschem users ask for this; Altium and KiCad users want it alongside the panel).
- **Find similar → edit all**: select by property (same cell, same param value) and edit the set. This is Altium's differentiator, and KiCad's selection filter is called "the work of a genius".
- Callbacks and derived params update live as you type; out-of-range values are flagged in the row at once.

## Setup and results tables

- Setup and results are **spreadsheet tables** in one place (merging ADE L and XL was welcomed): filter, choose columns, sort numerically, export as CSV.
- Corners × outputs grid with pass/fail per spec cell. Failing cells use the error colour plus a glyph, so colour is never the only signal.
- Each results row links back: clicking it plots the trace, annotates the schematic with that corner's op point, and selects the device.
- Monte Carlo shows as a histogram plus a stats table (mean, σ, yield), linked to traces, not a hairball of curves.
- Sweep any variable from the cell's `var` table (D19). Users miss this most when they leave Virtuoso.

## Op-point annotation

- **Annotation sets** are declared per device class by the PDK or cell (`params`, `op`, `model`) and cycled with one key. Typical op set: `id gm gds vdsat vth region`.
- About 5–6 values per device on canvas; the rest go in a **hover balloon** anchored at the device (Virtuoso's balloon is liked).
- Show the **region** at a glance: saturation, triode or subthreshold, as a glyph or tint on the device, never by recolouring the whole symbol.
- Current arrows follow a stated, predictable convention (into the pin is positive). Wrong-looking arrows are a known complaint.
- The chosen sets and visibility persist per cell (rule *state survives re-runs*).
- Values follow rule *honest numbers*: `gm 1.23 mS`, `vdsat 112 mV`.
