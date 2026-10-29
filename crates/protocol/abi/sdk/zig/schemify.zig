//! schemify.zig: the Schemify plugin ABI v1 for Zig 0.16 (docs/spec/plugin-abi.md).
//!
//! One file, std only, no WASI. A plugin imports it and exports its glue:
//!
//!     const schemify = @import("schemify");
//!     comptime { schemify.exportPlugin(@This()); }
//!     pub fn onEvent(ev: schemify.Event, out: *schemify.Out) void { ... }
//!
//! Build an executable for wasm32-freestanding with `entry = .disabled` and
//! `rdynamic = true`; the module imports nothing but module "schemify".
//!
//! Buffers are static: an event over `input_max` bytes is refused (the host reports
//! E_SPACE), and output past `output_max` is replaced by one error log. A plugin changes
//! either by declaring `pub const schemify_input_max` / `schemify_output_max`.
const std = @import("std");

// ---- ABI numbers, from crates/protocol/abi (cargo test -p schemify-abi --test sdk) ----
// BEGIN GENERATED
pub const abi_version: u32 = 1;

/// Error codes (ABI §8); imports return them negated.
pub const Code = enum(u32) {
    denied = 1,
    gone = 2,
    space = 3,
    table = 4,
    column = 5,
    range = 6,
    parse = 7,
    fuel = 8,
    version = 9,
    trap = 10,
    module = 11,
};
/// Wire names of `Code`, indexed by `code - 1`.
pub const code_names = [_][]const u8{
    "E_DENIED",
    "E_GONE",
    "E_SPACE",
    "E_TABLE",
    "E_COLUMN",
    "E_RANGE",
    "E_PARSE",
    "E_FUEL",
    "E_VERSION",
    "E_TRAP",
    "E_MODULE",
};

/// Snapshot tables (ABI §4).
pub const Table = enum(u32) {
    docs,
    instances,
    wires,
    labels,
    nets,
    inst_params,
    paths,
    name_map,
    violations,
    doc_vars,
    bench_specs,
    bench_outputs,
    results,
    selection,
    ui_nodes,
};

/// Column ids per table: `col.nets.pin_count`.
pub const col = struct {
    pub const docs = struct {
        pub const handle: u32 = 0; // u32
        pub const kind: u32 = 1; // u8
        pub const dirty: u32 = 2; // u8
        pub const path: u32 = 3; // str
        pub const cell: u32 = 4; // str
    };
    pub const instances = struct {
        pub const doc: u32 = 0; // u32
        pub const id: u32 = 1; // u32
        pub const x: u32 = 2; // i32
        pub const y: u32 = 3; // i32
        pub const orient: u32 = 4; // u8
        pub const cell: u32 = 5; // str
        pub const name: u32 = 6; // str
    };
    pub const wires = struct {
        pub const doc: u32 = 0; // u32
        pub const id: u32 = 1; // u32
        pub const x1: u32 = 2; // i32
        pub const y1: u32 = 3; // i32
        pub const x2: u32 = 4; // i32
        pub const y2: u32 = 5; // i32
        pub const net: u32 = 6; // u32
    };
    pub const labels = struct {
        pub const doc: u32 = 0; // u32
        pub const id: u32 = 1; // u32
        pub const x: u32 = 2; // i32
        pub const y: u32 = 3; // i32
        pub const net: u32 = 4; // u32
        pub const text: u32 = 5; // str
    };
    pub const nets = struct {
        pub const name: u32 = 0; // str
        pub const width: u32 = 1; // u16
        pub const global: u32 = 2; // u8
        pub const pin_count: u32 = 3; // u32
    };
    pub const inst_params = struct {
        pub const inst_row: u32 = 0; // u32
        pub const name: u32 = 1; // str
        pub const value: u32 = 2; // str
        pub const source: u32 = 3; // u8
    };
    pub const paths = struct {
        pub const path: u32 = 0; // str
        pub const cell: u32 = 1; // str
        pub const impl: u32 = 2; // str
        pub const parent: u32 = 3; // u32
    };
    pub const name_map = struct {
        pub const path: u32 = 0; // str
        pub const emitted: u32 = 1; // str
    };
    pub const violations = struct {
        pub const code: u32 = 0; // str
        pub const severity: u32 = 1; // u8
        pub const object: u32 = 2; // u32
        pub const param: u32 = 3; // str
    };
    pub const doc_vars = struct {
        pub const name: u32 = 0; // str
        pub const expr: u32 = 1; // str
        pub const value: u32 = 2; // str
    };
    pub const bench_specs = struct {
        pub const name: u32 = 0; // str
        pub const expr: u32 = 1; // str
        pub const min: u32 = 2; // str
        pub const max: u32 = 3; // str
    };
    pub const bench_outputs = struct {
        pub const name: u32 = 0; // str
        pub const expr: u32 = 1; // str
        pub const min: u32 = 2; // str
        pub const max: u32 = 3; // str
    };
    pub const results = struct {
        pub const job: u32 = 0; // u32
        pub const output: u32 = 1; // str
        pub const value: u32 = 2; // str
        pub const pass: u32 = 3; // u8
    };
    pub const selection = struct {
        pub const doc: u32 = 0; // u32
        pub const id: u32 = 1; // u32
    };
    pub const ui_nodes = struct {
        pub const id: u32 = 0; // str
        pub const kind: u32 = 1; // u8
        pub const parent: u32 = 2; // str
        pub const owner: u32 = 3; // u16
    };
};

