# Plugins in any language

A plugin is a core wasm32 module ([plugin ABI](../spec/plugin-abi.md)), so any language that emits one can write a plugin. Schemify ships a guest library for each family:

- Rust uses the `schemify-abi` crate.
- C and C++ share `schemify.h`. Nim, D and Swift import that same header.
- Zig, Odin, Go and AssemblyScript (TypeScript) each have their own library, in [crates/protocol/abi/sdk/](../../crates/protocol/abi/sdk).

Every library covers the whole ABI: the host imports, the table and column ids, event parsing, line-record values, the output records and the module's exports. The plugin writes only its event handler.

Each template in [examples/plugins_new/](../../examples/plugins_new) is a complete plugin repository root, built with the language's own build system. Every template does the same thing:

- On `Load` it adds **Tools → Count nets**.
- The command logs the nets that have a single connection, with their names (`2 single-connection nets: c, d`).

`cargo test -p schemify-plugin --test templates` loads every committed `hello.wasm` in the real host and checks that behaviour. In Claude Code, the [schemify-plugin](../dev/skills/schemify-plugin/SKILL.md) skill copies a template and builds your plugin from it.

| Language | Template | Library | Build | Module |
|---|---|---|---|---|
| Rust | [rust/](../../examples/plugins_new/rust) | `schemify-abi` (feature `guest`) | Cargo | 38 KB |
| C | [c/](../../examples/plugins_new/c) | [sdk/c/schemify.h](../../crates/protocol/abi/sdk/c/schemify.h) | Make | 9.0 KB |
| C++ | [cpp/](../../examples/plugins_new/cpp) | sdk/c/schemify.h | CMake | 6.5 KB |
| Zig | [zig/](../../examples/plugins_new/zig) | [sdk/zig/schemify.zig](../../crates/protocol/abi/sdk/zig/schemify.zig) | `build.zig` | 7.2 KB |
| Odin | [odin/](../../examples/plugins_new/odin) | [sdk/odin/schemify/](../../crates/protocol/abi/sdk/odin/schemify) | `odin build` | 13 KB |
| Go | [go/](../../examples/plugins_new/go) | [sdk/go/](../../crates/protocol/abi/sdk/go) (module) | `go.mod` + TinyGo | 11 KB |
| TypeScript | [assemblyscript/](../../examples/plugins_new/assemblyscript) | [sdk/assemblyscript/](../../crates/protocol/abi/sdk/assemblyscript) | npm + `asconfig.json` | 7.0 KB |
| Nim | [nim/](../../examples/plugins_new/nim) | sdk/c/schemify.h | `config.nims` | 6.9 KB |
| D | [d/](../../examples/plugins_new/d) | sdk/c/schemify.h | dub | 6.5 KB |
| Swift | [swift/](../../examples/plugins_new/swift) | sdk/c/schemify.h | SwiftPM | 9.7 KB |

After copying a template, follow [Repository layout](repository.md), [Testing](testing.md) and [Publishing](publishing.md).

## What a library gives you

The names follow each language's conventions (`sch_contribute`, `out.contribute`, `Out.Contribute`), but every library has the same parts:

- **Exports.** The library provides `schemify_abi`, `schemify_alloc`, `schemify_on_event` and, for plugins with expression functions, `schemify_call`. It parses each event and calls your handler with a typed event and an output builder.
- **Events.** All 14 kinds of ABI §6 arrive as one typed value. Strings in an event borrow the input buffer, which is valid only until your handler returns.
- **Reads.** Every import of ABI §3 and §5 is wrapped, including `read_column`, `read_strings` (with a walker for its length-prefixed strings) and the inverted I/O that completes as `io_done`. Table and column ids are named constants, such as `SCH_NETS_PIN_COUNT`, `col.nets.pin_count` or `NetsPinCount`.
- **Values.** An iterator unescapes line-record values, such as command arguments and function arguments. A writer quotes a value only when it needs quoting.
- **Outputs.** The builder writes `contribute` (with a scope, for annotations), `edit` (stage or commit), `run` with `vary`, `timer` and `log`. Contributions are immediate-mode: send the whole set again every time it changes.
- **Buffers.** Input and output are static, 1 MiB each, and nothing is allocated per event. An event over the input size is refused, which the host reports as `E_SPACE`. Output past the cap is replaced by one error log. C and Zig let you change the sizes; the per-language notes say how.

The ids in each library are generated from `schemify-abi`. `cargo test -p schemify-abi --test sdk` fails when a library falls behind, and `SCHEMIFY_BLESS=1` rewrites its generated region. Each library also has a conformance guest ([sdk/CONFORMANCE.md](../../crates/protocol/abi/sdk/CONFORMANCE.md)). `cargo test -p schemify-plugin --test sdk` runs those guests in the host and checks that events, values, records, table reads and function calls behave as in the Rust SDK.

## Using a library from your own repository

The templates reach their library by a path inside this checkout. In your own repository, pin a Schemify revision; use the full 40-character commit hash wherever a tool takes one.

