---
name: analog-eda-ux
description: UI/UX rules for an analog IC schematic editor and simulator front end, drawn from what Virtuoso, Custom Compiler, ADS, LTspice, KiCad, Altium and xschem users praise and resent. Use when designing, changing or reviewing any Schemify UI surface (canvas, toolbar, menus, panels, property editing, op-point annotation, simulation setup, waveform viewer, themes), or when asked to make the UI "modern", "clean" or "better".
---

The user is an analog designer in the **loop**: tweak a size → simulate → read gm/gds/vdsat on the schematic → probe a waveform → repeat, hundreds of times a day. Every UI decision is judged by one question: *does it shorten or protect the loop?* Designers forgive an ugly UI and never forgive friction ("not beautiful, but very productive"). For a designer, "modern" means calm, fast and dense, not spacious, animated or bubbly.

Evidence for every rule: [docs/research/eda-ui-preferences.md](../../../docs/research/eda-ui-preferences.md) and [xschem-complaints.md](../../../docs/research/xschem-complaints.md). Cite a rule by its word (e.g. "breaks *muscle memory*").

## Rules

1. **Muscle memory.** Keys and mouse buttons never move. D25 makes xschem's keymap the default, with a Virtuoso preset; single mnemonic letters (`i w q e x f u r`). A new look ships as an opt-in preset, and the classic set stays one click away (LTspice 24 and KiCad 6 met revolts; LTspice's "Restore Classic" defused part of it). Every key and button can be rebound, and the bindings survive upgrades.
2. **Point-and-key.** Hover an object and press a key, without selecting first (KiCad v5, LTspice, Virtuoso). Select-then-act also works; both coexist. A tool stays armed until Esc, and wires start, bend and end from the keyboard alone.
3. **Speed is a feature.** Never block a frame on I/O, simulation, netlisting or a large plot. Show something within 1 s and stream the rest (Surfer's lazy loading). Cold start and netlist time are the numbers designers quote. Fusion 360 had a modern look and still lost its users to lag.
4. **Calm canvas.** Dark by default; light is required for print and papers; Light/Dark/System is a setting. The grid is sparse, dim dots, thinning out as you zoom out; a bright or dense grid gets called "blinding". Chrome is neutral grey, and saturated colour is **reserved for circuit meaning** (nets, pins, selection, highlight, ERC severity). Detail in `schemify-theme`.
5. **The canvas is precious.** One chrome row where possible, panels that toggle with one key, and nothing floating that the user can't dismiss (Altium's ActiveBar). Nothing modal covers the schematic: dialogs that grab focus are the top xschem complaint, and the modal `q` form is Virtuoso's.
6. **Visible options.** The active tool's options (orientation, snap, wire mode, filters) sit in a passive inline bar in the status row, not behind a hidden F3 form (Virtuoso's most-cited complaint). Custom Compiler is praised for doing exactly this.
7. **Docked inspector.** Properties live in a docked, non-modal, multi-select table. Near-cursor inline edit handles the quick one-off change. See [inspector.md](inspector.md).
8. **Results on the schematic.** Show the DC op point next to each device as switchable annotation sets (params / op / model), with a hover balloon for the rest, settings that persist, and editable value labels. This is the signature analog interaction.
9. **Probe-to-plot.** One click on a net plots V, a pin plots I, and a modifier plots power; drag between two nodes for a differential. No mode switch: the cursor shape previews what a click will plot. See [waves.md](waves.md).
10. **State survives re-runs.** Zoom, cursors, traces, annotation sets and panel layout persist across simulations and sessions, keyed by signal or object name. Users hate re-navigating after every run.
11. **Honest numbers.** Display engineering notation with an SI prefix and unit (`1.23 µA`), 3–4 significant digits, and full precision on copy. Input accepts SPICE suffixes, and the display shows the *parsed* value: `1M` is milli, so warn on it. The display never shows a value different from what is simulated. Tables sort numerically, with `1k` < `2.2k` < `10k`.
12. **Live feedback, no silent edits.** Wires rubber-band while dragging, junctions appear live, and a short or merge is flagged as it happens. Never silently merge nets on drop ("some i/o's got grounded").
13. **Cheap hierarchy.** Descend and return on single keys, a tree that jumps straight to a deep instance, and a net navigator listing every occurrence. The breadcrumb path is always visible.
14. **Text reachable, not in the way.** SPICE directives and netlist text are hidden behind structured UI by default and one key away for experts. Errors name the object and attribute in plain words.
15. **Crisp chrome.** Thin vector icons at 16 px with tooltips naming the command *and its key*, plus text labels where space allows. Avoid "cartoon" or fat icons. Render at device scale on HiDPI from day one. Frequent tools never hide in a flyout.
16. **Settings are text.** Every preference is a line in `init.lua` (D9) or a records file: shareable, diffable, persistent. The GUI writes the same file it reads; no setting is GUI-only.
17. **One consistent grammar.** The same gesture does the same thing in every editor (schematic, symbol, docs, waves), whatever the object kind. An interaction that depends on the item type is a bug (an Altium complaint).

## Checklist

Run this against every render or design. Give each item a verdict of pass, fail or n/a, and name the rule above.

- [ ] No existing key, accelerator or mouse binding changed, or the change is in an opt-in preset. (1)
- [ ] Nothing new is modal over the canvas; any new panel is docked and toggles with one key. (5, 7)
- [ ] Chrome takes ≤ ~10% of the window height at 1280 × 800; canvas is the largest region. (5)
- [ ] Chrome uses only neutral greys and one accent; saturated hues appear only on circuit meaning. (4)
- [ ] Grid is dimmer than every circuit layer; selection and highlight stay distinguishable from net colours. (4)
- [ ] Every new control has a tooltip with its key; every icon has an accessible label. (15)
- [ ] Numbers show unit and SI prefix; inputs show the parsed value. (11)
- [ ] Tool options are visible inline while the tool is armed. (6)
- [ ] The look holds in both the dark and light themes (render both). (4)
- [ ] Nothing new waits on the simulator or the disk in the frame path. (3)

## Anti-taste (what "modern" must not mean here)

- Large padding and hamburger menus that hide functions: designers want **density**.
- Animations and transitions on canvas operations: they add latency to the loop.
- Rounded "card" UIs and drop shadows over the canvas: chrome stays flat and quiet.
- Command palette *instead of* keys. A palette is welcome as a discovery layer, as Surfer shows; a typo must not run an unrelated command.
