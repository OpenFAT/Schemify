# Elaboration and netlisting v1 (normative)

Implements D11a, D13, D22, D29, D40. Inputs: a project (manifest + lockfile), a top cell (a `.shbench` for simulation, any cell for `schemify netlist`), a config name, a corner name, a dialect, optional var overrides. Output: netlist text + name map + violations. Pure function of its inputs: byte-identical output on every platform.

## 1. Cell resolution

- `lib:cell` resolves through the manifest's `lib` entries (D10); `./cell` resolves in the referencing file's directory. A cell is found by trying `<cell>.shcomp`, then `<cell>.shprim`; `.shbench` is never instantiable (`E001`).
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
3. no name → `net_<id>` where `<id>` is the bytewise-smallest id among the net's wires/labels/instances-pins' owning instance ids (stable across unrelated edits).

Global nets: the name `0`, and any name ending in `!`. Globals are the same net in every cell and are never subckt ports. An inherited pin (`pin … inherit <net>`) is not a geometric node; see §5.

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
- `body` text is emitted once per netlist per (cell, impl), verbatim.
- Ordering (deterministic): header comment; bench `.param` lines (vars, name order); corner `.lib`/`.include`/`.temp` lines in file order; top-level instance lines sorted by emitted name; bench `code` blocks for the dialect sorted by name; subckt definitions sorted by subckt name, each with its instance lines sorted by emitted name; once-only lines (models, OSDI loads) sorted; `.end`.
- Numbers use canonical number text (expr §7). Names: hierarchical emitted names are local (`XM1` inside a subckt); the name map records path ↔ emitted hierarchical name (`/X1/M1` ↔ `X1.XM1`; ngspice lowercases at runtime, the map stores both).

### 7.2 ngspice

- Subckt: `.subckt <name> <port nets in pin order>` … `.ends <name>`. Inherited pins are appended after ordinary pins in declaration order.
- Globals other than `0`: one `.global <names…>` line after the header.
- Verilog-A: once per module `.model <model> <module>`; OSDI load as `.control` / `pre_osdi <file stem>.osdi` / `.endc` once per file (the runner compiles `.va` → `.osdi`).
- Verilog: `.model <model> d_cosim simulation="<file stem>.vvp"`; digital-domain nets get ngspice auto-bridges (no explicit bridges emitted).
- Code blocks with lang `spice` or `ngspice` are emitted verbatim.

### 7.3 xyce

As ngspice except: subckt params use `PARAMS:` when present; `.control` blocks are never emitted (a `spice` code block containing `.control` → `E033`); lang `xyce` blocks emitted.

### 7.4 cdl

For LVS: no code blocks, no corners, no `.param`, no `.control`. Top is the netlisted cell as a `.SUBCKT` with its pins. `*.PININFO <pin>:<I|O|B>…` after each `.SUBCKT` line. Instance templates `format cdl` when present (PDK LVS formats), else the `spice` template.

### 7.5 spectre, verilog

Spectre: `subckt <name> (<ports>)` … `ends <name>`, instances `<name> (<nets>) <cell> k=v`, `simulator lang=spectre` header; spice-format templates are not reused (an impl without `format spectre` and not `schematic` → `E013`). Structural Verilog: one `module` per schematic cell, `wire` per internal net, named port connections; leaf impls need `format verilog` or a `verilog` body. (Full details land with these emitters; M1 ships ngspice, xyce and cdl first.)

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
| E008 | E | expression error (any `X0xx`) |
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
| I005 | I | waived violation |

Violations are keyed by (code, instance path or object id, param) and sorted for output.
