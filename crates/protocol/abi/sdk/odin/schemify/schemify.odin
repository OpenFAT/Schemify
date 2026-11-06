// Package schemify: the Schemify plugin ABI v1 for Odin (docs/spec/plugin-abi.md).
//
// No allocator, no context: every procedure is "contextless", so it runs under the host's
// calls without an Odin context. The package itself exports `schemify_abi` and
// `schemify_alloc`; the plugin exports `schemify_on_event` (and, for `functions`,
// `schemify_call`) as one-liners that hand its handler to `on_event` (and `call`):
//
//     @(export)
//     schemify_on_event :: proc "contextless" (ptr, n: u32) -> u64 {
//         return schemify.on_event(ptr, n, handle)
//     }
//
// (Exports carry the handler because `@(init)` never runs without an entry point.) Build:
// odin build . -target:freestanding_wasm32 -no-entry-point -o:size
//
// Buffers are static: an event over INPUT_MAX bytes is refused (the host reports E_SPACE),
// and output past OUTPUT_MAX is replaced by one error log. Change either with
// -define:SCHEMIFY_INPUT_MAX=<bytes> (or SCHEMIFY_OUTPUT_MAX).
package schemify

// ---- ABI numbers, from crates/protocol/abi (cargo test -p schemify-abi --test sdk) ----
// BEGIN GENERATED
ABI_VERSION :: 1

// Error codes (ABI §8); imports return them negated.
Code :: enum u32 {
	Denied = 1,
	Gone = 2,
	Space = 3,
	Table = 4,
	Column = 5,
	Range = 6,
	Parse = 7,
	Fuel = 8,
	Version = 9,
	Trap = 10,
	Module = 11,
}
// Wire names of `Code`.
CODE_NAMES :: [Code]string {
	.Denied = "E_DENIED",
	.Gone = "E_GONE",
	.Space = "E_SPACE",
	.Table = "E_TABLE",
	.Column = "E_COLUMN",
	.Range = "E_RANGE",
	.Parse = "E_PARSE",
	.Fuel = "E_FUEL",
	.Version = "E_VERSION",
	.Trap = "E_TRAP",
	.Module = "E_MODULE",
}

// Snapshot tables (ABI §4).
Table :: enum u32 {
	Docs,
	Instances,
	Wires,
	Labels,
	Nets,
	Inst_Params,
	Paths,
	Name_Map,
	Violations,
	Doc_Vars,
	Bench_Specs,
	Bench_Outputs,
	Results,
	Selection,
	Ui_Nodes,
}

// Column ids per table: `u32(Nets_Col.Pin_Count)`.
Docs_Col :: enum u32 {
	Handle, // u32
	Kind, // u8
	Dirty, // u8
	Path, // str
	Cell, // str
}
Instances_Col :: enum u32 {
	Doc, // u32
	Id, // u32
	X, // i32
	Y, // i32
	Orient, // u8
	Cell, // str
	Name, // str
}
Wires_Col :: enum u32 {
	Doc, // u32
	Id, // u32
	X1, // i32
	Y1, // i32
	X2, // i32
	Y2, // i32
	Net, // u32
}
Labels_Col :: enum u32 {
	Doc, // u32
	Id, // u32
	X, // i32
	Y, // i32
	Net, // u32
	Text, // str
}
Nets_Col :: enum u32 {
	Name, // str
	Width, // u16
	Global, // u8
	Pin_Count, // u32
}
Inst_Params_Col :: enum u32 {
	Inst_Row, // u32
	Name, // str
	Value, // str
	Source, // u8
}
Paths_Col :: enum u32 {
	Path, // str
	Cell, // str
	Impl, // str
	Parent, // u32
}
Name_Map_Col :: enum u32 {
	Path, // str
	Emitted, // str
}
Violations_Col :: enum u32 {
	Code, // str
	Severity, // u8
	Object, // u32
	Param, // str
}
Doc_Vars_Col :: enum u32 {
	Name, // str
	Expr, // str
	Value, // str
}
Bench_Specs_Col :: enum u32 {
	Name, // str
	Expr, // str
	Min, // str
	Max, // str
}
Bench_Outputs_Col :: enum u32 {
	Name, // str
	Expr, // str
	Min, // str
	Max, // str
}
Results_Col :: enum u32 {
	Job, // u32
	Output, // str
	Value, // str
	Pass, // u8
}
Selection_Col :: enum u32 {
	Doc, // u32
	Id, // u32
}
Ui_Nodes_Col :: enum u32 {
	Id, // str
	Kind, // u8
	Parent, // str
	Owner, // u16
}

