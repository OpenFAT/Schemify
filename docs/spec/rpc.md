# Command stream: JSON-RPC (normative)

Implements D27's "one JSON-RPC command stream (stdio/socket) over the same command table the UI and plugins use". Two servers speak it:

- **`schemify rpc [--listen <socket>]`**, the headless core: no editor, no window. Its methods are the CLI's commands and a stateless document edit (§3.1).
- **`schemify-gui --rpc <socket>`** (also through `schemify edit … --rpc <socket>`): the running editor. Its methods are the editor's command table: every core command and every plugin command, exactly what menus, keys, Lua keymaps and plugins name (§3.2).

Implementation: `crates/cli/src/rpc.rs` (codec, framing, sockets, the headless server), `crates/app/src/rpc.rs` (the editor's server). No JSON dependency: the codec is a few hundred lines with its own oracles (§8).

## 1. Transport and framing

- **stdio** (headless default): requests on stdin, responses on stdout. The server exits 0 when stdin ends; stderr carries diagnostics only.
- **Local socket**: `<socket>` is a socket address (`127.0.0.1:7777`, `[::1]:7777`) for TCP, which must be loopback so nothing off the machine can drive the tools (D7); anything else is a Unix-domain socket path (Unix only; Windows uses the TCP form). A stale socket file nobody listens on is replaced; any other existing file is never touched (the bind fails, exit 2). The headless server serves one client after another; the editor serves up to 16 at once (more are closed on accept).
- **Framing:** one JSON text per line (UTF-8, LF; a CR before the LF is dropped; blank lines are ignored). The server writes compact JSON, never a raw line break inside a message. A line longer than 16 MiB is consumed and answered with `-32700`.

## 2. Messages

JSON-RPC 2.0 (`jsonrpc` is `"2.0"`):

```json
{"jsonrpc":"2.0","id":1,"method":"fmt","params":["--check","top.shcomp"]}
{"jsonrpc":"2.0","id":1,"result":{"code":0,"stdout":"","stderr":""}}
{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"unknown method `nope`"}}
```

- `id`: a number, a string or `null`; it is echoed. A request without `id` is a **notification**: it runs and nothing is answered.
- `params`: an array or an object (absent = `[]`). Arguments are data: strings, never code.
- Batches (a JSON array of requests) are not supported: `-32600`.
- Responses come in request order per client (the editor answers a command after the frame that ran it, §3.2).
- JSON numbers are finite doubles; a request with `1e999` does not parse. Object member order is kept; duplicate names are allowed and the first one is read.

## 3. Methods

Every server answers `rpc.methods`: the sorted list of method names it serves (the headless list; for the editor `rpc.methods`, then the core commands, then the plugin commands).

### 3.1 Headless (`schemify rpc`)

| Method | Params | Result |
|---|---|---|
| `fmt`, `diff`, `netlist`, `check`, `run` | the CLI arguments after the command, as an array of strings (`["top.shbench", "--dialect", "xyce"]`) | `{"code", "stdout", "stderr"}`: the exit code (D27: 0 ok, 1 design errors, 2 usage/I/O; −1 killed) and the output text (lossy UTF-8) |
| `edit` | `{"file": <path>, "ops": <op text>, "label"?: <undo label>}` | `{"changed": bool}` |
| `rpc.methods` | none | the method names |

- A CLI method runs the same `schemify` binary as a child process, with the server's startup flags (`--clean`, `-u <init.lua>`) before the command and an empty stdin, in the server's working directory. A crash or hang of one spine is the child's: the stream goes on (D27, "xschem hangs on script errors").
- `edit` opens `file` in its project (as `netlist` does), applies `ops` (edits spec §1 op text, one record per op) as one committed transaction and writes the canonical file atomically when it changed. It keeps no state between calls and writes no journal (the file is the record; undo is another `edit` or the VCS). Ops that do not parse or validate, or a document refused by `doc`, answer `1` with the op errors; a file that cannot be read or written answers `2`.

### 3.2 Editor (`schemify-gui --rpc`)

- A method is a **command name** from the command table (`commands.rs`, `Commands::resolve_arg`): a core command (`edit.undo`, `file.save`, `view.zoom_full`, …), a plugin command (`count_nets.run`), or a core command with its argument as a name suffix (`layers.set_current.3`, `sim.dialect.xyce`). Anything else is `-32601`.
- `params`: an array of strings, passed to the command as its arguments in line-record value form (format spec §2: bare when legal, else quoted), the same text a menu row's or key's `args` carries; `-32602` for anything else. A plugin command receives them as `Event::Command { args }`.
- The command is queued (`Editor::command`) between frames and runs in the next frame, after that frame's input, exactly as a key press would. The response comes after that frame: `{"status": <the status line>}`. At most 256 requests are taken per frame; the rest wait for the next one.
- While the server runs the editor ticks every 50 ms to look for requests (so an idle editor answers within that); without `--rpc` nothing changes (D39 idle CPU 0%).

## 4. Errors

| Code | Meaning |
|---|---|
| −32700 | The line is not UTF-8 JSON, or is over 16 MiB (`id` is `null`). |
| −32600 | Not a JSON-RPC 2.0 request object (no `"jsonrpc":"2.0"`, `method` not a string, `params` not an array/object, `id` not a number/string/null, a batch). |
| −32601 | No such method or command. |
| −32602 | Params of the wrong shape. |
| −32603 | The server cannot run the method (no process support for a CLI method, e.g. under WASI). |
| 1 | The design was refused (`edit`: ops, validation, kind rules), the CLI's exit code 1. |
| 2 | A file could not be read or written, the CLI's exit code 2. |

`message` is human-readable text. A malformed line never stops the stream: it gets its error and the next line is read.

## 5. Notifications (editor)

Every editor event the plugin host and the user config see is sent to every connected client as a notification, after the responses of the frame that raised it:

```json
{"jsonrpc":"2.0","method":"event","params":{"kind":"doc_changed","values":["65536","7"]}}
```

`kind` and `values` are the plugin event's wire form (plugin-abi §6): `doc_opened <doc>`, `doc_closed <doc>`, `doc_changed <doc> <epoch>`, `selection_changed`, `saved <doc>`, `run_done <job>`, values as strings. Document ids are plugin ids (`Docs.handle`). The editor keeps the last 256 events; a client that stops reading for 200 ms is disconnected. The headless server sends no notifications.

## 6. Security

Arguments are data; no method evaluates code. The socket is local (loopback TCP or a Unix socket file, whose permissions are the OS's). Anyone who can connect can run commands the user could run, including CLI runs that start simulators (`run`): do not expose the socket to other users.

## 7. MCP bridge (`schemify mcp`)

D27's "MCP = thin plugin": a Model Context Protocol server on stdio, so an agent can call the headless methods as tools. Same framing (§1) and codec; it adds no method of its own. It lives in the core binary, not in a wasm plugin, because a plugin owns no stdio (plugin-abi §5).

- `initialize` → `{"protocolVersion", "capabilities": {"tools": {}}, "serverInfo": {"name": "schemify", "version"}}`; the client's `protocolVersion` is echoed when it is one of `2024-11-05`, `2025-03-26`, `2025-06-18`, else `2025-06-18`. `ping` → `{}`. Notifications (`notifications/initialized`, …) are accepted silently. Other methods: `-32601`.
- `tools/list` → one tool per headless method except `rpc.methods` (§3.1), in that order: the CLI tools take `{"args": [<string>…]}`, `edit` takes `{"file", "ops", "label"?}` (JSON Schema `inputSchema` for each).
- `tools/call {"name", "arguments"}` → `{"content": [{"type": "text", "text"}], "isError"}`: a CLI tool's text is `exit code <n>`, a line break, then its stdout and stderr; `edit`'s is its JSON result. `isError` is true when the method failed or the command exited non-zero. An unknown tool or a call without `name` is `-32602`.

## 8. Oracles (TESTING.md `rpc`)

- The codec: `parse(write(v)) == v` for generated values (property); RFC 8259 acceptance and rejection cases; any bytes decode to a request or a declared `-32700`/`-32600` response, never a panic (property, and the `cli/fuzz` `rpc_decode` target, nightly).
- Headless: every method of `rpc.methods` is reachable; CLI methods return the CLI's own exit code and output for the same arguments; `edit` changes exactly the edited attribute (`schemify diff` as oracle) and refuses invalid ops without touching the file; garbage lines do not stop the stream; the Unix-socket listener answers like stdio.
- Editor: every command `rpc.methods` lists (core and plugin, suffix arguments) is queued, unknown names and bad params are declared errors; a client on a socket drives the frame spine (select all, delete) and gets its answers and the `doc_changed` notification.
- MCP: the lifecycle (version negotiation, the initialized notification unanswered), `tools/list` equals the headless methods minus `rpc.methods`, a failing tool is a result with `isError`, unknown tools and methods are errors.
