//! Builds conformance.wasm (../../CONFORMANCE.md) next to this file: `zig build`.
const std = @import("std");

pub fn build(b: *std.Build) void {
    const exe = b.addExecutable(.{
        .name = "conformance",
        .root_module = b.createModule(.{
            .root_source_file = b.path("conformance.zig"),
            .target = b.resolveTargetQuery(.{ .cpu_arch = .wasm32, .os_tag = .freestanding }),
            .optimize = .ReleaseSmall,
            .imports = &.{.{ .name = "schemify", .module = b.createModule(.{ .root_source_file = b.path("../schemify.zig") }) }},
        }),
    });
    exe.entry = .disabled;
    exe.rdynamic = true;
    const install = b.addInstallFileWithDir(exe.getEmittedBin(), .{ .custom = "../" }, "conformance.wasm");
    b.getInstallStep().dependOn(&install.step);
}
