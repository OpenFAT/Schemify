Status: research, 2026-10-12. Evidence base for the `schemify-symbols` skill (`.claude/skills/schemify-symbols/`). D1: no xschem source read; GPL symbol files viewed for appearance only, nothing copied.

# Analog device symbol style: research for Schemify's core library

Scope: how to draw nmos4, pmos4, npn, pnp, diode, res, cap, parax_cap, ind, vsource, isource, vcvs/vccs/ccvs/cccs, bsource, switch, vswitch, tline, ammeter, probe, vdd, gnd so they read like textbook figures (Razavi, Gray-Meyer, Johns-Martin, Sedra-Smith).
Clean-room: no xschem source or `.sym` files were opened. The geometry figures below come from published docs, the CircuiTikZ manual and parameter tables (LPPL), KiCad library files (CC-BY-SA, used here only as measurements), and LTspice `.asy` text (measurements only).
Low-trust SEO sites (jlcpcb blog, electricalflux, etc.) are marked **(low-trust)**. Anything I inferred without a source is marked **[unsourced]**.

---

## 1. Textbook and standards conventions

### Standards status
- IEEE 315 (ANSI Y32.2 / CSA Z99) was "reaffirmed in 1993, inactivated without replacement as of November 7, 2019". IEC 60617 (online DB) is "the current international standard". The page also notes: "The standards do not all agree, and use of unusual (even if standardized) symbols can lead to confusion and errors." https://en.wikipedia.org/wiki/Electronic_symbol
  - Takeaway: no live standard binds us. US analog-IC textbook practice is the de facto reference for this audience.
- IEEE 315 symbols are "designed so that their connection points fall on a modular grid". 0.1 in (2.54 mm) is the usual pitch. https://www.edaboard.com/threads/guidelines-for-schematic-symbols-design-projects-libraries.227421/ (forum summary, medium trust)

### MOSFET variants (the main decision)
- Wikipedia (MOSFET, Circuit symbols): "The bulk or body connection, if shown, is shown connected to the back of the channel with an arrow indicating pMOS or nMOS." "If the bulk is not shown (as is often the case in IC design as they are generally common bulk) an inversion symbol is sometimes used to indicate pMOS. Alternatively, as is done with bipolar transistors, an arrow on the MOSFET's source may indicate the direction of conventional current: pointing out for nMOS, in for pMOS." "Sometimes three line segments are used for enhancement mode and a solid line for depletion mode." https://en.wikipedia.org/wiki/MOSFET
- Sedra-Smith (Microelectronic Circuits, Fig. 5.11 in several editions): (a) the full 4-terminal symbol; (b) an arrowhead on the source "thus distinguishing it from the drain terminal" that "points in the normal direction of current flow and thus indicates the polarity of the device"; (c) a simplified symbol with no bulk and the arrow on the source, used "when the source is connected to the body or when the effect of the body on device operation is unimportant". https://electrongen.blogspot.com/2017/06/schematics-why-does-circuitlab-have.html , https://commons.wikimedia.org/wiki/File:Mosfet_N-Ch_Sedra.svg
- "Razavi-style" symbols, as requested in CircuiTikZ issue #934: a solid drain-source channel line (enhancement is the default IC device), the arrow on the source, no bulk drawn ("usually NMOS bulk to VSS, PMOS bulk to VDD"), no PMOS gate bubble, and optionally a thicker gate line for device variants such as thick-oxide. The issue cites Pretl & Eberlein, "Fifty Nifty Variations of Two-Transistor Circuits" (IEEE SSC Magazine 13(3), 2021), which "uses thickened gate lines for device differentiation". This led CircuiTikZ to add `transistors/gate thickness` (v1.8.6). https://github.com/circuitikz/circuitikz/issues/934
- Arrow on the body lead is "popular in power electronics". Arrow on the source is "popular in analog signal processing circuits". (Summary of the jlcpcb/wevolver articles, **low-trust**.) https://jlcpcb.com/blog/mosfet-symbol-explained
- CircuiTikZ's own guidance: the `lazymos` option "draws lazy nmos and pmos transistors. Chip designers with huge circuits prefer this notation". The manual's comparison grid labels "no circle, no arrows" as "**DON'T do it**", because P and N then look the same. The author writes "my preferred way of drawing a MOSFET is with arrows" and moves arrows to the pin end with `transistors/arrow pos=end`. CircuiTikZ manual (circuitikzmanual.tex), §Transistors customization: https://ctan.org/pkg/circuitikz
- CircuiTikZ also has an explicit bulk option (`nmos, bulk`): it adds a 4th pin from the channel back, keeps the arrow on the source, and offers `fetsolderdot` for a dot at the bulk-source tie. Same manual, §"Bulk terminals".
- Thick-oxide/IO marking: I found no standard. The only documented convention is the thicker gate line (Pretl/Eberlein, CircuiTikZ `gate thickness`). Searches for PDK conventions found nothing authoritative. **[unsourced]**: PDKs often add a second gate line or an "HV" text tag.