/// Log levels.
pub const Level = enum(u32) {
    @"error",
    warn,
    info,
    debug,
};

/// Contribution kinds (ABI §1); `@tagName` is the wire name.
pub const Kind = enum {
    ui,
    commands,
    keymap,
    annotations,
    functions,
    dialects,
    runners,
    checks,
    constraint_types,
};
// END GENERATED

// ---- Errors ----

/// A failed import or call; `codeOf` gives the ABI number the host saw.
pub const Error = error{ Denied, Gone, Space, Table, Column, Range, Parse, Fuel, Version, Trap, Module };

/// `Error` by `Code` value minus one, in `code_names` order.
const errors = [_]Error{
    error.Denied, error.Gone, error.Space,   error.Table, error.Column, error.Range,
    error.Parse,  error.Fuel, error.Version, error.Trap,  error.Module,
};

/// The ABI code of `err`, for printing or returning to the host.
pub fn codeOf(err: Error) Code {
    for (errors, 1..) |e, c| if (e == err) return @enumFromInt(c);
    unreachable;
}

/// An import's return value: >= 0 is the result, < 0 is a negated code. A code this
/// library does not know (a newer host) comes back as `error.Version`.
fn check(r: i32) Error!u32 {
    if (r >= 0) return @intCast(r);
    const c = -@as(i64, r);
    return if (c <= errors.len) errors[@intCast(c - 1)] else error.Version;
}

// ---- Host imports (ABI §3, §5) ----

/// The raw imports, exactly as module "schemify" declares them: pointers are guest
/// addresses, lengths are bytes, and negative returns are `-Code`. The typed wrappers
/// below cover every one; reach for these only for ids outside `Table` or other raw use.
pub const raw = struct {
    pub extern "schemify" fn table_len(table: u32) i32;
    pub extern "schemify" fn table_epoch(table: u32) i32;
    pub extern "schemify" fn read_column(table: u32, column: u32, start: u32, count: u32, dst: [*]u8, dst_len: u32) i32;
    pub extern "schemify" fn read_strings(table: u32, column: u32, start: u32, count: u32, dst: [*]u8, dst_len: u32) i32;
    pub extern "schemify" fn eval(doc: u32, expr: [*]const u8, len: u32) i32;
    pub extern "schemify" fn log(level: u32, text: [*]const u8, len: u32) void;
    pub extern "schemify" fn fs_read(path: [*]const u8, path_len: u32) i32;
    pub extern "schemify" fn fs_list(path: [*]const u8, path_len: u32) i32;
    pub extern "schemify" fn fs_write(path: [*]const u8, path_len: u32, data: [*]const u8, data_len: u32) i32;
    pub extern "schemify" fn fs_remove(path: [*]const u8, path_len: u32) i32;
    pub extern "schemify" fn spawn(argv: [*]const u8, argv_len: u32, stdin: [*]const u8, stdin_len: u32) i32;
    pub extern "schemify" fn fetch(url: [*]const u8, url_len: u32, body: [*]const u8, body_len: u32) i32;
    pub extern "schemify" fn clipboard_get() i32;
    pub extern "schemify" fn clipboard_set(text: [*]const u8, len: u32) i32;
};

