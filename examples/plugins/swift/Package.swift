// swift-tools-version:6.0
// Builds hello.wasm: Embedded Swift on freestanding wasm32, no libc, no WASI
// (docs/plugins/languages.md). Needs a swift.org 6.x toolchain (nixpkgs' Swift is too old;
// it ships usr/lib/swift/embedded/wasm32-unknown-none-wasm) and wasm-ld:
//   swift build --triple wasm32-unknown-none-wasm -c release
//   cp "$(swift build --triple wasm32-unknown-none-wasm -c release --show-bin-path)/Hello.wasm" hello.wasm
//
// Schemify's C library is included by Sources/CSchemify/include/CSchemify.h through a relative
// path; in your own repository vendor schemify.h there or point it at a pinned checkout.
import PackageDescription

let package = Package(
    name: "hello",
    targets: [
        // schemify.h's implementation: the exports, which call sch_on_event in Hello.
        .target(name: "CSchemify"),
        .executableTarget(
            name: "Hello",
            dependencies: ["CSchemify"],
            swiftSettings: [
                .enableExperimentalFeature("Embedded"),
                // No libc to supply __stack_chk_guard.
                .unsafeFlags(["-wmo", "-Osize", "-Xfrontend", "-disable-stack-protector"]),
            ],
            linkerSettings: [
                .unsafeFlags(["-Xclang-linker", "-nostdlib", "-Xlinker", "--no-entry",
                              "-Xlinker", "--strip-all"]),
            ]
        ),
    ]
)
