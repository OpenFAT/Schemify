# Elaboration and netlisting v1 (normative)

Implements D11a, D13, D22, D29, D40. Inputs: a project (manifest + lockfile), a top cell (a `.shbench` for simulation, any cell for `schemify netlist`), a config name, a corner name, a dialect, optional var overrides. Output: netlist text + name map + violations. Pure function of its inputs: byte-identical output on every platform.

## 1. Cell resolution

- `lib:cell` resolves through the manifest's `lib` entries (D10); `./cell` resolves in the referencing file's directory (open or only indexed: the library index keeps each cell's directory). A cell is found by trying `<cell>.shcomp`, then `<cell>.shprim`; `.shbench` is never instantiable (`E001`).
- Missing lib or cell → `E001` (error). Hierarchy recursion (a cell instantiating itself at any depth under the selected impls) → `E014`.

## 2. Impl selection (per instance path)

For an instance at hierarchical path `p` (e.g. `/X1/X3`) of cell `c`, in the selected bench config:
1. `inst <p> <impl>` if present;
2. else `cell <c> <impl>` if present;
3. else the first name in `order` that `c` has;
4. else `c`'s `default` (or its only impl).

An impl named by 1–2 that `c` lacks is `E031`. The selected impl must support the dialect: `schematic` always; text/leaf impls need a `format <dialect>` (or `spice` fallback for `xyce`) or a default template from §6; otherwise `E013`.

## 3. Connectivity (per cell, independent of instance)

Union-find over the cell's schematic points, using exact i32 coordinates:

- **Nodes:** every wire endpoint; every instance pin position (after the instance transform); every label, port and noconn anchor.
- **Edges:**
  - the two endpoints of a wire;
  - any node lying on a wire segment, endpoints included (T-junctions connect);
  - nodes at identical coordinates (pin-to-pin abutment, label on wire end, etc.).
- Wires that only cross in their interiors do **not** connect.
- Wires with `bus true` carry buses; connecting a bus wire to a scalar wire is fine (width comes from names).

Each connected component is a **net**. Naming, per net:
1. names from `port` records (the interface pin name), then from labels;
2. more than one distinct name on one net → `E003` (shorted names); the emitted name is the port name if any, else the bytewise-smallest label;
3. no name → `net_<id>` where `<id>` is the bytewise-smallest id among the net's wires, labels, ports and noconns (each belongs to exactly one net); a net made only of instance pins → `net_<inst id>_<pin index>` of its bytewise-smallest (instance id, pin index) pin node. A generated name is therefore unique per net and can never merge two nets (SPICE connects equal node names).

Global nets: the name `0`, any name ending in `!`, and any name carried by a label with `global true` anywhere in the elaborated hierarchy (xschem's `.GLOBAL` symbols import this way; the name keeps its spelling). Globals are the same net in every cell and are never subckt ports. An inherited pin (`pin … inherit <net>`) is not a geometric node; see §5. An alias pin (`pin … alias <P>`, format spec rank 1: a second position of the earlier ordinary pin `P`, xschem's same-named pins) is a geometric node joined to `P`'s node in every instance; it carries `P`'s bits, is no port of the cell's definition and no connection of its own, and needs no `port` record (§8 `E012`).

## 4. Buses, bundles, iterated instances

Net expression grammar (labels, ports, pin names, inherit nets, instance names):

```
netexpr = term *("," term)                 ; bundle, left to right
term    = [count "*"] base                 ; repeat
base    = ident ["[" int [":" int] "]"]    ; bit or range, msb:lsb in written order
ident   = [A-Za-z_][A-Za-z0-9_$.]* ["!"]   ; or the literal "0"
```

- A label's netexpr expands to an ordered bit list (`d[3:0]` → `d[3] d[2] d[1] d[0]`; `2*a,b` → `a a b`). The net's width is the list length; all names on one net must have equal width (`E011`).
- Instance `name X1[3:0]` creates 4 instances `X1[3]…X1[0]`. For each pin of width w connected to a net of width N: N = w → each instance gets all bits; N = w × count → instance k gets the k-th slice of w bits (in the instance name's order); N = 1 and w = 1 → broadcast; otherwise `E011`.
- Emitted names flatten bits for the dialect (§6).

## 5. Inherited connections

For an inherited pin `P` with default net `D` on cell `c`, at instance path `p`: walk from `p` toward the root; the first ancestor-or-self instance with `inherit P <net>` supplies `<net>`, interpreted in the scope of the cell that contains that instance. If none, `D` (a global). Inherited pins are emitted like ordinary pins (they become subckt ports carrying the resolved net).

## 6. Parameter resolution

- Effective value of param `k` on an instance: the instance's override expression if present, else the cell's `default`. Overrides of unknown params → `W007` (kept, not emitted). Overrides of `computed` params → `E030`.
- Evaluation scope: overrides are evaluated in the **parent** cell scope (parent's effective params, then parent vars); `computed` params and the cell's own vars are evaluated in the **child** scope with the child's effective params. Bench vars are the root scope, after corner `set`, sweep/MC/optimizer assignments, then CLI overrides.
- Every param is evaluated to a concrete value by our evaluator (no SPICE-side expressions for design params). Gating (D13), per instance and param, on the evaluated value:
  - `grid`/`step`: values are snapped on edit by the editor; an off-grid value in a file is `E032` (error, waivable);
  - outside `[min, max]` → `E005`; not in `choices` → `E006`;
  - a matching `waive <id> <inst_id> <param>` downgrades the violation to `I005` (info, reported, not blocking).
- **Subckt specialization:** a `schematic` impl is emitted once per distinct tuple of its evaluated params. The tuple equal to the cell defaults is named `<cell>`; others `<cell>__<h>` where `<h>` is the first 6 base32 chars of a stable hash (FNV-1a 64) of the canonical text of the sorted `k=v` list. Identical tuples share one definition.

## 7. Emission

### 7.1 Common rules

- Dialect ∈ `ngspice xyce cdl spectre verilog`. `spice` templates serve `ngspice`, `xyce` and `cdl` unless a dialect-specific `format` exists.
- Instances (or impls) with `ignore true` are not emitted; `ignore open` is not emitted and its pins are left unconnected; `ignore short` is not emitted and all its pin nets are merged into one net (named by §3 rules). Ignored instances still elaborate for checks.
- Instance lines come from the selected impl's template ([expr.md §6](expr.md)). Default templates when an impl has none:

  | impl lang | ngspice / xyce | cdl |
  |---|---|---|
  | `schematic` | `X{name} {pins} {cell}` | `X{name} {pins} {cell}` |
  | `spice` (text body) | `X{name} {pins} {cell}` | same |
  | `veriloga` | `N{name} {pins} {model}` (ngspice OSDI); xyce → `E013` | `E013` |
  | `verilog` | `A{name} [{in_pins}] [{out_pins}] {model}` (ngspice d_cosim); xyce → `E013` | `E013` |

  `in_pins`/`out_pins` are the template identifiers for digital-domain pins by direction.
- `body` text is emitted once per netlist per (cell, impl), verbatim except for `${NAME}` expansion.
- `${NAME}` in bench `code` text, impl `body` text and corner paths expands from project `set` entries, then the environment; an undefined name is `E034` (nothing is executed; `$$` is a literal `$`).
- Ordering (deterministic): header comment; bench `.param` lines (vars, name order); corner `.lib`/`.include`/`.temp` lines in file order; top-level instance lines sorted by emitted name; bench `code` blocks for the dialect sorted by name; subckt definitions sorted by subckt name, each with its instance lines sorted by emitted name, then its component `code` blocks (§7.6.15); once-only lines (models, OSDI loads) sorted; `.end`.
- Numbers use canonical number text (expr §7). Names: hierarchical emitted names are local (`XM1` inside a subckt); the name map records path ↔ emitted hierarchical name (`/X1/M1` ↔ `X1.XM1`; ngspice lowercases at runtime, the map stores both).

### 7.2 ngspice

- Subckt: `.subckt <name> <port nets in pin order>` … `.ends <name>`. Inherited pins are appended after ordinary pins in declaration order.
- Globals other than `0`: one `.global <names…>` line after the header.
- Verilog-A: once per module `.model <model> <module>`; OSDI load as `.control` / `pre_osdi <stem>.osdi` / `.endc` once per source (the runner compiles `.va` → `.osdi`; `stem` is the `file` stem, else the cell stem for a source `body`, §7.6.8).
- Verilog: `.model <model> d_cosim simulation="<file stem>.vvp"`; digital-domain nets get ngspice auto-bridges (no explicit bridges emitted).
- Code blocks with lang `spice` or `ngspice` are emitted verbatim.

- **Bus bits in XSPICE vectors (ngspice):** a net that connects to an XSPICE port vector (`[ … ]`, digital-domain pins of `verilog` impls) is spelled with `[k]` → `_k` (`d[0]` → `d_0`) everywhere in that netlist, because ngspice's tokenizer splits `d[0]` inside `[ … ]`. All other bus bits keep the `d[0]` spelling (xschem parity). The nets are those some emitted row writes inside `[ … ]`: a template hole inside brackets (outside holes) naming a pin, `pins`, `in_pins` (the `in` pins) or `out_pins` (the others), and every ordinary pin of a `verilog` impl on its default template. The respelling replaces `[` by `_` and drops `]` (`x[1][2]` → `x_1_2`); when the result is already another net's name, `_` is appended until it is not, so no two nets merge. Ports, instance lines and the name map use the same spelling. (Decided and implemented 2026-10-10; xschem writes `Y_NAND_Ax4x` there and leaves the bit spelled `[4]` elsewhere.)

### 7.3 xyce

As ngspice except: subckt params use `PARAMS:` when present; `.control` blocks are never emitted (a `spice` code block containing `.control` → `E033`); lang `xyce` blocks emitted.

### 7.4 cdl

For LVS: no code blocks, no corners, no `.param`, no `.control`. Top is the netlisted cell as a `.SUBCKT` with its pins. `*.PININFO <pin>:<I|O|B>…` after each `.SUBCKT` line. Instance templates `format cdl` when present (PDK LVS formats), else the `spice` template.

### 7.5 spectre, verilog

Spectre: `subckt <name> (<ports>)` … `ends <name>`, instances `<name> (<nets>) <cell> k=v`, `simulator lang=spectre` header; spice-format templates are not reused (an impl without `format spectre` and not `schematic` → `E013`). Structural Verilog: one `module` per schematic cell, `wire` per internal net, named port connections; leaf impls need `format verilog` or a `verilog` body. Details in §7.6.

### 7.6 Emission decisions (amendments, WS3 `netlist`)

Where §7.1–§7.5 left a choice open, `crates/netlist` decided as follows; these are normative.

1. **API.** `emit(&Elab, Dialect) -> Netlist { text, name_map }`. It never fails: the caller gates on violations (§8). A template that fails to render (`X0xx`, a param without a value `E008`, a lang without a default `E013`) becomes the comment line `* error in <name>: <message>` (`//` for spectre/verilog) and gets no name-map row; an unconnected pin bit (`E011`, `ignore open`) is written `?`. A template that renders nothing emits no line.
2. **Header:** one comment line `* <top cell> (schemify <dialect>)` (`//` for spectre and verilog; spectre adds `simulator lang=spectre`). No dates, paths or versions, so output is byte-identical across runs and machines.
3. **Order.** Header; `.global`; bench vars one `.param k=v` per line in name order; corner lines in file order; the top block; code blocks (elab's order); definitions by name (each block: instance lines, then the cell's own code, §7.6.15); once-only text: OSDI `.control`/`pre_osdi <stem>.osdi`/`.endc` blocks by file stem, then generated `.model` lines sorted, then impl bodies by (cell display name, impl name); `.end`. Only definitions reachable through non-ignored rows are emitted, and only impls some emitted row uses contribute once-only text.
4. **Instance order:** within a block, lines sort bytewise by emitted name, which is the first whitespace-delimited token of the rendered text (the second for verilog: `<module> <instance> (…);`); ties keep elab's row order. A multi-line template output sorts as one unit.
5. **Device letters of default templates** (`X` schematic/spice, `N` veriloga, `A` verilog) are added only when the instance name does not already start with that letter, ASCII case-insensitively: `X1` stays `X1`, `M1` becomes `XM1` (so `/X1/M1` ↔ `X1.XM1` holds with xschem-style names). Explicit templates are rendered as written.
6. **Template identifiers** (expr §6): `name` the local element name (`X1[3]`) in the dialect's spelling; `cell` the definition name for schematic targets, the cell stem for leaves; `model` the impl `model` attr, else the `model` param; `file` the impl `file` attr as written (relative to the cell file); `pins` every connection, ordinary then inherited; a pin by its name up to `[` (bus pins: space-joined bits); `in_pins` the nets of `in` pins, `out_pins` those of every other pin (`out`, `inout`, inherited); `params` the `k=v` of non-computed params whose value differs from the cell default for leaf rows, empty for schematic rows (a specialized subckt takes no params, §6). Builtins shadow pins, pins shadow params. A schematic impl's own `format <dialect>` is used when present (the importer writes `{name} {pins} {cell}`), else the default.
7. **Templates render fast** when every hole is a bare identifier: elab's canonical value texts are copied without evaluation; any other hole (or a `cnum`/failed param value) goes through `schemify_expr::Template`. Both produce identical text (differential test).
8. **Bodies.** SPICE dialects emit the body of every used `spice` impl and, except cdl, of every precompiled `veriloga` impl (no `file`, and a body without an `endmodule` line: SPICE text, model card and OSDI load, as the importer writes it). A `veriloga` impl whose source is a `file` or its `body` (a body with an `endmodule` line: Verilog-A source, the module named after the cell) is compiled by the runner (run spec §5): `pre_osdi <stem>.osdi` once per source and `.model <model> <stem>` per model name (impl attr, and each default-template row's model), `stem` being the `file` stem, else the cell stem; its body is never emitted. `netlist::compiled_source` is the one predicate. A `verilog` impl gets `.model <model> d_cosim simulation="<file stem, else cell stem>.vvp"` (ngspice); the verilog dialect emits `verilog` bodies (the module source). The `body` of the `schematic` impl of every emitted definition (simulation text such as the model cards of an imported instance `device_model`) is emitted once, like a `veriloga` body: SPICE dialects except cdl (xschem keeps it in its LVS netlist's architecture block). Spectre wraps SPICE bodies in `simulator lang=spice` … `simulator lang=spectre`.
9. **xyce:** `.temp t` is written `.options device temp=t`; `PARAMS:` never occurs since specialized subckts take no params.
10. **cdl:** `.SUBCKT`/`.ENDS`/`.GLOBAL`/`.END` in capitals; the top is `.SUBCKT <top definition name> <ports>` (a bench has none) followed by its `*.PININFO` (omitted without ports). `*.PININFO` directions: ordinary pins by `dir` (`in` I, `out` O, `inout` B) bit by bit, declared inherited pins by their `dir`, inherited pins propagated from the subtree B.
11. **spectre:** identifiers escape every character outside `[A-Za-z0-9_]` with `\` (`vdd\!`, `d\[3\]`); `global` lists `0` too; vars on one `parameters` line; corner `lib` → `include "<path>" section=<s>`, `include` → `include "<path>"`, `temp` → `corner_temp options temp=<t>`; code blocks of lang `spectre` verbatim; no end statement.
12. **verilog:** one `module` per emitted definition, the top included (`module tb ();` without ports); port directions from the pins (propagated inherited pins `inout`); a `wire` per other net the block's rows use, sorted; names outside `[A-Za-z_][A-Za-z0-9_$]*` are escaped identifiers (`\d[3] `); a `verilog` leaf without `format verilog` instantiates `<model> <name> (.<pin bit>(<net>), …);`; definition rows instantiate `<definition> <name> (.<port net>(<net>), …);`. Global nets are module-local wires (Verilog has none; open issue).
13. **Name map** (path ↔ emitted name, both directions, instances and nets). Stored per emitted definition like elab's tables (rows and nets of a definition, not flattened), so its cost is the design's rows. Instance `/X1/M1` ↔ `X1.XM1` (emitted local names joined with `.`); net `/X1/n1` ↔ `X1.n1`; a port net resolves to the net it connects to one level up (ngspice has no `x1.a` node) and a global is its bare name at every depth. Reverse lookups compare ASCII case-insensitively for the SPICE dialects (ngspice lowercases; an exact spelling wins), accept escaped or unescaped net names, and try every `.` split of dotted names. Ignored rows and rows whose template failed have no entry. The reverse indexes are sorted on the first reverse lookup.
14. **Open issues.** `file` stays as written, relative to the cell file: an absolute path would make the netlist depend on the checkout location (§7.6.2 forbids paths in the output), and the runner resolves `file` against the cell's directory itself; a template that needs a path should get a project-relative one (decision pending). Spectre/verilog have no corpus oracle (xschem writes neither); Verilog output is compiled by Icarus Verilog in CI (parse and elaboration, TESTING.md), Spectre has snapshots only (no Spectre-syntax simulator runs in CI). (XSPICE bus bits: resolved, §7.2.)
15. **Component code** (D17 as amended 2026-10-11). The `code` blocks of a `.shcomp` that the dialect takes (§9.13: `spice` for ngspice and xyce, a dialect word for that dialect, none for CDL) are the component's own netlist text: written verbatim (`${NAME}` expanded) after the instance lines of **every** definition of the cell, specializations included, blocks sorted by name, before the closing `.ends`/`ends`/`endmodule`; for a component netlisted as the top, after the top block's instance lines. xschem writes a subcircuit schematic's code blocks and `S {}` text inside its `.subckt` the same way (goldens `MSA-2643`: the `Q1` model cards; `test_stdcells`: the XSPICE gates of `stdcells_xspice`). A bench's own code stays §7.6.3's single block at the top. Xyce's `.control` rule (`E033`) applies to component code too. The flattened test oracle (`emit_flat`) leaves component code out: its text names nets in the subckt's scope.

## 8. Checks

Severity `E` blocks Run and makes `schemify netlist` exit 1 (unless `--allow-errors`); `W` is reported; `I` is informational.

| Code | Sev | Meaning |
|---|---|---|
| E001 | E | missing lib/cell, or bench instantiated |
| E002 | E | impl pins ≠ symbol pins |
| E003 | E | shorted names on one net |
| W004 | W | instance pin unconnected (no wire, no abutment, no noconn) |
| E005 | E | param outside min/max (waivable) |
| E006 | E | param not in choices (waivable) |
| W007 | W | override of unknown param |
| E008 | E | expression error (any `X0xx`), including an impl `format` template that does not parse or names an identifier no builtin, pin or param provides (checked when elaboration selects the impl, so the gate stops what the netlister would write as `* error in …`) |
| E009 | E | duplicate instance name in a cell |
| W010 | W | `real` domain pin on an analog/digital net |
| E011 | E | bus width mismatch |
| E012 | E | port without interface pin, or interface pin without port (shcomp) |
| E013 | E | dialect unsupported by selected impl |
| E014 | E | hierarchy recursion |
| W016 | W | net with a single connection |
| E030 | E | override of computed param |
| E031 | E | config names an impl the cell lacks |
| E032 | E | param off grid/step (waivable) |
| E033 | E | `.control` in a code block for xyce |
| E034 | E | undefined `${NAME}` in code, body or corner path |
| E035 | E | unknown corner or config named by the request |
| E036 | E | inherited pins of one name with different default nets in one definition |
| I005 | I | waived violation |

Violations are keyed by (code, cell, object id, instance path, param) and sorted bytewise on that key (code text first). A finding true for every path through an object has no path.

## 9. Elaboration decisions (amendments, WS2 `elab`)

Where §1–§8 left a choice open, `crates/elab` decided as follows; these are normative.

1. **Names connect.** Components carrying the same bit name are one net, in the cell and (for globals) everywhere. A component with names whose bit lists differ is `E003` (one row per extra name); names of different width on one component are `E011` and the narrower-ranked name is ignored.
2. **Emitted bit name**, per merged net: global names first (`0`, `…!`, `global true`), then port names (and interface pins bound by name), then label and `inherit` names; bytewise smallest within the class. Unnamed: the §3 rule (`net_<id>` from an owned wire/label/port/noconn id, else `net_<inst>_<pin>`), with `[k]` (component bit `k`, from 0) appended for wider components. The component (render/plugin `Nets` row) is named by its best name expression (`d[3:0]`).
3. **Width of an unnamed component** = its widest attached pin, so an unnamed wire between two 4-bit pins is 4 bits, and an iterated instance's 1-bit pins slice it.
4. **Net expressions** outside the grammar are one scalar name, verbatim (importer leftovers keep working); an expression over 65 535 bits is `E011`.
5. **Pins bind by name.** A `port` names its component with its pin's bits; every interface pin (ordinary or inherited) binds to the nets named by its bit names, so a label spelled like the pin binds it too. Inside a cell's schematic an inherited pin `P` is the net named `P` (not its default).
6. **Inherited connections, hierarchically.** A definition takes as ports its cell's declared inherited pins (pin order) and every inherited pin its subtree leaves unresolved (sorted by name), after its ordinary pins. At the top they bind to their default net. An `inherit P <net>` override of a pin propagated from deeper is width-checked where it is resolved (`E011`); one pin name with two defaults in one definition is `E036`.
7. **`global true` labels** are collected over every schematic reachable from the top (a superset of the selected impls, but path-independent); the editor's single-document view uses its own document's.
8. **Checks:** `W004` = an instance pin alone in its component; `W016` = a non-global bit net with exactly one connection (instance pin bits, port bits, `inherit` overrides); `W010` per bit net; `E009` on element names after iterated expansion; `E002` once per schematic impl whose ports and pins disagree (alongside the per-object `E012`); `E031` from a `cell` rule has no path, from an `inst` rule the rule's path; a cell with no selectable impl is `E013`.
9. **Specialization text:** the non-computed params as `k=v`, sorted bytewise by `k`, joined by single spaces; numbers in canonical text (expr §7), strings quoted with expression escapes. The hash digits are the Crockford lowercase alphabet of ids, most significant 5 bits first. The default tuple is the cell's defaults evaluated in its own scope. Two definitions of one cell with one tuple but different children (config `inst` rules) both hash `text|name=target,…` (children are named first; targets are child definition names or `cell:impl`). Distinct tuples whose 6 digits collide use all 13. Distinct cells sharing a stem use `<stem>__<6 digits of FNV-1a 64 of lib:dir/stem>` as their base name.
10. **`ignore`:** the instance attr wins over the selected impl's. An impl-level `short` merges the instance's nets within the definition (renamed to the best name: global, then a definition port, then bytewise smallest).
11. **Request:** corner `set` lines apply first, then the request's overrides in order (later wins); an override of an unknown var adds it. The default corner/config is the bench's header attr; an unknown name is `E035`. Corner `lib` sections are literal; `temp` is evaluated in the bench scope.
12. **`${NAME}`** is expanded by elab from project `set` entries, then an environment table the caller passes in the request (elab reads no environment), so the netlister receives final text.
13. **Code blocks per dialect:** `spice` → ngspice and xyce, a dialect word → that dialect, CDL → none.
14. **Loaded documents:** cells whose schematic or body elaboration needs but which are only indexed (ranks 0–2: interface and symbol graphics) are listed for the app to open, then elaboration runs again; meanwhile an unopened schematic child is skipped and a leaf keeps its template.
15. **D20:** a param's or var's `cached` value stands in for the whole expression when a plugin function is missing; the value and everything computed from it are flagged unverified.
16. **Scopes:** child vars read by a child-scope expression are evaluated per instance, in that instance's scope.
17. **The name map** (path ↔ emitted name) is the netlister's: it depends on the dialect's emitted names. Elab's definitions keep the hierarchy; a path is the `/`-joined local names from the top.
18. **Alias pins** (§3): `alias <P>` names an earlier ordinary pin of the same cell, else the pin is ordinary; an alias pin shares `P`'s connection slots (`pin_off`), adds no bits to the row and no port to the definition, and is skipped by `E012`, `*.PININFO`, `in_pins`/`out_pins` and Verilog port lists.
19. **Definition bodies:** a definition records the `Impls` row holding its cell's schematic-impl `body` (`Defs::body`, `u32::MAX` without one) so the netlister can emit it once (§7.6.8).
20. **Template check:** `E008` for a leaf impl's template is reported once per (cell, impl) with the impl name as the finding's param; identifiers resolve as in §7.6.6 (`model` needs the impl `model` attr, a `model` param or pin; `file` needs the impl `file` attr).
21. **Component code:** a definition records its cell's `code` lines for the dialect (`Defs::code`, a CSR range per definition, empty unless the cell is a `.shcomp`), selected and ordered like bench code (§9.13, `E033` for xyce) with `${NAME}` expanded (§9.12); the netlister writes them per §7.6.15. A bench top's code stays in `Bench`.
