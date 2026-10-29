//! The Zig library's conformance guest (../../CONFORMANCE.md).
const std = @import("std");
const schemify = @import("schemify");

comptime {
    schemify.exportPlugin(@This());
}

var text_buf: [1 << 16]u8 = undefined;

pub fn onEvent(ev: schemify.Event, out: *schemify.Out) void {
    var t: schemify.Out = .init(&text_buf);
    switch (ev) {
        .load => {
            out.contribute(.commands,
                \\command conf.echo "Echo"
                \\command conf.out "Out"
                \\command conf.read "Read"
            );
            out.contribute(.functions, "function conf.add 0 2\nfunction conf.greet 1 1");
            t.put("load");
        },
        .unload, .selection_changed => t.put(@tagName(ev)),
        .command => |c| {
            if (std.mem.eql(u8, c.name, "conf.out")) return outRecords(out);
            if (std.mem.eql(u8, c.name, "conf.read")) return readNets(out, &t);
            t.print("command {s}", .{c.name});
            echoArgs(&t, c.args);
        },
        .doc_opened, .doc_closed, .saved => |doc| t.print("{s} {d}", .{ @tagName(ev), doc }),
        .doc_changed => |d| t.print("doc_changed {d} {d}", .{ d.doc, d.epoch }),
        .netlist_request => |n| t.print("netlist_request {s} {s}", .{ n.dialect, n.bench }),
        .run_job => |r| t.print("run_job {d} {s}", .{ r.job, r.lang }),
        .run_done => |job| t.print("run_done {d}", .{job}),
        .timer => |id| t.print("timer {d}", .{id}),
        .io_done => |io| t.print("io_done {d} {d} {s}", .{ io.handle, io.status, io.bytes }),
        .rejected => |r| t.print("rejected {d} {s}", .{ @intFromEnum(r.code), r.record }),
    }
    out.log(.info, t.slice());
}

fn echoArgs(t: *schemify.Out, args: []const u8) void {
    var buf: [4096]u8 = undefined;
    var vals = schemify.values(args);
    while (vals.next(&buf) catch return t.put(" !parse")) |v| t.print(" [{s}]", .{v});
}

fn outRecords(out: *schemify.Out) void {
    out.contributeScoped(.annotations, "erc pass", "mark R1 warn \"hi\"");
    out.edit(1, .stage, "size it", "set_attr M1 w 2u");
    out.edit(1, .commit, "move", "set_attr R1 at 10 20\nset_attr R2 at 0 0");
    out.run("lib:tb", "default", "tt");
    out.vary("vdd", "1.8");
    out.vary("w", "two words");
    out.timer(7, 250);
    out.log(.warn, "q\"\\\tx\ny\x01");
    out.log(.info, "done");
}

fn readNets(out: *schemify.Out, t: *schemify.Out) void {
    var pins: [65]u32 = undefined;
    var names: [4096]u8 = undefined;
    var n = schemify.tableLen(.nets) catch 0;
    if (n >= pins.len) n = 0;
    const counts = schemify.readColumn(u32, .nets, schemify.col.nets.pin_count, 0, pins[0..n]) catch pins[0..0];
    t.print("nets {d}:", .{n});
    if (schemify.readStrings(.nets, schemify.col.nets.name, 0, n, &names)) |strings| {
        var s = strings;
        for (counts) |pc| t.print(" {s}={d}", .{ s.next() orelse break, pc });
    } else |_| {}
    out.log(.info, t.slice());

    t.* = .init(&text_buf);
    const no_table = -schemify.raw.table_len(99);
    const string_col = errCode(schemify.readColumn(u32, .nets, schemify.col.nets.name, 0, pins[0..n]));
    const past_end = errCode(schemify.readColumn(u32, .nets, schemify.col.nets.pin_count, 0, pins[0 .. n + 1]));
    t.print("errors {d} {d} {d}", .{ no_table, string_col, past_end });
    out.log(.info, t.slice());
}

fn errCode(r: schemify.Error![]u32) u32 {
    _ = r catch |e| return @intFromEnum(schemify.codeOf(e));
    return 0;
}

pub fn call(fn_id: u32, args: []const u8, result: *schemify.Out) schemify.Error!void {
    var vals = schemify.values(args);
    var buf: [1024]u8 = undefined;
    switch (fn_id) {
        0 => {
            const a = try int(&vals);
            const b = try int(&vals);
            if (try vals.next(buf[0..0]) != null) return error.Parse;
            result.print("{d}", .{a + b});
        },
        1 => {
            const v = try vals.next(&buf) orelse return error.Parse;
            var greet: [1100]u8 = undefined;
            result.putValue(std.fmt.bufPrint(&greet, "hi {s}", .{v}) catch return error.Space);
        },
        else => return error.Range,
    }
}

fn int(vals: *schemify.Values) schemify.Error!i64 {
    var buf: [24]u8 = undefined;
    const v = try vals.next(&buf) orelse return error.Parse;
    return std.fmt.parseInt(i64, v, 10) catch error.Parse;
}