// Log levels.
Level :: enum u32 {
	Error,
	Warn,
	Info,
	Debug,
}

// Contribution kinds (ABI §1).
Kind :: enum {
	Ui,
	Commands,
	Keymap,
	Annotations,
	Functions,
	Dialects,
	Runners,
	Checks,
	Constraint_Types,
}
// Wire names of `Kind`.
KIND_NAMES :: [Kind]string {
	.Ui = "ui",
	.Commands = "commands",
	.Keymap = "keymap",
	.Annotations = "annotations",
	.Functions = "functions",
	.Dialects = "dialects",
	.Runners = "runners",
	.Checks = "checks",
	.Constraint_Types = "constraint_types",
}
// END GENERATED

// The success value of a `Call_Handler`; every other `Code` is a failure.
OK :: Code(0)

INPUT_MAX :: #config(SCHEMIFY_INPUT_MAX, 1 << 20)
OUTPUT_MAX :: #config(SCHEMIFY_OUTPUT_MAX, 1 << 20)

// ---- Host imports (ABI §3, §5). Negative returns are -Code. ----

foreign import host "schemify"

@(default_calling_convention = "contextless")
foreign host {
	// Rows in `table` (a `Table`, or any number: unknown ones are -E_TABLE).
	table_len :: proc(table: Table) -> i32 ---
	// Bumps whenever `table`'s rows change; equal epochs mean an unchanged table.
	table_epoch :: proc(table: Table) -> i32 ---
	// Returns a handle for the `io_done` that carries the clipboard text.
	clipboard_get :: proc() -> i32 ---
	@(private, link_name = "read_column")
	raw_read_column :: proc(table: Table, column, start, count: u32, dst: rawptr, dst_len: u32) -> i32 ---
	@(private, link_name = "read_strings")
	raw_read_strings :: proc(table: Table, column, start, count: u32, dst: rawptr, dst_len: u32) -> i32 ---
	@(private, link_name = "eval")
	raw_eval :: proc(doc: u32, expr: [^]u8, len: u32) -> i32 ---
	@(private, link_name = "log")
	raw_log :: proc(level: Level, text: [^]u8, len: u32) ---
	@(private, link_name = "fs_read")
	raw_fs_read :: proc(path: [^]u8, len: u32) -> i32 ---
	@(private, link_name = "fs_list")
	raw_fs_list :: proc(path: [^]u8, len: u32) -> i32 ---
	@(private, link_name = "fs_write")
	raw_fs_write :: proc(path: [^]u8, path_len: u32, data: [^]u8, data_len: u32) -> i32 ---
	@(private, link_name = "fs_remove")
	raw_fs_remove :: proc(path: [^]u8, len: u32) -> i32 ---
	@(private, link_name = "spawn")
	raw_spawn :: proc(argv: [^]u8, argv_len: u32, stdin: [^]u8, stdin_len: u32) -> i32 ---
	@(private, link_name = "fetch")
	raw_fetch :: proc(url: [^]u8, url_len: u32, body: [^]u8, body_len: u32) -> i32 ---
	@(private, link_name = "clipboard_set")
	raw_clipboard_set :: proc(text: [^]u8, len: u32) -> i32 ---
}

// Copies rows start..start+count of a fixed-width column (little-endian) into `dst`, whose
// element type must match the column's (`u32` for `Nets_Col.Pin_Count`); returns bytes
// written. -E_COLUMN for a string column, -E_RANGE for rows past the table or a short `dst`.
read_column :: proc "contextless" (table: Table, column: u32, start, count: u32, dst: []$T) -> i32 {
	return raw_read_column(table, column, start, count, raw_data(dst), u32(len(dst) * size_of(T)))
}