fn len32(s: anytype) u32 {
    return @intCast(s.len);
}

/// Rows in `table` in the current snapshot.
pub fn tableLen(table: Table) Error!u32 {
    return check(raw.table_len(@intFromEnum(table)));
}

/// `table`'s epoch; it changes whenever any of its rows does, so equal epochs mean the
/// rows read before are still current.
pub fn tableEpoch(table: Table) Error!u32 {
    return check(raw.table_epoch(@intFromEnum(table)));
}

/// Reads rows `start .. start + dst.len` of a fixed-width column into `dst` and returns
/// it. `T` must be the column's element type (see `col`): a string column fails with
/// `error.Column`, a range past the end with `error.Range`.
pub fn readColumn(comptime T: type, table: Table, column: u32, start: u32, dst: []T) Error![]T {
    const bytes = std.mem.sliceAsBytes(dst);
    _ = try check(raw.read_column(@intFromEnum(table), column, start, len32(dst), bytes.ptr, len32(bytes)));
    return dst;
}

/// Reads `count` strings of a string column into `dst`; walk the result with `next`.
/// `error.Space` means `dst` was too small, and the size needed is then in its first
/// 4 bytes (`std.mem.readInt(u32, dst[0..4], .little)`).
pub fn readStrings(table: Table, column: u32, start: u32, count: u32, dst: []u8) Error!Strings {
    const n = try check(raw.read_strings(@intFromEnum(table), column, start, count, dst.ptr, len32(dst)));
    return .{ .buf = dst[0..n] };
}

/// A `read_strings` buffer: each string is a u32 little-endian length then its bytes.
pub const Strings = struct {
    buf: []const u8,
    at: usize = 0,

    /// The next string, borrowing `buf`; null at the end or at a truncated entry.
    pub fn next(s: *Strings) ?[]const u8 {
        if (s.buf.len - s.at < 4) return null;
        const n = std.mem.readInt(u32, s.buf[s.at..][0..4], .little);
        if (n > s.buf.len - s.at - 4) return null;
        defer s.at += 4 + n;
        return s.buf[s.at + 4 ..][0..n];
    }
};

/// Evaluates `expr` in `doc`'s scope. Inverted: returns a handle at once, and the value
/// arrives later as an `io_done` event carrying that handle.
pub fn eval(doc: u32, expr: []const u8) Error!u32 {
    return check(raw.eval(doc, expr.ptr, len32(expr)));
}

/// Logs `text` immediately, unlike `Out.log`, which logs when the event returns.
pub fn log(level: Level, text: []const u8) void {
    raw.log(@intFromEnum(level), text.ptr, len32(text));
}

// Capability imports: each needs its grant in schemify.project (else `error.Denied`) and
// returns a handle; the result arrives as an `io_done` event with that handle.

