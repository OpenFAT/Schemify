# What designers like and dislike in schematic, simulation and waveform UIs

Status: research, 2026-10-12. Evidence base for the `analog-eda-ux` skill (`.claude/skills/analog-eda-ux/`). Three parts:
1. commercial IC tools (Virtuoso, Custom Compiler, ADS, S-Edit);
2. open and board-level tools (KiCad, LTspice, Qucs-S, Altium, Micro-Cap, Flux);
3. waveform viewers plus canvas visual conventions.

Each part ends with its cross-tool patterns. Companion: [xschem-complaints.md](xschem-complaints.md).

D1: no xschem source was read. Reddit, edaboard and the Cadence forums blocked fetches, so claims from them come from search-engine extracts and are tagged `[snippet]` (paraphrased); `[unsourced]` marks inference.

---

## Part 1: What analog IC designers like and dislike about commercial schematic + simulation UIs

Research for Schemify UI design. Collected 2026-10-12. No xschem source was opened. The only xschem material used was user reviews, forums and interviews.

**How the sources were reached and how far to trust them.** Reddit (www/old/JSON) and the Pushshift mirror refused automated access. So did edaboard, which serves a Cloudflare challenge. community.cadence.com answered one plain request and then went behind Cloudflare. Hacker News came through the Algolia API, and deepchip, SourceForge, GitHub, Keysight and Microwaves101 loaded directly.
Tags used below:
- `[page]`: I read the page itself.
- `[snippet]`: the claim comes from the search engine's extract of that URL, because the page was blocked. It is a paraphrase, not a verbatim quote.
- `[vendor]`: vendor material. It shows what users complained about enough to get fixed, not what users said themselves.
- `[unsourced]`: my own inference.

Tanner S-Edit and Custom Compiler have little public user discussion. Most of the strong evidence is about Virtuoso.

---

### Cadence Virtuoso: Schematic Editor (L/XL), forms, assistants, canvas

#### Liked
- **Single-key bindkeys are the core of the workflow, and they carry over between tools.** Users of other tools describe them as the thing to keep. An xschem reviewer wrote: "If you're a Cadence user this schematic editor will feel pretty natural, right down to the bindkeys" (contrast: XCircuit "requires you to unlearn all of your mouse and keyboard habits"). [page] https://sourceforge.net/projects/xschem/reviews/
- **The canonical key set** that course handouts teach as muscle memory: `i` instance, `w` wire, `p` pin, `l` label, `q` properties, `e`/`Shift-E` descend read-only/edit, `Ctrl-E` return, `f` fit, `u`/`Shift-U` undo/redo, `x`/`Shift-X` save / check-and-save, `r` rotate, `Shift-R`/`Ctrl-R` mirror, Space auto-names nets. [page] https://analoghub.ie/category/cadenceTricks/article/cadenceTricksHotkeys
- **Power users add more keys rather than more toolbars.** One widely shared SKILL kit is entirely bindkeys: ALT+scroll changes wire thickness or label bussing, CTRL+arrow changes pin-name size, CTRL+F10 toggles the grid, and "edit multiple with ALT+SHIFT+Q **very useful**". [page] https://github.com/ananthchellappa/SKILL
- **The docked Property Editor assistant (IC6) fixes multi-select editing.** With several instances selected, `+` expands each property to show every instance's value. A category pulldown filters mixed selections. Grey means read-only, blue italic means default value, and bold orange means the values differ across the selection. Selection is cross-highlighted between the assistant and the canvas. [snippet] https://community.cadence.com/cadence_blogs_8/b/cic/posts/things-you-didn-t-know-about-virtuoso-editing-properties
- **The Navigator assistant answers a real pain point.** Cadence frames it as relief from "constantly zoom[ing] and pan[ning]" in big schematics. It shows a hierarchy tree, and "Open Instance" jumps straight to a deep cell without opening the levels in between. [snippet] https://community.cadence.com/cadence_blogs_8/b/cic/posts/things-you-didn-t-know-about-virtuoso-navigator-assistant
- **The IC6 refresh kept the look and feel.** Cadence's own pitch was "the same look and feel and similar use model" plus lockable assistants (Navigator, Property Editor, Search, Annotation Browser, World View), on-canvas editing, and multi-tab windows. [snippet] https://community.cadence.com/cadence_blogs_8/b/cic/posts/virtuoso-ic-5-1-41-was-good-but-virtuoso-ic-6-1-is-better
- **Users in the IC5 to IC6 transition era called IC6.1 "nicer looking and easier to use"**, but many stayed on IC5141 for PDK availability. [snippet] https://www.edaboard.com/threads/which-version-of-cadence-should-i-install-ic5141-or-ic61x.126616/
- **The IC6 context-sensitive right-click popups were valued.** A forum respondent warned against rebinding the right mouse button back to IC5 behaviour: "There are a lot of useful new context sensitive pop-ups that are implemented in 6.x. You will miss on these". [page] https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/17506/mousefunctionality-ic5-in-ic6
- **Every mouse button and drag is a rebindable bindkey** (`Btn1Down`…`Btn5Down`, `DrawThru1..3`). Users rely on this to restore old habits. [page] same URL
- **The dark canvas with dot grid is the reference look.** An HN commenter gave a new tool this advice: "get rid of the grid lines entirely and only have 'dots' at regular spacing. Here's what Cadence Virtuoso looks like". [page] https://news.ycombinator.com/item?id=47642323
- **Some praise the UX as above average.** "I personally found Cadence Virtuoso to have above average UX, but I do agree that the GUI looks slightly dated." [page] https://news.ycombinator.com/item?id=12950390

#### Disliked
- **Options are hidden behind F3, the option form for the current command.** A former Synopsys engineer prefers Custom Compiler because "most tool options are not hidden under F3 (as in Virtuoso) and show on the toolbar instead". [page] https://news.ycombinator.com/item?id=39171500
- **The modal "q" form gets in the way.** Cadence's own blog concedes that the traditional select-then-`q` flow means "wearing out the 'q' key on your keyboard quite fast". The forms are large and cover the schematic. Multi-instance editing in the form ("Apply to all selected instances of same master") is described as "somewhat intimidating". [snippet] https://community.cadence.com/cadence_blogs_8/b/cic/posts/things-you-didn-t-know-about-virtuoso-editing-properties
- **Property editing was the reason xschem was written.** Stefan Schippers: "It was also extremely difficult to edit schematic properties in systems like cadence which is another reason I started xschem." [page] https://www.openedhardware.com/p/xschem-and-the-chip-revolution
- **The IC6 default grid is "blinding".** The SKILL kit disables it to "make it similar to IC5". [page] https://github.com/ananthchellappa/SKILL
- **The grid is too loud by default.** Forum threads ask how to dim major dots and shrink minor dots. Users report the schematic is "much calmer to look at" with smaller minor dots, and Cadence added `dotStyleMajorGrid` to dim major grid dots. [snippet] https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/62538/schematic-editor-minor-grid-dot-size , https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/27251/dim-major-dots-in-schematic
- **Changing the background colour needed a resource-file editor.** It was done in display.drf through the Display Resource Manager until Options→User Preferences gained it in an IC617 hotfix. People do ask for a white background. [snippet] https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/40138/how-to-change-background-color-of-schematic-editor-in-cadence-virtuoso
- **ALT/CTRL menu mnemonics steal keys.** The SKILL kit calls disabling them a **MUST**: "else, you couldn't use ALT-x for what **you** want but for what Cadence **thinks** you should want". [page] https://github.com/ananthchellappa/SKILL
- **The IC6 mouse remap was disruptive.** IC6 moved the right-button and middle-button actions (IC5 used RMB to rotate during placement and MMB for a small popup). Users asked how to get the old behaviour back. [page] https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/17506/mousefunctionality-ic5-in-ic6
- **The Motif-to-Qt port broke form layout.** Buttons moved from the top of forms to the bottom, and some custom forms rendered shifted or hidden. [snippet] https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/19003/ic5---ic6-gui-forms-comapatibility
- **"I hate the palette but it's just me"**: an edaboard comment on IC615's dockable palette. [snippet] https://www.edaboard.com/threads/opinion-about-cadence-virtuoso-615-and-isr.228273/
- **The tool is slow and heavy.** Schippers: "Cadence was a very big program that needed endless time to start… in cadence even a small amplifier with let's say, 20 transistors, took 30 seconds to get a netlist." Also: crashes "due to memory exhaust because the program kept growing." [page] https://www.openedhardware.com/p/xschem-and-the-chip-revolution
- **New windows sometimes open behind other windows.** New windows and forms can appear behind the Library Manager or start iconified (a Cadence support note). [snippet] https://support1.cadence.com/public/docs/content/11612426.html
- **The UI is widely called dated.** "EDA software like Cadence Virtuoso and ICCAP have had crappy UIs for decades." [page] https://news.ycombinator.com/item?id=38498051. Also: "Synopsys/Cadence tools are severely outdated and terrible. They need to be rewritten from scratch and designed by actual UX/UI designers for the front end." [page] https://news.ycombinator.com/item?id=11002123
- **Designers stay with Virtuoso out of habit.** Analog engineers stay because they "already absorbed Virtuoso's quirks", not because it is better (HN thread replies). [page] https://news.ycombinator.com/item?id=39171500

### Cadence Virtuoso: DC operating-point back-annotation

#### Liked
- **Op-point values sit on the schematic next to each device.** The parameters are configurable: gm, gds, vdsat, region, or expressions like `vgs-vth`. They are edited in the Annotation Setup form with a dropdown per slot. [snippet] https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/47275/adding-parameters-for-annotating-additional-dc-operating-points
- **Info balloons on hover hold extra parameters.** A designer liked "Anchored at cursor position", liked being able to choose extra balloon parameters, and keeps several annotation sets that they switch between with keybindings. [page] https://blog.wangxm.com/2025/05/virtuoso-annotation/

#### Disliked
- **The number of annotated values is limited by the symbol's `cdsParam` label slots**, typically about 5–6. More values need symbol edits, a CDF `opPointLabelSet`, or balloons. [snippet] https://community.cadence.com/cadence_technology_forums/f/rf-design/20419/showing-more-number-of-oppointlabelset-in-cdf-for-dc-op-point-annotation ; [page] https://blog.wangxm.com/2025/05/virtuoso-annotation/
- **Custom annotation setups don't persist easily.** "customizing DC Operating Points annotation permanently" and "how to change default annotation setup" are recurring forum questions. [snippet] https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/52209/cadence-schematic---customizing-dc-operating-points-annotation-permanently , https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/28735/ade-how-to-change-default-annotation-setup
- **The default balloon placement is distracting.** "a bit distracted when the balloon does not show around the component". Finding where op-point parameters are configured (per cell, not global) was the first hurdle. [page] https://blog.wangxm.com/2025/05/virtuoso-annotation/
- **A DC result annotation once vanished while panning with the arrow keys.** Tanner fixed this bug in S-Edit. It shows that annotations must survive navigation. [vendor] https://www.eda-solutions.com/app/uploads/2023/07/tanner_rh_2023.2.pdf

### Cadence Virtuoso: ADE L / XL / Explorer / Assembler