// Copies `count` strings as u32 length + UTF-8 bytes each into `dst`; walk them with
// `strings_next`. On -E_SPACE the needed size is in the first 4 bytes of `dst`.
read_strings :: proc "contextless" (table: Table, column: u32, start, count: u32, dst: []u8) -> i32 {
	return raw_read_strings(table, column, start, count, raw_data(dst), u32(len(dst)))
}

// Inverted: returns a handle; the value arrives as an `io_done` event.
eval :: proc "contextless" (doc: u32, expr: string) -> i32 {
	return raw_eval(doc, raw_data(expr), u32(len(expr)))
}

// Logs at once, bypassing the output buffer: survives an event that later traps.
host_log :: proc "contextless" (level: Level, text: string) {
	raw_log(level, raw_data(text), u32(len(text)))
}

// Capability imports (grant them in schemify.project); each returns a handle for `io_done`.
// Paths are project-relative; anything else is -E_DENIED.
fs_read :: proc "contextless" (path: string) -> i32 {
	return raw_fs_read(raw_data(path), u32(len(path)))
}
// `io_done` bytes: one path relative to `path` per line, recursive.
fs_list :: proc "contextless" (path: string) -> i32 {
	return raw_fs_list(raw_data(path), u32(len(path)))
}
fs_write :: proc "contextless" (path: string, data: []u8) -> i32 {
	return raw_fs_write(raw_data(path), u32(len(path)), raw_data(data), u32(len(data)))
}
fs_remove :: proc "contextless" (path: string) -> i32 {
	return raw_fs_remove(raw_data(path), u32(len(path)))
}
// `argv` is one line of line-record values (write it with `put_value`); `io_done` status is
// the exit status, bytes are stdout then stderr.
spawn :: proc "contextless" (argv: string, stdin: []u8) -> i32 {
	return raw_spawn(raw_data(argv), u32(len(argv)), raw_data(stdin), u32(len(stdin)))
}
fetch :: proc "contextless" (url: string, body: []u8) -> i32 {
	return raw_fetch(raw_data(url), u32(len(url)), raw_data(body), u32(len(body)))
}
clipboard_set :: proc "contextless" (text: string) -> i32 {
	return raw_clipboard_set(raw_data(text), u32(len(text)))
}

// Next string of a `read_strings` buffer, advancing `rest` past it (start with
// `buf[:written]`); false when none is left or the buffer is truncated. The string borrows
// the buffer.
strings_next :: proc "contextless" (rest: ^[]u8) -> (s: string, ok: bool) {
	b := rest^
	if len(b) < 4 do return
	n := int(b[0]) | int(b[1]) << 8 | int(b[2]) << 16 | int(b[3]) << 24
	if n > len(b) - 4 do return
	rest^ = b[4 + n:]
	return string(b[4:4 + n]), true
}

// ---- Events (ABI §6) ----

Event_Kind :: enum u8 {
	Load,
	Unload,
	Command,
	Doc_Opened,
	Doc_Closed,
	Doc_Changed,
	Selection_Changed,
	Netlist_Request,
	Run_Job,
	Run_Done,
	Io_Done,
	Timer,
	Saved,
	Rejected,
}

// One event; the fields its kind uses are set, the rest are zero. Strings borrow the input
// buffer, which lives until the handler returns.
Event :: struct {
	kind:    Event_Kind,
	name:    string, // command
	args:    string, // command: line-record values, walk with `values_next`
	dialect: string, // netlist_request
	bench:   string, // netlist_request
	lang:    string, // run_job
	doc:     u32, // doc_opened, doc_closed, doc_changed, saved
	epoch:   u32, // doc_changed
	job:     u32, // run_job, run_done
	handle:  u32, // io_done
	status:  i32, // io_done: >= 0 success (exit status for spawn), < 0 -Code
	id:      u32, // timer
	code:    Code, // rejected
	bytes:   string, // io_done payload, rejected record
}

