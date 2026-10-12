# Editability audit: can the UI edit everything in .shprim / .shcomp / .shbench?

Status: audit, 2026-10-12, branch `ui/modern-preset`. **Update (same day):** the cell settings panel (`view.cell`, `crates/app/src/cell.rs`, plugin-abi §11 item 17) closes gaps 1–4 below for header, pins, params, impls and bodies (marked ✅); the rest stays open. Question: is every record and attribute of `docs/spec/format.md`, especially those that feed the netlist (`docs/spec/netlist.md`), editable through structured UI rather than only through the raw whole-file dialog (`properties.edit_file_danger`, Alt+Q)?

Status values:
- **STRUCTURED**: a dedicated UI path exists.
- **PARTIAL**: create-only, or only some attributes.
- **RAW-ONLY**: only the Edit-file dialog.
- **TOOL**: written by the tool, never by the user.

`file:line` references are under `crates/app/src` unless a path is given.

**Totals** (rows as listed, after the cell panel): 5 PARTIAL, 17 RAW-ONLY, 24 STRUCTURED, 1 STRUCTURED / PARTIAL, 1 STRUCTURED / RAW-ONLY, 1 TOOL. Before the panel: 20 STRUCTURED, 9 PARTIAL, 31 RAW-ONLY, 1 TOOL (finer rows).

| Record / attr | Files | Netlist? | UI path | Status |
|---|---|---|---|---|
| hdr `doc` | cells | no | `properties.edit_header_license_text`, editor/parity.rs:1617 | STRUCTURED |
| hdr `prefix`, `category` | prim/comp | no | same | STRUCTURED |
| hdr `default` | prim/comp | **yes** (impl choice) | cell panel ✅ | STRUCTURED |
| hdr `provenance`, unknown | cells | no | — | RAW-ONLY |
| hdr `corner`, `config` | bench | **yes** | cell panel ✅ | STRUCTURED |
| `pin` create/delete | prim/comp | yes | `symbol.place_symbol_pin` editor.rs:1877 (always `inout`); make_symbol parity.rs:1756 | STRUCTURED |
| pin `name`, `dir` | prim/comp | yes | properties.edit editor.rs:2695 (rename does not follow into ports) | STRUCTURED |
| pin order (= port order) | prim/comp | **yes** | cell panel ↑/↓ ✅ | STRUCTURED |
| pin `domain` | prim/comp | **yes** | cell panel ✅ | STRUCTURED |
| pin `inherit`, `alias` | prim/comp | **yes** | — | RAW-ONLY |
| pin `number` | prim/comp | no | — | RAW-ONLY |
| `param` record (name, default) | prim/comp | **yes** | cell panel ✅ (add, rename, delete) | STRUCTURED |
| param `kind`, `computed` | prim/comp | **yes** | cell panel ✅ | STRUCTURED |
| param `min/max/step/grid/choices` | prim/comp | run gate | cell panel ✅ | STRUCTURED |
| param `unit/doc` | prim/comp | no | cell panel ✅ | STRUCTURED |
| param `show` | prim/comp | no | — | RAW-ONLY |
| param/var `cached` | cells | yes | docs.rs:670 | TOOL |
| `impl` create / delete | prim/comp | **yes** | cell panel ✅ (+ Schematic/SPICE/Verilog-A/Verilog) | STRUCTURED |
| impl rename, lang change | prim/comp | **yes** | — | RAW-ONLY |
| impl `format` (per dialect), `model`, `file` | prim/comp | **yes** | cell panel ✅ | STRUCTURED |
| impl `ignore` | prim/comp | **yes** | — | RAW-ONLY |
| `body` (verbatim impl text) | prim/comp | **yes** | cell panel ✅ (text area, Ctrl+Enter) | STRUCTURED |
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
| `prose` | cells | no | docs.add_prose/edit (live preview)/move/delete; docs.source shows the raw text | STRUCTURED |
| `var` create, expr | cells | **yes** | docs.add_var (fixed name `v<k>`, docs.rs:626), docs.var_ok docs.rs:564 | STRUCTURED |
| var name | cells | **yes** | — (no rename) | RAW-ONLY |
| var `unit/min/max/doc` | cells | gate / no | — (displayed only) | RAW-ONLY |
| var `dist` | bench | **yes** (Monte Carlo) | — | RAW-ONLY |
| `optimize` `output` `spec` | bench | **yes** | cell panel ✅ (#48) | STRUCTURED |
| `corner` `config` `sweep` `montecarlo` | bench | **yes** | — | RAW-ONLY |
| `code` | bench/comp | **yes** | `sim.new_bench` seeds `.op` (editor/integrate.rs:479); no edit | PARTIAL |
| comments | all | no | — | RAW-ONLY |

## Netlist-affecting gaps

1. ✅ Header `default`; bench `corner` and `config`.
2. ✅ Pin `domain` and pin order. Open: `inherit`, `alias`.
3. ✅ Every `param` definition and its attributes (except `show`).
4. ✅ `impl` add/delete, `format`, `model`, `file`, and `body`. ✅ Port → pin sync and symbol from ports for a Verilog-A/Verilog/SPICE body (cell panel, plugin-abi §11 item 18). Open: impl rename and `ignore`.
5. Instance `inherit`, `ignore open|short`, and swapping `cellref`.
6. Label `global`.
7. Var name and `dist`; var bounds also gate a run.
8. ✅ `optimize`, `output`, `spec` (cell panel, #48). Open: `corner`, `config`, `sweep`, `montecarlo`, and editing `code`.

## Defects found on the way

- ✅ fixed by the cell panel's "+ Schematic" / "+ SPICE": **New files can't be saved.** File → New schematic/symbol creates an empty document. Save fails with E025: a `.shcomp` needs a schematic impl, a `.shprim` a pin plus an impl (crates/doc/src/project.rs:886). No UI adds an impl, so only the raw dialog can make one saveable.
- **Search-replace on `inherit` writes a single value** (search.rs:174), which turns `inherit <pin> <net>` into a malformed record.