### BJTs
- CircuiTikZ draws npn/pnp without an envelope circle by default. Since 1.2.6 the circle is optional, "intended in some case as the component's housing, and used to distinguish discrete components from integrated ones". So the IC convention is **no circle**. Manual §"Circles". The KiCad discrete `Q_NMOS` symbol does draw a circle (radius 2.794 mm), which is consistent with that discrete/integrated split (Device.kicad_symdir/Q_NMOS.kicad_sym, measured).
- Arrow on the emitter: out for npn, in for pnp (universal).

### Resistor: zigzag vs rectangle
- KiCad chose IEC rectangles for its official library: "IEC is an international norm, while the wiggly IEEE-resistors are mostly used in the US". `R_US` is kept as an alternate. A US user counters that the zigzag appears "on more than 95% of the schematics I drafted". Another argues the box avoids "confusion with inductors". https://forum.kicad.info/t/alternate-resistor-symbol-kicad-evaluation-for-company-use/6310
- CircuiTikZ's own default style mix is `americanresistors` with `cuteinductors` (looped coils) and European sources. Its `romano` style is the author's preferred look. Manual §Style files.
- All the US analog-IC texts named in the brief draw the zigzag. Razavi, Gray-Meyer and Sedra-Smith use zigzag resistors and humped/looped inductors **[unsourced: from familiarity with the books; I found no citation]**. For our audience the zigzag is the expected default.
- A typical IEEE-315 zigzag has "three complete zig-zag cycles (six peaks total)" (Inkscape tutorial summary, **low-trust**). CircuiTikZ's default is `resistors/zigs=3`. The LTspice resistor (`res.asy`) has 2.5 cycles over 64 units with ±16-unit amplitude: 4 grid squares long, 2 grid squares wide (16 units = 1 grid). https://raw.githubusercontent.com/evenator/LTSpice-Libraries/master/sym/res.asy