@(private)
is_space :: proc "contextless" (c: u8) -> bool {return c == ' ' || c == '\t'}

// A bare-value character (format §2): not whitespace, quote, backslash or control.
@(private)
is_bare_char :: proc "contextless" (c: u8) -> bool {return c > 0x20 && c != 0x7f && c != '"' && c != '\\'}

@(private)
is_bare :: proc "contextless" (t: string) -> bool {
	if len(t) == 0 || t[0] == '#' || t[0] == '|' do return false
	for i in 0 ..< len(t) do if !is_bare_char(t[i]) do return false
	return true
}

// Next whitespace-separated token of `rest` (bare text only: identifiers and numbers).
@(private)
token :: proc "contextless" (rest: ^string) -> string {
	s := rest^
	i := 0
	for i < len(s) && is_space(s[i]) do i += 1
	start := i
	for i < len(s) && !is_space(s[i]) do i += 1
	rest^ = s[i:]
	return s[start:i]
}

@(private)
num :: proc "contextless" (t: string, lo, hi: i64) -> (v: i64, ok: bool) {
	neg := len(t) > 0 && t[0] == '-'
	i := int(neg)
	if i == len(t) do return
	n: i64
	for ; i < len(t); i += 1 {
		if t[i] < '0' || t[i] > '9' || n > 1 << 40 do return
		n = n * 10 + i64(t[i] - '0')
	}
	if neg do n = -n
	if n < lo || n > hi do return
	return n, true
}

@(private)
read_u32 :: proc "contextless" (rest: ^string, v: ^u32) -> bool {
	n, ok := num(token(rest), 0, 0xffffffff)
	v^ = u32(n)
	return ok
}

@(private)
read_ident :: proc "contextless" (rest: ^string, v: ^string) -> bool {
	v^ = token(rest)
	return is_bare(v^)
}

// Parses the wire form; false (E_PARSE) for anything but one well-formed event line, with a
// payload after a NUL exactly for io_done and rejected.
event_parse :: proc "contextless" (input: string) -> (ev: Event, ok: bool) {
	line := input
	payload := false
	for i in 0 ..< len(input) {
		if input[i] == 0 {
			line = input[:i]
			ev.bytes = input[i + 1:]
			payload = true
			break
		}
	}
	if len(line) > 0 && line[len(line) - 1] == '\n' do line = line[:len(line) - 1]
	for i in 0 ..< len(line) do if line[i] == '\n' || line[i] == '\r' do return
	rest := line
	if token(&rest) != "event" do return
	good := true
	wants_payload := false
	switch token(&rest) {
	case "load":
		ev.kind = .Load
	case "unload":
		ev.kind = .Unload
	case "selection_changed":
		ev.kind = .Selection_Changed
	case "command":
		ev.kind = .Command
		good = read_ident(&rest, &ev.name)
		for len(rest) > 0 && is_space(rest[0]) do rest = rest[1:]
		ev.args = rest
		rest = ""
	case "doc_opened":
		ev.kind = .Doc_Opened
		good = read_u32(&rest, &ev.doc)
	case "doc_closed":
		ev.kind = .Doc_Closed
		good = read_u32(&rest, &ev.doc)
	case "saved":
		ev.kind = .Saved
		good = read_u32(&rest, &ev.doc)
	case "doc_changed":
		ev.kind = .Doc_Changed
		good = read_u32(&rest, &ev.doc) && read_u32(&rest, &ev.epoch)
	case "netlist_request":
		ev.kind = .Netlist_Request
		good = read_ident(&rest, &ev.dialect) && read_ident(&rest, &ev.bench)
	case "run_job":
		ev.kind = .Run_Job
		good = read_u32(&rest, &ev.job) && read_ident(&rest, &ev.lang)
	case "run_done":
		ev.kind = .Run_Done
		good = read_u32(&rest, &ev.job)
	case "timer":
		ev.kind = .Timer
		good = read_u32(&rest, &ev.id)
	case "io_done":
		ev.kind = .Io_Done
		wants_payload = true
		status: i64
		good = read_u32(&rest, &ev.handle)
		if good do status, good = num(token(&rest), -0x80000000, 0x7fffffff)
		ev.status = i32(status)
	case "rejected":
		ev.kind = .Rejected
		wants_payload = true
		name := token(&rest)
		for wire, c in CODE_NAMES do if name == wire do ev.code = c
		good = ev.code != OK
	case:
		return
	}
	if !good || payload != wants_payload || token(&rest) != "" do return
	return ev, true
}