#### Liked
- **ADE stays the default way to drive SPICE.** "Cadence ADE continues to be our 'go to' tool for our analog designers to drive SPICE simulation." [page] https://www.deepchip.com/items/0560-01.html
- **Merging L/XL into Explorer/Assembler was welcome.** It fixed a split that users disliked: "ADE-XL really felt like a separate tool for what is a natural task that belongs all together." [page] https://www.deepchip.com/items/0560-01.html
- **Sweeping any design variable is something users miss elsewhere.** "Cadence virtuoso has spoiled me": an xschem/ngspice user who wanted DC sweeps over a parameter rather than a source. [page] https://sourceforge.net/p/ngspice/discussion/133842/thread/588cafa7fe/
- **Spreadsheet-style outputs, corners and results tables get steady usability work, most of it filter-driven.** Users can choose which columns show in Outputs Setup. Filters with range, all/any-word and prefix/suffix matching appear on the Corners, Results, Outputs, Data View and Setup assistants. Corners stay in sync between the form and Data View (`autoCornerUpdate`), and results export pivot-ready to Excel. [vendor/snippet] https://community.cadence.com/cadence_blogs_8/b/cic/posts/virtuosity-more-usability-enhancements , https://community.cadence.com/cadence_blogs_8/b/cic/posts/virtuosity-advanced-filtering
- **Plotting templates** save a graph-window layout and reapply it to new results. [vendor/snippet] https://community.cadence.com/cadence_blogs_8/b/cic/posts/virtuoso-ic6-1-8-viva-plotting-templates
- **Direct Plot means clicking nets or terminals on the schematic to plot them**, for example Function=Voltage, Select=Net. This click-to-probe flow is taught in every course. [snippet] https://www.vtvt.ece.vt.edu/tutorial/tutorialmixedsignal_ElanSim.php

#### Disliked
- **The L/XL/GXL split** (see above). Users were sceptical of the 2016 repackaging as an "'easier to use' flow". [page] https://www.deepchip.com/items/0560-01.html
- **Migration friction.** Old ADE L states still open in ADE L by default, so users need an env var or Import. Recurring threads ask "how to identify an old ADE-L vs Explorer session". [snippet] https://community.cadence.com/cadence_blogs_8/b/cic/posts/virtuosity-opening-old-ade-states-and-views-with-explorer-assembler , https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/58584/how-to-identify-if-an-ade-session-is-an-old-ade-l-or-explorer-assembler
- **The ADE window didn't resize its contents.** Earlier versions left blank space when resized, and this was fixed later. [snippet] (search extract for https://community.cadence.com/cadence_blogs_8/b/cic/posts/things-you-didn-t-know-about-virtuoso-ade)

### Cadence ViVA (waveform viewer + calculator)

#### Liked
- **Strip mode, where each trace has its own strip on a shared X axis, plus drag-and-drop of traces between strips, subwindows and windows.** [snippet] https://community.cadence.com/cadence_blogs_8/b/cic/posts/things-you-didn-t-know-about-virtuoso-viva-viva
- **Markers are keyboard-driven:** `m` point, `h`/`v` horizontal/vertical, `d` delta, `t` rise/fall, `c` trace cursor, `[`/`]` zoom, `f` fit, `q` properties. A commenter called delta markers "very useful". [page] https://blog.asicedu.com/2020/11/cadence-viva-commonly-used-keybindings.html
- **Delta markers show dx, dy and slope between two points**, on the same trace or on different traces. [snippet] https://community.cadence.com/cadence_blogs_8/b/cic/posts/things-you-didn-t-know-about-virtuoso-delta-markers-in-viva

#### Disliked
- **Plotting does not scale.** Plotting all outputs into 24 subwindows "takes more than 13 minutes". ViVA and Assembler freeze, and dragged windows paint only partially. Another user reports DC-sweep plots taking over an hour with 4,543 signals in a 433 MB database. [snippet] https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/52733/viva-slow-plotting-speed , https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/66311/cadence-extremely-slow-plot-for-dc-sweeps/1409279
- **Markers are "all about bindkeys"**: for delta markers you "have to know just where and in what combination" to press keys, so they are hard to discover. [snippet] https://community.cadence.com/cadence_blogs_8/b/cic/posts/things-you-didn-t-know-about-virtuoso-delta-markers-in-viva
- **The calculator's RPN stack and buffer model is "somewhat daunting"**, according to Cadence's own blog. Building expressions needs Push/Pop and "Pick selected item and put into buffer". [snippet] https://community.cadence.com/cadence_blogs_8/b/cic/posts/things-you-didn-t-know-about-virtuoso-viva-part-4-and-final
- **Viewer settings don't stick.** Forum threads ask how to make ViVA remember its waveform setup, defaults and background colour, which changed between IC6.1.7 and 6.1.8. [snippet] https://community.cadence.com/cadence_technology_forums/f/38/t/30828 , https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/45423/defining-viva-background-color-changed-from-ic6-1-7-to-6-1-8

### Synopsys Custom Compiler + PrimeWave + (Custom) WaveView

#### Liked
- **Schematic entry quality-of-life:** "Probing is more efficient, adding stubs to instances as well, lot of QoL stuff like these dynamic alignment guides, and finally most tool options … show on the toolbar instead". Also, "keybinds are 95% the same" as Virtuoso. [page] https://news.ycombinator.com/item?id=39171500
- **"I enjoyed Custom Designer better than Cadence Virtuoso as it was less confusing"**, from a Big-A/Little-D user on Custom Designer, CC's predecessor. [page] https://www.deepchip.com/items/0547-08.html
- **Vendor-claimed schematic features:** on-canvas parameter editing, automatic wiring, cross-probing and back-annotation in both schematic and text views, and a hierarchical net tracer. [vendor/snippet] https://www.synopsys.com/content/dam/synopsys/implementation&signoff/datasheets/custom-compiler-ds.pdf
- **WaveView's selling point is speed on huge files:** "fast loading, display scrolling, and zooming of very large waveform files". [vendor/snippet] https://www.synopsys.com/content/dam/synopsys/verification/datasheets/Custom-WaveView-ds.pdf
- **People wire WaveView into Virtuoso.** A community tool adds "Send to WV (Direct)" to Virtuoso's right-click menu, so some Virtuoso users would rather plot in WaveView. [page] https://github.com/DoYourLLM/wv_cds

#### Disliked
- **The tool works but doesn't inspire.** "Custom Compiler is good enough for anything you need to do... but you won't be exited [sic] by the tool." [page] https://www.deepchip.com/items/0571-01.html
- **Its strengths don't overcome habit.** Few people use it. "Barely anyone uses Synopsys Custom Compiler" despite better schematic QoL, and switching means relearning. [page] https://news.ycombinator.com/item?id=39171500
- **Synopsys's own engineers bypassed PrimeWave.** They "did not dogfood it and used custom text-based DSLs for all simulations instead". [page] https://news.ycombinator.com/item?id=39171500
- **Graphics-heavy operations slow down over a remote host** (WaveView). [snippet] search extract for https://www.synopsys.com/content/dam/synopsys/verification/datasheets/custom-waveview-adv-ds.pdf

### Keysight ADS (schematic, Data Display, tuning)

#### Liked
- **Tuning with sliders updates results without a full manual re-run.** Successive tuning trials can be overlaid as traces in the Data Display, which helps find the most sensitive parameters. [vendor/snippet] https://edadownload.software.keysight.com/eedl/ads/2011_01/pdf/optstat.pdf
- **The results display is considered ADS's strength:** "ADS has a better user interface for output results that includes both textual and graphic output". The Data Display combines plots, Smith charts, marker readouts and equations on one page. [snippet] https://www.edaboard.com/threads/ads-v-s-ansoft-v-s-mwo.149468/
- **A docked Property Editor and Info panel became the default in ADS 2024**, and the Edit Component Parameters dialog gained multi-component editing. Vendors are converging on a docked inspector plus multi-select. [vendor/page] https://docs.keysight.com/display/engdocads/ADS+2024+Release+Notes

#### Disliked
- **"the interface of ADS was found to be the least user-friendly of the software packages and would, therefore, require plenty of training and practice"** (ARMMS tool comparison). [page] https://www.armms.org/media/uploads/1335467278.pdf
- **"suffers from a very outdated GUI / file / library system"** and **"ADS just gets in the way of my creativity and I feel more like a programmer than a designer"**, from Microwaves101 forum user Scion. [page] https://forum.microwaves101.com/discussion/102/eda-software-recommendations
- **Users who move to Microwave Office don't come back.** "I was forced to switch from ADS, nine years ago, now I don't want to go back". In MWO, "the load pull tools are a dream to use". [page] same URL
- **Data Display equations are post-processing only**, so the optimizer cannot use them. Users have to duplicate the same expression as MeasEqn on the schematic. [snippet] https://www.edaboard.com/threads/keysight-ads-how-do-i-optimize-for-an-expression-in-data-view.410185/

### Siemens Tanner S-Edit

Little public user opinion exists. Most of what follows is vendor material.

#### Liked
- **"easy to use", with an "intuitive and quick learning curve".** Toolbars and navigators dock or float, and connectivity editing rubber-bands with snap-to-pin. [vendor] https://www.allaboutcircuits.com/uploads/articles/s-edit-ds_compressed.pdf
- **Cheaper and easier than Cadence**, "not bad at all for small medium analog design", according to edaboard users (mostly about L-Edit). [snippet] https://www.edaboard.com/threads/cadence-vs-tanner-l-edit.38556/
- **Recent requested features:** back-annotation of voltages, currents and op-point, Schematic Compare from the Library Manager, richer simulation-status tooltips, and up to 10× faster library browsing on network drives. [vendor/page] https://www.eda-solutions.com/app/uploads/2023/07/tanner_rh_2023.2.pdf

#### Disliked
- **"almost zero support in the Internet"**: the user community is thin. [snippet] https://www.edaboard.com/threads/ic-design-software-tools-other-than-tanner-eda.127795/
- **The simulation toolbar was split in two** to fit a second simulation environment (SDE), which crowds the toolbars. [vendor/page] https://www.eda-solutions.com/app/uploads/2023/07/tanner_rh_2023.2.pdf

### What designers miss when they move to open-source tools

- **Waveform viewing is the weakest link.** xschem's built-in graphs "does not seems to be very reactive", and the UI/UX of GAW and GWAVE is "well terrible". [page] https://gitlab.com/surfer-project/surfer/-/issues/391
- **There is no ADE-equivalent GUI for corners and Monte Carlo.** People fill the gap with frameworks (CACE) or home-made Tkinter front ends. [snippet] https://github.com/moduhub/analog-designer-core , https://wiki.f-si.org/index.php?title=CACE%3A_Defining_an_open-source_analog_and_mixed-signal_design_flow
- **Variable sweeps** ("Cadence virtuoso has spoiled me"). [page] https://sourceforge.net/p/ngspice/discussion/133842/thread/588cafa7fe/
- **Connect-by-abutment ("connect-by-kissing") and wire snap-to-pin** are reported missing by Cadence users in xschem discussions. [snippet] search extract over https://sourceforge.net/p/xschem/discussion/general/
- **What they gain is speed and a quiet UI.** "Xschem slowed me down not one bit, from a cold start, and lets me do what I need to do without a lot of distractions"; "the flexibility and speed of interface/graphic are incredible". [page] https://sourceforge.net/projects/xschem/reviews/

---

### Cross-tool patterns (what designers consistently value)

