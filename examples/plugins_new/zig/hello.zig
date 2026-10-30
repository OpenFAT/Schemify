//! A Schemify plugin in Zig: Tools → Count nets logs the nets with a single connection.
const std = @import("std");
const schemify = @import("schemify");

comptime {
    schemify.exportPlugin(@This());
}

var pins: [1 << 16]u32 = undefined;
var names: [1 << 20]u8 = undefined;

pub fn onEvent(ev: schemify.Event, out: *schemify.Out) void {
    switch (ev) {
        // Contributions are immediate-mode: send the whole set every time.
        .load, .doc_changed => {
            out.contribute(.ui,
                \\node tools.hello item
                \\  parent menu.tools
                \\  label "Count nets"
                \\  command hello.count
            );
            out.contribute(.commands, "command hello.count \"Count nets\"");
        },
        .command => |c| if (std.mem.eql(u8, c.name, "hello.count")) countNets(out),
        else => {},
    }
}

fn countNets(out: *schemify.Out) void {
    const n = @min(schemify.tableLen(.nets) catch 0, pins.len);
    const counts = schemify.readColumn(u32, .nets, schemify.col.nets.pin_count, 0, pins[0..n]) catch pins[0..0];
    var strings = schemify.readStrings(.nets, schemify.col.nets.name, 0, n, &names) catch schemify.Strings{ .buf = "" };

    var list_buf: [1024]u8 = undefined;
    var list: schemify.Out = .init(&list_buf);
    var single: u32 = 0;
    for (counts) |count| {
        const name = strings.next() orelse break;
        if (count != 1) continue;
        list.put(if (single == 0) ": " else ", ");
        list.put(name);
        single += 1;
    }
    var msg: [1100]u8 = undefined;
    out.log(.info, std.fmt.bufPrint(&msg, "{d} single-connection nets{s}", .{ single, list.slice() }) catch "too many nets");
}
