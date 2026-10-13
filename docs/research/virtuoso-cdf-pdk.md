# Virtuoso CDF, simInfo, analogLib/basic and PDK structure — research notes for the Schemify ↔ Virtuoso bridge

Researched 2026-10-12. No Virtuoso access; every fact below comes from documents, forum threads, PDK files and
real tool output found online. Nothing here is copied from xschem or from any GPL file. SKILL snippets are
written fresh for this note and are illustrative, not tested.

Confidence tags:
- **VERIFIED**: stated in a Cadence manual, or seen directly in a real Virtuoso-produced file (dump, netlist, library tree).
- **LIKELY**: stated by a credible secondary source (Cadence forum staff, PDK files, Keysight docs), or inferred from two or more consistent observations.
- **GUESS**: my own inference with thin evidence. Check it before relying on it.

> **Legal flag (read first).** Cadence manuals carry a "Restricted Permission" clause (point 4: the information
> "cannot be used in the development of like products or software"). Several sources below are Cadence manuals:
> the CDF User Guide mirrors, the ADE L guide, and the unofficial IC20.1 doc mirror `wyz569/virtuoso_20.1`.
> Wherever I could, I also cite a source that is not a Cadence manual (PDK files, Keysight docs, forum posts,
> real netlists), and I mark those facts "(corroborated: …)". Have counsel decide which evidence base the
> bridge may rely on. Interop facts (file formats, function signatures) are usually fine, but that is a call
> for counsel.

---

## Source index (short names used below)

