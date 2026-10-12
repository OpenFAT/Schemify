# Interop plugin v1: Virtuoso and xschem (normative)

Implements D24 as amended 2026-10-12 (interop is a plugin; xschem import **and export**; Cadence Virtuoso import and export) and D41 (this bridge). Readers and writers live in `plugins/interop`, never in a core crate. The xschem *import* is specified in [import.md](import.md); this spec covers everything else and the shared model under both.

Evidence: [virtuoso-skill-db.md](../research/virtuoso-skill-db.md) (SKILL database API), [virtuoso-cdf-pdk.md](../research/virtuoso-cdf-pdk.md) (CDF, simInfo, analogLib, PDK structure), [virtuoso-netlist-ade.md](../research/virtuoso-netlist-ade.md) (netlists, values, ADE), [virtuoso-tooling-prior-art.md](../research/virtuoso-tooling-prior-art.md) (SKILL lexing, test strategy). Facts there carry VERIFIED/LIKELY/GUESS marks; every GUESS this spec relies on is listed in §13 with the probe that settles it.

No Virtuoso runs in CI. The SKILL side is therefore (a) written in a subset a test-only interpreter executes exactly (§5.1), (b) kept free of translation logic — every decision is made in Rust, where it is tested — and (c) shipped with a self-test a user runs once in a real Virtuoso (§5.5).

## 1. Shape

```
                       ┌──────────── plugins/interop ─────────────┐
 Schemify files ──load──► Design ──virtuoso::export──► .sfyx ──SKILL loader──► Virtuoso cellviews, CDF, maestro
                        │   ▲   └──xschem::export───► .sch/.sym                   │
 Schemify files ◄─store─┘   └──virtuoso::import◄── .sfyx ◄──SKILL dumper──────────┘
 (merging the existing)                                      (cells, PDK libraries, maestro)
 xschem files ──xschem import (import.md)──► Schemify files
```

**Spine** (`interop::run`, one transform per line; the plugin command and the native CLI both call it):

| Direction | Transforms |
|---|---|
| export | `design::load(files, tops) → Design` · `nets::extract(Design) → Nets` · `map::resolve(Design, Mapping) → Plan` · `remap::apply(Design, Nets, Plan)` (labels + stubs for re-mastered instances, §6.4) · `virtuoso::write(Design, Plan, Options) → .sfyx` **or** `xschem::write(Design, Plan) → files` · report |
| import | `sfyx::read(text) → Dump` · `virtuoso::read(Dump, Mapping, Options) → Design` · `nets::extract` · `remap::apply` · `design::store(Design, existing files) → files` · report |

**Tables** (`design.rs`; owner: the transform that builds them; read by every later one; arena = one run):

| Table | Rows | Capacity |
|---|---|---|
| `cells` | lib, name, kind (`prim comp bench`), header attrs, provenance | 2^16 |
| `pins` | cell, name, dir, x, y, domain, inherit, alias, number | 2^16 per cell |
| `params` | cell, name, default, kind, attrs (`unit min max step grid choices computed show doc`) | 2^8 per cell |
| `impls` | cell, name, lang, formats (dialect, template), model, file, ignore, body | 2^4 per cell |
| `graphics` | cell, form (`line rect arc circle poly stext`), coordinates, layer, fill, dash, width, closed, text, orient, size, hcenter, vcenter | 2^16 per cell |
| `insts` | cell, id, cellref, name, x, y, orient, ignore, overrides, inherits | 2^22 per cell |
| `wires`, `labels`, `ports`, `noconns`, `texts`, `shapes` | the rank-3 records, one table each, typed | 2^22 per cell |
| `extras` | cell, `RecordBuf` of every rank-4..6 record and `constraint`/`waive` (passed through, interpreted only by §9) | 2^16 per cell |

Coordinates are Schemify milli-units, y down, orientations Schemify's, net expressions, templates and expressions in Schemify syntax: the Design is Schemify-shaped, and every tool-specific spelling is produced or consumed at the tool edge through the profile tables (§3). A tool difference is a row in a profile table, never a branch in a shared transform.

**Native CLI** `schemify-interop` (feature `cli`): `xschem-import …` (import.md), `xschem-export <project> <cell>… -o <dir>`, `virtuoso-export <project> <cell>… -o <file.sfyx> [--map F] [--lib L=V]… [--retarget]`, `virtuoso-import <file.sfyx> <project> [--map F] [--into <lib>]`, `install-skill <dir>` (writes `schemify.il`). Exit codes as the core CLI (0 ok, 1 design problems reported, 2 usage/I/O).

