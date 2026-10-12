# Virtuoso SKILL database API for schematics and symbols, plus SKILL file I/O

Research for the Schemify ⇄ Virtuoso interop bridge. We have no Virtuoso, so the SKILL we
ship has to work the first time it runs. Each fact below names its source and a confidence level:

- **VERIFIED**: read in Cadence reference text (signature or attribute table).
- **LIKELY**: several independent examples, or one live-tested report, agree.
- **GUESS**: an inference. A probe in §9 settles it on first contact.

Clean-room note: the sources are Cadence manuals (proprietary, not GPL) and MIT-licensed
bridge code, read for API facts only. No xschem source was opened. All snippets here are
new code written for this file. Signatures are facts and appear verbatim; prose is paraphrased.

## Source keys

| Key | Source | Notes |
|---|---|---|
| **[SCH20]** | *Virtuoso Schematic Editor SKILL Reference*, ICADVM20.1, Oct 2020 (`skcompref.pdf`). Mirror: https://github.com/wyz569/virtuoso_20.1/blob/main/doc/skcompref/skcompref.pdf | Authoritative for `sch*`. Covers IC6.1.8 and ICADVM18+. |
| **[DB20]** | *Virtuoso Design Environment SKILL Reference*, ICADVM20.1 (`skdfref.pdf`), same mirror `doc/skdfref/skdfref.pdf` | Authoritative for `db*`/`dd*` (OpenAccess era) and the attribute tables. |
| **[LANG20]** | *Cadence SKILL Language Reference*, ICADVM20.1 (`doc/sklangref/sklangref.pdf`) | Core language and I/O. |
| **[LUG20]** | *Cadence SKILL Language User Guide*, ICADVM20.1 (`doc/sklanguser/sklanguser.pdf`) | Syntax rules, string limits. |
| **[UG20]** | *Virtuoso Schematic Editor User Guide*, ICADVM20.1 (`doc/comphelp/comphelp.pdf`) | Naming conventions, LPP table, NLP syntax, schConfig variables. |
| **[BASIC20]** | *Virtuoso Basic Library Reference*, ICADVM20.1 (`doc/basicLib/basicLib.pdf`) | basic/ipin, noConn, … |
| **[TECH20]** | `doc/finder/SKILL/Tech_File/sktechfile.fnd` and `doc/techfileuser/chap*.html`, same mirror | techBindTechFile etc. |
| **[DB04]** | *Design Framework II SKILL Functions Reference*, PV 6.3, July 2004 (CDB era). http://pwp.gatech.edu/wp-content/uploads/sites/367/2016/03/Cadence_functions_reference.pdf | Older, but its attribute and argument-type tables mostly carry over. |
| **[LANG03]** | *SKILL Language Reference* 06.10 (2003). https://github.com/hanun2999/cadence-skill/blob/main/SKILL%20Language%20Reference%20.pdf | Agrees with [LANG20] on the I/O functions. |
| **[VCLI]** | `deanyou/virtuoso-cli` (MIT), a SKILL bridge **live-tested on IC23.1**. https://github.com/deanyou/virtuoso-cli, especially `src/client/schematic_ops.rs`, `.pensieve/knowledge/*`, PR #115 https://github.com/deanyou/virtuoso-cli/pull/115 | Empirical. Some notes there contradict the docs; each conflict is flagged below. |
| **[NG-x]** | comp.cad.cadence threads on narkive: [creating schematic by SKILL](https://comp.cad.cadence.narkive.com/ISd3ySl2/creating-schematic-by-skill), [ILLabels](https://comp.cad.cadence.narkive.com/xRig47Br/skill-q-creating-illabels-in-the-schematics), [cdsTerm labels](https://comp.cad.cadence.narkive.com/GTVMi6kf/change-cdsterm-labels), [schematic pins](https://comp.cad.cadence.narkive.com/jyy2qlfI/schematic-pins), [pin per instTerm](https://comp.cad.cadence.narkive.com/8Q8KVusb/is-it-possible-to-create-pin-for-each-inst-terminal-using-skill), [DBUPerUU](https://comp.cad.cadence.narkive.com/lQuwRJ8K/mixed-library-settings-of-dbuperuu-for-schematic-and-schematicsymbol) | Forum evidence. Andrew Beckett (Cadence) posts in several of these threads. |
| **[PEAR]** | Peardrop (Glade) forum, Keith's replies: https://peardrop.co.uk/forums/users/keith/replies/page/8/ | Says schematic/symbol dbuPerUU is always 160, "to be compatible with Cadence". |

The Cadence community forum (community.cadence.com) answers HTTP 403 to automated fetches, so
none of its threads were read directly.

---

## 1. Opening, creating, saving, libraries, units, checking

### 1.1 `dbOpenCellViewByType`: VERIFIED [DB20]

```
dbOpenCellViewByType( { gt_lib | nil } t_cellName lt_viewName
                      [ t_viewTypeName [ t_mode [ d_contextCellView ] ] ] )
  => d_cellView / nil
```

- `gt_lib` is a library name string or a ddId. The library must already be in the libList (cds.lib).
- `lt_viewName` is one view name, a space-separated string of names, or a list. The first match opens.
- `t_viewTypeName`: **schematics use `"schematic"`, symbols use `"schematicSymbol"`** (stated
  outright in [DB20]). To *create* a cellview you must give a viewType. If it is `nil`, `""` or
  omitted, the cellview must already exist.
- `t_mode` (default `"r"`):
  - `"r"`: read. The cellview must exist.
  - `"a"`: append. Opens an existing cellview, or creates it.
  - `"w"`: write. Creates the cellview, or **truncates the in-memory contents** of an existing one.
  - `"s"`: scratch. Like `w`/`a`, but it can never be saved.
  - `"wc" "wd" "ac" "ad" "sc" "sd"`: variants for non-master cellviews and context cellviews. Not needed here.
- Returns `nil` on any error. It does not signal, so always test the result.
- Only one user/process can hold a cellview in `a`/`w` mode at a time [DB20 dbClose].

To change the mode of an already-open cellview, use `dbReopen(d_cv t_mode) => t/nil`, with mode
`"r"`, `"a"`, `"w"` or `"s"`. Going from edit to `"r"` discards unsaved changes. VERIFIED [DB20]

Related calls:
- `dbFindOpenCellView(g_libId t_cell t_view) => d_cv/nil`
- `dbGetOpenCellViews() => list`
- `cv~>mode` returns `"r" "w" "a" "s"`

VERIFIED [DB20]

**Conflict, [VCLI] on IC23.1.** VCLI's notes claim that (a) the 3-argument form opened in `"a"`
mode, and (b) the 5-argument form with viewType `"schematic"` returned nil. Both contradict
[DB20] and many forum examples [NG-creating].

The likely cause is that the cellview was already open, in the GUI or held by OCEAN.
`dbOpenCellViewByType` then returns the existing in-memory cellview with *its* mode, or nil when
the requested mode conflicts. VCLI's own fallback (`dbGetOpenCellViews` filtered by
lib/cell/view/mode) supports this reading. Confidence: LIKELY.

Bridge rule: before opening, call `dbFindOpenCellView(ddGetObj(lib) cell view)`. If it finds a
cellview, use it, calling `dbReopen(cv "a")` when you need to write.

VCLI also records a destructive incident: opening with `"w"` "just to look" replaced a 38 KB
schematic with an empty one once something saved it. **Read-only paths must use `"r"`**, for example
`dbOpenCellViewByType(lib cell "schematic" nil "r")`. LIKELY [VCLI `.pensieve/knowledge/cadence-ic23-dbopencellviewbytype`]

### 1.2 Save, close, purge: VERIFIED [DB20]

- `dbSave(d_cv [ d_destCv | t_lib t_cell t_view ]) => t/nil`
  - Saves a cellview opened in `w` or `a` mode. An `r` or `s` cellview cannot be saved over itself.
  - A save clears the undo checkpoints unless the cdsenv variable `dbUndoAcrossSave` is t.
- `dbClose(d_cv) => t/nil`
  - Reference counted: memory is freed when closes equal opens.
- `dbPurge(d_cv) => t/nil`
  - Forces the cellview out of memory and loses unsaved edits.
  - Any window showing the cellview goes blank.
  - Every dbObject id from that cellview becomes invalid.
- `dbCopyCellView(d_src t_lib t_cell t_view [nil [nil [g_overwrite]]]) => d_cv/nil`

### 1.3 Libraries: VERIFIED signatures [DB20]; tech attachment LIKELY/GUESS

- `ddGetObj(t_lib [t_cell [t_view [t_file [ctx [t_mode]]]]]) => b_ddId`
  - Finds a library, cell, view or file. `ddGetObj("lib")` returns nil when the library is unknown.
  - Do not use it to open cellviews; use `dbOpenCellViewByType`.
- `ddCreateLib(t_libName [t_libPath]) => b_ddId`
  - Creates the directory if missing.
  - Adds a `DEFINE` to cds.lib. If no cds.lib exists in the cwd, it writes one there that
    `SOFTINCLUDE`s the cds.lib found by the normal search.
  - Relative paths resolve against cwd.
- `dbCreateLib(t_libName [t_libPath]) => ddId/nil`
  - The older call. Its path semantics are odd: `t_libPath` is the *parent* directory.
- `ddGetLibList()` lists the libraries; `(ddGetLibList())~>name` gives their names.

**Technology for a schematic-only library.** In OpenAccess, every technology database implicitly
references `cdsDefTechLib`, which defines all system-reserved layers and purposes: `wire`,
`device`, `pin`, `instance`, `annotate`, `text`, `drawing1..9`, and so on [TECH20 techfileuser].

The technology docs say a design library must either *reference* or *attach* a technology library
to get technology data [TECH20]. The GUI's New Library form, however, offers **"Do Not Need Process
Information"** for schematic-only work. LIKELY: course notes (e.g. Georgia Tech lab handouts) show
this option.

SKILL to attach a library, VERIFIED signatures from [TECH20]:
```
techBindTechFile( g_ID [ t_techLibName [ t_binaryName [ updateDev ] ] ] ) => t / nil   ; g_ID from ddGetObj
techSetTechLibName( g_ID t_libName ) => t / nil   ; sets the "techLib" property on the library
techGetTechLibName( g_ID ) => t_name / nil
```

