---
name: schemify-theme
description: Colour and chrome tokens for Schemify's UI and canvas, with a checker that gates them. Use when choosing or changing any colour, grid, highlight palette, waveform trace palette, font size or spacing in `ui.records`, or when building a theme preset.
---

Colour on a schematic is **meaning**. The chrome is neutral grey plus at most two accents; every saturated hue on screen says something about the circuit (net, pin, selection, highlight, ERC severity, pass/fail). Rule 4 of `analog-eda-ux` (*calm canvas*) is the why; this skill is the how.

## The gate

```
python3 docs/dev/skills/schemify-theme/check_theme.py [base.records] [preset.records]
```

Later files override earlier ones, the same D8 last-wins rule. Exit 0 is required for any theme you ship as new. It checks:

| Check | Threshold | Why (research) |
|---|---|---|
| Circuit layer vs background contrast | ≥ 3.0 (WCAG non-text); text layer ≥ 4.5 | low-vision users; LTspice 24 "can't see the grid" |
| Grid contrast | 1.3 ≤ grid < dimmest of wire/symbol/pin | a bright grid is "blinding" (Virtuoso IC6); a faint one is invisible (LTspice 24) |
| Highlight separation, 8 highlight layers + wire + selection | ΔE76 ≥ 20 normal, ≥ 10 under deutan and protan simulation | Virtuoso's 4–5 probe colours repeat; KiCad's red/green complaint |
| Chrome text vs its background | ≥ 4.5 | — |
| Decorative saturated chrome colours (Lab chroma > 30, status selectors exempt) | ≤ 2 | chrome must not compete with the circuit |

The xschem-parity default **fails** this gate on purpose: it reproduces xschem's colours (D6). Leave it alone and put a new look in a preset. The highlight layer list is a copy of `HILITE_LAYERS` in `crates/app/src/editor/parity.rs`; when that constant changes, update `HILITE` in the script.

## Layer roles (canvas)

`style canvas.<theme>.<n>`, `fg` only. Roles: `0` background, `1` wires and net labels, `3` text, `4` symbol bodies, `5` pins and ERC/short markers, `.grid`, `.sel`; `8 9 10 12 13 15 17 21` per-net highlights; the remaining `6+` are user drawing layers. Each role keeps one meaning across both themes.

## Baseline modern palette (passes the gate)

A starting point, not settled design. Change a value only by re-running the gate.

| Role | Dark | Light |
|---|---|---|
| background | `15171b` | `fbfbfa` |
| grid (dots) | `2c3036` | `dcdcd8` |
| wire | `5aaeea` | `1f5fbf` |
| text | `c9cdd3` | `1e2126` |
| symbol | `8fc35f` | `2f7d1f` |
| pin / ERC | `e2574c` | `b3261e` |
| selection | `ffffff` | `000000` |
| highlights 8…21 | `9406f9 06f906 b8148f 7f75f0 06f9a8 c78605 14b89c e6196e` | `0606f9 149cb8 b81474 f906f9 1c0495 4e0a5c 5c180a 959504` |

Chrome (dark): panel `1e2127`, raised `23262c`, hover `2a2e35`, active `2f343c`, border `3a3f48`, text `d4d7dc`, muted text `8b919a`, accent `4c8dff`. Chrome (light): panel `f3f4f6`, raised `ffffff`, hover `e6e8eb`, active `d8dce1`, border `d0d4da`, text `1f2328`, muted `5f6670`, accent `2563d0`. Status colours (error, warning, info, pass, fail) are separate tokens and exempt from the accent budget; still pair each one with a glyph so colour never carries meaning alone.

The highlights are saturated **on purpose**. A greedy search showed that no softer set of eight stays distinguishable under colour-blind simulation. To change them, rerun that search: candidate HSL grid, maximise min(ΔE/20, ΔE_deutan/10, ΔE_protan/10) against the fixed core layers, then keep only sets that score ≥ 1.

## Rules the gate can't check

- **Halo, not recolour**: draw a highlighted net as a wide translucent stroke under the wire, keeping the wire's own colour (KiCad netclass halo, Virtuoso dynamic highlight).
- **Region and state glyphs** (saturation, triode, ERC severity) pair colour with shape.
- **Trace palette** (waves): one palette per theme, colour-blind safe, no yellow on light or dark blue on dark, plus dash styles for monochrome print. Reuse the highlight set so that a net and its trace share one colour (cross-probe).
- **Library colours never override the theme.** A symbol or PDK file may name a layer, never an RGB value (EEVblog: overrides make a theme "useless").
- **Type**: one UI font at 13 px for chrome, tabular figures for numbers; canvas text sizes come from the design, not the theme.
- **Spacing**: a 4 px grid (pad 4/8, row height 24–28). Dense, not cramped. `analog-eda-ux` rule 5 caps chrome at ~10% of window height.
- **Light theme is not an inversion.** Retune every role through the gate; dark-tuned colours on white are a known ViVA complaint.
- Every colour is a preference reachable from `init.lua` (D9), so users can share themes as files.
