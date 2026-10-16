# Interop plugin v1: Virtuoso and xschem (normative)

Implements D24 as amended 2026-10-12 (interop is a plugin; xschem import **and export**; Cadence Virtuoso import and export) and D41 (this bridge). Readers and writers live in `plugins/interop`, never in a core crate. The xschem *import* is specified in [import.md](import.md); this spec covers everything else and the shared model under both.


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

**Native CLI** `schemify-interop` (feature `cli`): `xschem-import …` (import.md), `xschem-export <project> <cell>… -o <dir>`, `virtuoso-export <project> <cell>… -o <file.sfyx> [--map F] [--lib L=V]… [--core-cells]` (also writes the OCEAN scripts `ocean/<cell>.ocn` beside the `.sfyx`, §9.6; core devices go to their analogLib masters unless `--core-cells`, §6.5), `virtuoso-import <file.sfyx> <project> [--map F] [--into <lib>]`, `install-skill <dir>` (writes `schemify.il`). Exit codes as the core CLI (0 ok, 1 design problems reported, 2 usage/I/O).

**Editor commands** (wasm guest, capabilities `read_project` + `write_project`): `interop.virtuoso_export <cell> [<out.sfyx>]`, `interop.virtuoso_import <file.sfyx>`, `interop.xschem_export <cell> [<dir>]`, `interop.xschem_import <file.sch|.sym>…`, `interop.install_skill`. File ▸ Interop submenu; each without arguments opens a path panel (cktimg's pattern). Reads follow references from the named cells (the guest has no directory listing): the closure of `cellref`s through the project manifest, plus `interop.map` and the existing target files for merging.

## 2. Design load and store

- **load:** every file of the closure is opened through `schemify_doc` (so only valid files are exported; a file that fails to load stops the export with its diagnostics) and its records are converted to Design rows. Closure = the named cells plus every `cellref` reachable from their schematics, through `lib:cell` and `./cell`; `core:*` cells come from `schemify_doc::core_library()`.
- **store:** Design rows → canonical Schemify text (`schemify_format::write`). When the target file exists, its **Schemify-only data is kept** (§7.3): the stored file is the existing file with the tool-owned parts replaced. Ids come from the source (`schemifyId`, §6.2); objects without one get `IdRng::for_import("<lib>/<cell>", ordinal)` ids (Virtuoso library and cell), the ordinal being the object's rank in the dump's deterministic order (instances, schematic pins, wire segments, labels, shapes; every `schemifyId` of the cellview is reserved first, so a fresh id never takes one a later object wants), so re-importing an unchanged dump is a no-op diff. The store is `merge` (§7.3); a library the manifest lacks is added as `lib <name> path <dir>` (dir: `ImportOptions::lib_dir`, else the name) and the manifest returned as a file.
- **Library names.** The project root (Schemify library `""`) is the Virtuoso library `schemify`, bundled `core` cells are `schemify_core`, every other library keeps its name; `--lib L=V` (export) and `ImportOptions::lib_map` (import, the inverse) rename. An instance whose master lands in its own directory is written `./cell`, else `lib:cell`.

## 3. Profiles (tool syntax as data)

### 3.1 Coordinates

| Tool | Unit | y | Exact mapping |
|---|---|---|---|
| xschem | 1 xschem unit = 1000 milli | down | identity (import.md, D33) |
| Virtuoso | user unit inch; `.sfyx` integers in 1/`upi` inch, `upi` per cellview | up | `x_milli = x · 160000 / upi`, `y_milli = −y · 160000 / upi` |

One Virtuoso schematic DBU at the standard `DBUPerUU = 160` is one xschem unit: the 1/16″ snap grid is xschem's 10-unit grid and the 1/8″ pin pitch its 20-unit pitch. Both writers use milli-units, `upi = 160000`: the SKILL dumper writes `round(v · 160000)`, exact for every coordinate on any DBU grid that divides 160000 (160, 1000, …) and keeping sub-DBU values such as text heights (a 0.15-size text is 1.5 DBU at 160 per inch; dumping whole DBU would round it), so both sides produce the same numbers for the same object. Readers accept any `upi`. A conversion that is not exact rounds to the nearest milli-unit and is reported (`rounding`). The loader places at `v / float(upi)` inch; Virtuoso snaps to its DBU grid, and the exporter reports every coordinate that is not a multiple of `160000 / DBUPerUU` milli (`off-grid`, assuming 160).

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
| ground: `0`, and global labels spelled `gnd` in any case | `gnd!` (Spectre maps `gnd!` to node 0) |

Import: `gnd!` → global label `GND` (ngspice's ground alias, xschem's spelling); `x!` → `x!` (a global by name in Schemify); step ranges, suffix repeats and grouping (`a<0:6:2>`, `a<0:2*2>`, `<*2>(a,b)`) expand to the equivalent bundle (`a[0],a[2],a[4],a[6]`); a bare tap (`<3:0>` with no base) and a non-numeric parenthesis (`x(a)`) are verbatim scalar names + `needs_review: name`. Characters Schemify identifiers lack (`-`, `+`, `#`, …) keep the name verbatim (elab §9.4 treats it as one scalar). Every name whose export spelling is not the default inverse of its import spelling stores the exact Schemify spelling in property `schemifyNet` on its label, so a round trip is exact.

### 3.4 Values

Schemify expressions (expr.md) ↔ Virtuoso CDF/AEL strings (Spectre semantics: case-sensitive scale factors, `M` = mega):

| Schemify | Virtuoso |
|---|---|
| number with suffix `f p n u m k meg g t` (any case; `M` = milli) | the number's digits as written with the suffix respelled `f p n u m k M G T` (`1meg` → `1M`, `2M` → `2m`, `1.50K` → `1.50k`); never `meg`, never milli `M`; a number with both an exponent and a suffix folds to canonical number text (expr §7: Spectre ignores a factor after an exponent) |
| `mil` | folded into the number: canonical text of `v · 25.4e-6` |
| a parameter of the containing cell | `pPar("x")` |
| a bench var | the bare name (an ADE design variable) |
| a var of the containing component | its expression, inlined (Virtuoso cells have no variables), `needs_review: inlined-var` |
| `+ - * /`, unary `-`, parentheses | the same |
| `x^y`, `pow(x,y)` | `pow(x y)` → written `pow(x,y)` |
| `ln log10 exp sqrt abs min max sin cos tan asin acos atan atan2 sinh cosh tanh floor ceil int` | `log log10 exp sqrt abs min max …` (`ln` → `log`) |
| anything else (`if`, `?:`, comparisons, strings, plugin functions) | a constant-folded number when it has no identifiers; otherwise `needs_review: expression` and the instance keeps the CDF default |

Import is the inverse, plus: `iPar("x")` → the instance's own value of `x` (its override, else the cell default) in parentheses; a bare identifier that is no parent parameter and no bench var is a design variable — imported as a bench var when the cell is a bench, else `needs_review: design-variable`. String-kind parameters (`model`, choices) are literal text both ways (template braces doubled on import).

Decided (`virtuoso::ael`):
- Both directions rewrite text token by token and keep the source's spacing and parentheses, so a value needing no rewrite comes back byte for byte; only `x^y` is restructured (`pow(x,y)`, comes back `pow(x,y)`). `pi` is written as its value.
- A CDF *default* is evaluated in the cell's own scope: a parameter of the same cell is `iPar("x")` on export and `x` on import.
- Import respells Spectre factors `T G M K k m u n p f` as `t g meg k k m u n p f`; `a c % _` (no Schemify suffix) and a factor after an exponent fold to canonical number text; unit letters after a factor are dropped (`1pF` → `1p`); `5.` becomes `5.0`; `**` becomes `^`; `log` becomes `ln`. A value naming an AEL-only function (`hypot fmod asinh acosh atanh`) or `pPar` of a non-parameter is reviewed (`expression`); a translated override that does not compile as a Schemify expression is dropped and reviewed, never written.
- A string-kind value with template holes has no CDF form: it is sent as its text and reviewed (`template`).

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

Decided: parameter NLP labels and literal labels sit on `annotate drawing`; literal text inside an NLP format escapes `\ [ ] : %` with `\` (and is unescaped on import); `[@partName]`/`[@cellName]` → `{cell}`. A `stext` anchor maps to `justify`: no flag `upperLeft`, `hcenter` `upperCenter`, `vcenter` `centerLeft`, both `centerCenter`; on import a justification containing `Center`/starting with `center` sets the flag. Text height is `size · 10000` milli-units, i.e. `size = height · 16 / upi` (a `stext`/`text` without `size` exports the editor's defaults 0.3/0.4; import always writes `size`). A template with several holes, each a parameter or `eng(<parameter>)` (a display formatter: NLP shows the value as entered), is one NLP label of consecutive groups, the literal before the first hole the first group's `pre`, each literal after a hole that hole's `post`: `{eng(w)}/{eng(l)}` → `[@w:%/][@l]`, `{value}·i({vnam})` → `[@value:%·i(][@vnam:%)]` (GUESS G23: Virtuoso evaluates several groups in one NLP label); import concatenates the groups' templates (the `eng()` is lost).

## 4. The `.sfyx` exchange file

One file carries any mix of cells, PDK reference cells and maestro setups, in both directions (the dumper writes it, the loader reads it, the Rust side reads and writes it). It is **data**: SKILL's `lineread` parses each line without evaluating it, and the loader dispatches on the record tag (D7).

### 4.1 Lexical rules

- UTF-8, LF line ends. One record per line: a parenthesised list whose first element is a string tag. A line that is empty or starts with `;` is skipped.
- Elements are only **strings**, **integers** and **lists** of elements. No symbols, no floats, no `nil`/`t` (booleans are `0`/`1`): a bare word would be read as a SKILL symbol, scale suffixes would apply to it (`1u`), a leading `0` would make it octal, and floats print with 7 digits.
- Strings: `"…"` with escapes `\\` `\"` `\n` `\t` `\r` and `\ooo` (three octal digits) for every other byte below 0x20 and 0x7f; other bytes raw. U+0000 never appears (a SKILL string cannot hold NUL; writers drop it). A string longer than 2000 bytes is written as a list of chunks `("…" "…")` wherever a field is marked *text* below (SKILL's parser refuses literals over 8191 characters); chunks are cut greedily on UTF-8 character boundaries, each at most 2000 bytes (escaped, at most 8000 characters: under SKILL's 8191-character literal limit); readers concatenate.
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
| `("prop" ref name type value)` | `type` one of `string int float boolean ILList netSet` (`netSet`: an inherited-connection override, value the net name); value string (`"TRUE"`/`"FALSE"` for booleans, ints `%d`, floats `%.17g`, ILLists their print form) | a property of object `ref` (0 = the cellview). Of the cellview's own properties only those named `schemify*` are dumped (the rest — `lastSchematicExtraction`, `instance#`, … — is Virtuoso bookkeeping that changes on every check); properties of other value types (`time fileName ILExpr NLPExpr hierProp`) are skipped and counted in the log |
| `("inst" ref name lib cell view x y orient)` | | an instance (`name` may be iterated `I0<3:0>`) |
| `("wire" ref points width)` | `points` = flat list `x1 y1 x2 y2 …`, `width` integer (0 = narrow) | one wire figure |
| `("label" ref parent text x y orient justify font height kind layer purpose)` | `parent` = ref of the glued figure (a `pinfig`, `inst`, `spin` or `wire`, written earlier) or 0 (none, or a figure the dump does not write); `kind` `normal`/`NLP`/`IL` | a label (wire name, note text, symbol label) |
| `("spin" ref term dir lib cell view x y orient)` | `dir` `input output inputOutput switch` | a schematic pin (an instance of `basic/ipin` …) |
| `("term" name dir netexpr)` | `netexpr` the inherited-connection expression or `""` | a terminal of a symbol (a schematic's terminals are its `spin`s); its pins follow as `pinfig`s |
| `("pinfig" ref term x1 y1 x2 y2 [dir])` | `dir` (the terminal's direction) only in a schematic, which dumps no `term`s | a symbol pin rectangle (one per pin of the terminal); in a schematic, a pin drawn as a bare pin shape (Cadence's `sample` library) |
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
| `(SfyExport path lib cells @key (ref t) (maestro t))` | Dumps `cells` (a list of names, `nil` = every cell of `lib`) of library `lib` to `path` as `"design"` cells, together with every cell of `lib` their schematics instantiate, transitively (a hierarchy crosses whole). Per design cell: base CDF, the views named `schematic` and `symbol`, text views (the `veriloga.va`/`verilog.v` master of any other view), maestro when `maestro` (`SfyAdeExport port lib cells`, when `sfy_ade.il` defines it). With `ref`, every master outside `lib` that a design schematic instantiates (schematic pins excepted) is a `"ref"` cell: its effective CDF (`cdfGetCellCDF`: the base CDF plus the library and user layers its instances see) and the views the instances name. A master `ddGetObj` cannot find (its library left cds.lib) is refused (`master lib/cell not found`) and skipped; the rest dumps. Writes `<path>.log` (counts per record kind, skipped objects, refusals, `PASS`/`FAIL`); returns `t` on PASS. |
| `(SfyExportLib path lib)` | A PDK library: every cell with a `symbol` view as a `"ref"` cell (CDF, simInfo, symbol); cells without one are skipped. Log as above. |
| `(SfyImport path @key (lib nil) (callbacks nil) (check t) (overwrite nil))` | Loads `path`. The whole file is read with `lineread` and checked first (header, `end`, every record's field kinds, every record in its context); a malformed or truncated file is refused with its line and nothing is created. Creates or replaces (with `overwrite`; else an existing view, text view or base CDF is refused) each `"design"` cell's base CDF, views and text views in library `lib` (default: the record's library; must already exist in cds.lib); never touches `"ref"` cells. Two passes: first every design cell's CDF, symbol and text views, then the schematics, so the dump's own masters exist (an instance of a design cell of the dump resolves to the target library) and the cross-view check sees the new symbol. Instances are created with `schCreateInst`, wires with `schCreateWire "draw"`, labels glued with `schCreateWireLabel`, schematic pins with `schCreatePin`, symbols with `db*` shapes, terminals with `dbMakeNet`/`dbCreateTerm`/`dbCreatePin`, net expressions with `dbCreateTermNetExpr`/`schCreateNetExpression`. Instance parameters go through `cdfGetInstCDF` (so the property type is the CDF's); with `callbacks`, the set parameters' CDF callbacks run as the Edit Properties form runs them (§5.3). A target cellview open other than read-only (an editor window, `dbGetOpenCellViews`) is refused (`… is open for editing in Virtuoso; close it first`), never rewritten under the window. Masters are opened read-only once and closed when the import ends. Each schematic is `schCheck`ed and saved only when the check reports no error (`check nil` saves anyway); an unsaved cellview is purged. Maestro records go to `SfyAdeImport records lib` when `sfy_ade.il` defines it. Writes `<path>.log`: `refused line N: …` per refusal, `check lib/cell/view: E errors W warnings` per schematic, `callback failed line N: inst param: callback`, `clamped line N: inst param sent X now Y`, `derived line N: inst param file X now Y` (§5.3), then `count <kind> <n>` (sorted) and the final `PASS` (nothing refused; callback failures and clamps are not refusals) or `FAIL`; returns `t` on PASS. |
| `(SfySelfTest lib cell scratchLib)` | Refuses up front a `scratchLib` equal to `lib` (the load overwrites) or missing from cds.lib. Dumps `lib/cell` to `/tmp/sfy_selftest_a.sfyx`, loads it into `scratchLib` under the same cell names (`?overwrite t ?check nil`), dumps that to `/tmp/sfy_selftest_b.sfyx` and compares the two record by record, a string equal to `lib` in the first matching `scratchLib` in the second (the `lib` record, instances of the dump's own cells); prints `PASS` or the first differing records, and writes the same lines to `/tmp/sfy_selftest.txt`. The only test a user needs to run. |

Every database call whose `nil` result means failure is checked; a failure is logged with the record's line number and the object is skipped; the import continues (the log is the oracle of what landed). A SKILL error (a call that signals instead of answering nil) is caught per cell (export, import's first pass), per cellview, per maestro test and for the whole entry point: it is logged as `refused …: SKILL error <message>` (`errset.errset`, the property `errset` of the symbol `errset`, read with `getqq` right after the `errset` that caught it and outside any other `errset`, since an `errset` clears it; a record whose fields are not strings, integers or lists of those signals before a byte of it is written, so a caught error never leaves half a line), a cellview it left open for writing is purged, and the log still ends with `FAIL`. Every log starts with the Virtuoso version (`getVersion t`) and the Cadence functions the scripts call that `isCallable` denies on this release (`SfyCadenceFns`, equal to the mock's signature table by test), and is flushed (`drain`) after every line; the CIW gets a `schemify: dumping|loading …` line per cell or cellview, so `CDS.log` shows where a run that stopped on a dialog was. A written text view is not passed to `ahdlUpdateViewInfo` (it can open a port-mismatch dialog); the log says how to run it.

### 5.3 CDF parameters and callbacks

Setting a parameter: `(setSGq (cdfFindParamByName cdf name) value value)` on the instance CDF, which stores the property with the CDF's type (the dumped text converted by the CDF type: `int` `atoi`, `float` `atof`, `boolean` `TRUE`/`t`); a `prop` that names no CDF parameter goes through `dbReplaceProp` with its dumped type. A value equal to the CDF default is not set (D40 matches Virtuoso's storeDefault "no"), except for a storeDefault parameter, whose instances always carry it. Callbacks (opt-in for `SfyImport`, default off because they are PDK code), run as the Edit Properties form runs them — the sequence virtuoso-bridge-lite runs against live PDKs (research virtuoso-tooling-prior-art.md §5): every value is set first (in CDF order); then the master's cell CDF (`cdfGetCellCDF`) is loaded with the instance's values and bound by `let` as `cdfgData` and `cdfgForm` (so `cdfgData->id` is the cell, as PDK callbacks expect), each set parameter's callback runs in CDF order as `(errset (evalstring callback) t)`, `cdfUpdateInstParam inst` writes the result (and the `doneProc`'s) to the instance — `cdfgData`/`cdfgForm` stay bound to the cell CDF through it, as in the tested sequence — and the cell CDF gets its own values back (an `unwindProtect` whose cleanup has no `errset`, so an error's message survives). A failed callback or update is logged `callback failed line N: inst param: callback`. With callbacks a parameter the PDK locks (`editable` the string `"nil"`: derived from others, as TSMC's `nf` from `fingers`) is not sent (`skipped:read-only param`): its callbacks derive it, and the file's value would be stale; without callbacks it is sent, being the only source, and each set parameter that has a callback is counted `callback not run`. Afterwards every set parameter is read back from a fresh instance CDF and differences from what was sent are logged (`clamped line N: inst param sent X now Y`), and every locked parameter the file held is read back and a difference from the file logged (`derived line N: inst param file X now Y`: a callback that did not derive it shows here), so the Rust side can show what the PDK changed. SKILL scope is dynamic: every procedure active while a callback runs (`SfyImport` down to `SfyEvalCallback`, `SfySetParams`, `SfyProbe`) binds only `sfy`-prefixed names (keyword parameters excepted), so a callback that sets an undeclared `w`, `p` or `inst` cannot reach them (`tests/skill_lint.rs`). This is the only `evalstring` in the file, and it evaluates PDK code, never file data.

### 5.4 Test interpreter and mock database

`plugins/interop/tests/skill/`: `mod.rs` (reader, evaluator, builtins, in-memory files) and `mockdb.rs` (a Virtuoso model: libraries, cells, views, text-view files, cellviews with instances, shapes, labels, terminals, pins, nets-free extraction, CDF with defaults and storeDefault semantics, instance CDFs whose `value` writes the instance property; maestro and OCEAN in `mock_mae.rs`/`mock_ocean.rs`). Each mock function checks its arity and argument types against `mockdb::SIGNATURES` (taken from the IC20.1 manuals) and fails like the documented function (signals on a bad argument, returns `nil` where Virtuoso returns `nil`); list attributes come back in reverse creation order, so a dump that forgets to sort fails its golden. A cellview opened `"w"` keeps a backup until `dbSave`, so a purge restores it (or removes a never-saved view). The mock is the oracle only for our *reading* of the manuals; every GUESS it encodes is in §13 and marked `GUESS(Gn)` in the code.

Tests (`tests/virtuoso_skill.rs`, `tests/skill_lint.rs`): every shipped line reads; lint (every called function known, arities against the signature tables, `sfy`-only globals, one `evalstring`); the SKILL escaper equals `sfyx::write_str` and `lineread` gives the string back (property, every byte but NUL, UTF-8), text chunks equal `sfyx::text`; hand-built mock cellviews (`tests/fixtures/skill/build.il`: an inverter with analogLib masters, a bus/iterated design, a symbol with every shape kind and label language, a cell with a net expression, a text-view cell, a PDK device with every CDF type + simInfo) dump to canonical `.sfyx` equal to committed goldens; each golden → loader → dumper equals itself (modulo `lib`); `SfySelfTest` passes; callbacks run in CDF order and failures are logged, not fatal; malformed and truncated files are refused with their line and nothing is saved; a schematic with check errors is not saved; `conn` stubs land at the real pins; the probe runs. A maestro setup and the dump → Rust `Design` comparison come with `sfy_ade.il` and `virtuoso::read`.

### 5.5 First contact with a real Virtuoso

`SfySelfTest` plus `skill/sfy_probe.il` (`(SfyProbe scratchLib @key pdkLib pdkCell)`, the research probe, flushed after each section: the release and the functions it lacks, the `errset.errset` shape, printing, orientations, `dbProduceMemName`, string splitting and order, the escaper round trip, analogLib globals and basic pin views, a reference symbol's anatomy, CDF attribute spellings, and in `scratchLib/sfy_probe/schematic` — opened `"w"`, purged, never saved — units, wires and solder dots, `schCreatePin`'s result, instance CDF storage, the open-mode spelling, a `netSet` property's type (G13), boolean properties, arcs, check bookkeeping; with `pdkLib`/`pdkCell`, that device's CDF (`editable`, `display`, callbacks) and `SfyCallbacks` on its `w`/`fingers`/`nf` with the properties before and after (G30); and a maestro view `scratchLib/sfy_probe/maestro` created, filled with every maestro call the scripts make, read back raw, saved and reopened read-only (G6, G18–G22, G28, G29)) write `/tmp/sfy_probe.txt`. Its output is the golden that replaces the mock's GUESSes (§13); `tests/virtuoso_probe.rs` checks the mock against a committed probe file when one exists.

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

Decided (import): `basic/ipin opin iopin noConn` and the global symbols are consumed by the objects they become and get no mirror file; `schemify_core` cells are the bundled core (`core:*`) and are never written; a derived interface puts inputs on the left (x = −40), outputs and inouts on the right (x = 40), 20 units apart, inside a box with `{name}`; impls are named `schematic`, `spice` (devices), `veriloga`, `verilog`, the first being `default` when there are several. With `ImportOptions::analoglib`, analogLib instances move onto their §6.5 core cells (§6.4 on the Schemify side), else they instantiate the analogLib mirror.

Export is the inverse: `.shcomp` → symbol + schematic + CDF; `.shprim` → symbol + CDF + simInfo (spectre and auCdl from its `format spectre`/`format cdl`, §6.3); `.shbench` → schematic (no symbol) + maestro; `core:*` cells → library `schemify_core` (symbols drawn as in Schemify, CDF from the core params, simInfo from §6.5) unless the mapping sends them elsewhere; cells whose provenance is `virtuoso <lib>/<cell>` are never exported (they *are* the Virtuoso cell) and instantiate `<lib>/<cell>/symbol`.

### 6.2 Objects

| Virtuoso object | Schemify record |
|---|---|
| instance (`inst`) | `inst`, name `I0<3:0>` ↔ `I0[3:0]`; CDF props → overrides (§3.4); prop `schemifyId` ↔ id; netSet props → `inherit <pin> <net>` (written as `netSet`-typed props; a netSet prop naming no inherited pin of the master overrides a net expression deeper down and is `needs_review: inherit`); prop `nlAction` `"ignore"` ↔ `ignore true` |
| wire (`wire`, wire/drawing) | one `wire` per segment; width > 0 ↔ `bus true`; a multi-point wire splits, the first segment keeping its id |
| wire label (wire/label, glued) | `label` at the point of its wire nearest the label origin (on a horizontal or vertical segment the projection; else the nearest end); its text through §3.3; an alias label is one of these too (Virtuoso stores it on wire/label, live ICADVM20.1); an `annotate drawing3` label is dropped |
| net-expression label `[@p:%:d!]` on a wire | the cell gains inherited pin `p` (`inherit d`, §3.3) and a label `p` there (elab §9.5) |
| schematic pin (`spin`) | `port` bound to the interface pin of that name, at the pin instance's origin; in a bench a `label` |
| schematic pin drawn as a bare pin shape (`pinfig` with `dir`) | `port` at the rectangle centre with attribute `vfig <w> <h>` (the rectangle's size; export draws such a port as the same shape, with its terminal, instead of a `basic` pin instance); in a bench a `label` |
| `basic/noConn` instance | `noconn` at its origin |
| note label (text/drawing) | `text`, size = height · 16 / upi (§3.5), orient by §3.2 |
| note shape (text/drawing, drawing1 = dashed) | `shape`; ellipse → `circle` when square, else a 32-point closed `poly` (reviewed) |
| symbol shape (device/drawing, drawing1 = filled) | rank-2 graphics; same ellipse rule (export: a `circle` box goes out rounded outward to whole DBUs, as Virtuoso stores it, live ICADVM20.1; the stash keeps an untouched circle); arcs with sweep > π are two Virtuoso arcs (export) / merged when they share centre and radius (import) |
| symbol label | `stext` by §3.5; pin-name labels (pin/label) are written by export and ignored by import (Schemify draws pin names) |
| symbol pin (`term` + `pinfig`) | `pin` at the rectangle centre; `dir` input/output/inputOutput ↔ in/out/inout (switch, jumper, unused → inout); several pins of one terminal → `alias` pins; term net expression → `inherit` |
| selection box (instance/drawing) | dropped on import; export writes the box around graphics, pins and labels |
| solder dots, markers, instance labels | dropped |
| cellview props `schemifyRecords1…N` | the Schemify-only records (§7.3) |

Port order: export writes simInfo `termOrder` (spectre and auCdl) = Schemify pin order; import takes `termOrder` of the cell's spectre simInfo, else Virtuoso's default (terminal names sorted ASCII).

Decided (exact forms, `virtuoso::write`/`read`):
- **Pins.** Each pin is a `pinfig` square of ±2000 export units (1/80″) around the pin point; an alias pin is a further `pinfig` of its target's terminal carrying prop `schemifyPin` (its Schemify name; a dump without it names extra rectangles `<term>_<k>`); the main pin is the first rectangle without `schemifyPin`. An inherited pin is a terminal with net expression `[@<pin>:%:<default>]` (default by §3.3 with `global`) plus a CDF `netSet` param of the pin's name; an instance's `inherit <pin> <net>` is a `netSet`-typed prop of that name (the type `dbReplaceProp … "netSet"` stores; G13). On import a default `x!` becomes `x` (inherited defaults are global by definition) and `gnd!` becomes `GND`.
- **Names.** Prop `schemifyNet` (labels and schematic pins) holds the Schemify spelling when the default inverse of the export spelling differs; its value is the net expression, followed by ` global` when the label carries `global true` (`VDD global` for the export spelling `VDD!`).
- **Wires** export width 5000 for `bus true`. **Wire labels** are `normal` labels on `wire label`, height 10000, `lowerLeft`, font `stick`. A label not glued to anything imports at its origin, reviewed.
- **Schematic pins** use `basic/ipin`, `opin`, `iopin` (`symbol` view) by the interface pin's direction (inout/unknown: `iopin`). **Noconns** are `basic/noConn` instances named `NC<k>` (the first number no instance uses); the pin is the origin.
- **Notes**: `text` → `normal` label on `text drawing`, `upperLeft`; `shape` → `text drawing`, or `text drawing1` when `dash` is non-zero (import: `dash 1`); `layer`, `fill`, `width` are Schemify-only.
- **Graphics.** Schemify arcs turn clockwise on screen (y down): the Virtuoso arc runs counter-clockwise from −(start+sweep) to −start, angles in micro-radians rounded; import rounds to milli-degrees and merges arcs that continue one another on the same box. A closed `poly` exports as a `polygon` without a repeated closing point; an open `poly` (and a `path`, whose width is lost) is a multi-point `line`. An ellipse or arc with a non-square box imports as a 32-point closed `poly`, reviewed. `fill true` ↔ `device drawing1`. The selection box is the bounding box of the shapes', pin rectangles' and labels' points.

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

Decided: the pin list of a generated format is `{t1} {t2} …` when every terminal is a template identifier, else `{pins}` (terminal names such as `NC+` are not identifiers; `pins` is the terminal order because import takes the pin order from `termOrder`). A string param of `instParameters` is exactly `{c != "" ? " k=" + c : ""}` (no space before the hole). "Numeric default" (import) means the default translates by §3.4, compiles, and names only params of the same cell. An auCdl `ansCdlCompPrim` with namePrefix `R C L D` and a `modelName` is `<P>{name} {pins} K={c}… $[<modelName>]` (no positional model). A `.shcomp` exports spectre simInfo `termOrder` + `instParameters` (its non-computed params) and auCdl `ansCdlSubcktCall` with the same and namePrefix `X`; a `.shprim` without a matching `format spectre` still exports spectre `termOrder`. A `schemify_core` source exports CDF params `srcType` (cyclic `dc pulse sine pwl exp`) and every waveform param of §6.6 (PWL as `tvpairs`, `t1…t8`, `v1…v8`/`i1…i8`) with defaults from parsing the core default, in place of `value`; spectre `propMapping` covers DC, AC and the pulse/sine/exp params (PWL has no simInfo mapping and is reviewed).

Export of a `.shprim` reverses the spectre rule when its `format spectre` matches that shape (`{name} ({pins…}) master k={p}…` with every hole a pin or param) and the cdl rule likewise; any other template exports the symbol and CDF only, `needs_review: format` (the cell then netlists in Virtuoso only through a text view the user adds).

### 6.4 Re-mastered instances

When the mapping (§8) sends an instance to a master with **different pin geometry** (analogLib ↔ core with `--analoglib`, a PDK map between libraries of different origin), connectivity cannot ride on geometry. `remap::apply`, for each such instance, using `nets::extract` (netlist.md §3 union-find, net names by §3.3; an unnamed net is named `net_<id>` exactly as elab would):
1. every wire endpoint on one of the instance's old pin points is pulled back one grid step along its wire (a wire shorter than a step is deleted) and a label with the pin's net is glued at the new end;
2. the instance gets the new master at the same origin and orientation; if the new master's pin geometry is known (it is in the Design), a stub + label per pin is drawn at its real pins; otherwise `("conn" inst term net)` records tell the loader to do it at the pins it finds in Virtuoso;
3. after loading, the loader reports every real pin that coincides with a pulled-back end of another net (`schCheck` would call it a short).
Geometry-identical masters (mirrors, `schemify_core`, the user's own cells) are never re-mastered.

Decided: a step is 10 units; a pin whose new position equals its old one keeps its connection by geometry (no pull-back, no stub); a point other objects still share after the pull-back (labels, ports, pins of instances keeping their master, a wire passing through) keeps a label of the net; a stub leaves the new pin away from the instance origin, turning to the first direction whose segment touches no existing wire or node. `remap::apply` returns the edits as a request table: the exporter applies it while writing records, `remap::apply_to_doc` on a Schemify document (import with `ImportOptions::analoglib`).

### 6.5 Built-in device table (analogLib ↔ core)

Used for the ngspice templates of analogLib mirror cells (import) and for the simInfo of `schemify_core` cells (export), and by analogLib re-mastering. Pins by order; values through §3.4.

Decided (2026-10-12): the command line and the editor command export core devices onto these analogLib masters by default (ADE users place analogLib; the `schemify_core` mirror stays available with `--core-cells`); `ExportOptions::analoglib` keeps its library default `false`. A re-mastering onto analogLib is not a report row (it is the default); a `map` row's is (`remapped`). A core param with no analogLib counterpart is reported (`remapped`) unless its override equals the core default or it only shapes the ngspice netlist (`savecurrent`: Spectre saves currents through the test, §9.6).

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

The ngspice templates of `vcvs`, `vccs` and `tline` use `{pins}` (their terminals `NC+`, `IN+`… are not template identifiers; terminal order is the pin order); `vpulse`/`vsin`/`vpwl`/`vexp` write `pulse({v1} {v2} {td} {tr} {tf} {pw} {per})`, `sin({vo} {va} {freq} {td} {theta})`, `pwl({t1} {v1} {t2} {v2})` (`ponytail:` two pairs), `exp({v1} {v2} {td1} {tau1} {td2} {tau2})`, i-sources likewise.

Spectre simInfo of `schemify_core` cells: res → `resistor` `r=`, cap → `capacitor` `c=`, ind → `inductor` `l=`, vsource/isource → `vsource`/`isource` with `type=` and the waveform params of §6.6, vcvs/vccs/cccs/ccvs → `vcvs gain=`, `vccs gm=`, `cccs gain= probe=`, `ccvs rm= probe=`, diode/nmos4/pmos4/npn/pnp → master `{model}`, ammeter → `iprobe`, tline → `tline z0= td=`; bsource → `bsource` with `v=`/`i=` from `src`/`func`; probe → nothing (a `.save` in SPICE; not netlisted to spectre, reviewed); switch/vswitch → `relay`-free: not netlisted (reviewed).

### 6.6 SPICE source text ↔ waveform params

`value` of `core:vsource`/`isource` is parsed as SPICE independent-source text: `[DC] v`, `AC mag [phase]`, and one of `PULSE(v1 v2 td tr tf pw per)`, `SIN(vo va freq td theta)`, `PWL(t1 v1 t2 v2 …)`, `EXP(v1 v2 td1 tau1 td2 tau2)` (case-insensitive, commas or spaces). Export picks the analogLib cell by waveform (`vdc` when none) and the Spectre `vsource type=` likewise; params: `vdc acm acp` + pulse `v1 v2 td tr tf pw per`, sine `vo va freq td theta`, pwl `tvpairs t1 v1 …`, exp `v1 v2 td1 tau1 td2 tau2` (i-sources: `idc i1 i2 io ia` names). Import formats the same text back (`DC 1 AC 1 0 PULSE(0 1.8 0 10p 10p 5n 10n)`). Every value is a number literal (optionally negative), one whole `{expr}` template hole, or a bare name (the hole of that one name: `VCM`, `dc VDD`): `dc {VGS} ac 1` → `vdc` `VGS`, `acm` `1`; `{VDD}` → `vdc` `VDD`; `dc {IDW*W/1u}` → `vdc` `IDW*W/1u` — the hole's expression by §3.4 in the instance's parent scope (bench vars bare: ADE design variables; a parent parameter `pPar("x")`), its review reasons reported. Text that does not parse — a hole without an AEL form, a hole glued to other text (`{a}u`), an empty or unclosed hole, any other word — exports as `vdc` with `vdc` = the whole text, `needs_review: source` (one row; never a CDF with part of the text). Import writes a value that is not a number literal back as a hole (`DC {VGS} AC 1`). Decided: a leading bare value is the DC value; a PWL keeps at most 8 pairs; keywords are written upper-case; a waveform's trailing unset params are omitted and an unset one inside is `0`; only an instance override is sent to `schemify_core` (D40), an analogLib master gets the parsed effective value.

## 7. Round trip and teamwork

### 7.1 Who owns what

Virtuoso owns, after an import: instances, their parameters, wires, labels, pins, symbols, CDF params (name, kind, default, choices, unit), bench variables, analyses, corners, outputs and specs. Schemify owns: docs (`prose`, cell `var`s), param attributes Virtuoso has no field for (`min max step grid show computed valid vderived`, `cached`), waivers, constraints, impl formats for dialects Virtuoso does not produce (`spice`, `xyce`, `verilog`), configs, sweeps, Monte Carlo and optimizer records beyond what §9 maps.

### 7.2 Ids

Every exported object carries prop `schemifyId` (instances, wires, labels, pins, shapes, notes); import reuses it when it is a valid, unused id of its kind. A teammate's new object has none and gets a deterministic import id (§2). A teammate's copy of an object duplicates its id: the second (in dump order) gets a fresh one.

### 7.3 Schemify-only data

Export writes the Schemify-only data of each cell as canonical record text into cellview props `schemifyRecords1…N` (strings ≤ 2000 bytes) on the schematic (benches: the schematic; devices: the symbol). Import rebuilds each target file from the dump, then takes the Schemify-only data from the **existing local file** when there is one, else from those props, and drops every piece whose subject is gone: a waiver whose instance id is absent, a param attribute whose param is gone, a format of an impl that no longer exists — each listed in the report (`dropped: stale-schemify-data`). A cell edited only in Virtuoso therefore keeps its docs and waivers, and a Schemify file is never silently overwritten: when the target exists and differs, the import writes it (the editor's watched-file merge, D28, shows the change) and the report lists the cell as `updated`.

Decided (`merge`): a document is cut into **units** — the header, the rank-1 block, the rank-2 block, and every other record by identity (kind and id or name). The export computes the *image* (the import of its own records) and stashes every unit of the cell the image does not reproduce, as canonical text of a document of the cell's kind (the rank-1 block is always present for kind rules; header attributes `vimg_header vimg_rank1 vimg_rank2` and record attribute `vimg` hold the FNV-1a 64 digest of the unit the import will produce; a unit without a digest has no Virtuoso form). On import, per unit: when the fresh unit's digest equals the stashed one, or — without a stash — it equals the local unit with its Schemify-only attributes stripped, the Virtuoso side did not change it and the local unit (else the stashed one) is written; otherwise the fresh unit is written, a rank-1 block enriched by name with the local pin `domain number`, param `min max step grid show computed cached valid vderived`, impl names, `model file ignore` and formats of dialects other than spectre/cdl (the local `format spice` wins, §8.1). Schemify-only records absent from the fresh document are kept; a waiver whose instance is gone, a constraint with a gone member and a body whose impl is gone are dropped with `dropped:stale-schemify-data`. A unit the stash holds without a digest had no Virtuoso form and stays Schemify's even in a bench whose maestro came back (an output §9 cannot translate, a sweep it does not export). Rank-4 order is semantic: the merge writes the units the base has in the base's order (fresh-only units after), and the export stashes every rank-4 unit when the image orders the shared ones differently (the import writes vars sorted). ponytail: without a stash, a Virtuoso-owned record the local file changed since the export cannot be told from a Virtuoso edit, and the dump's version wins (the D28 merge shows it).

### 7.4 Oracle

`import(export(x)) ≡ x` for every fixture project and generated designs (property: random cells with every record kind, buses, iterated instances, globals, inherited pins, params of every kind, benches): files byte-identical after `schemify fmt` except the documented losses of this spec (listed per fixture). With the stash (§7.3) the round trip is byte-identical; without it (`tests/virtuoso_rust.rs` strips the `schemifyRecords` props) the documented losses are: header attributes other than `provenance` (added to every `.shprim`); pin `domain`/`number`; param `min max step grid show computed cached valid vderived` and units outside §6.3; impl names and formats other than spectre/cdl; symbol graphic order, attributes and closing points, `stext` templates outside §3.5; text sizes; the order and spelling of instance overrides (suffix case, `mil`, `^`), the cellref spelling (`./c` vs `lib:c`), `ignore open|short`; ground spellings `0`/`gnd` (→ `GND`); shape `layer fill width`; ranks 4–6 beyond §9's mapping (§9.2 losses); comments. Wires, labels, ports, noconns, instance placement and names, pins and param names and kinds come back exactly. `export(import(d)) ≡ d` for every dump fixture, and through the interpreter + mock (§5.4) in both directions.

### 7.5 Untouched cellviews stay as Virtuoso drew them

Virtuoso keeps cosmetic fields Schemify has no slot for (label justification, pin-label heights, `cdsName()` versus `[@instanceName]`, wire drawing direction, note fonts). So that a round trip through Schemify never rewrites a cellview nobody edited there:
- **Import** writes header attributes `vsym <fp>` (shprim, shcomp) and `vsch <fp>` (shcomp, shbench): the fingerprint (`virtuoso::fingerprint`, FNV-1a 64) of what the *dump* gave that part, over the Virtuoso-owned fields only (param `min max step grid show computed cached valid vderived`, pin `domain number` and formats other than `spectre`/`cdl` are left out; rank-3 lines are sorted, rank 1–2 keep their order; within a record the attribute lines are sorted, so the order attributes were written in, which the format writer changes, never counts).
- **Export** recomputes the fingerprint of the current file; when it still equals the header attribute, the cellview's records carry `("prop" 0 "schemifyKeep" "string" "1")`.
- **The loader** leaves an existing cellview marked so exactly as it is (counted `kept unchanged`), and for a kept symbol also its CDF (`kept unchanged CDF`); the marker itself is never stored. A cellview edited in Schemify (any Virtuoso-owned field) has no marker and is regenerated; one edited in Virtuoso since the last import keeps the Virtuoso edit, since Schemify did not touch it.
- Two-point wires are written from their smaller end by both writers, so drawing direction never shows as a difference.
- Global labels whose Virtuoso name is an analogLib global symbol's (`gnd!`, `vdd!`, … `devices::GLOBAL_SYMBOLS`) and that sit on no wire, or that came from such a symbol, are written as that symbol (GUESS G3: its pin is its origin); import turns the symbol into a global label carrying attribute `vinst <instance name>` (an unknown attribute, kept by the format, §5 there), so export draws the same instance again; `vlib <library>` when that symbol is not analogLib's (`basic/gnd`), so export draws the same master.
- **What Virtuoso drew, kept for the way back.** Where export would draw a Virtuoso object differently from how the dump had it, import keeps the difference in an unknown attribute (the format keeps those, §5 there), so a library goes Virtuoso → Schemify → Virtuoso unchanged (the kit's `--lib` check): `vpin <lib> <cell> <view>` on a port whose pin master is not basic's `ipin`/`opin`/`iopin` for its direction; `vfig <w> <h>` on a port drawn as a bare pin shape (§6.1 table); `vexpr <text>` on a label that was a net expression; `vstyle <justify> <font> <height>` on a wire label not in export's style (lowerLeft, stick, `LABEL_HEIGHT`); `vlabel <layer> <purpose> <justify> <font> <kind>` on a `text` from any label that is not a wire label (e.g. a schematic pin's name on pin/label) or not in export's note style; on a symbol `stext`, `vlabel <layer> <purpose> <justify> <font> <kind> <text> <template>`, used only while the stext's template is still `<template>` (an edit in Schemify wins); `vplabel <dx> <dy> <orient> <justify> <font> <height>` (or `none`) on a pin whose name label differs from export's (at the pin, R0, centerLeft, stick, half `LABEL_HEIGHT`); `vpfig <w> <h>` on a pin whose main rectangle is not export's ±`PIN_HALF` square (Cadence's sample library draws ±4000); header `vibox <x0> <y0> <x1> <y1>` (or `none`), the symbol's selection box. **The CDF's reverse stash:** a `.shcomp` from Virtuoso carries header `vcdf <fp>` (the fingerprint of its pins, params and impls, the records export derives a CDF from) and one `vcdfrec "<record>"` per `cdf`/`param`/`siminfo` record of its dumped CDF (none: it had no CDF). While `vcdf` still matches, export sends those records back verbatim (so `instParameters` and the rest stay as Virtuoso had them); after an edit to the interface it derives the CDF as §6.3 says, adding its netlisting siminfo keys and a `netSet` param per inherited pin. A `.shprim` from Virtuoso goes back as a reference to the original cell (§8.1) and needs none.

Oracle (`tests/virtuoso_e2e.rs`): Rust export → SKILL load → SKILL dump → Rust import is byte-identical on every fixture project; SKILL dump → Rust import → Rust export → SKILL load over the original library → SKILL dump equals the first dump on every SKILL fixture; an edit in Schemify clears the marker of that cellview only and reaches the dump.

## 8. PDK flows and the mapping file

### 8.1 Two ways a PDK crosses

1. **Mirror (any PDK in Virtuoso, open or commercial).** `SfyExportLib` dumps the PDK library; `virtuoso-import` writes a Schemify library of the same name with one `.shprim` per device (symbol, CDF params as Schemify params, spectre and cdl formats, `provenance virtuoso <lib>/<cell>`). Designs place these cells; export references the real PDK cells by name: geometry, parameter names and netlisting are identical by construction. The ngspice side of a mirror is added by the user (a `format spice` on the mirror cell, kept across re-imports, §7.1) or by a mapping rule (§8.2). Mirrors of commercial PDKs contain the PDK's symbols and parameter names: keep that library out of public repositories (the report says so; `virtuoso-import` writes a `.gitignore`-ready note into the library directory, never a file outside it).
2. **Map (two libraries that model the same devices differently)**, e.g. the open sky130 library imported from xschem (`sky130_fd_pr:nfet_01v8`, params `W L nf mult`) and a Virtuoso sky130 kit. Rules in `interop.map` (§8.2) translate cell, pins and params; instances are re-mastered (§6.4).

**Callback-derived parameters.** A mirror carries a device's CDF params but not its CDF callbacks (SKILL, often encrypted): parameters a callback derives from what the user sets (a MOSFET can netlist its source/drain areas and perimeters `as ad ps pd` and layout-dependent-effect parameters, all derived from `w l nf` and layout options; live ICADVM20.1) keep their defaults when a Schemify instance is resized. `SfyCharacterize` (§8.3) measures them. Those are the ones Schemify needs Virtuoso's values for (the round trip with `?callbacks t` today).

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
- Lookups: an instance's cell has a `map` row → it; else the cell is a mirror (provenance) → itself; else `core:*` → `schemify_core` (or analogLib with `--analoglib`); else a user cell exported with the design. Errors (a pin of the cell left unmapped, an unknown cell) refuse the export with the list.
- Decided: the file is read by the format crate's reader behind a `journal` header (so quoting and attribute lines are exactly format.md §2); errors carry the file's own line. `Mapping::parse` refuses, all listed: a record other than `map`/`class`, wrong arity, a `lib:cell` that is not one, an attribute a record does not take, a `scale` that does not compile, `same` beside other attributes, a second `spice`, a pin or param mapped twice, a param both mapped and dropped, a Schemify cell mapped twice, two rows with the same Virtuoso target (import's reverse lookup must be unique), a class defined twice, `role` other than `n`/`p`. `map::resolve` checks only rows of cells the export uses: every non-alias pin of the cell has a `pin` row (unless `same`), every `pin`/`param` row names something the cell has. A scaled value folds to a number when it has no identifiers (`W 2` × `1e-6` → `0.000002`), else `(v)*(k)`; import divides likewise. On import `set` params are not taken and unmapped Virtuoso params keep their names.

### 8.3 Characterization (`SfyCharacterize`)

`(SfyCharacterize path lib cells scratch ?inputs ?span ?points)` measures what the CDF callbacks of each of `cells` (PDK library `lib`) derive from the parameters a user sets, and writes it to `path` (an `.sfyx` file, read by `virtuoso::characterize`; it names the PDK's parameters and values, so it stays on the machine it was made on). Nothing in it knows a device type or a parameter name. **Discovery:** the candidates are every CDF param with a callback and every spectre `instParameter` whose type is numeric, boolean or a choice; each is classed by measured type alone (`int` or a whole-number default: `count`; another numeric default: `num`; `boolean`: `bool`; `cyclic`/`radio`: `choice`) and nudged once from the defaults (twice a number, one more count, the other boolean, the next choice). The probes form an **influence graph**: an edge x → y when nudging x (callbacks all running) changes netlisted y. The **inputs** are the seeds — the CDF's `paramLabelSet` params (a leading `-` stripped: the PDK's own declared sizing parameters) with an edge — and every other candidate with an edge that no seed moves; a param a seed moves is derived even when it is editable with a callback (a source/drain area recomputed from the width), and seeds that move one another (a finger count raising the width) all stay inputs. Without a seed, the inputs are the graph's sources (candidates with an edge whose strongly connected component has no edge into it), at most 6, in CDF order, logged as `info`. The netlisted params that change are the **derived** ones. `?inputs` names the inputs instead. **Points:** each places a fresh instance on `scratch/sfy_char/schematic` (never saved), sets the point's inputs in input order with the callbacks running as `SfyImport ?callbacks t` runs them (`SfySetParams`), reads every netlisted param and every input back, and deletes the instance. **Sweep** (without `?points`): the defaults; then per input, the others at their defaults, a `num` scanned from a quarter of its default by steps of ×1.2, ending on `?span` (100) times it (from 1e-9 when the default is 0), a `count` at 1 2 3 4 8, a `bool` both ways, a `choice` at every value; along a `num` axis the **stepping** values are the netlisted params that change between at most a third of neighbouring scan points (one that changes almost everywhere varies continuously), and wherever two neighbours differ in a stepping value, in whether the input read back as set, or in status, the interval is bisected (a worklist, not recursion) until its width is 1e-5 of its upper end; then every pair of inputs at values off their defaults (a `num` at 4× and 10× the default, 1e-6 and 1e-5 when it is 0; a `count` at 2 and 4; the other boolean; every other choice), and 10 cross points at random scan values from a fixed-seed generator, so a sweep is reproducible. A device's sweep stops at 4000 points (an `info` line in the log). **Records:** `("char" lib cell (inputs…) (netlisted…) (kind per input) (default text per input))`, a `("probe" candidate kind set status (netlisted params it changed…))` per candidate (names only), then per point `("pt" (set value per input, "" at its default) status (value per netlisted param) (value read back per input))`, `status` `"ok"` or the reason a callback failed. `?points` (value lists in input order, `nil` for a default) measures only those points: the holdout. Writes `<path>.log`; `t` on PASS.

`schemify-interop pdk-rules <project-dir> <char.sfyx>` turns a characterization into rules on the project's mirror cells (`virtuoso::characterize::Device::rules`), by the role each input plays in the data, never by its name. Coordinates are the inputs as read back (what the device holds after its callbacks). Each derived parameter gets the first of these forms that fits every successful point: (1) one value; (2) a step table `steps(x, …)` over one `num` or `count` input (D12 as amended 2026-10-15); (3) a table (or one value) times a monomial of up to two other numeric inputs, each factor `n`, `1/n` and, for a count, `floor(n/2)+1`, `ceil(n/2)` and those over `n`; (4) an affine `m·(a·u + T(x))`, `m` and `u` such monomials over at most two inputs between them, `a` a constant; (5) one of (1)–(4) per value of a `bool` or `choice` input, as `if()` (true arm first; `x == "v"` per choice, the last value the else). Fitting: a value fits when it lies within the digits it was printed with — half a unit in its last printed digit, at least 1e-6 relative (`6.72f` is good to ±0.005f) — and each constant is the number with the fewest significant digits the tolerances allow (nearest the middle of what they allow); a breakpoint lies in the bracket between the last value measured on one step and the first on the next, and is the shortest number in it nearest the latter. A step must be located: its bracket within 1e-4 relative, consecutive counts, or two points set within 1e-4 of each other reading back on either side (a rewrite that skips the values between); a table with more than 64 breakpoints, or a step the sweep did not bisect (the value varies continuously), fits nothing. The table covers the range measured (`steps` is `X010` outside it). A rule is kept only when schemify-expr, evaluating its expression at every successful point, reproduces the value within its printed digits; kept rules become `computed <expr>`, and a derived parameter without one gets `vderived true` (its value is Virtuoso's and goes stale when the inputs change), the mirror's header `vinputs <input>…` naming what it depends on. **Cached derived values** (D20 as amended 2026-10-15): the import writes no override for a `computed` parameter, and writes each `vderived` one as an instance line `cached <param> <value> <digest>` — Virtuoso's value (a parameter Virtuoso did not store is at its default), canonical (expr §7, a string quoted), with the digest of the instance's input values (FNV-1a 64 of one `<name>=<canonical value>` line per `vinputs` name in order, the instance's override else the default, 16 lowercase hex digits; `characterize::inputs_digest`); an instance whose inputs are not constants gets no cached value. Elab uses the cached value; when the digest of the instance's current input values differs it is `E039` (stale: blocks Run, waivable), and without a cached value it is `E039` once an input is overridden. The export sends a cached value only while it is fresh: a stale one is left to Virtuoso's callbacks, and `SfyImport ?callbacks t` then the next import brings it back fresh. **Legality** comes from the rewrites alone. A value set is **kept** when it reads back equal within 1e-6 relative, or reprinted with six significant digits or more within half a unit of the last; otherwise the callbacks rewrote it. Per input with a rewrite, its points sorted by the value set (taken as it is, and over each `count` input's value: a floor per finger) split into runs of kept and rewritten values; a value set both ways has no rule. A rewritten run is a **clamp** when every rewrite gives one value (a floor or a ceiling: illegal), a **snap** when the rewrites are multiples of one grid (the smallest step between them) no more than half a step away (legal on the grid); a kept run between two runs snapped to its grid joins them; anything else has no pattern. The rule keeps the kept and snapped runs: `x >= c` and `x < c` between runs (`x <= c` below a ceiling), `c` the clamp value where one bounds the run, else the shortest number in the bracket nearest the kept side, and `abs(x/g - round(x/g)) <= 1e-6` on a grid; the reading with the fewest terms that holds exactly at every point wins (`zq/ct >= 4e-8` over `zq` alone). When no reading holds, one is tried per value of a `bool` or `choice` input (a value with no rewrite measured is `true`), as `if()`. The rule must be true exactly at the kept points, evaluated by schemify-expr at the values set; it becomes the input's `valid <expr>`, so a value the callbacks would rewrite is `E038` (D13 as amended 2026-10-15), and an input whose rewrites no rule explains is reported (`Rules::unruled`). `characterize::holdout_points` picks three points the characterization never measured (each `num` input at ×2.9, ×0.62, ×6.7 its default inside the range read back, each count at 5 6 7 up to the largest measured, booleans and choices cycling through the values measured) for `SfyCharacterize ?points`, and `characterize::check` evaluates each rule there with schemify-expr against the callbacks' values, and each legality rule against what they kept; the live kit grades it as `pdk.rules.check`. Every design instance of a characterized cell loses its overrides of the now-computed parameters (E030 otherwise). The live kit runs `SfyCharacterize` with `--pdk LIB[/CELL,…]` over the cells named plus every cell of `LIB` the user's Virtuoso cells (`SfyLiveUsedCells`, their schematics' instances) and Schemify projects (`inst … LIB:cell`) instantiate (`pdk.characterize`; per device, `pdk.derived` names the parameters that change with the inputs beyond the inputs themselves and `pdk.rules`, `pdk.holdout`, `pdk.rules.check` grade its rules).


## 9. ADE (maestro) ↔ bench

### 9.1 Export of a `.shbench`

One maestro view on the bench cell with one test, design = the bench schematic, simulator `spectre`. The test is named after the bench's **analysis block** — the first `code` block (by name) that is SPICE (`spice ngspice xyce`) with a line mapping to an analysis below, or Spectre whose every line but `//` comments has a maestro form (§9.2's Spectre form) and one is an analysis — else after the cell; Virtuoso-safe (`[A-Za-z0-9_]`, others `_`). Only the analysis block's lines map; analysis lines of other SPICE blocks are `needs_review: code`.

| Bench | Maestro |
|---|---|
| `var name expr` | `("var" name value "")`, value by §3.4 (bench vars bare) |
| `sweep <v>` of var `v` (named after it): `values a b c` / `lin a b n` / `log a b n` | the var's value becomes the comma list `"a,b,c"`: `values` entries by §3.4, `lin`/`log` points by run.md §2 rounded to 15 significant digits, then canonical number text (`lin 0 0.8 81` → `0,0.01,…,0.8`, never the binary noise `0.010000000000000002`; the import recognizes `lin`/`log` by the same grid). This list *is* the maestro parametric sweep (ADE Assembler sweeps a global variable whose value is a comma list); the OCEAN script (§9.6) writes the same points as a `paramAnalysis`. A sweep named otherwise, a second sweep of a var, a one-point sweep or an entry without a comma-free AEL form: `needs_review: sweep` |
| `corner name` with `lib path section`, `include path`, `set var expr`, `temp t` | `("corner" name 1)`, `("model" name path section)` (include: section `""`), `("var" var value name)`, `("var" "temperature" t name)`; `${NAME}` in paths → `$NAME` |
| `.tran tstep tstop [tstart [tmax]]` | `("analysis" "tran" 1 (("stop" tstop) ("start" tstart) ("maxstep" tmax)))` (tstep has no field) |
| `.ac dec\|lin n f1 f2` | `("analysis" "ac" 1 (("start" f1) ("stop" f2) ("incrType" "Logarithmic") ("stepTypeLog" "Points Per Decade") ("dec" n)))`; `lin` → `("incrType" "Linear") ("stepTypeLin" "Number of Steps") ("lin" n)`; `oct` is not mapped |
| `.dc <source> start stop step` | `("analysis" "dc" 1 (("sweep" "Component Parameter") ("dev" "/<source>") ("param" "dc") ("start" …) ("stop" …) ("step" …)))` |
| `.op` | `("analysis" "dc" 1 (("saveOppoint" "t")))`; with a `.dc` both share the one dc analysis, sweep options first |
| `.noise v(out) src dec\|lin n f1 f2` | `("analysis" "noise" 1 (("start" f1) ("stop" f2) <the five sweep options of .ac> ("oprobe" "/out") ("iprobe" "/src")))`; `v(out,ref)` is not mapped |
| `.temp t` | var `temperature` (global) |
| `.options errpreset=liberal\|moderate\|conservative` | option `("errpreset" p)` of the test's `tran` analysis (any line order); with no `.tran`, another option, or a second preset: `needs_review: code`. Import writes it back as that line before `.tran` |
| `.param k=v …` | var `k` (global); a `k` that is also a bench var is not mapped |
| `.include f` / `.lib f s` | `("model" "" f s)` (quotes dropped, `${NAME}` → `$NAME`) |
| any other line of the analysis block (device lines, `.ic`, `.save`, `.option`, `.meas`, a line with extra words such as `uic`, a value that is no number literal, a second analysis of a kind) | `needs_review: code` (stashed, §7.3) |
| other `code` blocks: lang `spectre` | `("defn" "<name>.scs" text)` (the lines joined by LF, a final LF) |
| other `code` blocks: SPICE, other langs | `needs_review: code` (stashed) |
| `output name expr` | `("output" name "point" ade)` by the outputs table below (`v(n)` → `VT("/n")`, `i(dev)` → `IT("/dev/<terminal>")`, bench vars → `VAR("x")`, `^` → `**`, …). Anything it does not cover: `needs_review: output` (stashed) |
| `spec out min a max b` | `("spec" out "gt" a)` and `("spec" out "lt" b)` (one record per bound present; bounds below). A spec of an output that does not map: `needs_review: spec` |
| `montecarlo`, `optimize`, `config` | stashed (§7.3), `needs_review: <kind>` |

Record order (the SKILL dumper writes the same): `maestro`; global `var`s by name; `analysis` by name; the test's `model`s in block order; per corner (by name) `corner`, its `model`s in attribute order and its `var`s by name; `output`s by name; `spec`s by (output, op); `defn`s by file. Several benches give several tests, each after the previous one's records.

Decided: ADE's `>`/`<` are taken for Schemify's inclusive `min`/`max` (an equality at the bound may judge differently). A bench var named `temperature` exports as the test temperature.

**Outputs in detail** (`ade::out_expr`; the same text is the maestro output and the OCEAN expression, OCEAN evaluating ADE's calculator functions). The source's spacing is kept outside rewritten calls. Signals: `v(a.b.n)` → `V<L>("/<path>/n")`, the instance prefix through the bench's own instances (a subcircuit's SPICE name `X<name>` is the instance `<name>`: `v(XDUT.n1)` → `VF("/DUT/n1")`); `i(dev)` → `I<L>("/<path>/<terminal>")` with the terminal the *exported* master's first pin (analogLib `PLUS`, a `schemify_core` cell's own `p`, a map row's mapping), `PLUS` for a name that is no instance; import reads `I<L>` of terminal `PLUS` or `p` back as `i()`. Letter `<L>`: the test's (tran `T`, else ac `F`, else a dc sweep `S`, else `DC`), or the output's attribute `analysis tran|ac|dc|op` (format.md rank 6) — one test can then hold the dc operating-point table and the ac outputs; an `analysis` the test does not run is `needs_review: output`. Functions:

| Schemify | ADE |
|---|---|
| `ymax ymin avg rms integ deriv abs sqrt exp ln log10` | `ymax ymin average rms integ deriv abs sqrt exp ln log10` |
| `re im mag ph db pp` | `real imag mag phaseDeg dB20 peakToPeak` |
| `phase_margin gain_margin` | `phaseMargin gainMargin` |
| `at(y, x0)` | `value(y x0)` |
| `x()` | `xval(<the expression's first signal>)` (none: not mapped) |
| `max(a, b, …)`, `min(…)` | `((a)+(b)±abs((a)-(b)))/2`, folded left (exact; the calculator's `max` is not element-wise) |
| `pow(a, b)`, `a^b` | `(a)**(b)`, `a**b` |
| `cross(y, l[, edge[, n]])` | `cross(y l n "rising"\|"falling"\|"either")` (edge `1` `-1` `0`, default either; `n` default 1; anything but those literals: not mapped) |
| `settle(y, tol)` | `settlingTime(y ymin(xval(y)) t ymax(xval(y)) t 100*(tol)/abs(value(y ymax(xval(y)))-value(y ymin(xval(y)))))` (GUESS G24: `settlingTime` returns the absolute x, percent of the whole step) |
| `op("M1", "gm")`, `op("/X1/M2", "id")` | `OP("/M1" "gm")`, `OP("/X1/M2" "id")` (below) |
| `pi` | its value |
| bench var `x` | `VAR("x")` |

`op(inst, param)` (expr.md: the device parameter at the job's DC operating point; `inst` `/`-separated schematic instance names, leading `/` optional; segments after the device name a device inside its PDK subcircuit): the longest prefix that names instances of the bench is written by its Virtuoso path, the rest as written (`op("XDUT/M1/main","gm")` → `OP("/DUT/M1/main" "gm")`). OCEAN uses the same `OP()` (GUESS G25: OCEAN's `OP` reads `dcOpInfo` after `run`/`paramRun`; the fallback is `pv("/M1" "gm" ?result 'dcOpInfo)`). The bench needs `.op` (a `saveOppoint` dc analysis). Not mapped (`needs_review: output`, never a partial output): strings elsewhere, comparisons, `?:`/`if` (a column condition has no calculator form), `delay settle` with other arities, `fft cross` with other arguments, functions not in the table, nesting deeper than 64 calls.

**Spec bounds**: a number by §3.4; an expression over bench vars none of which is swept is folded to its canonical number (`min A0_spec/(1+A0_spec)` → `0.975609756097561`; an ADE spec target is a value); one naming a swept var, or not evaluating, is `needs_review: spec` and that bound is not sent.

**Plots**: an `output y …` with attribute `vs x` (another output; format.md rank 6) is plotted by the OCEAN script as `waveVsWave(?x X ?y Y)` printed to `<cell>/<y>_vs_<x>.txt` — over a sweep, each output is a family of per-point values. A maestro output is one value per point, so the plot itself is OCEAN-only (`needs_review: plot`, one row per plot); `y` and `x` are maestro outputs as usual.

### 9.2 Import

The dumper reads the maestro setup (§9.4) and writes the records of §4.2; `virtuoso::read` refuses a malformed test with its line (`ade::check`: arity and element types, `enable` 0/1, options `(key value)` strings, a `var`/`model` scope naming a corner declared before it, a `spec` naming an output declared before it, op `lt`/`gt`, output kind `point`/`net`, a name twice in its kind, a definition file that is a path). `ade::import` then appends to the bench:

- analysis options: the form fields `maeGetAnalysis` answers though nothing set them — `anaName` equal to the analysis name, and `sweep "Frequency"` of an ac or noise — are dropped first; form-only options (`sweep incrType stepTypeLin stepTypeLog`) never reach a Spectre or OCEAN line.
- global vars → `var name value` by §3.4 (identifiers that are global vars are bench vars, others `needs_review: design-variable`); a comma list (commas outside parentheses) → `var` = its first point plus `sweep <name>` `var <name>` with `lin a b n` when the points are exactly run.md §2's `lin` of canonical first and last points, else `log a b n` likewise, else `values …`; ADE's `start:step:stop` form (`0.81:0.09:0.99`, three constants, the step landing on stop within 1e-9 of a step) is that lin sweep of its points, any other colon text an expression. Global var `temperature` (the test temperature, §9.4) → the analysis block's `.temp`.
- corners → `corner name`: its models in order (`include path` for section `""`, else `lib path section`; a section in double quotes loses them; `$NAME` → `${NAME}`), `set var value` per corner var, `temp t` last. A disabled corner is imported and reported (`needs_review: config`); a corner var holding a comma list is dropped (`needs_review: expression`).
- the **analysis block** `code <test name> <lang>`: when every enabled analysis has a SPICE form (options exactly those §9.1 writes for it, values numbers) it is `spice`: `.include f`/`.lib f s` per test model, `.temp t`, then per analysis in record order `.tran <stop/1000> stop [start [maxstep]]` (tstep canonical text; `start` `0` when only maxstep is set), `.ac dec|lin n f1 f2`, `.op` then `.dc src start stop step`, `.noise v(o) src dec|lin n f1 f2`. Otherwise it is `spectre`, one line per item in the same order: `include <file> [section=<s>]`, `temp options temp=<t>`, `<kind> <kind> k=v …` (a value quoted with `\"`/`\\` escapes when empty or holding white space, `"`, `\` or `=`); each analysis without a SPICE form is reported (`needs_review: code`). A disabled analysis is dropped (`needs_review: code`). No block when nothing goes in it.
- outputs: `point` outputs whose ADE expression translates back by the §9.1 table (`VT VF VS VDC` → `v`, `IT IF IS IDC` of terminal `PLUS` → `i`, `VAR("x")` → `x`, `**` → `^`) and compiles in calculator mode → `output`; others (and `net` outputs) become comment lines `* ade output <name> <kind>: <expr>` (`//` in a Spectre block) + `needs_review: output`. Specs of imported outputs → `spec out` with `min` (gt) and `max` (lt); others `needs_review: spec`.
- `defn` files → `code <stem> spectre` with the file's lines (`<stem>` = the name without `.scs`; a stem equal to the test name is dropped, `needs_review: code`).
- Only the first test of a view maps; the others are reported (`needs_review: config`).

The names are those the export carries — the analysis block after the test, a sweep after its var, a definition file after its block — so an unchanged maestro re-imports to the same record identities, and with the stash (§7.3) the bench comes back byte for byte.

Losses (documented, `tests/ade.rs`): `tstep` of `.tran` (rebuilt as stop/1000), spellings §3.4 normalizes, a swept var's own value (its first point), sweep names other than the var (not exported), the order of corner `set`s (by var name) and of the analysis block's lines; var attributes (`unit min max doc dist cached`), lines of the analysis block §9.1 does not map and outputs without an ADE form come back only from the stash or the local file (§7.3; when the Virtuoso side changes the analysis block, the rebuilt block wins and its unmapped lines are lost); a `.param` line comes back as a bench var (the stash also restores the line: such a bench is not byte-identical after a round trip).

### 9.3 Not mapped in v1

Config views (impl selection) ↔ bench `config`: stashed. Monte Carlo settings, optimization setups, run modes, simulator options other than `temp`, convergence aids, model groups, several tests per view: stashed or left to the user (report). Test-level variables are dumped as the test's own: each overrides the global of its name in that test's `var` records (§9.4), and comes back to Virtuoso as a global; when two tests of one view disagree, the import logs `warning line N: … both now see <later value>` and counts `maestro var conflict` (not a refusal). A test bound to a `config` view or to a cell other than its maestro's is dumped as bound to the maestro cell's `schematic` (G31).

### 9.4 SKILL side (`skill/sfy_ade.il`)

`(SfyAdeExport port lib cells)` dumps, per cell of the design closure with a `maestro` view (`ddGetObj`), the setup opened read-only (`maeOpenSetup lib cell "maestro" ?mode "r"`, closed by `maeCloseSession ?forceClose t` inside `errset` in an `unwindProtect` (a plain `maeCloseSession` only when that signals; a session left open keeps the maestro lock and the next open fails with ASSEMBLER-8127, so that is logged): per test (`maeGetSetup`, sorted) the `maestro` record (view `schematic`, simulator `spectre`), vars sorted by name — the globals (`maeGetSetup ?typeName "variables"`, `maeGetVar`), each overridden by the test's own of that name (`axlGetVars (axlGetTest sdb test)`, `axlGetVar`, `axlGetVarValue`), and the test temperature, simulator option `temp` (`maeGetSimOption test ?option "temp"`, G28), as var `temperature` — enabled analyses (`maeGetEnabledAnalysis`, `maeGetAnalysis` with every option value printed by `SfyAtomStr`), test model files (`maeGetEnvOption ?option "modelFiles"`), corners (`maeGetSetup ?typeName "corners"`; through `axlGetMainSetupDB`/`axlGetCorner`: `axlGetEnabled`, `axlGetModels`/`axlGetModel`/`axlGetModelFile`/`axlGetModelSection`, `axlGetVars`/`axlGetVar`/`axlGetVarValue`), outputs (`maeGetTestOutputs`: `~>name`, `~>expression` (else `~>expr`) → `point`, else `~>signal` (else `~>signalName`) → `net`; G29; an output without a name, which ADE allows for a saved or plotted signal, is skipped and logged, one answering neither field refused), specs (`axlGetSpecData`: `gt`, `lt`, `range` → both; a call that signals skips that output's specs, logged), definition files (`maeGetEnvOption ?option "definitionFiles"`, each read with `infile`/`gets`, named by its base name). Names are sorted with `alphalessp`; model order is the setup's.

`(SfyAdeImport records lib)` takes the (line record) pairs of every test. Per test: the target library (`sfyTargets` of the cell, else `lib`, else the record's); an existing maestro view is refused without `?overwrite` (a view this import already wrote for an earlier test is not "existing"), which replaces the test of the same name (`maeDeleteTest`); `maeOpenSetup … ?mode "a"`, `maeCreateTest ?lib ?cell ?view ?simulator`, then per record `maeSetVar` (global, or `?typeName "corner" ?typeValue (list corner)`; the global `temperature` is the test's simulator option instead, `maeSetSimOption name ?options (("temp" t))`), `maeSetAnalysis ?enable ?options` (options checked to be `(key value)` strings first; `("saveOppoint" "t")` is sent as the boolean `t`), `maeSetCorner ?enabled`, corner models through `axlPutModel` (named after the file, `_2`, `_3`… on repeats within a corner) + `axlSetModelFile` + `axlSetModelSection`, `maeAddOutput ?outputType "point" ?expr` / `"net" ?signalName`, definition files written beside the view (`ddGetObj lib cell "maestro" file nil "a"`, `ddGetObjWritePath`, `outfile`); then one `maeSetSpec` per output (`?gt`, `?lt`, or `?range (lo hi)` for both), one `maeSetEnvOption ?options` with `modelFiles` and `definitionFiles`; for a test without model files, `maeSetEnvOption ?options (("modelFiles" nil))`, because the export's model list is the test's and a session can give every new test default model files (a PDK's `libInit.il`; live run), and a failure there is logged rather than refused; `maeSaveSetup`, `maeCloseSession ?forceClose t`. Every maestro call runs inside `errset`, so a release that signals instead of answering costs that record, never the rest; every `nil` is logged as `refused line N: …` (§5.2); counts `maestro`, `maestro <tag>`, `maestro spec set`. A maestro view open in an ADE window is not detected (G6).

Signatures (rows of `tests/skill/mockdb.rs` `SIGNATURES`; source: the IC23.1 *ADE SKILL Reference* syntax lines as collected (MIT) in virtuoso-bridge-lite's function database, research §5.3 for usage): `maeOpenSetup maeCloseSession maeSaveSetup maeGetSetup maeCreateTest maeDeleteTest maeSetAnalysis maeGetEnabledAnalysis maeGetAnalysis maeSetVar maeGetVar maeSetCorner maeAddOutput maeGetTestOutputs maeSetSpec maeSetEnvOption maeGetEnvOption maeSetSimOption maeGetSimOption` (forum-verified usage for `maeOpenSetup maeCreateTest maeSetAnalysis maeSetVar maeAddOutput maeCloseSession`, LIKELY for the rest) and `axlGetMainSetupDB axlGetTest axlGetCorner axlGetEnabled axlGetModels axlGetModel axlPutModel axlGetModelFile axlGetModelSection axlSetModelFile axlSetModelSection axlGetVars axlGetVar axlGetVarValue axlGetSpecData` (syntax lines; the return shapes the reference leaves open are G18–G22).

### 9.5 Oracles

`tests/ade.rs`: (a) 32 generated benches (vars with lin/log/values sweeps, corners with `lib`/`include`/`set`/`temp`, every analysis kind, test models, `.temp`, `.param`, spectre blocks, outputs and specs, unmapped parts) export deterministically, report every unmapped part, import without the stash to exactly the §9 records, re-export to the same maestro records, and import with the stash (and onto the existing file) byte for byte; the §9.1 table record by record; (b) hand-written maestro dumps (`tests/fixtures/ade/*.sfyx`, the dumper's shape) import to golden benches and reports; (c) the Rust export of a bench → `SfyImport` on the mock maestro → the setup §9.1 says (tests, options, vars, corners, models, outputs, specs, env options, definition files) → `SfyExport` gives the same maestro records → the Rust import gives the bench back byte for byte (and the §9 records without the stash); `?overwrite` replaces the test; (d) malformed records are refused with their line on both sides, analyses without a SPICE form are reported and kept as Spectre, and that Spectre block exports back to the same analyses.

### 9.6 OCEAN script per bench

The export writes one script per maestro test, `ocean/<cell>.ocn` beside the `.sfyx`, named after the bench cell (tests of different benches may share a name; a cell name taken twice gets `_2`, …), and the test's definition files under `ocean/<cell>/`. Run headless after `SfyImport` of the same `.sfyx`: `cd ocean && ocean -nograph < <cell>.ocn`. Syntax from the *OCEAN Reference*; one call per line, in this order:

```
; dc_vds.ocn: OCEAN script of bench exercise1/dc_vds.shbench, written by schemify-interop (interop.md §9.6).
createDir("dc_vds")
simulator('spectre)
design("schemify" "dc_vds" "schematic")
resultsDir("dc_vds/psf")
modelFile('("/pdk/models/spectre/toplevel.scs" "tt_lib"))
desVar("L" "Lmin*pow(2,p)")
desVar("Lmin" 20n)
desVar("p" 0)
analysis('dc ?dev "/VD" ?param "dc" ?start "0" ?stop "0.8" ?step "1m")
save('i "/VMDN/PLUS" "/VMDP/PLUS")
paramAnalysis("p" ?values '(0 1 2 3 4))
paramRun()
ocnPrint(?output "dc_vds/ro_n.txt" ?numberNotation 'scientific ?precision 8 1/value(deriv(IS("/VMDN/PLUS")) VAR("VDS")))
exit()
```

- `design` names the maestro test's library, cell and view; `modelFile` the test's model files and those of the bench's default corner (header `corner`, else the first); one corner per script (a bench with several: `needs_review: corner`, maestro has them all). `definitionFile` per definition file.
- `desVar` per global var and default-corner `set`, by name: a SKILL number unquoted, an expression as a string; a swept var starts at its first point. The temperature is `temp(t)`.
- `analysis('<name> ?<key> "<value>" …)` per enabled analysis by name, the maestro options without the ADE form's choosers (`sweep incrType stepTypeLin stepTypeLog`; OCEAN infers them), `saveOppoint` as `?saveOppoint t` (GUESS G26: OCEAN takes the maestro option names and string values).
- `save('v …)` / `save('i …)` of every signal the outputs read.
- Swept vars (§9.1 sweeps, by name): nested `paramAnalysis("<var>" ?values '(<points>) …)` then `paramRun()`; else `run()`. A point that is not a number is left out (`needs_review: sweep`).
- One `ocnPrint` table per output (`<cell>/<output>.txt`; over a sweep a table per swept point), one per plot (§9.1), then `exit()`.

Oracle (`tests/exercise1.rs`): the four Exercise 1 benches export to goldens (`tests/fixtures/exercise1/ocean`, `SCHEMIFY_BLESS=1`), every line is a comment or one of the calls above with balanced parentheses and quotes; their maestro records carry every output and spec, the decimal sweep grid and the bench's own models; the report has no `needs_review: output spec source sweep` row.

## 10. gm/Id retarget (removed)

Removed 2026-10-15 (D41 amendment): PDKs cross by translation (§8.1), not by gm/Id intent solved in Virtuoso. The section number is kept so references to §11–§13 stay valid.

## 11. xschem export

Schemify → xschem `.sch`/`.sym` (file format from xschem-feature-inventory §5, checked by running xschem 3.4.7). `interop::xschem_out::export(design, tops) → files + notes` (pure, byte-deterministic); refused (one line each) for no tops, an unknown index, or a `core` cell as a top.

**Layout.** Each written cell becomes `<lib>/<cell>.sym` and/or `<lib>/<cell>.sch` (project-root cells: `root/`, `root_2/`, … while a library has that name; `core` cells without an xschem device: `schemify_core/`). Own cells are referenced as `<lib>/<cell>.sym`, xschem devices by bare name (`res.sym`, on xschem's default path). `xschemrc` (a fragment for `--rcfile` or sourcing) appends the output directory to `XSCHEM_LIBRARY_PATH` and lists the libraries in comments. A non-top cell with header `provenance xschem <lib>/<file>` is not written: instances reference `<lib>/<file>` (`devices/<file>` → `<file>`).

| Schemify | xschem |
|---|---|
| `.shcomp` (default impl `schematic`) | `<cell>.sym` with `type=subcircuit`, `format="@name @pinlist [@<inherited pin>…] @symname k=@k…"` (every overridable param passed by name; the importer strips the pairs again), plus `<cell>.sch` |
| `.shprim`, or a `.shcomp` whose default impl is a leaf | `type=primitive`; `format`/`lvs_format`/`spectre_format`/`verilog_format` from `format spice`/`cdl`/`spectre`/`verilog` of the default impl (templates below; `xyce`, a non-default impl, `file` → reviewed); a `spice`/`schematic` impl `body` → `device_model`; impl `ignore` → `spice_ignore`; a leaf with no translatable spice format gets none (xschem does not netlist it), reviewed |
| symbol `template` | `name=<header prefix, else x (subcircuit) / U>1`, every overridable param default (values below), and `<pin>=<net>` per inherited pin, whose names also go to `extra=` (import.md §2 implicit pins, inverted) |
| `pin` | `B 5 x±2.5 y±2.5 {name=… dir=…}` in pin order; an `alias` pin takes its target's name (xschem shorts same-named pins); inherited pins get no box. `domain`, `number` and the param attributes `unit min max step grid choices show doc cached` have no field: reviewed (`param-attr`) |
| rank-2 graphics, rank-3 `shape` | `line` → `L`, `rect` → `B` (`fill=false` unless `fill true`), `arc` → `A` (same angles), `circle` → `A … 0 360`, `poly` → `P` (`closed` repeats the first point); `layer` kept (default 4; a symbol graphic on layer 5, xschem's pin layer, moves to 4, reviewed), `dash` kept, `width` reviewed. A symbol with no graphics gets a box around its pins and `T {@symname}` |
| `stext` | `T` with `size` as both scales, `layer`, `hcenter`/`vcenter`; template by §3.5 (other holes dropped from the text, reviewed) |
| `core:*` | `devices/<sym>.sym` by inverting `DEVICE_MAP` (first row per core cell: `cap` → `capa.sym`); **every** param written (xschem's device defaults differ: `nmos4` w=5u against core 1u), names through the row's param renames inverted (`src` → `VAR`). `core:gnd`/`core:vdd` → `gnd.sym`/`vdd.sym` with `lab`. Other core cells (`switch`) are written to `schemify_core/` like any `.shprim` |
| `inst` | `C {ref} x y rot flip {name=… overrides}`, `(rot, flip)` = the inverse of import.md §3.1's table; only overrides (D40) for own and xschem cells; `ignore` → `spice_ignore`; `inherit <pin> <net>` → `<pin>=<net>`. A call through Schemify's default template (`X{name} {pins} {cell}`, netlist §7.6.5) whose name lacks the `X` is written `X<name>` (xschem prints names as written), reviewed (`name`) |
| `wire` | `N x1 y1 x2 y2 {}` (`bus=true` for buses) |
| `label` | `lab_pin.sym` at the point with `lab=<netexpr>` (same bus syntax); `global=true` for `global true` and names ending in `!` |
| `port` | `ipin.sym`/`opin.sym`/`iopin.sym` by the interface pin's dir, `lab=` the pin name (an alias pin's target) |
| `noconn`, `text` | `noconn.sym`; `T {text} x y rot flip s s {}` (`size`, default 0.4) |
| `code` (lang `spice`/`ngspice`) | `code_shown.sym` (`name=<code name> only_toplevel=false value="<text>"`, `$$` → `$`) in a column under the drawing; in a `.shcomp` xschem emits it inside the `.subckt` (netlist §7.6.15). Other langs and text using `${NAME}` → reviewed. A `.shcomp`'s schematic-impl `body` → a `body` block |
| `.shbench` | `<cell>.sch` only; vars → one `params` block of `.param k=v` lines in file order (the default corner's `set`s applied; a var calling a plugin function by its `cached` value, reviewed); the default corner (header `corner`) → a `corner` block (`.lib path section`, `.include path`, `.temp t`); other corners, `sweep`, `montecarlo` (and var `dist`), `optimize`, `output`, `spec`, `config` → reviewed |
| `prose`, header `doc`/`category`/`config`, `constraint`, `waive` | reviewed (`docs`, `config`, `constraint`, `waive`) |

**Templates** (import.md §4 inverted): `{name}` → `@name`, `{cell}` → `@symname`, `{pins}` → `@pinlist` then ` @<p>` per inherited pin, a pin → `@@<pin>` (alias pins by target), an inherited pin or a param → `@<p>`, `{model}` with an impl `model` attribute → that name, the `savecurrent ? …` idiom → `@savecurrent`. Literal `@ % \` are escaped with `\`, and a literal right after a token is `\`-escaped too, since only white space or the next `@`/`%` ends a token (`v({p})` → `v(@@p\)`). `{params}`, `{file}`, unknown names, strings and non-SPICE functions → the format is reviewed and omitted.

**Values** (refining §3.4's xschem paragraph): number literals as written; other number expressions as SPICE `'…'` (`^` → `**`, functions `abs sqrt exp ln log10 sin cos tan asin acos atan sinh cosh tanh floor ceil int min max pow`), parent params and bench vars verbatim. A component's vars and computed params have no xschem place: they are inlined in parentheses (a plugin-call var by its `cached` value, reviewed `expression`). **Raw parameters:** xschem pastes values into formats as text, so `W='wn*2'` meets `{W}u` as `W='wn*2'u`. A param that a format uses as a number — a hole followed by a glued scale suffix (`f p n u m k meg g t mil`) or read inside an arithmetic hole — is written in every format as `'(@W)'` (a glued suffix folded in: `'(@W)*1u'`; arithmetic holes become SPICE expressions over `(@p)`, computed params inlined), and its values (template default and overrides) unquoted (`W=wn*2`). `bool` → `true`/`false`; `string`/`choice` templates → literal text with each hole naming one identifier written as that bare name (`'{WP}*2'` → `'WP*2'`). Anything else (`if`, `?:`, comparisons, strings, plugin calls without a cache) → reviewed, the attribute is not written (the symbol default applies).

**Losses** (beyond the reviewed rows): the importer drops schematic graphics (import.md §3), so they do not survive a round trip; inlined vars come back as numbers; a param default that references another param lands on xschem's `.subckt` line as written.

Report reasons of this export: `needs_review:` `expression template format name param-attr layer shape code corner docs config constraint waive sweep montecarlo optimize output spec header`.

**Oracles** (`plugins/interop/tests/xschem_export.rs`): (1) round trip — for the app fixtures `rc`, `inv`, `ota`, the elab fixtures `inv_tb`, `bus_tb`, `opamp_tb` and generated benches of core devices (one with all ten in all eight orientations, seven random), `translate_library(export(x))` netlists like `x` under spicecmp (search roots: the export and xschem's system `devices`, else hand-written interface stand-ins in `tests/fixtures/xschem_export/devices`); (2) real xschem — `xschem -q -x -n -s -r --rcfile <out>/xschemrc --netlist_path <dir> <top>.sch` on the export equals Schemify's own ngspice netlist of `x` under spicecmp (skipped without an `xschem` on PATH unless `SCHEMIFY_REQUIRE_XSCHEM=1`; 14 of 14 agree with xschem 3.4.7); (3) untranslatable values, templates and bench records give notes and write no attribute; (4) byte determinism.

## 12. Report

`interop-report.md` + `.txt` beside the outputs, like import.md §7: classes `needs_review:<reason>` (`expression template name callback design-variable inlined-var computed format cdl-approximate source code output spec sweep plot corner config montecarlo optimize inherit`), `dropped:<reason>` (`display-only selection-box solder-dot stale-schemify-data pin-label unknown-record`), `rounding`, `off-grid` (export: one row per cell with the count and the first point), `remapped` (per instance re-mastered by a `map` row: old and new master), `clamped` (from the loader log), `updated` (cells changed by an import). Count conservation per cellview: objects in = records out + reported. Decided: every record of a cellview is consumed by exactly one builder or reported (dropped kinds as one row per cellview and reason with the count, unknown tags one row each with their line), and the import asserts the equality; records outside any cellview with an unknown tag are reported likewise; `conn` and `intent` records are reported on import (loader-only, §10).

## 13. GUESSes and their probes

| # | Assumption | Settled by |
|---|---|---|
| G1 | `MYR90`/`MXR90` point maps (§3.2); virtuoso-bridge-lite's live-checked `exact_geometry.py` uses the same maps | probe `orient` lines |
| G2 | schematic/symbol `DBUPerUU` 160, user unit inch | probe `units` |
| G3 | analogLib `gnd`/`vdd` terminal names `gnd!`/`vdd!`; `basic/iopin` view `symbol` | probe `symshape` of each |
| G4 | setting a CDF value equal to its default removes nothing (we never set it, §5.3) | — |
| G5 | `schCreateWire "draw"` adds no segments and no solder-dot figures we would dump | self-test round trip |
| G6 | maestro read in a background session (`maeOpenSetup ?mode "r"`) works without a window | self-test on a bench |
| G7 | auCdl formatting of devices (`$[model]`, case of `W L M`) | compare with a real CDL netlist. Settled for analogLib `res`/`cap` (live ICADVM20.1): `RR1 in out R $[RP]`, `CC1 out gnd C $[CP]` (the CDF `namePrefix` before the instance name, no `m` at 1) under `.SUBCKT rcfilter gnd in out R=1K C=1u`. Schemify's core devices keep xschem's LVS form without `$[model]` (the corpus ratchet), so the kit compares without the model names; the `.SUBCKT` parameter defaults are now written (netlist §7.6.10). Still open for MOS (`W L M` case) |
| G8 | Spectre port order default = sorted terminal names | compare with a real netlist |
| G9 | `%d` of `round(…)` and our escaper round-trip every byte | self-test |
| G10 | `dbCreateArc` + setting `startAngle`/`stopAngle` gives the arc | self-test; probe `arc` |
| G11 | `schCreatePin` returns the pin instance (the loader also accepts a pin object and takes its `fig`); pin instances do not take names the dump's instances use (instances are created first) | probe `schCreatePin returns` |
| G12 | `alphalessp` (the dump's sort) compares bytes, like C `strcmp` | probe `string alphalessp` |
| G13 | an inherited-connection override is a property of type `netSet` (virtuoso-bridge-lite sets it with `dbReplaceProp … "netSet"`; was: a `string` property); one set through a netSet CDF parameter is stored the same way | probe `netSet` |
| G14 | `schCheck` stamps bookkeeping properties on the cellview (`lastSchematicExtraction`, …); the dumper ignores every cellview property not named `schemify*` either way | probe `cvprop` |
| G15 | a solder dot is a `dot` (or `ellipse`) on `wire drawing`; the dumper skips every non-line figure there | probe `solder` |
| G16 | CDF flags read back as `"yes"`/`t`, an unset `display` as `nil`, `paramType` as the type string | probe `cdfparam` |
| G17 | `parseString s ""` splits characters or bytes and `charToInt` of a high byte may be negative; the chunker handles all four cases | probe `string parseString-utf8`, `charToInt-high` |
| G18 | `maeGetSetup ?typeName "variables"` lists the global variable names of a background session | self-test on a bench with a maestro view |
| G19 | `axlGetModels` returns the corner's model *names* in setup order; `axlGetModelSection` returns the section without the sdb's quotes (the import strips one pair either way) | self-test on a corner with two models |
| G20 | `maeSetAnalysis ?options` takes every value as a string except `saveOppoint`, which takes `t` (bridge docs: `(("saveOppoint" t))`); `maeGetAnalysis` returns what was set plus `anaName` first and, for ac, `sweep "Frequency"` (bridge docs), which the import drops | self-test; compare `maeGetAnalysis` after the load |
| G21 | `axlGetSpecData` returns `(op value)` pairs (`gt`, `lt`, `range` with `(lo hi)`); `maeSetSpec ?range` takes `(lo hi)`; one spec per output | self-test on an output with both bounds |
| G22 | the `definitionFiles` environment option is a list of file paths (`maeSetEnvOption`/`maeGetEnvOption`) | self-test with a spectre code block |
| G23 | an NLP label evaluates several `[@…]` groups in one text | place a core `nmos4` symbol and read its label |
| G24 | `settlingTime(y x0 t x1 t pct)` returns the absolute x at which `y` stays within `pct`% of the step from y(x0) to y(x1) | a tran with a known exponential settle |
| G25 | OCEAN's `OP("/inst" "param")` reads the dc operating point after `run`/`paramRun` (else `pv(… ?result 'dcOpInfo)`) | the Exercise 2 `ota_op` script |
| G26 | OCEAN `analysis` takes the maestro option names with string values (`?stop "1u"`), `?saveOppoint t`, `?errpreset "conservative"` | the Exercise 1/2 scripts |
| G27 | `desVar("L" "Lmin*pow(2,p)")` (a string) is re-evaluated per `paramAnalysis` point | `dc_vgs.ocn` against its maestro run |
| G28 | `maeGetSimOption test ?option "temp"` answers the option's value as `maeGetEnvOption` does; `maeSetSimOption test ?options (("temp" "27"))` sets it (bridge: `maeSetSimOption` with `(("temp" "85"))`) | probe `maestro` on a bench |
| G29 | an IC23.1 `sevOutputStruct` answers `~>expression`/`~>signal` (bridge's live probe) where the reference names `~>expr`/`~>signalName`; the dumper tries both | probe `maestro` |
| G30 | `cdfUpdateInstParam inst` copies the in-memory cell CDF's values (as left by the callbacks) to the instance and runs `doneProc` (bridge's live test: a finger count reaches the netlist this way) | self-test with `?callbacks t` on a PDK MOS |
| G31 | a maestro test's design binding (a `config` view, another cell) has no getter the dumper uses: it dumps the maestro cell's `schematic` | — |

## 14. Amendments

- 2026-10-12 (Cadence export gaps, `feat/interop-ade`): source values with `{expr}` holes and bare names (§6.6); sweep points on the decimal grid (§9.1); the outputs table (`re im mag db at x() max min cross settle phase_margin op()`, per-output `analysis`, `vs` plots), folded spec bounds and `.options errpreset` (§9.1); OCEAN scripts (§9.6); `--target` corner models, `param m`, `fingerw_min`, `split_above` (§10.2–10.3); fixed-size placeholders (§10.5); analogLib masters by default (§6.5); multi-hole symbol labels (§3.5); one `off-grid` row per cell (§12).
- 2026-10-15 (pin-pattern alignment, §6.4): a re-mastered instance whose target geometry is known (a mirror in the project, else analogLib's measured pin table `devices::alib_pins`) is placed where the target's pins land on the drawn ones, in the instance's own orientation (`align::fit`: every paired pin, or the most when at least two land); pins that still miss get stubs; unknown geometry keeps the `conn` records. Import aligns the Schemify cell onto the Virtuoso pins the same way. Two-terminal devices and MOS/BJT line up with analogLib (origin offsets (0,-30) and (-20,0)); `tline`, `iprobe`, `parax_cap` and the control pins of `vcvs`/`vccs` keep stubs. The loader draws no stub where a wire already ends on the real pin. The live kit checks the table against the running analogLib (`analoglib.table.*`).
- 2026-10-15 (first live run, ICADVM20.1, `plugins/interop/live`): G5 contradicted: `schCreateWire "draw"` splits a wire where another wire's end T-joins it, the new piece without the id or with a copy; import folds such pieces back into the collinear wire that owns the id (`rejoin_split_wires`), the mock splits the same way and the SKILL goldens hold the split form. G20 refined: `maeGetAnalysis` answers every form field with its default (`trannoisemethod`, `finalTimeOp t`, …); import drops the ICADVM20.1 defaults (`ade::FORM_DEFAULTS`) and the loader sends `"t"`/`"nil"` option values as booleans (a string fails ADE-5019). G29 refined: `~>expression` answers the parsed expression (a list); the dumper prints it. A read-only maestro session answers setup database 0; the dumper reopens the view in `"a"` mode (never saved). Virtuoso refuses CDF `units` on a `string` parameter without `parseAsNumber`: export sends none. Virtuoso's `append` takes two lists; its reader overflows on `-2147483648`.
- 2026-10-15 (virtuoso-bridge-lite audit, `fix/interop-bridge-audit`): `netSet` props (§4.2, §6.2; G13 contradicted); ref cells dump their effective CDF and an unbound master is refused, not fatal (§5.2); a target open for editing is refused, masters are closed after an import, SKILL errors are caught per cell, cellview, maestro part and export (§5.2); CDF callbacks run as the form runs them — cell CDF as `cdfgData`, values first, `cdfUpdateInstParam` — and locked params are left to them (§5.3, G30); maestro: `anaName`/`sweep` echoes dropped, form-only options kept out of Spectre lines, `start:step:stop` sweeps, test-level vars, the test temperature as simulator option `temp`, `saveOppoint` as `t`, `?forceClose t`, several tests per view, every call in `errset` (§9.2–9.4, G20, G28, G29, G31); retarget: `width` required, no `nf` in the characterization deck, callbacks on by default, `%.12g` numbers, locked params refused (§10.2–10.3).