// ---- Line-record values (format spec §2): `bare` or "quoted \"escaped\"" ----

// A cursor over line-record values: `it := Values{rest = ev.args}`, then
// `for v in values_next(&it, buf[:]) {...}`; afterwards `it.malformed` tells a bad value
// from the end.
Values :: struct {
	rest:      string,
	malformed: bool, // set when values_next stopped on a malformed value (or one over dst)
}

@(private)
hex_digit :: proc "contextless" (c: u8) -> u32 {
	switch c {
	case '0' ..= '9': return u32(c - '0')
	case 'a' ..= 'f': return u32(c - 'a' + 10)
	case 'A' ..= 'F': return u32(c - 'A' + 10)
	}
	return 16
}

// The next value, unescaped into `dst` (the result borrows it); false at the end or on a
// malformed value, which also sets `it.malformed` and stops the cursor.
values_next :: proc "contextless" (it: ^Values, dst: []u8) -> (value: string, ok: bool) {
	s := it.rest
	i, n := 0, 0
	for i < len(s) && is_space(s[i]) do i += 1
	it.rest = "" // stop after an error
	if i == len(s) do return
	push :: proc "contextless" (dst: []u8, n: ^int, c: u32) -> bool {
		if n^ == len(dst) do return false
		dst[n^] = u8(c)
		n^ += 1
		return true
	}
	bad := false
	if s[i] == '"' {
		for i += 1; ; i += 1 {
			if i == len(s) || s[i] == '\n' || s[i] == '\r' {bad = true; break}
			if s[i] == '"' do break
			if s[i] != '\\' {
				if !push(dst, &n, u32(s[i])) {bad = true; break}
				continue
			}
			i += 1
			if i == len(s) {bad = true; break}
			e := s[i]
			ok := true
			switch {
			case e == '"' || e == '\\': ok = push(dst, &n, u32(e))
			case e == 'n': ok = push(dst, &n, '\n')
			case e == 't': ok = push(dst, &n, '\t')
			case e == 'u' && i + 1 < len(s) && s[i + 1] == '{':
				cp, digits: u32
				for i += 2; i < len(s) && s[i] != '}'; i += 1 {
					if digits == 6 || hex_digit(s[i]) == 16 {ok = false; break}
					cp = cp * 16 + hex_digit(s[i])
					digits += 1
				}
				if !ok || i == len(s) || digits == 0 || cp > 0x10ffff || (cp >= 0xd800 && cp < 0xe000) {
					ok = false
				} else if cp < 0x80 {
					ok = push(dst, &n, cp)
				} else if cp < 0x800 {
					ok = push(dst, &n, 0xc0 | cp >> 6) && push(dst, &n, 0x80 | cp & 0x3f)
				} else if cp < 0x10000 {
					ok = push(dst, &n, 0xe0 | cp >> 12) && push(dst, &n, 0x80 | cp >> 6 & 0x3f) && push(dst, &n, 0x80 | cp & 0x3f)
				} else {
					ok = push(dst, &n, 0xf0 | cp >> 18) && push(dst, &n, 0x80 | cp >> 12 & 0x3f) && push(dst, &n, 0x80 | cp >> 6 & 0x3f) && push(dst, &n, 0x80 | cp & 0x3f)
				}
			case: ok = false
			}
			if !ok {bad = true; break}
		}
		i += 1
	} else if s[i] == '#' || s[i] == '|' {
		bad = true
	} else {
		for ; i < len(s) && is_bare_char(s[i]); i += 1 {
			if !push(dst, &n, u32(s[i])) {bad = true; break}
		}
	}
	if bad || (i < len(s) && !is_space(s[i])) {
		it.malformed = true
		return
	}
	it.rest = s[i:]
	return string(dst[:n]), true
}

