# Spectre netlist syntax used by the core library (2026-10-12)

What the `format spectre` templates of `lib/core` and `spectre_source()` (expr spec §5) rely on,
with the public sources each fact comes from. No proprietary Cadence files were read; Spectre
itself is not run in CI (TESTING.md), so these are snapshot-tested only.

| Fact | Used as | Source |
|---|---|---|
| Instance: `name (nodes) master param=value …`; a DC source `vdd (vdd! 0) vsource dc=3` | every core template | [1] §DC Source; [2] (Vsource, Isource) |
| Pulse: `vsource type=pulse val0= val1= delay= rise= fall= width= period=` | `pulse(v1 v2 td tr tf pw per)` | [1] §Voltage Pulse Source; [2] Vsource example |
| PWL: `vsource type=pwl wave=[t1 v1 t2 v2 …]` | `pwl(t1 v1 …)` | [1] §Piecewise Linear Source; [2] Vsource alternate example |
| `isource` takes the same `dc`/`type=pulse`/`val0`/`val1`/… parameters; first terminal is where positive current enters (as SPICE `n+`) | `isource` | [2] Isource example `i1 (in 0) isource dc=0 type=pulse delay=10n val0=0 val1=500u …` |
| Sine: `type=sine` with `sinedc`, `ampl`, `freq` (plus `delay`, `damp`, `sinephase`); AC: `mag` (and `phase`) | `sin(vo va f td theta phase)`, `ac mag phase` | [3] (netlists with `vsource type=sine sinedc=1`, `type=sine ampl=1m freq=20K`, `vsource mag=1 type=dc`); `delay`/`damp`/`sinephase` and `phase` from the Spectre components reference parameter list [4] |
| Exponential: `type=exp val0 val1 td1 tau1 td2 tau2` | `exp(v1 v2 td1 tau1 td2 tau2)` | [4]; mapping of SPICE `EXP()` arguments in [2] (`exp_pulse(time, low, high, tdelay1, tau1, tdelay2, tau2)`) |
| `vcvs (p n ps ns) gain=`; `vccs (sink src ps ns) gm=` (output pair first, then the controlling pair; current enters `sink`, as SPICE `G`) | `vcvs`, `vccs` | [2] Linear VCVS / VCCS, parameter mapping tables |
| `cccs (p n) probe=<source> gain=` | `cccs` | [2] Linear CCCS example `cccs probe=v002 gain=0.013357` |
| `ccvs (p n) probe=<source> rm=` (transresistance) | `ccvs` | [4] parameter `rm`; note [2]'s ccvs example writes `gain=`, copied from its cccs example, so `rm` follows the components reference |
| `resistor r=`, `capacitor c=`, `inductor l=`, all with `m=` | passives | [2] Inductor (`inductor l=10e-9 r=1 m=1`), Resistor/Capacitor mapping tables |
| A model or subcircuit is called the same way (`name (nodes) <model> params`) | diode, MOS, BJT, PDK cells | [2] (inductor "with model referenced": `ind l=…`) |
| `tline (p1 n1 p2 n2) z0= td=` | `tline` | [4] |
| Scale factors: `M` is mega, `m` milli, no `meg` | numbers written canonical (`num()`, `spectre_source()`) | [1] (numeric abbreviations), [4] |
| `bsource` exists (`v=` / `i=` expressions) but its expression language is Spectre's, not ngspice's; no public text gives its access-function spelling | `bsource` has no `format spectre` (`E013`) | [5] |

Not translated, by decision: SPICE `CSW`/`SW` switch models (Spectre's `relay` takes other
parameters), `sffm`/`am` sources, PWL options (`r=`, `td=`), the `pulse` count argument.

## Sources

1. Michigan State University ECE 410, *Guide to Writing Stimulus Files (modified for using
   spectre)*, https://www.egr.msu.edu/classes/ece410/mason/files/guide-stimulus.pdf
2. Keysight, *Netlist Translator for SPICE and Spectre* (ADS 2011),
   https://edadownload.software.keysight.com/eedl/ads/2011/pdf/netlist.pdf: the Spectre
   syntax side of every component it translates.
3. Search excerpts of public Spectre netlists (UP Microlab *Cadence Spectre Command-Line
   Tutorial*, https://www.up-microlab.org/resources/tutorials/cadence-spectre-command-line-tutorial/;
   comp.cad.cadence threads).
4. Cadence, *Spectre Circuit Simulator Components and Device Models Reference* (the
   `spectre -h <component>` text), parameter names as commonly quoted; the public
   *Virtuoso Spectre Circuit Simulator User Guide* 5.1.41 (University of Tennessee copy,
   https://web.eecs.utk.edu/~dbouldin/protected/spectreuser.pdf) could not be fetched
   (certificate error), so these names are the ones still to confirm against a Spectre run.
5. Cadence Community forum, *Syntax errors when utilising idt() in bsource* (thread 65672),
   and EDAboard *What is the function of bsource in Spectre?*: `bsource` with `V=`/`I=`.

Confirm on first access to a real Spectre: `sinephase`, `damp`, `td1`/`tau1`/`td2`/`tau2`,
`ccvs rm`, `tline z0/td` (rows sourced from [4]).
