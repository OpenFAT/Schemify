# File format v1 (normative)

Implements D9, D10, D11, D13, D17, D19, D20, D29, D30, D33, D35, D40. Rationale and measurements: [../research/file-format.md](../research/file-format.md). Where this spec and the research doc differ, this spec wins.

## 1. Files

| File | Header kind | Purpose |
|---|---|---|
| `<cell>.shprim` | `shprim` | symbol + ≥1 leaf impl (device binding, Verilog-A, Verilog, SPICE text) |
| `<cell>.shcomp` | `shcomp` | symbol + exactly one schematic impl + 0..N leaf impls + docs |
| `<cell>.shbench` | `shbench` | schematic + docs + bench tables; no symbol |
| `schemify.project` | `project` | project manifest: libraries, plugins, settings |
| `schemify.lock` | `lock` | resolved sources + content hashes (written by the tool only) |
| `config` (user, `$XDG_CONFIG_HOME/schemify/config`) | `config` | settings, keymap, simulators, plugin order |

A library is a directory; one cell per file; the cell name is the file stem. Two files with the same stem and different kinds in one library is an error (`E020`).

`ponytail:` v1 allows exactly one schematic impl per `.shcomp`. Multiple schematic variants would need impl-scoped rank-3 records; add when a user needs it.

## 2. Lexical layer

```abnf
file      = header *line
header    = "schemify" WS version WS kind EOL          ; line 1, always
version   = 1*DIGIT                                     ; this spec: 1
kind      = "shprim" / "shcomp" / "shbench" / "project" / "lock" / "config"
line      = blank / comment / record / attr / verbatim
blank     = *WSP EOL
comment   = *WSP "#" *CHAR EOL                          ; whole-line only; attaches to the next record
record    = word *(WS value) *WSP EOL                   ; starts in column 0
attr      = 1*WSP word *(WS value) *WSP EOL             ; indented: belongs to the previous record
verbatim  = 1*WSP "|" [SP *CHAR] EOL                    ; raw text line, content after "| " (or after "|" if line is exactly "|")
word      = ALPHA *(ALPHA / DIGIT / "_" / "-" / "." / ":")
value     = bare / quoted
bare      = bstart *bchar                                ; bchar: any char except WSP, CR, LF, DQUOTE, "\", C0 controls, DEL
bstart    = bchar - ("#" / "|")
quoted    = DQUOTE *(qchar / escape) DQUOTE              ; qchar: any char except DQUOTE, "\", CR, LF
escape    = "\" (DQUOTE / "\" / "n" / "t" / "u{" 1*6HEXDIG "}")
WS        = 1*WSP
EOL       = LF / CRLF
```

- Encoding UTF-8. A BOM or invalid UTF-8 is an error (`E028`). The reader accepts CRLF and a missing final newline.
- Header: `version` is a positive integer below 65536 (written without leading zeros); a larger version than the reader's enables §5 forward compatibility. Anything malformed on line 1 is `E028`.
- **Writer canonical form:** LF only, final newline, single space separators, attributes indented by exactly two spaces, verbatim lines as `  | text` (or `  |` for an empty line), no trailing spaces outside verbatim text (verbatim text is kept exactly), one blank line between consecutive records with different words and before end-of-file comments, coordinates as the shortest decimal (§3), quoting only when a value is not a legal `bare`. Inside quotes the writer escapes `"` `\` LF TAB as `\" \\ \n \t`, and CR, other C0 controls and DEL as `\u{hex}` (lowercase, no leading zeros); everything else is written raw. Attributes are written in model order, known attributes of the record kind before unknown ones (§5). The same model always produces the same bytes.
- Comments attach to the next record and are written back immediately before it (indentation and trailing whitespace dropped). A comment between attribute lines also attaches to the next record. Comments at end of file attach to EOF. The header never carries comments.
- Line length is unbounded; a file is capped at 2^30 bytes (`E021`).
- Diagnostics carry a 1-based line and a 1-based byte column (0 when the position is the whole line or an unescaped value).