// ---- Output (ABI §7) ----

// An append-only text buffer: the event's output records, or any text you compose (a log
// message, an edit's ops) over your own `[N]u8`: `t := schemify.Out{buf = mem[:]}`. Once
// full, `overflow` is set and appends are dropped.
Out :: struct {
	buf:      []u8,
	len:      int,
	overflow: bool,
}

// What has been written so far; borrows `o.buf`.
out_str :: proc "contextless" (o: ^Out) -> string {return string(o.buf[:o.len])}

// Raw text.
put :: proc "contextless" (o: ^Out, s: string) {
	if o.overflow || len(s) > len(o.buf) - o.len {
		o.overflow = true
		return
	}
	copy(o.buf[o.len:], s)
	o.len += len(s)
}

// Decimal.
put_u32 :: proc "contextless" (o: ^Out, v: u32) {
	d: [10]u8
	k := len(d)
	for v := v; ; v /= 10 {
		k -= 1
		d[k] = '0' + u8(v % 10)
		if v < 10 do break
	}
	put(o, string(d[k:]))
}

// Decimal, `-` for negatives.
put_i32 :: proc "contextless" (o: ^Out, v: i32) {
	if v < 0 do put(o, "-")
	put_u32(o, v < 0 ? 0 - u32(v) : u32(v))
}

// One value: bare if legal, else quoted with \" \\ \n \t and other controls as \u{x}.
put_value :: proc "contextless" (o: ^Out, v: string) {
	if is_bare(v) {
		put(o, v)
		return
	}
	put(o, "\"")
	for i in 0 ..< len(v) {
		c := v[i]
		switch {
		case c == '"': put(o, "\\\"")
		case c == '\\': put(o, "\\\\")
		case c == '\n': put(o, "\\n")
		case c == '\t': put(o, "\\t")
		case c < 0x20 || c == 0x7f:
			e := [6]u8{'\\', 'u', '{', hex[c >> 4], hex[c & 15], '}'}
			if c < 0x10 { 	// no leading zero: \u{a}
				e[3] = hex[c]
				e[4] = '}'
				put(o, string(e[:5]))
			} else {
				put(o, string(e[:]))
			}
		case: put(o, v[i:i + 1])
		}
	}
	put(o, "\"")
}

// Constants cannot be indexed by a variable; these copies can.
@(private, rodata)
hex := "0123456789abcdef"
@(private, rodata)
kind_names := KIND_NAMES

// Each line of body as `  | line`; an empty line as `  |`.
@(private)
verbatim :: proc "contextless" (o: ^Out, body: string) {
	start := 0
	for i in 0 ..= len(body) {
		if i < len(body) && body[i] != '\n' do continue
		if i == len(body) && start == i do break
		line := body[start:i]
		if len(line) > 0 && line[len(line) - 1] == '\r' do line = line[:len(line) - 1]
		put(o, len(line) > 0 ? "  | " : "  |")
		put(o, line)
		put(o, "\n")
		start = i + 1
	}
}

// Records. A body is line-record text, one record per line; it is written verbatim.
// Contributions are immediate-mode: each one replaces the plugin's whole set of that kind.
contribute :: proc "contextless" (o: ^Out, kind: Kind, body: string) {
	put(o, "contribute ")
	put(o, kind_names[kind])
	put(o, "\n")
	verbatim(o, body)
}

// Like `contribute`, but replaces only the set under `scope` (e.g. one check's annotations).
contribute_scoped :: proc "contextless" (o: ^Out, kind: Kind, scope, body: string) {
	put(o, "contribute ")
	put(o, kind_names[kind])
	put(o, " ")
	put_value(o, scope)
	put(o, "\n")
	verbatim(o, body)
}