/// Reads a file; `io_done` carries its bytes.
pub fn fsRead(path: []const u8) Error!u32 {
    return check(raw.fs_read(path.ptr, len32(path)));
}
/// Lists a directory; `io_done` carries the entries.
pub fn fsList(path: []const u8) Error!u32 {
    return check(raw.fs_list(path.ptr, len32(path)));
}
/// Writes `data` to a file, replacing it.
pub fn fsWrite(path: []const u8, data: []const u8) Error!u32 {
    return check(raw.fs_write(path.ptr, len32(path), data.ptr, len32(data)));
}
/// Removes a file.
pub fn fsRemove(path: []const u8) Error!u32 {
    return check(raw.fs_remove(path.ptr, len32(path)));
}
/// Runs a program: `argv` is line-record values (write them with `Out.putValue`).
/// `io_done`'s status is the exit status and its bytes are stdout.
pub fn spawn(argv: []const u8, stdin: []const u8) Error!u32 {
    return check(raw.spawn(argv.ptr, len32(argv), stdin.ptr, len32(stdin)));
}
/// Fetches `url`; a non-empty `body` makes it a POST.
pub fn fetch(url: []const u8, body: []const u8) Error!u32 {
    return check(raw.fetch(url.ptr, len32(url), body.ptr, len32(body)));
}
/// Reads the clipboard; `io_done` carries its text.
pub fn clipboardGet() Error!u32 {
    return check(raw.clipboard_get());
}
/// Sets the clipboard to `text`.
pub fn clipboardSet(text: []const u8) Error!u32 {
    return check(raw.clipboard_set(text.ptr, len32(text)));
}

// ---- Events (ABI §6) ----

/// One event. Strings borrow the input buffer, which is reused after `onEvent` returns:
/// copy what must outlive the event.
pub const Event = union(enum) {
    load,
    unload,
    /// `args` is line-record values; walk them with `values(args)`.
    command: struct { name: []const u8, args: []const u8 },
    doc_opened: u32,
    doc_closed: u32,
    doc_changed: struct { doc: u32, epoch: u32 },
    selection_changed,
    netlist_request: struct { dialect: []const u8, bench: []const u8 },
    run_job: struct { job: u32, lang: []const u8 },
    run_done: u32,
    /// `status` >= 0 is success (the exit status for `spawn`), < 0 a negated `Code`.
    io_done: struct { handle: u32, status: i32, bytes: []const u8 },
    timer: u32,
    saved: u32,
    rejected: struct { code: Code, record: []const u8 },

    /// Parses the wire form: one `event` line, then for `io_done` and `rejected` only, a
    /// NUL and the payload. Anything else, including an unknown kind, is `error.Parse`.
    pub fn parse(in: []const u8) error{Parse}!Event {
        const nul = std.mem.indexOfScalar(u8, in, 0);
        var line = in[0 .. nul orelse in.len];
        const payload: ?[]const u8 = if (nul) |i| in[i + 1 ..] else null;
        if (std.mem.endsWith(u8, line, "\n")) line.len -= 1;
        if (std.mem.indexOfAny(u8, line, "\r\n") != null) return error.Parse;
        var t: Tokens = .{ .rest = line };
        if (!std.mem.eql(u8, t.next(), "event")) return error.Parse;
        const tag = std.meta.stringToEnum(std.meta.Tag(Event), t.next()) orelse return error.Parse;
        const bytes = payload orelse "";
        const ev: Event = switch (tag) {
            .load => .load,
            .unload => .unload,
            .selection_changed => .selection_changed,
            .command => .{ .command = .{
                .name = try t.ident(),
                .args = std.mem.trimStart(u8, t.take(), " \t"),
            } },
            .doc_opened => .{ .doc_opened = try t.int(u32) },
            .doc_closed => .{ .doc_closed = try t.int(u32) },
            .saved => .{ .saved = try t.int(u32) },
            .doc_changed => .{ .doc_changed = .{ .doc = try t.int(u32), .epoch = try t.int(u32) } },
            .netlist_request => .{ .netlist_request = .{ .dialect = try t.ident(), .bench = try t.ident() } },
            .run_job => .{ .run_job = .{ .job = try t.int(u32), .lang = try t.ident() } },
            .run_done => .{ .run_done = try t.int(u32) },
            .timer => .{ .timer = try t.int(u32) },
            .io_done => .{ .io_done = .{ .handle = try t.int(u32), .status = try t.int(i32), .bytes = bytes } },
            .rejected => .{ .rejected = .{ .code = try t.code(), .record = bytes } },
        };
        const wants_payload = tag == .io_done or tag == .rejected;
        if ((payload != null) != wants_payload or t.next().len != 0) return error.Parse;
        return ev;
    }
};

fn isSpace(c: u8) bool {
    return c == ' ' or c == '\t';
}

