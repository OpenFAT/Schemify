//! Builds hello.wasm next to schemify.project: `zig build` (Zig 0.16).
const std = @import("std");

pub fn build(b: *std.Build) void {
    // The library is one file inside the Schemify repository (build.zig.zon's `schemify`).
    const schemify = b.dependency("schemify", .{}).path("crates/protocol/abi/sdk/zig/schemify.zig");
    const exe = b.addExecutable(.{
        .name = "hello",
        .root_module = b.createModule(.{
            .root_source_file = b.path("hello.zig"),
            .target = b.resolveTargetQuery(.{ .cpu_arch = .wasm32, .os_tag = .freestanding }),
            .optimize = .ReleaseSmall,
            .imports = &.{.{ .name = "schemify", .module = b.createModule(.{ .root_source_file = schemify }) }},
        }),
    });
    exe.entry = .disabled; // a plugin has no main
    exe.rdynamic = true; // keep the schemify_* exports
    const wasm = b.addInstallFileWithDir(exe.getEmittedBin(), .prefix, "../hello.wasm");
    b.getInstallStep().dependOn(&wasm.step);
}