| Tag | Source |
|---|---|
| CDFUG614 | Cadence *Component Description Format User Guide* 6.1.4 (Feb 2011), mirror: <https://amarketplaceofideas.com/wp-content/uploads/2015/08/cdfuser.pdf> |
| CDFUG201 | Same guide, IC20.1 (Oct 2020), unofficial mirror: <https://github.com/wyz569/virtuoso_20.1/tree/master/doc/cdfuser> |
| SKART-FND | IC20.1 ADE SKILL quick-reference index (function signatures): <https://github.com/wyz569/virtuoso_20.1/blob/master/doc/finder/SKILL/ADE-L/skartistref.fnd> |
| SKDF | *Virtuoso Design Environment SKILL Reference* IC20.1: <https://github.com/wyz569/virtuoso_20.1/blob/master/doc/skdfref/skdfref.pdf> |
| ALIBREF | *Analog Library Reference* IC20.1: <https://github.com/wyz569/virtuoso_20.1/blob/master/doc/analoglibref/analoglibref.pdf> |
| BASICREF | *Virtuoso Basic Library Reference* IC20.1: <https://github.com/wyz569/virtuoso_20.1/blob/master/doc/basicLib/basicLib.pdf> |
| CAIUG | *Cadence Application Infrastructure User Guide* IC20.1: <https://github.com/wyz569/virtuoso_20.1/blob/master/doc/caiuser/caiuser.pdf> |
| ADEL61 | *Virtuoso ADE L User Guide* 6.1 (Jan 2007): <https://amarketplaceofideas.com/wp-content/uploads/2015/09/anasimhelp610.pdf> |
| KS-LIBINT | Keysight/Agilent *Cadence Library Integration* (RFIC Dynamic Link): <https://edadownload.software.keysight.com/eedl/ads/2011/pdf/dynlnklc.pdf> |
| KS-ALIB | Keysight *analogLib Components* (a modified analogLib, IC5.1.41 era): <https://edadownload.software.keysight.com/eedl/ads/2011_01/pdf/anloglib.pdf> |
| FPDK45 | NCSU FreePDK45 `ncsu_basekit` (Apache-2.0 upstream <https://github.com/baichen318/FreePDK45>), mirror used: <https://github.com/lnis-uofu/FreePDK45-RRAM-Addon/tree/master/ncsu_basekit> |
| NCSU-AP | NCSU CDK 1.6 `NCSU_Analog_Parts` (an analogLib clone whose CDF is stored as `artSimInfoCDF.att` text): <https://github.com/zonyzhao/Cadence/tree/master/ncsu-cdk-1.6.0.beta/lib/NCSU_Analog_Parts> |
| GPDK045 | Cadence GPDK045 Reference Manual rev 6.0: <http://www.princeton.edu/~nverma/cadence_23.1/gpdk045_v_6_0/docs/gpdk045_pdk_referenceManual.pdf> |
| GPDK17 | Cadence GPDK Reference Manual rev 1.7: <https://research.ece.cmu.edu/sandbox/cadfaq/docs/gpdk_referenceManual.pdf> |
| ABINV | A.D. Beckett, `abInvokeCdfCallbacks.il` (the code later shipped as `CCSinvokeCdfCallbacks`), mirror: <https://github.com/ebecheto/Skill/blob/master/ab/abInvokeCdfCallbacks.il>. I read it for behaviour only; none of its code is reproduced here. |
| BECKETT-PROPS | comp.cad.cadence thread "Using Skill to change properties on fets": <https://comp.cad.cadence.narkive.com/HGLE4utw/using-skill-to-change-properties-on-fets> |
| AMPCOP | Real Virtuoso library and spectre netlists: <https://github.com/AmpCopilot/Amplifier-Copilot> (`Cadence_Lib/TSMC22ULL_Std_AMP_LIB/…`) |
| USCPOSH | Real Virtuoso spectre netlists with pPar subckts: <https://github.com/USCPOSH/AMPSE> (`Tutorial/SAR_ADC/Netlists/PTM65/sequential_v1_part2_65_PTM.scs`) |
| NSSAR | Real auCdl/PVS CDL (Sky130 in Virtuoso): <https://github.com/KavishkaJayakody/NS_SAR_ADC/blob/main/libraries/NS_SAR_Analog/comparator_preamp/comparator_preamp.cdl> |
| PVTS | Real spectre netlist with `global 0 vdd!`: <https://github.com/scale-lab/PVTsensors/blob/master/PVT_Sensor/Testbench/VSesnor_Testbench.sp> |

---

## 1. CDF levels, the CDF SKILL API, parameters

### 1.1 Levels

- There are three levels: **base** (stored permanently on the cell or library; needs write permission), **user** (a per-session overlay; never saved by the system, so you dump and reload it yourself; no write permission needed), and **effective** (user overlaid on base). **VERIFIED** CDFUG614 ch.1 "Levels of CDF Data".
- Both libraries and cells have base and user levels. The effective cell CDF stacks four layers, later ones winning: base lib, then user lib, then base cell, then user cell. The *effective instance CDF* is the effective cell CDF plus the values stored on that instance. **VERIFIED** CDFUG614 ch.1; ch.8 `cdfGetCellCDF`.
- Cadence discourages the user level ("simulation results can be different for different users"). **VERIFIED** CDFUG614.
- Only the base level is ever written to disk. In OA libraries the cell-level CDF lives in `<lib>/<cell>/data.dm` and the library-level CDF in `<lib>/data.dm`; in CDB-era libraries it lived in `prop.xx`. **LIKELY.** `data.dm` at both levels is seen in FPDK45's tree (`NCSU_Devices_FreePDK45/data.dm`, `…/NMOS_VTL/data.dm`). CAIUG ch.2 says `data.dm` holds "view property files created by Virtuoso applications". The cdsp4 guide (<https://swarm.workshop.perforce.com/view/guest/perforce_software/cdsp4/release/2.2/Doc/cdsp4_local.html?v=5>) lists `prop.xx` as a library file. Exactly which `data.dm` holds the CDF bag is my inference.
- `data.dm` is binary (an OA DM-data file). Do not try to write it; create CDF through SKILL. **LIKELY**.

### 1.2 SKILL functions (signatures)

All **VERIFIED** CDFUG614 ch.8; signatures re-checked against SKART-FND (IC20.1).

| Function | Signature / behaviour |
|---|---|
| `cdfCreateBaseLibCDF(libId [?doneProc t] [?formInitProc t] [?fieldWidth x] [?fieldHeight x] [?buttonFieldWidth x] [?promptWidth x])` | → cdfDataId/nil. The library must be writable. **Does not update** an existing base CDF; delete it first. |
| `cdfCreateUserLibCDF(libId …same keys…)` | user level |
| `cdfCreateBaseCellCDF(cellId …same keys…)` | → cdfDataId/nil. Created with no params or simInfo. The cell must be writable. Does **not** update an existing one. |
| `cdfCreateUserCellCDF(cellId …)` | user level |
| `cdfCreateParam(cdfDataId ?name ?type ?defValue ?units ?parseAsNumber ?choices ?prompt ?use ?display ?editable ?dontSave ?callback ?storeDefault ?parseAsCEL [?description])` | `?name` and `?type` are always required; `?defValue` is required unless it overrides a base param. **Silently skips a name that already exists.** The type cannot be overridden at the user level. `?description` exists only in IC20.1 (SKART-FND). |
| `cdfGetBaseLibCDF(libId)`, `cdfGetUserLibCDF`, `cdfGetLibCDF` (effective) | → cdfDataId/nil |
| `cdfGetBaseCellCDF(cellId)`, `cdfGetUserCellCDF`, `cdfGetCellCDF` (effective incl. lib) | → cdfDataId/nil |
| `cdfGetInstCDF(instId)` | effective CDF in which each param's `value` reflects the instance's stored properties |
| `cdfSaveCDF(cdfDataId)` | base level only; needs write permission; overwrites |
| `cdfDeleteCDF(cdfDataId)` | deletes the CDF and all its params, including the saved copy |
| `cdfDeleteParam(paramId)` | refuses if a partial user override would be left dangling |
| `cdfDump(t_lib t_file [?cellName t] [?level 'base\|'user] [?edit g])` | with no `?cellName`, dumps the **library** CDF only; default level `'base` |
| `cdfDumpAll(t_lib t_file [?level] [?edit])` | library CDF plus every cell, in one file |
| `cdfCopyCDF(id "baseLibData"\|"userLibData"\|"baseCellData"\|"userCellData" srcCdf)` | the destination must not already have that level |
| `cdfCopyParam(dataId paramId)`, `cdfFindParamByName(dataId name)` | |
| `cdfUpdateInstParam(instId)` | writes every effective-CDF param onto the instance as properties; in IC20.1 it also runs `doneProc` (SKART-FND) |
| `cdfUpdateInstSingleParam(instId name)` | copies one param's effective value onto the instance |
| `cdfSyncInstParamValue(inst1 inst2)` | same cell required |
| `cdfRefreshCDF(libOrCellId)` | reloads the in-memory CDF from disk. Cadence warns that saved param pointers can become stale. |
| `cdfParseFloatString(str)` | `"1.0u"`→`1e-06`; `"1g"`→`nil`; `"abcd"`→`"abcd"` (non-numeric strings come back unchanged) |
| `cdfFormatFloatString(str scale)` | `("123.4" "m")`→`"123400.0m"` |
| `cdfGetUnitScaleFactor(unit)` / `cdfSetUnitScaleFactor(unit sf)` | display scaling only |
| `artGenerateHierSymbolCDF(cv [g_overwrite])` | builds the cell CDF the way "create symbol from schematic" does, adding a param for every `pPar()` it finds in the cellview. **VERIFIED** SKART-FND. |

Data-object fields (CDFUG614 ch.8, **VERIFIED**):
- On a cdfDataId: `id`, `type`, `parameters`, `doneProc`, `formInitProc`, `fieldWidth` (350), `fieldHeight` (35), `buttonFieldWidth` (340), `promptWidth` (175), plus label properties (`paramLabelSet`, `opPointLabelSet`, `modelLabelSet`, …), `simInfo`, and `viewInfo` (KS-LIBINT).
- `cdfDataId->paramName` is shorthand for the param object.
- `type` strings: the manual lists `baseLibCDF … effInstCDF`, but its own example prints `"userCellData"`, and Beckett's code tests for `"instData"` and writes `"cellData"`. The runtime strings are therefore probably `"baseCellData"`, `"userCellData"`, `"cellData"` (effective) and `"instData"` (instance). **LIKELY** (ABINV, CDFUG614 example).

### 1.3 Parameter attributes (every `?keyword`)

**VERIFIED** CDFUG614 ch.3 tables 3-1/3-2 and ch.8, unless marked otherwise.

| Keyword | Value form | Meaning |
|---|---|---|
| `?name` | string | Cannot start with a digit. Reserved words to avoid: simulator keywords, field names, `others`, `loadSave`, `simInfo`, `othersFG`, `area`, `resistor`, `multiplier` (App. A). |
| `?type` | `"string" "int" "float" "radio" "cyclic" "boolean" "button" "netSet"` | Quotes required. `netSet` stores inherited-connection overrides and **must** have `storeDefault "yes"` (CDFUG201). |
| `?prompt` | string | form label (required) |
| `?defValue` | depends on type | Mandatory for int, float, radio, cyclic and netSet; a param missing one is dropped when the Edit CDF form is applied. For string params it is a string such as `"1u"`. FPDK45 passes a SKILL variable holding `"90n"`. |
| `?choices` | list of strings | required for radio/cyclic, ignored otherwise |
| `?units` | one of `"resistance" "capacitance" "inductance" "conductance" "time" "frequency" "power" "powerDB" "lengthMetric" "lengthEnglish" "angle" "voltage" "current" "temperature"` | Display scaling and suffix only. Takes effect only on string params with parseAsNumber. There are no user-defined units. |
| `?parseAsNumber` | `"yes"`/`"no"`/omitted | String params only. A numeric value is re-formatted for display (`5.4e-12`→`5.4p` with capacitance units); non-numbers are left alone. Must be `"yes"` when parseAsCEL is `"yes"`. |
| `?parseAsCEL` | `"yes"`/`"no"` | String (and radio/cyclic) params. Treats the value as a CEL/AEL expression, so `iPar()`, `pPar()` and design variables work. |
| `?storeDefault` | `"yes"`/`"no"`/`"don't use"` | Default "no". See 1.4. |
| `?display` | string evaluated to t/nil | Usually `"artParameterInToolDisplay('w)"` (tool filter). `"nil"` hides the param. |
| `?editable` | string evaluated to t/nil | string, int and float only; false shows the field greyed out |
| `?use` | string evaluated to t/nil | "programming only". When false, the param is treated as absent (not displayed and not used), e.g. `"cdfgData->FNpairs->value >= 1"` (NCSU-AP). |
| `?dontSave` | string evaluated to t/nil | When true, the value is never stored on the instance, overriding storeDefault. Cadence strongly advises against it. |
| `?callback` | string of SKILL | Evaluated when the value changes **in a form**. See section 2. |
| `?description` | string | IC20.1 only (SKART-FND) |

Example `?defValue`/`?units` usage seen in the wild: `?units "lengthMetric" ?defValue "4u" ?type "string" ?parseAsNumber "yes" ?parseAsCEL "yes"`, i.e. CDFUG614's NFET example and FPDK45. **VERIFIED**.

`~>value` vs `~>defValue`:
- `defValue` is the CDF default; `value` is the current value in the context you fetched (cell CDF: the default; instance CDF: the stored property or else the default). **VERIFIED** (CDFUG614 ch.7: "To get the default … `cdfgData->p->defValue`; current … `->value`").
- Assigning to `value` on an instance CDF writes the instance property. **LIKELY.** BECKETT-PROPS and ABINV both depend on this.

Unit and scale-factor letters in CDF:
- `f p n u m c k M G` = femto … giga; **M = mega, m = milli**. **VERIFIED** CDFUG614 "Scale Factors".
- Spectre uses the same SI rule (case-sensitive: `M`=1e6, `m`=1e-3, plus `T G K k _ % c u n p f a`). **VERIFIED** Spectre UG 5.1.41 p.83 (<https://web.eecs.utk.edu/~dbouldin/protected/spectreuser.pdf>).
- SPICE/CDL readers treat `M` as milli. That is why auCdl writes a `*.MEGA` header line, which tells the LVS tool to read `M` as mega; `auCdlSkipMEGA='t` suppresses it. **VERIFIED** ADEL61 p.527; the `*.MEGA` line appears in NSSAR.
- Whitespace matters: `"12 u"` ≠ `"12u"`, and `" 12"` ≠ `"12"` (App. A). **VERIFIED**.
- Bridge rule: always emit CDF numeric strings as `<number><SI letter>` with no space, and never emit `M` meaning milli.

### 1.4 How instance properties relate to CDF (what is stored)

- When an instance is placed with the form, a property is stored **only for params whose value differs from the CDF default**, unless `storeDefault "yes"`. With `storeDefault "no"`, changing the CDF default later changes every instance that still uses it. **VERIFIED** CDFUG614 ch.3/ch.8/ch.9 (corroborated: Beckett on the Cadence forum, "instance properties are stored if any parameter in the CDF is changed away from the CDF default").
- All ADE tools fall back to the CDF default when an instance has no property. **VERIFIED**.
- `dontSave` true means never stored, even with storeDefault yes. **VERIFIED**.
- `dbCreateInst` places a bare instance with **no** CDF properties; the netlister then uses CDF defaults. **LIKELY** (forum thread "Instantiating instance from command line: no properties stored", <https://ardell3.rssing.com/chan-3180850/all_p97.html>; also implied by `cdfUpdateInstParam` existing at all).
- `dbReplaceProp(obj name type value)` replaces or creates one scalar property (type strings such as `"string" "int" "float" "boolean" "ILList"`), with **no** CDF awareness: no callbacks run, no type checking against the CDF, and no removal when the value equals the default. **VERIFIED** for the signature (SKDF). **VERIFIED** (forum) that no callbacks run: BECKETT-PROPS, "Either using dbReplaceProp() or the above [inst~>W=2] will _NOT_ trigger the CDF callbacks".
- Pitfall: `type(nil)` is `list`, so a generic `dbReplaceProp(inst n type(v) v)` breaks for nil/boolean values. **LIKELY** (forum).
- Prefer `cdfgData->p->value = "…"` on `cdfGetInstCDF(inst)`, because the CDF layer picks the property type. Whether it *removes* the property when the value equals the default (storeDefault no) is **unknown** (see open questions).

### 1.5 `cdfDump` output shape (exact)

**VERIFIED.** CDFUG614 Appendix C (the analogLib `nbsim` dump) gives the shape; KS-LIBINT (analogLib `npn` dump) and FPDK45 `NMOS_VTL.il` (a real PDK dump) match it byte-for-byte in structure. The layout is:

```
/****************************************************/
 LIBRARY = "<lib>"
 CELL    = "<cell>"
/****************************************************/

let( ( libId cellId cdfId )
    unless( cellId = ddGetObj( LIBRARY CELL )
        error( "Could not get cell %s." CELL )
    )
    when( cdfId = cdfGetBaseCellCDF( cellId )
        cdfDeleteCDF( cdfId )
    )
    cdfId  = cdfCreateBaseCellCDF( cellId )

    ;;; Parameters
    cdfCreateParam( cdfId
        ?name           "w"
        ?prompt         "Width"
        ?units          "lengthMetric"
        ?defValue       "1u"
        ?type           "string"
        ?display        "artParameterInToolDisplay('w)"
        ?parseAsNumber  "yes"
        ?parseAsCEL     "yes"
    )
    ...one cdfCreateParam block per param, in display order...

    ;;; Simulator Information
    cdfId->simInfo = list( nil )
    cdfId->simInfo->auCdl = '( nil
        netlistProcedure  ansCdlCompPrim
        ...
    )
    cdfId->simInfo->spectre = '( nil ... )

    ;;; Properties
    cdfId->formInitProc            = ""
    cdfId->doneProc                = ""
    cdfId->buttonFieldWidth        = 340
    cdfId->fieldHeight             = 35
    cdfId->fieldWidth              = 350
    cdfId->promptWidth             = 175
    cdfId->paramLabelSet           = "-model l w"
    cdfId->opPointLabelSet         = "id vgs vds"
    cdfId->modelLabelSet           = "vto kp gamma"
    cdfSaveCDF( cdfId )
)
```

Observations for writing a parser:
- Keys appear in roughly this order: name, prompt, units, defValue, choices, type, use, display, editable, dontSave, parseAsNumber, storeDefault?, parseAsCEL, callback. FPDK45 places `?storeDefault` before `?parseAsCEL` and `?callback` last. Do not depend on the order.
- Boolean params appear with **no** `?defValue` line (`off` in nbsim/FPDK45).
- int params appear with an unquoted integer (`?defValue 0`). Choices appear as a quoted list `'("off" "triode" "sat" "subth")`.
- `?defValue` can be a **SKILL expression** (FPDK45 uses `ncsuGlobalData->MinW`), so a dump is executable SKILL and does not always hold literal values.
  - Mitigation: run the dump inside Virtuoso, or read values back through `cdfGetBaseCellCDF(cell)~>parameters` after loading. **VERIFIED** (FPDK45 file).
- simInfo values are quoted lists whose atoms are symbols, strings, `nil`/`t`, and `progn(bn)`-style forms.
  - termOrder/termMapping symbols escape special characters: `NC\+`, `a\<0\>`, `\:d`.
  - `"(FUNCTION minus(root(\"PLUS\")))"` appears as a string atom.
  - **VERIFIED** CDFUG614 ch.4 and NCSU-AP.
- `cdfDumpAll` writes many such blocks into one file. **LIKELY** (repeats the per-cell shape; I found no sample).
- A dump is re-loadable with `load("file")`, but only by someone with write permission on the cell. **VERIFIED**.

### 1.6 Writing a base cell CDF from SKILL (illustrative, my own code)

```skill
; Create (or replace) the base CDF for a user subcircuit cell that takes w/l/m parameters.
procedure( schemifyMakeCellCdf(libName cellName)
  let( (cell cdf)
    cell = ddGetObj(libName cellName)
    unless(cell error("schemify: no cell %s/%s" libName cellName))
    when(cdf = cdfGetBaseCellCDF(cell) cdfDeleteCDF(cdf))   ; create never updates
    cdf = cdfCreateBaseCellCDF(cell)
    foreach(p list(list("w" "Width" "1u") list("l" "Length" "100n"))
      cdfCreateParam(cdf ?name car(p) ?prompt cadr(p) ?type "string"
                     ?defValue caddr(p) ?units "lengthMetric"
                     ?parseAsNumber "yes" ?parseAsCEL "yes"))
    cdfCreateParam(cdf ?name "m" ?prompt "Multiplier" ?type "string"
                   ?defValue "1" ?parseAsNumber "yes" ?parseAsCEL "yes")
    ; Hierarchical cell: no netlistProcedure for spectre; termOrder fixes port order.
    cdf->simInfo = list(nil)
    cdf->simInfo->spectre = '(nil termOrder ("in" "out" "vdd" "vss") instParameters (w l m))
    cdf->simInfo->auCdl   = '(nil netlistProcedure ansCdlSubcktCall
                              termOrder ("in" "out" "vdd" "vss") instParameters (w l m)
                              componentName subcircuit namePrefix "X")
    cdfSaveCDF(cdf)
  ))
```

---

## 2. Callbacks: running them without a form

**What Virtuoso guarantees.** **VERIFIED** CDFUG614 ch.3, ch.7, App. A.
- A CDF param callback is a SKILL string that is evaluated **only** when the value changes in the Add Instance / Edit Object Properties form.
- Changing a property from SKILL (`dbReplaceProp`, `inst~>w = …`, `cdfgData->w->value = …`), Search & Replace, extraction, or a change to an inherited `iPar`/`pPar`/design-variable value does **not** trigger callbacks.
- Callbacks *are* triggered when a value is changed by loading a default.
- ADE "Design Variables → Update" fires every callback for every instance in the open window. It covers one level only and runs in **no guaranteed order**.
- Callbacks cannot change simInfo.
- Callbacks must use the globals `cdfgData` (the CDF in use) and `cdfgForm` (the form, if one exists). When a callback edits values, it should set `cdfgForm->cdfModified = t`. **VERIFIED** ch.8 "Global Variables".

**formInitProc / doneProc.** **VERIFIED** CDFUG614 ch.6/ch.8.
- `formInitProc`: the *name* of a procedure (no parens), called with one argument, the cdfDataId being loaded into the form. It runs every time the form is (re)displayed, including after each callback.
- `doneProc`: the name of a procedure called with one argument, the **instance**, after Apply or placement. Cadence's example rebinds `cdfgData = cdfGetInstCDF(inst)` inside it.
- In IC20.1 `cdfUpdateInstParam` also runs `doneProc`. **VERIFIED** SKART-FND.

**Calling a callback by hand.** **LIKELY.** BECKETT-PROPS describes the idiom (summarised): bind `cdfgData` to `cdfGetInstCDF(inst)`, fetch `param->callback`, and if it is non-empty run `evalstring(callback)` inside `errset(... t)`.

**How CCSinvokeCdfCallbacks / abInvokeCdfCallbacks behaves.** **LIKELY.** This is from reading ABINV; the forum confirms CCS is "essentially the same as abInvokeCdfCallbacks" (search result summarising <https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/37665>).
- Entry points:
  - the per-cellview function walks `cv~>instances`;
  - the per-instance function works on one instance;
  - the CCS version adds a per-library walker that opens each schematic, runs the callbacks, runs `schCheck` and saves.
- It binds `cdfgData` and `cdfgForm` **locally** with `let`, so the user's globals are not clobbered.
- By default it does not pass the raw instance CDF. It builds an "effective-CDF lookalike":
  - a struct with the same fields, whose `id` is reset from the instance to the master's **cell** ddId and whose `type` is reset from `"instData"` to `"cellData"`;
  - the parameter objects are the *instance's*, so writes still land on the instance.
  - Reason: during a form edit, `cdfgData->id` is the cell. Callbacks that do `cdfgData->id~>…` would otherwise get an instance. Beckett notes the same `id` mismatch in BECKETT-PROPS.
- With "set cdfgForm" on (the default unless `useInstCDF`), it sets `cdfgForm = cdfgData`, so callbacks that read `cdfgForm->p->value` still work.
- Optional `?callInitProc t` evaluates `formInitProc` first, as `"<name>(cdfgData)"`.
- Order: if `?order '(p1 p2 …)` is given, only those params run, in that order. Otherwise it walks `cdfgData->parameters` in CDF order and runs every non-empty callback.
- Each callback runs as `evalstring` inside `errset`. Any error marks the run as failed (returns nil) but does not stop the loop.
- A pattern list lets it skip callbacks known to be unsafe.

**Pitfalls to design around:**
1. Callbacks that touch form-only things: `cdfgForm->field->editable`, `hiGetCurrentForm()`, or `formName->...` widgets. These fail without a real form; `errset` catches them, so check the return value. **LIKELY**.
2. Callbacks that parse the value numerically fail if the value is an expression. FPDK45's `ncsuWidthCallBack` does `round(cdfParseFloatString(cdfgData->w->value)/grid)`, which raises an error when `w` is `"wn*2"`, because `cdfParseFloatString` returns the string. **VERIFIED** (FPDK45 `skill/callbacks.il`) as to the code; the failure is my inference.
3. Callbacks write values back with `sprintf(s "%g" …)`, so after a callback run `w` becomes `"9e-08"` rather than `"90n"`. Normalise when comparing round-trips. **VERIFIED** (FPDK45 callbacks.il, CDFUG614 NFET example).
4. Callbacks can *clamp or snap* values (min/max, grid), so the value Schemify sent may not be the value that ends up stored. Re-read after running callbacks and report the differences. **VERIFIED** (FPDK45, CDFUG614 pcMOSw).
5. Order dependence: there is no defined order, so run them in CDF param order, which is what the form does when you tab top to bottom. **GUESS** (closest to the GUI).
6. Callbacks may call `artError`/`warn` or pop up dialogs. Run in batch mode (`virtuoso -nograph`) to avoid blocking. **GUESS**.
7. Library code must be loaded first. Callbacks live in `libInit.il`-loaded files or `.cdsinit`-loaded files (FPDK45 loads `loadSkill.il` via an environment-variable path). Make sure the library has been touched (`ddGetObj(lib)`) so `libInit.il` has run. **VERIFIED** CDFUG614 ch.7 (libInit.il is loaded "the first time a library is accessed").

**Bridge recipe** (illustrative, my own code; not derived from ABINV):

```skill
; Set CDF params on one instance and fire their callbacks the way a form edit would.
; params: list of (name valueString) pairs. Returns list(okFlag cdf) so the caller can read back.
procedure( schemifySetInstParams(inst params)
  let( ((ok t) cdfgData cdfgForm cb)          ; local rebinding, globals untouched
    cdfgData = cdfGetInstCDF(inst)
    unless(cdfgData error("schemify: %s has no CDF" inst~>name))
    cdfgForm = cdfgData                        ; some PDK callbacks read cdfgForm
    when(cdfgData->formInitProc && cdfgData->formInitProc != ""
      unless(errset(evalstring(strcat(cdfgData->formInitProc "(cdfgData)")) t) ok = nil))
    foreach(pv params
      let( ((p get(cdfgData concat(car(pv)))))
        if(p then
          p->value = cadr(pv)                  ; stores the property via CDF
          cb = p->callback
          when(cb && cb != ""
            unless(errset(evalstring(cb) t) ok = nil))
        else
          warn("schemify: no CDF param %s on %s\n" car(pv) inst~>name)
          ok = nil)))
    when(cdfgData->doneProc && cdfgData->doneProc != ""
      unless(errset(evalstring(strcat(cdfgData->doneProc "(inst)")) t) ok = nil))
    list(ok cdfgData)
  ))
```

The caveats from Beckett's lookalike apply: if a PDK's callbacks use `cdfgData->id` as a cell, add the
`id`-rewrite trick, either by porting the idea in your own code or by letting the user load CCS's file, which
Cadence ships to customers as a support solution.

---

## 3. simInfo

### 3.1 Structure

- `cdfId->simInfo` is a disembodied property list (DPL) keyed by simulator name (`spectre`, `auCdl`, `auLvs`, `hspiceD`, `ams`, `UltraSim`, older ones: `cdsSpice`, `spectreS`, `hspiceS`, `ads`…). Each value is itself a DPL `'(nil key value …)`. **VERIFIED** CDFUG614 ch.4.
- Fields and which simulators use them (CDFUG614 ch.4, table "Simulator Options", **VERIFIED**):

| Field | spectre | auLvs | ams | auCdl | UltraSim | hspiceD |
|---|---|---|---|---|---|---|
| netlistProcedure | Y | Y | Y | Y | Y | Y |
| otherParameters | Y | Y | Y | Y | Y | Y |
| instParameters | Y | Y | Y | Y | Y | Y |
| componentName | Y | Y | Y | Y | Y | Y |
| termOrder | Y | Y | Y | Y | Y | Y |
| termMapping | Y | – | Y | – | Y | Y |
| propMapping | Y | Y | Y | Y | Y | Y |
| namePrefix | – (unused) | Y | – | Y | Y | Y |
| deviceTerminals, permuteRule | – | Y | – | – | – | – |
| enumParameters, excludeParameters, extraTerminals, isPrimitive, arrayParameters, referenceParameters | – | – | Y | – | – | – |
| stringParameters | Y | – | Y | – | – | – |
| opParamExprList | Y | – | Y | – | – | – |
| optParamExprList | Y | – | Y | – | Y | Y |
| modelParamExprList | Y | – | – | – | Y | – |
| dollarParams, dollarEqualParams, modelName | – | – | – | Y | – | – |
| macroArguments, modelArguments, current | all "No" in the table (legacy) | | | | | |

Field meanings (**VERIFIED** CDFUG614 ch.4 unless noted):
- **netlistProcedure**: a SKILL netlist function, given as a symbol or a string. For spectre it is optional; the netlister picks a default. Seen in the wild:
  - `ansSpectreDevPrim`, `ansSpectreCompPrim`, `ansSpectreSrcPrim_csrc`, `ansSpectreSrcPrim_mind` (NCSU-AP);
  - `ansCdlCompPrim`, `ansCdlSubcktCall`, `ansLvsCompPrim`, `hspiceDCompPrim` (FPDK45).
- **instParameters**: the *simulator-side* names to print as `name=value`, after propMapping is applied. The order matters only for old macro models (`&2`, `&3`, …).
- **otherParameters**: params handled specially (typically `model`, `bn`). They stay visible under the tool filter but are not printed as `name=value`.
- **componentName**: for spectre, the master name comes from the `model` param if it has a value, otherwise from `componentName`, otherwise from the cell name. auCdl/auLvs accept any name; other simulators need lowercase analogLib-style names. **VERIFIED** (corroborated: KS-LIBINT says the same for ADS).
- **termOrder**: terminal names in netlist order. Symbol atoms need escapes (`a\<0\>`). `progn(bn)` (a "programmable node" taken from the `bn` param) is legacy and unsupported by spectre; use inherited connections instead. For subcircuits, termOrder is also the subckt port order.
- **termMapping**: `(nil symTerm simName …)`. It maps symbol terminals to the names the simulator saves results under. `\:d` means instance terminal `d`; for subckts spectre needs `\:1`, `\:2` (port index). `"(FUNCTION minus(root(\"PLUS\")))"` derives a current from another terminal.
- **propMapping**: `(nil simName cdfName …)`, e.g. `(nil L l W w)` for auCdl and `(nil dc vdc mag acm phase acp type srcType)` for vdc (NCSU-AP).
- **namePrefix**: prepended to the instance name in SPICE-like netlists. It can be `"FUNCTION myFn"`.
- **modelName**: auCdl only.
- **deviceTerminals / permuteRule**: auLvs, e.g. `"(p D S)"`.
- **opParamExprList** etc.: lists of `(name exprString)` pairs used for annotation.
- **viewInfo** (`cdfId->viewInfo->veriloga`, …): a per-stop-view override that uses `netlistProc`, `moduleName`, `parameterList`, `termOrder`, `termMapping`, `propMapping`. It is SKILL-only and cannot be edited in the form. **VERIFIED** KS-LIBINT. Verilog-A views get their viewInfo via `ahdlUpdateViewInfo`. **LIKELY** (Yeager Engineering, <https://sites.google.com/site/yeagerengineering/cadence/cdb-cdf>; SKART-FND).
- `noPortDelimiter t`, `current port`, `source t`, `dataAccessMap ((IDC (D "id")))` appear in real CDFs (NCSU-AP, FPDK45). Their meaning beyond annotation is **unknown**.

### 3.2 Real examples (VERIFIED from files)

FPDK45 NMOS_VTL (a real PDK MOS; note spectre has no componentName/netlistProcedure):
```
spectre: otherParameters (model)
         instParameters (w l as ad ps pd nrd nrs ld ls m trise region degradation geo rdc rsc sa sb sd)
         termOrder (D G S B)  termMapping (nil D \:d G \:g S \:s B \:b)  namePrefix ""
auCdl:   netlistProcedure ansCdlCompPrim  instParameters (m L W)  componentName nmos
         termOrder (D G S B)  propMapping (nil L l W w)  namePrefix "M"  modelName "NMOS_VTL"
         dollarParams nil  dollarEqualParams nil
auLvs:   netlistProcedure ansLvsCompPrim  instParameters (m l w)  componentName nmos
         termOrder (D G S B)  permuteRule "(p D S)"  namePrefix "M"
hspiceD: netlistProcedure hspiceDCompPrim  termMapping (nil D \,D …)  namePrefix "M" …
ams:     isPrimitive (t)  enumParameters (region)  termOrder (D G S B) …
```
In the same file, `model` has `?defValue "NMOS_VTL" ?storeDefault "yes"`. That makes the stored model name appear on every instance, and spectre then uses it as the master name.

### 3.3 Netlist value formatting and the inheritance functions

- `pPar("x")` (NLP `[+x]`): a parent parameter. In a spectre netlist the call is **replaced by the bare name `x`**, and the subckt gets a `parameters x …` line. **VERIFIED** ADEL61 p.123 ("During netlisting, the pPar expression is substituted by the name of the parameter").
  - Real output (USCPOSH): `subckt NOR_PTM_3_v1 A B C VDD VSS Z` / `parameters fnnn fppp` / `M11 (Z C VSS VSS) nmos w=400n l=65n m=fnnn`; the caller writes `I6 (…) NOR_PTM_3_v1 fnnn=nor3 fppp=3*nor3`.
  - pPar passes one level only. **LIKELY** (forum).
  - At the top level a pPar falls back to the top cell's properties, then to its CDF default. **VERIFIED** CDFUG614.
- `iPar("x")` (NLP `[~x]`): the same instance; replaced by `(value)` in parentheses. **VERIFIED** ADEL61. Cyclic references crash (iPar w↔l example). **VERIFIED** CDFUG614.
- `atPar("x")` (NLP `[@x]`) and `dotPar("x")` (NLP `[.x]`): discouraged. atPar searches the whole hierarchy and then `nlpglobals`. **VERIFIED** CDFUG614.
- Design variables (`VDD`, `CLOAD`) are passed through as identifiers, and ADE writes a top-level `parameters VDD=1.9 …` line. **VERIFIED** AMPCOP netlist.
- Values are written as the stored strings (`w=400n`, `sd=180n`, `nf=1`), and expressions are written verbatim (`w=MOSFET_17_7_W_BIASCM_NMOS*1`). **VERIFIED** AMPCOP, USCPOSH.
- GPDK045 shows `l=180.0n` (a callback-formatted string). **VERIFIED** GPDK045 datasheet.
- In the form and labels, a `lengthMetric` value is *displayed* with its unit (Cadence's NFET label example shows `l:3u w:7u`). The stored property is the bare string. **LIKELY**. The common sight of "180n M" in forms is display only; **GUESS** that the trailing " M" is never stored.
- `cdsTerm("D")`, `cdsName()`, `cdsParam(1)`/`cdsParam('w)` are **ILLabel** functions placed on symbols (layer annotate, drawing7/8). NLP labels use `[@instanceName]`, `[@partName]` and similar. **VERIFIED** CDFUG614 ch.5 (corroborated: forum "NLPLabels vs ILLabels" summary, <https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/34429/nlplabels-vs-illabels>).

### 3.4 auCdl procedures and CDL formatting

**VERIFIED** ADEL61 pp.531–536 (corroborated by the real CDL in GPDK045/NSSAR):
- **ansCdlCompPrim** (primitives): the instance name is the **namePrefix + schematic instance name**, then the nets in termOrder, then the instParameters (only the Dracula-known ones, but `m` always). It can also print `$PINS term=net` for overridden inherited terminals.
  - Real lines:
    - `MNM1 D G S B g45n1svt W=4u L=180.0n M=1.0` (GPDK045);
    - `MNM0 net011 N_BIAS VSS VSS nfet_01v8 M=14 L=1u W=1u` (NSSAR, Sky130);
    - resistor-style `RIPRB0 plus minus 1m $[SH]` (forum, <https://comp.cad.cadence.narkive.com/0JQVu8iL/iprobe-component-stb-analysis-and-cdl-out>);
    - inductor `LInd_1 net7 net4 $[Ind]` (<https://comp.cad.cadence.narkive.com/ryNCWPuc/cdl-export-parameters-missing>).
  - So for MOS the model name is positional and bare; for R/C/L/D, auCdl writes `$[modelName]`. **LIKELY** (inferred rule).
  - The prefix is always prepended (`M`+`NM0`→`MNM0`, `R`+`IPRB0`→`RIPRB0`). **VERIFIED**.
- **ansCdlSubcktCall** (hierarchical cells): `X<inst>`, then the nets, then `/ <cellName>`, then `name=value` for instParameters. For non-stopping cells only `M=` is printed. Real line: `XI1 net8 net5 net6 net7 / inv_1X` (<https://support1.cadence.com/public/docs/content/11030558.html>).
- **ansCdlCompParamPrim / ansCdlSpecParamPrim / ansCdlNameValueNetlistProc / ansCdlSubcktCallExtended**:
  - `dollarParams` print as `$<value>`; `dollarEqualParams` print as `$<name>=<value>`;
  - a `DOTMODEL` instParam prints `$.MODEL=<model>`.
- Subckt port order: alphabetical by default. Set `auCdlCDFPinCntrl = 't` in `.simrc` to make CDF termOrder decide. **LIKELY** (ADEL61 "Changing the Pin Order" and forum; the default sort is my inference from forum complaints).
- Header lines seen: `*.MEGA`, `*.EXPAND_ON_M_FACTOR` (NSSAR), `*.SCALE METER` (`auCdlScale`), `*.PININFO A:I B:O` (with `displayPININFO`), `*.GLOBAL VCC!:P GND!:G` (powerNets/groundNets in `.simrc`), `*.RESI` (shortRES).
- si.env keys include `simLibName simCellName simViewName simSimulator simViewList simStopList hnlNetlistFileName`. **VERIFIED** ADEL61.
- An `nlAction` property set to `"ignore"` on a stop view makes the netlister skip the instance (analogLib iprobe's auCdl view uses it). **LIKELY** (forum above; KS-LIBINT table).
- `$SUB=<net>` for substrate pins is a common foundry CDL convention. I found **no primary source** showing how it is produced; **GUESS**: via dollarEqualParams or a custom netlistProcedure.

### 3.5 Hierarchical user cells without simInfo

- A schematic+symbol cell with no CDF is netlisted as a subcircuit: spectre writes `subckt <cell> <ports…>` … `ends <cell>`, and the instance as `I0 (nets) <cell> p=v…`. **VERIFIED** (AMPCOP, USCPOSH netlists).
- Port order follows the cell CDF `termOrder` when present. Symbol generation creates the termOrder, sorted. Pins added later are appended (USCPOSH `OUT VDD VSS clk clkb clkb_high IN` versus sorted elsewhere). **LIKELY** (inferred from two netlists).
  - The env var `updateCDFtermOrder` (`auCore.misc`) keeps termOrder in sync. **LIKELY** (Yeager).
  - The `.cdsenv` var `queryCDFtermOrder` controls the "Want to Update" prompt. **VERIFIED** CDFUG614 ch.4.
- To pass parameters, the cell needs CDF params whose **names match the `pPar("x")` names** used inside, typically string type with parseAsNumber and parseAsCEL "yes". `artGenerateHierSymbolCDF(schCv t)` adds them automatically. **VERIFIED** (SKART-FND, ADEL61 p.123 figure).
- The CDF default supplies the value when an instance has no property. **VERIFIED**.
- Whether the spectre `parameters` line carries `x=<default>` or just `x` (USCPOSH shows bare names) is **unknown**; it probably depends on whether the CDF default is non-empty.
- The netlister prints only params listed in that cell's spectre `instParameters` (if any) or in the CDF. **GUESS**: with no simInfo, every CDF param that has a value is passed.

---

## 4. analogLib and basic

### 4.1 Global and supply cells

- analogLib "Global Components": `gnd gnda gndd vcc vcca vccd vdd vdda vddd vee veea veed vrefgnd vss vssa vssd`. All have **no CDF params** and "netlist as a global net". **VERIFIED** ALIBREF ch.7.
- Pin names: the cell's single terminal is named after the global net, e.g. `gnd!` and `vdd!`. **LIKELY** (convention widely reported; NCSU nmos `bn` default `"gnd!"`). **Unknown** for `vrefgnd`.
- Spectre maps `gnd!` to node `0`, and other `!` nets become globals: real header `global 0 vdd!`, then `V0 (vdd! 0) vsource …`. `vdd!` is written unescaped in spectre. **VERIFIED** PVTS.
- The `basic` library has its own `gnd vdd vcc vss` plus `*_inherit` variants carrying a net-expression property for inherited connections. **VERIFIED** BASICREF.

### 4.2 Cell table

Sources:
- Param names and simulator usage: ALIBREF IC20.1. **VERIFIED**.
- Terminal names, defaults and spectre simInfo: NCSU-AP, an older analogLib clone (corroborated by KS-ALIB defaults and CDFUG614 examples). **LIKELY** for modern analogLib.
- Spectre component and instance syntax: ALIBREF examples. **VERIFIED**.

| analogLib cell | Terminals (termOrder) | Key CDF params (default) | Spectre master and param mapping |
|---|---|---|---|
| res | PLUS MINUS | `r` ("1K", resistance), `model`, `w`, `l`, `m` ("1"), `tc1 tc2 trise scale ac dtemp`, `isnoisy` | `resistor`, instParams `(r l w m trise tc1 tc2 resform)`, otherParams `(model)`; e.g. `r1 (1 2) resistor r=1.2K m=2`. auCdl: `R… $[RP]`, instParams `(R)`, propMapping `(nil R r)`. |
| cap | PLUS MINUS | `c` ("1p"), `ic`, `model`, `w`, `l`, `m` ("1"), `tc1 tc2 trise`, `polyCoef` (int 0) + `c1..c20` | `capacitor` `(c w l m trise ic tc1 tc2)`; auCdl `C… $[CP]` |
| ind | PLUS MINUS | `l` ("1n"), `r`, `ic`, `model`, `m` | `inductor` `(l m trise ic)`; e.g. `l33 (0 net29) inductor l=10e-9 r=1 m=1` |
| mind | (no terminals; termOrder nil) | `ind1`, `ind2` (instance names of two `ind`), `k` ("0") | `mutual_inductor coupling=k ind1=… ind2=…` (propMapping coupling→k). Note: the ALIBREF example shows `ml1 mutual_inductor coupling=1 ind1=l1 ind2=l2`. |
| vdc | PLUS MINUS | `vdc`, `acm`, `acp`, `srcType` ("dc", hidden), `tc1 tc2 tnom`, noise `noisefile FNpairs F1..`, `xfm pacm pacp` | `vsource`, propMapping `(nil dc vdc mag acm phase acp type srcType)` gives the real line `V0 (VDD 0) vsource dc=VDD mag=1 type=dc` (**VERIFIED** AMPCOP) |
| vpulse | PLUS MINUS | `v1` ("0"), `v2` ("0"), `per`, `td`, `tr`, `tf`, `pw`, `vdc`, `acm`, `acp`, `srcType` ("pulse") | `vsource type=pulse val0=<v1> val1=<v2> period=<per> delay=<td> rise=<tr> fall=<tf> width=<pw>` |
| vsin | PLUS MINUS | `vo` (offset), `va` (ampl), `freq`, `td`, `theta`, `sinephase` (`phi` in older versions), `vdc acm acp`, FM/AM params, `srcType` ("sine") | `vsource type=sine sinedc=<vo> ampl=<va> freq=… delay=<td> damp=<theta> …` |
| vpwl | PLUS MINUS | `tvpairs` (int 2), `t1 v1 … tN vN`, `td`, `vo`, `scale`, `stretch`, `pwlperiod`, `twidth`, `srcType` ("pwl") | `vsource type=pwl wave=[t1 v1 t2 v2 …]` (built by the netlist proc; **VERIFIED** PVTS) |
| vpwlf | PLUS MINUS | `fileName` | `vsource type=pwl file="…"` (**LIKELY**) |
| vexp | PLUS MINUS | `v1`, `v2`, `td1`, `tau1`, `td2`, `tau2`, `vdc acm acp` | `vsource type=exp val0= val1= td1= tau1= td2= tau2=` |
| vsource | PLUS MINUS | see 4.3 | `vsource` |
| idc / ipulse / isin / ipwl / iexp / isource | PLUS MINUS | as the v-versions, with `idc`, `i1`, `i2`, `io`, `ia` in place of the voltage names; `m` present | `isource`, e.g. `i1 (in 0) isource dc=0 type=pulse delay=10n val0=0 val1=500u period=500n rise=1n fall=1n width=250n`. ALIBREF writes the instance syntax as `Name ( sink src ) isource` (current flows from PLUS through the source to MINUS). |
| vcvs | PLUS MINUS NC+ NC- (out+, out−, ctrl+, ctrl−) | `egain` ("1.0"), `maxv minv`, `r1 r2 f3db td ic`, `csType` ("linear", cyclic) | `vcvs gain=<egain>`; e.g. `e1 (out1 0 pos neg) vcvs gain=10` |
| vccs | PLUS MINUS NC+ NC- | `ggain` ("1.0"), `m` | `vccs gm=<ggain>` |
| cccs | PLUS MINUS | `fgain` ("1.0"), `vref` (name of a controlling **vsource instance**), `m` | `cccs gain=<fgain> probe=<vref>`; e.g. `vcs (pos gnd) cccs gain=2.5 probe=v1 m=1` |
| ccvs | PLUS MINUS | `hgain` ("1.0"), `vref` | `ccvs rm=<hgain> probe=<vref>` |
| diode | PLUS MINUS | `model`, `area`, `m` ("1"), `region` ("on"), `off`, `Vd`, `pj`, `l w`, `trise` | master = model; `(area m trise region)`; auCdl `D… $[DP]` |
| nmos / pmos (3-term) | D G S, bulk via `bn` param (default "S" in old analogLib, `gnd!`/`vdd!` in NCSU) | `model`, `w`, `l`, `m`, `ad as pd ps nrd nrs ld ls`, `region`, `off`, `Vds Vgs Vbs`, `trise`, `bn` | master = `model`; spectre `(w l as ad ps pd nrd nrs ld ls m trise region)`; ALIBREF example `M0 (net3 net1 net2) nmos` |
| nmos4 / pmos4 | D G S B | same as nmos minus `bn` | `M0 (net1 net3 net4 net2) nmos4` (master = model or the cell name) |
| npn / pnp | C B E (+S optional; old: `progn(bn)`) | `model`, `area`, `m`, `region` ("fwd"), `Vbe Vce off`, `areab areac`, `trise` | master = model; `q1 (vcc net3 minus) npn_mod region=fwd area=1 m=1`; auCdl `Q… $[NP]` (**LIKELY**) |
| iprobe | PLUS MINUS (positive current flows PLUS→MINUS) | none | `ip (1 0) iprobe` (a zero-volt source); namePrefix "IPRB". Its auCdl view carries `nlAction=ignore`. |
| switch (analogLib) | t1 t2 ps ns (4-terminal relay) | `vt1 vt2 ropen rclosed`… | `relay` (ALIBREF: "Four Terminal Relay"; e.g. `rel1 (1 2 ps ns) my_relay ropen=1G rclosed=2`) **LIKELY** |
| sp1tswitch (sp2t/sp3t/sp4t) | t1 t0 (NCSU termOrder) | `position` (cyclic "0"), `offset` | `switch`, e.g. `sw1 (t1 t2 t3) switch dc_position=0 ac_position=1 tran_position=2` (ALIBREF spxtswitch) |
| tline | IN+ IN- OUT+ OUT- (NCSU) | `zo` ("50"), `td`, `freq`, `nl` ("0.25"), `len`, `vel`, `model`, loss params | `tline z0=<zo> td= f=<freq> nl= vel= len=`; ALIBREF example `t1 (1 0 2 0) lmodel z0=100` |
| xfmr | P+ P- S+ S- (NCSU); ALIBREF syntax `( t1 b1 t2 b2 )` | `n1`, `n2`, `k`, `pi`, `si` | `transformer n1= n2=`; e.g. `tr1 (1 0 2 0) transformer n1=3 n2=3 m=2` |
| presistor / pcapacitor / pinductor / pdiode | PLUS MINUS | like res/cap/ind/diode | resistor/capacitor/…; auCdl componentName `presistor` / `pcapacitor` |
| port | PLUS MINUS | `num`, `r` ("50"), `srcType`, `vdc`, and all vsource waveform params | `port num=2 r=50 type=pulse …` |
| cdsParam | not a cell: an ILLabel function `cdsParam(n)` on symbols | | |
| noConn (basic) | one pin | none | Suppresses the floating-pin check; not netlisted. **VERIFIED** BASICREF. Terminal name **unknown**. |
| ipin / opin / iopin (basic) | schematic pin masters | none | used by `schCreatePin(cv dbOpenCellViewByType("basic" "ipin" "symbol") name "input" …)`. **LIKELY** (forum examples). The iopin view may be `symbol` (forum) or `symbolr` (my memory, **GUESS**). BASICREF also lists `in/io/out` and `sympin` (symbol-pin masters via `schSymbolPinMasters`). |
| cds_thru (basic) | 2 pins | | Spectre: `I0 (out1 out2) iprobe`; CDL: a small resistor. **VERIFIED** BASICREF. |

### 4.3 vsource `srcType` (the multi-function source)

- Choices in IC20.1: **`dc`, `sine`, `pulse`, `exp`, `pwl`, `pwlz`, `bit`, `prbs`**. **VERIFIED** ALIBREF ("Symbol: vsource"). Older releases had only `dc pulse sine exp pwl` (**LIKELY**, KS-ALIB). In KS-ALIB's era the vsource default was "sine"; in modern releases it is **unknown**, so set it explicitly.
- Params per type (CDF names; ALIBREF table, **VERIFIED**):
  - all types: `vdc` (DC level), `acm`/`acp` (AC magnitude/phase, used only in AC analysis), `xfm`, `pacm`/`pacp`, noise `noisefile FNpairs F1..F50 N1..N50`, `tc1 tc2 tnom`, `m`.
  - **pulse**: `v1` (val0), `v2` (val1), `per`, `td`, `tr`, `tf`, `pw`.
  - **sine**: `va` (ampl 1), `freq`, `sinephase`, `sinedc`, `td`, `theta` (damping), `numofsines` (bool) → `va2 freq2 sinephase2`, `modulation` (bool) → `fmmodindex fmmodfreq ammodindex ammodfreq ammodphase`, `fundname`.
  - **exp**: `v1 v2 td1 tau1 td2 tau2`.
  - **pwl**: `pwlEntryMethod` (radio, default "File") → `fileName`, or `tvpairs` + `t1..t50`/`v1..v50`; `offset scale stretch pwlperiod twidth td`.
  - **bit**: `data`, `td01 td10`, `rjseed rjrms pjamp pjfreq` …
  - **prbs**: `lfsrtaps lfsrseed seed lfsrmode`, plus trigger options that add two ports.
  - display toggles `tempParam smallSig noiseParam` (bool).
- Bridge advice: emit the **single-purpose cells** (`vdc`, `vpulse`, `vsin`, `vpwl`, `vexp`, and the i-versions), whose `srcType` is a hidden fixed string. That avoids depending on vsource's version-specific display toggles.

---

## 5. Libraries on disk

### 5.1 cds.lib (and lib.defs)

**VERIFIED** CAIUG ch.5 (corroborated: cmosedu sample <https://cmosedu.com/cmos1/cadence/cds_lib_IC61.txt>, FPDK45 `cdssetup/cds.lib`).

| Statement | Meaning |
|---|---|
| `DEFINE lib path` | Logical name → directory. One directory cannot back two libraries. Errors if the path is missing. |
| `SOFTDEFINE lib path` | Same as DEFINE, silent when the path is missing. |
| `UNDEFINE lib` | Removes an earlier definition; not an error if undefined. |
| `INCLUDE file` | Reads another cds.lib-format file. Paths inside it are relative to *that* file. Errors if missing or recursive. |
| `SOFTINCLUDE file` | Same as INCLUDE, silent when missing. |
| `ASSIGN lib TMP dir` / `ASSIGN AllLibs TmpRootDir dir` | temporary directory for derived data |
| `ASSIGN lib DISPLAY attr`, `ASSIGN combo COMBINE libA libB` | Library Manager display only |
| `UNASSIGN lib attr` | |

- Syntax: one statement per line. Comments start with `#` or `--`, followed by whitespace. Keywords are case-insensitive. Library names are case-sensitive. `$VAR` and `~` are expanded. Relative paths are relative to the cds.lib being read. `$(compute:THIS_TOOL_INST_ROOT)` names the install root.
- The first `cds.lib` found via `setup.loc` (CSF) is used, and DEFINE lines accumulate depth-first.
- `lib.defs` is **not supported in Virtuoso since IC6.1.4**; it remains only for OA command-line tools. Its format is the same as cds.lib, including `ASSIGN lib libMode shared` (FPDK45 lib.defs).
- `ddCreateLib` appends the DEFINE automatically (5.3).

### 5.2 cdsinfo.tag, .oalib, master.tag, categories

- `cdsinfo.tag`: an ASCII file of `KEY value` lines. Keys: `CDSLIBRARY` (marks the directory as a library root), `CDSLIBCHECK ON|OFF`, `DMTYPE none|crcs|tdm|sync|…` (lower-cased), `NAMESPACE LibraryUnix|LibraryNT`. It is searched upward from the object's path. **VERIFIED** CAIUG ch.4.
  - A real minimal body, after the stock comment header, is exactly `CDSLIBRARY` / `NAMESPACE LibraryUnix`. **VERIFIED** (<https://github.com/markemer/EDA-Cadence/blob/master/t/perl_lib_tests/cdsinfo.tag>; AMPCOP `cdsinfo.tag`).
  - FPDK45's device library has **no** cdsinfo.tag and still works (CDSLIBCHECK defaults OFF). **VERIFIED** (tree).
- `.oalib` (OA library marker, XML). **VERIFIED** AMPCOP / FPDK45:
  ```xml
  <?xml version="1.0"?>

  <Library DMSystem="oaDMFileSys">
      <oaDMFileSys libReadOnly="No"
                   origFileSystem="Unix"/>
  </Library>
  ```
  FPDK45 has the shorter `<Library DMSystem="oaDMFileSys">\n</Library>` form. Whether Virtuoso *needs* it or recreates it is **unknown**. **LIKELY** that OA creates it when ddCreateLib makes the library.
- Layout: `<lib>/<cell>/<view>/`; cell and view names may not contain `.`. Library-level and cell-level loose files need an extension, or tools treat them as cells or views. **VERIFIED** CAIUG ch.2.
- `master.tag` names a view's master file. Real content is two lines: **VERIFIED** (AMPCOP, FPDK45, ucb-art/laygo)
  ```
  -- Master.tag File, Rev:1.0
  sch.oa
  ```
  Masters: `sch.oa` (schematic), `symbol.oa` (symbol, and also simulator stop views such as `spectre/`, `auCdl/`, `auLvs/`, `hspiceD/`, `ams/`, which in FPDK45 are copies of the symbol), `layout.oa`, `veriloga.va`, `verilog.v`.
  - Master-resolution rule: master.tag if present; else the only file; else `vhdl.vhd` or `verilog.v`. **VERIFIED** CAIUG.
- Other files seen: `<cell>/data.dm`, `<cell>/<view>/data.dm` (co-master property bags), `thumbnail_128x128.png`, `netlist.oa` in veriloga views, and `<lib>/*.Cat` category files:
  ```
  TDMCHECKPOINT="1.0"
  <lib>/<cell> type="cell"
  ```
  **VERIFIED** (AMPCOP `Basic_TB.Cat`, ucb-art/laygo veriloga view).
- Techfile: CDB libraries kept it in `techfile.cds`; OA libraries keep it in `tech.db` (FPDK45 `NCSU_TechLib_FreePDK45/tech.db`). **VERIFIED** (tree, cdsp4 doc).

### 5.3 Creating libraries and views from SKILL

- `ddCreateLib(t_libName [t_libPath])` → ddId. **VERIFIED** SKDF.
  - It creates the directory and appends `DEFINE` to the cds.lib that `ddUpdateLibList` found.
  - If that cds.lib is not in the cwd, it writes a new `./cds.lib` containing a `SOFTINCLUDE` of the old one plus the DEFINE.
  - A missing path means `cwd/libName`; a relative path is relative to the cwd.
  - It does not attach a technology.
- Attaching a tech library (needed for layout, not for schematics):
  - `techBindTechFile(libId "techLibName")` is the GUI "Attach" equivalent;
  - alternatively `techSetTechLibName(libId "techLibName")`.
  - **LIKELY** (forum summary of Beckett, <https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/51208>; Rk example in <https://comp.cad.cadence.narkive.com/1xI4WOh1/create-a-tech-lib-with-skill>).
- A schematic-only library needs no attachment: schematics use `basic`/`cdsDefTechLib` layers. **LIKELY** (cdsDefTechLib is DEFINEd in FPDK45's cds.lib; analogLib itself has no PDK tech).
- `ddGetObj(lib [cell] [view] [file] [contextId] [mode])`. **VERIFIED** SKDF. Modes:
  - `"r"` (default);
  - `"w"`: create new master data; **fails if the view already has a master**; the first file created in a view becomes its master;
  - `"wc"`: co-master;
  - `"wd"`: derived; needs a master context;
  - `"a"`: modify, creating if absent;
  - `"ad"`: derived, modify or create.
  - It creates cell and view directories as a side effect.
  - Pair a placeholder create with `ddReleaseObj(id)` if you end up not writing it.
  - Write the file with `outfile(ddGetObjWritePath(id))`.
  - Do not use ddGetObj for OA cellviews; use `dbOpenCellViewByType`.
- `dbOpenCellViewByType(lib cell view [viewType [mode [contextCv]]])`: view types include `"schematic"`, `"schematicSymbol"`, `"maskLayout"`; mode `"w"` creates. **VERIFIED** for the signature (SKDF). The viewType strings are **LIKELY**.

Text view (Verilog-A) from SKILL (illustrative, my own code):

```skill
procedure( schemifyWriteVaView(lib cell text)
  let( (id port)
    ; "a" so re-export overwrites; "w" fails once a master exists
    id = ddGetObj(lib cell "veriloga" "veriloga.va" nil "a")
    unless(id error("schemify: cannot create %s/%s/veriloga" lib cell))
    port = outfile(ddGetObjWritePath(id))
    fprintf(port "%s" text)
    close(port)
    ; let Virtuoso parse it and (re)build the pin info used by the netlister
    ahdlUpdateViewInfo(lib ?cell cell ?view "veriloga")
    t))
```

Status of the facts behind this:
- master.tag naming `veriloga.va` and a `netlist.oa` alongside it: **VERIFIED** (ucb-art/laygo, <https://github.com/ucb-art/laygo/tree/master/generators/splash/adc_sar_testbenches/dac_8bit_ideal/veriloga>).
- `netlist.oa` holding the interface the netlister reads; editing the `.va` outside Virtuoso leaves it stale until re-parsed: **LIKELY** (forum summary).
- `ahdlUpdateViewInfo` re-parsing and updating the cellview CDF (viewInfo): **VERIFIED** for the signature (SKART-FND). It may pop a port-mismatch dialog (**LIKELY**, forum).
- Whether ddGetObj with a fresh view writes master.tag by itself: **LIKELY**. SKDF says the first file in a view "is marked as the master", and its master.tag note implies the file gets written.

### 5.4 How PDKs are delivered

- The bundle (GPDK17 / GPDK045, **VERIFIED**):
  - a `cds.lib` that users `INCLUDE`;
  - `lib.defs` for OA tools;
  - the PDK OA library (symbols, stop views, layouts/pcells, CDF in `data.dm`, `libInit.il`);
  - `techfile` ASCII and `display.drf` (loaded by the library's `libInit.il` in GPDK045);
  - a `models/spectre/<pdk>.scs` file with corner **sections** (GPDK045: `tt ff ss fs sf mc`; GPDK17: `NN FF SF FS SS`);
  - PV rule decks (assura/pvs/qrc), stream layer maps, docs.
- The environment requires `CDS_Netlisting_Mode=Analog`. Design libraries **attach** to the PDK tech library.
- CDF and callback code ship either as SKILL dump files loaded at startup (FPDK45: `ncsu_basekit/skill/cdf/<CELL>.il`, `callbacks.il`, `globalData.il`, `loadSkill.il` loaded via `$PDK_DIR`) or baked into `data.dm`, with callbacks loaded by `libInit.il`. **VERIFIED** (FPDK45 tree; CDFUG614 ch.7).
- `libInit.il` / `libInit.ils` is the only allowed file name. It sits in the library root and is loaded the first time the library is accessed, typically doing `load(ddGetObj("lib" nil nil "file.il")~>path)`. **VERIFIED** CDFUG614 ch.7.
- `.cdsinit` is SKILL run at Virtuoso startup. `.cdsenv` is a defaults file of lines `tool[.partition] varName type value [nil]`, e.g. `layout xSnapSpacing float 0.0025` (FPDK45) and `cdf.editor defaultDisplay cyclic "SelectionThenLast" nil` (CDFUG201). The SKILL equivalent is `envSetVal("tool.part" "var" 'type value)`. **VERIFIED**.
- ADE model setup from SKILL. **VERIFIED** ADEL61:
  - `envSetVal("spectre.envOpts" "modelFiles" 'string "./m1.scs ./m2.scs")`; a `#;` prefix disables a file;
  - or per session `asiSetEnvOptionVal(asiGetTool('spectre) "modelFiles" list(list("/p/model.scs" "tt") …))`;
  - the result in the netlist is `include "<file>" section=<sec>` (real: `include ".../crn22ull….scs" section=FFMacro_MOS_MOSCAP`, AMPCOP);
  - the `includePath` and `definitionFiles` vars sit alongside `stopViewList` (default `"spectre"`) and `switchViewList`.
  - The `(path section)` pair format is **LIKELY**.

---

## 6. PDK device conventions

- MOS terminal order is **D G S B** in every PDK seen (GPDK045 "Four terminals (D, G, S, B)", FPDK45 termOrder, IHP ngspice `.subckt sg13_lv_nmos d g s b`). **VERIFIED**.
  - GPDK045 also ships `_3` variants with an inherited bulk (a netSet param). **VERIFIED** GPDK045 §11.
- Spectre netlisting of a PDK MOS: master = the `model` param value, or a hard-coded componentName. GPDK045: cell `nmos1v` → model `g45n1svt`, giving `NM1 (D G S B) g45n1svt w=4u l=180.0n nf=1 as=… ad=… ps=… pd=… m=1 nrs=… nrd=… sa=… sb=… sd=…`. **VERIFIED** GPDK045 datasheet.
- CDL netlisting of the same device: `MNM1 D G S B g45n1svt W=4u L=180.0n M=1.0` (uppercase W/L/M via propMapping). **VERIFIED** GPDK045, NSSAR.
- Param names vary by foundry:
  - analogLib/FPDK45: `w l m ad as pd ps nrd nrs ld ls sa sb sd region trise`.
  - GPDK045: Total Width `w`, Finger Width, Fingers (`nf` in the netlist), `m`, plus layout-only params (bodytie, S/D connection, "Edit Area & Perim" …).
  - TSMC 22ULL in the wild uses `multi=` (not `m`), plus `nf`, `sd`, `spa`, `spa1`, and long CEL expressions for `ad/as/pd/ps` (**VERIFIED** AMPCOP netlist).
  - IHP SG13G2 model subckt params are `w l ng m as ad pd ps trise …` (**VERIFIED** <https://github.com/IHP-GmbH/IHP-Open-PDK/blob/main/ihp-sg13g2/libs.tech/ngspice/models/sg13g2_moslv_mod.lib>). The Cadence cell/CDF names in IHP's Virtuoso kit are **unknown**: there is no public Cadence kit, and `libs.tech/` has no cadence directory.
  - `fw`/`wf` (finger width) and `sigma` exist in some PDKs; I found **no primary evidence** to pin the exact names. **GUESS**.
- Values are stored as **strings with SI suffixes** (`"90n"`, `"1u"`; FPDK45 globalData `?MinW "90n" ?MinL "50n"`). Callbacks may rewrite them to `%g` form (`"9e-08"`). Some PDK params are `int`/`float`/`boolean`/`cyclic`, e.g. GPDK045 bodytie options, NCSU `w_lambda` (int). Schemify must keep each param's CDF type. **VERIFIED**.
- `parseAsNumber`/`parseAsCEL` "yes" on numeric strings means users can type expressions (`wn*2`) or design variables, and both flow verbatim into the netlist. **VERIFIED**.
- The `m` vs `M` hazard: see 1.3. CDF `M` means mega. The CDL writer adds `*.MEGA`. Never write SPICE-style milli `M` into CDF.
- auCdl for PDK MOS commonly uses `componentName nmos/pmos` and `modelName "<model>"`, with `instParameters (m L W)` (FPDK45). `$SUB=` bulk handling: **GUESS** (3.4).

---

## 7. Getting a PDK library's devices out as text

Recommended export pass (illustrative, my own code). It writes, per cell, the base CDF dump plus a symbol dump that Schemify parses.

```skill
procedure( schemifyExportLib(lib outDir)
  let( (port cv)
    cdfDumpAll(lib strcat(outDir "/" lib ".cdf.il") ?level 'base)   ; all cells' base CDF
    foreach(cell ddGetObj(lib)~>cells
      when(member("symbol" cell~>views~>name)
        cv = dbOpenCellViewByType(lib cell~>name "symbol" nil "r")
        port = outfile(sprintf(nil "%s/%s.sym.txt" outDir cell~>name))
        fprintf(port "cell %s bbox %L\n" cell~>name cv~>bBox)
        foreach(term cv~>terminals
          fprintf(port "term %s dir %s pins %L\n" term~>name term~>direction
                  foreach(mapcar p term~>pins p~>fig~>bBox)))
        foreach(s cv~>shapes
          case(s~>objType
            ("label" fprintf(port "label %L %s %L %s\n" s~>xy s~>labelType s~>theLabel s~>layerName))
            ("line"  fprintf(port "line %s %L\n" s~>layerName s~>points))
            ("rect"  fprintf(port "rect %s %L\n" s~>layerName s~>bBox))
            ("polygon" fprintf(port "poly %s %L\n" s~>layerName s~>points))
            ("ellipse" fprintf(port "ellipse %s %L\n" s~>layerName s~>bBox))
            ("arc"   fprintf(port "arc %s %L %L\n" s~>layerName s~>ellipseBBox s~>arcAngles))
            (t       fprintf(port "other %s\n" s~>objType))))
        close(port)
        dbClose(cv)))
    t))
```

- `cdfDumpAll`/`cdfDump`, `cdfGetBaseCellCDF(cell)~>parameters` and `~>simInfo` are the canonical text routes. **VERIFIED**.
  - Because dumps can contain SKILL expressions as defaults (FPDK45), a second pass that evaluates and prints each param is safer: walk `cdf~>parameters` and print `name paramType defValue units parseAsNumber parseAsCEL storeDefault choices`. Evaluated `defValue` is then a literal.
  - The dbObject attribute names used in the symbol dump (`objType`, `labelType`, `theLabel`, `ellipseBBox`, `arcAngles`) are **LIKELY**, from general SKILL DB knowledge; check them on first run.
- Simulator stop views (`spectre`, `auCdl`, …) are usually symbol copies; they matter only for the view lists. **VERIFIED** (FPDK45, GPDK045 §12).
- Precedent tools:
  - **Synopsys Custom Compiler** reads OA libraries plus CDF natively. Foundries ship "iPDKs": OA + CDF + Tcl callbacks + PyCells, with SKILL callbacks kept for Virtuoso compatibility. **LIKELY** (EDN/Si2 and GF press coverage, e.g. <https://www.edn.com/si2-rolls-open-pdk-effort/>).
  - **Xic** (wrcad) reads Cadence schematic, symbol and layout views via OA and "to varying degrees" obtains a simulatable hierarchy. It can dump a pcell's CDF to a `.cdf` file, but cannot write schematics back to Cadence without corruption. **VERIFIED** (<http://www.wrcad.com/manual/xicmanual/node11.html>, <http://wrcad.com/manual/xicmanual/node806.html>).
  - **oaFileParser** is a low-level reader of the `.oa` format (<https://github.com/EDDRSoftware/oaFileParser>), possibly a way to read without Virtuoso. Its maturity is **unknown**.
  - KLayout does not read OA schematics. **LIKELY**.
  - **vcli** (MIT, Rust; <https://github.com/deanyou/virtuoso-cli>) is a TCP SKILL bridge daemon plus CLI. It is useful prior art for driving Virtuoso from outside; check its license before borrowing ideas, and don't copy code.
- Public sample dumps:
  - FPDK45 `ncsu_basekit/skill/cdf/{N,P}MOS_{VTL,VTG,VTH,THKOX}.il` (Apache-2.0 upstream): a complete real PDK MOS CDF with callbacks.
  - NCSU CDK `NCSU_Analog_Parts/*/artSimInfoCDF.att`: analogLib-style CDF for every source and passive.
  - analogLib `nbsim` (CDFUG614 App. C) and `npn` (KS-LIBINT) dumps.
  - FreePDK15 equivalents: <https://github.com/zonyzhao/Cadence/tree/master/ncsu-FreePDK15/cdslib/skill/cdf>.
  - Inductor pcell CDFs using `ansCdlSubcktCall` exist in <https://github.com/taoyilee/pcell> (GPL-3.0; do not copy).
  - **No public IHP SG13G2 Cadence CDF dump found.** The IHP open PDK ships xschem/ngspice/qucs/klayout/etc. but no Cadence kit (checked `ihp-sg13g2/libs.tech/` listing).

Side note: Schemify already has `crates/import/src/cdf.rs` (found by GitHub code search for `cdfDump analogLib`). Compare it against 1.5 when this research is used.

---

## Open unknowns (need a Virtuoso run or a user with access)

1. Whether `cdfgData->p->value = <default>` on an instance CDF **deletes** the instance property (storeDefault no) or stores it.
2. The exact runtime `cdf~>type` strings for each level; only `"instData"`, `"cellData"` and `"userCellData"` have been seen.
3. Whether `dbCreateInst` plus nothing else leaves *zero* CDF properties for every PDK, and whether pcell masters need `dbCreateParamInst` with an explicit param list (expected yes for layout; schematic symbols are usually not pcells).
4. The exact vsource `srcType` default and the visibility toggles in current IC23.1; the full propMapping of modern analogLib (NCSU is older).
5. Terminal names of analogLib `gnd`/`vdd` (assumed `gnd!`/`vdd!`), basic `noConn`, and whether iopin's view is `symbol` or `symbolr`.
6. Spectre subckt `parameters` line: whether CDF defaults are written (`x=1u`) or only names (`x`).
7. Port order when a cell has no CDF termOrder: alphabetical, or pin creation order?
8. How `$SUB=` and `$[model]` are chosen by ansCdlCompPrim for each device class (rule inferred, not documented).
9. Whether ddGetObj `"w"` on a new view writes `master.tag` itself, and whether `netlist.oa` must exist before spectre netlists a veriloga stop view (`ahdlUpdateViewInfo` assumed to build it).
10. Whether `.oalib` must be present for ddCreateLib-made libraries (it is probably created by OA).
11. Which PDK callbacks fail without a form (cdfgForm field widgets, `hiGetCurrentForm`) in TSMC/GF/IHP kits. Needs a per-PDK smoke run that logs each `errset` failure.
12. IHP SG13G2 Virtuoso kit cell names and CDF params (the kit is not public).
13. Legal: whether Cadence-manual-derived facts are acceptable evidence under D1 (clean room).