## 3. Numbers, coordinates, orientation

- **Number literal (file level):** `-?digits(.digits)?` with no exponent and no SI suffix. Anything else in a numeric position is an error. (SI suffixes belong to expressions, §6.)
- **Coordinates** are written in xschem units, y pointing down, with at most 3 fractional digits. In memory they are `i32` milli-units (`value × 1000`, exact). A coordinate needing more digits or outside `[-2^31, 2^31-1]` milli-units is an error (`E022`); further fractional digits that are all zero are exact and accepted (`1.5000`). Writer: shortest decimal (`-60`, `12.5`, `0.156`); the reader already rewrites coordinates to that form, so `-80.000` and `-80` are the same model (and never a merge change). A non-coordinate in a coordinate slot is `E029`.
- **Orientation:** `R0 R90 R180 R270 MR0 MR90 MR180 MR270`. Transform of a symbol-local point `(x, y)` placed at `(X, Y)`:
  1. if `M`: `(x, y) ← (−x, y)`
  2. rotate clockwise on screen (y down) by k·90°: `R90: (x, y) → (−y, x)`, `R180: (−x, −y)`, `R270: (y, −x)`
  3. translate: `(x + X, y + Y)`

## 4. Sections and records

Records appear in rank order; a record of lower rank after a higher rank is an error (`E023`). Within rank 3 the writer emits kinds in the order listed and each kind sorted by id (bytewise); the reader accepts any order within rank 3. Ranks 1, 2, 4, 5 keep file order (it is semantic). Rank 6 is written with kinds in the order listed, each sorted bytewise by name. Project and config records are rank 1 in file order; lockfile records are rank 1 sorted by (type, name).

Shapes: a record line or attribute line with the wrong number of values, or a value of the wrong form in a typed slot (id, coordinate, number, orientation, `true|false`, or one of the listed words such as pin `dir`), is `E029`; so are a missing required attribute and `|` verbatim lines under a record kind that takes none (only `prose`, `body`, `code` and unknown records do). Flag attributes (`hcenter`, `vcenter`) take no value. A second line of a non-repeatable known attribute is `E024`; repeatable ones are `format` (impl), `inherit` (inst), `member` (constraint), `lib`/`include`/`set` (corner), `cell`/`inst` (config), `goal`/`vary` (optimize) and `grant` (plugin).

Ids: 1–13 chars of lowercase Crockford base32 (`0-9 a-h j k m n p-t v-z`); the writer generates 6 (random, caller-seeded; importers seed from FNV-1a of (source path, record ordinal) and redraw on collision). A malformed id is `E029`. Unique per file across all id-bearing kinds; a duplicate on load is reported as warning `W001` on the later occurrence and re-id'd by the `doc` layer. Names (pins, params, impls, vars, bodies, every rank-6 kind, project/config entries; `locked` by (type, name)) are unique within their kind (`E024`). The reader also normalizes wires so `(x1, y1) < (x2, y2)`.

### Rank 0 — header

`schemify 1 <kind>` with attributes:

| Attr | Kinds | Meaning |
|---|---|---|
| `doc <text>` | cells | one-line description |
| `default <impl>` | shprim, shcomp | default impl name (required when >1 impl) |
| `prefix <letter>` | shprim, shcomp | default instance-name prefix in the editor (`M`, `R`, `X`) |
| `category <word>` | cells | library browser grouping |
| `provenance <text>` | cells | importer source note |
| `corner <name>` | shbench | default corner |
| `config <name>` | shbench | default config |

The writer treats the union of these as the known header attributes for every kind; anything else (e.g. `pdk`, `library`) is kept and written after them.

### Rank 1 — symbol interface (shprim, shcomp only)

