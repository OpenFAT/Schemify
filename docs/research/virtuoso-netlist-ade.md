# Virtuoso interop research: netlisting, values, ADE testbench state

Clean-room research note for a Schemify <-> Cadence Virtuoso bridge. Written 2026-10-12 without access to Virtuoso.
Every fact carries a source tag `[Sn]` (list at the end) and a confidence mark:

- **VERIFIED**: stated in Cadence documentation, or visible in a real Virtuoso-generated file that I read.
- **LIKELY**: consistent across secondary sources (forums, third-party docs, inferred from several real files) but not stated normatively.
- **GUESS**: my inference; needs a golden file from a real Virtuoso run.

No GPL code and no xschem source was opened. Netlist snippets in §7 are written by me to illustrate the observed conventions; they are not copies.

---

## 1. Spectre netlist syntax (what Virtuoso emits and Spectre accepts)

### 1.1 Language mode, case, comments, continuation

| Fact | Conf. | Src |
|---|---|---|
| Spectre reads SPICE syntax by default; `simulator lang=spectre` switches to native Spectre mode; `simulator lang=spice` switches back. Modes can alternate within a file. | VERIFIED | S1, S2 |
| Spectre mode is fully case-sensitive and keywords are lower case. SPICE mode lower-cases everything except quoted strings. | VERIFIED | S1, S2 |
| An `include`d file starts in SPICE mode and the mode reverts at end of file, **unless** the file has a `.scs` extension or begins with `simulator lang=spectre`. Same rule for `include "f" section=x` library files. | VERIFIED | S1, S2 |
| Comments: `//` (preferred) or a leading `*`; ` //` mid-line starts a trailing comment. | VERIFIED | S2 |
| Continuation: trailing `\` (preferred, must be the last char) or leading `+` on the next line. ADE output uses trailing ` \`. | VERIFIED | S2, S10 |

### 1.2 Names and escaping

| Fact | Conf. | Src |
|---|---|---|
| Non-node names must start with a letter or `_`; node names may also be integers, and **leading zeros are significant** in Spectre mode (`01` != `1`). | VERIFIED | S2 |
| Any printable ASCII char (including space, tab) may appear in a name if preceded by `\`. Characters that need it: non-alphanumerics, `\ + - // *`, punctuation. **`!` is the documented exception** (`vdd!` needs no escape). | VERIFIED | S2 |
| Reserved words (do not use as node/instance/model/param names): `M_PI ... P_Q`, `abs acos ... if inline int library local log ... model nodeset parameters paramset plot pow print pwr real return save sens sin ... subckt tan tanh to truncate vary`, plus `ends end else global ic for function export correlate march altergroup statistics`. Full list in source. | VERIFIED | S2 |
| Since 5.0.32 instances, nets, models and parameters live in separate namespaces (a node and an instance may both be `vcc`; `save vcc` picks the node). | VERIFIED | S2 |
| Virtuoso's spectre netlister writes bus bits with escaped brackets: net `in<3>` becomes `in\<3\>`, iterated instance `I25<14:0>` becomes 15 lines `I25\<14\> (...)`, `I25\<13\> (...)` ... `I25\<0\> (...)`. | VERIFIED | S11, S16, S15 |
| Unexpanded buses never appear in a Spectre netlist; every bit is a scalar node. | VERIFIED (Andrew Beckett) | F3 |
| Name mapping is customisable: `hnlSpectreMapNetInName` (in `.simrc`) maps characters in spectre net names, e.g. to strip `!`. The CDL equivalent is `hnlMapNetInName`. Name mapping for spectre changed in IC61x (Cadence article, not readable). | VERIFIED (existence) / exact default table: unknown | F1, S4 |
| Spectre netlister does **not** use the CDF `namePrefix`; the schematic instance name (`M0`, `R3`, `I7`) is written as-is. | VERIFIED | S6, S10 |

### 1.3 Ground and globals

| Fact | Conf. | Src |
|---|---|---|
| `global n1 n2 ...`: the **first** node listed is the ground (datum) node; if no global statement, `0` is ground. Ground is always global. | VERIFIED | S3 |
| Hence ADE writes `global 0` (no other globals) or `global 0 vdd! vss!` (observed). | VERIFIED | S10, S11, F1 |
| The 2011 reference still says "at most one global statement, must be first"; Andrew Beckett states multiple global statements have been legal since MMSIM6.0 (~2004) and the doc was wrong (CCR filed). The 2004 user guide's SPICE-compat notes say "You can now have multiple global statements". Emit **one** statement to be safe. | VERIFIED (both statements exist) | S3, F2, S2 |
| The analogLib ground symbol's net is `gnd!`; the netlister writes it as node `0`. ADE L env `globalGroundNet` (socket netlister) overrides the default analogLib global ground net `gnd!`. An old forum netlist shows `gnd!` surviving as a node name, so the mapping depends on netlisting mode. | LIKELY (direct netlister maps gnd! -> 0) | S5, S17, F5 |
| `vdd!`-style globals keep their `!` in Spectre netlists unless remapped. | VERIFIED | F1 |

### 1.4 Statements

```
subckt NAME [(] t1 ... tn [)]          // ADE writes it without parentheses
parameters p1 p2=default ...           // must be the first line in the subckt; ADE emits pPar-only params with no default
  ...instances / models / nested subckts...
ends [NAME]                            // ADE writes "ends NAME" then "// End of subcircuit definition."
NAME [(] n1 ... nn [)] MASTER p=v ...  // instance; ADE always writes the parentheses
model NAME PRIMITIVE p=v ...
parameters a=1 b=a*2                   // top-level netlist parameters (ADE design variables)
include "file" [section=NAME]
ahdl_include "path/to/file.va"
NAME options k=v ...                   // e.g. simulatorOptions, saveOptions
NAME ANALYSIS [(nodes)] k=v ...        // analyses: tran ac dc noise ...
save sig1 inst:term inst.sub:param ...
```

| Fact | Conf. | Src |
|---|---|---|
| Instance form `name [(]node...[)] master [param=value...]`; first char of the instance name carries no meaning; master may be a primitive (`resistor`, `capacitor`, `vsource`, `isource`, `iprobe`...), a model name, or a subckt. | VERIFIED | S1, S2 |
| Subckt `parameters` give defaults; a subckt inherits all parameters of enclosing scopes (top-level and parent subckt definitions), and a locally re-declared param shadows the inherited one. Params must be declared before use. | VERIFIED | S2 |
| Library file form: `library L` / `section S` ... `endsection [S]` / `endlibrary`-style close `library [L]`; referenced with `include "file" section=S`. | VERIFIED | S2 |
| `include` paths are relative to the including file; `~` and `$ENV` expand in `include` (not `#include`); `-I` adds search dirs. | VERIFIED | S2 |
| ADE emits `ahdl_include "<lib>/<cell>/veriloga/veriloga.va"` automatically for cells bound to a `veriloga` view. | VERIFIED (observed) | S11 |
| ADE emits per-analysis statements, e.g. `tran tran stop=30n ...`, `ac ac start=0.1 stop=10G annotate=status`, `dcOp dc write="spectre.dc" maxiters=150 maxsteps=10000 annotate=status`, `dc dc param=VCM start=0 stop=VDD ...`, `noise ( VOUT 0 ) noise start=1 stop=10G iprobe=V2`, `stb stb ... probe=IPRB0`, `pz ( VOUT 0 ) pz iprobe=V2 ...`. | VERIFIED | S10 |
| ADE emits `simulatorOptions options psfversion="1.4.0" reltol=1e-3 vabstol=1e-6 iabstol=1e-12 temp=27 tnom=27 scalem=1.0 scale=1.0 gmin=1e-12 rforce=1 maxnotes=5 maxwarns=5 digits=5 cols=80 pivrel=1e-3 sensfile="../psf/sens.output" checklimitdest=psf`; **temperature lives in `temp=` here**. | VERIFIED | S10 |
| ADE emits info statements (`modelParameter info what=models where=rawfile`, `element info what=inst ...`, `outputParameter`, `designParamVals`, `primitives`, `subckts`) and `saveOptions options save=selected` (or `allpub`). | VERIFIED | S10 |
| `save` syntax: node `VOUT`; hierarchical node/instance with `.` (`I32.M19`); terminal current by **index** `V8:n`/`I32.M19:1` (positional) unless `options useterms=name`; op-point param `I32.M19:gm`. | VERIFIED | S10, F6 |