### Capacitor
- IEEE 315 style: for a polarized cap the curved plate is the negative or outer-foil electrode. The outer foil historically went to the lower-impedance net as a shield (**low-trust**: https://jlcpcb.com/blog/capacitor-symbol-explained). For a non-polar on-chip cap, two straight plates are the norm. The curved plate is worth keeping only for parasitic/MOS caps, to mark the "bottom-plate" side **[unsourced]**.
- Proportions: LTspice `cap.asy` uses plates 32 units wide (2 grid), a gap of 8 units (½ grid) and pins 64 units apart. KiCad `Device:C` uses plates 4.064 mm (160 mil) wide, drawn **20 mil thick (2× the 10 mil body line)**, with a 60 mil gap and pin ends ±150 mil. CircuiTikZ uses capacitor height 0.6 and gap width 0.2 (in units of bipole length 1.4 cm), so the plates are 3× as long as the gap.

### Sources
- Independent source = circle; dependent source = diamond. Inside: +/− marks for a voltage source, an arrow for a current source (textbook convention, **low-trust** summary at https://www.electronics-tutorials.ws/dccircuits/current-source.html). CircuiTikZ has `american controlled voltage/current source` (diamond with +/− or arrow) at 0.7 relative size, against 0.6 for the independent circle. So **the diamond is drawn about 17% larger than the circle** (pgfcircbipoles.tex: `isourceam/height=.60`, `cvsourceam/height=.7`).
- European/IEC style: a circle with a line through it, along the wire for a voltage source and across the wire for a current source. CircuiTikZ also has a "cute european" double-circle current source. Its double-circle sources are scaled so that "the full source occupy more or less the same horizontal space than one of the single-circle one". Manual §Sources.
- US analog textbooks use the circle with an arrow for current sources and the circle with +/− for voltage sources **[unsourced; matches Sedra-Smith and Razavi figures from memory]**.

### Ground / supply
- IEC 60417 symbols: 5017 earth, 5018 functional earth, 5019 protective earth, 5020 chassis. "A solid or hollow triangle represents signal or common ground". https://jlcpcb.com/blog/ground-symbols-explained (**low-trust**). CircuiTikZ offers `ground` (3 bars), `tlground` (tailless), `sground` (signal triangle) and `eground`, plus `vdd`/`vcc` (bar or arrow) and `vee`. Manual §Grounds and supply voltages.
- KLC S4.2: positive power at the *top* and ground at the *bottom* of a symbol. https://gitlab.com/kicad/libraries/klc/-/raw/master/content/symbol/S4/S4.2.adoc

---

## 2. Tool guidelines with numbers

### KiCad Library Conventions (KLC), read from the KLC source repo
- S4.1: "Using a `100mil` grid, pin origin must lie on a grid node (IEC-60617)". Pins are ≥100 mil, in steps of 50 mil, ≤300 mil. "Shorter pins are allowed on simple symbols for discrete devices such as resistors, capacitors". "All pins on a symbol must have the same length." https://gitlab.com/kicad/libraries/klc/-/raw/master/content/symbol/S4/S4.1.adoc
- S3.1: the origin is centered on symmetrical symbols, otherwise as close as possible while keeping pins on the 100 mil grid.
- S3.2: "All text fields … should use a text size of `50mil` (1.27 mm)". Pin names and numbers may go "as small as 20mil if the symbol is small".
- S3.3: "Symbol body must have a line width of `10mil` (0.254 mm)". Black-box ICs are filled with background. "Simple components (e.g. discrete components or those with a distinctive shape) should **not** be filled."
- S4.6: hidden pins are not allowed except power symbols ("any connection point must be visible").
- S4.7: active-low pins get an overbar, not the bubble pin style ("cleaner look").
- Measured from KiCad's stock symbols (gitlab kicad-symbols, `Device.kicad_symdir`):
  - `R`: body 2.032 × 5.08 mm (80 × 200 mil, aspect 1:2.5), 10 mil line, 50 mil pins, pin-to-pin 300 mil.
  - `C`: plates 160 mil, 20 mil plates, 60 mil gap, 300 mil pin-to-pin.
  - `L`: 4 arcs over 200 mil.
  - `Q_NMOS`: channel as 3 segments of 40 mil with 30 mil gaps (enhancement), gate-to-channel gap 20 mil, filled arrowhead 40 mil long × 30 mil wide, 10 mil-radius dots, discrete-device circle r = 110 mil.
- Junction dot default diameter is 36 mil (KiCad `default_values.h`), configurable in Schematic Setup → Formatting. Users asked for more sizes (gitlab issue #9797). https://docs.kicad.org/doxygen/default__values_8h.html

### LTspice (`.asy`)
- Grid: "16 LTspice units to 50 mils" (KiCad importer). The editor has 16 sub-grid points per visible grid point. Line "Normal" = 5 units (≈6 mil), "Wide" = 10 units. Font sizes 1–7 map to 36…108 mil, and the default size 2 is about 42 mil. https://dev-docs.kicad.org/en/import-formats/ltspice/index.html
- `nmos.asy` (measured): D–S pins are 96 units apart (6 grid) and the gate pin sits at y=80, level with the source, not centered. The channel is 3 segments (enhancement), and the arrow on an internal body stub points in, which is IEEE-style. InstName and Value are placed to the right of the body, stacked (WINDOW 0 at 56,32 and WINDOW 3 at 56,72). https://raw.githubusercontent.com/evenator/LTSpice-Libraries/master/sym/nmos.asy
- `res.asy`: pins 80 units apart (5 grid), labels to the right at x=36. `cap.asy`: pins 64 apart (4 grid).

### Cadence Virtuoso / analogLib
- Symbol editor snap is 0.0625 by default. https://www.ece.rice.edu/~cavallar/cadence/tutorial/layout.html (mentions; medium trust). analogLib `nmos4` has terminals D G S B. Op-point back-annotation goes through `cdsParam(n)` ilLabels on the symbol, and the number of annotatable rows is capped by the number of `cdsParam` labels (often 3–5). `cdsTerm("x")` labels at pins annotate node voltages and currents. https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/54264/expanding-the-list-of-the-transistor-operating-point-annotation , https://groups.google.com/g/comp.cad.cadence/c/8lYleSZRfrc
  - Takeaway: annotation slots are geometry the symbol owns. Reserve a parameter text column beside the body and a terminal-label spot at each pin.
- I found no published Cadence or gpdk "symbol look" guide, and no direct "analogLib looks dated" quotes. **[unsourced]**: the common complaint is wide default spacing and inconsistent label placement.

### CircuiTikZ proportions (best documented parametric style)
- Every length is relative to `bipoles/length` = 1.4 cm, "the length of a resistor (including reasonable connections)". Manual §Components size.
- Line weight: `bipoles/thickness` = **2**, "relative to the thickness of the normal lines leading to the component". **Component bodies are drawn at 2× wire width.** Transistors have their own `transistors/thickness` and, since 1.8.6, `gate thickness`. Manual §Thickness. The manual also warns that zigzag joins look bad at thickness >2 without miter/round joins (§"Details of American resistors").
- Default shape sizes (pgfcircbipoles.tex, pgfcirctripoles.tex), × bipole length:
  - Resistor: 0.8 wide × 0.3 tall, so the zigzag body is 80% of pin-to-pin length.
  - Capacitor: 0.6 plate length × 0.2 gap.
  - Inductor: 0.8 × 0.3.
  - Diode: 0.4 long × 0.5 tall.
  - Independent sources: circle 0.6. Controlled diamond: 0.7.
  - nmos/pmos: height 1.1, width 0.7, gate width 0.62, gate height 0.35, base (channel) height 0.5. Arrow position 0.6 along the source lead for nmos and 0.4 for pmos.
  - npn/pnp: height 1.1, width 0.6, base height 0.4–0.45.
- Arrowheads: `current arrow scale` = 16. Higher numbers give smaller arrows, so the arrow is about the bipole length / 16 ≈ 0.09 cm. PMOS gate dots and terminal dots share `nodes width` = 0.04 × length (radius), so a junction dot is about 8% of a resistor length in diameter.
- Labels: since 0.9.7 the transistor name sits "near the center of the device" and no longer by the collector. Old placement is still available (`legacy transistors text`), which is useful when a bulk pin occupies that side.
- Thick-lined graphene FETs "look better" with `arrow pos=end` and thickness ≥3, which shows the authors tuned for appearance.

---

## 3. What users say looks good or bad

- Qucs symbol overhaul (qucs-devel): the existing symbols were "not really clear or just outdated" and did not follow IEC. The replacement was praised for its "puristic engineering look", and the thread stresses library consistency. https://sourceforge.net/p/qucs/mailman/message/37101074/
- CircuitLab dropped its idiosyncratic MOSFET symbols for IEC 60617 after users said "I don't think I've ever seen CircuitLab's symbols used in the wild". Lesson: invented symbols lose to familiar ones, even well-reasoned ones. https://electrongen.blogspot.com/2017/06/schematics-why-does-circuitlab-have.html
- KiCad ANSI vs IEC debate: Europeans call the box the 30-year default, and US users want the zigzag. The maintainers' argument is the cost of keeping two sets. The resolution was to ship one default with an alternate (`R_US`). https://forum.kicad.info/t/alternate-resistor-symbol-kicad-evaluation-for-company-use/6310
- CircuiTikZ manual: "no circle, no arrows, DON'T do it" for PMOS/NMOS. Lazy MOS is "preferred" by chip designers for huge circuits. Rounded caps help when lines are thick ("with very thick segments the drawing is quite harsh").
- xschem: issue #421 asks for symbol selection by shape, not bounding rectangle (an interaction point, not looks). https://github.com/StefanSchippers/xschem/issues/421. I found no public threads that criticise xschem's symbol appearance. Users mostly praise its Cadence-like feel. https://sscs-ose.github.io/analog/
- Falstad and LTspice: I found no specific citable praise in this pass **[gap]**. LTspice's look is minimal: thin lines, a 16-unit grid, labels right of the body, as measured above.

---

## 4. Readability at zoom

- Text: KLC uses 50 mil text against 100 mil pins, so **text is ½ of the pin pitch**, and 20 mil is the floor for cramped symbols. LTspice's default is about 42 mil on a 50 mil grid.
- One LOD implementation hides body text below about 7.2 screen pixels and secondary labels at a lower threshold. https://github.com/bdf1992/schematically/pull/165 (small project, low trust). A screen font is generally treated as unreadable below about 9 px/em (general typography; https://en.wikipedia.org/wiki/Font_hinting).
- Junction dots: "A dot means the conductors join … no dot means they cross". The risk is dots "drawn too small or accidentally omitted". Modern practice avoids 4-way junctions by staggering two T-junctions. https://www.allaboutcircuits.com/textbook/reference/chpt-9/wires-and-connections/
  - Size references: KiCad dot 36 mil ≈ 0.36 pitch; CircuiTikZ dot ≈ 0.08 × resistor length (≈ 0.11 cm at the default scale).
- Pin markers: KLC hides nothing and marks no pin ends in plots. KiCad, and xschem per general use, show an unconnected-pin marker (small square or circle) only on screen, not in print **[unsourced for xschem]**. CircuiTikZ separates `circ` (connection dot), `ocirc` (open terminal) and `squarepole`, and open poles are filled with the background so wires do not show through. Manual §Terminals.
- Line weight vs zoom: with lines a fixed fraction of the grid, a 10 mil line on a 100 mil pitch drops below 1 px when the pitch is under 10 px. **[unsourced]** Clamp stroke width to ≥1 device px and fade or hide text by LOD.

---

## Recommended conventions for Schemify

Let **G** be the pin pitch: every pin endpoint sits on G. All numbers are in G.

1. **One pin pitch G and integer-G pins.** Every pin endpoint lies on G (KLC S4.1, IEEE 315 modular grid). Body details may use a G/8 sub-grid (LTspice uses 16 sub-units per grid). Integer coordinates (D33) map onto that directly.
2. **Two-terminal footprint = 4G pin-to-pin, vertical by default.** That is LTspice's cap (4 grid) and close to KiCad's 3G, and it leaves 1G of lead at each end for a 2G body. CircuiTikZ's body is about 80% of the span. Pick 4G with a 2.5G body if labels need room.
3. **Stroke: wires 1 unit, device bodies 2× wire width** (CircuiTikZ `bipoles/thickness=2`). Body lines read as "the device", leads read as "the wire". Clamp to ≥1 device px at all zooms.
4. **Round joins and caps on every polyline.** CircuiTikZ documents the harsh zigzag joins and adds `rounded caps` for thick strokes. Round joins also hide 1-px seams at low zoom.
5. **MOSFET default = Razavi/Sedra IC style.**
   - Solid channel line.
   - Gate plate parallel to the channel, with a gap of about 0.25G.
   - Arrow on the **source** lead: out for N, in for P.
   - No PMOS bubble.
   - Bulk drawn as a 4th pin off the channel's back center, because the library's devices are nmos4/pmos4. Draw it short and in a lighter weight (1×), so a 4-terminal device still reads like the 3-terminal textbook figure.
   This combines what Sedra-Smith 5.11(b), Razavi (issue #934) and the Wikipedia IC note agree on, and avoids the CircuiTikZ "DON'T" case.
6. **MOSFET proportions** (from CircuiTikZ, rounded to G):
   - D to S: 4G (nmos height 1.1 against resistor 1.4 → about 0.8 of a resistor; LTspice uses 6 sub-grid).
   - Channel line: 2G.
   - Gate stub: 1G.
   - Gate pin centered on the channel (CircuiTikZ), not offset like LTspice.
   - Body width about 1.5–2G.
7. **Thick-oxide/IO variants = thicker gate line** (about 2× body stroke), Pretl/Eberlein convention, rather than a new shape. Variation by data (a style flag), not a new symbol.
8. **Optional "lazy MOS" view** (CircuiTikZ `lazymos`) as a display preference for dense digital-ish blocks. Same pins, different glyph.
9. **BJT: no envelope circle** (CircuiTikZ marks the circle as the discrete-package marker). Emitter arrow at the pin end. Same 4G C–E span as the MOSFET, so the two families align on mixed schematics.
10. **Filled, solid arrowheads, one size everywhere.** Length about 0.35G, width about 0.25G, a 40:30 ratio as in KiCad's Q_NMOS (40×30 mil on 100 mil). Put the arrow at the pin end (CircuiTikZ `arrow pos=end`, its author's preference) so it does not collide with the channel.
11. **Resistor: ANSI zigzag by default, IEC box as an alternate glyph** (KiCad's R/R_US resolution). Analog textbooks and US users expect the zigzag.
    - 3 full cycles (6 peaks), the IEEE-315 and CircuiTikZ default.
    - Body 2.5G long × 0.75G wide (CircuiTikZ ratio 0.8:0.3 ≈ 2.7:1).
    - Peaks on the G/8 sub-grid.
12. **Inductor: 3–4 semicircular humps** (KiCad 4 arcs) at the same body length as the resistor, so R/L swaps keep wiring.
13. **Capacitor: two straight plates**, plate length 1.5G, gap 0.4G (about 3:1 like CircuiTikZ 0.6:0.2), plates drawn **thicker than the body stroke** (KiCad draws plates at 2× body). That is the most recognizable cap cue at low zoom.
14. **parax_cap: same shape, dashed or half-weight, in a muted color.** It reads as "not a real device" **[unsourced]**. Use the curved plate only if a bottom-plate or polarity marker is wanted, with curved = negative/outer per IEEE 315.
15. **Sources: independent = circle of diameter 1.5G; controlled = diamond about 17% larger** (CircuiTikZ 0.6 vs 0.7).
    - Inside: +/− for V-type, a straight arrow for I-type (US textbook style).
    - The 4 controlled kinds differ only by inner mark and parameter text.
    - Control pins on vcvs/vccs (4-terminal) are short leads on the left with their own +/−.
16. **bsource: a diamond with the inner mark by output type, plus a small "B" or "f(·)" tag** **[unsourced]**, so behavioral sources stay in the dependent-source family visually.
17. **Switch / vswitch: an open blade at about 30° between two small open circles** (CircuiTikZ `ocirc` poles). vswitch adds two control leads, like the controlled sources.
18. **Ground = 3 shrinking bars on a 1G stem; vdd = bar (T) on a 1G stem**, labeled with the net name. Keep the triangle "signal ground" as an alternate. Ground always points down and supply up (KLC S4.2). The pin is the stem end, on G.
19. **Labels:** instance name and value/model stacked to the **right** of the body (LTspice WINDOW placement), name above and parameters below, left-aligned at body edge + 0.5G. Text height **0.5G** (KLC 50 mil on 100 mil), floor 0.3G for pin names. MOS W/L and nf go in the same column.
20. **Op-point annotation slots are part of the symbol.** Up to 5 parameter rows in that right-hand column (Virtuoso caps at the number of `cdsParam` labels, usually ≤5), and one terminal-voltage slot per pin, offset 0.25G off the lead (`cdsTerm`). Shown in a distinct color and dropped first by LOD.
21. **Unfilled bodies** for all primitives (KLC S3.3: distinctive-shape symbols are not filled). The background shows through and the symbols stay "puristic" (the Qucs praise).
22. **Junction dots:** diameter about 0.35G (KiCad 36 mil / 100 mil), drawn only where 3+ wire ends meet, never on a plain pin-to-wire end. Flag 4-way junctions in a lint, the staggered-T practice. Dot diameter has a pixel floor (≥4 px) so dots never vanish when zoomed out.
23. **Unconnected-pin marker:** a small open square of about 0.15G at a pin end with no wire, on screen only, never in export. Connected pins show nothing.
24. **LOD:**
    - Hide instance and parameter text when its height is under about 7 px.
    - Hide op-point text before names.
    - Below about 3 px/G, draw bodies only (no arrowheads, no dots).
    - Strokes never under 1 px.
25. **Consistency over novelty:** every symbol is built from the same primitives (stroke weights, arrowhead, dot, label column), and nothing is invented that a reader of Razavi or Sedra-Smith would not recognise. CircuitLab's invented MOSFET was abandoned. Settle proportions by rendering against a CircuiTikZ figure of the same circuit.