**Recommendation: GUESS, pending the probe.** Do not create libraries in v1. Ask the user for an
existing library: they always have one that already holds schematics. If we must create one,
call `ddCreateLib(name path)`, then `techSetTechLibName(ddGetObj(name) "<PDK tech lib or analogLib's tech lib>")`.
Before writing, probe that a schematic opens in `"w"` mode and that `dbCreateLine(cv list("wire" "drawing") …)`
returns non-nil.

### 1.4 Units: `cv~>DBUPerUU`, `cv~>userUnits`, `cv~>bBox`

| Attribute | Settable | Type | Meaning |
|---|---|---|---|
| `DBUPerUU` | yes | float | database units per user unit |
| `userUnits` | yes | string | `"micron" "millimeter" "centimeter" "meter" "mil" "inch"` |
| `bBox` | no | bBox | the cellview bounding box, in user units |

Source: [DB20] attribute table. VERIFIED.

- **Schematic and schematicSymbol: user unit `"inch"`, DBUPerUU `160`.** LIKELY.
  - [PEAR] says it is always 160 for Cadence compatibility.
  - The comp.cad.cadence DBUPerUU thread [NG-DBUPerUU] says the same. It also notes that legacy
    libraries had odd values (5, 100). On OpenAccess the value lives only in the technology file.
  - Technology files declare it via `viewTypeUnits( (schematic "inch" 160) (schematicSymbol "inch" 160) )`.
  - **Always read `cv~>DBUPerUU` at runtime. Never hard-code 160.**
- All `~>xy`, `~>points` and `~>bBox` values are **floats in user units (inches)**. For example
  `inst~>xy => (0.0 3.0)` [VCLI]. The default schematic snap is `0.0625` inch (1/16″ = 10 DBU), as
  in every `schCreateWire` example.
- Exact export: `round(x * cv~>DBUPerUU)` gives an integer DBU. Every legal coordinate is a multiple
  of 1/160 inch.
  - Use `round`, not `fix`: [LANG20] shows `fix(4.1*100) => 409`.
  - `round` clamps to the 32-bit int range with a warning [LANG20].
- [DB20] caps a point list at **4000 points**. VERIFIED.

### 1.5 `schCheck`, `schExtractConn`, `schClearConn`: VERIFIED [SCH20]

```
schCheck( d_cvId ) => l_errors        ; list(nErrors nWarnings)
schExtractConn( d_cvId ) => l_result  ; list(errors warnings), deprecated in favour of schCheck
schClearConn( d_cvId ) => t / nil
```

**`schCheck`:**
- Extracts connectivity (when env `updateConn` is set), runs the schematic rules checker
  (`runSRC`) and the cross-view checker (`runVIC`).
- The returned counts include SRC and VIC findings. Read `car(res)` for errors and `cadr(res)` for warnings.
- You need write permission on the cellview.

**What extraction reads:**
- Figures on layer `wire` with purpose `drawing`, `flight` or `label` are processed.
- Figures on `pin`/`drawing` are processed as schematic pins.
- Instances have purpose `cell` or `pin`.
- Labels within env `maxLabelOffsetUU` of a wire are auto-glued to it.

**What `schClearConn` (run before each extraction) does:**
- Deletes all markers.
- Deletes all **non-terminal nets**.
- Detaches shapes from terminal nets.
- Breaks explicit net equivalences.
- Removes inherited net expressions on nets.
- Detaches instance pins from terminal nets.

**Consequence: connectivity is derived from geometry plus labels.** A connection made only with
`dbCreateInstTerm` or `dbMakeNet` is **erased by the next `schCheck`**, and terminals revert to
`net1..N`. [VCLI] measured this live on IC23.1. LIKELY, and consistent with the [SCH20] text.

**Netlisting needs `schCheck` after edits.** `dbSave` alone leaves the extraction timestamp
stale, and OSSHNL-109 reports "modified since last extraction" [VCLI]. Always run `schCheck(cv)`,
then `dbSave(cv)`. LIKELY. Extraction stamps the cellview with a `"lastSchematicExtraction"`
property of type `"time"` (the [DB20] `~>??` example shows it).

To skip SRC/VIC during the check:
```
schSetEnv("runSRC" nil)
schSetEnv("runVIC" nil)
```
`schSetEnv(t_var g_value) => t/nil` and `schGetEnv(t_var)` are VERIFIED [SCH20].

### 1.6 `~>`, `dbGet`, `dbGetq`: VERIFIED [DB20], [LUG20]

- `obj~>name` is `dbGetq(obj name)`; the name is not evaluated. `dbGet(obj sym)` evaluates its second argument.
- `~>` works on attributes **and** properties. On a name clash, **the attribute wins**: a property
  that shares an attribute's name cannot be read through `~>`. Use `dbGetPropByName` or walk
  `obj~>prop` instead.
- `~>` applied to a *list* of objects maps over it: `cv~>instances~>name` is a list of names
  ([LUG20], "squiggle arrow").
- `obj~>?` lists the attribute names; `obj~>??` gives a property list of every attribute and its
  value. Use these to explore unknown objects in the probe.
- Assignment works for settable attributes: `fig~>bBox = list(…)`, `label~>labelType = "ILLabel"`.
- **User *boolean* properties read back as the strings `"TRUE"`/`"FALSE"`**, not `t`/`nil`, and
  time properties read back as strings [DB20 dbGet]. VERIFIED.

### Snippets §1

```skill
;; 1a. Open for edit, reusing a copy that is already open (avoids the nil-on-conflict trap).
procedure(SfyOpenEdit(lib cell view vtype)
  let((ddLib cv)
    ddLib = ddGetObj(lib)
    unless(ddLib error("SfyOpenEdit: library %s is not in cds.lib" lib))
    cv = dbFindOpenCellView(ddLib cell view)
    if(cv then
      unless(cv~>mode == "a" || cv~>mode == "w" dbReopen(cv "a"))
    else
      cv = dbOpenCellViewByType(lib cell view vtype "a"))
    unless(cv error("SfyOpenEdit: cannot open %s/%s/%s" lib cell view))
    cv))
```

```skill
;; 1b. Read-only open plus a units report.
let((cv)
  cv = dbOpenCellViewByType("myLib" "amp" "schematic" nil "r")
  when(cv
    printf("units=%s dbu=%L bbox=%L type=%s\n"
           cv~>userUnits cv~>DBUPerUU cv~>bBox cv~>cellViewType)
    dbClose(cv)))
```

```skill
;; 1c. Check and save; refuse to save if the check found errors.
procedure(SfyCheckSave(cv)
  let((res)
    res = schCheck(cv)                  ; => (nErrors nWarnings)
    printf("schCheck %s/%s: %d errors, %d warnings\n"
           cv~>libName cv~>cellName car(res) cadr(res))
    when(car(res) == 0 dbSave(cv))
    res))
```

---

## 2. Reading a schematic

### 2.1 Instances: VERIFIED [DB20] attribute tables

Attributes common to all instances (generic anyInst):
- `name`, `baseName`, `numInst`
- `libName`, `cellName`, `viewName`
- `master`: the master cellview. It is **nil if the master cannot be found/bound**; then fall back
  to the lib/cell/view strings.
- `instHeader`, `instTerms`

Instance-specific attributes:
- `xy`: point in user units.
- `orient`: `"R0" "R90" "R180" "R270" "MY" "MYR90" "MX" "MXR90"`.
- `transform`: `list(xy orient mag)`.
- `mag`: always 1.0. Magnification is unsupported.
- `conns`, `status`

Generic figure attributes:
- `bBox`: includes label extents.
- `purpose`: schematic pin instances have purpose `"pin"`; ordinary ones `"cell"` [SCH20 schExtractConn].
- `net`, `pin`, `parent`, `children`, `objType` (`"inst"`), `prop`.

Properties:
- `inst~>prop` is a list of prop objects, each with `name`, `valueType`, `value`, `range`, `enum`.
  VERIFIED [DB20].
- `valueType` is one of `"boolean" "fileName" "float" "hierProp" "ILExpr" "ILList" "int" "NLPExpr" "string" "time"`.
- **CDF parameters still at their default are often *not* stored on the instance.** That depends on
  each parameter's CDF "Store Default" flag [UG20]. To get every effective parameter, read the CDF
  rather than `inst~>prop`: `cdfGetInstCDF(inst)~>parameters`, each with `~>name ~>value ~>paramType`.
  LIKELY [VCLI uses `cdfFindParamByName(cdfGetInstCDF(i) "r")~>value`]. CDF details belong to the
  CDF research note.

**Iterated instances.** `name "I0<3:0>"`, `baseName "I0"`, `numInst 4`.
- Instance names allow only `base<N>` or `base<N:M>`: no step, no repeat, no grouping
  [DB20 Instance Name Syntax]. VERIFIED.
- `schCreateInst` creates an iterated instance when given `"I24<0:1>"` [SCH20]. VERIFIED.

