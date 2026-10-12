# Editability audit: can the UI edit everything in .shprim / .shcomp / .shbench?

Status: audit, 2026-10-12, branch `ui/modern-preset`. Question: is every record and attribute of `docs/spec/format.md`, especially those that feed the netlist (`docs/spec/netlist.md`), editable through structured UI rather than only through the raw whole-file dialog (`properties.edit_file_danger`, Alt+Q)?

Status values:
- **STRUCTURED**: a dedicated UI path exists.
- **PARTIAL**: create-only, or only some attributes.
- **RAW-ONLY**: only the Edit-file dialog.
- **TOOL**: written by the tool, never by the user.

`file:line` references are under `crates/app/src` unless a path is given.

**Totals:** 20 STRUCTURED, 9 PARTIAL, 31 RAW-ONLY, 1 TOOL.

| Record / attr | Files | Netlist? | UI path | Status |
|---|---|---|---|---|
| hdr `doc` | cells | no | `properties.edit_header_license_text`, editor/parity.rs:1617 | STRUCTURED |
| hdr `prefix`, `category` | prim/comp | no | same | STRUCTURED |
| hdr `default` | prim/comp | **yes** (impl choice) | — | RAW-ONLY |
| hdr `provenance`, unknown | cells | no | — | RAW-ONLY |
| hdr `corner`, `config` | bench | **yes** | — | RAW-ONLY |
| `pin` create/delete | prim/comp | yes | `symbol.place_symbol_pin` editor.rs:1877 (always `inout`); make_symbol parity.rs:1756 | STRUCTURED |
| pin `name`, `dir` | prim/comp | yes | properties.edit editor.rs:2695 (rename does not follow into ports) | STRUCTURED |
| pin order (= port order) | prim/comp | **yes** | only by regenerating with make_symbol | PARTIAL |
| pin `domain`, `inherit`, `alias` | prim/comp | **yes** | — | RAW-ONLY |
| pin `number` | prim/comp | no | — | RAW-ONLY |
| `param` record (name, default) | prim/comp | **yes** | — (default shown read-only in instance dialog, editor.rs:2658) | RAW-ONLY |
| param `kind`, `computed` | prim/comp | **yes** | — | RAW-ONLY |
| param `min/max/step/grid/choices` | prim/comp | run gate | — | RAW-ONLY |
| param `unit/show/doc` | prim/comp | no | — | RAW-ONLY |
| param/var `cached` | cells | yes | docs.rs:670 | TOOL |
| `impl` create / name / lang | prim/comp | **yes** | only Make Subcircuit seeds `impl sch schematic` (editor/subckt.rs:428) | PARTIAL |
| impl `format`, `model`, `file`, `ignore` | prim/comp | **yes** | — | RAW-ONLY |
| `body` (verbatim impl text) | prim/comp | **yes** | — | RAW-ONLY |
| rank-2 shapes | prim/comp | no | `tools.insert_*`, move/delete | STRUCTURED |
| rank-2 `layer`, poly `closed` | prim/comp | no | set at creation only | PARTIAL |
| rank-2 `fill/dash/width` | prim/comp | no | — | RAW-ONLY |
| `stext` | prim/comp | no | created by make_symbol only (parity.rs:2911) | PARTIAL |
| `inst` create, `at`, orient | comp/bench | yes | `tools.insert_symbol`; move/rotate/flip | STRUCTURED |
| inst `name` | comp/bench | yes | props editor.rs:2635 | STRUCTURED |
| inst `cellref` | comp/bench | yes | Tools→Search replace on `cell` only (search.rs:82) | PARTIAL |
| inst param overrides | comp/bench | yes | props editor.rs:2636 (lists whole schema; no bounds/choices/snap) | STRUCTURED |
| inst `ignore` | comp/bench | **yes** | toggle parity.rs:493 (`true` only; no `open`/`short`) | PARTIAL |
| inst `inherit <pin> <net>` | comp/bench | **yes** | — | RAW-ONLY |
| `wire`, `bus` | comp/bench | yes | insert_wire editor.rs:1884; props :2686 | STRUCTURED |
| `label` netexpr | comp/bench | yes | editor.rs:1873; props :2677 | STRUCTURED |
| label `global` | comp/bench | **yes** | — | RAW-ONLY |
| `port` | comp | yes | editor.rs:1874; props | STRUCTURED |
| `noconn` | comp/bench | net naming, W004 | no place command | RAW-ONLY |
| `text` / `size` | comp/bench | no | insert_text + props / — | STRUCTURED / RAW-ONLY |
| `shape` + attrs | comp/bench | no | insert tools; attrs at creation | STRUCTURED / PARTIAL |
| `constraint` | comp/bench | no | — | RAW-ONLY |
| `waive` | comp/bench | run gate | — | RAW-ONLY |
| `prose` | cells | no | docs.add_prose/edit/move/delete | STRUCTURED |
| `var` create, expr | cells | **yes** | docs.add_var (fixed name `v<k>`, docs.rs:626), docs.var_ok docs.rs:564 | STRUCTURED |
| var name | cells | **yes** | — (no rename) | RAW-ONLY |
| var `unit/min/max/doc` | cells | gate / no | — (displayed only) | RAW-ONLY |
| var `dist` | bench | **yes** (Monte Carlo) | — | RAW-ONLY |
| `corner` `config` `sweep` `montecarlo` `optimize` `output` `spec` | bench | **yes** | — (results panel and `sim.optimize` only read them) | RAW-ONLY |
| `code` | bench/comp | **yes** | `sim.new_bench` seeds `.op` (editor/integrate.rs:479); no edit | PARTIAL |
| comments | all | no | — | RAW-ONLY |

## Netlist-affecting gaps

1. Header `default`; bench `corner` and `config`.
2. Pin `domain`, `inherit`, `alias`, and pin order.
3. Every `param` definition and its attributes.
4. `impl` and its `format`, `model`, `file`, `ignore`; and `body` (Verilog-A/Verilog/SPICE text; there is no port → pin sync).
5. Instance `inherit`, `ignore open|short`, and swapping `cellref`.
6. Label `global`.
7. Var name and `dist`; var bounds also gate a run.
8. All bench rank-6 tables, plus editing `code`.

## Defects found on the way

- **New files can't be saved.** File → New schematic/symbol creates an empty document. Save fails with E025: a `.shcomp` needs a schematic impl, a `.shprim` a pin plus an impl (crates/doc/src/project.rs:886). No UI adds an impl, so only the raw dialog can make one saveable.
- **Search-replace on `inherit` writes a single value** (search.rs:174), which turns `inherit <pin> <net>` into a malformed record.
