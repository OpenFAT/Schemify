# Plugin ABI v1 (normative draft — M2)

Implements D3, D3a, D6, D7, D8, D20, D27. Host: `wasmi` (native and web). Guest: any language compiling to `wasm32-unknown-unknown` (core module, no WASI, no component model). Designed with `api-design` (Muratori's five characteristics); evaluation in §9. Implementation: `crates/abi` (shared types + Rust guest SDK), `crates/plugin` (host), `plugins/` (example guests). Amendments made while implementing are listed in §11.

## 0. Usage first

What a plugin author writes (Rust, using the `schemify-abi` guest SDK, feature `guest`; this is `plugins/hello`):

```rust
use schemify_abi::guest::{self, Host};
use schemify_abi::{Event, Out, Table, col};

guest::export!(on_event);                 // emits the wasm exports of §2

fn on_event(ev: Event<'_>, host: &Host, out: &mut Out) {
    match ev {
        // Contributions are immediate-mode: re-declare the whole set whenever it changes.
        Event::Load | Event::DocChanged { .. } => {
            let nets = host.len(Table::Nets).unwrap_or(0);
            out.contribute_ui(&format!(
                "node tools.count_nets item\n  parent menu.tools\n  label \"Count nets ({nets})\"\n  command count_nets.run\n"
            ));
            out.contribute_commands("command count_nets.run \"Count nets\"\n");
        }
        Event::Command { name: "count_nets.run", .. } => {
            let nets = host.len(Table::Nets).unwrap_or(0);
            let pins = host.column::<u32>(Table::Nets, col::nets::PIN_COUNT, 0..nets).unwrap_or_default();
            let floating = pins.iter().filter(|&&c| c == 1).count();
            out.log(&format!("{floating} single-connection nets"));
        }
        _ => {}
    }
}
```

And the AI-sizing drill (D38), condensed: read `doc_vars`, `bench_specs`, `results` columns → `out.run(job)` → on `Event::RunDone` → `out.stage(doc, label, ops)` (a staged edit transaction the user reviews in the visual diff).

What the app spine writes (frame spine: `plugin::deliver(plugins, plugin_events, snapshots) → request_queues`):

```rust
events.push(None, &Event::DocChanged { doc, epoch })?;                   // broadcast
events.push(Some(owner), &Event::Command { name: "count_nets.run", args: "" })?;
plugin::deliver(&mut plugins, &events, &mut snapshots, &mut queues);   // never fails
// owners drain: queues.edits → doc, queues.runs → run, queues.timers / io / evals → app,
// queues.logs → log panel; plugins.composed() → ui / command table / expr registry
```

## 1. Model

- A plugin is an **event handler**. The host calls it; it reads what it needs through host imports (plain calls that return) and returns **outputs**: contributions (immediate-mode sets) and requests (one-shot). The plugin never holds the host's stack and the host never blocks on the plugin beyond a fuel budget.
- **Contributions** (replace semantics): each `contribute_*` call replaces that plugin's previous set of that kind (and scope); an empty set removes it. The host composes `plugin₁ ⊕ … ⊕ pluginₙ` in configured (= load) order (D8, last wins), records overrides in the health report, and drops a plugin's sets when it unloads. The owning crate applies its core base first and the composed plugin set on top, so core records are overridden the same way. Kinds: `ui` (nodes, styles), `commands`, `keymap`, `annotations` (per scope), `functions` (expression functions, D20), `dialects` (netlist langs), `runners` (code-block langs), `checks` (check codes + severities), `constraint_types`.
- **Requests** (one-shot, validated by the owning crate): `edit` (commit or stage transactions, edits spec §1), `run` (jobs), I/O and `eval` requests (§3, §5), `timer`, `log`.
- Text encoding for contributions and requests is the **line-record format** (format spec §2) — the same codec as documents and the journal, so UI contributions can also be written by hand in user config.
- Bulk reads (snapshots) are **binary columns** copied into guest memory on request (§3), because plugins may scan 10^5–10^6 rows.

## 2. Guest exports

| Export | Signature | Meaning |
|---|---|---|
| `schemify_abi` | `() -> u32` | ABI version (this spec: 1). Mismatch → not loaded, `E_VERSION`. |
| `schemify_alloc` | `(len: u32) -> u32` | Guest allocates `len` bytes for host-written input; returns ptr (0 = OOM → `E_SPACE`). The host calls it exactly once per `on_event`/`call`, so a guest may reuse one buffer (the SDK does). |
| `schemify_on_event` | `(ptr: u32, len: u32) -> u64` | Event record in; returns `(out_ptr << 32) \| out_len` of the output records; `0` = no outputs (`out_ptr == 0` with `out_len != 0` is `E_RANGE`). The host copies the output before the next call. |
| `schemify_call` | `(fn_id: u32, ptr: u32, len: u32) -> u64` | Optional. Pure expression function (D20). Args: one line of line-record values (canonical number text, quoted strings); result: one canonical value. `out_ptr == 0` means failure with the §8 code in `out_len`. |
| `memory` | linear memory | Exported, 32-bit, at most 256 MiB. |

Each `on_event` and each `call` runs with a **fuel budget** covering `schemify_alloc` plus the export (default 10^8; per-plugin override in config). Loading spends the same budget on the start function and `schemify_abi`. Exhaustion aborts the call (`E_FUEL`), discards its outputs **and** every request staged through imports during it, and logs to the health report. A trap (unreachable, out-of-bounds, stack overflow, failed allocation) is `E_TRAP` with the same discarding; the plugin stays loaded. Memory cap: 256 MiB per plugin (wasmi limiter; `memory.grow` past it returns −1). Output cap: 16 MiB per call (`E_SPACE`).

## 3. Host imports (module `schemify`)

All return immediately. Errors are negative `i32` codes from §8. Every import returns `-E_DENIED` (or does nothing, for `log`) outside `schemify_on_event`: in the start function, in `schemify_alloc` and in pure `schemify_call`.

| Import | Signature | Notes |
|---|---|---|
| `table_len` | `(table: u32) -> i32` | Row count of a snapshot table (§4). |
| `table_epoch` | `(table: u32) -> i32` | Changes whenever the table changes (31-bit, wraps); cheap staleness check. |
| `read_column` | `(table: u32, column: u32, start: u32, count: u32, dst: u32, dst_len: u32) -> i32` | Copies a fixed-width column slice (little-endian) into guest memory; returns bytes written. `E_COLUMN` for a string column. |
| `read_strings` | `(table: u32, column: u32, start: u32, count: u32, dst: u32, dst_len: u32) -> i32` | Copies `count` strings as `u32 len` (LE) + UTF-8 bytes each; returns bytes written. |
| `read_record` | `(doc: u32, id: u32, dst: u32, dst_len: u32) -> i32` | One document record as line-record text; `E_GONE` if absent. |
| `eval` | `(doc: u32, ptr: u32, len: u32) -> i32` | **Inverted** (amended): returns a handle; the value text arrives as `Event::IoDone { handle, status, bytes }`. `E_GONE` for a closed document. No capability needed. |
| `log` | `(level: u32, ptr: u32, len: u32)` | Level 0 error, 1 warn, 2 info, 3 debug; text truncated to 4 KiB. Queued only if the call completes. |
| capability imports | §5 | Present for every plugin; return `-E_DENIED` without a grant. |

All three `read_*` imports, on `-E_SPACE` with `dst_len >= 4`, write the needed size (`u32` LE) into the first 4 bytes of `dst`. A `dst` range outside guest memory is `E_RANGE`; rows past the table are `E_RANGE`.

Host work is metered: every import that moves bytes (reads, `eval`, `log`, capability imports) costs 1 fuel per 8 bytes on top of the instruction cost, so a loop of large reads exhausts the budget instead of keeping the host copying.

Granularity tiers: `read_column` (finest) ← SDK typed column readers (`Host::column::<T>`, `Host::strings`) ← row iterators (not yet in the SDK); `read_record` is the diagonal route for "just give me this object".

## 4. Snapshot tables

Tables are exposed **as the owner stores them** (data that has no reason to be opaque is transparent). Table ids are the row order below (`Docs` = 0 … `UiNodes` = 14; `BenchSpecs` = 10, `BenchOutputs` = 11); column ids are the order within a row (`Nets.pin_count` = 3). Ids and types are fixed per ABI version; new columns append only. The authoritative list is `schemify_abi::Table::columns` / `schemify_abi::col`.

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

At most 2^24 rows per table. `read_record` is served from a separate record table the app fills (sorted by `(doc, id)`). Object ids are the file's stable ids (30-bit), not memory handles, so they never go stale across edits; a deleted id simply fails validation (`-E_GONE`) in a later request. The host itself checks only that an `edit`'s document is in `Docs`.

## 5. Capabilities (D7)

Declared in the plugin manifest (`plugin … grant <capability>`) and granted by the user at install. Absent grant → the import returns `-E_DENIED` and the health report records it (even if the event later aborts). An unknown grant string refuses the load (`E_PARSE`).

| Capability | Import |
|---|---|
| `read_project` | `fs_read(path: u32, path_len: u32) -> i32` |
| `write_project` | `fs_write(path: u32, path_len: u32, data: u32, data_len: u32) -> i32` |
| `spawn:<binary>` | `spawn(argv: u32, argv_len: u32, stdin: u32, stdin_len: u32) -> i32` — `argv` is one line of line-record values; `argv[0]` must equal a granted binary name exactly (grants never contain `/` or `\`); resolved on PATH by the app; native only |
| `network` | `fetch(url: u32, url_len: u32, body: u32, body_len: u32) -> i32` |
| `clipboard` | `clipboard_get() -> i32`, `clipboard_set(ptr: u32, len: u32) -> i32` |

Paths are project-relative: no leading `/`, no `\` or `:`, no empty, `.` or `..` segment; anything else is `E_DENIED`. Path/URL/argv at most 8 KiB, data at most 64 MiB (`E_SPACE`).

I/O is **inverted**: each import returns a positive handle (unique per plugin, never reused) immediately and appends a row to the host's `io` request table, which the app executes; completion arrives later as `Event::IoDone { handle, status, bytes }` (`status >= 0` success — the exit status for `spawn` — or a negated §8 code). Requests staged by an event that aborts are discarded and never complete. The host crate itself performs no I/O and builds for `wasm32-unknown-unknown`.

## 6. Events

`Load`, `Unload`, `Command { name, args }`, `DocOpened/DocClosed { doc }`, `DocChanged { doc, epoch }`, `SelectionChanged`, `NetlistRequest { dialect, bench }` (dialect/runner plugins), `RunJob { job, lang }` (runner plugins), `RunDone { job }`, `IoDone { handle, status, bytes }`, `Timer { id }`, `Saved { doc }`, `Rejected { code, record }`.

Wire form: one line record `event <kind> <values…>` + LF, kinds in snake case (`doc_changed`, `io_done`). `io_done` and `rejected` append one NUL byte and a raw binary payload (`bytes`, the rejected record); the record text never contains NUL. Identifier fields (`name`, `dialect`, `bench`, `lang`) are bare values; `args` is the remaining line-record values; `code` is the §8 name (`E_PARSE`).

`Load` is queued at load and delivered by the next `deliver`; `Unload` is delivered by `unload` itself, and contributions made during it are dropped. Broadcast events reach every plugin in composition order; `Command` is targeted at the command's owner (from the composed `commands` set). An event targeted at an unloaded plugin is an `E_GONE` health entry, never misdelivered.

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
contribute functions
  | function gmid.width 0 3        # <name> <fn_id passed to schemify_call> <arity>
edit <doc> commit|stage <label>
  | <op lines per edits spec §1>   # at least one
run <bench> <config> <corner>
  vary <var> <value>
timer <id> <ms>                    # ms <= 86_400_000
log <level> <text>                 # error | warn | info | debug
```

Legal record words inside a contribution, per kind: `ui` → `node style remove set`; `commands` → `command`; `keymap` → `key`; `annotations` → `mark`; `functions` → `function` (exact form above, name a word); `dialects` → `dialect`; `runners` → `runner`; `checks` → `check`; `constraint_types` → `constraint`. Anything else rejects the whole contribution (`E_PARSE`), keeping the previous set.

Composition key of a contributed record: its word and first value (`node tools.x`, `command x.run`); `set <node> <prop> <value>` is keyed by node and prop; `remove X` shares `node X`'s key, so a later plugin's `remove` overrides an earlier `node` and vice versa. A surviving record keeps the position where its key first appeared and the text of the last plugin. UI node kinds are the core widget enum (ARCHITECTURE.md `ui`); core node ids are stable public names (`menu.file`, `file.save`, …). Known ceiling: an earlier plugin's `set X p` still applies after a later plugin redefines `node X` (different keys).

## 8. Errors

`E_DENIED 1, E_GONE 2, E_SPACE 3, E_TABLE 4, E_COLUMN 5, E_RANGE 6, E_PARSE 7, E_FUEL 8, E_VERSION 9, E_TRAP 10, E_MODULE 11`. `E_TRAP`: the guest trapped. `E_MODULE`: invalid wasm, unknown import, missing or mistyped export, initial memory over the cap.

Rejected outputs (parse errors, validation failures, full queues) never abort the rest: each output record is accepted or rejected individually and rejections go to the health report and back to the plugin as `Event::Rejected { code, record }` (record truncated to 256 bytes) at the next `deliver`. Rejections caused while handling a `Rejected` event are recorded but not echoed again; at most 64 host events wait per plugin, the rest are dropped with an `E_SPACE` health entry.

## 9. Evaluation (api-design checklist)

| Axis | Choice |
|---|---|
| Granularity | column slice → typed column → (row iterator) → `read_record`; outputs per record. Host: `deliver` = run guest + `intake`; both public, so outputs can be fed without wasm. |
| Redundancy | diagonal: `read_record` beside raw columns, `DocVars.value` beside `eval`; UI as records or SDK builders (`contribute_ui`). |
| Coupling | no hidden state: every read names its table; contributions are per-kind; capabilities are independent. |
| Retention | contributions are immediate-mode (re-declare the full set; the host diffs and ignores identical re-declarations). Documents are the only retained state, and that's the user's data. |
| Flow control | host → plugin only via events; plugin → host only via returning imports; all I/O and `eval` inverted to handles + events. No callbacks into the plugin except pure `schemify_call`. |
| Discontinuity watch | huge scans (10^6 rows) cost a copy into guest memory. If that becomes the wall, add a zero-copy shared-memory tier (`map_column`) without changing existing imports. |

## 10. Host tables and capacities

| Table (owner `plugin`) | Capacity | Notes |
|---|---|---|
| plugins (+ grants, stores) | 255 | Row order = composition order; ids never reused. |
| contributions | 4096 sets, 1 MiB each | Existence-based: an empty set has no row. |
| request queues: edits / runs / timers / logs / io / evals | 1024 / 256 / 256 / 1024 / 256 / 256 per frame | Owners drain them; a full queue rejects with `E_SPACE` (imports refuse before issuing a handle). |
| plugin events | 4096 per `deliver`, 64 MiB per event | |
| health faults | 1024 (ring, dropped count kept) | Plus the overrides of the current composition. |

## 11. Amendments (WS6 implementation)

1. `E_TRAP 10` and `E_MODULE 11` added (§8).
2. `eval` is inverted: `eval(doc, ptr, len) -> handle`, value via `IoDone` (§3). Why: the host crate cannot depend on `expr`/`elab` (dependency direction) and scopes are `elab`-owned; `DocVars.value` covers the synchronous common case.
3. Capability import signatures fixed (§5); `spawn` argv is line-record values; path rules made explicit.
4. Exports: `0` packed return conventions, `schemify_call` optional, one `schemify_alloc` per call, fuel covers alloc and load (§2). Imports are denied outside `on_event` (§3).
5. `read_*` all report the needed size on `E_SPACE`; `table_epoch` is 31-bit; imports are charged 1 fuel per 8 bytes moved (§3).
6. Event wire form and NUL payload for `io_done` / `rejected`; `Rejected` added to the event list; echo rules (§6, §8).
7. `log` output record, `functions` record form, per-kind record words, composition keys (`set`, `remove` aliasing), first-appearance order (§7).
8. The host composes plugin sets only; the owning crate applies its core base first (§1).
9. §0 usage: `host.len` returns `Result`; typed reads are `Host::column::<T>(table, column, rows)`.
10. (M2 editor) UI composition order is core base ⊕ plugin₁ … pluginₙ ⊕ app state. The core base (a `window` holding the `canvas`) applies only when no plugin declares a `window` node: a re-declared node keeps the position where it first appeared, so a base canvas would sit ahead of every plugin bar. The app projects session state (tab titles, status fields, dialog rows) as a last contribution of `set` records and child nodes under plugin-declared containers (contract nodes listed in `plugins/default-ui/src/lib.rs`); its overrides are not reported as plugin health.