**Instance terminals (`instTerm`):** `name` (the master terminal's name), `net`, `term`, `inst`,
`direction`, `numBits`. VERIFIED [DB20]. Two caveats from [VCLI], both LIKELY:
- A terminal with nothing connected may have **no instTerm at all**. For a full pin list, iterate
  `inst~>master~>terminals` and look each one up by name in `inst~>instTerms`.
- `cons` inside `foreach` reverses order. Use `tconc`/`mapcar` or sort by name, and never rely on
  positional order.

**Pin position of an instance terminal.** LIKELY [NG-pin per instTerm], [VCLI]:
```
dbTransformBBox(car(term~>pins)~>fig~>bBox inst~>transform)
```
Here `term` is the master's terminal object.

### 2.2 Shapes and `objType` values: VERIFIED [DB20]

`cv~>shapes` returns every shape; instances are separate, in `cv~>instances`.

| `objType` | Notes |
|---|---|
| `"line"` | `points`, `nPoints`. Schematic narrow wires are lines (`schCreateWire` width 0 ⇒ line) [SCH20]. [VCLI] finds wires with `objType == "line"`. |
| `"path"` | `points`, `width`, `pathStyle`, `beginExt`, `endExt`. Wide wires (`schCreateWire` width > 0) are paths. |
| `"rect"` | the geometry is `bBox` |
| `"polygon"` | `points`, implicitly closed (a rectangle has 4 points, not 5) |
| `"ellipse"` | `bBox` only. A circle is an ellipse with a square bBox. |
| `"arc"` | `ellipseBBox`, `startAngle`, `stopAngle` (radians, CCW sweep from start to stop, **sweep ≤ π**), `bBox` (of the arc portion only) |
| `"dot"` | `xy`, `width`, `height` |
| `"donut"` | `xy`, `innerRadius`, `outerRadius` |
| `"label"` | see §2.4 |
| `"textDisplay"` | `associate`, `owner`, `text`, plus the label display attributes |

Every shape also has `layerName`, `layerNum`, `purpose`, `lpp` (`(layer purpose)`), `net`, `pin`,
`parent`, `children`.

### 2.3 Schematic layer-purpose pairs: VERIFIED [UG20 "schematic objects and LPPs" table]

| Object | LPP |
|---|---|
| Wires (narrow and wide) | `wire drawing` |
| Flight wires | `wire flight` |
| Wire names (wire labels) | `wire label` |
| Wire net expression labels | `wire label` |
| Wire text (aliases) | `annotate drawing3` |
| Solder dot | `wire drawing` (objType: see the unknowns) |
| Schematic pins / instance pins | `pin drawing` |
| Pin names | `pin label` |
| Pin net expressions | `pin annotate` |
| Instance name / instance label | `instance label` |
| Instance bounding box (symbol selection box) | `instance drawing` |
| Symbol shapes, outline | `device drawing` |
| Symbol shapes, filled | `device drawing1` |
| Symbol name / logical label (`[@partName]`) | `device label` |
| Device annotate (`[@refDes]`) | `device annotate` |
| Physical labels | `annotate drawing2` (or `annotate drawing`) |
| Analog instance label `cdsName()` | `annotate drawing7` |
| Analog pin annotate `cdsTerm("x")` | `annotate drawing8` |
| Analog device annotate `cdsParam(n)` | `annotate drawing` |
| Note label (note text) | `text drawing` |
| Note shape, solid lines | `text drawing` |
| Note shape, dashed lines | `text drawing1` |
| Sheet borders | `border drawing` |
| Markers | `marker error`, `marker warning` |

`noConn` is an **instance** of `basic/noConn/symbol`, attached to a pin to exempt it from the
floating-pin check. VERIFIED [BASIC20]. `basic/onPageConn` does the same for dangling wires.

### 2.4 Labels: VERIFIED [DB20] label attribute table

| Attribute | Values |
|---|---|
| `theLabel` | the text |
| `xy` | origin point |
| `orient` | `R0 R90 R180 R270 MY MYR90 MX MXR90` |
| `justify` | `lowerLeft lowerCenter lowerRight centerLeft centerCenter centerRight upperLeft upperCenter upperRight` |
| `font` | `euroStyle gothic math roman script stick swedish`. The `sch*` functions also accept `fixed` and `milSpec`. |
| `height` | float, user units |
| `labelType` | `normalLabel` / `NLPLabel` / `ILLabel` |
| `isDrafted` | t: the text always reads upright |
| `isOverbar` | t: bar over the text |
| `isVisible` | t: shown |
| `parent` | the figure the label is glued to |
| `net` | net of the parent figure (generic figure attribute) |

All of these are settable [DB20].

**Gluing.** A wire label is glued to its wire through a child/parent relation: `label~>parent`
is the wire, and the wire's `~>children` contains the label [SCH20 schGlueLabel] (VERIFIED).
[VCLI] walks `wire~>children` filtered by `objType == "label"` (LIKELY).

Rules for what a label can glue to [SCH20]:
- A wire label glues to a wire, a component pin, or a schematic pin, and names that net.
- A pin label glues only to the pin it names.
- Note labels glue to anything.

`schIsWireLabel(d_fig) => t/nil` and `schIsWire(d_obj) => t/nil` exist [SCH20].

`g_aliasP` (the last argument of `schCreateWireLabel`): t means the label is an *alias* rather
than the net name [SCH20]. Alias text shows on `annotate drawing3`.

### 2.5 Nets: VERIFIED [DB20] net attribute table

- `name`, `numBits`, `isGlobal` (settable boolean)
- `sigType`: `"analog" "clock" "ground" "reset" "scan" "signal" "supply" "testLatch" "tieHi" "tieLo" "tieOff"`,
  plus optical types
- `figs`: wires and labels on the net
- `instTerms`, `allInstTerms`, `term`, `terms`, `pins`
- `signals`, `sigNames`, `aliases`
- `memNets`, `memInstTerms`, `memTerms`: `(obj bitIndex)` pairs, for bus bits

Net names follow §5. After `schCheck`, unnamed nets are called `net1…` and those names change
between runs, so **do not round-trip unnamed net names**. Re-derive connectivity on our side.

### 2.6 Terminals and pins: VERIFIED [DB20]

**Terminal** (`objType "term"`):
- `name`, `numBits`, `net`, `pins`, `pinCount`
- `direction`: `"input" "output" "inputOutput" "jumper" "switch" "unused"`. The argument-type list
  also accepts `powerInput powerOutput bidir tristate`. **`schCreatePin` accepts only
  `input output inputOutput switch`** [SCH20]. `jumper` is accepted by VCLI's mapper; unconfirmed
  for `schCreatePin`.

**Pin** (`objType "pin"`):
- `name`, `term`, `net`, `accessDir`
- `fig`: the first figure; `figs` holds all of them.
- In a **schematic**, `pin~>fig` is the pin **instance** (for example of `basic/ipin`, with `purpose "pin"`).
- In a **symbol**, `pin~>fig` is a **rect on `pin drawing`**.

**Schematic pin masters**, from the GUI default `schPinMasters` [UG20]:

| Direction | Master |
|---|---|
| input | `basic/ipin/symbol` |
| output | `basic/opin/symbol` |
| inputOutput | `basic/iopin/symbolr` |
| switch | `basic/iopin/symbolr` |

Offsheet pins use views `symbolrOff`/`symbollOff`.

SKILL code in the wild uses `basic/iopin/symbol` [NG-pin per instTerm], [VCLI]; both views exist.
`basic/ipin` at R0 has its bBox entirely to the left of the origin, with the connection point at
the origin. Orientation therefore decides which way the wire leaves [VCLI PR #115] (LIKELY).

### Snippets §2

```skill
;; 2a. Deterministic dump of instances and their stored properties (integer DBU coordinates).
procedure(SfyByName(objs) sort(copy(objs) lambda((a b) alphalessp(a~>name b~>name))))
procedure(SfyDbu(cv v) round(v * cv~>DBUPerUU))
procedure(SfyDumpInsts(cv port)
  foreach(inst SfyByName(cv~>instances)
    fprintf(port "inst %s %s %s %s %d %d %s %s\n"
            inst~>name inst~>libName inst~>cellName inst~>viewName
            SfyDbu(cv xCoord(inst~>xy)) SfyDbu(cv yCoord(inst~>xy))
            inst~>orient inst~>purpose)
    foreach(p SfyByName(inst~>prop)
      fprintf(port "  prop %s %s %L\n" p~>name p~>valueType p~>value))))
```

```skill
;; 2b. Wires (lines and paths on wire/drawing) with their glued labels.
procedure(SfyDumpWires(cv port)
  foreach(f cv~>shapes
    when(f~>layerName == "wire" && f~>purpose == "drawing"
         && member(f~>objType '("line" "path"))
      fprintf(port "wire %s width=%d net=%s pts"
              f~>objType
              if(f~>objType == "path" SfyDbu(cv f~>width) 0)
              if(f~>net f~>net~>name "-"))
      foreach(pt f~>points
        fprintf(port " %d %d" SfyDbu(cv xCoord(pt)) SfyDbu(cv yCoord(pt))))
      fprintf(port "\n")
      foreach(k f~>children
        when(k~>objType == "label"
          fprintf(port "  label %L type=%s\n" k~>theLabel k~>labelType))))))
```

```skill
;; 2c. Every label (wire names, notes, pin names) with full display attributes.
procedure(SfyDumpLabels(cv port)
  foreach(f cv~>shapes
    when(f~>objType == "label"
      fprintf(port "label %s/%s %L %s %d %d %s %s %s %d par=%s\n"
              f~>layerName f~>purpose f~>theLabel f~>labelType
              SfyDbu(cv xCoord(f~>xy)) SfyDbu(cv yCoord(f~>xy))
              f~>justify f~>orient f~>font SfyDbu(cv f~>height)
              if(f~>parent f~>parent~>objType "-")))))
```

```skill
;; 2d. Terminals, nets and connections after a check.
procedure(SfyDumpConn(cv port)
  foreach(t SfyByName(cv~>terminals)
    fprintf(port "term %s %s net=%s pins=%d\n"
            t~>name t~>direction t~>net~>name length(t~>pins)))
  foreach(n SfyByName(cv~>nets)
    fprintf(port "net %s bits=%d global=%L\n" n~>name n~>numBits n~>isGlobal)
    foreach(it n~>instTerms
      fprintf(port "  conn %s %s\n" it~>inst~>name it~>name))))
```

---

## 3. Reading a symbol

### 3.1 Symbol content conventions: VERIFIED [UG20] LPP table and `schSymbolLabelChoices`

- **Body graphics**: `device drawing` (outline) and `device drawing1` (filled). Shapes are lines,
  rects, polygons, arcs, ellipses (circle = ellipse with a square bBox) and paths.
- **Selection box**: one rect on `instance drawing`. It defines the instance's selectable extent.
  The GUI's "Add Selection Box → Automatic" creates it.
- **Pins**: each terminal has a pin whose `fig` is a rect on `pin drawing`. The default symbol pin
  master is `basic/sympin/symbolNN` for the "square" type (`basic/circle/symbol` for "round")
  [UG20 schSymbolPinMasters].
- **Labels** (default label choices; text, labelType, LPP):

| Choice | Text | labelType | LPP |
|---|---|---|---|
| instance label | `[@instanceName]` | NLPLabel | `instance label` |
| device annotate | `[@refDes]` | NLPLabel | `device annotate` |
| logical label | `[@partName]` | NLPLabel | `device label` |
| physical label | `[@userPartName:%:[@phyPartName]]` | NLPLabel | `annotate drawing2` |
| pin name | `{pinName}`, a literal pin name | normalLabel | `pin label` |
| pin annotate | `[@p_{pinName}]` | NLPLabel | `pin annotate` |
| art instance label | `cdsName()` | ILLabel | `annotate drawing7` |
| art pin annotate | `cdsTerm("{pinName}")` | ILLabel | `annotate drawing8` |
| art device annotate | `cdsParam(1)` | ILLabel | `annotate drawing` |

  `{pinName}` is a form placeholder that the GUI replaces with the actual pin name.
  "art" means the analog/ADE flavour. analogLib-style symbols use the `cdsName()`, `cdsParam(n)`
  and `cdsTerm("t")` ILLabels.

- **Text height** is commonly `0.0625` inch [SCH20 defaults]. The `instance label` default height in
  the tsg examples is `0.075`–`0.08125` [UG20] (LIKELY).
- A `cdsTerm` label is usually glued with `parent` set to its pin figure so that it moves with the
  pin. The gluing is a convenience; it is not required [NG-cdsTerm] (LIKELY).

### 3.2 Label languages

**NLP (`NLPLabel`)**, VERIFIED [UG20 "Adding an NLP Label"]:
- `[@prop]` or `[@prop:%]` → the value of `prop`.
- `[@prop:fmt1:fmt2]`: `fmt1` if the property exists (`%` is replaced by the value), otherwise `fmt2`.
  With no `fmt2` and no property, the label is not displayed.
- Text outside `[...]` is literal.
- `\` escapes a reserved character. `\n`, `\t` and `\nnn` (octal) are also recognised.
- Expressions can be nested or chained: `[@w:w=%;[@l:l=%]]`, `[@w:w=%;][@l:l=%;]`.
- NLP looks up **database properties**. In the "Analog" netlisting mode (`CDS_Netlisting_Mode`) the
  lookup also covers CDF.
- Common properties: `instanceName`, `partName`, `cellName`, `refDes`.

**IL (`ILLabel`)**, VERIFIED [UG20], LIKELY [NG-ILLabels]:
- The text is a SKILL expression evaluated with `ilInst` bound to the instance; `ilInstPath`
  covers deeper display levels.
- `cdsName()` → instance name.
- `cdsParam(n)` → the n-th parameter in the CDF "interpreted labels" set.
- `cdsTerm("t")` → terminal/net annotation for terminal `t`.
- These are what ADE annotation controls drive.
- ILLabels are only interpreted on the *master* of a placed instance (i.e. on a symbol), never on
  level-0 labels in the schematic itself (Andrew Beckett) [NG-ILLabels].

**Mapping consequence for Schemify:** symbol labels carry a *language tag*. Preserve
`labelType` and the raw text verbatim, and map only the known forms (`cdsName()`,
`[@instanceName]`, `cdsParam(n)`, `[@p:fmt]`) to Schemify's own templating.

### 3.3 Arcs, ellipses, justify, orient, fonts

- **Arc**: `ellipseBBox` is the full ellipse (axes parallel to x/y). `startAngle` and `stopAngle`
  are in radians, the sweep runs CCW from start to stop, and **the sweep is at most π**. `bBox` is
  the arc's own box. All three are settable. VERIFIED [DB20].
  - Writing an arc longer than π needs **two arcs**.
- **Ellipse**: `bBox` only (settable).
- `justify`, `orient`, `font`: the value sets are in §2.4. Older aliases are also accepted on input:
  `leftTop…`, `0 90 180 270 sideways upsideDown`, and fonts `fixed raster` [DB20 arg types]. **Emit
  only the canonical names.**

### Snippets §3

```skill
;; 3a. Classify every shape of a symbol by role.
procedure(SfySymRole(f)
  let((lp)
    lp = list(f~>layerName f~>purpose)
    cond((equal(lp '("instance" "drawing")) "selbox")
         ((f~>layerName == "device" && f~>objType != "label") "body")
         ((f~>pin != nil) "pinfig")
         ((f~>objType == "label") strcat("label:" f~>labelType))
         (t "other"))))
let((sym)
  sym = dbOpenCellViewByType("analogLib" "nmos4" "symbol" nil "r")
  foreach(f sym~>shapes
    printf("%s %s %s/%s\n" SfySymRole(f) f~>objType f~>layerName f~>purpose))
  dbClose(sym))
```

```skill
;; 3b. Pins of a symbol: terminal name, direction and pin rect in DBU.
foreach(t sym~>terminals
  foreach(p t~>pins
    let((bb) bb = p~>fig~>bBox
      printf("pin %s %s %d %d %d %d\n" t~>name t~>direction
             round(xCoord(lowerLeft(bb))*sym~>DBUPerUU)
             round(yCoord(lowerLeft(bb))*sym~>DBUPerUU)
             round(xCoord(upperRight(bb))*sym~>DBUPerUU)
             round(yCoord(upperRight(bb))*sym~>DBUPerUU)))))
```

```skill
;; 3c. Arc geometry export (ellipse box plus angles in radians).
foreach(f sym~>shapes
  when(f~>objType == "arc"
    printf("arc ell=%L start=%L stop=%L\n" f~>ellipseBBox f~>startAngle f~>stopAngle)))
```

---

## 4. Creating schematics and symbols

### 4.1 Instances: VERIFIED [SCH20], [DB20]

```
schCreateInst( d_cvId d_masterId t_instanceName l_origin t_orient [ n_magnification ] ) => d_inst / nil
dbCreateInst( d_cellView d_master t_name l_point t_orient [ x_stopIndex ] [ g_physOnly ] ) => d_inst / nil
dbCreateInstByMasterName( d_cellView t_lib t_cell t_view t_instName l_origin t_orient [ x_numInst ] [ g_physOnly ] )
dbCreateParamInst( d_cv d_master t_name l_point t_orient [ x_numInst [ l_params [ g_physOnly ] ] ] )
     ; l_params = list( list(t_propName t_propType g_value) ... )
```

- `schCreateInst` takes a **master cellview object**, not lib/cell/view strings. It has **no
  `?param` keywords**. [VCLI] confirmed both on IC23.1. A `nil` name auto-generates `I<n>`.
- Orientation values are `R0 R90 R180 R270 MX MXR90 MY MYR90`. R90 is counter-clockwise (§7).
- **Use `schCreateInst` for schematics**, so the schematic editor's bookkeeping runs.
  `dbCreateInst` is fine for symbols and other raw DB work.
- **Parameters**: use `dbReplaceProp(inst name type value)` for each one; CDF analog params are
  usually type `"string"`, e.g. `"1u"`. This does **not** run CDF callbacks, so derived parameters
  (e.g. `fingers` → `fw`) will not update.
  - PDK devices that depend on callbacks need the CDF route:
    `cdfFindParamByName(cdfGetInstCDF(inst) "w")~>value = "1u"` (LIKELY [VCLI]).
  - Callbacks still won't fire without an extra step (see the CDF note). Mark this
    `ponytail: no CDF callbacks; upgrade = cdf callback invocation` in the bridge.

### 4.2 Wires and wire labels: VERIFIED [SCH20]

```
schCreateWire( d_cvId t_entryMethod t_routeMethod l_points n_xSpacing n_ySpacing n_width
               [ t_color ] [ t_lineStyle ] ) => l_wireId
schCreateWireLabel( d_cvId d_glue l_point t_text t_justify t_orient t_fontStyle n_fontHeight g_aliasP )
  => d_labelId / nil
```

**`schCreateWire`:**
- `t_entryMethod` is `"draw"` or `"route"`.
  - With `"draw"`, the point list is used verbatim and `t_routeMethod` is ignored. **Always use
    `"draw"` with our own points.** `"route"` runs the router on the first two points.
  - `t_routeMethod` is still required positionally: `"full"`, `"direct"` or `"flight"`.
- Each consecutive point pair becomes one segment, and **it returns a list**: one wire object per
  segment. Take `car()` to glue a label [NG-creating].
- `n_width` 0 gives a narrow wire (a `line`); greater than 0 gives a wide wire (a `path`).
- `n_xSpacing`/`n_ySpacing` are the snap applied to the points. Use `0.0625` to snap to the
  standard grid.
- [VCLI] observed that it **returns `nil` when the same segment already exists**.

**`schCreateWireLabel`:**
- `d_glue` must be a wire, or a pin. A label not glued to anything names nothing.
- [VCLI] found the wire at a point with `dbGetOverlaps(cv list(pt pt))`, filtering for
  `objType == "line"`.
- The font may be any of `euroStyle fixed gothic math roman script stick swedish milSpec`; the
  default height is `0.0625`.

**Connectivity by name.** Wires carrying the same label are one net even if far apart
[UG20 "Wire-by-Name Connections"]. A label stub on each instance pin therefore needs no routing.
Draw a short wire from the pin's connection point and label it. [VCLI] uses a 0.25-inch stub.

**Junctions.** T-junctions connect, and the editor auto-adds a solder dot. A `+` crossing does
**not** connect unless a solder dot is placed. L-junctions and endpoint meetings connect without a
dot. A wire connects to a pin if it ends on, ends in, or passes through the pin; an angled wire
must meet it at an endpoint [UG20]. VERIFIED.

`schSolder(d_cvId l_solderPt) => d_shapeId/nil` places a dot at a T or + junction [SCH20]. Env
`srcSolderOnCrossover` controls the checker's behaviour (LIKELY).

### 4.3 Pins: VERIFIED [SCH20]

```
schCreatePin( d_cvId d_master t_termName t_direction g_offSheetP l_origin t_orientation
              [g_powerSens] [g_groundSens] [g_sigType] ) => d_pin / nil
schCreateSymbolPin( d_cvId d_master t_termName t_direction l_origin t_orientation
                    [ g_flatten ] [g_powerSens] [g_groundSens] [g_sigType] ) => d_pinFigId / nil
```

**`schCreatePin`:**
- `t_direction` is `input output inputOutput switch`.
- `g_sigType` is one of `analog clock ground power reset scan signal tieHi tieLo tieOff`, or `nil`.
  When it is omitted or nil, the pin **inherits the sigType of an existing same-named wire**,
  otherwise `signal`.
- A sigType that disagrees with the symbol terminal's sigType (e.g. `power`) produces a persistent
  `schCheck` warning [VCLI PR #115].
- The master is opened as `dbOpenCellViewByType("basic" "ipin" "symbol" "" "r")`.

**`schCreateSymbolPin`** works only in a symbol cellview. It copies the master's figures (or
instances them when `g_flatten` is nil — semantics only partly documented) and creates the
terminal. The usual master is `basic/sympin/symbolNN`.

**The low-level pin recipe** works in symbols and in pcells. It is VERIFIED [DB20] and LIKELY in
practice [NG-schematic pins]:
```
net  = dbMakeNet(cv "A")                 ; dbCreateNet returns nil if the net exists; dbMakeNet returns it
term = dbCreateTerm(net "A" "input")     ; [x_unused] [g_physOnly] are optional
fig  = dbCreateRect(cv list("pin" "drawing") bbox)
pin  = dbCreatePin(net fig "A")          ; dbCreatePin(d_net d_fig|nil [t_pinName|nil] [d_term|nil])
```
If the root net has no terminal, `dbCreatePin` creates one automatically [DB20].

### 4.4 Note labels and shapes, symbol labels and shapes: VERIFIED [SCH20]

```
schCreateNoteLabel( d_cvId l_point t_text t_just t_orient t_fontStyle n_fontHeight t_type ) => d_label/nil
     ; t_type: "normalLabel" | "NLPLabel" | "ILLabel"; schematic or symbol cellviews
schCreateNoteShape( d_cvId t_type t_lineStyle l_points [ n_width ] ) => d_shape/nil
     ; t_type: line rectangle polygon arc ellipse circle ; t_lineStyle: "solid" | "dashed"
schCreateSymbolLabel( d_cvId l_point t_labelChoice t_text t_justify t_orient t_fontStyle n_fontHeight t_type )
     ; t_labelChoice: "instance label" "device label" "device annotate" "pin name" "pin annotate"
     ; symbol cellview opened in "a" mode
schCreateSymbolShape( d_cvId t_shape t_style l_points [ n_width ] ) => d_shapeId/nil
     ; t_shape: line rectangle polygon arc circle ellipse ; t_style: "outline" | "solid"
schGlueLabel( d_label d_figure ) => t / nil
schCreateNetExpression( d_cvId t_netExpr d_glueId l_point t_justify t_orient t_fontStyle n_fontHeight ) => d_id/nil
```

- **The point semantics of `arc`/`circle` in `sch*Shape` are not documented.** For exact geometry,
  use `dbCreateArc`/`dbCreateEllipse` with explicit LPPs instead (§4.5).
- The `schCreateNetExpression` example in [SCH20] passes an extra `"net1"` argument. Follow the
  argument list, not the example.

### 4.5 Raw db shape creation: VERIFIED [DB20]

`txl_layerPurpose` is a layer name, a layer number, or `list(layer purpose)`. A bare layer means
purpose `drawing`.

```
dbCreateLine( d_cv txl_layerPurpose l_points ) => d_line/nil          ; >= 2 points
dbCreatePath( d_cv txl_layerPurpose l_points n_width [ t_pathStyle ] ) ; default "truncateExtend"
dbCreateRect( d_cv txl_layerPurpose l_bBox )
dbCreatePolygon( d_cv txl_layerPurpose l_points )                     ; >= 3 points, no closing point
dbCreateEllipse( d_cv txl_layerPurpose l_bBox )
dbCreateArc( d_cv txl_layerPurpose l_ellipse_bBox l_arc_bBox )        ; the arc is ellipse ∩ arc box
dbCreateDot( d_cv txl_layerPurpose l_point )
dbCreateLabel( d_cv txl_layerPurpose l_point t_label t_just t_orient t_font x_height ) => d_label/nil
dbCreateTextDisplay( d_assoc d_owner txl_lpp l_displayFlags l_point t_just t_orient t_font x_height [...] )
dbCreateProp( g_obj t_name t_propType g_value ) => d_prop/nil        ; nil if it already exists
dbReplaceProp( g_obj t_name t_propType g_value ) => d_prop/nil       ; create or overwrite
dbDeleteObject( d_obj ) => t/nil
```

- `dbCreateLabel` always creates a `normalLabel`. To make an NLP or IL label, set it afterwards:
  `lbl~>labelType = "ILLabel"` [NG-cdsTerm], [DB20] (the attribute is settable).
- **Arc from angles** (our native form). Create the arc with any arc box that overlaps the ellipse
  box. Then set `a~>startAngle`/`a~>stopAngle` (radians), which [DB20] lists as settable. Split
  sweeps larger than π into two arcs. LIKELY: the attributes are VERIFIED settable, but this exact
  usage was not seen in the wild.
- Property type strings: `boolean int float string time fileName ILExpr ILList NLPExpr hierProp`.
  Aliases are also accepted: `fixnum integer flonum Boolean ilExpr nlpExpr property ilList` [DB20].
  Inherited-connection overrides use **netSet** properties [UG20]. How a netSet is typed at the DB
  level is unknown (see the unknowns).
- `dbDeleteObject` on a net deletes the net's terminal and disconnects its instTerms. On a terminal
  it deletes the terminal's pins. On a pin it unbinds the pin; the figure stays [DB20].
- [VCLI] says deleting an instance also deleted co-located net labels on IC23.1 (LIKELY).
- [VCLI] reports that **`dbCreateWire` and `dbFindLayerByName` do not exist** on IC23.1. Do not use
  them.

### 4.6 Symbol generation from a pin list ("Create Cellview from Cellview", tsg): VERIFIED [SCH20]

```
schSchemToPinList( t_lib t_cell t_view ) => g_pinList
schSymbolToPinList( t_lib t_cell t_view ) => g_pinList / nil
schPinListToSymbol( t_lib t_cell t_view g_pinList ) => t / nil
schPinListToSymbolGen( t_lib t_cell t_view g_pinList ) => t / nil
schPinListToView( t_lib t_cell t_view g_pinList t_toFunc ) => t / nil   ; t_toFunc e.g. "schPinListToSymbol"
schViewToView( t_srcLib t_srcCell t_lib t_cell t_viewFrom t_viewTo t_fromFunc t_toFunc ) => t / nil
schSetAndLoadTsgTemplateType( t_tsgType [ t_templateFileName ] ) => t / nil
     ; "digital" | "artist" | "PCB" | "metric" | "other" -> install_dir/tools/dfII/samples/symbolGen/*.tsg
```

The pin list is a disembodied property list:
```
'(nil ports ((nil name "a" direction "input")
             (nil name "y" direction "output" pins ((nil name "y" accessDir "right"))))
      prop (nil propName propValue ...))
```

- **Easiest path to a Virtuoso symbol:** create the schematic, then call
  `schPinListToSymbol(lib cell "symbol" schSchemToPinList(lib cell "schematic"))`. The tsg
  template decides the look, the labels and the selection box.
- How `schPinListToSymbol` and `schPinListToSymbolGen` differ is not documented (see the unknowns).
- Env var `ssgSortPins` (`"alphanumeric"` or `"geometric"`) changes pin ordering, and
  `schGetPinOrder(cv)` reads it back [VCLI symbol_ops] (LIKELY).

### 4.7 Functions to avoid in scripts

`schHi*` functions are interactive: they need a window and prompt for clicks. `geOpen`/`hiOpenWindow`
are optional and GUI-only. Everything above works with no window. [SCH20 notes that `schExtractConn`
"can be used only when editing a schematic cellview"; `schCheck` accepts a read-only or editable ID.]

### Snippets §4

```skill
;; 4a. Build a tiny schematic: an nmos, a labelled drain stub, an input pin on the gate.
let((cv nm ip m0 ws)
  cv = dbOpenCellViewByType("scratchLib" "sfyDemo" "schematic" "schematic" "w")
  nm = dbOpenCellViewByType("analogLib" "nmos4" "symbol" "schematicSymbol" "r")
  ip = dbOpenCellViewByType("basic" "ipin" "symbol" "" "r")
  unless(cv && nm && ip error("sfyDemo: open failed"))
  m0 = schCreateInst(cv nm "M0" list(0.0 0.0) "R0")
  dbReplaceProp(m0 "w" "string" "1u")   ; ponytail: no CDF callbacks run
  ws = schCreateWire(cv "draw" "full" list(list(0.0 0.5) list(0.0 1.0)) 0.0625 0.0625 0.0)
  schCreateWireLabel(cv car(ws) list(0.0 0.75) "out" "lowerLeft" "R90" "stick" 0.0625 nil)
  schCreatePin(cv ip "in" "input" nil list(-1.0 0.0) "R0")
  SfyCheckSave(cv)
  dbClose(cv))
```

```skill
;; 4b. Glue a label to the wire found under a point (no coordinates guessed).
procedure(SfyLabelAt(cv x y text)
  let((w)
    w = car(setof(f dbGetOverlaps(cv list(list(x y) list(x y))) f~>objType == "line"))
    unless(w error("SfyLabelAt: no wire at (%g %g)" x y))
    schCreateWireLabel(cv w list(x y) text "lowerLeft" "R0" "stick" 0.0625 nil)))
```

```skill
;; 4c. Symbol by raw db calls: body, selection box, one pin with name and cdsTerm labels, cdsName.
let((sym net fig lbl)
  sym = dbOpenCellViewByType("scratchLib" "sfyBuf" "symbol" "schematicSymbol" "w")
  dbCreatePolygon(sym list("device" "drawing")
                  list(list(0.0 -0.25) list(0.5 0.0) list(0.0 0.25)))
  dbCreateRect(sym list("instance" "drawing") list(list(-0.125 -0.3125) list(0.625 0.3125)))
  net = dbMakeNet(sym "A")
  dbCreateTerm(net "A" "input")
  fig = dbCreateRect(sym list("pin" "drawing") list(list(-0.1375 -0.0125) list(-0.1125 0.0125)))
  dbCreatePin(net fig "A")
  lbl = dbCreateLabel(sym list("pin" "label") list(-0.0875 0.0) "A" "centerLeft" "R0" "stick" 0.0625)
  lbl~>parent = fig
  lbl = dbCreateLabel(sym list("annotate" "drawing8") list(-0.15 0.05) "cdsTerm(\"A\")"
                      "lowerRight" "R0" "stick" 0.0625)
  lbl~>labelType = "ILLabel"
  lbl = dbCreateLabel(sym list("annotate" "drawing7") list(0.25 0.3) "cdsName()"
                      "lowerLeft" "R0" "stick" 0.0625)
  lbl~>labelType = "ILLabel"
  dbSave(sym)
  dbClose(sym))
```

```skill
;; 4d. Symbol via the native generator from an existing schematic.
unless(schPinListToSymbol("scratchLib" "sfyDemo" "symbol"
                          schSchemToPinList("scratchLib" "sfyDemo" "schematic"))
  error("symbol generation failed"))
```

```skill
;; 4e. Arc of 90 degrees on a circle of radius 0.125 centred at the origin.
let((a)
  a = dbCreateArc(sym list("device" "drawing")
                  list(list(-0.125 -0.125) list(0.125 0.125))     ; ellipse box
                  list(list(0.0 0.0) list(0.125 0.125)))          ; arc box (first quadrant)
  a~>startAngle = 0.0
  a~>stopAngle = 1.5707963267948966)
```

---

## 5. Net, bus and instance naming

**Net/terminal name grammar (BNF):** VERIFIED [DB20 "Net and Terminal Name Syntax"]
```
nameExpr ::= [ <*N> ] nameTerm { , nameExpr }
nameTerm ::= baseName [ < vectExpr > ] | < nameExpr >      ; ( ) grouping also used: <*2>(a,b)
vectExpr ::= vectTerm [ *N ] { , vectExpr }
vectTerm ::= N [ : N [ : N ] ] | < vectExpr >              ; ( ) grouping: b<(0:2)*2>
N < 65536
```

- **baseName**: printable ASCII excluding `< > ( ) , /` and whitespace. Parentheses are allowed
  only around a number.
- **Illegal in wire/net names**: `\` and `/` [UG20].

Expansions, all VERIFIED [DB20] and [UG20]:

| Name | Expansion |
|---|---|
| `b<0:2>` | b<0>,b<1>,b<2> |
| `DATA<2:0>` | descending: DATA<2>,DATA<1>,DATA<0> |
| `b<3:0:2>` | b<3>,b<1> |
| `DATA<1:7:2>` | DATA<1>,<3>,<5>,<7> |
| `<*2>(a,b),c` | a,b,a,b,c |
| `<*2>A,B,C` | A,A,B,C (the prefix binds to the first term only) |
| `<*2>(A,<*2>(X,Y)),B` | A,X,Y,X,Y,A,X,Y,X,Y,B |
| `A<0:2*2>` | A<0>,A<0>,A<1>,A<1>,A<2>,A<2> (suffix repeat, per bit) |
| `A<(0:2)*2>` | A<0>,A<1>,A<2>,A<0>,A<1>,A<2> |
| `A,B,Data<0:15>` | a bundle of 18 bits |

- **There is no `a<3:0>*2` form.** The suffix repeat goes *inside* the brackets (`a<3:0*2>`), or
  use a prefix repeat `<*2>a<3:0>`.
- **Global**: append `!` to the name, as in `vdd!` or `gnd!`. For buses, put it on the base name:
  `A!<0:3>` [UG20]. VERIFIED.
  - A partially global bundle such as `gnd!,net3` automatically gets a separate `gnd!` net.
  - At the DB level, `net~>isGlobal`; `dbIsSigNameGlobal` exists.
- **Bus taps**: a wire named `<3:0>` (vector only) taps a bus it touches, e.g. `DATA<0:7>` → it becomes
  `DATA<3:0>`. `<S,R>` taps from a bundle. Cascades are allowed [UG20]. A bare vector name with no
  bus to touch is an error or ambiguous, so **export full names, never bare taps**.
- **Width matching at iterated instances**: the net width must equal inst-width × pin-width, the
  pin width, or 1. Otherwise the editor inserts `<*n>`. E.g. `N` on `I<0:2>` pin `P<0:2>` becomes
  `<*9>N` [UG20].
- **Instance names**: `base[<N[:M]>]` only. No step, repeat, comma or grouping [DB20]. System names
  are `I0, I1, …` [UG20].
- **Name helpers on the Virtuoso side**, VERIFIED [DB20]. **Use them as the oracle for Schemify's
  own name parser** in differential tests:
  - `dbProduceMemName(t_name) => list(memNames)`, e.g. `"A<2:0>,B,C<3:4>"` gives `("A<2>" "A<1>" "A<0>" "B" "C<3>" "C<4>")`.
    It maps non-numeric parentheses to `#28`/`#29`: `"x(a)"` gives `"x#28a#29"`.
  - `dbGetMemName(t_name x_index) => t_memName`.

**Inherited connections**, VERIFIED [UG20], [DB20], [SCH20]:
- **Expression syntax**: `[@prop:%:default!]`, e.g. `[@vdd:%:vdd!]`. The default must be a scalar
  global name.
- The editor displays a trailing `*` on net expression labels; that `*` is not part of the
  expression.
- **Override mechanism**: a **netSet** property named `prop` on an ancestor instance. Its value is
  an existing signal name or another net expression.
- **Restriction**: one cellview cannot use two different expressions for the same default global.
- **On a symbol pin (terminal)**: `dbCreateTermNetExpr(d_term t_expr) => t/nil` (fails if one
  already exists), `dbReplaceTermNetExpr(d_term t_expr)`, `dbGetTermNetExpr(d_term) => t_expr/nil`.
  Also `dbParseTermNetExpr(expr)` → `("@" "vdd" "%" "vdd!")`.
- **On a schematic wire or pin**: `schCreateNetExpression(cv expr glueFig pt just orient font height)`
  creates the label, glued, on `wire label` / `pin annotate`. Run `schCheck` afterwards.
- **Don't** call `dbCreateSigNetExpr` directly: the extractor deletes inherited signals that lack
  a net-expression label [SCH20].
- basic library samples: `vdd_inherit [@vdd:%:vdd!]`, `gnd_inherit [@gnd:%:gnd!]`,
  `vcc_inherit [@vcc:%:vcc!]`, `vss_inherit [@vss:%:vss!]` [UG20].

**Patch cords**: `basic/patch`, with an NLP expression `src = dst` such as `0:3 = 3:0` [UG20, BASIC20].
Out of scope for v1.

### Snippets §5

```skill
;; 5a. Expand a name into its bits (oracle for our parser).
foreach(n '("A<2:0>,B,C<3:4>" "<*2>(a,b),c" "b<3:0:2>" "A<(0:2)*2>" "vdd!" "A!<0:3>")
  printf("%L => %L\n" n dbProduceMemName(n)))
```

```skill
;; 5b. Read inherited-connection expressions on a symbol's terminals.
foreach(t sym~>terminals
  let((e) e = dbGetTermNetExpr(t)
    when(e printf("term %s netExpr %s parsed %L\n" t~>name e dbParseTermNetExpr(e)))))
```

```skill
;; 5c. Wire with a net expression label (schematic side), then extract.
let((ws)
  ws = schCreateWire(cv "draw" "full" list(list(2.0 0.0) list(2.0 0.5)) 0.0625 0.0625 0.0)
  schCreateNetExpression(cv "[@vdd:%:vdd!]" car(ws) list(2.0 0.25)
                         "lowerLeft" "R90" "stick" 0.0625)
  schCheck(cv))
```

---

## 6. SKILL language, file I/O, data printing and parsing

### 6.1 Ports: VERIFIED [LANG20]/[LANG03]

```
outfile( S_fileName [ t_mode ] [ g_openHiddenFile ] ) => p_outport / nil   ; mode "w" (default) | "a"
infile( S_fileName ) => p_inport / nil
close( p_port ) => t                     ; drains first; never close piport/poport/stdin/stdout/stderr
drain( [p_port] )                        ; flush
```

**Relative paths go through the SKILL path** (`getSkillPath()`). `outfile` with a relative name may
overwrite the *first updatable file of that name on the SKILL path*. **Always pass absolute paths.**
`simplifyFilename(t_name [g_dontResolveLinks]) => t_abs` expands `~`, `.`, `..` and cwd.

### 6.2 Writing: VERIFIED [LANG20]

```
fprintf( p_port t_formatString [ g_arg1 ... ] ) => t
printf( t_formatString [ g_arg ... ] ) => t                ; to poport (the CIW)
sprintf( { s_Var | nil } t_formatString [ g_arg ... ] ) => t_string
print( g_value [ p_port ] ) => nil                          ; default print form, no newline
println( g_value [ p_port ] ) => nil                        ; adds a newline and flushes
```

| Directive | Argument | Prints |
|---|---|---|
| `%d %o %x` | fixnum | integer in decimal / octal / hex |
| `%f` | flonum | `[-]ddd.ddd` |
| `%e` | flonum | `[-]d.ddde[-]ddd` |
| `%g` | flonum | `f` or `e` style, "full precision in minimum space" |
| `%s` | string or symbol | the text *without* quotes |
| `%c` | string | its first character |
| `%n` | number | any number |
| `%L` | any | default print form |
| `%A` | any | printself representation |
| `%N` | any | old style, no printself |
| `%P` | list | a point |
| `%B` | list | a box |

- Width, precision and `-` (left-justify) work as in C: `%[-][width][.precision]code`. Precision
  applies to `f`/`e` [LUG20]. `%L`, `%P` and `%B` ignore width and precision.
- [LANG20] warns that putting a width on `%g` "may cause imprecise results".
- `%d` on a float: [VCLI] reports that it **silently made the whole expression return nil** (no
  error printed). **Always `round()` floats before `%d`.**
- `sprintf` has an internal limit of about 8192 characters [LUG20]. Build long output with
  `fprintf` straight to the port, or with `strcat`/`buildString`.

**Float precision:**
- Unformatted printing (`print`, `println`, `printlev`, and `%L` on floats) shows **about 7
  significant digits** unless you set `sstatus(fullPrecision t)`. Then it shows about 16–17
  [LANG20 sstatus]. VERIFIED.
- `%g` is documented as full precision, but [LANG20] itself hedges.
- **Bridge rule: never ship floats in text.** Emit integer DBU (`round(v*cv~>DBUPerUU)`) with `%d`.
  For true reals (property values), store the property's *string* form, since CDF values are
  strings anyway. If a raw float is unavoidable, use `%.17g` (GUESS: confirm in the probe) or
  `sstatus(fullPrecision t)` plus `%L`.

**Strings and `%L`/`print`:**
- `print` and `%L` print strings *in double quotes*.
- Escaping of embedded quotes, backslashes and newlines is not documented. The CIW echoes values in
  a re-readable form with escapes (forum statement), so LIKELY `%L` escapes too.
- **The bridge should escape strings itself** (snippet 6c) and write them with `%s` between quotes.
- `pprintresult` (default t) makes the I/O routines pretty-print lists with alignment and line
  breaks [LANG20 sstatus]. **Don't `%L` long lists into a data file**: iterate and print atoms.

### 6.3 Reading: VERIFIED [LANG20]/[LANG03]

```
gets( s_var [ p_inport ] ) => t_string / nil       ; macro: stores into s_var; keeps the trailing "\n"; nil at EOF
lineread( [ p_inport ] ) => t / nil / l_results    ; *parses* one logical line into SKILL objects, does NOT evaluate
                                                   ;   t = blank line, nil = EOF; keeps reading while parens are open
linereadstring( t_string ) => g_form / nil         ; lineread on a string; first form only
parseString( S_string [ S_breakChars ] ) => l_strings   ; default breaks on whitespace; runs of breaks collapse;
                                                   ;   "" as the break string splits into characters
readTable( S_file o_table ) => t / nil             ; only reads files written by writeTable(S_file o_table)
fscanf / scanf / sscanf                            ; C-like; buffer limit on %s
evalstring( t_string ) / errsetstring( t_string [g_errprint] )   ; read AND evaluate
```

`parseString` collapses empty fields, so **tab-separated data with empty columns cannot be split
with it**. Use a sentinel such as `-` for empty fields, or `rexMatchp`/`substring` scanning.

**Recommended direction split:**
- Virtuoso → Schemify: a SKILL dump writes a line-oriented text file (§2 snippets), and Schemify
  parses it.
- Schemify → Virtuoso: Schemify *generates a `.il` script* of literal `sch*`/`db*` calls (§4), and
  the user runs `load("/abs/path/file.il")`.

No SKILL-side parser is then needed. `load(t_file)` evaluates every top-level form.

### 6.4 Errors: VERIFIED [LANG20]

- `errset( g_expr [ g_errprint ] ) => list(value) / nil`
  - Traps errors. Success wraps the value in a one-element list.
  - After a nil, `errset.errset` holds the error info, e.g.
    `("sqrt" 0 t nil ("*Error* sqrt: can't handle sqrt(x)"))`.
- `error("fmt" args…)` signals an error.
- `unwindProtect(g_body g_cleanup)` always runs the cleanup. Use it to `dbClose`/`close`.
- `sstatus(errsetTrace t)` prints errors that `errset` would otherwise swallow.

### 6.5 Files and the environment: VERIFIED [LANG20]

| Call | Returns | Notes |
|---|---|---|
| `getShellEnvVar("HOME")` | string or nil | |
| `setShellEnvVar("X=1")` | | |
| `isFile(S [tl_path])` | t if a regular file exists | not true for directories; relative names use the SKILL path |
| `isDir(S)` | | |
| `isFileName(S)` | | |
| `createDir(S)` | t, or nil if it already exists | **signals an error** if a parent is missing or not writable |
| `deleteFile(S)` | | |
| `getSkillPath()` | | |

### 6.6 Syntax essentials: VERIFIED [LUG20]/[LANG20]

**Calls and literals:**
- **No whitespace between a function name and `(`**: `f(a b)` is legal, `f (a b)` is not.
- Unary minus must touch its operand: `-1`, `-a`.
- Binary minus needs spaces on both sides or neither: `a - b` or `a-b`, never `a -b`.
- Negative literals inside argument lists are fine: the manuals show `fileSeek(p -12 2)` and
  `difference(12.2 -13) => 25.2`. When generating code, write `list(1.0 -2.0)` and never `list(1.0 - 2.0)`.
- `x:y` builds the 2-element list `(x y)`. Prefer `list(x y)` in generated code.

**Strings:**
- Delimited by `"…"`.
- Escapes: `\n \t \v \b \r \f \\ \" \ddd` (octal).
- A backslash before a newline continues the line.
- **The parser caps a single string *literal* at 8191 characters.** Runtime strings have no cap.
  Split long text in generated `.il` files: `strcat("…" "…")`.
- `multilineString` (default t) lets literals span lines.

**Symbols and atoms:**
- Symbol print names are at most 255 characters.
- `t` is true; `nil` is both false and the empty list. Never use either as a variable name.
- Unset values are `unbound`.

**Equality and comparison:**
- `==` is `equal`, which compares string contents. [VCLI] probed `strcat("M" "1") == "M1"` → t.
- `eq` compares identity. `alphalessp(a b)` compares strings or symbols.

**Control and binding forms:**
- `procedure(name(a b @optional (c 1)) body…)` or `procedure(name(a @key (d nil)) body…)`.
  **`@optional` and `@key` are mutually exclusive** in one argument list [LANG20]. `@rest` is also
  available.
- `let((x (y 0)) body…)` binds locals; the result is the last form.
- `prog((x) … return(v))` allows `return`/`go`.
- `when(c body…)`, `unless(c body…)`, `if(c then a else b)` (or `if(c a b)`),
  `cond((c1 e…) (c2 e…) (t e…))`.
- `case(key (k1 e…) ((k2 k3) e…) (t e…))` uses `equal`, so string keys work. Keys are not
  evaluated; `t` is the catch-all. `caseq` uses `eq`: symbols and fixnums only.
- `foreach(x list body…)` returns the *input list*. `foreach(x table …)` walks the keys of an
  association table **in hash order (non-deterministic)**. `for(i 1 n …)` is inclusive.
  `while(c …)`.
- `setof(x list cond)` filters; **only the last body form filters**, so use `and(...)` for
  several conditions [VCLI].
- `exists(x list cond)`, `mapcar(lambda((x) …) list)`.

**Lists:**
- `list`, `cons` (prepends), `append` (copies), `append1(l x)` (copies, slow), `nconc` (destructive).
- `tconc(nil x)` builds lists efficiently: take `car` at the end.
- `car`/`cdr`/`cadr`…
- `sort(l fn)` is **destructive**, so sort a `copy(l)`; `fn` = nil means alphabetical.
  `sortcar` sorts on the car of each element.

**Tables and strings:**
- `makeTable(name [default])` creates an association table; access with `tbl[key]`. A missing key
  gives `unbound` unless a default was given.
- `writeTable`/`readTable` persist plain data only.
- `strcat`, `buildString(l [glue])`, `substring(s i [len])` (**1-based**; negative `i` counts from
  the end), `strlen`, `getchar(s i)` (returns a *symbol*), `rexCompile`/`rexMatchp`/`rexReplace`
  (`&` in the replacement inserts the match).

**Determinism:**
- `cv~>instances`, `~>shapes`, `~>nets` come back in database order, with no guarantee.
- Table iteration is hash-ordered.
- **Sort by name (or by coordinates) before writing anything.**

### Snippets §6

```skill
;; 6a. Safe writer: absolute path, always closed, error captured.
procedure(SfyWithOut(path fn)
  let((p res)
    p = outfile(simplifyFilename(path) "w")
    unless(p error("SfyWithOut: cannot write %s" path))
    res = errset(unwindProtect(funcall(fn p) close(p)) t)
    unless(res printf("SfyWithOut failed: %L\n" errset.errset))
    res))
;; usage: SfyWithOut("/tmp/amp.sfy" lambda((p) SfyDumpInsts(cv p) SfyDumpWires(cv p)))
```

```skill
;; 6b. Line reader that does not evaluate anything.
procedure(SfyReadLines(path)
  let((p line acc)
    p = infile(simplifyFilename(path))
    unless(p error("SfyReadLines: cannot read %s" path))
    acc = nil
    while(gets(line p) acc = tconc(acc line))   ; each line keeps its "\n"
    close(p)
    car(acc)))
```

```skill
;; 6c. String escaper for our text format (quotes, backslashes, newlines, tabs).
;; ponytail: O(n^2) strcat; fine for labels and property values.
procedure(SfyEsc(s)
  let((out c)
    out = ""
    for(i 1 strlen(s)
      c = substring(s i 1)
      out = strcat(out cond((c == "\\" "\\\\") (c == "\"" "\\\"")
                            (c == "\n" "\\n") (c == "\t" "\\t") (t c))))
    out))
;; write: fprintf(p "text \"%s\"\n" SfyEsc(lbl~>theLabel))
```

```skill
;; 6d. Deterministic table dump: sort the keys first.
let((tbl keys)
  tbl = makeTable("sfy" nil)
  tbl["b"] = 2  tbl["a"] = 1
  keys = sort(tbl~>? nil)          ; ~>? on a table lists its keys (LIKELY; else collect via foreach)
  foreach(k keys printf("%s=%L\n" k tbl[k])))
```

---

## 7. Coordinates and transforms

- **Y points up.** Coordinates are Cartesian, in user units.
- **R90 is counter-clockwise.** VERIFIED: [DB20] `dbTransformPoint((1.5:2.5) list(0:10 "R90" 2.5)) => (-6.25 13.75)`.
  R90 maps (1.5, 2.5) to (-2.5, 1.5); ×2.5 gives (-6.25, 3.75); adding (0, 10) gives (-6.25, 13.75).
  So the order is **rotate/mirror, then magnify, then translate**.
- **Orientation semantics.** [DB20] instance table: MY = "mirrored on the y axis", MX = "mirrored on
  the x axis", MYR90/MXR90 = "mirrored … and rotated 90 degrees counterclockwise". So the mirror
  comes first, then the rotation. Point maps (before translation):

| orient | (x, y) → |
|---|---|
| R0 | ( x,  y) |
| R90 | (-y,  x) |
| R180 | (-x, -y) |
| R270 | ( y, -x) |
| MY | (-x,  y) |
| MYR90 | (-y, -x) |
| MX | ( x, -y) |
| MXR90 | ( y,  x) |

  R0–R270, MY and MX are VERIFIED by the definitions. MYR90 and MXR90 are LIKELY: they are derived
  from "mirror, then rotate CCW", which matches OpenAccess's `oacMYR90`/`oacMXR90`. The [DB20]
  *label* table has a typo calling MXR90 "mirrored on the y axis"; the instance table says x.
  Probe 9.4 settles it.
- **Transform list**: `list(offset orient [mag])`, e.g. `list(10.0:2.0 "R90" 2.0)` or
  `list(list(10.0 2.0) "R90")`. `inst~>transform` = `list(inst~>xy inst~>orient inst~>mag)`.
  VERIFIED [DB20].
- `dbTransformPoint(l_point l_transform) => l_point` and `dbTransformBBox(l_bBox l_transform) => l_bBox`
  (the result is normalised to lower-left/upper-right). VERIFIED.
- `dbConcatTransform(T1 T2)` = apply T1, then T2. E.g. `(list(0:0 "R90") list(1.5:2.5 "R90"))` gives
  `((1.5 2.5) "R180" 1.0)`. VERIFIED.
- Setting `inst~>orient = "MY"` sets the orientation **absolutely**. `dbMoveFig` with a transform
  *composes* [VCLI] (LIKELY).

### Snippets §7

```skill
;; 7a. Where does master terminal "D" land in the schematic?
let((t pc)
  t = dbFindTermByName(inst~>master "D")
  pc = centerBox(car(t~>pins)~>fig~>bBox)  ; centerBox(l_bBox) => l_point [DB20]; integer boxes truncate
  printf("D at %L\n" dbTransformPoint(pc inst~>transform)))
```

```skill
;; 7b. Orientation self-test (prints the image of (1,2) under all eight orients).
foreach(o '("R0" "R90" "R180" "R270" "MY" "MYR90" "MX" "MXR90")
  printf("%s %L\n" o dbTransformPoint(list(1.0 2.0) list(list(0.0 0.0) o))))
```

---

## 8. Bridge design notes

1. **Export (Virtuoso → Schemify).** Open with `"r"`, skip `schCheck` unless the user asks, and
   dump in sorted order with integer DBU (§2 snippets). Include `cv~>DBUPerUU`. For connectivity,
   prefer re-deriving it in Schemify from wires and labels over trusting `net1…` names. Also dump
   `cv~>nets` with instTerms as a cross-check oracle.
2. **Import (Schemify → Virtuoso).** Generate a `.il` that:
   - opens the target with `"w"` into a scratch cellview name first;
   - creates instances (`schCreateInst`), wires (`schCreateWire "draw"`), labels
     (`schCreateWireLabel` glued) and pins (`schCreatePin`);
   - runs `schCheck` and only `dbSave`s when it reports no errors;
   - prints the counts.

   **Every call is wrapped so that a nil result raises `error`.** All create functions return nil
   on failure rather than signalling.
3. **Never round-trip via `dbCreateInstTerm`/`dbMakeNet` for schematic connectivity.** `schCheck`
   wipes it (§1.5).
4. **Symbols.** For our own `.shprim`, build with raw db calls (§4c snippet) on the documented LPPs.
   For a quick Virtuoso-native look, use `schPinListToSymbol`.

---

## 9. Open unknowns and the first-run probe

| # | Unknown | Why it matters | Current assumption |
|---|---|---|---|
| U1 | Solder dot `objType` on `wire drawing`: `"ellipse"`? `"dot"`? | Reader must classify it, not treat it as geometry | Treat either on wire/drawing as a dot (GUESS) |
| U2 | Does `%L`/`print` escape `"`, `\` and newline inside strings? | Data file safety | Escape ourselves (snippet 6c) |
| U3 | `%g` precision, and whether `%.17g` is accepted | Float fidelity | Use integer DBU (no floats) |
| U4 | `DBUPerUU`/`userUnits` on real schematic and symbol cellviews (160/"inch"?) | Unit mapping | Read at runtime |
| U5 | Library creation: is `techSetTechLibName` needed or sufficient for a schematic-only library? Does a bare `ddCreateLib` library accept schematic cellviews? | `w`-mode creation | Use an existing user library |
| U6 | MYR90/MXR90 exact maps | Instance placement | Table in §7 (LIKELY) |
| U7 | How netSet properties are typed in the DB (`valueType`?), and the `schInhConFind`/`schInhConSet` signatures | Inherited-connection round trip | Read and preserve raw `inst~>prop` |
| U8 | `schPinListToSymbol` vs `schPinListToSymbolGen` | Symbol generation | Use `schPinListToSymbol` |
| U9 | Point semantics of `schCreateNoteShape`/`schCreateSymbolShape` for `arc`/`circle` | Shape writing | Use `dbCreateArc`/`dbCreateEllipse` |
| U10 | Does `schCreateWire` auto-insert solder dots at T junctions in a script (no window)? Does extraction merge or split segments? | Geometry round-trip identity | Re-export after import and diff |
| U11 | Does `~>?` on an association table list its keys? | Snippet 6d | Fall back to collecting keys via `foreach` |
| U12 | Is `jumper` accepted by `schCreatePin` (the docs list 4 directions)? | Pin export | Map jumper to inputOutput |
| U13 | Does `lbl~>parent = fig` work for gluing in a symbol, or is `schGlueLabel` required? | Label stays attached to its pin | Try `schGlueLabel(lbl fig)` first, then fall back |
| U14 | Do instTerms exist for unconnected terminals after `schCheck`? | Netlist completeness | Iterate master terminals |
| U15 | Does the `cv~>mode`/nil behaviour [VCLI] saw happen when the GUI holds the cellview? | Robust open | Use `SfyOpenEdit` (snippet 1a) |

**Probe script.** Ship it as `sfy_probe.il`. The user runs `load("/abs/sfy_probe.il")` once and
sends us `/tmp/sfy_probe.txt`. Placeholders `<LIB>` and `<CELL>` are filled in by the user.

```skill
let((p cv sym ws dot)
  p = outfile("/tmp/sfy_probe.txt" "w")
  fprintf(p "db=%L\n" dbGetDatabaseType())
  ;; U2/U3: printing
  fprintf(p "L-string=%L\n" "a\"b\\c\nd")
  fprintf(p "g=%g g17=%s L=%L\n" 0.1+0.2 sprintf(nil "%.17g" 0.1+0.2) 0.1+0.2)
  ;; U6: orientations
  foreach(o '("R0" "R90" "R180" "R270" "MY" "MYR90" "MX" "MXR90")
    fprintf(p "orient %s %L\n" o dbTransformPoint(list(1.0 2.0) list(list(0.0 0.0) o))))
  ;; name expansion oracle
  fprintf(p "names %L\n" dbProduceMemName("A<2:0>,<*2>(b,c),d<0:4:2*2>,vdd!"))
  ;; U4/U1/U10: a scratch schematic in a user-chosen library
  cv = dbOpenCellViewByType("<LIB>" "sfy_probe" "schematic" "schematic" "w")
  when(cv
    fprintf(p "sch units=%L dbu=%L\n" cv~>userUnits cv~>DBUPerUU)
    ws = schCreateWire(cv "draw" "full" list(list(0.0 0.0) list(1.0 0.0)) 0.0625 0.0625 0.0)
    schCreateWire(cv "draw" "full" list(list(0.5 0.0) list(0.5 0.5)) 0.0625 0.0625 0.0)
    schCreateWire(cv "draw" "full" list(list(2.0 0.0) list(3.0 0.0)) 0.0625 0.0625 0.0)
    schCreateWire(cv "draw" "full" list(list(2.5 -0.5) list(2.5 0.5)) 0.0625 0.0625 0.0)
    dot = schSolder(cv list(2.5 0.0))
    fprintf(p "solder ret=%L type=%L lpp=%L\n" dot && dot~>objType dot && dot~>lpp)
    schCreateWireLabel(cv car(ws) list(0.25 0.0) "n1" "lowerLeft" "R0" "stick" 0.0625 nil)
    fprintf(p "check %L\n" schCheck(cv))
    foreach(f cv~>shapes
      fprintf(p "shape %s %L parent=%L net=%L\n" f~>objType f~>lpp
              f~>parent && f~>parent~>objType f~>net && f~>net~>name))
    dbPurge(cv))                     ; never saved
  ;; symbol units and a reference symbol's anatomy
  sym = dbOpenCellViewByType("analogLib" "nmos4" "symbol" nil "r")
  when(sym
    fprintf(p "sym units=%L dbu=%L\n" sym~>userUnits sym~>DBUPerUU)
    foreach(f sym~>shapes
      fprintf(p "symshape %s %L %L %L\n" f~>objType f~>lpp
              if(f~>objType == "label" f~>theLabel f~>bBox)
              if(f~>objType == "label" f~>labelType nil)))
    dbClose(sym))
  close(p))
```

Run the probe in a throwaway library. Without the user's explicit OK it touches only
`<LIB>/sfy_probe`, which is opened `"w"` and purged without saving.

---

## 10. Additional source URLs

- IC20.1 manuals directory: https://github.com/wyz569/virtuoso_20.1/tree/main/doc. This is a copy
  of the documentation tree from a Cadence installation.
  - Function finder files with one-line signatures: `doc/finder/SKILL/Schematics/skcompref.fnd`,
    `doc/finder/SKILL/DFII_SKILL/skdfref.fnd`, `doc/finder/SKILL/Tech_File/sktechfile.fnd`.
  - Technology database structure (cdsDefTechLib, reference vs attach): `doc/techfileuser/chap1-4.html`.
- SKILL Language User Guide 06.40 (2005): https://github.com/hanun2999/cadence-skill/blob/main/SKILL%20Language%20User%20Guide.pdf
- MIT-licensed live-tested bridge: https://github.com/deanyou/virtuoso-cli. Files used:
  `src/client/schematic_ops.rs`, `src/client/symbol_ops.rs`, `resources/rb_connect_terminal.il`,
  `.pensieve/knowledge/{cadence-ic23-dbopencellviewbytype,cadence-ic23-schcreateinst,cadence-osshnl-109,skill-instterms-cons-reversal,skill-sprintf-float-coords}/content.md`.
- Forum statement on CIW echo versus printf escaping: Cadence community thread "Very fundamental- put
  quotation mark(s) into a string" (#19409), seen only as a search-engine summary because the
  forum blocks fetches.
- "Do Not Need Process Information" in the New Library form: https://limsk.ece.gatech.edu/course/ece6133/lab/virtuoso.pdf
  and https://hasler.ece.gatech.edu/Courses/ECE4430/Cadence_setup_instruction_Apr15_layout.pdf,
  both seen only as search-engine summaries.
