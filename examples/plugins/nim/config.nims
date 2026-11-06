# Builds hello.wasm: freestanding wasm32, no GC, no libc, no WASI (docs/plugins/languages.md).
# Needs nim 2.x, clang with the WebAssembly target and wasm-ld:
#   nix shell nixpkgs#nim nixpkgs#llvmPackages.clang-unwrapped nixpkgs#lld -c nim c hello.nim
# (on Nix the wrapped clang adds host flags; use clang-unwrapped).
#
# Schemify's C library (crates/protocol/abi/sdk/c). In your own repository, vendor that
# directory or a pinned Schemify checkout and point this at it.
const schemify = thisDir() & "/../../../crates/protocol/abi/sdk/c"

switch "cpu", "wasm32"
switch "os", "standalone"   # needs panicoverride.nim
switch "mm", "none"
switch "threads", "off"
switch "noMain"
switch "define", "release"
switch "opt", "size"
switch "nimcache", "build"
# Nix's nim calls gcc for --cc:clang unless told otherwise; -ldl is the default linker option.
switch "cc", "clang"
switch "clang.exe", "clang"
switch "clang.linkerexe", "clang"
switch "clang.options.linker", ""
switch "passC", "--target=wasm32 -ffreestanding -nostdlib -I" & thisDir() & "/include -I" & schemify
switch "passL", "--target=wasm32 -nostdlib -Wl,--no-entry -Wl,--strip-all"
switch "out", "hello.wasm"