**Editor commands** (wasm guest, capabilities `read_project` + `write_project`): `interop.virtuoso_export <cell> [<out.sfyx>]`, `interop.virtuoso_import <file.sfyx>`, `interop.xschem_export <cell> [<dir>]`, `interop.xschem_import <file.sch|.sym>…`, `interop.install_skill`. File ▸ Interop submenu; each without arguments opens a path panel (cktimg's pattern). Reads follow references from the named cells (the guest has no directory listing): the closure of `cellref`s through the project manifest, plus `interop.map` and the existing target files for merging.

## 2. Design load and store

- **load:** every file of the closure is opened through `schemify_doc` (so only valid files are exported; a file that fails to load stops the export with its diagnostics) and its records are converted to Design rows. Closure = the named cells plus every `cellref` reachable from their schematics, through `lib:cell` and `./cell`; `core:*` cells come from `schemify_doc::core_library()`.
- **store:** Design rows → canonical Schemify text (`schemify_format::write`). When the target file exists, its **Schemify-only data is kept** (§7.3): the stored file is the existing file with the tool-owned parts replaced. Ids come from the source (`schemifyId`, §6.2); objects without one get `IdRng::for_import("<lib>/<cell>", ordinal)` ids, the ordinal being the object's rank in the dump's deterministic order, so re-importing an unchanged dump is a no-op diff.

## 3. Profiles (tool syntax as data)

### 3.1 Coordinates

| Tool | Unit | y | Exact mapping |
|---|---|---|---|
| xschem | 1 xschem unit = 1000 milli | down | identity (import.md, D33) |
| Virtuoso | user unit inch; `.sfyx` integers in 1/`upi` inch, `upi` per cellview | up | `x_milli = x · 160000 / upi`, `y_milli = −y · 160000 / upi` |

One Virtuoso schematic DBU at the standard `DBUPerUU = 160` is one xschem unit: the 1/16″ snap grid is xschem's 10-unit grid and the 1/8″ pin pitch its 20-unit pitch. The SKILL dumper writes `round(v · DBUPerUU)` with `upi = round(cv~>DBUPerUU)`; the exporter writes milli-units with `upi = 160000`. A conversion that is not exact rounds to the nearest milli-unit and is reported (`rounding`). The loader places at `v / float(upi)` inch; Virtuoso snaps to its DBU grid, and the exporter reports every coordinate that is not a multiple of `160000 / DBUPerUU` milli (`off-grid`, assuming 160).

### 3.2 Orientation

Virtuoso orientations act in a y-up frame: `R90` turns counter-clockwise, `MY` negates x, `MX` negates y, `MYR90`/`MXR90` mirror then turn. Conjugating by the y flip gives the bijection (checked against both matrix definitions by a test):

| Schemify | R0 | R90 | R180 | R270 | MR0 | MR90 | MR180 | MR270 |
|---|---|---|---|---|---|---|---|---|
| Virtuoso | R0 | R270 | R180 | R90 | MY | MXR90 | MX | MYR90 |

xschem: import.md §3.1 (`(rot, flip)` table), inverted for export.

### 3.3 Net and instance names

Schemify netexpr (netlist.md §4) ↔ Virtuoso nameExpr:

| Schemify | Virtuoso |
|---|---|
| `d[3:0]`, `d[3]` | `d<3:0>`, `d<3>` |
| `2*a` | `<*2>a` |
| `a,b[1:0]` | `a,b<1:0>` |
| instance `X1[3:0]` | `X1<3:0>` |
| `x!` | `x!` |
| global label `x` (`global true`) | `x!` |
| ground: `0`, and global labels spelled `gnd` in any case | `gnd!` (Spectre maps `gnd!` to node 0; netlist research §1.3) |

Import: `gnd!` → global label `GND` (ngspice's ground alias, xschem's spelling); `x!` → `x!` (a global by name in Schemify); step ranges, suffix repeats and grouping (`a<0:6:2>`, `a<0:2*2>`, `<*2>(a,b)`) expand to the equivalent bundle (`a[0],a[2],a[4],a[6]`); a bare tap (`<3:0>` with no base) and a non-numeric parenthesis (`x(a)`) are verbatim scalar names + `needs_review: name`. Characters Schemify identifiers lack (`-`, `+`, `#`, …) keep the name verbatim (elab §9.4 treats it as one scalar). Every name whose export spelling is not the default inverse of its import spelling stores the exact Schemify spelling in property `schemifyNet` on its label, so a round trip is exact.

### 3.4 Values

Schemify expressions (expr.md) ↔ Virtuoso CDF/AEL strings (Spectre semantics: case-sensitive scale factors, `M` = mega):

| Schemify | Virtuoso |
|---|---|
| number with suffix `f p n u m k meg g t` (any case; `M` = milli) | the canonical decimal with `f p n u m k M G T` (`1meg` → `1M`, `2M` → `2m`); never `meg`, never milli `M` |
| `mil` | `*25.4u` folded into the number |
| a parameter of the containing cell | `pPar("x")` |
| a bench var | the bare name (an ADE design variable) |
| a var of the containing component | its expression, inlined (Virtuoso cells have no variables), `needs_review: inlined-var` |
| `+ - * /`, unary `-`, parentheses | the same |
| `x^y`, `pow(x,y)` | `pow(x y)` → written `pow(x,y)` |
| `ln log10 exp sqrt abs min max sin cos tan asin acos atan atan2 sinh cosh tanh floor ceil int` | `log log10 exp sqrt abs min max …` (`ln` → `log`) |
| anything else (`if`, `?:`, comparisons, strings, plugin functions) | a constant-folded number when it has no identifiers; otherwise `needs_review: expression` and the instance keeps the CDF default |

Import is the inverse, plus: `iPar("x")` → the instance's own value of `x` (its override, else the cell default) in parentheses; a bare identifier that is no parent parameter and no bench var is a design variable — imported as a bench var when the cell is a bench, else `needs_review: design-variable`. String-kind parameters (`model`, choices) are literal text both ways (template braces doubled on import).

xschem export values: a number with a suffix is written as is (Schemify's suffixes are SPICE's); an expression with identifiers becomes a SPICE-quoted `'…'` expression (`^` → `**`, `ln` → `ln`, `log10` → `log10`, the identifiers verbatim: parent parameters are subcircuit parameters in xschem's netlist, bench vars are `.param`s) — the inverse of import.md §3's instance-value rule; anything untranslatable (strings, `if`, plugin functions) is reviewed and the instance keeps the symbol default.

### 3.5 Templates and labels

| Schemify `stext` / template | Virtuoso symbol label | xschem `T` text |
|---|---|---|
| `{name}` | NLP `[@instanceName]` on `instance label` | `@name` |
| `{cell}` | NLP `[@partName]` on `device label` | `@symname` |
| `{p}` (a parameter) | NLP `[@p]`; `pre{p}post` → `[@p:pre%post]` | `@p` |
| literal text | normalLabel on `annotate drawing` | text |
| any other hole | dropped, `needs_review: template` | dropped |

Virtuoso → Schemify adds: ILLabel `cdsName()` → `{name}`; `cdsParam(n)` → `<p>={<p>}` where `p` is the n-th entry of the cell CDF's `paramLabelSet` (dropped when it has none); `cdsTerm("t")` and `[@refDes]`, `[@userPartName…]` → dropped (runtime annotation); `[@p:a%b:c]` → `a{p}b` (the fallback `c` is lost, reviewed).

## 4. The `.sfyx` exchange file

One file carries any mix of cells, PDK reference cells and maestro setups, in both directions (the dumper writes it, the loader reads it, the Rust side reads and writes it). It is **data**: SKILL's `lineread` parses each line without evaluating it, and the loader dispatches on the record tag (D7).

### 4.1 Lexical rules

- UTF-8, LF line ends. One record per line: a parenthesised list whose first element is a string tag. A line that is empty or starts with `;` is skipped.
- Elements are only **strings**, **integers** and **lists** of elements. No symbols, no floats, no `nil`/`t` (booleans are `0`/`1`): a bare word would be read as a SKILL symbol, scale suffixes would apply to it (`1u`), a leading `0` would make it octal, and floats print with 7 digits.
- Strings: `"…"` with escapes `\\` `\"` `\n` `\t` `\r` and `\ooo` (three octal digits) for every other byte below 0x20 and 0x7f; other bytes raw. U+0000 never appears (a SKILL string cannot hold NUL; a writer refuses such a string). A string longer than 4000 bytes is written as a list of chunks `("…" "…")` wherever a field is marked *text* below (SKILL's parser refuses literals over 8191 characters); chunks are cut greedily on UTF-8 character boundaries, each at most 4000 bytes; readers concatenate.
- Integers: optional `-`, decimal digits, no leading zeros, within the 32-bit range.
- Writers emit elements separated by one space and nothing else; both writers produce byte-identical lines for the same data (a test compares the SKILL dumper run in the test interpreter with the Rust writer).

### 4.2 Records

Context records set the target of the records after them. `ref`s are positive integers unique within one cellview, assigned in the order objects are written; `0` means the cellview itself.

| Record | Fields | Meaning |
|---|---|---|
| `("sfyx" 1 producer)` | version, producer string | first line, always |
| `("lib" name)` | | library context |
| `("cell" name role)` | role `"design"` (exported/imported in full) or `"ref"` (a referenced master: symbol + CDF only, never written back) | cell context in the current library |
| `("cdf" paramLabelSet)` | text | the cell has a base CDF; its params and simInfo follow |
| `("param" name type defValue prompt units flags choices callback)` | `type` one of `string int float boolean radio cyclic button netSet`; `defValue` string (booleans `"t"`/`"nil"`, ints `%d`, floats `%.17g`, buttons `""`); `prompt` the name when unset; `units` string or `""`; `flags` list of strings, in this order, from `parseAsNumber parseAsCEL storeDefault dontSave hidden` (`hidden` = `display` is `"nil"`; an unset `display` means shown); `choices` list of strings; `callback` text | one CDF parameter, in CDF order |
| `("siminfo" simulator fields)` | `fields` = list of `(key value)` sorted by key; `value` is a string, or a list of strings (`termOrder`, `instParameters`, `otherParameters`), or a list of pairs (`propMapping`, `termMapping`), or `()` for `nil` | one simulator's simInfo, symbols printed as their names |
| `("cv" view viewType upi)` | view name, `"schematic"`/`"schematicSymbol"`, units per inch | cellview context |
| `("prop" ref name type value)` | `type` one of `string int float boolean ILList`; value string (`"TRUE"`/`"FALSE"` for booleans, ints `%d`, floats `%.17g`, ILLists their print form) | a property of object `ref` (0 = the cellview). Of the cellview's own properties only those named `schemify*` are dumped (the rest — `lastSchematicExtraction`, `instance#`, … — is Virtuoso bookkeeping that changes on every check); properties of other value types (`time fileName ILExpr NLPExpr hierProp`) are skipped and counted in the log |
| `("inst" ref name lib cell view x y orient)` | | an instance (`name` may be iterated `I0<3:0>`) |
| `("wire" ref points width)` | `points` = flat list `x1 y1 x2 y2 …`, `width` integer (0 = narrow) | one wire figure |
| `("label" ref parent text x y orient justify font height kind layer purpose)` | `parent` = ref of the glued figure (a `pinfig`, `inst`, `spin` or `wire`, written earlier) or 0 (none, or a figure the dump does not write); `kind` `normal`/`NLP`/`IL` | a label (wire name, note text, symbol label) |
| `("spin" ref term dir lib cell view x y orient)` | `dir` `input output inputOutput switch` | a schematic pin (an instance of `basic/ipin` …) |
| `("term" name dir netexpr)` | `netexpr` the inherited-connection expression or `""` | a terminal of a symbol (a schematic's terminals are its `spin`s); its pins follow as `pinfig`s |
| `("pinfig" ref term x1 y1 x2 y2)` | | a symbol pin rectangle (one per pin of the terminal) |
| `("shape" ref kind layer purpose coords width)` | `kind` `line rect polygon ellipse arc path`; `coords`: points for line/polygon/path, `x1 y1 x2 y2` box for rect/ellipse, `x1 y1 x2 y2 a0 a1` for arc (ellipse box, start and stop angle as `round(angle · 10^6)` micro-radians, counter-clockwise); `width` 0 except paths | any other figure (solder dots and text displays are not dumped, G15) |
| `("conn" ref term net)` | instance ref, terminal name, Virtuoso net name | loader-only: draw a stub + label for this terminal at its real pin position (§6.4) |
| `("textview" view file text)` | view name, master file name, *text* | a text view (`veriloga`/`veriloga.va`, `functional`/`verilog.v`) |
| `("maestro" test lib cell view simulator)` | | starts a maestro test context (§9) |
| `("var" name value scope)` | scope `""` (global) or a corner name | design variable |
| `("analysis" name enable options)` | `options` list of `(key value)` strings | one analysis of the current test |
| `("corner" name enable)` | | a corner |
| `("model" corner file section)` | corner `""` = all corners (test model files) | model file |
| `("output" name kind expr)` | kind `"point"`/`"net"`; `expr` the expression or signal | output |
| `("spec" output op value)` | op `lt gt` | spec bound |
| `("defn" file text)` | file name, *text* | a definition file the loader writes beside the maestro view and lists in the test's `definitionFiles` |
| `("intent" ref class fields)` | `fields` list of `(key value)` strings: `gmid l id vds w nf` | gm/Id sizing intent for an instance (§10) |
| `("end")` | | last line; a file without it is truncated and refused |

Order: `sfyx`; the `"ref"` cells, then the `"design"` cells, each group by library (libraries sorted by name, a `lib` record at each change) and by cell name; per cell `cell`, `cdf` + `param`s (CDF order) + `siminfo`s (by simulator name), then per view (sorted by view name) `cv` and its objects in the order `term pinfig inst spin wire label shape prop conn intent`, then the text views (by view name); then maestro records; `end`. Each kind is sorted by its key: `term` name; `pinfig` (term x1 y1 x2 y2); `inst` name; `spin` term; `wire` (points width); `label` (x y text layer purpose orient justify font height kind); `shape` (coords kind layer purpose width); `prop` (ref name); `conn` (ref term); `intent` ref. Keys compare integers by value, strings by bytes (G12), lists element by element with a prefix first, and integer < string < list. Refs are assigned in write order after sorting, so the same cellview always dumps to the same bytes.

### 4.3 Oracles

`sfyx::read(sfyx::write(d)) == d` (property, random dumps incl. every escape and chunking boundary); the test interpreter's `lineread` of every Rust-written line yields the same element tree (property); SKILL dumper output parsed by Rust equals the mock database contents (§5.4).

## 5. The SKILL side (`plugins/interop/skill/schemify.il`)

### 5.1 Subset

Every `.il` line the plugin ships is read by `tests/skill`'s reader, which refuses anything outside this subset, so a C-style call or an infix operator cannot ship:

- **Prefix form only**: `(f a b)`. No `f(a b)`, no infix operators, no `~>`/`->`: attribute access is `(getSGq obj attr)` and assignment `(setSGq obj value attr)` (what `~>` compiles to).
- Literals: strings, integers without leading zeros, floats with digits on both sides of `.`, `t`, `nil`, `'quoted` forms. No scale suffixes.
- Forms: `procedure` (`@optional`, `@key`, `@rest`), `let`, `prog`/`return`, `setq`, `if` (`(if c a b)` or `(if c then … else …)`), `when`, `unless`, `cond`, `case` (string and integer keys, `t` last), `foreach`, `setof`, `exists`, `for`, `while`, `and`, `or`, `errset`, `unwindProtect`, `gets` (a text view's lines), `lambda` (only as an argument to `sort`/`mapcar`).
- Functions: the interpreter's builtin list (`tests/skill/mod.rs` `BUILTINS`) and the Cadence functions the mock database implements (§5.4). `tests/skill_lint.rs` fails on any other called name and on arity mismatches against the signature table.
- No `eval`, `evalstring`, `load` of data, `sstatus` other than `printlength`/`printlevel` nil, no globals except `sfy`-prefixed ones. All names are prefixed `Sfy`.
- Coordinates are converted with `(round (times v upi))` on dump and `(quotient (float v) upi)` on load; every `fprintf` of a number uses `%d` on an integer. A float that must cross as text (a `float` CDF default, a float property) is `(sprintf nil "%.17g" v)` (exact round trip, G9).
- State is passed in dynamic variables named `sfy*`, `let`-bound by the entry points (the log port and counts; during an import the keyword arguments, the target table and the current cellview's ref table); the only global is the escape table `sfyEscapes`.

### 5.2 Entry points

| Procedure | Does |
|---|---|
| `(SfyExport path lib cells @key (ref t) (maestro t))` | Dumps `cells` (a list of names, `nil` = every cell of `lib`) of library `lib` to `path` as `"design"` cells, together with every cell of `lib` their schematics instantiate, transitively (a hierarchy crosses whole). Per design cell: base CDF, the views named `schematic` and `symbol`, text views (the `veriloga.va`/`verilog.v` master of any other view), maestro when `maestro` (`SfyAdeExport port lib cells`, when `sfy_ade.il` defines it). With `ref`, every master outside `lib` that a design schematic instantiates (schematic pins excepted) is a `"ref"` cell: its base CDF and the views the instances name. Writes `<path>.log` (counts per record kind, skipped objects, refusals, `PASS`/`FAIL`); returns `t` on PASS. |
| `(SfyExportLib path lib)` | A PDK library: every cell with a `symbol` view as a `"ref"` cell (CDF, simInfo, symbol); cells without one are skipped. Log as above. |
| `(SfyImport path @key (lib nil) (callbacks nil) (check t) (overwrite nil))` | Loads `path`. The whole file is read with `lineread` and checked first (header, `end`, every record's field kinds, every record in its context); a malformed or truncated file is refused with its line and nothing is created. Creates or replaces (with `overwrite`; else an existing view, text view or base CDF is refused) each `"design"` cell's base CDF, views and text views in library `lib` (default: the record's library; must already exist in cds.lib); never touches `"ref"` cells. Two passes: first every design cell's CDF, symbol and text views, then the schematics, so the dump's own masters exist (an instance of a design cell of the dump resolves to the target library) and the cross-view check sees the new symbol. Instances are created with `schCreateInst`, wires with `schCreateWire "draw"`, labels glued with `schCreateWireLabel`, schematic pins with `schCreatePin`, symbols with `db*` shapes, terminals with `dbMakeNet`/`dbCreateTerm`/`dbCreatePin`, net expressions with `dbCreateTermNetExpr`/`schCreateNetExpression`. Instance parameters go through `cdfGetInstCDF` (so the property type is the CDF's); with `callbacks`, each set parameter's CDF callback runs in CDF order under a locally bound `cdfgData`/`cdfgForm`, inside `errset` (§5.3). Each schematic is `schCheck`ed and saved only when the check reports no error (`check nil` saves anyway); an unsaved cellview is purged. Maestro records go to `SfyAdeImport records lib` when `sfy_ade.il` defines it. Writes `<path>.log`: `refused line N: …` per refusal, `check lib/cell/view: E errors W warnings` per schematic, `callback failed line N: inst param: callback`, `clamped line N: inst param sent X now Y`, then `count <kind> <n>` (sorted) and the final `PASS` (nothing refused; callback failures and clamps are not refusals) or `FAIL`; returns `t` on PASS. |
| `(SfyRetarget path map @key (lib nil) (characterize t))` | §10. |
| `(SfySelfTest lib cell scratchLib)` | Dumps `lib/cell` to `/tmp/sfy_selftest_a.sfyx`, loads it into `scratchLib` under the same cell names (`?overwrite t ?check nil`), dumps that to `/tmp/sfy_selftest_b.sfyx` and compares the two record by record, a string equal to `lib` in the first matching `scratchLib` in the second (the `lib` record, instances of the dump's own cells); prints `PASS` or the first differing records. Also runs `SfyRetargetSelfTest` (the pure-SKILL retarget math on built-in vectors, §10.4) when defined. The only test a user needs to run. |

Every database call whose `nil` result means failure is checked; a failure is logged with the record's line number and the object is skipped; the import continues (the log is the oracle of what landed).

### 5.3 CDF parameters and callbacks

Setting a parameter: `(setSGq (cdfFindParamByName cdf name) value value)` on the instance CDF, which stores the property with the CDF's type (the dumped text converted by the CDF type: `int` `atoi`, `float` `atof`, `boolean` `TRUE`/`t`); a `prop` that names no CDF parameter goes through `dbReplaceProp` with its dumped type. A value equal to the CDF default is not set (D40 matches Virtuoso's storeDefault "no"), except for a storeDefault parameter, whose instances always carry it. Callbacks (opt-in, default off because they are PDK code): parameters are set in CDF order, each set parameter's callback running right after it (as tabbing through the form would), `(errset (evalstring callback) t)` with `cdfgData` = the instance CDF and `cdfgForm` = the same object bound by `let`; afterwards every parameter is read back and differences from what was sent are logged (`clamped`), so the Rust side can show what the PDK changed. This is the only `evalstring` in the file, and it evaluates PDK code, never file data. (`ponytail:` the `id`-rewrite some PDK callbacks need — `cdfgData->id` as the cell — is not done; a user can load Cadence's `CCSinvokeCdfCallbacks` and run it after import.)

### 5.4 Test interpreter and mock database

`plugins/interop/tests/skill/`: `mod.rs` (reader, evaluator, builtins, in-memory files) and `mockdb.rs` (a Virtuoso model: libraries, cells, views, text-view files, cellviews with instances, shapes, labels, terminals, pins, nets-free extraction, CDF with defaults and storeDefault semantics, instance CDFs whose `value` writes the instance property; maestro and OCEAN in `mock_mae.rs`/`mock_ocean.rs`). Each mock function checks its arity and argument types against `mockdb::SIGNATURES` (taken from the IC20.1 manuals) and fails like the documented function (signals on a bad argument, returns `nil` where Virtuoso returns `nil`); list attributes come back in reverse creation order, so a dump that forgets to sort fails its golden. A cellview opened `"w"` keeps a backup until `dbSave`, so a purge restores it (or removes a never-saved view). The mock is the oracle only for our *reading* of the manuals; every GUESS it encodes is in §13 and marked `GUESS(Gn)` in the code.

Tests (`tests/virtuoso_skill.rs`, `tests/skill_lint.rs`): every shipped line reads; lint (every called function known, arities against the signature tables, `sfy`-only globals, one `evalstring`); the SKILL escaper equals `sfyx::write_str` and `lineread` gives the string back (property, every byte but NUL, UTF-8), text chunks equal `sfyx::text`; hand-built mock cellviews (`tests/fixtures/skill/build.il`: an inverter with analogLib masters, a bus/iterated design, a symbol with every shape kind and label language, a cell with a net expression, a text-view cell, a PDK device with every CDF type + simInfo) dump to canonical `.sfyx` equal to committed goldens; each golden → loader → dumper equals itself (modulo `lib`); `SfySelfTest` passes; callbacks run in CDF order and failures are logged, not fatal; malformed and truncated files are refused with their line and nothing is saved; a schematic with check errors is not saved; `conn` stubs land at the real pins; the probe runs. A maestro setup and the dump → Rust `Design` comparison come with `sfy_ade.il` and `virtuoso::read`.

### 5.5 First contact with a real Virtuoso

`SfySelfTest` plus `skill/sfy_probe.il` (`(SfyProbe scratchLib)`, the research probe: printing, orientations, `dbProduceMemName`, string splitting and order, the escaper round trip, analogLib globals and basic pin views, a reference symbol's anatomy, CDF attribute spellings, and in `scratchLib/sfy_probe/schematic` — opened `"w"`, purged, never saved — units, wires and solder dots, `schCreatePin`'s result, instance CDF storage, boolean properties, arcs, check bookkeeping) write `/tmp/sfy_probe.txt`. Its output is the golden that replaces the mock's GUESSes (§13); `tests/virtuoso_probe.rs` checks the mock against a committed probe file when one exists.

## 6. Virtuoso cells

### 6.1 What becomes what

| Virtuoso | Schemify |
|---|---|
| cell with `symbol` + `schematic` | `.shcomp`, impl `schematic`; leaf impls from text views |
| cell with `symbol`, no schematic (a device; every `"ref"` cell) | `.shprim`, one impl `spice` whose formats come from simInfo (§6.3); header `provenance "virtuoso <lib>/<cell>"` |
| cell with `schematic`, no symbol, with schematic pins | `.shcomp` with a derived interface (one pin per schematic pin, at a generated box symbol's sides) |
| cell with `schematic`, no symbol, no pins, or with a `maestro` view | `.shbench` (+ §9) |
| `veriloga` text view | impl `veriloga` with `body` = the file text; `functional`/`verilog` → impl `verilog` |
| `config` view | `needs_review: config` (§9.3) |
| analogLib / basic masters | the `analogLib`/`basic` mirror library (`"ref"` cells), with the built-in ngspice templates of §6.5; `basic/ipin opin iopin` are ports, `basic/noConn` is `noconn`, analogLib/basic global symbols (`gnd vdd vss vcc …`) are global labels |

Export is the inverse: `.shcomp` → symbol + schematic + CDF; `.shprim` → symbol + CDF + simInfo (spectre and auCdl from its `format spectre`/`format cdl`, §6.3); `.shbench` → schematic (no symbol) + maestro; `core:*` cells → library `schemify_core` (symbols drawn as in Schemify, CDF from the core params, simInfo from §6.5) unless the mapping sends them elsewhere; cells whose provenance is `virtuoso <lib>/<cell>` are never exported (they *are* the Virtuoso cell) and instantiate `<lib>/<cell>/symbol`.

### 6.2 Objects

| Virtuoso object | Schemify record |
|---|---|
| instance (`inst`) | `inst`, name `I0<3:0>` ↔ `I0[3:0]`; CDF props → overrides (§3.4); prop `schemifyId` ↔ id; netSet props → `inherit <pin> <net>`; prop `nlAction` `"ignore"` ↔ `ignore true` |
| wire (`wire`, wire/drawing) | one `wire` per segment; width > 0 ↔ `bus true`; a multi-point wire splits, the first segment keeping its id |
| wire label (wire/label, glued) | `label` at the point of its wire nearest the label origin (on a horizontal or vertical segment the projection; else the nearest end); its text through §3.3; `annotate drawing3` aliases are dropped |
| net-expression label `[@p:%:d!]` on a wire | the cell gains inherited pin `p` (`inherit d`, §3.3) and a label `p` there (elab §9.5) |
| schematic pin (`spin`) | `port` bound to the interface pin of that name, at the pin instance's origin; in a bench a `label` |
| `basic/noConn` instance | `noconn` at its origin |
| note label (text/drawing) | `text`, size = height · 160000 / upi / 10 (xschem text scale), orient by §3.2 |
| note shape (text/drawing, drawing1 = dashed) | `shape`; ellipse → `circle` when square, else a 32-point closed `poly` (reviewed) |
| symbol shape (device/drawing, drawing1 = filled) | rank-2 graphics; same ellipse rule; arcs with sweep > π are two Virtuoso arcs (export) / merged when they share centre and radius (import) |
| symbol label | `stext` by §3.5; pin-name labels (pin/label) are written by export and ignored by import (Schemify draws pin names) |
| symbol pin (`term` + `pinfig`) | `pin` at the rectangle centre; `dir` input/output/inputOutput ↔ in/out/inout (switch, jumper, unused → inout); several pins of one terminal → `alias` pins; term net expression → `inherit` |
| selection box (instance/drawing) | dropped on import; export writes the box around graphics, pins and labels |
| solder dots, markers, instance labels | dropped |
| cellview props `schemifyRecords1…N` | the Schemify-only records (§7.3) |

Port order: export writes simInfo `termOrder` (spectre and auCdl) = Schemify pin order; import takes `termOrder` of the cell's spectre simInfo, else Virtuoso's default (terminal names sorted ASCII, research §2.1).

Labels must glue to a wire. A Schemify label whose point lies on no wire gets a **stub**: a wire of one grid step (10 units) from the point away from the instance whose pin it sits on (or to the right when on none), the label glued to it, and prop `schemifyStub` `"1"` on the wire so import deletes it again.

### 6.3 CDF and simInfo

Schemify param ↔ CDF parameter:

| Schemify kind | CDF |
|---|---|
| `number` | `string`, flags `parseAsNumber parseAsCEL`, defValue by §3.4 |
| `int` | `int` |
| `bool` | `boolean` |
| `choice` | `cyclic`, `choices` |
| `string` | `string` (no parse flags) |
| `computed` | not a CDF param: exported as `string` with `parseAsCEL` whose default is the translated expression, `needs_review: computed` |
| `unit` | `units` by: m→lengthMetric, mil→lengthEnglish, F→capacitance, Ohm/Ω→resistance, H→inductance, S→conductance, s→time, Hz→frequency, W→power, dB→powerDB, deg→angle, V→voltage, A→current, C/K→temperature; others dropped |
| `doc` | `prompt` (else the name) |
| `min max step grid show` | Schemify-only (§7.3) |

Import: `float` → `number`; `radio` → `choice`; `button` dropped; `netSet` → not a param (its instance values are `inherit` overrides); a `string` with `parseAsNumber` and a numeric default → `number`, else `string`; `callback`, `display`, `editable`, `use`, `dontSave` are not representable → counted in the report (`needs_review: callback` for a non-empty callback).

simInfo → impl formats of a device (`.shprim`):
- **spectre** → `format spectre "{name} ({t1} {t2} …) <master> k1={c1} …"`: terminals in `termOrder`; master `{model}` when the cell has a `model` param with a value, else `componentName`, else the cell name; one `k={c}` per `instParameters` entry (`c` through `propMapping`), number params always, string params as `{c != "" ? " k=" + c : ""}`.
- **auCdl** → `format cdl`: `ansCdlSubcktCall` → `X{name} {pins} / <componentName|cell> k={c}…`; otherwise (`ansCdlCompPrim` and unknown procedures) `<namePrefix>{name} {pins} <modelName|{model}> K={c}…` with `propMapping` names, plus `$[<model>]` for namePrefix `R C L D` (research §3.4: LIKELY; reviewed `cdl-approximate`).
- **ngspice/xyce**: from the mapping file (§8) or the built-in analogLib table; otherwise none (netlisting such a device to ngspice is `E013`, as the report says).

Export of a `.shprim` reverses the spectre rule when its `format spectre` matches that shape (`{name} ({pins…}) master k={p}…` with every hole a pin or param) and the cdl rule likewise; any other template exports the symbol and CDF only, `needs_review: format` (the cell then netlists in Virtuoso only through a text view the user adds).

### 6.4 Re-mastered instances

When the mapping (§8) sends an instance to a master with **different pin geometry** (analogLib ↔ core with `--analoglib`, a PDK map between libraries of different origin, retarget classes), connectivity cannot ride on geometry. `remap::apply`, for each such instance, using `nets::extract` (netlist.md §3 union-find, net names by §3.3; an unnamed net is named `net_<id>` exactly as elab would):
1. every wire endpoint on one of the instance's old pin points is pulled back one grid step along its wire (a wire shorter than a step is deleted) and a label with the pin's net is glued at the new end;
2. the instance gets the new master at the same origin and orientation; if the new master's pin geometry is known (it is in the Design), a stub + label per pin is drawn at its real pins; otherwise `("conn" inst term net)` records tell the loader to do it at the pins it finds in Virtuoso;
3. after loading, the loader reports every real pin that coincides with a pulled-back end of another net (`schCheck` would call it a short).
Geometry-identical masters (mirrors, `schemify_core`, the user's own cells) are never re-mastered.

### 6.5 Built-in device table (analogLib ↔ core)

Used for the ngspice templates of analogLib mirror cells (import) and for the simInfo of `schemify_core` cells (export), and by `--analoglib` re-mastering. Pins by order; values through §3.4.

| core | analogLib | pins (core ↔ analogLib) | params (core ↔ analogLib) | ngspice template for the analogLib mirror |
|---|---|---|---|---|
| res | res | p n ↔ PLUS MINUS | value ↔ r, m ↔ m | `R{name} {PLUS} {MINUS} {r} m={m}` |
| cap | cap | p n ↔ PLUS MINUS | value ↔ c, m ↔ m | `C{name} {PLUS} {MINUS} {c} m={m}` |
| ind | ind | p n ↔ PLUS MINUS | value ↔ l, m ↔ m | `L{name} {PLUS} {MINUS} {l} m={m}` |
| vsource | vdc / vpulse / vsin / vpwl / vexp | p n ↔ PLUS MINUS | value (SPICE source text) ↔ the waveform params (§6.6) | `V{name} {PLUS} {MINUS} dc {vdc} ac {acm} {acp}` / `pulse(...)` / `sin(...)` / `pwl(...)` / `exp(...)` |
| isource | idc / ipulse / isin / ipwl / iexp | p n ↔ PLUS MINUS | as vsource | `I{name} …` likewise |
| vcvs | vcvs | p n cp cn ↔ PLUS MINUS NC+ NC- | value ↔ egain | `E{name} {PLUS} {MINUS} {NC+} {NC-} {egain}` |
| vccs | vccs | likewise | value ↔ ggain | `G{name} …` |
| cccs | cccs | p n ↔ PLUS MINUS | vnam ↔ vref, value ↔ fgain | `F{name} {PLUS} {MINUS} {vref} {fgain}` |
| ccvs | ccvs | p n ↔ PLUS MINUS | vnam ↔ vref, value ↔ hgain | `H{name} …` |
| diode | diode | p n ↔ PLUS MINUS | model, area | `D{name} {PLUS} {MINUS} {model} area={area}` |
| nmos4 / pmos4 | nmos4 / pmos4 | d g s b ↔ D G S B | model, w, l, m | `M{name} {D} {G} {S} {B} {model} w={w} l={l} m={m}` |
| npn / pnp | npn / pnp | c b e ↔ C B E | model, area, m | `Q{name} {C} {B} {E} {model} area={area} m={m}` |
| ammeter | iprobe | p n ↔ PLUS MINUS | — | `V{name} {PLUS} {MINUS} 0` |
| tline | tline | p1 n1 p2 n2 ↔ IN+ IN- OUT+ OUT- | z0 ↔ zo, td | `T{name} … z0={zo} td={td}` |
| parax_cap | cap | p gnd ↔ PLUS MINUS | value ↔ c | (as cap) |
| bsource, probe, switch, vswitch | — | | | not mapped: exported in `schemify_core` only |

Spectre simInfo of `schemify_core` cells: res → `resistor` `r=`, cap → `capacitor` `c=`, ind → `inductor` `l=`, vsource/isource → `vsource`/`isource` with `type=` and the waveform params of §6.6, vcvs/vccs/cccs/ccvs → `vcvs gain=`, `vccs gm=`, `cccs gain= probe=`, `ccvs rm= probe=`, diode/nmos4/pmos4/npn/pnp → master `{model}`, ammeter → `iprobe`, tline → `tline z0= td=`; bsource → `bsource` with `v=`/`i=` from `src`/`func`; probe → nothing (a `.save` in SPICE; not netlisted to spectre, reviewed); switch/vswitch → `relay`-free: not netlisted (reviewed).

### 6.6 SPICE source text ↔ waveform params

`value` of `core:vsource`/`isource` is parsed as SPICE independent-source text: `[DC] v`, `AC mag [phase]`, and one of `PULSE(v1 v2 td tr tf pw per)`, `SIN(vo va freq td theta)`, `PWL(t1 v1 t2 v2 …)`, `EXP(v1 v2 td1 tau1 td2 tau2)` (case-insensitive, commas or spaces). Export picks the analogLib cell by waveform (`vdc` when none) and the Spectre `vsource type=` likewise; params: `vdc acm acp` + pulse `v1 v2 td tr tf pw per`, sine `vo va freq td theta`, pwl `tvpairs t1 v1 …`, exp `v1 v2 td1 tau1 td2 tau2` (i-sources: `idc i1 i2 io ia` names). Import formats the same text back (`DC 1 AC 1 0 PULSE(0 1.8 0 10p 10p 5n 10n)`). Text that does not parse exports as `vdc` with `vdc` = the whole text, `needs_review: source`.

## 7. Round trip and teamwork

### 7.1 Who owns what

Virtuoso owns, after an import: instances, their parameters, wires, labels, pins, symbols, CDF params (name, kind, default, choices, unit), bench variables, analyses, corners, outputs and specs. Schemify owns: docs (`prose`, cell `var`s), param attributes Virtuoso has no field for (`min max step grid show computed`, `cached`), waivers, constraints, impl formats for dialects Virtuoso does not produce (`spice`, `xyce`, `verilog`), configs, sweeps, Monte Carlo and optimizer records beyond what §9 maps.

### 7.2 Ids

Every exported object carries prop `schemifyId` (instances, wires, labels, pins, shapes, notes); import reuses it when it is a valid, unused id of its kind. A teammate's new object has none and gets a deterministic import id (§2). A teammate's copy of an object duplicates its id: the second (in dump order) gets a fresh one.

### 7.3 Schemify-only data

Export writes the Schemify-only data of each cell as canonical record text into cellview props `schemifyRecords1…N` (strings ≤ 4000 bytes) on the schematic (benches: the schematic; devices: the symbol). Import rebuilds each target file from the dump, then takes the Schemify-only data from the **existing local file** when there is one, else from those props, and drops every piece whose subject is gone: a waiver whose instance id is absent, a param attribute whose param is gone, a format of an impl that no longer exists — each listed in the report (`dropped: stale-schemify-data`). A cell edited only in Virtuoso therefore keeps its docs and waivers, and a Schemify file is never silently overwritten: when the target exists and differs, the import writes it (the editor's watched-file merge, D28, shows the change) and the report lists the cell as `updated`.

### 7.4 Oracle

`import(export(x)) ≡ x` for every fixture project and generated designs (property: random cells with every record kind, buses, iterated instances, globals, inherited pins, params of every kind, benches): files byte-identical after `schemify fmt` except the documented losses of this spec (listed per fixture). `export(import(d)) ≡ d` for every dump fixture, and through the interpreter + mock (§5.4) in both directions.

## 8. PDK flows and the mapping file

### 8.1 Three ways a PDK crosses

1. **Mirror (any PDK in Virtuoso, open or commercial).** `SfyExportLib` dumps the PDK library; `virtuoso-import` writes a Schemify library of the same name with one `.shprim` per device (symbol, CDF params as Schemify params, spectre and cdl formats, `provenance virtuoso <lib>/<cell>`). Designs place these cells; export references the real PDK cells by name: geometry, parameter names and netlisting are identical by construction. The ngspice side of a mirror is added by the user (a `format spice` on the mirror cell, kept across re-imports, §7.1) or by a mapping rule (§8.2). Mirrors of commercial PDKs contain the PDK's symbols and parameter names: keep that library out of public repositories (the report says so; `virtuoso-import` writes a `.gitignore`-ready note into the library directory, never a file outside it).
2. **Map (two libraries that model the same devices differently)**, e.g. the open sky130 library imported from xschem (`sky130_fd_pr:nfet_01v8`, params `W L nf mult`) and a Virtuoso sky130 kit. Rules in `interop.map` (§8.2) translate cell, pins and params; instances are re-mastered (§6.4).
3. **Retarget (PDK-agnostic).** The design is sized by gm/Id on a sample PDK in Schemify (`plugins/gmid-lut`); export carries the *intent* (§10), and the target PDK — present only on the target machine — is characterized and the widths solved there, in SKILL.

### 8.2 `interop.map`

Project-root file in line-record syntax (format.md §2, no header), read by both directions:

```
map sky130:nfet_01v8 virtuoso sky130_fd_pr nfet_01v8
  pin d D
  pin g G
  pin s S
  pin b B
  param W w scale 1e-6
  param L l scale 1e-6
  param nf nf
  param mult m
  drop ad
  spice "X{name} {D} {G} {S} {B} sky130_fd_pr__nfet_01v8 w={w} l={l} nf={nf} m={m}"

class nmos virtuoso_target nch
  role n
```

- `map <schemify cellref> virtuoso <lib> <cell> [view]` (view default `symbol`): one cell each way. `pin <schemify> <virtuoso>`, `param <schemify> <virtuoso> [scale <factor>]` (Virtuoso value = Schemify value × factor, both as expressions; import divides), `drop <param>` (not sent / not taken), `set <virtuoso param> <value>` (always sent), `spice "<template>"` (the ngspice format for the Virtuoso cell's mirror). `same` (no other attribute) declares identical geometry: no re-mastering.
- `class <name> virtuoso_target <hint>` + `role n|p`: a retarget class (§10).
- Lookups: an instance's cell has a `map` row → it; else the cell is a mirror (provenance) → itself; else `core:*` → `schemify_core` (or analogLib with `--analoglib`); else a user cell exported with the design. Errors (a pin of the cell left unmapped, an unknown cell) refuse the export with the list.

## 9. ADE (maestro) ↔ bench

### 9.1 Export of a `.shbench`

One maestro view on the bench cell with one test named after the cell (Virtuoso-safe characters), design = the bench schematic, simulator `spectre`.

| Bench | Maestro |
|---|---|
| `var name expr` | `("var" name value "")`, value by §3.4 |
| `sweep` of a var: `values a b c` / `lin a b n` / `log a b n` | the var's value becomes the comma list `"a,b,c"` (points of run.md §2, canonical text, §3.4 suffixes) |
| `corner name` with `lib path section`, `include path`, `set var expr`, `temp t` | `("corner" name 1)`, `("model" name path section)` (include: section `""`), `("var" var value name)`, `("var" "temperature" t name)`; `${NAME}` in paths → `$NAME` |
| `code` lines `.tran tstep tstop [tstart [tmax]]` | `("analysis" "tran" 1 (("stop" tstop) ("start" tstart) ("maxstep" tmax)))` |
| `.ac dec|oct|lin n f1 f2` | `("analysis" "ac" 1 (("start" f1) ("stop" f2) ("incrType" "Logarithmic") ("stepTypeLog" "Points Per Decade") ("dec" n)))`; `lin` → `("incrType" "Linear") ("stepTypeLin" "Number of Steps") ("lin" n)` |
| `.dc <source> start stop step` | `("analysis" "dc" 1 (("sweep" "Component Parameter") ("dev" "/<source>") ("param" "dc") ("start" …) ("stop" …) ("step" …)))` |
| `.op` | `("analysis" "dc" 1 (("saveOppoint" "t")))` |
| `.noise v(out[,ref]) src dec n f1 f2` | `("analysis" "noise" 1 (("start" f1) ("stop" f2) ("oprobe" …) ("iprobe" "/src") …))` |
| `.temp t` | var `temperature` (global) |
| `.param k=v` | var `k` |
| `.include f` / `.lib f s` | `("model" "" f s)` |
| other code (`.save`, `.option`, `.meas`, `.control … .endc`, device lines) | lang `spectre` blocks → `("defn" "<name>.scs" text)`; SPICE text that is not an analysis → `needs_review: code` (stashed, §7.3) |
| `output name expr` | `("output" name "point" ade)`: `v(n)` → `VT("/n")` (tran), `VF` (ac), `VS` (dc), `VDC` (op); `i(dev)` → `IT("/dev/PLUS")` …; functions `max min avg rms integ deriv` → `ymax ymin average rms integ deriv`, `meas`-free; other functions → `needs_review: output` (stashed) |
| `spec out min a max b` | `("spec" out "gt" a)` and `("spec" out "lt" b)` (one of each at most; both bounds → both records) |
| `montecarlo`, `optimize`, `config` | stashed (§7.3), `needs_review: <kind>` |

### 9.2 Import

The dumper reads the maestro setup (`maeOpenSetup` read-only, `maeGetSetup`, `maeGetEnabledAnalysis`, `maeGetAnalysis`, `asiGetDesignVarList`, `maeGetTestOutputs`, `maeGetEnvOption "modelFiles"`, corners via `maeGetSetup ?typeName "corners"` and the `axl*` setup DB) and writes the records of §4.2; the inverse of 9.1 rebuilds bench records: analyses become one `code analyses spectre` block in Spectre syntax (`tran tran stop=… `), plus a `code analyses spice` block when every analysis has a SPICE equivalent; comma-list vars become `sweep` records; corners map back; outputs whose ADE expression parses back (the table above) become `output`, others stay as `code` comments + `needs_review: output`.

### 9.3 Not mapped in v1

Config views (impl selection) ↔ bench `config`: stashed. Monte Carlo settings, optimization setups, run modes, simulator options, convergence aids: stashed or left to the user (report).

## 10. gm/Id retarget (PDK-agnostic)

### 10.1 Intent

An instance whose width override calls `gmid.w(dev, gm_id, l, id)` or `gmid.w_at(dev, gm_id, l, id, vds)` (plugin `gmid-lut`) is sized by intent. With `--retarget`, export evaluates the arguments in the instance's scope (host `eval` in the editor; `schemify_expr` + the bench vars in the CLI), maps `dev` through `class` rows of `interop.map` (`dev` = the class name, or a `class` row's name whose `role` matches), and writes `("intent" ref class (("gmid" g) ("l" l) ("id" id) ("vds" v) ("w" w_sample) ("nf" nf)))` with numbers as canonical text. The instance's master becomes the class placeholder: `("inst" ref name "" class "" …)` and `conn` records (§6.4).

### 10.2 Target map (on the target machine only)

`<name>.retarget` (line records), written by the target user:

```
class nmos lib tsmc28 cell nch_lvt_mac
  pin d D
  pin g G
  pin s S
  pin b B
  param w w
  param l l
  param nf nf
  model "include \"/pdk/models/spectre/toplevel.scs\" section=tt"
  device "M0 (d g s b) nch_lvt_mac"
  vdd 0.9
  lgrid 30n
  fingerw_max 2u
```

### 10.3 Target procedure (`SfyRetarget`)

1. For each class used: characterize with Spectre through OCEAN — one netlist per class (the `model` line, the `device` line, DC sources on d/g/s/b), a DC sweep of `vgs` over `[0, vdd]` at each L of the intents (snapped to `lgrid`) and the intent's `vds` (else `vdd/2`), saving `gm`, `id`, `gds`, `cgg` op points; results kept in a SKILL table per (class, L).
2. For each intent: find the VGS where `gm/id` equals `gmid` (monotone inverse lookup from high VGS down, crossing local dips — the gmid-lut rule, its README "Query semantics"), interpolate `id/W` there, `W = id / (id/W)`; split into fingers when `W / nf > fingerw_max`; place the class's master with the solved `w l nf` (snapped to the PDK grid by the CDF callbacks when `?callbacks t`), then stubs + labels from the `conn` records.
3. Write `<path>.retarget.log`: per instance the intent, the solved W, nf, L, VGS, and the residual `gm/id` at the solution; per class the characterization grid. Nothing about the target PDK leaves this log.

### 10.4 Oracle

The interpolation and solve are written once in pure SKILL (no database calls) and once in Rust (`retarget.rs`, the reference); test vectors over a synthetic EKV table (`plugins/gmid-lut/src/synth.rs`) must agree within 1e-9 relative between the interpreter running the SKILL and the Rust reference, and within gmid-lut's own 2 % of the analytic answer. `SfySelfTest` re-runs the same vectors inside Virtuoso (no simulation) and prints PASS/FAIL.

## 11. xschem export

Schemify → xschem `.sch`/`.sym` (file format from xschem-feature-inventory §5, checked by running xschem):

| Schemify | xschem |
|---|---|
| `.shcomp` | `<cell>.sym` (`type=subcircuit`, `format="@name @pinlist @symname"`, `template` = params with defaults, `B 5` pin boxes with `name dir`, graphics on layer 4, `T` texts by §3.5) + `<cell>.sch` |
| `.shprim` | `<cell>.sym` with `type=primitive`, `format` = its `spice` template converted by §3.5 (`{p}` → `@p`, `{pins}` → `@pinlist`, `{name}` → `@name`), `template`; cells with xschem provenance are not written: instances point at the original symbol |
| `.shbench` | `<cell>.sch`; bench `code` → `code_shown.sym` instances (`value=`), vars → one `.param` code block, corners → `.lib`/`.include` lines of the default corner (others reviewed), sweeps/outputs/specs/optimizer → reviewed |
| `core:*` | `devices/<sym>.sym` by inverting `DEVICE_MAP` (import.md §6) |
| `inst` | `C {sym} x y rot flip {name=… overrides}` (§3.2 inverted) |
| `wire` | `N x1 y1 x2 y2 {}` (`bus=true` for buses) |
| `label` | `lab_pin.sym` instance (`lab=`; globals as `vdd.sym`/`gnd.sym`-style `global=true` attribute) at the point |
| `port` | `ipin.sym`/`opin.sym`/`iopin.sym` by the pin's dir |
| `noconn` | `noconn.sym` |
| `text`, `shape` | `T`, `L B A P` on layer 4 |

Oracles: for every fixture, `xschem import(xschem export(x))` netlists like `x` (spicecmp), and the exported files netlisted by a real xschem (nightly, `nix develop`) equal Schemify's own ngspice netlist of `x` under spicecmp.

## 12. Report

`interop-report.md` + `.txt` beside the outputs, like import.md §7: classes `needs_review:<reason>` (`expression template name callback design-variable inlined-var computed format cdl-approximate source code output config montecarlo optimize`), `dropped:<reason>` (`display-only selection-box solder-dot stale-schemify-data pin-label`), `rounding`, `off-grid`, `remapped` (per instance: old and new master), `clamped` (from the loader log), `updated` (cells changed by an import). Count conservation per cellview: objects in = records out + reported.

## 13. GUESSes and their probes

| # | Assumption | Settled by |
|---|---|---|
| G1 | `MYR90`/`MXR90` point maps (§3.2) | probe `orient` lines |
| G2 | schematic/symbol `DBUPerUU` 160, user unit inch | probe `units` |
| G3 | analogLib `gnd`/`vdd` terminal names `gnd!`/`vdd!`; `basic/iopin` view `symbol` | probe `symshape` of each |
| G4 | setting a CDF value equal to its default removes nothing (we never set it, §5.3) | — |
| G5 | `schCreateWire "draw"` adds no segments and no solder-dot figures we would dump | self-test round trip |
| G6 | maestro read in a background session (`maeOpenSetup ?mode "r"`) works without a window | self-test on a bench |
| G7 | auCdl formatting of devices (`$[model]`, case of `W L M`) | compare with a real CDL netlist |
| G8 | Spectre port order default = sorted terminal names | compare with a real netlist |
| G9 | `%d` of `round(…)` and our escaper round-trip every byte | self-test |
| G10 | `dbCreateArc` + setting `startAngle`/`stopAngle` gives the arc | self-test; probe `arc` |
| G11 | `schCreatePin` returns the pin instance (the loader also accepts a pin object and takes its `fig`); pin instances do not take names the dump's instances use (instances are created first) | probe `schCreatePin returns` |
| G12 | `alphalessp` (the dump's sort) compares bytes, like C `strcmp` | probe `string alphalessp` |
| G13 | a `netSet` value set through the instance CDF is stored as a `string` property | probe on a PDK `_3` device |
| G14 | `schCheck` stamps bookkeeping properties on the cellview (`lastSchematicExtraction`, …); the dumper ignores every cellview property not named `schemify*` either way | probe `cvprop` |
| G15 | a solder dot is a `dot` (or `ellipse`) on `wire drawing`; the dumper skips every non-line figure there | probe `solder` |
| G16 | CDF flags read back as `"yes"`/`t`, an unset `display` as `nil`, `paramType` as the type string | probe `cdfparam` |
| G17 | `parseString s ""` splits characters or bytes and `charToInt` of a high byte may be negative; the chunker handles all four cases | probe `string parseString-utf8`, `charToInt-high` |

## 14. Amendments