| Record | Attributes | Notes |
|---|---|---|
| `pin <name> <dir> <x> <y>` | `domain analog\|digital\|real` (default `analog`), `inherit <net>`, `number <n>`, `alias <pin>` | `dir` ∈ `in out inout`. File order = port order. `name` may be a bus `D[3:0]`. `inherit` makes the pin an inherited connection (D29): it is not drawn-connected; its net is resolved by name (§5 of netlist spec). `alias` makes the pin a second position of an earlier ordinary pin (one port, both positions one net; netlist spec §3); it is how xschem's same-named pins import. |
| `param <name> <default>` | `kind number\|int\|bool\|string\|choice` (default `number`), `unit <u>`, `min <expr>`, `max <expr>`, `step <expr>`, `grid <expr>`, `choices <v>…`, `computed <expr>`, `show <expr>`, `doc <text>`, `cached <value> <hash>` | `default` and `computed` are expressions for number/int/bool, template strings for string/choice. A `computed` param cannot be overridden (`E030`). `cached` holds the last value of an expression that calls a plugin function (D20). |
| `impl <name> <lang>` | `format <dialect> <template>` (repeatable, one per dialect), `model <name>`, `file <relpath>`, `ignore true\|open\|short` | `lang` ∈ `schematic spice veriloga verilog` or any plugin tag. `dialect` ∈ `spice xyce cdl spectre verilog` (`spice` is the fallback for `xyce`). `file` is relative to the cell file. |

Kind rules (`E025`): `shprim` has ≥1 pin and ≥1 impl, none with lang `schematic`. `shcomp` has exactly one impl with lang `schematic`. A cell with more than one impl has a `default` header attribute. Every impl of a cell matches the cell's pins (D11): enforced on save (`E002`). A `.shbench` has no impls, so no `body` records and no `port` records.

### Rank 2 — symbol graphics (shprim, shcomp only)

| Record | Attributes |
|---|---|
| `line <x1> <y1> <x2> <y2>` | `layer <n>`, `dash <n>`, `width <n>` |
| `rect <x1> <y1> <x2> <y2>` | `layer`, `fill true\|false` |
| `arc <cx> <cy> <r> <start_deg> <sweep_deg>` | `layer`, `fill` |
| `circle <cx> <cy> <r>` | `layer`, `fill` |
| `poly <x> <y> <x> <y> …` (≥ 2 points) | `layer`, `fill`, `closed true\|false` |
| `stext <x> <y> <orient> <template>` | `size <s>`, `hcenter`, `vcenter`, `layer` |

`stext` templates may reference `name`, params and `cell` (render-time only).

### Rank 3 — schematic (shcomp, shbench)

Kinds in writer order:

| Record | Attributes | Notes |
|---|---|---|
| `inst <id> <cellref>` | `at <x> <y> <orient>` (required, first), `name <instname>` (required), `ignore true\|short\|open`, `<param> <value>` overrides, `inherit <pin> <net>` | `cellref` = `lib:cell` or `./cell` (same directory). Only overrides are stored (D40). `name` may be iterated `X1[3:0]`. |
| `wire <id> <x1> <y1> <x2> <y2>` | `bus true` | Writer normalizes so `(x1, y1) < (x2, y2)` lexicographically. Zero-length wires are an error (`E026`). |
| `label <id> <x> <y> <orient> <netexpr>` | `global true` | Net names live only on labels and ports. |
| `port <id> <x> <y> <orient> <pin>` | | Binds a schematic net to interface pin `<pin>` (shcomp only). |
| `noconn <id> <x> <y>` | | Marks an intentionally unconnected pin. |
| `text <id> <x> <y> <orient> <text>` | `size <s>` | Annotation only. |
| `constraint <id> <type>` | `member <id>` (repeatable), any `<key> <value>` | D30. `type` is a plugin-defined word; core stores, diffs, revalidates members. |
| `waive <id> <inst_id> <param>` | `reason <text>` (required) | D13 waiver for one instance param. |

### Rank 4 — docs (cells)

Document order is preserved.