/// A bare-value byte (format §2): not whitespace, quote, backslash or control.
fn isBare(c: u8) bool {
    return c > 0x20 and c != 0x7f and c != '"' and c != '\\';
}

/// True when `v` can be written unquoted.
fn bare(v: []const u8) bool {
    if (v.len == 0 or v[0] == '#' or v[0] == '|') return false;
    for (v) |c| if (!isBare(c)) return false;
    return true;
}

/// Whitespace-separated tokens of an event line (bare text only).
const Tokens = struct {
    rest: []const u8,

    fn next(t: *Tokens) []const u8 {
        const s = std.mem.trimStart(u8, t.rest, " \t");
        const end = std.mem.indexOfAny(u8, s, " \t") orelse s.len;
        t.rest = s[end..];
        return s[0..end];
    }

    fn take(t: *Tokens) []const u8 {
        defer t.rest = "";
        return t.rest;
    }

    fn ident(t: *Tokens) error{Parse}![]const u8 {
        const s = t.next();
        return if (bare(s)) s else error.Parse;
    }

    /// Decimal with an optional '-', in `T`'s range; no '+', '_' or radix prefix.
    fn int(t: *Tokens, comptime T: type) error{Parse}!T {
        const s = t.next();
        const neg = s.len > 0 and s[0] == '-';
        const digits = s[@intFromBool(neg)..];
        if (digits.len == 0) return error.Parse;
        var n: i64 = 0;
        for (digits) |c| {
            if (c < '0' or c > '9' or n > 1 << 40) return error.Parse;
            n = n * 10 + (c - '0');
        }
        return std.math.cast(T, if (neg) -n else n) orelse error.Parse;
    }

    fn code(t: *Tokens) error{Parse}!Code {
        const s = t.next();
        for (code_names, 1..) |name, c| if (std.mem.eql(u8, s, name)) return @enumFromInt(c);
        return error.Parse;
    }
};

// ---- Line-record values (format spec §2): `bare` or "quoted \"escaped\"" ----

/// The values of `text` (a command's or function's args), one by one.
pub fn values(text: []const u8) Values {
    return .{ .rest = text };
}

/// An iterator over line-record values.
pub const Values = struct {
    rest: []const u8,

    /// Unescapes the next value into `dst` and returns that part of it; null when none is
    /// left. `error.Parse` (malformed, or longer than `dst`) also ends the iteration.
    pub fn next(v: *Values, dst: []u8) error{Parse}!?[]u8 {
        const s = std.mem.trimStart(u8, v.rest, " \t");
        v.rest = ""; // stop after an error
        if (s.len == 0) return null;
        var n: usize = 0;
        var i: usize = 0;
        if (s[0] == '"') {
            i = 1;
            while (true) : (i += 1) {
                if (i == s.len or s[i] == '\n' or s[i] == '\r') return error.Parse;
                if (s[i] == '"') break;
                if (s[i] != '\\') {
                    try push(dst, &n, &.{s[i]});
                    continue;
                }
                i += 1;
                if (i == s.len) return error.Parse;
                switch (s[i]) {
                    '"', '\\' => try push(dst, &n, &.{s[i]}),
                    'n' => try push(dst, &n, "\n"),
                    't' => try push(dst, &n, "\t"),
                    'u' => {
                        if (i + 1 >= s.len or s[i + 1] != '{') return error.Parse;
                        const close = std.mem.indexOfScalarPos(u8, s, i + 2, '}') orelse return error.Parse;
                        const hex = s[i + 2 .. close];
                        if (hex.len == 0 or hex.len > 6) return error.Parse;
                        for (hex) |c| if (!std.ascii.isHex(c)) return error.Parse;
                        const cp = std.fmt.parseInt(u21, hex, 16) catch return error.Parse;
                        var utf8: [4]u8 = undefined;
                        const len = std.unicode.utf8Encode(cp, &utf8) catch return error.Parse;
                        try push(dst, &n, utf8[0..len]);
                        i = close;
                    },
                    else => return error.Parse,
                }
            }
            i += 1;
        } else {
            if (s[0] == '#' or s[0] == '|') return error.Parse;
            while (i < s.len and isBare(s[i])) i += 1;
            try push(dst, &n, s[0..i]);
        }
        if (i < s.len and !isSpace(s[i])) return error.Parse;
        v.rest = s[i..];
        return dst[0..n];
    }

    fn push(dst: []u8, n: *usize, bytes: []const u8) error{Parse}!void {
        if (bytes.len > dst.len - n.*) return error.Parse;
        @memcpy(dst[n.*..][0..bytes.len], bytes);
        n.* += bytes.len;
    }
};