| Language | How |
|---|---|
| Rust | `schemify-abi = { git = "https://github.com/OmarSiwy/Schemify", rev = "<commit>", features = ["guest"] }` ([repository.md](repository.md)) |
| C, C++, Nim, D, Swift | Vendor `crates/protocol/abi/sdk/c/schemify.h` from a pinned revision, or add Schemify as a git submodule, and point the include path at it |
| Zig | `zig fetch --save=schemify git+https://github.com/OmarSiwy/Schemify#<commit>`. `build.zig` keeps `b.dependency("schemify", .{}).path("crates/protocol/abi/sdk/zig/schemify.zig")` |
| Odin | Vendor `crates/protocol/abi/sdk/odin` and point `-collection:schemify=` at it |
| Go | Delete the `replace` line in `go.mod`, then `go get github.com/OmarSiwy/Schemify/crates/protocol/abi/sdk/go@<commit>` |
| TypeScript | npm can't install a git subdirectory: vendor `crates/protocol/abi/sdk/assemblyscript` or add a submodule, and point the `file:` dependency at it |

## Check a module

```sh
cargo run -p schemify-plugin --example check -- path/to/my_plugin.wasm my.command
```

`check` loads the module in the real host and delivers `Load` plus each command you name, against four sample nets. It prints the contributions, logs and faults, and exits 1 on any fault. A load failure names its cause. The most common is an import outside module `schemify`, which means a runtime, libc or WASI slipped in.

## Per-language notes

### Rust

Build with `cargo build --release`: `.cargo/config.toml` sets the `wasm32-unknown-unknown` target, which needs `rustup target add wasm32-unknown-unknown`. Cargo can't choose where its output goes, so copy `target/wasm32-unknown-unknown/release/hello.wasm` next to `schemify.project`. The SDK and `std` cost about 30 KB of module size; the other libraries cost 5 to 10 KB.

### C

Build with `make`. One source file defines `SCHEMIFY_IMPLEMENTATION` before including `schemify.h`, which provides the exports. The plugin defines `sch_on_event`, and also `sch_call` when `SCHEMIFY_FUNCTIONS` is defined. Define `SCHEMIFY_INPUT_MAX` or `SCHEMIFY_OUTPUT_MAX` to resize the buffers.

There is no libc. Every libc for wasm (wasi-libc, Emscripten) imports WASI and so cannot load. `schemify.h` supplies weak `memset`, `memcpy`, `memmove` and `strlen`, which the compiler emits calls to on its own. On Nix, the wrapped `clang` adds host glibc flags: run `nix shell nixpkgs#llvmPackages.clang-unwrapped nixpkgs#lld -c make`.

### C++

Build with `cmake -B build -DCMAKE_CXX_COMPILER=clang++ && cmake --build build`. `CMakeLists.txt` sets the wasm32 target before `project()`, so CMake's compiler checks use it too. Use the same unwrapped clang as for C on Nix. There are no exceptions, no RTTI and no standard library (`<string_view>` needs libc++, which needs a libc). Templates, `constexpr` and classes all work; the template wraps `sch_out` in a 10-line `Text` with `operator<<`.

### Zig

Build with `zig build` (Zig 0.16). The plugin calls `comptime { schemify.exportPlugin(@This()); }` and declares `pub fn onEvent(ev: schemify.Event, out: *schemify.Out) void`. It can also declare `pub fn call(...)`, and `pub const schemify_input_max` / `schemify_output_max` to resize the buffers.

- `build.zig` needs `exe.rdynamic = true`, or the exports are dropped, and `exe.entry = .disabled`.
- Fetched packages go to a project-local `zig-pkg/`; list it in `.gitignore`.
- Renaming the package makes `build.zig.zon`'s `.fingerprint` invalid. The error prints the new value.
- `std.fmt` works without an OS; `out.print` uses it.

### Odin

Build with:

```sh
odin build . -target:freestanding_wasm32 -no-entry-point -o:size -collection:schemify=<sdk/odin>
```

The library exports `schemify_abi` and `schemify_alloc`. The plugin writes the one-line export `schemify_on_event :: proc "contextless" (ptr, n: u32) -> u64 { return schemify.on_event(ptr, n, on_event) }`, and `schemify_call` the same way. That is because `@(init)` procedures never run without an entry point.

- Every export is `"contextless"`, because the host calls without an Odin `context`. `core:strings`, `core:fmt` and the allocators need one, so either avoid them or set `context = runtime.default_context()` yourself.
- A constant array can't be indexed at run time; copy it into a global first.
- Bounds checks stay on, and a failed one traps; the host reports it as `E_TRAP`.

### Go

Build with `tinygo build -target=wasm-unknown -no-debug -o hello.wasm .`. TinyGo also needs the `go` binary on `PATH`. Standard Go cannot build a plugin: `GOOS=wasip1` imports WASI and `GOOS=js` imports `gojs`.