| Record | Attributes | Notes |
|---|---|---|
| `prose <id>` | verbatim lines | Markdown + `$…$` / `$$…$$` LaTeX math (D21). |
| `var <name> <expr>` | `unit <u>`, `min <expr>`, `max <expr>`, `doc <text>`, `cached <value> <hash>`, `dist gauss\|uniform <spread> [rel]` | The cell's variables table (D19). Bench vars are the sweep/optimizer variables. `dist` makes a bench var vary in Monte Carlo runs: normal with standard deviation `spread`, or uniform over `±spread`; `rel` scales the spread by the value ([run.md](run.md) §3). |

### Rank 5 — impl bodies

| Record | Attributes |
|---|---|
| `body <impl>` | verbatim lines: the definition text emitted once per netlist (SPICE subckt text, Verilog-A source, Verilog source) |

### Rank 6 — bench (shbench only)

| Record | Attributes | Notes |
|---|---|---|
| `corner <name>` | `lib <path> <section>` (repeatable), `include <path>` (repeatable), `set <var> <expr>` (repeatable), `temp <expr>` | Model sections and var overrides. Paths may contain `${NAME}` expanded from project `set` entries, then the environment; nothing executes. |
| `config <name>` | `order <impl>…`, `cell <cellref> <impl>` (repeatable), `inst <path> <impl>` (repeatable) | D11a impl selection. |
| `sweep <name>` | `var <v>`, and one of `values <expr>…` / `lin <from> <to> <n>` / `log <from> <to> <n>` | `lin`/`log` bounds are constants, `n` in `1..=65536` ([run.md](run.md) §2). |
| `montecarlo <name>` | `samples <n>` (required), `seed <n>` (default 0) | Varies every var with `dist` ([run.md](run.md) §3). |
| `optimize <name>` | `algo random\|nelder-mead` (default `nelder-mead`), `goal <output> min\|max\|target <expr>` (repeatable), `vary <var>` (repeatable; bounds come from the var), `budget <n>` (default 100), `seed <n>` (default 1) | [run.md](run.md) §6. |
| `output <name> <expr>` | `unit <u>` | Calculator-mode expression over each job's raw file, reduced to one number ([run.md](run.md) §4). |
| `spec <output>` | `min <expr>`, `max <expr>` | |
| `code <name> <lang>` | verbatim lines | `lang` `spice` = all SPICE dialects; `ngspice`/`xyce`/`spectre` = that dialect only; anything else goes to a runner plugin. |

### Project manifest (`schemify.project`)

| Record | Attributes |
|---|---|
| `lib <name> path <relpath>` / `lib <name> git <url> <rev>` / `lib <name> url <url> <sha256>` | `subdir <relpath>` |
| `plugin <name> path\|git\|url …` (same forms) | `grant <capability>` (repeatable) |
| `set <key> <value>` | |

`lib core builtin` is implicit (the bundled core library).

### Lockfile (`schemify.lock`)

`locked <lib|plugin> <name> <kind> <resolved> <sha256>`, sorted by (type, name). Tool-written only.

A library's content hash is the lowercase hex sha256 over its files sorted bytewise by library-relative path, each contributing its path bytes, one `0x00` byte, and the 32-byte sha256 of its contents. `<resolved>` is the commit a git rev resolved to, the url, or the path. `lib core builtin` is never locked (it ships with the tool); an explicit `lib core …` in the manifest replaces the builtin.

### User config