// ---- Output (ABI §7) ----

/// Whether an `edit` waits for the user (`stage`) or applies at once (`commit`).
pub const EditMode = enum { stage, commit };

/// An append-only text buffer: the event's output records, or any text you compose (a
/// log message, an edit's ops). Once full, `overflow` is set and every later append is
/// dropped whole, so the text never ends mid-append.
pub const Out = struct {
    buf: []u8,
    len: usize = 0,
    overflow: bool = false,

    /// An empty buffer writing into `buf`.
    pub fn init(buf: []u8) Out {
        return .{ .buf = buf };
    }

    /// The text written so far; valid until the next append.
    pub fn slice(o: *const Out) []const u8 {
        return o.buf[0..o.len];
    }

    /// Raw text.
    pub fn put(o: *Out, s: []const u8) void {
        if (o.overflow or s.len > o.buf.len - o.len) {
            o.overflow = true;
            return;
        }
        @memcpy(o.buf[o.len..][0..s.len], s);
        o.len += s.len;
    }

    /// Raw text formatted by `std.fmt`.
    pub fn print(o: *Out, comptime fmt: []const u8, args: anytype) void {
        if (o.overflow) return;
        const s = std.fmt.bufPrint(o.buf[o.len..], fmt, args) catch {
            o.overflow = true;
            return;
        };
        o.len += s.len;
    }

    /// One value: bare if legal, else quoted, with `"` `\` LF TAB escaped and other
    /// control bytes as `\u{x}` (lowercase hex, no leading zeros).
    pub fn putValue(o: *Out, v: []const u8) void {
        if (bare(v)) return o.put(v);
        o.put("\"");
        for (v) |c| switch (c) {
            '"' => o.put("\\\""),
            '\\' => o.put("\\\\"),
            '\n' => o.put("\\n"),
            '\t' => o.put("\\t"),
            0...8, 0x0b...0x1f, 0x7f => o.print("\\u{{{x}}}", .{c}),
            else => o.put(&.{c}),
        };
        o.put("\"");
    }

    // Records. A body is line-record text, one record per line, written verbatim.

    /// Declares this plugin's whole set of `kind` (immediate mode: it replaces the last).
    pub fn contribute(o: *Out, kind: Kind, body: []const u8) void {
        o.print("contribute {s}\n", .{@tagName(kind)});
        o.verbatim(body);
    }

    /// Like `contribute`, but replaces only the set under `scope`.
    pub fn contributeScoped(o: *Out, kind: Kind, scope: []const u8, body: []const u8) void {
        o.print("contribute {s} ", .{@tagName(kind)});
        o.putValue(scope);
        o.put("\n");
        o.verbatim(body);
    }

    /// Edits `doc` with `ops` as one undo step named `label`.
    pub fn edit(o: *Out, doc: u32, mode: EditMode, label: []const u8, ops: []const u8) void {
        o.print("edit {d} {s} ", .{ doc, @tagName(mode) });
        o.putValue(label);
        o.put("\n");
        o.verbatim(ops);
    }

    /// Runs a testbench; follow with `vary` calls for its sweep variables.
    pub fn run(o: *Out, bench: []const u8, config: []const u8, corner: []const u8) void {
        o.put("run ");
        o.putValue(bench);
        o.put(" ");
        o.putValue(config);
        o.put(" ");
        o.putValue(corner);
        o.put("\n");
    }

    /// Sets `variable` for the `run` written just before; anywhere else it is rejected.
    pub fn vary(o: *Out, variable: []const u8, value: []const u8) void {
        o.put("  vary ");
        o.putValue(variable);
        o.put(" ");
        o.putValue(value);
        o.put("\n");
    }

    /// Asks for a `timer` event with `id` after `ms` milliseconds.
    pub fn timer(o: *Out, id: u32, ms: u32) void {
        o.print("timer {d} {d}\n", .{ id, ms });
    }

    /// Logs `text` once the event returns (`schemify.log` logs at once).
    pub fn log(o: *Out, level: Level, text: []const u8) void {
        o.print("log {s} ", .{@tagName(level)});
        o.putValue(text);
        o.put("\n");
    }

    /// Each line of `body` as `  | line`, an empty one as `  |`; a final newline adds none.
    fn verbatim(o: *Out, body: []const u8) void {
        var lines = std.mem.splitScalar(u8, body, '\n');
        while (lines.next()) |l| {
            if (lines.index == null and l.len == 0) break;
            const line = if (std.mem.endsWith(u8, l, "\r")) l[0 .. l.len - 1] else l;
            o.put(if (line.len > 0) "  | " else "  |");
            o.put(line);
            o.put("\n");
        }
    }
};