- The library owns the exports. Register the handlers with `schemify.Handle(f)` and `schemify.HandleCall(f)` from `init()`, and keep an empty `func main() {}`, which never runs.
- TinyGo's `wasm-unknown` garbage collector never frees. Allocate nothing per event: keep buffers at package level, append only within capacity, and check with `tinygo build … -print-allocs=.`. A local array whose address is passed to a function moves to the heap.
- Event strings and bytes borrow the input buffer, so copy anything you keep.
- Don't `gofmt -w` the library file. Its generated region is checked byte for byte, and it isn't gofmt-aligned.

### TypeScript and JavaScript

Write TypeScript syntax in [AssemblyScript](https://www.assemblyscript.org). Build with `npm install && npm run build`.

- Only the entry file's exports become wasm exports. The plugin re-exports the library's (`export { schemify_abi, schemify_alloc, schemify_on_event } from "@schemify/sdk/schemify"`) and registers its handler with `onEvent`. AssemblyScript ignores `ascMain`, so the import needs the `/schemify` path.
- `asconfig.json` must keep `"runtime": "stub"` and `"use": ["abort="]`. Without `abort=`, the module imports `env.abort` and cannot load.
- The stub runtime never frees. `+` on strings, template literals, `toString()` and `new` inside a handler leak until the plugin unloads, so build `Out` and buffers at top level and reuse them.
- Event strings are UTF-8 byte ranges (`Str`) valid during the event. Compare them with `.eq("…")`, not `==`. The library converts JavaScript strings to UTF-8.

Plain JavaScript cannot load. JS-to-wasm toolchains embed an engine built on WASI: Javy 9 imports nine `wasi_snapshot_preview1` functions and uses wasm SIMD, and the host enables neither.

### Nim

Build with `nim c hello.nim` (Nim 2.2; the flags are in `config.nims`). The bindings come from `schemify.h` through `importc` with `header:`, and `{.compile: "schemify.c".}` builds the implementation.

- Import `sch_event` as `"const sch_event"`, so that the exported `sch_on_event` matches the header's prototype.
- `--os:standalone --mm:none` means no heap: no `string`, `seq` or `&` at run time. It also needs a `panicoverride.nim`. The generated C includes `<string.h>`, which the template's `include/string.h` declares.
- On Nix, set `clang.exe` and `clang.linkerexe` to `clang`, or Nix's Nim calls gcc. `clang.options.linker:""` drops `-ldl`.

### D

Build with `dub build` (LDC 1.41, clang for the C side), in `-betterC` mode.

- ImportC loses `schemify.h`'s wasm attributes. So `dub.sdl` compiles `schemify.c` with clang and links the object file, and `import schemify;` supplies only the types, enums and inline helpers.
- Redeclare each host import you call, with `@llvmAttr("wasm-import-module", "schemify")` and `@llvmAttr("wasm-import-name", "…")`. Without them, LDC assigns the import to module `env`.
- ImportC must preprocess for wasm32 (`CC=clang`, `-P=--target=wasm32`). `dub.settings.json` sets the target triple, because dub ignores it in `dflags`. `--no-export-dynamic` stops LDC from exporting every global.
- Compare strings with `sch_str_eq`: slice `==` needs `memcmp`.
- Declare large `char` globals `= void`, because `char.init` is 0xFF and would fill the data section.

### Swift

Build with SwiftPM using Embedded Swift and a swift.org 6.x toolchain. The Swift in nixpkgs is too old to target embedded wasm.

```sh
swift build --triple wasm32-unknown-none-wasm -c release
```

Then copy `Hello.wasm` from `--show-bin-path` next to `schemify.project`.

- The C target `CSchemify` compiles `schemify.h`'s implementation. Swift implements `@_cdecl("sch_on_event")`.
- Keep `-disable-stack-protector`: SwiftPM turns the stack protector on, and nothing defines `__stack_chk_guard`.
- `sch_out` holds a pointer, so its buffers are global `InlineArray`s used through `withUnsafeMutableBytes`.
- `withUnsafeTemporaryAllocation` pulls in `posix_memalign` and `free`; avoid it.
- On NixOS, the swift.org toolchain needs `patchelf` and library paths to run, and `--build-system native`. Normal Linux and macOS need neither.

## Cannot load today

| Language | Why |
|---|---|
| Standard Go | `GOOS=wasip1` imports 10 `wasi_snapshot_preview1` functions; `GOOS=js` imports `gojs`. Use TinyGo |
| Plain JavaScript | JS engines for wasm (Javy/QuickJS) are built on WASI and use wasm SIMD. Use AssemblyScript |
| Haskell | GHC's wasm backend targets WASI; stubbing those imports with binaryen `wasm-merge` works. Its prebuilt RTS, libc and gmp still use wasm SIMD, which Schemify's wasmi build leaves off, so the module is refused. It passes with wasmi's `simd` feature on; enabling it is a host decision (D3), and it would also turn on relaxed-simd, which is not deterministic |
