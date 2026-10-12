# Waveform viewer

Read when building or changing plots, cursors, measurements, or schematic ↔ waveform cross-probing.

## Probe-to-plot

- From the schematic: click a net to plot V, a pin or device to plot I, and use a modifier to plot power; drag from one node to another for a differential. The cursor shape previews which of these a click will plot (LTspice's probe, clamp and thermometer). No modal "pick analysis first" step (Virtuoso's Direct Plot is modal and liked less).
- Cross-probing runs both ways: hovering a trace highlights its net on the schematic, and selecting a net highlights its trace.
- Moving a cursor in time back-annotates instantaneous V and I on the schematic. xschem users called this "awesome".

## Layout

- **Strips on a shared X axis.** Drag a trace to a strip to stack it, or onto another trace to overlay it. Avoid a per-curve dropdown (disliked in KiCad 10).
- A plot shows within 1 s even for huge data: draw a decimated envelope first and refine. Signal names list instantly and values load lazily, as in Surfer. Never freeze (ViVA's "plot all" took 13 min).
- The signal list filters as you type and sorts active signals first.

## Cursors and markers

- At minimum two cursors A/B with a Δ readout: Δx, Δy, slope, 1/Δx.
- A per-trace ΔY, cursors placed by typing X or Y, snap to an edge or trace, and a cursor that follows a drag.
- Single-key markers in ViVA style: point, vertical, horizontal, A, B, delta. They go through the keymap, so they can be rebound.
- Window stats in one gesture: average and RMS over the visible range (LTspice's Ctrl+click).

## Measurements

- Measurements are **live objects**: they re-evaluate when results reload, persist across sessions, and use the same expression language as the docs `var` blocks (D19, D20). Define once, then reuse in plots, specs and the optimizer.
- No RPN or stack calculator: an expression with function completion instead.

## Analysis-specific defaults

- **AC:** Bode by default, with log frequency, dB magnitude solid and phase dashed on the right axis. One action switches to Nyquist or group delay.
- **Families** (sweep, corner, MC): map each sweep variable to its own channel (strip, colour, dash style). Every member gets a legend or hover tag naming its parameter values, never just "colour = run order".
- **MC:** histogram plus stats, linked to the traces.
- **Eye:** fold on a clock period (recovered, ideal or a reference signal); eye height and width are measurement functions.
- **Axes:** ticks on 1-2-5 values; units on the axis with SI prefixes; non-overlapping unit labels on secondary axes.

## Colour

- A trace palette per theme, colour-blind safe (Surfer uses Petroff's), with no yellow on light or dark blue on dark. Dash styles take over when printing in monochrome. Tokens are in `schemify-theme`.