// ---- The exports (ABI §2) ----

/// Exports the module's entry points for `Plugin`, which declares
/// `pub fn onEvent(ev: Event, out: *Out) void` and, if it contributes `functions`,
/// `pub fn call(fn_id: u32, args: []const u8, result: *Out) Error!void`: a pure function
/// (every import is denied) writing one value; `error.Range` for an unknown `fn_id`.
/// Call it once, from a `comptime` block in the plugin's root file.
pub fn exportPlugin(comptime Plugin: type) void {
    const G = Glue(Plugin);
    @export(&G.abi, .{ .name = "schemify_abi" });
    @export(&G.alloc, .{ .name = "schemify_alloc" });
    @export(&G.onEvent, .{ .name = "schemify_on_event" });
    if (@hasDecl(Plugin, "call")) @export(&G.call, .{ .name = "schemify_call" });
}

fn Glue(comptime Plugin: type) type {
    return struct {
        const input_max = if (@hasDecl(Plugin, "schemify_input_max")) Plugin.schemify_input_max else 1 << 20;
        const output_max = if (@hasDecl(Plugin, "schemify_output_max")) Plugin.schemify_output_max else 1 << 20;
        var in: [input_max]u8 = undefined;
        var out: [output_max]u8 = undefined;
        var in_len: u32 = 0;

        fn pack(ptr: usize, len: usize) u64 {
            return @as(u64, ptr) << 32 | len;
        }

        /// The host wrote `len` bytes at `ptr`, and only `alloc` hands out that address.
        fn ours(ptr: u32, len: u32) bool {
            return ptr == @intFromPtr(&in) and len == in_len;
        }

        fn abi() callconv(.c) u32 {
            return abi_version;
        }

        fn alloc(len: u32) callconv(.c) u32 {
            if (len > in.len) return 0; // the host reports E_SPACE
            in_len = len;
            return @intFromPtr(&in);
        }

        fn onEvent(ptr: u32, len: u32) callconv(.c) u64 {
            if (!ours(ptr, len)) return 0;
            var o: Out = .init(&out);
            if (Event.parse(in[0..len])) |ev| Plugin.onEvent(ev, &o) else |_| o.log(.@"error", "unparsable event");
            if (o.overflow) {
                o = .init(&out);
                o.log(.@"error", "output over schemify_output_max, dropped");
            }
            return if (o.len > 0) pack(@intFromPtr(&out), o.len) else 0;
        }

        fn call(fn_id: u32, ptr: u32, len: u32) callconv(.c) u64 {
            if (!ours(ptr, len)) return pack(0, @intFromEnum(Code.range));
            var o: Out = .init(&out);
            Plugin.call(fn_id, in[0..len], &o) catch |e| return pack(0, @intFromEnum(codeOf(e)));
            if (o.overflow) return pack(0, @intFromEnum(Code.space));
            if (o.len == 0) return pack(0, @intFromEnum(Code.parse));
            return pack(@intFromPtr(&out), o.len);
        }
    };
}