1. **Muscle memory beats discoverability.** Users praise single-letter bindkeys (`i w p l q e x f u r`), copy them across vendors (CC is "95% the same", and xschem is liked "right down to the bindkeys"), and treat breaking them as the biggest migration cost. Ship Virtuoso-compatible defaults and make every key and mouse button rebindable.
2. **Keyboard first, but options must be visible.** The top complaint about Virtuoso is options hidden behind F3. Custom Compiler's visible tool options and alignment guides are what users praise. Show command options in a passive inline bar, not a form.
3. **Property editing should be a non-modal, docked, multi-select table.** The modal `q` form is the most-cited annoyance, and Cadence's own blog says so. The Property Editor assistant, ADS 2024's docked inspector and its multi-component edit all moved the same way. Show differing values across the selection explicitly (Virtuoso's bold orange) and distinguish default from overridden values (blue italic).
4. **Speed is a UI feature.** Start-up time, time to netlist ("30 seconds" for 20 transistors) and plot time (13 minutes, over an hour) drive more resentment than how the UI looks. The xschem reviews praise cold-start speed and responsiveness above everything else.
5. **The canvas should be calm.** A dark background with a sparse, dim dot grid is the reference. Bright or line grids get called "blinding", users shrink minor dots and dim major ones, and HN advises "only dots". Make grid intensity and background user preferences, not resource-file edits.
6. **Op-point back-annotation is a signature feature, but it needs room.** Designers want gm/gds/vdsat/region/expressions next to each device, a richer hover balloon anchored at the device, switchable annotation sets on a key, and settings that persist. Fixed label slots (about 5–6) are the main limit.
7. **Click-to-probe from schematic to waveform** (Direct Plot, CC's "more efficient probing", right-click "Send to WV") is core to the loop. It has to be one click from net to trace.
8. **Waveform viewers live or die on speed with large data**, then on keyboard markers (point, delta with dx/dy/slope, horizontal/vertical) and strips on a shared X axis. RPN calculators and key combinations you can't discover draw complaints.
9. **Keep the simulation setup in one place.** Splitting it into separate tools (ADE L vs XL) was disliked, and the merge was welcomed. Setup is a set of spreadsheet-like tables (outputs, corners, results) that need filtering, column choice and export.
10. **Modernize without moving things.** The IC6 refresh was accepted because it kept the look and feel. The friction came from changes that broke habits: mouse buttons remapped, form buttons moved, ALT/CTRL taken by menu mnemonics. Users welcomed docking assistants and right-click context menus, while the palette and the new grid drew complaints.
11. **Settings must persist, in text form.** Users repeatedly ask how to make annotation, plot and viewer settings stick, and they share dotfiles (`.cdsinit`/`.cdsenv`) as the way to customize. Plain-text, versionable preferences are expected.
12. **Hierarchy navigation should be cheap.** Descend and return on single keys (`e`, `Shift-E`, `Ctrl-E`), a tree navigator that jumps straight to a deep instance, and tabs or bookmarks.
13. **Modal and floating windows that cover the canvas are a recurring irritant.** Forms cover the schematic and windows open behind each other. Docked panels are preferred. [inferred from the q-form, assistant and window-ordering items above]
14. **Being called "dated" doesn't mean users want a redesign.** HN calls these UIs "crappy" and "dated", yet experienced users rank them above average, and designers stay because they have learned the quirks. Polishing the look doesn't help much. Speed, density and keys do. [inference from the sources above]
15. **Power users script the UI.** SKILL bindkey kits, Python in ADS 2025 and Tcl in S-Edit all exist because the defaults never fit everyone. A scripting/plugin layer that can bind keys and add context-menu items is expected, not a luxury.
16. **Results and design should stay connected.** ADS users resent equations that live only in the Data Display and can't be optimized, and Cadence users want plotting templates reapplied to new runs. Expressions should be defined once and reused for plotting, measurement and optimization.

---

## Part 2: What circuit designers like and dislike in schematic/SPICE tool UIs

Research for Schemify UI design. Retrieved 2026-10-12. Sources are forums, HN, vendor docs and reviews.
Quotes are short excerpts or close paraphrases from the fetched pages. Claims with no source are marked [unsourced].
Access notes: reddit.com and groups.io (LTspice group) returned no fetchable content (groups.io answered HTTP 402).
For LTspice 24 the groups.io and EngineerZone text comes from search-result snippets. The EngineerZone threads timed out on fetch.

---

### KiCad Eeschema (v6 to v10)

#### Liked
- **v6 unified the editing model across editors.** The schematic editor got "the same object selection and manipulation paradigm as the PCB editor". Visual language, hotkeys, dialogs and workflows were harmonized, so it "no longer feels like using two different tools". https://hackaday.com/2022/01/18/kicad-6-0-what-made-it-and-what-didnt/ ; https://www.kicad.org/blog/2021/12/KiCad-6.0.0-Release/
- **HN called v6 KiCad's "Blender moment".** v6 was "supposed to focus on UX" like "what blender did", and v5 felt "very non-ergonomic". https://news.ycombinator.com/item?id=29018474
- **Defenders of v6 select-then-act.** A dev said things "went the right direction, more in line with common software". Another user: "sooo much goodness in the Accelerated canvas I don't ever consider stepping back". https://forum.kicad.info/t/fighting-with-v6-0-ui/37534
- **Directional box selection.** Readers liked learning that dragging left-to-right selects only contained items, while right-to-left selects anything touched. https://hackaday.com/2022/01/18/kicad-6-0-what-made-it-and-what-didnt/
- **Selection filter.** Rob Miles calls it "the work of a genius", evidence KiCad was "built by engineers for engineers". Right-clicking an object type sets the filter to that type only. https://www.robmiles.com/journal/2023/4/28/the-kicad-selection-filter-is-awesome ; https://docs.kicad.org/7.0/en/eeschema/eeschema.html
- **Net navigator (v7).** A docked panel lists every occurrence of the highlighted net (labels, pins, sheet pins), grouped by sheet. Clicking an entry jumps the canvas to it. https://docs.kicad.org/7.0/en/eeschema/eeschema.html
- **Properties panel (v8).** Edits apply immediately, and a mixed selection shows only the shared properties. The panel reached parity with PCB in v8 and was praised in the release reviews. https://docs.kicad.org/8.0/en/eeschema/eeschema.html ; https://techexplorations.com/blog/kicad/kicad-8-new-and-updated-features-review/
- **Mnemonic single-letter hotkeys.** A M W V L (add, move, wire, value, label). Advice: "left hand on shortcuts, right on mouse". The keys are "easy to remember because each hotkey matches the action name", and the common ones sit on the left of the keyboard. https://www.eevblog.com/forum/eda/drawing-schematics-in-kicad-eagle-is-so-frustrating-((-(howtogetstarted)/ ; https://www.build-electronic-circuits.com/kicad-vs-eagle-2018-comparison/
- **Hover + G drag.** Hovering a wire and pressing G drags it, which "leaves your left mouse button finger free". https://forum.kicad.info/t/strange-behaviour-dragging-wire-segment/52163
- **Simulator tuning slider (v8).** Pick a component, get a slider, and the circuit re-simulates immediately. Called "excellent for optimizing filter cutoffs". https://pcbsync.com/kicad-spice-simulation/
- **Live drag feedback (v10).** Junctions update live while dragging, a warning appears when a drag would short two nets, and unconnected crossings can draw as hop-over arcs. https://www.cnx-software.com/2026/03/22/kicad-10-release-dark-mode-graphical-drc-rule-editor-new-file-importers-and-more/
- **Dark mode (v10).** Users were enthusiastic that Windows dark mode finally works. https://alternativeto.net/news/2026/3/kicad-10-released-with-track-tuning-revamp-windows-dark-mode-design-variants-and-more
- **Community colour themes.** There are many JSON colour themes (solarized, nord, monokai, dracula) that drop into a folder. https://github.com/pointhi/kicad-color-schemes ; https://github.com/dracula/kicad

#### Disliked
- **Select-then-act broke muscle memory.** "V5 used to be: 'Point and use' / V6 is: 'Point, click to select, then use'". https://forum.kicad.info/t/fighting-with-v6-0-ui/37534
- **Esc to leave every tool.** "having to hit ESC after every tool means lifting my hand from the mouse". https://forum.kicad.info/t/fighting-with-v6-0-ui/37534
- **v6 looked worse to some.** One user called it "crude...almost cartoon like" next to v5. https://forum.kicad.info/t/fighting-with-v6-0-ui/37534
- **Wire hotkeys lost the keyboard-only path.** In v6 "it's impossible to have one hotkey that both starts and stops the wire", and there is "no hotkey to articulate the wire mid-draw". https://forum.kicad.info/t/restoring-schematic-wiring-hotkeys-in-6-0/33144
- **Hotkeys do not migrate between versions,** because conflicts can't be resolved automatically. https://forum.kicad.info/t/hotkeys-not-migrating-from-5-99-to-6-0/32149
- **Hotkey editor limits.** Alt+number can't be bound. A key can't be shared between the symbol editor and the schematic editor even when the contexts can't collide. Non-ASCII layouts and dead keys break. https://forum.kicad.info/t/setting-up-complex-hotkeys-is-a-nightmare-half-guide-half-rant/49109
- **Sticky auto-connect on drag.** A drag "some i/o's got grounded". A dragged segment that ends on another net merges with it, a "heated discussion" topic. https://forum.kicad.info/t/fighting-with-v6-0-ui/37534 ; https://forum.kicad.info/t/strange-behaviour-dragging-wire-segment/52163
- **Wires don't rubber-band (pre-v7).** "As soon as I drag a part somewhere, I need to redraw everything." https://www.eevblog.com/forum/eda/drawing-schematics-in-kicad-eagle-is-so-frustrating-((-(howtogetstarted)/
- **Weak selection feedback.** Clicking components gives poor confirmation of what is selected. https://www.eevblog.com/forum/eda/drawing-schematics-in-kicad-eagle-is-so-frustrating-((-(howtogetstarted)/
- **v10 grouped net tools.** The label/net tools became one flyout icon ("you have to select"). Wire mode defaults to 45° ("pretty annoying"). Copy/paste carries reference designators by default. https://www.eevblog.com/forum/kicad/kicad-v10-0-0-released/
- **Panel toggles removed from the toolbar (v9 nightly).** This hurt small-screen users who "often switch between hiding/showing all side panels" to get a big canvas. A dev called it a mistake and restored the buttons. Configurable toolbars were proposed. https://forum.kicad.info/t/accessing-left-side-panels-icons-removed/55180
- **Icon size vs screen height.** At 150% scaling the toolbar icons render at 48 px, and tools fall off the bottom of the vertical toolbar. v10 shortened the toolbars and cut the padding. https://forum.kicad.info/t/missing-text-and-line-tools/66233
- **v10 theme forced to follow the OS.** "I can't get Kicad back to 'light mode' without reverting this for either all apps or the whole system". Users asked for Light/Dark/System. https://forum.kicad.info/t/independent-light-dark-mode-toggle-in-kicad-10/67796
- **Default colours strain the eyes.** "Default color scheme burn my eyes", and reds stay hard to read even on black. https://forum.kicad.info/t/default-color-scheme-burn-my-eyes-a-lot-and-idea-color-setting-to-share/5444
- **Simulator UI is a thin layer.** "Kicad UI for simulations seems to limit possibilities of ngspice". ngspice errors are "written in Klingon". The setup is "extremely steep". https://forum.kicad.info/t/who-is-actually-using-kicad-for-spice-simulation/42210
- **Properties panel shipped half-done.** It stayed hidden through v7 because using it risked breaking designs. https://forum.kicad.info/t/solved-pcbnew-6-99-cannot-see-properties-panel/37751

---

### LTspice (XVII and 24)

#### Liked
- **Fast once learned.** "I can capture a schematic faster in LTSpice than in any other tool." "It's not beautiful, but it is very productive." https://news.ycombinator.com/item?id=31664226
- **Internally consistent.** It is "arcane" but internally consistent, so it is learnable. https://news.ycombinator.com/item?id=33992771
- **Wire tool.** Even a harsh critic said "The wire drawing tool is really nice though". https://news.ycombinator.com/item?id=22011994
- **Probe-to-plot.** After a run, click a wire to plot its voltage, click a device for its current, and Alt-click a device for its power. The cursor changes shape (voltmeter probe, clamp-on ammeter, thermometer) to show what the click will do. https://www.allaboutcircuits.com/technical-articles/basic-circuit-simulation-with-ltspice/ ; https://www.eevblog.com/forum/chat/ltspice-need-to-see-currents-when-clicking-nodes/
- **Probing multi-pin devices.** Hovering a pin of a multi-pin device switches the icon so you can probe that one pin's current. https://www.eevblog.com/forum/chat/ltspice-need-to-see-currents-when-clicking-nodes/
- **Waveform pane interactions.** Drag-zoom. Drag a label or part from the schematic into the plot. Alt-click a trace name for a draggable cursor. Ctrl-click a trace name for RMS/average. Right-click a trace to edit it as an expression (e.g. V(in)/I(in)). https://www.eecg.utoronto.ca/~ali/spice/LTspice/LTSpiceShortGuide.pdf ; https://ltwiki.org/LTspiceHelpXVII/LTspiceHelp/html/Viewer_Overview.htm
- **.op values on the schematic.** "Place .op Data Label" on a node shows its value. The label text is an expression (default `$` = node voltage) that can be edited to a current or power. https://ltwiki.org/index.php?title=Undocumented_LTspice
- **Two-key core vocabulary.** F2 part, F3 wire, Ctrl+R rotate, Ctrl+E mirror. These are taught on one-page cheat sheets. https://web.mit.edu/6.101/www/s2020/handouts/LTspice_shortcuts.pdf
- **LTspice 24 kept an escape hatch.** "Restore LTspice Classic Values" restores the old shortcuts. Toolbar Style can be set to Legacy, and the icon size is configurable. https://ez.analog.com/design-tools-and-calculators/ltspice/f/q-a/581013/ui-ltspice24-is-horrible

#### Disliked
- **Mode system for move and rotate.** "you fall into this strange mode system". Its "prehistoric interface paradigm" makes your existing software knowledge "counterproductive". https://news.ycombinator.com/item?id=22011994
- **Unconventional keys.** Move is on F7, and "The key shortcuts in ltspice are hilarious". It is "very painful to use after any kind of time off". https://news.ycombinator.com/item?id=33992771 ; https://news.ycombinator.com/item?id=31664226
- **Models via text directives.** "Including models is done by writing arcane text commands on the diagram". "The parameter/settings windows are cryptic". "all the underlying complexity of Spice exposed". https://news.ycombinator.com/item?id=22011994
- **Pin snapping on third-party symbols.** "Finickity pin alignment on custom/3rd party components sometimes leads to open circuits". https://news.ycombinator.com/item?id=22011994
- **No live annotation, plus a sign problem.** By default "LTspice does not display voltages and currents on the schematic" (unlike Tina and Falstad). The current arrow is "often...the opposite direction of what you want". https://www.eevblog.com/forum/chat/ltspice-need-to-see-currents-when-clicking-nodes/
- **1M means milli.** Suffixes are case-insensitive, so `1M` is 1 milli-ohm. You must write `1Meg`. This is "the multiplier that gives everyone trouble". https://ltwiki.org/LTspiceHelp/LTspiceHelp/A_General_Structure_and_Conventions.htm ; https://www.experimentalistsanonymous.com/ve3wwg/doku.php?id=ltspice_dirty_secrets
- **LTspice 24 icons.** Icons and probes were "overblown and chunky--like something from 1995". The probe cursors were "particularly awful" and "too big to place accurately". https://groups.io/g/LTspice/topic/ltspice_24_pre_release_now/103804593 (via search snippet)
- **LTspice 24 overall.** "the new UI is HORRIBLE"; "The icons are NOT intuitive, the lines are fat and difficult to see detail". The bolder icons "look bad in my 1080p screen". https://ez.analog.com/design-tools-and-calculators/ltspice/f/q-a/581013/ui-ltspice24-is-horrible ; https://ez.analog.com/design-tools-and-calculators/ltspice/f/q-a/581137/ltspice-24-offers-a-downgraded-user-experience-where-i-can-find-the-previous-versions
- **LTspice 24 changed the defaults.** The background, the cursors and the default shortcuts all changed at once. ADI pitched the new keys as "easier to remember". https://ez.analog.com/design-tools-and-calculators/ltspice/a/faqs-docs/FD559/what-s-new-in-ltspice-24 ; https://forum.digikey.com/t/tips-tricks-from-adi-for-ltspice-24-features-new-easier-to-remember-keyboard-shortcuts/46287
- **Viewer and measurements.** "LTSpice's [waveform viewer] sucks and has a few annoying quirks and bugs". Measurements only work as `.meas` directives. https://www.eevblog.com/forum/projects/micro-cap-12-(circuit-simulation)-vs-ltspice/

---

### Qucs / Qucs-S

#### Liked
- **Simple GUI.** Described as "really simple and user-friendly (very similar to that of LTSpice)", even "better designed than that of other paid tools". https://electroagenda.com/en/a-review-of-qucsstudio/
- **Tuning mode.** Sliders on component values update results in real time, instead of a sweep you set up first. https://qucs-s-help.readthedocs.io/en/latest/overview/simulation-types/tuning-mode.html
- **On the schematic sheet.** Simulation commands and result diagrams live on the schematic page itself [unsourced as a "liked" claim; the docs describe the design]. https://qucs-s-help.readthedocs.io/en/legacy/BasSim.html

#### Disliked
- **Fragmented forks.** Qucs, QucsStudio and Qucs-S share "not a lot of interoperability". One user: "I can see no reason for this tool except to confuse us". https://hackaday.com/2022/08/04/30-free-circuit-simulators-lightly-reviewed/ ; https://sourceforge.net/projects/qucs/reviews/
- **Bare crash messages.** Users get "ERROR: Simulator crashed!" with nothing more. https://sourceforge.net/p/qucs/discussion/311049/thread/2c1fd93aaf/
- **Slow editing in old Qucs.** Wire drawing and component moves were slow, which drove a GraphicsView rewrite. Qucs-S later added arbitrary-angle wires and a redesigned properties dialog. https://github.com/Qucs/qucs/issues/238 ; https://github.com/ra3xdh/qucs_s/releases

---

### Altium Designer (schematic)

#### Liked
- **One integrated tool.** "the best package in the way that Schematic and PCB layout are both integrated into the same tool". "way ahead in both the integration and user friendliness". https://www.eevblog.com/forum/eda/orcad-compared-to-others/
- **Find Similar Objects + Inspector.** Right-click an object, then "find similar" to select by matching properties. The panel then edits every selected object at once (including mixed types), and supports delta and expression edits. This is the bulk-edit workflow power users cite. https://www.altium.com/documentation/altium-designer/find-similar-objects-tools ; https://www.altium.com/documentation/altium-designer/editing-multiple-design-objects
- **Dark theme (AD18).** "much better for the eyes"; "a lot less eye straining". https://www.eevblog.com/forum/altium/should-i-upgrade-to-ad18/
- **Panels on wide screens.** One user liked properties on a side panel with a super-wide monitor. Overall verdict: "take some time to get used to ... but overall I think it is better". https://www.eevblog.com/forum/altium/should-i-upgrade-to-ad18/
- **Look and feel.** "the look and feel of the Altium Designer are the best"; "schematic design ... is easy to learn". https://g2.com/compare/altium-altium-designer-vs-kicad-eda

#### Disliked
- **Panel cramped vs grouped dialogs.** The AD18 panel replaced the modal dialogs and feels "very cramped". Users "cannot navigate the properties list as quickly as the old dialog boxes (which had things fairly intelligently grouped)". https://www.eevblog.com/forum/altium/should-i-upgrade-to-ad18/
- **Double-click goes to the screen edge.** Double-click now focuses the panel instead of opening a popup, and it is "hard to move the mouse to the edge of screen every time". The workaround is to undock the panel so it floats. Altium later added a "Double Click Runs Interactive Properties" option. https://www.eevblog.com/forum/altium/ad18-popup-window/ ; https://www.altium.com/documentation/altium-designer/nfs-20-0properties-panel-improvements-ad?version=20.0
- **Floating ActiveBar.** "that stupid floating toolbar thing at the top". The new toolbar layout also wastes rows. https://www.eevblog.com/forum/altium/ad18-ui-switch/
- **Inconsistency.** "The way one does something should not depend on what the item is." The Parameter Manager has no undo and sorts "1k" wrongly. https://mediawiki.dideas.com/index.php?title=Altium_needs_to_fix_their_UI_and_bugs
- **Manual needed.** "probably the only software package for which I really needed the manual". A competitor's UI was called "much better than the Altium interface". https://www.eevblog.com/forum/eda/orcad-compared-to-others/
- **Clunky after KiCad.** KiCad shortcuts are "more intuitive"; "once you've gotten used to KiCad, Altium can feel clunky". https://c2s2.engineering.cornell.edu/blogposts/SP24/chooseFighter.html

---

### Micro-Cap 12

#### Liked
- **Better waveform viewer.** "The waveform viewer is infinitely better in Micro-cap." https://www.eevblog.com/forum/projects/micro-cap-12-(circuit-simulation)-vs-ltspice/
- **GUI measurements.** "a measurement system that's nice to use, whereas LTSpice only offers you what Spice does". https://www.eevblog.com/forum/projects/micro-cap-12-(circuit-simulation)-vs-ltspice/
- **Polish and point-and-probe.** It is "more polished". Point-and-probe is what sets it apart from LTspice, along with easier part creation from SPICE models. https://www.eevblog.com/forum/projects/micro-cap-12-(circuit-simulation)-vs-ltspice/
- **Mature and intuitive.** "very mature ... very intuitive and has a huge library of both parts and sample circuits". https://hackaday.com/2020/01/08/commercial-circuit-simulator-goes-free/
- **HN wish.** LTspice should take the "MC GUI and features" on top of the LT engine. https://news.ycombinator.com/item?id=22010147

#### Disliked
- **Abandoned.** It was frozen when Spectrum Software closed in 2019, and Windows-only. https://en.wikipedia.org/wiki/Micro-Cap

---

### xschem (reviews only; no source read, per D1)

#### Liked
- **Fast and natural.** "the flexibility and speed of interface/graphic are incredible". "If you're a Cadence user this schematic editor will feel pretty natural". "learning curve is about as light as it gets". https://sourceforge.net/projects/xschem/reviews/
- **Undistracting.** It "slowed [me] down not one bit from a cold start". https://sourceforge.net/projects/xschem/reviews/ (via search snippet)

#### Disliked
- **Learning curve.** "There is a learning curve, but it is a price for flexibility". https://sourceforge.net/projects/xschem/reviews/
- **Dated look.** The Tcl/Tk + X11 UI looks dated [unsourced as user sentiment; general Tk observation only]. https://cgicoffee.com/blog/2026/04/tcl-tk-develop-cross-platform-cli-gui-tools-tutorial-guide

---

### Revolution EDA

- **No public UI feedback found.** The v0.6.0 release discussion has 0 comments. https://github.com/eskiyerli/revolution-eda/discussions/10
- **Positioning.** A Python-scriptable schematic, symbol and layout editor with Python callbacks for instance parameters, a human-readable JSON format, and integrated plotting. https://reveda.eu/ ; https://pypi.org/project/revolution-eda/0.6.1/

---

### Flux.ai and browser tools (contrast)

#### Liked
- **Modern feel.** "a modern take on electronics design with slick UX". https://news.ycombinator.com/item?id=31236207
- **Live pricing and AI.** Real-time part pricing and an AI copilot. https://www.g2.com/products/flux-ai/reviews
- **Falstad/CircuitJS.** It edits live and is "3 times quicker at building circuit", but "it's Generic parts". https://hackaday.com/2022/08/04/30-free-circuit-simulators-lightly-reviewed/ ; https://news.ycombinator.com/item?id=47138135

#### Disliked
- **Performance.** Example projects made the "fans start to spin, with several seconds delay for any interaction". https://news.ycombinator.com/item?id=31236207
- **Browser CAD weaknesses.** "Browser apps ... are at their worst in CAD - speed, keyboard shortcuts, reproducibility, stability". Hotkeys clash with the browser's own ("incognito mode"). https://forum.kicad.info/t/anybody-looked-at-or-used-flux-ai/40449
- **Lock-in and longevity.** Cloud storage is a problem for regulated design retention. "Collaborative editing ... is just not a big enough need". https://forum.kicad.info/t/anybody-looked-at-or-used-flux-ai/40449
- **CircuitLab.** It has "no custom key commands", and part search "stalled looking for 'op amp'". https://news.ycombinator.com/item?id=22011994

---

### Others mentioned in passing
- **OrCAD.** It "always seems to select exactly what I do not want" and "still feels like it is 1999". https://www.eevblog.com/forum/eda/orcad-compared-to-others/
- **DipTrace.** Recommended as "one of the more intuitive electronics CAD packages", often for beginners. https://www.eevblog.com/forum/eda/drawing-schematics-in-kicad-eagle-is-so-frustrating-((-(howtogetstarted)/
- **Eagle.** It lost UI comparisons over the "Move group" quirk and having no default shortcuts for rotate or wire. https://www.build-electronic-circuits.com/kicad-vs-eagle-2018-comparison/
- **Every ECAD.** "pick something, stick with it ... learn the shortcuts and after a while you'll stop noticing that it sucks." https://www.eevblog.com/forum/eda/drawing-schematics-in-kicad-eagle-is-so-frustrating-((-(howtogetstarted)/

---

### Cross-tool patterns

1. **Speed beats looks.** LTspice and xschem are loved despite a dated look because capture is fast once the keys are learned. Users forgive ugliness. They don't forgive friction ("not beautiful, but very productive").
2. **Hover-then-key is the expert gesture.** Point at an object and press a key (KiCad v5 "point and use", G-drag, LTspice). When v6 made select-then-act mandatory, it caused the loudest complaints. Supporting both modes keeps both camps.
3. **Single-letter mnemonic keys win, odd ones lose.** KiCad's A/M/W/L earns praise. LTspice's F7 move and the Ctrl+R/E mode system are mocked. Keyboard-only wire start, bend and end matters to power users.
4. **Changing defaults loses users unless "classic" is one click away.** LTspice 24 (keys, icons, background) and KiCad 6 (selection) both met revolts. LTspice's "Restore Classic Values" defused part of it. Hotkey sets should be data that survives upgrades.
5. **Probe-to-plot is the killer simulation interaction.** Click a net for V, a device for I, Alt for power, with the cursor showing what will be measured. Micro-Cap and LTspice are praised for it, and KiCad's thin simulator layer is criticized next to them.
6. **Users want simulation results on the schematic.** People ask for click-for-current. LTspice .op labels as editable expressions are valued, and Falstad/Tina-style live annotation is the envy. Current arrow direction must be predictable.
7. **The waveform viewer matters as much as the editor.** Micro-Cap wins on its viewer and GUI measurements. LTspice loses because measurements are `.meas`-only text.
8. **A live-tuning slider is a beloved interaction.** KiCad 8 tuner and Qucs-S tuning mode both get praise for a slider that re-simulates immediately.
9. **A properties panel is good for bulk edits and bad for one quick edit.** Mixed selection showing the shared fields (KiCad 8, Altium Inspector + Find Similar) wins. Forcing the mouse to the screen edge and losing grouped dialogs loses. Best of both: double-click opens a near-cursor popup (or a floatable panel), and the docked panel handles multi-select.
10. **"Find similar → edit all" is the Altium differentiator.** Selection by property plus a filter for which object types are selectable (KiCad selection filter, "work of a genius") is what makes dense sheets editable.
11. **Live feedback during drags.** Rubber-banding wires, live junctions and short warnings (KiCad 10) are praised. Silent auto-merge on drop ("some i/o's got grounded") is hated.
12. **Theme: dark is liked, but it must be a choice.** Offer Light/Dark/System, independent of the OS (KiCad 10 complaint). Ship sane default colours (no eye-burning reds) and colour themes users can share as files.
13. **Icons: crisp and thin beats chunky.** LTspice 24's fat icons and big probe cursors were hated. Small screens need configurable icon size and compact toolbars (KiCad 9/10). Don't hide frequent tools in flyouts (the KiCad 10 net-label flyout).
14. **The canvas is precious.** Users toggle all panels to maximize it. Floating bars that can't be turned off (Altium ActiveBar) and extra toolbar rows are resented. One-click toggles for panel visibility are expected.
15. **Number entry: forgive the user.** SPICE's `1M` = milli trap is a universal footgun. Accept engineering suffixes (k, u, n, p, meg) and show the parsed value (or warn on `M`). Altium's "1k sorts wrong" shows that values must sort numerically in tables.
16. **Consistency across editors and object types.** KiCad v6 was praised for unifying the schematic and PCB editors. Altium is criticized when "the way one does something depends on what the item is".
17. **Hide SPICE text by default, keep it reachable.** Directive soup ("arcane text commands on the diagram") and cryptic error messages ("written in Klingon") are top complaints. Experts still want the text.
18. **Browser and cloud tools are judged on latency, shortcut capture and lock-in.** Any web build must own its keyboard and stay responsive at 1000+ objects. Local, plain-text files are a feature to users.
19. **Hierarchy and net navigation in a dockable side panel.** The net navigator lists every occurrence of a net, grouped by sheet, and clicking jumps to it. It is expected in modern tools [the praise for it is thinly sourced; docs only].

---

## Part 3: Waveform viewer UX and EDA visual conventions: research notes

Purpose: give UI design for Schemify's schematic canvas and built-in waveform viewer something to stand on.
Date: 2026-10-12. Clean-room: no xschem source was read. One Surfer issue (#391) mentions xschem's built-in viewer; that is a user opinion, not source.

Method note: Reddit (www and old) and the Cadence community (Cloudflare) blocked direct fetches. Reddit content therefore only shows up through search-engine summaries. Hacker News was read through the Algolia API, GitLab issues through the GitLab API, and EEVblog and KiCad forum pages through WebFetch. Claims with no source are marked [unsourced].

---

### A. Waveform viewer UX

#### Cadence ViVA (Virtuoso Visualization and Analysis)

- **Markers are keyboard-first.** M = point marker, V = vertical, H = horizontal, A/B = an A/B delta pair (pressing A or B again moves that marker and the delta updates), D = add a second marker of the same type and show the delta, Shift-D = deltas between all selected markers. The sequence "M, D, D, D…" chains deltas. Liked: fast single-key measurement. Sources: https://blog.asicedu.com/2020/11/cadence-viva-commonly-used-keybindings.html, https://community.cadence.com/cadence_blogs_8/b/cic/posts/things-you-didn-t-know-about-virtuoso-delta-markers-in-viva
- **Gap reported by a user:** you "cannot add delta marker between horizontal cursor just as vertical does". Delta support differs between marker types, and users notice the asymmetry. https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/47054/cannot-add-delta-marker-between-horizontal-cursor-just-as-vertical-does
- **Strip mode vs overlay.** "Strip Chart Mode" (Axis→Strips) puts each waveform in its own strip, and all strips share one X axis. You move traces by dragging them between strips, subwindows or windows. Subwindows place different analyses side by side. https://community.cadence.com/cadence_blogs_8/b/cic/posts/things-you-didn-t-know-about-virtuoso-viva, https://community.cadence.com/cadence_blogs_8/b/cic/posts/virtuosity-subwindows-in-viva
- **Users ask for the opposite as well:** "how to direct ViVA XL to display all signals in single window/strip", and "ViVA plot all in a single window?" The default layout is a recurring complaint in both directions. https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/24320/viva-plot-all-in-a-single-window
- **Sweep and corner families are organised on three independent channels: strip, color and symbol.** Strips can group by "Family" (signal name) or "leaf" (one temp/voltage point). Colors can follow the family, so the same signal is the same color across strips. Symbols can encode a third sweep variable. Example: strip = vdd, color = temperature, symbol = family. Liked: it scales to multi-dimensional corner sets. https://community.cadence.com/cadence_blogs_8/b/cic/posts/virtuosity-organizing-waveform-families
- **Color defaults are a recurring friction point.** Threads include "VIVA waveform colors/types", "Defining waveform thicknesses and colors on a white background in VIVA", "Setting VIVA Waveform Color Defaults When Using ADE" (Cadence RF blog), and "default settings for ViVa waveform viewer". Users switch to a white background (for papers and slides) and then find the default trace palette is tuned for black. https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/38736/defining-waveform-thicknesses-and-colors-on-a-white-background-in-viva, https://community.cadence.com/cadence_blogs_8/b/rf/posts/setting-viva-waveform-color-defaults-when-using-ade
- **Calculator.** An expression buffer shows e.g. `VT("/vout")`, and a "Plot" button sends it to the graph. `fam*` functions operate on whole families. The `dr.*` functions turn a family into a value-vs-sweep-variable plot. Liked: measurements are expressions that can be saved and re-evaluated. https://www.bioee.ee.columbia.edu/courses/cad/html/spectresimulation.html, https://picture.iczhiku.com/resource/eetop/WHidDLSLTqdhKmvB.pdf
- **Eye diagrams are calculator functions.** `eyeDiagram`, plus `eyeHeightAtXY` and `eyeWidthAtXY` (since IC6.1.8 / ICADVM18.1 ISR4) with selectable height definitions. They are measurements on a folded view, not a separate tool. https://community.cadence.com/cadence_blogs_8/b/cic/posts/virtuoso-video-diary-do-more-with-eyeheightatxy-and-eyewidthatxy-calculator-functions-in-virtuoso-visualization-and-analysis-xl, https://community.cadence.com/cadence_blogs_8/b/cic/posts/virtuosity-eye-diagram-measurements
- **Monte Carlo:** in ADE's "Yield" view, right-click an expression and choose "Plot Histogram". The yield table shows mean and std-dev next to it, and `histogram("DCgain" ?numBins 20 ?density t)` is the scripted form. Distributions live in a table-plus-histogram pairing, not as a pile of overlaid traces. https://www.edaboard.com/threads/histogram-plot-in-cadences-post-processing-environment.216697/, https://groups.google.com/d/topic/comp.cad.cadence/n0sg8OlG3vM
- **Cross-probing:** ADE Explorer Results → Direct Plot → Transient Signal, then click nets on the schematic and press Esc to plot. This is modal: you pick the analysis first, then click. https://www.bioee.ee.columbia.edu/courses/cad/html/spectresimulation.html
- **Disliked, performance:** "Plot all" from Assembler to a new ViVA window took "more than 13 minutes" (more than 4× the simulation time). The ViVA and Assembler windows froze, and dragged windows were "only partially painted". https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/52733/viva-slow-plotting-speed (search summary; page is behind Cloudflare)
- ViVA offers a "fast viewing waveform" mode for large data. It is opt-in, which suggests the default path is slow on big PSF/FSDB. https://blog.eetop.cn/blog-6503-27767.html

#### Synopsys Custom WaveView

- Positioned on "fast loading, display scrolling, and zooming of very large waveform files". Panels in a window are "a vertical stack or … independent rows and columns", and multiple tabs let users "mix-and-match time and frequency domains in a single session". Synopsys datasheet: https://www.synopsys.com/content/dam/synopsys/verification/datasheets/Custom-WaveView-ds.pdf
- **Measurement tool:** more than 35 measurement types (time, frequency, statistical, level, S-domain, RF). Measurements are "persistent across sessions" and "updated automatically" when results change ("Measure update on reload"), and can be exported as `.meas`. Liked: measurements behave as live objects, not one-shot annotations. (same datasheet)
- **Analog-to-digital conversion in the viewer:** analog results can become single- or multi-bit digital signals "with user-selectable thresholds". This bridges mixed-signal debugging. (same datasheet)
- **Eye diagrams:** a folded eye is built by "automatically extracting the clock cycle, or by reference to an ideal clock, or to an external signal". An "Automatic Eye Measurement tool" reports opening and aperture and builds a jitter histogram. https://www.synopsys.com/content/dam/synopsys/verification/datasheets/Custom-WaveView-ds.pdf
- Signal search uses pattern matching and wildcards, with drag-and-drop from the search results. Hot keys are user-definable. WaveView cross-probes with the Custom Designer schematic editor. (same datasheet)
- ADC toolbox: INL/DNL, histogram, SNR and similar. Domain-specific analysis panels sit beside the generic calculator. (same datasheet)

#### LTspice

- **Probing from the schematic is modeless.** The mouse cursor changes shape by context. Clicking a wire shows a voltage probe and plots the node. Clicking a component body shows a current clamp and plots the current. Alt+click shows a thermometer and plots power in W on its own axis. Dragging from one node to another plots the differential voltage (red probe at the first node, black at the second). Liked: zero-mode cross-probing, and the most-cited reason people like LTspice. https://analogdevicesinc.github.io/ltspice-reference/ai_ref/WAVEFORM-VIEWER-GUIDE.html, https://www.eecg.utoronto.ca/~ali/spice/LTspice/LTSpiceShortGuide.pdf
- **Attached cursors:** at most 2. Left-click a trace label to attach cursor 1. Right-click the label to choose "1st & 2nd". A readout window then shows x1, y1, x2, y2, Δx and Δy. Arrow keys move the cursor, and Up/Down cycle the cursor through `.step` runs. https://ltwiki.org/LTspiceHelp/LTspiceHelp/Attached_Cursors.htm
- **Ctrl+click a trace label** computes the average and RMS over the visible window. This is a one-gesture measurement scoped to the current zoom. https://analogdevicesinc.github.io/ltspice-reference/ai_ref/WAVEFORM-VIEWER-GUIDE.html
- **Bode by default for AC:** magnitude is solid, phase is dashed on the right axis. Left-clicking the vertical axis switches between Bode, Nyquist and Cartesian. The right axis can show phase, group delay, or nothing. https://analogdevicesinc.github.io/ltspice-reference/ai_ref/WAVEFORM-VIEWER-GUIDE.html, https://www.ednasia.com/how-to-use-ltspice-to-produce-bode-plots-for-led-drivers/
- **Expressions** are typed on the trace label and include FFT and derivatives. "Dimensional analysis" labels results with computed units, so V*I shows as W. Liked. (same guide)
- **Disliked, stepped runs:** colors of `.step` traces "are strictly defined by the order of the waveform being plotted". If you plot one stepped waveform per pane, each step gets a different color, "if you keep track of the colors, you can identify them that way". Step legends are buried (View → Step Legend, or Notes & Annotations → Annotate Steps). `V(out)@3` selects a single step. Users keep asking how to identify which curve is which step. https://groups.io/g/LTspice/topic/identification_of_traces_in/50142447, https://groups.io/g/LTspice/topic/labelling_stepped_waveforms/83300194, https://ez.analog.com/design-tools-and-calculators/ltspice/f/q-a/566133/is-there-anyway-to-get-details-of-step-run-parameter-details-from-the-monte-carlo-simulation-plot
- **Disliked, zoom lost on re-run:** "when the sim is restarted, the origin and scaling axis are always reset. So after each run it is necessary to re-navigate back to the area/point of interest." The workaround is to save a `.plt` file and bind Space to "Reload Plot Settings File". https://www.eevblog.com/forum/projects/ltspice-run-simulaton-without-changing-the-waveform-zoom-level-and-focus/
- **Disliked, cursors cannot be typed:** there is "no way of placing cursor(s) at specific points by typing values" (feature request: "Ability to Enter Value, Save Positions"). https://ez.analog.com/design-tools-and-calculators/ltspice/f/q-a/578166/ltspice-waveform-cursors-ability-to-enter-value-save-positions-feature-request
- **Disliked:** holding Ctrl drags the plots when you meant to Ctrl+click a label. https://www.eevblog.com/forum/eda/ltspice-xvii-disabling-waveform-_drag_-with-ctrl-depressed/
- **Disliked, general:** "Its like working with a software from 199X". The scroll-wheel zoom "scrolls only in tiny steps". The UI is "more like a DOS-based UI, with heavy use of function keys, no sensible use of right click" (SiliconWizard). Even a defender says "The user interface sucks for sure." https://www.eevblog.com/forum/eda/ltspice-is-a-big-dissapointment-!/
- **LTspice 24 changes:** pane rearrangement via right-click, menu items and shortcuts for cursors, annotated phase/gain margin, and zoom/pan *while the simulation is running*. A thread titled "UI LTspice24 is horrible" exists. Grid lines became so faint they are "difficult to see"; Analog Devices staff replied to change the color, and noted the grid is clear in dark mode. https://ez.analog.com/design-tools-and-calculators/ltspice/a/faqs-docs/FD559/what-s-new-in-ltspice-24, https://ez.analog.com/design-tools-and-calculators/ltspice/f/q-a/581013/ui-ltspice24-is-horrible, https://ez.analog.com/design-tools-and-calculators/ltspice/f/q-a/579635/grids-are-difficult-to-see
- The waveform background defaults to black and is changed through Tools → Color Preferences → WaveForm → Background. Printer-friendly settings pages exist precisely because the default does not suit print. https://www.macnica.co.jp/en/business/semiconductor/articles/analog_devices/144247/, https://low-level.wiki/diverse/ltspice.html
- The XOR crosshair cursor is "visible no matter what screen color is behind the cursor". An opaque fallback exists for GPUs that cannot do XOR. https://ltwiki.org/files/LTspiceHelp.chm/html/WaveForms.htm
- Wish list: line styles (dotted, hatched) "for black and white copying", and a togglable legend showing step parameter values. https://groups.io/g/LTspice/topic/suggestions_for_waveform/50142338

#### KiCad simulator (ngspice front end)

- The GUI exposes two cursors plus a differential cursor per plot. A table lets you type a time and read the voltage, but a user asks to type the *voltage* instead, to measure time between zero crossings (#22862). https://docs.kicad.org/doxygen/classCURSOR.html, https://gitlab.com/kicad/code/kicad/-/work_items/22862
- **Disliked, auto palette:** "never have yellow autoassigned again (similarly, blue on the dark background)". The user asks for a configurable palette, perhaps matplotlib's qualitative colormaps (#25673, KiCad 10). https://gitlab.com/kicad/code/kicad/-/work_items/25673
- **Disliked, the signal list does not scale:** "there is a voltage for every node, a current for every pin, and … a power for each component". Users spend time "scrolling back and forth … to remove them … change their color, add cursors, or just see which color is which". They ask for active signals pinned to the top (#25649). https://gitlab.com/kicad/code/kicad/-/work_items/25649
- **Disliked, multi-pane in KiCad 10:** choosing the target pane per curve from a dropdown means that splitting 6 AC traces (3 nets × mag/phase) "may take dozens of mouse clicks". Asked for instead: drag-and-drop and a "delete all curves" button (#25359). https://gitlab.com/kicad/code/kicad/-/work_items/25359
- **Disliked, sweeps:** in a double DC sweep the cursor can only measure the last curve, and the user cannot pick which family member it attaches to (#21758). https://gitlab.com/kicad/code/kicad/-/work_items/21758
- **Disliked, axes:** auto ticks land at 1.6 and 0.6 intervals, and there are no minor grids. Users want ticks on 1-2-5 values (#23689). https://gitlab.com/kicad/code/kicad/-/work_items/23689
- When current and power share the right Y axis, the unit titles overlap and become unreadable (#21645). https://gitlab.com/kicad/code/kicad/-/work_items/21645
- On restart, Smith chart cursors reset (#25153) and cursor tags overlap (#25079). This is the same "re-run loses my view state" pain as LTspice. https://gitlab.com/kicad/code/kicad/-/work_items/25153

#### gaw / gwave / gawm (open-source analog viewers)

- gaw offers unlimited panels and waves. You drag a signal button onto a panel to plot it and drag waves between panels. A click in empty space moves "the last one [cursor] that was dragged". http://gaw.tuxfamily.org/linux/gaw.php
- **Disliked:** "gaw is a fork of gwave, with features removed and bugs introduced… barely functioned and was hard to read" (Ubuntu wiki review of large analog data). It also says gtkwave "doesn't seem to support ngspice files". https://wiki.ubuntu.com/Waveform_Viewers-Plotting_Large_Analog_Data
- **Disliked:** an IHP-130 xschem+ngspice user tried gaw and gwave: "its UI/UX is, well terrible". They also found the schematic tool's built-in viewer "not … very reactive" and asked Surfer to add `.raw` support (Surfer #391). https://gitlab.com/surfer-project/surfer/-/issues/391
- The gawm fork (2025–26) is a checklist of what was missing: an embedded resizable signal sidebar, ellipsized labels with tooltips, hidden waves shown with strikethrough and reduced opacity, a 10-px circular hit test for steep edges, an X-value box on each cursor, Y-value dots plus boxes at cursor/trace intersections, a ΔX box, **per-signal ΔY between cursors**, and a Python shell bridge (`g.get_cursor_values()`, `g.plot_analysis()` for PySpice). https://github.com/kcaisley/gawm

#### GTKWave (digital contrast)

- Analog in GTKWave is bolted on. Trace height is limited to integer multiples of a digital row. "Analog interpolated" renders like steps on some builds (#463). Height rescales continuously while scrolling. https://github.com/gtkwave/gtkwave/issues/463, https://github.com/gtkwave/gtkwave/issues/241, https://sourceforge.net/p/gtkwave/mailman/message/30551143/
- Performance: a 2.9 GiB VCD takes 16 s and 800 MiB in GTKWave, against Surfer's 2.0 s on 8 threads and 74 MiB. https://kevinlaeufer.com/pdfs/surfer_cav2025.pdf
- On HN, GTKWave is often called ugly: "It requires Gtkwave to view waveforms with (opinion, but I hate the UI)" (analognoise). https://news.ycombinator.com/item?id=19551862. Also "Surfer … Much better than gtk wave" (etep). https://news.ycombinator.com/item?id=43965629
- What Surfer users ask to keep from GTKWave: "centered on the cursor on any zoom operation (as in GTKWave)" (Surfer #394). https://gitlab.com/surfer-project/surfer/-/issues/394

#### Surfer (Rust, egui, native + WASM)

- **Motivation:** the author was "bothered by small things like how zooming and scrolling works in gtkwave". The UI is keyboard-first with a "fuzzy matching based command palette, similar to ctrl-p in Visual Studio Code" ("As a vim user, I hate reaching for my mouse"). The author is candid: "I'm not a UI designer… I'm just winging it", and relies on community feedback. https://blog.yosyshq.com/p/community-spotlight-surfer/
- **Performance design:** signal names and hierarchy are parsed first, in "much less than one second", and value data parses in the background. This "allowed Surfer to avoid a loading screen". Only the signals a user selects are decompressed (271 µs on average per signal). Parsing is parallel, and a server mode streams compressed signals lazily from remote compute machines. https://kevinlaeufer.com/pdfs/surfer_cav2025.pdf
- **Analog support came late.** NLnet funded "drawing of analog signals". The renderer offers "linear or stepped" styles. https://nlnet.nl/project/Surfer/, https://zenodo.org/records/18622145
  - Users want analog traces taller by default and type-based defaults, the way "in Cadence Xcelium real numbers are traced as analog step by default and have a bigger height" (#569). https://gitlab.com/surfer-project/surfer/-/issues/569
  - Autoscale confusion: a 4e9 → 1e9 step does not look like a 4:1 ratio, so the user cannot tell where the Y axis starts (#534, 15 comments). https://gitlab.com/surfer-project/surfer/-/issues/534
  - The analog min/max labels are nearly invisible on the light+ theme (#535). https://gitlab.com/surfer-project/surfer/-/issues/535
  - Requested: "Display variables on top of each other" (overlay) by dragging one onto another (#314). https://gitlab.com/surfer-project/surfer/-/issues/314
- **Cursor and marker feedback:**
  - A user asked to restore drag-to-move-cursor: "When I read the diagram, I usually move the cursor to where I look" (#421). https://gitlab.com/surfer-project/surfer/-/issues/421
  - Asked for: numbered marker tags, an "active marker" concept, Δt drawn as a horizontal dimension line with the number above it, and edge counting between markers (#265). https://gitlab.com/surfer-project/surfer/-/issues/265
  - Asked for: markers that snap to signal edges (#586), and labelled range regions as well as points (#566). https://gitlab.com/surfer-project/surfer/-/issues/586, https://gitlab.com/surfer-project/surfer/-/issues/566
  - Disliked: fuzzy matching runs an unrelated command when the typed command matches nothing, so "a slight typo quickly becomes a nuisance" (#345). https://gitlab.com/surfer-project/surfer/-/issues/345
- **Theme feedback:**
  - Default dark; one user wrote "Can we have a light theme, I don't like working in the dark" (#512).
  - Theme choice is not auto-saved, which breaks "user expectation from working with other GUI apps" (#489).
  - The default theme "lacks sufficient contrast" on hovered and selected menu items (#554).
  - Users dislike zebra striping every 3 rows: "I feel distracted" (#476).
  - Color-blind themes exist, and adopting Petroff's 6/8/10-color palettes (arXiv 2107.02270) was proposed (#409).
  - Text color is chosen automatically against the background (#181).
  https://gitlab.com/surfer-project/surfer/-/issues/512, …/489, …/554, …/476, …/409, …/181
- **Reception:** classes at Cornell, MIT, UCSC and JKU recommend it. Quicksilicon made it their primary viewer. Others write that "Surfer is fantastic" and "Surfer deserves to hit the front page… Much better than gtk wave." https://kevinlaeufer.com/pdfs/surfer_cav2025.pdf, https://newsletter.quicksilicon.in/posts/waveform-debug-using-surfer, https://news.ycombinator.com/item?id=47747453
- VaporView (a VS Code viewer) argues that an integrated viewer pays off: terminal links from sim logs jump to a timestamp, remote SSH works without "a laggy VNC session", and hierarchy maps back to source. https://news.ycombinator.com/item?id=47747453

#### ngspice native plotting and the Python/matplotlib escape hatch

- ngspice's `plot` is a basic X11 window. `gnuplot` and `xgraph` commands write files and shell out; users report gnuplot spawning extra terminals and leaving stray files. https://ngspice.sourceforge.io/docs/ngspice-html-manual/manual.xhtml, https://sourceforge.net/p/ngspice/discussion/127605/thread/04c1823416/
- Many raw-file readers exist for matplotlib: spicelib (LTspice, ngspice, QSPICE `.raw`), `ltspice`, spice-plot, spyci and PyLTSpice. Their existence shows that users routinely leave the GUI viewer for scripting, batch plots and publication figures. https://github.com/nunobrum/spicelib, https://github.com/lrvdijk/spice-plot, https://github.com/gmagno/spyci
- Motivation stated in one write-up: "using LTspice just like another python module – without clicking on anything, without even opening the LTspice window". https://acidbourbon.wordpress.com/2019/11/26/seamless-integration-of-ltspice-in-python-numpy-signal-processing/
- Cadence Monte Carlo data gets exported to MATLAB for plotting ("Import MC results from Cadence and Plot using Matlab"). https://ntuzxy.github.io/posts/matlab_plot_MC_results/
- Qucs-S puts plots on the schematic sheet as "diagram" components, including a tabular display. Arrow Up/Down move a marker between sweep curves. https://qucs-help.readthedocs.io/en/spice4qucs/PostSim.html

---

### B. Schematic canvas and EDA chrome conventions

#### Backgrounds and default schemes

- **Virtuoso** defaults to a black canvas with a white dot grid. "How do I change Virtuoso's black background to white" is a perennial question, answered with the `Opus.editorBackground` X resource or, in IC6, CIW Options → User Preferences. Users usually flip it for printing and documentation. https://www.edaboard.com/threads/how-to-change-the-virtuosos-black-background-to-white.142661/, https://kwagjj.wordpress.com/2014/06/13/change-black-background-color-in-cadencevirtuoso/
- **Virtuoso schematic wires** default to "CadetBlue" for wires and labels, with colors set per layer-purpose pair ("device drawing1", "wire drawing", "wire label") in the Display Resource Manager. Colors are table data, not code. https://miscircuitos.com/how-to-change-the-wire-colour-in-cadence/, https://cmosedu.com/jbaker/courses/ee421L/f18/students/skellj1/aesthetics/aestheticsnew.htm
- Typical Virtuoso schematic colors besides the blue wires: green symbol bodies, red pins, and white or yellow highlight for selection. [unsourced]
- **KiCad:** eeschema shipped with only a light theme and pcbnew with only a dark one. A request asks for both in both editors, chosen from the OS theme (#11780). KiCad 6 added JSON theme files ("kicad-2020" default, "kicad-classic" legacy). https://gitlab.com/kicad/code/kicad/-/issues/11780, https://github.com/pointhi/kicad-color-schemes
- **KiCad 6 theme redesign goals:** "Improve accessibility to colorblind and low vision users", "Improve contrast", and pick colors "that respond well to opacity less than 100%". The redesign became possible because rendering "does not depend on XOR mixing" any more (#4259). https://gitlab.com/kicad/code/kicad/-/work_items/4259
- **Colorblind, concrete case:** KiCad's top and bottom copper defaulted to red and green, "bad for color blindness (8% of males…)" (#1991). Via-span color coding was criticised as "not great … especially for people with co[lor blindness]" (#13566). https://gitlab.com/kicad/code/kicad/-/work_items/1991, https://gitlab.com/kicad/code/kicad/-/work_items/13566
- **The white schematic hurts some users:** "the longer I spend the longer I hurt my eye from white background". The same user still finds black uncomfortable when component colors lack contrast, likes "the yellow scheme of protel/altium" and DipTrace dark ("bright and high contrast"), and admires a "grey background and magenta component and yellow wire" setup. Disliked: having to edit the settings file for colors that the GUI picker would not offer. https://forum.kicad.info/t/default-color-scheme-burn-my-eyes-a-lot-and-idea-color-setting-to-share/5444
- **EEVblog "Black background" thread:**
  - For black: "The blazing white burns my eyes" (Dago).
  - Against: "Expect to waste a lot of ink/toner if you ever commit them to paper… don't expect them to be readable" (T3sl4co1l).
  - Against: dark themes are a "childish" trend, and turning down monitor brightness is better (Bud).
  - Pain point: library-defined object colors override the theme, which "makes it useless" (Warhawk).
  https://www.eevblog.com/forum/altium/black-background/
- **Altium AD18's official rationale for going dark:** "while a text-oriented interface generally works best with dark text on a light background, an image-intensive interface is better for the eyes with light content on a dark background". https://www.altium.com/documentation/altium-designer/nfs-18-0a-fresh-user-interface-ad?version=18.1
- **Why CAD went dark historically:** on early low-refresh CRTs a black screen flickered less visibly, and entity colors "look more vibrant" against black. The counter-argument is that white "most closely resembles the paper". https://www.canadacad.ca/why-autocad-black-background-tutocad/
- In IC *layout*, black stands in for the substrate and the layer fills read against it. Schematic tools inherited the same canvas. https://www.edaboard.com/threads/how-to-change-the-virtuosos-black-background-to-white.142661/ (search summary)
- **LTspice:** the schematic is light (white or grey) and the waveform pane is black by default. Both are configurable in Color Preferences. https://www.macnica.co.jp/en/business/semiconductor/articles/analog_devices/144247/

#### Grid

- KiCad lets users pick the grid style (dots, lines or small crosses), the grid thickness, a "minimum displayed spacing" (the grid thins out when zoomed out) and the grid color. "How to change the grid style from dots to lines" and "Grid lines vs grid dots" are recurring threads, with no consensus. https://docs.kicad.org/8.0/en/eeschema/eeschema.html, https://forum.kicad.info/t/how-to-change-the-grid-style-from-dots-to-lines-in-the-schematics-editor/14783, https://forum.kicad.info/t/grid-lines-vs-grid-dots/5038
- Virtuoso's schematic default is "black background and many white dots". https://www.edaboard.com/threads/how-to-change-the-virtuosos-black-background-to-white.142661/
- LTspice 24's faint grid drew complaints; ADI's answer was to raise the grid color contrast. https://ez.analog.com/design-tools-and-calculators/ltspice/f/q-a/579635/grids-are-difficult-to-see

#### Net highlighting, selection and cross-probe colors

- **Virtuoso probe colors** cycle through only "4 or 5" colors before repeating. Probes use the `hilite` layer-purpose pairs, not `y0..y9`, so they are customisable. Dynamic net highlighting puts a colored halo on all segments of the hovered net, magenta when connectivity is complete and yellow when it is incomplete, so the color itself signals the state. https://groups.google.com/d/topic/comp.cad.cadence/Y8iA9Cty5rU, https://www.edaboard.com/threads/net-probe-colors-in-virtuoso-schematic-editor.189976/, https://support1.cadence.com/public/docs/content/20242490.html
- XL Probe: clicking a net in layout highlights it in both layout and schematic. Users expect bidirectional highlight between views. https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/43441/how-to-highlight-net-in-schematic-when-selecting-a-net-in-layout
- **KiCad netclass colors:** each netclass sets wire color, thickness and line style (solid, dashed, dotted). A "Highlight netclass colors" option draws the class color as a *halo around* the wire and leaves the wire in the theme color. A transparent class color falls back to the theme. Highlight color and depth are settings. One known bug: "Using net colors messes up net highlighting" (#10003). https://docs.kicad.org/7.0/en/eeschema/eeschema.html, https://gitlab.com/kicad/code/kicad/-/issues/10003

#### Fonts

- KiCad keeps its built-in **stroke font** as the default even after KiCad 7 added TrueType/system fonts. TrueType is described as "less readable and [with] a strong performance hit" on the canvas. A user asked for a slashed or dotted zero to tell 0 from O (#15400). https://forum.kicad.info/t/kicad-stroke-font/30140, https://gitlab.com/kicad/code/kicad/-/work_items/15400

#### High-DPI

- **KiCad:**
  - "Either blurry or tiny icons on high DPI display".
  - "Performance is very poor and KiCAD feels very sluggish… when high-DPI scaling is used" (#14533).
  - Icons are small under GNOME text scaling, a v8 regression (#16918).
  - Layer color squares are inconsistent at 200 % (#12805).
  - "Bad rendering quality with HiDPI" (#6794).
  https://forum.kicad.info/t/either-blurry-or-tiny-icons-on-high-dpi-display-nightly-build/31635, https://gitlab.com/kicad/code/kicad/-/issues/14533, …/16918, …/12805, …/6794
- **Qt-based EDA** (Virtuoso has been Qt since IC6.1): the usual workaround is `QT_SCALE_FACTOR` / `QT_AUTO_SCREEN_SCALE_FACTOR`. Only `QT_SCALE_FACTOR` also scales point-sized fonts. https://doc.qt.io/qt-5.14/highdpi.html
- Applying this workaround to Virtuoso is commonly done. [unsourced]

#### Toolbars and "modern" redesign reception

- **Virtuoso IC5 to IC6:** the move from Motif to Qt shifted dialog buttons to the bottom "to align with modern user interface conventions". It broke users' SKILL forms ("shifted or not visible") and the bindkey load order. On HN: "Cadence is a fine example of how horrible it can be… on a Sun Ultra 1 with Motif UI. Everything is better now", and Virtuoso "ha[s] had crappy UIs for decades". https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/19003/ic5---ic6-gui-forms-comapatibility, https://news.ycombinator.com/item?id=7373816, https://news.ycombinator.com/item?id=38498051
- A student's take on the Cadence UI: "It takes a certain grit to stare at the Cadence UI for 10 hours… Just learning the software alone was hell". https://news.ycombinator.com/item?id=36013099
- **Altium AD18:** the dark theme and new Properties panel were liked; menus moving and "that stupid floating toolbar thing at the top" (the ActiveBar) were not. Users miss putting a toolbar on the menu row and want screen space back. https://www.eevblog.com/forum/altium/ad18-ui-switch/
- **KiCad 6:** reviewers praised that clicking "actually selected it—matching every other design application", and that the schematic and PCB editors now feel like one program. Veterans objected to "Point and use" becoming "Point, click to select, then use", to more clicks, to hotkey churn, and to a look that is "almost cartoon like" compared with v5 (icon redesign). Maintainers answered that the old paradigm was "widely regarded as very non-intuitive". https://techexplorations.com/blog/kicad/kicad-6-review-new-and-improved-features/, https://forum.kicad.info/t/fighting-with-v6-0-ui/37534
- **Fusion 360 Electronics (Eagle successor):**
  - Thread title: "Fusion pcb design is the worst UI and UX I have experienced".
  - Startup takes 30–60 s, against Eagle's under 3 s.
  - Lag when switching tabs and dragging makes schematic editing "absolutely infuriating".
  - The library manager blocks the UI and updates itself every time it opens.
  The lesson: a modern look does not save a tool that has lost responsiveness. https://forums.autodesk.com/t5/fusion-electronics-forum/fusion-pcb-design-is-the-worst-ui-and-ux-i-have-experienced/td-p/12736733, https://forums.autodesk.com/t5/eagle-forum/fusion-eagle-feedback-from-a-long-time-eagle-user/td-p/10977783
- HN on EDA UIs generally: "tools like ModelSim don't look like modern software, the UI was never changed because it's not important". https://news.ycombinator.com/item?id=12950244

#### Engineering numbers

- **SPICE suffixes are case-insensitive, so `M` and `m` both mean milli and mega is `meg`.** The classic bug is "1M" read as 1 mΩ. One GUI shows "1 MΩ" while ngspice simulates 1 mΩ, and was filed as a bug: "Values with a bare M suffix display as mega but simulate as milli". The display layer must not reinterpret what the netlister emits. https://github.com/SDSMT-Capstone-Spice-GUI-Team/Spice-GUI/issues/948, https://github.com/z2amiller/kicad-jlcpcb-tools/pull/17, https://www.experimentalistsanonymous.com/ve3wwg/doku.php?id=ltspice_dirty_secrets
- SPICE accepts both `u` and `µ` for micro. Users type `u` because it is ASCII, while plots and readouts usually show SI-style numbers (e.g. 1.23µ or 1.23u, depending on the tool). https://www.experimentalistsanonymous.com/ve3wwg/doku.php?id=ltspice_dirty_secrets
- KiCad bug reports print `Encoded кΩ丈` in the version string, as an explicit test that Ω and non-ASCII text survive in values. https://gitlab.com/kicad/code/kicad/-/work_items/22862
- KiCad users ask for axis ticks on round 1-2-5 values rather than arbitrary intervals such as 1.6 or 0.6. https://gitlab.com/kicad/code/kicad/-/work_items/23689
- LTspice's unit-aware expressions label results with derived units (W, Ω, A). Readouts show units, not bare numbers. https://analogdevicesinc.github.io/ltspice-reference/ai_ref/WAVEFORM-VIEWER-GUIDE.html
- Designers expect cursor readouts in engineering notation (exponent a multiple of 3) with about 3–4 significant digits, and full precision available on copy. [unsourced; no settings documentation found for ViVA or WaveView readout precision]

---

### Cross-tool patterns

1. **Clicking the schematic to plot is the killer feature.** LTspice's modeless probe (cursor shape: voltage, current or power, plus drag between nodes for a differential) is the most-praised interaction. Virtuoso's Direct Plot works but is modal. Plan for click-to-plot with no mode switch and a cursor icon that previews what will be plotted.
2. **View state must survive a re-run.** LTspice, KiCad (Smith cursors) and ViVA "remember setup" threads all show the same complaint: re-simulating resets zoom, cursors and trace selection. Keep the viewport and cursors keyed by signal name across runs.
3. **Overlay vs stacked is a per-trace decision, made by dragging.** ViVA strips, WaveView stack or grid, LTspice panes, gaw panels and Surfer's overlay request all converge on drag-to-pane and drag-onto-trace-to-overlay. Dropdowns per curve, as in KiCad 10, are disliked.
4. **Families need more than one visual channel.** ViVA maps sweep variables independently onto strip, color and symbol. LTspice's "color = plot order" for `.step` is the most-complained-about alternative. Every family member needs a legend entry or hover tag that names its parameter values.
5. **Two cursors with Δ is the floor.** Users also want per-trace ΔY (gawm), typed cursor positions in both X and Y (LTspice, KiCad #22862), snap-to-edge or snap-to-trace (Surfer #586), and the cursor to follow a drag (Surfer #421).
6. **Single-key markers (ViVA M/V/H/A/B/D) are the power-user benchmark.** A fuzzy command palette is liked (Surfer), but a typo must not run an unrelated command (Surfer #345).
7. **Measurements should be live objects.** WaveView's measurements re-evaluate on reload and persist across sessions, and ViVA calculator expressions are re-plottable. Static annotations are the weak form.
8. **Make the window-scoped quick stats one gesture.** LTspice's Ctrl+click for avg/RMS over the visible window is a cheap pattern people love.
9. **Monte Carlo belongs in a histogram plus stats table, linked to traces, not a hairball.** ViVA's Yield → Plot Histogram and WaveView's jitter and ADC histograms both work this way.
10. **Eye diagrams are a fold transform plus measurement functions.** The fold can come from an auto-recovered clock, an ideal period or a reference signal. Height and width are calculator functions (ViVA, WaveView).
11. **AC means Bode by default:** log-frequency X, dB magnitude solid, phase dashed on the right axis, and one click to switch to Nyquist or group delay (LTspice). Axis ticks on 1-2-5 values. Shared secondary axes need non-overlapping unit labels (KiCad #21645).
12. **The trace palette has to fit the background.** Users complain about auto-assigned yellow on light and blue on dark (KiCad #25673), and about palettes tuned for black being used on white (ViVA). Ship a palette per theme, color-blind safe (Petroff and similar, as Surfer did), and add line styles for monochrome print (LTspice wish).
13. **Keep large data responsive.** Show signal names in under a second and parse values lazily (Surfer avoids any loading screen). Never block the UI: ViVA's 13-minute frozen "plot all" and Fusion's lag are the cautionary tales. Signal lists need filtering and active-first sorting (KiCad #25649).
14. **Dark canvas is the IC norm (Virtuoso, LTspice waveforms, Altium AD18), but a light theme is non-negotiable.** Users need light for print and papers, and some just prefer it (Surfer #512, EEVblog). Follow the OS setting (KiCad #11780) and auto-save the choice (Surfer #489).
15. **Colors are table data, user-editable in the GUI.** Virtuoso's display resource LPPs and KiCad's JSON themes both work this way. Hand-editing files to reach colors the GUI won't offer is a top complaint. Do not let library or symbol colors override the theme (EEVblog).
16. **Net highlight works best as a halo around the wire, not a recolor.** KiCad's "Highlight netclass colors" option and Virtuoso's dynamic highlight both draw a halo. The palette needs more than 4–5 distinct probe colors (Virtuoso repeats), and color can carry meaning, as with complete vs incomplete connectivity.
17. **Grid style is a preference, not a design decision.** Offer dots, lines and crosses, adaptive spacing that hides dense grids when zoomed out, and enough contrast (LTspice 24 backlash).
18. **Redesigns are judged on responsiveness and keeping muscle memory.** KiCad 6 (more clicks, hotkey churn, "cartoon" icons), AD18 (a floating toolbar that cannot be removed) and Fusion (slow) were punished; dark panels and click-to-select were welcomed. Text-plus-icon chrome and keyboard paths soften the transition. [partly unsourced inference]
19. **Treat high-DPI as day-one work.** KiCad's history of blurry or tiny icons and slow scaling, and Qt's env-var workarounds, show that bolting it on later hurts. Vector icons and a canvas rendered at device scale avoid the whole class.
20. **The display layer must round-trip SPICE suffix semantics** (`m` is milli, `meg` is mega, `u`/`µ` is micro) and must never show a value different from what is simulated.
