# SDK conformance guest

Every language library in this directory has a `conformance/` guest that behaves exactly as below. `cargo test -p schemify-plugin --test sdk` loads each committed `conformance/conformance.wasm` in the real host and checks it, so every library parses events and writes records the way the Rust SDK does. A new library adds its guest here and its directory to that test.

## On every event

The guest writes one `log info <text>` record, where `<text>` is:

| Event | Text |
|---|---|
| `load` | `load`, after contributing `commands` = `command conf.echo "Echo"`, `command conf.out "Out"`, `command conf.read "Read"` (three lines) and `functions` = `function conf.add 0 2`, `function conf.greet 1 1` |
| `unload`, `selection_changed` | the kind |
| `command <name> <args>` (except `conf.out`, `conf.read`) | `command <name>`, then ` [<value>]` per argument, unescaped; a malformed argument appends ` !parse` and stops |
| `doc_opened`, `doc_closed`, `saved` | `<kind> <doc>` |
| `doc_changed` | `doc_changed <doc> <epoch>` |
| `netlist_request` | `netlist_request <dialect> <bench>` |
| `run_job` | `run_job <job> <lang>` |
| `run_done` | `run_done <job>` |
| `timer` | `timer <id>` |
| `io_done` | `io_done <handle> <status> <bytes>` (the payload verbatim) |
| `rejected` | `rejected <numeric code> <record>` |

## `conf.out`

Writes, in order and nothing else:

1. `contribute annotations` with scope `erc pass` and body `mark R1 warn "hi"`
2. `edit` of doc 1, staged, label `size it`, ops `set_attr M1 w 2u`
3. `edit` of doc 1, committed, label `move`, ops `set_attr R1 at 10 20` and `set_attr R2 at 0 0` (two lines)
4. `run` bench `lib:tb`, config `default`, corner `tt`, varying `vdd` = `1.8` and `w` = `two words`
5. `timer` id 7 after 250 ms
6. `log warn` with text `q"\`, TAB, `x`, LF, `y`, U+0001
7. `log info done`

## `conf.read`

Reads the `nets` table and writes two logs:

1. `nets <n>:` then ` <name>=<pin_count>` per row
2. `errors <a> <b> <c>`: the positive codes of `table_len(99)`, of `read_column` on `nets.name` (a string column) and of `read_column` on `nets.pin_count` for one row past the end. Expected: `errors 4 5 6`.

## `schemify_call`

| fn_id | Args | Result |
|---|---|---|
| 0 (`conf.add`) | two decimal integers | their sum, decimal; `E_PARSE` for anything else |
| 1 (`conf.greet`) | one value | `hi <value>` written as one value (quoted) |
| other | | `E_RANGE` |