`set <key> <value>`, `key <chord> <command>`, `sim <name>` with `path`, `args` (`ngspice`: the simulator command line, `{netlist}` and `{raw}` replaced by the job's files; `openvaf`, `iverilog`: compilers, [run.md](run.md) §5), `plugin <name>` (order = load order, D8). The app edits single lines in place and never rewrites hand-written config.

## 5. Unknowns and forward compatibility

- Unknown attributes are preserved verbatim, in order, after known ones.
- An unknown record word is an error (`E027`) unless the header version is newer than the reader's, in which case the record (with its attributes and verbatim lines) is preserved verbatim at its position. A known word that the file kind does not allow (`pin` in a `.shbench`) is unknown for that kind.
- An unknown record has the rank of the closest known record before it (0 right after the header). In a sorted rank (3, 6) it is written after the rank's known kinds, keeping file order; elsewhere it keeps its position. For merge and diff it is identified by its word and first field.

## 6. Values inside records

The file layer only splits values. Their meaning:

- **Expression positions** (`param` default for number/int/bool kinds, `computed`, `min`, `max`, `step`, `grid`, `show`, `var`, `output`, `corner set`, `sweep` bounds, instance overrides of number/int/bool params): parsed by [expr.md](expr.md).
- **Template positions** (`format`, `stext`, string/choice param defaults and overrides): template strings ([expr.md §6](expr.md)).
- Everything else: literal text.

## 7. Error and warning codes

`E0xx` codes are shared with the checker (netlist spec §8). Format-level codes:

| Code | Meaning |
|---|---|
| E020 | same cell stem with two kinds in one library |
| E021 | file too large |
| E022 | coordinate not representable |
| E023 | record out of rank order |
| E024 | duplicate name within a kind |
| E025 | kind rule violated (§4 rank 1: pins, impl count and langs, `default`) |
| E026 | zero-length wire |
| E027 | unknown record |
| E028 | malformed line (lexical; also BOM, invalid UTF-8, bad header) |
| E029 | wrong arity or value form (number, coordinate, id, orientation, listed word), missing required attribute, verbatim lines where none are allowed |
| E030 | override of a computed param |
| W001 | duplicate id (later occurrence re-assigned by the `doc` layer) |

## 8. Merge and diff

The merge driver (D34) and the in-app reload merge (D28) are record-level, need no semantics beyond §4, and are implemented by `schemify-format` (`merge3`, `diff`).

- **Identity.** The header is one record. Id-keyed kinds are identified by (kind, id), named kinds by (kind, name), `locked` by (kind, type, name), unknown records by (word, first field); a further occurrence number separates duplicates (`W001` files). Rank-2 graphics have no identity.
- **Three-way pick** for any unit given base O, ours A, theirs B: the side that changed wins; an identical change on both sides is taken once; different changes on both sides conflict.
- **Records** present on all sides: the pick applies to the whole record; if both changed it differently, it applies per component: the record line, each attribute line (identified by key, plus the first value for repeatable attributes, plus occurrence), the verbatim block and the comment block. Attribute adds and deletes merge like records. Any conflicting component makes the record a conflict.
- **Adds and deletes.** A record added on one side is kept; deleted on one side and unchanged on the other is dropped; deleted against modified is a conflict. Added on both sides: identical is kept once; different with the same name is a conflict; different with the same id re-ids theirs: the new id is drawn deterministically (splitmix64 seeded with FNV-1a of the old id, redrawn while taken in any input) and every id field or id reference (`waive` target, constraint `member`) in theirs is rewritten.
- **Rank 2** (symbol graphics) merges as one block by the pick rule.
- **Order** in file-order ranks: records common to all sides keep theirs' order if ours kept the base order, else ours' order (both reordering differently conflicts over the whole rank). Every other kept record follows the nearest common record before it on its own side; records added at the same place by both sides are ordered by identity, so clean merges are symmetric in A and B. Sorted ranks need no order merge (the writer sorts).
- **Output** is the canonical form (§2). A conflict is written as ours' records, then theirs', between `<<<<<<< ours`, `=======` and `>>>>>>> theirs` lines (an empty side has no records); the driver reports the number of such blocks and fails when it is not 0. End-of-file comments conflict the same way.
- **Diff** lists records added, removed and changed between two files, in identity order, with which components changed and which attribute keys were added, removed or changed. Graphics are matched by occurrence within their kind. Order-only changes are not reported.