### 1.5 Scale factors (exact)

Spectre mode (`lang=spectre`) accepts **only** these SI factors, case-sensitive [VERIFIED S1, S2]:

| T | G | M | K | k | _ | % | c | m | u | n | p | f | a |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1e12 | 1e9 | 1e6 | 1e3 | 1e3 | 1 | 1e-2 | 1e-2 | 1e-3 | 1e-6 | 1e-9 | 1e-12 | 1e-15 | 1e-18 |

- `P` = 1e15 appears in Kundert's table [S1] but not in the 2004 user guide list [S2]. **LIKELY** accepted in modern Spectre; do not emit it.
- **`M` = 1e6, `m` = 1e-3 in Spectre mode** [VERIFIED S1, S2]. In SPICE mode (`lang=spice`) factors are case-insensitive: `t g meg k m mil(25.4e-6) u n p f`, and `M` = `m` = 1e-3 [VERIFIED S2]. The guide's own caution: `1.0M` is 1e-3 in SPICE mode, 1e6 in Spectre mode.
- Units may follow a scale factor and are ignored (`1pf`, `1uH` ok); units without a scale factor are rejected (`50Ohms` is an error; write `50_Ohms`) [VERIFIED S2].
- **`meg` is not a Spectre-mode factor** [VERIFIED: absent from the list S2]. By the unit rule above, `1meg` would read as `1m` + unit `eg` = 1e-3 [GUESS; no source shows Spectre's actual behaviour or a warning]. A Schemify Spectre writer must never emit `meg`.
- Scale factor and exponent cannot be combined: `1.234E-3p` ignores the `p` [VERIFIED S2].
- Data files (PWL `file=`, noise files, S-param) generally do not accept scale factors [VERIFIED S2].

### 1.6 Expressions

| Fact | Conf. | Src |
|---|---|---|
| Expressions are allowed wherever a number is expected on the RHS of `=` (instance, model, analysis params, ic/nodeset) and inside vectors `[ ... ]`; inside a vector any non-trivial expression must be parenthesised: `values=[0.5 1 +p2 (sqrt(p2*p2))]`. | VERIFIED | S2, S3 |
| Operators, highest precedence first: unary `+ -`; `**` (power); `* /`; `+ -`; `<< >>`; `< <= > >=`; `== !=`; `&`; `~^` / `^~` (bitwise XNOR); `|`; `&&`; `||`; `?:`. All left-assoc except `**` and `?:` (right-assoc). Relational/equality return 1/0. **No short-circuit** for `&&`/`||`. | VERIFIED | S2, S3 |
| **`^` is not a power operator** in Spectre (only `**` and `pow()`). A bare `^` (XOR) is not in the documented table. | VERIFIED (absence) | S2, S3 |
| Logical NOT `!` is not in the documented 2004/2011 operator table. | VERIFIED (absence); modern support unknown | S2, S3 |
| Functions: `log log10 exp sqrt min max abs pow sin cos tan asin acos atan atan2 hypot sinh cosh tanh asinh acosh atanh int ceil floor fmod`. `int(x)` = integer part (truncation). Constants `M_PI`, `M_E`, `P_K`, `P_Q`... are reserved names. | VERIFIED | S2 |
| PDK CDF callbacks can produce long expressions written verbatim, e.g. `ad=((1-int(1/2)*2)*(8.5e-08+...))*W_VAR` (shows `int()` in real ADE output). | VERIFIED | S10 |

### 1.7 How ADE assembles a deck (observed layout)

1. Header comments: `// Generated for: spectre`, `// Generated on: <date>`, `// Design library name: L`, `// Design cell name: C`, `// Design view name: schematic` [VERIFIED S10, S11].
2. `simulator lang=spectre`, then `global 0 [globals!]` [VERIFIED].
3. `parameters` with **all design variables** (values as typed, e.g. `VDD=1.9 CLOAD=10p`), plus hoisted analysis-field expressions as `_EXPR_n=...` (e.g. `_EXPR_2=PULSE_WIDTH*10+1u`) [VERIFIED S10].
4. `include "<model file>" section=<sec>` one per Model Library Setup row; `ahdl_include` lines [VERIFIED S10, S11].
5. Subckt definitions, leaf first (define before use), each preceded by `// Library name: / // Cell name: / // View name:` and followed by `// End of subcircuit definition.` [VERIFIED S10, S11].
6. The **top cell is flattened at top level**, not wrapped as a subckt (hspiceD has `setTopLevelAsSubckt`) [VERIFIED S10, S5].
7. Options, analyses, info, `save`, `saveOptions` [VERIFIED S10].
- Netlist path: `<projectDir>/<topCell>/<simulator>/<view>/netlist/input.scs`, projectDir default `~/simulation` [VERIFIED S5]. Maestro: `maeCreateNetlistForCorner(test corner dir)` writes `<dir>/netlist/input.scs` [LIKELY S15].
- Default subckt terminal order observed is ASCII-sorted terminal names (e.g. `GNDA VDDA VINN VINP VOUT`, `A B C VDD VSS Z`), with bus bits in their declared order; CDF `termOrder` (spectre simInfo) overrides for stopping cells [LIKELY S10, S11, S6].
- CDF parameter values go out as stored strings: `w=600n`, `sd=180.0n`, `multi=MOSFET_17_7_M*4`; values stored as floats come out in exponent form (`c=1e-12`, `r=1000`) [VERIFIED S10, S15 sample]. `simPropValueNotation` (Engineering/Scientific) reformats values for socket netlisters [VERIFIED S5].

---

## 2. auCdl (CDL Out) netlist format

### 2.1 Structure (all VERIFIED from Cadence doc S4 and real files S12, F7)

```
************************************************************************
* auCdl Netlist:
*
* Library Name:  <lib>
* Top Cell Name: <cell>
* View Name:     schematic
* Netlisted on:  <date>
************************************************************************

*.BIPOLAR            <- preserveRES/CAP/DIO set
*.RESI = 2000        <- shortRES (+ resistorModel name if set: "*.RESI = 2000 resmod")
*.RESVAL             <- checkRESVAL
*.CAPVAL             <- checkCAPVAL
*.DIOPERI            <- checkDIOPERI
*.DIOAREA            <- checkDIOAREA
*.EQUATION
*.SCALE METER        <- auCdlScale "METER" (default) | "MICRON"
*.MEGA               <- omitted if auCdlSkipMEGA = 't
.PARAM               <- empty, or design vars from top-cell designVarList
*.GLOBAL vdd!        <- only when globals exist; continuation lines "+ vss!"
*.PIN vdd!           <- continuation lines "*+ vss!"

************************************************************************
* Library Name: <lib>
* Cell Name:    <cell>
* View Name:    schematic
************************************************************************

.SUBCKT <cell> <pins...>
*.PININFO a:I b:O c:B
<instances>
.ENDS
```

| Fact | Conf. | Src |
|---|---|---|
| Every cell including the **top** becomes a `.SUBCKT` (a top with no pins prints `.SUBCKT toptest` and an empty `*.PININFO`). | VERIFIED | S4, F8, S12 |
| `.PARAM` lists `designVarList` from the top cell, one per line: `.PARAM CAP=0.8p` / `+ RES=20` / `+ X=35`. | VERIFIED | S4 |
| `*.PININFO name:D` with D in `I O B`; `:P`/`:G` for pins listed in cellview properties `cellViewPowerPins`/`cellViewGroundPins` (ilList). Long PININFO wraps by starting a new `*.PININFO` line (not `+`). Printed only if `displayPININFO = 't`. | VERIFIED | S4, F9 |
| PININFO order observed: inputs, then outputs, then bidirectionals, each group name-sorted (`CLK:I D:I Q:O gnd:B vdd:B`). | LIKELY | S12, F7, F8 |
| Default `.SUBCKT` pin order: ASCII-sorted terminal names, buses expanded in declared order (`in<2> in<1> in<0> out<0> out<1> out<2>`); `auCdlCDFPinCntrl = 't` makes CDF `termOrder` control top/intermediate pin order; `auCdlTermOrderStr` "A"/"D" forces bus bit order. Andrew Beckett recalls the default as "some sorted order". | LIKELY (order rule) / VERIFIED (variables) | S4, F10 |
| Long `.SUBCKT` lines continue with `+ `. | VERIFIED | S18 |
| Bus bits stay as `a<0>` (no escaping) by default; `pinMap = 't` or the CDL Out form option "Map Pin Names from <> to []" applies `<`->`[`, `>`->`]`, and deletes `+ ( ) , / . $ [ ] !`. | VERIFIED | S4, F8 |
| Globals keep `!` in CDL (`CC0 M0 gnd! 1p $[CP]`), and `gnd!` is **not** turned into `0`. `preserveBangInNetlist` exists in si.env. | VERIFIED | S4, S12 |
| `powerNets = '("VCC!")` / `groundNets = '("GND!" "gnd!")` in `.simrc` print `*.GLOBAL VCC!:P GND!:G gnd!:G`. | VERIFIED | S4 |
| Net-name collisions after mapping are renamed with `simNetNamePrefix` (e.g. `M0`); `simCheckNetCollisionAction` = `"warning"` / `"error"`. Odd names like `N$1158` appear for some generated nets. | VERIFIED | S4 |

### 2.2 Instance lines

Netlister prefixes the schematic instance name with the CDF `namePrefix` (M, R, C, L, D, Q, J, X) [VERIFIED S4, S6]: `NM0` -> `MNM0`, `M0` -> `MM0`, `I0` -> `XI0`, `R0` -> `RR0`.

| Device | Example (from Cadence doc) | Src |
|---|---|---|
| MOS | `MM1 g d gnd! gnd! nmos.mod W=3 L=2 M=2` | S4 |
| MOS (real PDK) | `MPM24 CLKA CLKB vdd vdd g45p1svt m=1 l=45n w=600n` | S12 |
| Subckt call | `XI0 A B / mid` ; with params `XC1 out gnd! / umim cl=20 cw=20` | S4, F11 |
| 2-term cap | `CC5 n3 gnd! 1p $[CP] M=10` | S4 |
| 3-term cap | `CC32 n5 gnd! 1p $[CP] $SUB=n5 M=3` | S4 |
| Resistor | `RR3 n1 n4 1000 $SUB=gnd! $[RP] $W=20 $L=100 M=3` | S4 |
| Diode | `DD6 a gnd! DP 10 3 M=12` | S4 |
| BJT | `QQ4 c b gnd! NP M=12 $EA=100 $W=4 $L=3` | S4 |
| FET | `JJ7 d g gnd! fet.mod W=3 L=2 M=2` | S4 |

- `$[model]` carries the simInfo `modelName` (device "subtype") for R/C/L; `$SUB=` the substrate net; `dollarParams` print `$value`, `dollarEqualParams` print `$name=value`; `TSMCMODEL` instParam prints `$.MODEL=LP` [VERIFIED S4].
- Instance params are those in auCdl simInfo `instParameters`, mapped by `propMapping`; **case and order follow the PDK CDF** (`W= L= M=` vs `m= l= w=`) [VERIFIED S4, S12].
- **Values are printed as stored, no unit conversion**: `W=128e-6 L=8u`, `10e3`, `perimpd=150.00000u` all appear [VERIFIED S4, S18]. `*.SCALE METER` declares that bare numbers are metres; `*.MEGA` makes `M` mean 1e6 instead of milli for LVS [LIKELY, Dracula LOGLVS ref via search summary S19].
- Netlist procedures: `ansCdlCompPrim` (primitives FET/CAP/IND/DIODE/BJT/RES/MOS), `ansCdlSubcktCall` (prints `X... nets / cell params`), `ansCdlHnlPrintInst` [VERIFIED S4, F12]. The CDL Out form's "Analog Netlisting Type" (`auCdlDefNetlistProc`) chooses `ansCdlSubcktCall` (connect by order) or `ansCdlHnlPrintInst` (connect by name) [LIKELY S20].
- pPar in CDL: inner expression keeps the bare parameter name (`CC0 PLUS MINUS (cw)*(cl) $[CA]`), the `.SUBCKT` line carries **no** parameter defaults, and the caller passes `cl=20 cw=20`; LVS resolves via `*.EQUATION`/`.PARAM` [VERIFIED forum netlist F11; BTSW example shows `l=LA w=WA` S18].
- `*.EQUATION`: LOGLVS evaluates and passes parameter expressions down the hierarchy (needs `.PARAM`) [LIKELY S19].
- Inherited connections become extra ("pseudo") `.SUBCKT` ports, observed as `inh_dig_vdd`/`inh_dig_vss` in CDL and `vdd_inh`/`gnd_inh` in a spectre netlist. `simPrintInhConnAttributes = t` suppresses pseudo ports and prints `*.NETEXPR prop_name netName default_signal` and `$netSet prop_name = value` instead [VERIFIED S4, F7, F13]. The exact pseudo-port naming rule is unknown.
- `*.CONNECT`: si.env has a `connects` variable (observed empty) [VERIFIED existence S21], and `setEQUIV`; I found no source showing the `*.CONNECT` line auCdl prints [GUESS: `*.CONNECT net1 net2`].
- `*.BUSDELIMITER` (Dracula/Calibre: tells LVS which bracket delimits bus bits: `[`, `<` or `{`) is **not** in any auCdl sample I found; it appears in CDL from other tools [LIKELY auCdl does not emit it by default; S19, S22].
- Black box: cellview boolean property `auCdlPrintBlkBox = t` prints only the interface (`.SUBCKT res2 B C` / `*.PININFO` / `.ENDS`); `auCdlDisableBlkBox`. `CDL_NETLIST_FILE` property on an auCdl view + `auCdlEnableNetlistInclusion = 't` concatenates a text netlist [VERIFIED S4].

### 2.3 Batch netlisting with `si`

```
# auCdl (VERIFIED S4, S12, S20)
si -batch -command netlist [-cdslib ./cds.lib]
# spectre (LIKELY S15: "-command nl"; "netlist" gives OSSHNL-510 for spectre)
si -batch -cdslib ./cds.lib -command nl
```

`si.env` keys seen in a real auCdl run dir [VERIFIED S12] and in the Cadence doc [S4]:

```
simLibName = "DETFF"           simCellName = "TGFF"         simViewName = "schematic"
simSimulator = "auCdl"         simNotIncremental = 'nil     simReNetlistAll = 'nil
simViewList = '("auCdl" "schematic")   simStopList = '("auCdl")
cdlSimViewList / cdlSimStopList (same)  simNetlistHier = t   hnlNetlistFileName = "netlist"
resistorModel = ""  shortRES = 2000.0  preserveRES/CAP/DIO/ALL = 't
checkRESVAL checkRESSIZE checkCAPVAL checkCAPAREA checkCAPPERI checkDIOAREA checkDIOPERI checkLDD
checkScale = "meter"  shrinkFACTOR = 0.0  displayPININFO = 't  pinMAP = 'nil
globalGndSig = ""  globalPowerSig = ""  incFILE = ""  setEQUIV = ""
preserveBangInNetlist = 'nil  simPrintInhConnAttributes = 'nil
auCdlDefNetlistProc = "ansCdlSubcktCall" | "ansCdlHnlPrintInst"   (S20, LIKELY)
simRunDir = "<dir>"
```

- `simInitEnvWithArgs(runDir lib cell view "spectre" nil)` writes a spectre `si.env` from the CIW [LIKELY S15]; spectre si.env adds `nlDesignVarNameList` [LIKELY S15].
- `.simrc` search order: `$SIMRC/.simrc`, `$ossSimUserSiDir/.simrc`, `dfII/local/.simrc`, `./.simrc`, `~/.simrc`; first found wins [VERIFIED S4].
- `.simrc` auCdl knobs: `auCdlCDFPinCntrl`, `auCdlScale`, `auCdlCheckLDD`, `auCdlSkipMEGA`, `auCdlSetTopLevelEvalMode` (nil = inheritance operators, t = full AEL eval), `cdlNetlistType 'hnl|'fnl`, `cdlPrintComments`, `pinMap`, `hnlMapNetInName`, `simNetNamePrefix`, `simCheckNetCollisionAction`, `hnlReadHdbProps`/`ansCdlHdbFilePathProp` [VERIFIED S4].
- `CDS_Netlisting_Mode=Analog` must be set for CDF-based netlisting [VERIFIED S4, F14].
- Third-party report: `si` auCdl export outside a Virtuoso session may miss SKILL callbacks; a newer Pegasus path (`pvsBuildNetlist`) emits a terser CDL (`*.EXPAND_ON_M_FACTOR`, `*.MEGA`, lowercase `.subckt`) [LIKELY S15, S23].

---

## 3. Name and net-expression syntax in Virtuoso schematics

Primary source: Virtuoso Schematic Composer User Guide (4.4.6 / 5.0) [S7, S8]. Later IC6 releases rewrote the extractor (ICADVM20.1) but aim for compatibility [F15].

### 3.1 Grammar (VERIFIED S8 page 32, S7)

```
nameExpr ::= [ <*n> ] nameTerm { , nameExpr }        // ',' bundles; <*n> prefix repeat
nameTerm ::= baseName [ < vectExpr > ] | ( nameExpr )
vectExpr ::= vectTerm [ *n ] { , vectExpr }          // '*n' suffix repeat (each bit)
vectTerm ::= n [ : n [ : n ] ] | ( vectExpr )        // start:stop:step, step > 0
instName ::= baseName [ < n [ : n ] > ]              // n < 16,375
```

| Example | Expansion | Src |
|---|---|---|
| `b<0:2>` | b<0>,b<1>,b<2> | S8 |
| `DATA<2:0>` | descending: DATA<2>,DATA<1>,DATA<0> | S7 |
| `DATA<1:7:2>` / `b<3:0:2>` | step: <1,3,5,7> / <3,1> (step always positive; direction from bounds) | S7, S8 |
| `DATA<2,1,0>` | explicit list | S7 |
| `<*2>A,B,C` | A,A,B,C | S7 |
| `<*2>(A,B),C` | A,B,A,B,C | S7 |
| `<*2>(A,<*2>(X,Y)),B` | A,X,Y,X,Y,A,X,Y,X,Y,B (inner first) | S7 |
| `A<0:2*2>` | A<0,0,1,1,2,2> (suffix repeat each bit) | S7 |
| `A<(0:2)*2>` | A<0,1,2,0,1,2> (repeat the sequence) | S7 |
| `b<0,1:3:4*1,2:2>` | b<0,1,2> | S8 |
| `J20<0:1>` (instance) | J20<0>, J20<1>; `K30<2:1>` -> K30<2>, K30<1> | S8 |

- Reserved in wire/net/pin names: `, < > ( ) * : !`; `/` is illegal in net names. Instance names: `< > :` reserved; `, ( ) * : /` illegal. Names up to 1024 chars; surrounding blanks ignored [VERIFIED S8].
- Member n of an expression = zero-based index into the expanded list [VERIFIED S8].
- A tap wire named only by indices (`<3:0>`, `<0>`) attached to a named bus takes the bus base name; taps cascade; tap bit order follows the tap name (`DATA<3:0>` tapping `DATA<0:3>` reverses) [VERIFIED S8]. ICADVM20.1 briefly broke base-less taps with repeat (`<1*3,2*3,3*3>`), fixed in IC6.1.8 ISR25 [VERIFIED F15].
- A bundle is a comma list (`S,R`); a bus is one base name with a vector (`Q<0:15>`) [VERIFIED S8].

### 3.2 Globals

- Trailing `!` makes a net global across the hierarchy, without pins: `gnd!`, `vdd!`; a global bus puts `!` on the base name: `A!<0:3>`. Connecting to a global pin/instance pin makes the net global and it takes that name [VERIFIED S8].
- A net that is both an explicit pin and a differently named global is a short (illegal); workaround is `basic/cds_thru` or a 0 V source [LIKELY F16, F17].

### 3.3 Iterated instances: width rules (VERIFIED S8 page 24)

Connection width = `iterations * pinWidth`. For a wire on a pin of `I<0:2>` with pin `P<0:2>`:

1. Net width == connection width (9: `N<0:8>`): bits distributed in order, instance member 0 first: `N<0>`->`P<0>` of `I<0>`, `N<1>`->`P<1>` of `I<0>`, `N<2>`->`P<2>` of `I<0>`, `N<3>`->`P<0>` of `I<1>`, ...
2. Net width == pin width (3): editor adds `<*3>(N<0:2>)` so every iteration gets the whole bus.
3. Net width == 1: editor adds `<*9>N` (all bits tied to N).
4. Otherwise: error at check time.

Here "member 0" is the first name in the expanded instance list, so for `I25<14:0>` member 0 is `I25<14>`; a real netlist shows `I25\<14\>` getting `net021\<0\>` [VERIFIED S11]. Series chains via shifted buses: `IN,internal<127:1>` -> `RES<128:1>` -> `internal<127:1>,OUT` [VERIFIED F18].

### 3.4 Unnamed nets and multiple names

- Unnamed intersecting wires form one net; its name is chosen from intersecting named nets (wire names, schematic pins, tap names), picking the name with the **most unique members**; at every intersection all names must be members of that name, else error [VERIFIED S8 page 26].
- A wire attached to a pin with no label takes the pin name [VERIFIED S7].
- Two different scalar names on one net -> "Short between X and Y" style check error (message text seen in IC20.1) [LIKELY F15, S8].
- System-generated names use the editor's "Net Name Prefix" (Options - Editor); observed generated names: `net1`, `net02`, `net011`, `net80`, `net021<0:14>` for a bus feeding an iterated instance [VERIFIED observed S10, S11, S12; numbering algorithm unknown].
- Default instance names: prefix `I` + counter from 0 (`I0`, `I1`), prefix changeable [VERIFIED S8].

### 3.5 Inherited connections (VERIFIED S8 pages 30-31 unless noted)

- Net expression: `[@propName:%:default!]`, e.g. `[@power:%:vdd!]`. Components: search scope `@`, property name, format `%`, default global. Displayed with a trailing `*`.
- Placed as a label on a wire, or on a symbol pin (propagated to the net at check time). `basic` library: `vdd_inherit [@vdd:%:vdd!]`, `gnd_inherit [@gnd:%:gnd!]`, `vcc_inherit`, `vss_inherit`.
- Evaluation per hierarchical path: walk up one instance at a time; the **first** instance with a property of that name (type `netSet`) wins; its value must be an existing scalar net in that parent cellview, or another net expression (redirect, which may change the property name/default below). No override found -> default global. A lower-level netSet always beats a higher one for its subtree.
- Restrictions: default must be a scalar global (`vdd!` ok, `vdd`/`bus!<0:15>` not); one cellview cannot use two different expressions for the same default global (`[@power:%:vdd!]` and `[@vdd:%:vdd!]` together is illegal).
- netSet values name the net **without** `!` when overriding with a local net (forum fix: the property name is `vdd`, not `vdd!`) [VERIFIED F13].
- Three styles in use: explicit supply pins; net expressions on wires (no symbol pins, override by netSet); inherited terminals (expression on a real pin, may be wired or netSet) [LIKELY F16].
- Netlisters add pseudo ports for inherited nets (see §2.2).

### 3.6 Schematic checks relevant to export (VERIFIED S8 pages 117-118, 223, 165)

Logical: Floating Nets, Floating Input/Output/IO/Switch Pins, **Shorted Output Pins**, Offsheet Connectors. Physical: Unconnected Wires, Solder On CrossOver, Overlapping Instances, Max Label Offset. Name: Connection By Name, Instance/Net Name Collision, Pin/Net Name Collision (pin connected to differently named wire), Verilog/VHDL syntax, regex name checks. Inherited: Inherited Pin/Net Connections, Missing Override Nets. Env vars `srcFloatingNets` etc. default `"warning"`, `srcConnByName` `"ignored"`. Netlisters refuse to run until the design is checked [VERIFIED S8].

---

## 4. Parameter values and hierarchy

### 4.1 Inheritance functions (VERIFIED S6)

| Function | NLP syntax | Meaning |
|---|---|---|
| `pPar("x")` | `[+x]` | value of `x` on the **parent instance**; falls back to the parent cell's effective CDF default. At top level: top schematic property list, then top cell CDF default (that default may only use iPar/dotPar). |
| `iPar("x")` | `[~x]` | another parameter on the **same instance** (falls back to the cell CDF default). Mutual iPar loops crash (documented segfault example w/l). |
| `atPar("x")` | `[@x]` | searches parent, grandparent ... then instance master, then simulator `nlpglobals`. Discouraged. |
| `dotPar("x")` | `[.x]` | like iPar at leaf; from higher levels searches down to the leaf being netlisted. Discouraged, unsupported in several netlisters. |

- Callbacks are **not** triggered when an inherited value or a design variable changes [VERIFIED S6].
- Bare identifiers in a CDF value that are not parameters are **design variables**, resolved by ADE (Variables - Copy From/To cellview; stored on the cellview as `designVarList` = `(("CAP" "0.8p") ...)`) [VERIFIED S5, S4].

### 4.2 Spectre output of parameterised hierarchy (VERIFIED S11 sample unless noted)

- `pPar("fnnn")` on a device inside cell `NOR3` netlists as the bare name: `M0 (Z A VSS VSS) nmos w=400n l=65n m=fnnn`; the cell becomes `subckt NOR3 ... ` + `parameters fnnn fppp` (no defaults when none apply; with CDF defaults you get `name=default`, e.g. `parameters ... fin_battery_cap=80`).
- The caller passes values or expressions: `I6 (...) NOR3 fnnn=nor3 fppp=3*nor3` where `nor3` is a design variable in the top `parameters` line.
- Expressions are written unquoted (`w=w_sw_cas*2`, `stop=VDD`); string-typed simulator params are quoted (`psfversion="1.4.0"`, `write="spectre.dc"`) [VERIFIED S10]. CDF `stringParameters` lists params forced to strings [VERIFIED S6].
- "During netlisting, the pPar expression is substituted by the name of the parameter"; "The iPar expression is substituted with the value of the parameter, enclosed by parentheses" (error if no value found); circular references are reported at netlist time [VERIFIED S4 pp.127-129, consistent with S11]. Automatic symbol generation turns pPar names into component (CDF) parameters of the generated symbol [VERIFIED S4].
- Design-variable and pPar names must not collide with simulator reserved words (Spectre reserved list, simulator options, CDF field names) [VERIFIED S4].
- `CDS_Netlisting_Mode` default: `Analog` when started with `icms`/`msfb`, otherwise `Digital` [VERIFIED S4].
- CDF value with units: stored as a string such as `"1u"`, `"10f"`, `"1K"` and written verbatim, so the Spectre SI rules apply (`M` = mega). CDF unit scale factors offered: e.g. resistance `M` = MOhm [VERIFIED S6]. A value typed `1meg` would be passed through unchanged [GUESS].
- Model name: CDF param `model` wins; else simInfo `componentName`; else cell name [VERIFIED S6]. `termOrder`, `termMapping` (`(nil D \:d G \:g ...)`), `propMapping (nil simName cdfName ...)`, `instParameters`, `otherParameters (model bn)` control the line [VERIFIED S6].

### 4.3 CDL output of the same

pPar names stay symbolic, subckt line has no defaults, caller passes values (§2.2). `auCdlSetTopLevelEvalMode` chooses inheritance-operator evaluation (default) vs full evaluation for design variables [VERIFIED S4]. Cadence's own auCdl example uses `m = pPar("m")` and `AD = iPar("w")*5u` [VERIFIED S4].

---

## 5. ADE testbench state and SKILL APIs

### 5.1 Product status

ADE L is end-of-life: not in ICADVM20.1 or IC23.1; ADE Explorer/Assembler (maestro view) replace it [VERIFIED F19, F20]. ADE XL used an `adexl` view (results path `.../adexl/results/data/...` seen in an old state) [VERIFIED observed S14].

### 5.2 ADE L SKILL (`asi*` / `sev*`)

```
sess = asiGetCurrentSession()                     ; or sevSession(hiGetCurrentWindow())
asiGetDesignVarList(sess)        => (("VDD" "0.9") ("CL" "1p") ...)
asiSetDesignVarList(sess list(list("var1" "20") ...))   ; replaces all
asiAddDesignVarList(sess list(list("var1" "20") ...))   ; adds
asiGetModelLibSelectionList(sess)
asiAddModelLibSelection(sess "file.scs" "NN")           ; file + section
asiRemoveAllModelLibSelection(sess)
asiSetEnvOptionVal(asiGetTool('spectre) "modelFiles" list(list("/m/a.scs") list("#" "/m/b.scs")))  ; "#" disables
asiSaveState(sess)   ; only works from an interactive ADE (sev) session, not plain OCEAN
```
Sources: [VERIFIED F19, F21, S5, F22].

OCEAN (batch-friendly) [VERIFIED S9]: `simulator('spectre)`, `design("lib" "cell" "view")` or `design("netlist")`, `desVar("rs" 1k)` / `desVar("r2" "rs*2")`, `modelFile("bjt.scs" '("nmos.scs" "typ"))` => `(("bjt.scs" "") ("nmos.scs" "typ"))`, `temp(125)`, `analysis('tran ?stop 1u ?step 10n)`, `analysis('ac ?start 1 ?stop 10000 ?lin 100)`, `analysis('dc ?saveOppoint t)`, `analysis('noise ?start 1 ?stop 10e6 ?oprobe "V4")`, `save('v "net1")`, `option('abstol 1e-10)`, `envOption('paramRangeCheckFile "...")`, `createNetlist(?recreateAll t)`, `run()`, `resultsDir()`.

### 5.3 Maestro (ADE Explorer/Assembler) `mae*`

Forum-verified calls (exact text in Cadence-community posts by users and Andrew Beckett):
```
sess = maeOpenSetup("myLib" "myCell" "maestro")              ; returns e.g. "fnxSession0"; batch, no window, takes edit lock
maeCreateTest("myTest1" ?lib "myLib" ?cell "myCell" ?view "schematic" ?simulator "spectre" ?session sess)
maeSetAnalysis("myTest1" "dc" ?enable t ?options '(("saveOppoint" t)) ?session sess)
maeSetVar("myVar" "myValue" ?typeName "test" ?typeValue '("myTest1") ?session sess)
maeAddOutput("name" "test" ?outputType "point" ?expr "average(VT(\"/net\"))")
maeRunSimulation(?session sess)
maeCloseSession(...)                                          ; releases the lock
geOpen(?lib "L" ?cell "C" ?view "maestro")                    ; interactive open instead
```
[VERIFIED F23, F24, F25]. Known bug (CCR 1949611): `maeSetAnalysis` ignored `?session` and acted on the latest session [VERIFIED F23].

Additional signatures from a third-party (MIT) automation project, tested on IC6.1.8 / IC23.1 [LIKELY S15]:
```
maeOpenSetup(lib cell "maestro" ?mode "a")
maeGetSetup([?typeName "tests"|"corners"|"variables"|"parameters"] [?session s])
maeGetEnabledAnalysis(test)   maeGetAnalysis(test "pss") => (("fund" "1G") ...)
maeSetAnalysis("TRAN" "tran" ?enable t ?options `(("stop" "60n") ("errpreset" "conservative")))
maeSetAnalysis("AC" "ac" ?enable t ?options `(("start" "1") ("stop" "10G") ("incrType" "Logarithmic") ("stepTypeLog" "Points Per Decade") ("dec" "20")))
maeAddOutput("OutPlot" "TRAN" ?outputType "net" ?signalName "/OUT")
maeAddOutput("maxOut" "TRAN" ?outputType "point" ?expr "ymax(VT(\"/OUT\"))")
maeSetSpec("maxOut" "TRAN" ?lt "400m")     ; also ?gt ?minimum ?maximum ?tolerance
maeSetVar("vdd" "1.3")                      ; global var; "1p,100f" = sweep list
maeGetVar("vdd") => "1.3"
maeSetCorner("tt_25" ?enabled t)            ; only ?enabled / ?disableTests accepted
maeSetVar("temperature" "25" ?typeName "corner" ?typeValue '("tt_25") ?session s)
sdb = axlGetMainSetupDB(s)  corn = axlGetCorner(sdb "tt_25")
model = axlPutModel(corn "pdk.scs")  axlSetModelFile(model "/p/pdk.scs")  axlSetModelSection(model "tt")
maeGetEnvOption("TRAN" ?option "modelFiles") => (("/p/model.scs" "tt") ...)
maeSetEnvOption("TRAN" ?options `(("modelFiles" (("/p/model.scs" "tt")))))
maeSaveSetup(?lib L ?cell C ?view "maestro" ?session s)
maeRunSimulation([?session s] [?callback "procName"])  maeWaitUntilDone('All)
maeOpenResults(?history "Interactive.2")  maeGetOutputValue("maxOut" "TRAN2" ?cornerName "c")
maeGetSpecStatus  maeExportOutputView(?fileName f ?view "Detail")  maeCloseResults()
maeCreateNetlistForCorner("TRAN2" "myCorner_2" "./dir")   ; -> ./dir/netlist/input.scs
maeWriteScript("setup.il")    ; dumps the whole setup as a replayable SKILL script
maeMigrateADELStateToMaestro("lib" "cell" "spectre_state1")
maeMigrateADEXLToMaestro("lib" "cell" "adexl" ?maestroView "maestro_convert")
maeDeleteCorner  maeMakeEditable  maeGetSessions
```
Cadence publishes a RAK "Using mae SKILL with Maestro Views" (tested IC23.1) [VERIFIED existence S24]. `maeWriteScript` is the best single target for a bridge to read or write: replaying a script avoids editing XML.

Per-test model files can also be set via `asiSetEnvOptionVal(asiGetSession(axlGetToolSession(session test)) "modelFiles" list(list("$PDK/toplevel.scs" "tt_mos") ...))` [VERIFIED F26].

### 5.4 State on disk

**Maestro view** `<lib>/<cell>/maestro/` [VERIFIED observed S13; layout described in S15]:
- `maestro.sdb`: XML `<setupdb version="6|7">maestro`, `<active>Active Setup` with `<tests><test [enabled="1"]>NAME` + `<tool>ADE</tool>` + `<tooloptions>` (`cell`, `lib`, `sim`=spectre, `view`=schematic|config, `path`=`$AXL_SETUPDB_DIR`, `state`=active) + per-test `<vars><var>NAME<value>V</value></var>`; `<corners><corner enabled="1">_default</corner>` and named corners carrying `<vars>` (including `temperature`) and `<models><model enabled="1">file.scs<modeltest>All</modeltest><modelblock>Global</modelblock><modelfile>/path</modelfile><modelsection>"tt" "sf" "fs"</modelsection></model>`; `<currentmode>Single Run, Sweeps and Corners</currentmode>`, `<history>`. Default ADE Explorer test name: `<lib>_<cell>_1`.
- `active.state`: XML `<statedb ICVersion="IC23.1-64b.ISR9.24" version="5">active`, one `<Test Name="...">` with `<component Name="adeInfo">` (`designInfo` = `("lib" "cell" "schematic" "spectre")`), `analyses` (`<analysis Name="tran">` with fields such as `stop` `"350u"`, `enable` `(t)`), `modelSetup` (`modelFiles` = `(("/path/models" ""))`), `outputs` (`sevOutputStruct` list), `simulatorOptions`, `environmentOptions`, ...
- Sweep value syntaxes seen: `"0.8,0.9,1.0"` (list), `"0.81:0.09:0.99"` (start:step:stop), `"Linear:0:1:5"` [VERIFIED observed S13, S15; exact semantics of `Linear:` form unknown].
- Also `data.dm` (binary prop bag, do not edit), `master.tag`, `test_states/`, `results/maestro/Interactive.N*` (SQLite `.rdb`/`.msg.db`), `maestro.sdb.cdslck` lock [LIKELY S15].

**ADE L state**: directory `<saveDir>/<lib>/<cell>/<simulator>/<stateName>/`, `saveDir` default `~/.artist_states`; or, with `saveAsCellview`, a cellview `<lib>/<cell>/<stateName>/` (commonly `spectre_state1`) [VERIFIED S5]. Files (one per "What to Save" item) [VERIFIED observed S14]: `ADE_state.info` (`projectDir`, `designInfo = '("Lib" "Cell" "schematic" "spectre")`), `analyses.state` (`analysis(tran fields stop) "30n"` lines), `variables.state` (SKILL that builds `sevVariableStruct`s: `tmp1->name = "cl" tmp1->expression = "10f" tmp1->index = 2`), `modelSetup.state` (`(("/path/toplevel.scs" "top_tt"))`), `outputs.state` (`sevOutputStruct`s), `environmentOptions.state` (`(envOpts switchViewList) ("spectre" "cmos_sch" "cmos.sch" "schematic" "veriloga")`, `(envOpts stopViewList) ("spectre")`), `simulatorOptions.state` (`(opts gmin) "1e-12"`), `convergence.state`, `simulationFiles.state`, `graphicalStimuli.state`, etc.

**Env defaults** [VERIFIED S5]: `envSetVal("asimenv.startup" "projectDir" 'string "~/simulation")`, `envSetVal("spectre.envOpts" "modelFiles" 'string "a.scs b.scs")` (`#` prefix disables), `asimenv stateName` autoload.

---

## 6. Prior interchange routes

| Route | Facts | Conf. | Src |
|---|---|---|---|
| **EDIF 200 Out** | CIW File - Export - EDIF 200; template file (sample `<install>/tools/dfII/samples/xlUI/edifOut.il`) loaded/saved in the form; runs the `edifout` translator, log `edifout.log`; error messages are terse. | VERIFIED | S25, F27 |
| **EDIF 200 In** | CIW File - Import - EDIF 200 (also EDIF 300); runs `edifin` (license "edifin (940)"), log `edifin.log`; CRLF line endings break the parser; scaling fixed via "Set Schematic DBUPerUU" on the import form. Andrew Beckett: works for S-Edit exports with pre/post-processing; the only alternative is SPICE In, which loses the drawing. | VERIFIED | F28 |
| EDIF 200 general limits | Ambiguous bus representation, awkward rippers, no view-wide connectivity, no borders/title blocks, weak pin spacing/grid/text support; imports usually need re-symbolising. Other vendors map `GND!` -> `GND` on import. | LIKELY | S26 (search summary), S27 |
| **SPICE In** (`spiceIn`) | GUI File - Import - Spice, or `spiceIn -param <file>` with `spiceInParams = list(nil 'language "CDL"|"SPICE"|"HSpice"|"Spectre" 'netlistFile f 'outputLib L 'refLibList "analogLib basic ..." 'devMapFile "dev.map" 'overwriteCells "all" 'conn2schArgs "-MIN_CROSSOVERS -FAST_LABELS")`. Device map lines: `devSelect := resistor rm2`, `propMatch := subtype M2` (CDL `$[M2]`), `termOrder := PLUS MINUS`, `propMap := w w l l`, `addProp := model rm2`. CDL params arrive upper-case. Command-line runs do **not** trigger CDF callbacks (GUI can). `overwriteCells "all"` ignores refLib symbols except primitives. Generated schematics are auto-placed, not the original drawing. | VERIFIED | F29, F30, F31, S15 |
| CDL bus pins on import | CDL subckt pins must be scalar; `input<2:0>` works only by accident; legacy CDL `.SUBCKT blk a b / c d` puts outputs before `/`, inputs after. | VERIFIED (Andrew Beckett) | F32 |
| **Verilog In** | File - Import - Verilog; target lib, ref libs, `-f`, `-y`; structural modules -> schematic, all modules -> symbol, behavioural -> `functional` view; `a[31:0]` -> bus `a<31:0>`; supply pins absent from std-cell Verilog cause pin-count warnings. | VERIFIED | S28, S29 |
| SKILL bridges | Open tools drive Virtuoso through SKILL over IPC (e.g. virtuoso-bridge-lite, MIT; BAG). Practical bridge: emit CDL -> `spiceIn`; read back via SKILL DB (`cv~>instances`, `inst~>instTerms`, `instTerm~>net~>name`, `cv~>terminals`) or via `maeCreateNetlistForCorner`. | VERIFIED (that these are used) | S15 |

---

## 7. Illustrative netlists (my own words, conventions from §1-§2)

Design: library `demo`. Leaf `inv` (pins `A` in, `Y` out) uses PDK `nch`/`pch`, sizes via `pPar("wn")`/`pPar("wp")`, supplies are globals `vdd!`/`gnd!`. Mid cell `buf4` has bus pins `in<3:0>`, `out<3:0>` and one iterated instance `I0<3:0>` of `inv` with `wn=wn0`, `wp=2*wn0` (design variable `wn0`). Testbench `tb_buf4` instantiates `buf4` as `I1`, drives `in<3:0>` and has a supply source.

### 7.1 Spectre (ADE style)

```
// Generated for: spectre
// Generated on: Oct 12 10:00:00 2026
// Design library name: demo
// Design cell name: tb_buf4
// Design view name: schematic
simulator lang=spectre
global 0 vdd!
parameters wn0=1u vsup=1.8 cl=10f
include "/pdk/models/spectre/toplevel.scs" section=tt

// Library name: demo
// Cell name: inv
// View name: schematic
subckt inv A Y
parameters wn wp
    M1 (Y A vdd! vdd!) pch w=wp l=180n m=1
    M0 (Y A 0 0) nch w=wn l=180n m=1
ends inv
// End of subcircuit definition.

// Library name: demo
// Cell name: buf4
// View name: schematic
subckt buf4 in\<3\> in\<2\> in\<1\> in\<0\> out\<3\> out\<2\> out\<1\> out\<0\>
    I0\<3\> (in\<3\> out\<3\>) inv wn=wn0 wp=2*wn0
    I0\<2\> (in\<2\> out\<2\>) inv wn=wn0 wp=2*wn0
    I0\<1\> (in\<1\> out\<1\>) inv wn=wn0 wp=2*wn0
    I0\<0\> (in\<0\> out\<0\>) inv wn=wn0 wp=2*wn0
ends buf4
// End of subcircuit definition.

// Library name: demo
// Cell name: tb_buf4
// View name: schematic
I1 (in\<3\> in\<2\> in\<1\> in\<0\> out\<3\> out\<2\> out\<1\> out\<0\>) buf4
V0 (vdd! 0) vsource dc=vsup type=dc
V1 (in\<0\> 0) vsource type=pulse val0=0 val1=vsup period=2n
C0 (out\<0\> 0) capacitor c=cl
simulatorOptions options reltol=1e-3 vabstol=1e-6 iabstol=1e-12 temp=27 \
    tnom=27 gmin=1e-12
tran tran stop=20n errpreset=moderate annotate=status
save out\<0\> in\<0\>
saveOptions options save=selected
```

Notes: `gnd!` became `0`; `vdd!` stays a global; the inv source of `pch` uses a global without a port; `wn0` resolves inside `buf4` through Spectre's parameter inheritance from top level. Whether ADE lists `I0\<3\>` first (declared order) is per §3.3 [LIKELY]; the subckt port order is the sorted default [LIKELY].

### 7.2 auCdl

```
************************************************************************
* auCdl Netlist:
*
* Library Name:  demo
* Top Cell Name: buf4
* View Name:     schematic
* Netlisted on:  Oct 12 10:00:00 2026
************************************************************************

*.BIPOLAR
*.RESI = 2000
*.RESVAL
*.CAPVAL
*.DIOPERI
*.DIOAREA
*.EQUATION
*.SCALE METER
*.MEGA
.PARAM wn0=1u

*.GLOBAL vdd!
+        gnd!

*.PIN vdd!
*+    gnd!

************************************************************************
* Library Name: demo
* Cell Name:    inv
* View Name:    schematic
************************************************************************

.SUBCKT inv A Y
*.PININFO A:I Y:O
MM1 Y A vdd! vdd! pch W=wp L=180n m=1
MM0 Y A gnd! gnd! nch W=wn L=180n m=1
.ENDS

************************************************************************
* Library Name: demo
* Cell Name:    buf4
* View Name:    schematic
************************************************************************

.SUBCKT buf4 in<3> in<2> in<1> in<0> out<3> out<2> out<1> out<0>
*.PININFO in<3>:I in<2>:I in<1>:I in<0>:I out<3>:O out<2>:O out<1>:O
*.PININFO out<0>:O
XI0<3> in<3> out<3> / inv wn=wn0 wp=2*wn0
XI0<2> in<2> out<2> / inv wn=wn0 wp=2*wn0
XI0<1> in<1> out<1> / inv wn=wn0 wp=2*wn0
XI0<0> in<0> out<0> / inv wn=wn0 wp=2*wn0
.ENDS
```

Notes: top cell is a `.SUBCKT`; `gnd!` is kept; `.PARAM wn0=1u` only appears if `designVarList` was copied to the top cell [VERIFIED rule S4]; `XI0<3>` naming of iterated instances in CDL and whether `wn=`/`wp=` are printed on the `X` line (needs the cell's auCdl `instParameters` or default subckt-call behaviour) are [GUESS].

---

## 8. Implications for Schemify (short)

- Spectre writer: `simulator lang=spectre` first; `global 0 <globals>` as one statement; escape `<`, `>` and other non-alnum except `!` with `\`; never emit `meg` (use `M`) or `^`; power is `**`/`pow`; bus bits expanded per bit; iterated instances expanded per member in declared order; top cell flat; subckt params from pPar emitted without defaults; design variables as top `parameters`; corners as `include ... section=` + `temp=` in `options`.
- CDL writer: header + directives block above, top as `.SUBCKT`, `*.PININFO` (I/O/B), `X... / cell`, keep `!` globals and `<>` bits raw, values verbatim with `*.SCALE METER` + `*.MEGA`, CDF-style namePrefix (`M`+name).
- Reading Virtuoso output: map `0` back to the ground global; strip `\` escapes; rebuild buses from `name<i>` bits; rebuild iterated instances from `I\<k\>` runs; pPar names = subckt parameters without default.
- Testbench exchange: prefer generating a `maeWriteScript`-style SKILL script (or OCEAN script) over editing `maestro.sdb`; read `maestro.sdb`/`active.state` XML for import.

---

## 9. Open unknowns (need goldens from a real Virtuoso run)

1. Spectre-mode handling of `1meg` (silent 1e-3? warning? error?) in current Spectre (21.x+).
2. Exact default spectre/auCdl subckt port-order rule (sorted? symbol pin order? `port_order "sch"` env option) and CDL PININFO ordering.
3. auCdl naming of iterated instances (`XI0<3>`?) and iterated primitives (`MM0<1>`?); whether `hnlMapNetInName` defaults touch instance names.
4. Default `hnlSpectreMapNetInName` table (which characters are escaped vs renamed) in IC6.1.8/IC23.1.
5. Pseudo-port naming for inherited connections (`vdd_inh` vs `inh_<prop>`), and their position in the port list.
6. Whether the direct spectre netlister always maps `gnd!` -> `0`, and how a user-defined ground global is handled.
7. auCdl `*.CONNECT` output format and when `*.BUSDELIMITER` is printed (if ever).
8. Whether auCdl prints subckt-call parameters for plain schematic cells (no auCdl simInfo) by default.
9. Exact system-generated net numbering (`net011` vs `net11`) and stability across re-checks (matters for round-trip diffs).
10. Maestro: authoritative signatures for `maeSetCorner`, `maeSetEnvOption`, `maeSetSpec` keyword names (`?tolerance` spelling), and semantics of `Linear:0:1:5` sweep strings; maestro.sdb schema differences between version 6 and 7.
11. EDIF 200 Out details: how Virtuoso encodes buses, globals (`!`), net expressions and CDF params in EDIF; template-file keys.
12. `si -batch -command nl` vs `netlist` for spectre (single third-party source).
13. Whether Spectre now accepts `!` (logical not) and `P` (peta) in Spectre mode.

---

## Sources

Cadence documents (mirrors):
- S1 Kundert, *Designer's Guide to SPICE and Spectre*, App. B Spectre Netlist Language: https://designers-guide.org/analysis/dg-spice/chB.pdf
- S2 Virtuoso Spectre Circuit Simulator User Guide 5.1.41 (Jul 2004): https://web.eecs.utk.edu/~dbouldin/protected/spectreuser.pdf
- S3 Virtuoso Spectre Circuit Simulator Reference 11.1 (Sep 2011): https://amarketplaceofideas.com/wp-content/uploads/2015/09/Virtuoso-Spectre-Circuit-Simulator-Reference.pdf
- S4 Virtuoso Analog Design Environment User Guide 5.1.41 (2006), App. "auCdl Netlisting": https://class.ece.iastate.edu/djchen/ee501/2011/Cadence%20analog%20design%20environment%20user%20guide%202006.pdf
- S5 Virtuoso ADE L User Guide 6.1 (Jan 2007): https://amarketplaceofideas.com/wp-content/uploads/2015/09/anasimhelp610.pdf
- S6 Component Description Format User Guide 6.1.4 (Feb 2011): https://amarketplaceofideas.com/wp-content/uploads/2015/08/cdfuser.pdf
- S7 Virtuoso Schematic Composer User Guide 4.4.6, bus naming excerpt: https://www.eecs.umich.edu/courses/eecs427/f10/Common/busnames.pdf
- S8 Virtuoso Schematic Composer User Guide 5.0 (Oct 2002), mirror pages: https://studfile.net/preview/4437265/page:22/ , page:24, page:26, page:30, page:31, page:32, page:44, page:117, page:118, page:165, page:223
- S9 OCEAN Reference 5.1.41: https://www.eecis.udel.edu/~vsaxena/courses/ece697A/docs/OCEAN+Reference.pdf
- S24 Cadence support RAK "Using mae SKILL with Maestro Views": https://support1.cadence.com/public/docs/content/20467966.html

Real Virtuoso-generated files:
- S10 ADE spectre deck (IC, 2025): https://github.com/AmpCopilot/Amplifier-Copilot/blob/master/Read_scs_Script/input_scs/1.scs
- S11 ADE spectre decks (pPar subckt params, ahdl_include, iterated instances; some "sanitized"): https://github.com/USCPOSH/AMPSE/blob/master/Tutorial/SAR_ADC/Netlists/PTM65/sequential_v1_part2_65_PTM.scs and https://github.com/USCPOSH/AMPSE/blob/master/Block/DAC/CS_DAC/TSMC65_CS_DAC_1.3.2020/netlists_ptm/tb_cs_array_8b_sanitized.scs
- S12 auCdl netlist + si.env (2026): https://github.com/T0nyStark5/Low-Power-DET-Flip-Flop-in-Cadence-Virtuoso/tree/main/Cadence_Virtuoso/DETFF/TGFF/lvs
- S13 maestro views: https://github.com/T0nyStark5/Low-Power-DET-Flip-Flop-in-Cadence-Virtuoso/blob/main/Cadence_Virtuoso/DETFF/TGFF_tb/maestro/maestro.sdb ; https://github.com/antonio8400/gpdk045_v_6_0/blob/main/cmos_char/pmos/maestro/maestro.sdb ; https://github.com/BenjaminGao99/ECE581_PipelineADCModel/tree/main/Tb_ADC_sine/maestro
- S14 ADE L states: https://github.com/shirishavissom/Cell-Characterization-using-Spectre ; https://github.com/ucb-art/laygo/tree/master/generators/adc_sar/adc_sar_testbenches/clktoq_tb/spectre_state1
- S18 auCdl samples: https://github.com/uwidea/UW-IDEA_AnalogTestCases/blob/master/Module/BTSW/Santized_BTSW.cdl ; https://github.com/Spalding13/ESD-Topology-Checker/blob/main/input/netlist.cdl
- S23 Pegasus CDL sample: https://github.com/KavishkaJayakody/NS_SAR_ADC/blob/main/libraries/NS_SAR_Analog/comparator_preamp/comparator_preamp.cdl

Third-party docs:
- S15 virtuoso-bridge-lite (MIT) references: https://github.com/Arcadia-1/virtuoso-bridge-lite (skills/virtuoso/references/maestro-skill-api.md, batch-netlist-si.md, netlist.md, cellview-on-disk-layout.md, netlist_samples/)
- S16 same repo, netlist.md bus escaping note (as S15)
- S17 SignalStorm tutorial ("vdd! and gnd! (which is changed to 0 by the netlister)"): https://www.cse.sc.edu/~jbakos/612/tutorials/scells_signalstorm.shtml
- S19 Dracula LOGLVS reference (read via search summaries only): https://picture.iczhiku.com/resource/eetop/wykHQaqHTlHWgnCn.pdf
- S20 si.env export script (auCdlDefNetlistProc): https://www.cnblogs.com/yeungchie/p/16101093.html and https://blog.csdn.net/m0_56069948/article/details/123963741
- S21 auCdl export setup gist: https://gist.github.com/02015678/301c21d95b07f938112218a36bf1c8b2
- S22 BSD CDL->SPICE converter stripping `*.BUSDELIMITER`: https://github.com/kcaisley/frida/blob/main/flow/util/netlist.py
- S25 Cadence-Synopsys transfer (EDIF 200 Out template): https://www.vrg.utoronto.ca/vrg/DesignManual/CDS_SYN_2006.pdf
- S26 EDIF limitations (Elgris overview): https://www.elgris.com/content/edif_overview.html
- S27 Silvaco EDIF 200 import of Virtuoso schematics: https://silvaco.com/wp-content/uploads/content/appNotes/analog/1-012_EDIF200.pdf
- S28 UW Verilog In tutorial: https://class.ece.uw.edu/cadta/verilog/verilog2schem/
- S29 UBC Verilog In notes: https://people.ece.ubc.ca/~cad/local/html/verilogIn.html

Forums (Cadence community threads read through a text proxy):
- F1 Global net name mapping for spectre netlist: https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/43572/global-net-name-mapping-for-spectre-netlist
- F2 Global net not ending in !: https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/30226/how-can-you-create-a-global-net-in-a-spectre-netlist-that-does-not-end-in
- F3 Importing spectre netlist with arrayed net names: https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/31153/importing-spectre-netlist-with-arrayed-net-names
- F5 About the netlist generated by spectre: https://comp.cad.cadence.narkive.com/Bgep1eMK/about-the-netlist-genarated-by-spectre
- F6 Save currents by name (useterms=name): https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/14371/save-currents-in-a-spectre-netlist-run-the-netlist-in-batch-mode-and-the-currents-is-saved-as-instance_name-number
- F7 auCdl LVS naming conflict (full auCdl netlists): https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/49599/aucdl-netlist-lvs-naming-conflict
- F8 Flat vs hierarchical auCdl / CDL Out bus pins: https://support1.cadence.com/public/docs/content/11030558.html ; https://support1.cadence.com/public/docs/content/20487550.html
- F9 PININFO power/ground markers: https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/51061/adding-power-ground-signal-markers-to-pininfo-in-aucdl-netlist
- F10 LVS port mismatch, auCdlCDFPinCntrl: https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/51304/lvs-port-mismatch-errors-with-cdl-netlist
- F11 auCdl subckt params / dollarEqualParams: https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/17604/aucdl-netlist-subckt-missing-parameters ; https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/17662/missing-dollarequalparams-in-capacitor-subckt-in-aucdl-netlist
- F12 ansCdlSubcktCall vs ansCdlCompPrim: https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/38788/aucdl-question-about-anscdlsubcktcall-netlist-procedure
- F13 netSet override of vdd!: https://www.edaboard.com/threads/vdd-net-override-using-netset-property.283724/
- F14 CDS_Netlisting_Mode (same thread as F5)
- F15 Schematic bus connection, IC20.1 extractor: https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/51325/virtuoso-schematic-bus-connection
- F16 Inherited connections and explicit pins: https://comp.cad.cadence.narkive.com/qFXuRHKX/inherited-connections-and-explicit-pins
- F17 Inherited connections, connecting to nets: https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/38850/inherited-connections---connecting-to-nets
- F18 Bus naming and iterated chains: https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/38904/schematic-editor-bus-naming-and-connections-questions
- F19 ADE L SKILL interaction (asi*, ADE L EOL): https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/58080/how-to-interact-with-ade-l-window-with-skill-code
- F20 ADE L state from SKILL (asiSaveState limits): https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/48897/creating-ade-l-state-file-from-skill-script-simulation
- F21 Import long list of design variables: https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/37824/how-to-import-values-for-a-long-list-of-design-variables
- F22 sev and asi functions: https://comp.cad.cadence.narkive.com/cDe2H17x/sev-and-asi-functions
- F23 Maestro cannot set analysis (mae* calls, CCR): https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/39777/maestro-cannot-set-analysis-for-test-using-skill
- F24 maeOpenSetup not opening the window: https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/50768/maeopensetup-is-not-opening-the-assembler-window
- F25 maeAddOutput expression building: https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/46746/how-to-evaluate-the-variable-in-a-maestro-measurement-expression-and-not-treat-it-as-a-literal-string
- F26 Change Model Library Setup in maestro via SKILL: https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/40009/how-to-change-model-library-setup-in-maestro-using-skill
- F27 Cannot EDIF out: https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/43034/cannot-edif-out
- F28 Import schematic from Tanner (EDIF in): https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/41168/import-schematic-from-mentor-tanner-to-cadence
- F29 SpiceIn reference library list: https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/21579/spicein-reference-library-list
- F30 Spice In device map / subtype: https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/47960/virtuoso-spice-in-device-map-property-is-not-honored
- F31 Different results from spice in (callbacks): https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/49354/different-results-from-spice-in
- F32 Bus direction in spicein (CDL `/` syntax): https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/35466/how-can-i-set-bus-s-direction-in-spicein
