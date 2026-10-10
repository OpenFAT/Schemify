# Plugin ABI v1 (normative draft — M2)

Implements D3, D3a, D6, D7, D8, D20, D27. Host: `wasmi` (native and web). Guest: any language compiling to `wasm32-unknown-unknown` (core module, no WASI, no component model). Designed with `api-design` (Muratori's five characteristics); evaluation in §9.

## 0. Usage first

What a plugin author writes (Rust, using the `schemify-abi` guest SDK):

```rust
use schemify_abi::guest::{self, Event, Host, Out};

guest::export!(on_event);                 // emits the wasm exports of §2

fn on_event(ev: Event, host: &Host, out: &mut Out) {
    match ev {
        // Contributions are immediate-mode: re-declare the whole set whenever it changes.
        Event::Load | Event::DocChanged { .. } => {
            let nets = host.len(Table::Nets);
            out.contribute_ui(&format!(
                "node tools.count_nets item\n  parent menu.tools\n  label \"Count nets ({nets})\"\n  command count_nets.run\n"
            ));
            out.contribute_commands("command count_nets.run \"Count nets\"\n");
        }
        Event::Command { name: "count_nets.run", .. } => {
            let floating = host.column_u32(Table::NetPinCounts, 0..host.len(Table::Nets))
                .iter().filter(|&&c| c == 1).count();
            out.log(&format!("{floating} single-connection nets"));
        }
        _ => {}
    }
}
```

And the AI-sizing drill (D38), condensed: read `doc_vars`, `bench_specs`, `results` columns → `out.run(job)` → on `Event::RunDone` → `out.stage("tx …")` (a staged edit transaction the user reviews in the visual diff).

## 1. Model

- A plugin is an **event handler**. The host calls it; it reads what it needs through host imports (plain calls that return) and returns **outputs**: contributions (immediate-mode sets) and requests (one-shot). The plugin never holds the host's stack and the host never blocks on the plugin beyond a fuel budget.
- **Contributions** (replace semantics): each `contribute_*` call replaces that plugin's previous set of that kind. The host composes `core base ⊕ plugin₁ ⊕ … ⊕ pluginₙ` in configured order (D8, last wins), records overrides in the health report, and drops a plugin's sets when it unloads. Kinds: `ui` (nodes, styles), `commands`, `keymap`, `annotations` (per scope), `functions` (expression functions, D20), `dialects` (netlist langs), `runners` (code-block langs), `checks` (check codes + severities), `constraint_types`.
- **Requests** (one-shot, validated by the owning crate): `edit` (commit or stage transactions, edits spec §1), `run` (jobs), I/O requests behind capabilities (§5), `timer`, `log`.
- Text encoding for contributions and requests is the **line-record format** (format spec §2) — the same codec as documents and the journal, so UI contributions can also be written by hand in user config.
- Bulk reads (snapshots) are **binary columns** copied into guest memory on request (§3), because plugins may scan 10^5–10^6 rows.

## 2. Guest exports

| Export | Signature | Meaning |
|---|---|---|
| `schemify_abi` | `() -> u32` | ABI version (this spec: 1). Mismatch → not loaded, health error. |
| `schemify_alloc` | `(len: u32) -> u32` | Guest allocates `len` bytes for host-written input; returns ptr (0 = OOM). |
| `schemify_on_event` | `(ptr: u32, len: u32) -> u64` | Event record (line-record text) in; returns `(out_ptr << 32) \| out_len` of the output records. The host copies the output before the next call. |
| `schemify_call` | `(fn_id: u32, ptr: u32, len: u32) -> u64` | Pure expression function call (D20): args/result as canonical-text values. Must not emit outputs. |

Each `on_event` call runs with a **fuel budget** (default 10^8 instructions; per-plugin override in config). Exhaustion aborts the event, discards its outputs, and logs to the health report. Memory cap: 256 MiB per plugin (wasmi limiter).

## 3. Host imports (module `schemify`)

All return immediately. Errors are negative `i32` codes from §8.

| Import | Signature | Notes |
|---|---|---|
| `table_len` | `(table: u32) -> i32` | Row count of a snapshot table (§4). |
| `table_epoch` | `(table: u32) -> i32` | Changes whenever the table changes; cheap staleness check. |
| `read_column` | `(table: u32, column: u32, start: u32, count: u32, dst: u32, dst_len: u32) -> i32` | Copies a fixed-width column slice (little-endian) into guest memory; returns bytes written. |
| `read_strings` | `(table: u32, column: u32, start: u32, count: u32, dst: u32, dst_len: u32) -> i32` | Copies `count` strings as `u32 len` + bytes; returns bytes written or `-E_SPACE` with the needed size in the first 4 bytes. |
| `read_record` | `(doc: u32, id: u32, dst: u32, dst_len: u32) -> i32` | One document record as line-record text. |
| `eval` | `(doc: u32, ptr: u32, len: u32, dst: u32, dst_len: u32) -> i32` | Evaluates an expression in a cell scope (read-only). |
| `log` | `(level: u32, ptr: u32, len: u32)` | |
| capability imports | §5 | Present for every plugin; return `-E_DENIED` without a grant. |

Granularity tiers: `read_column` (finest) ← SDK typed column readers ← SDK row iterators; `read_record` is the diagonal route for "just give me this object".

## 4. Snapshot tables

Tables are exposed **as the owner stores them** (data that has no reason to be opaque is transparent). Column ids and types are fixed per ABI version; new columns append only.

| Table id | Rows | Columns (type) |
|---|---|---|
| `Docs` | open documents | handle u32, kind u8, dirty u8, path str, cell str |
| `Instances` | of the active document | doc u32, id u32, x i32, y i32, orient u8, cell str, name str |
| `Wires` | active document | doc u32, id u32, x1 i32, y1 i32, x2 i32, y2 i32, net u32 |
| `Labels` | active document | doc u32, id u32, x i32, y i32, net u32, text str |
| `Nets` | active document | name str, width u16, global u8, pin_count u32 |
| `InstParams` | active document | inst_row u32, name str, value str (canonical), source u8 (default/override/computed) |
| `Paths` | elaborated active bench | path str, cell str, impl str, parent u32 |
| `NameMap` | last netlist | path str, emitted str |
| `Violations` | active document | code str, severity u8, object u32, param str |
| `DocVars` | active cell | name str, expr str, value str |
| `BenchSpecs`, `BenchOutputs` | active bench | name str, expr str, min str, max str |
| `Results` | finished jobs | job u32, output str, value str, pass u8 |
| `Selection` | | doc u32, id u32 |
| `UiNodes` | composed UI | id str, kind u8, parent str, owner u16 |

Object ids are the file's stable ids (30-bit), not memory handles, so they never go stale across edits; a deleted id simply fails validation (`-E_GONE`) in a later request.

## 5. Capabilities (D7)

Declared in the plugin manifest (`plugin … grant <capability>`) and granted by the user at install. Absent grant → the import returns `-E_DENIED` and the health report records it.

| Capability | Imports |
|---|---|
| `read_project` | `fs_read(path…) -> handle` (project-relative paths only) |
| `write_project` | `fs_write(path…, bytes…) -> handle` |
| `spawn:<binary>` | `spawn(argv…, stdin…) -> handle` (exact binary name, resolved on PATH by the host; native only) |
| `network` | `fetch(url…, body…) -> handle` |
| `clipboard` | `clipboard_get / clipboard_set` |

I/O is **inverted**: each import returns a handle immediately; completion arrives later as `Event::IoDone { handle, status, bytes }`. No plugin ever blocks the host.

## 6. Events

`Load`, `Unload`, `Command { name, args }`, `DocOpened/DocClosed { doc }`, `DocChanged { doc, epoch }`, `SelectionChanged`, `NetlistRequest { dialect, bench }` (dialect/runner plugins), `RunJob { job, lang }` (runner plugins), `RunDone { job }`, `IoDone { … }`, `Timer { id }`, `Saved { doc }`. Each event is a line record (`event <kind>` + attrs).

## 7. Output records

```
contribute ui            # replaces this plugin's UI set; nested node/style records follow
  | node tools.count_nets item
  |   parent menu.tools
  |   label "Count nets"
  |   command count_nets.run
contribute commands
  | command count_nets.run "Count nets"
contribute annotations <scope>
  | mark <path-or-id> <severity> <text>
edit <doc> commit|stage <label>
  | <op lines per edits spec §1>
run <bench> <config> <corner>
  vary <var> <value>
timer <id> <ms>
```

UI node kinds are the core widget enum (ARCHITECTURE.md `ui`). A UI edit may `remove <node-id>`, `set <node-id> <prop> <value>` or add nodes; core node ids are stable public names (`menu.file`, `file.save`, …).

## 8. Errors

`E_DENIED 1, E_GONE 2, E_SPACE 3, E_TABLE 4, E_COLUMN 5, E_RANGE 6, E_PARSE 7, E_FUEL 8, E_VERSION 9`. Rejected outputs (parse errors, validation failures) never abort the rest: each output record is accepted or rejected individually and rejections go to the health report and back to the plugin as `Event::Rejected { record, code }`.

## 9. Evaluation (api-design checklist)

| Axis | Choice |
|---|---|
| Granularity | column slice → typed column → row iterator → `read_record`; outputs per record. |
| Redundancy | diagonal: `read_record`/`eval` beside raw columns; UI as records or SDK builders. |
| Coupling | no hidden state: every read names its table; contributions are per-kind; capabilities are independent. |
| Retention | contributions are immediate-mode (re-declare the full set; the host diffs). Documents are the only retained state, and that's the user's data. |
| Flow control | host → plugin only via events; plugin → host only via returning imports; all I/O inverted to handles + events. No callbacks into the plugin except pure `schemify_call`. |
| Discontinuity watch | huge scans (10^6 rows) cost a copy into guest memory. If that becomes the wall, add a zero-copy shared-memory tier (`map_column`) without changing existing imports. |