// An edit of `doc`: staged (previewed, not yet on the undo stack) when `stage`, else
// committed as one undo step named `label`.
edit :: proc "contextless" (o: ^Out, doc: u32, stage: bool, label, ops: string) {
	put(o, "edit ")
	put_u32(o, doc)
	put(o, stage ? " stage " : " commit ")
	put_value(o, label)
	put(o, "\n")
	verbatim(o, ops)
}

// A simulation run; its `run_done` comes back to this plugin. Follow with `vary` calls.
run :: proc "contextless" (o: ^Out, bench, config, corner: string) {
	put(o, "run ")
	put_value(o, bench)
	put(o, " ")
	put_value(o, config)
	put(o, " ")
	put_value(o, corner)
	put(o, "\n")
}

// One variable override of the `run` written just before.
vary :: proc "contextless" (o: ^Out, var, value: string) {
	put(o, "  vary ")
	put_value(o, var)
	put(o, " ")
	put_value(o, value)
	put(o, "\n")
}

// A `timer` event with `id` at the first frame `ms` milliseconds or more from now.
timer :: proc "contextless" (o: ^Out, id, ms: u32) {
	put(o, "timer ")
	put_u32(o, id)
	put(o, " ")
	put_u32(o, ms)
	put(o, "\n")
}

// A log record; dropped with the rest of the output if the event traps (see `host_log`).
log :: proc "contextless" (o: ^Out, level: Level, text: string) {
	NAMES := [4]string{"error", "warn", "info", "debug"}
	put(o, "log ")
	put(o, NAMES[u32(level) & 3])
	put(o, " ")
	put_value(o, text)
	put(o, "\n")
}

// ---- The exports (ABI §2) ----

// The plugin's event handler; `out` is the event's output.
Handler :: proc "contextless" (ev: Event, out: ^Out)
// A pure expression function `fn_id` (D20): `args` are line-record values; write one
// canonical value into `result` and return OK, or a Code. Every import is denied here.
Call_Handler :: proc "contextless" (fn_id: u32, args: string, result: ^Out) -> Code

@(private)
in_buf: [INPUT_MAX]u8
@(private)
out_buf: [OUTPUT_MAX]u8
@(private)
in_len: u32

@(private)
pack :: proc "contextless" (ptr: rawptr, n: int) -> u64 {return u64(uintptr(ptr)) << 32 | u64(n)}

@(export)
schemify_abi :: proc "contextless" () -> u32 {return ABI_VERSION}

@(export)
schemify_alloc :: proc "contextless" (n: u32) -> u32 {
	if n > INPUT_MAX do return 0 // the host reports E_SPACE
	in_len = n
	return u32(uintptr(&in_buf))
}

// The body of the plugin's `schemify_on_event` export: parses the event in the input
// buffer and runs `handler`; an unparsable event logs an error instead.
on_event :: proc "contextless" (ptr, n: u32, handler: Handler) -> u64 {
	if ptr != u32(uintptr(&in_buf)) || n != in_len do return 0
	out := Out{buf = out_buf[:]}
	if ev, ok := event_parse(string(in_buf[:n])); ok {
		handler(ev, &out)
	} else {
		log(&out, .Error, "unparsable event")
	}
	if out.overflow {
		out = Out{buf = out_buf[:]}
		log(&out, .Error, "output over SCHEMIFY_OUTPUT_MAX, dropped")
	}
	return out.len > 0 ? pack(&out_buf, out.len) : 0
}

// The body of the plugin's optional `schemify_call` export (for `functions`
// contributions): runs `handler` on the args; an empty or overflowing result is E_PARSE or
// E_SPACE.
call :: proc "contextless" (fn_id, ptr, n: u32, handler: Call_Handler) -> u64 {
	if ptr != u32(uintptr(&in_buf)) || n != in_len do return pack(nil, int(Code.Range))
	result := Out{buf = out_buf[:]}
	code := handler(fn_id, string(in_buf[:n]), &result)
	if code == OK && (result.overflow || result.len == 0) do code = result.overflow ? .Space : .Parse
	return code != OK ? pack(nil, int(code)) : pack(&out_buf, result.len)
}
