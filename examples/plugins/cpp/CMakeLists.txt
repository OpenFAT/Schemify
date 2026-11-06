# Builds hello.wasm: freestanding wasm32, no libc or libc++, no WASI
# (docs/plugins/languages.md). Needs clang++ with the WebAssembly target and wasm-ld (lld):
#   cmake -B build -DCMAKE_CXX_COMPILER=clang++ && cmake --build build
# On Nix use nixpkgs#llvmPackages.clang-unwrapped and nixpkgs#lld (the wrapped clang adds
# host flags).
cmake_minimum_required(VERSION 3.20)

# Cross to wasm32 before project(), so CMake's compiler checks use the target too.
set(CMAKE_SYSTEM_NAME Generic)
set(CMAKE_SYSTEM_PROCESSOR wasm32)
set(CMAKE_CXX_COMPILER_TARGET wasm32)
set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)

project(hello CXX)

# Schemify's C library. In your own repository, point this at a vendored copy or fetch a
# pinned Schemify revision (docs/plugins/languages.md, C++).
set(SCHEMIFY_SDK "${CMAKE_CURRENT_SOURCE_DIR}/../../../crates/protocol/abi/sdk/c"
    CACHE PATH "Directory holding schemify.h")

add_executable(hello hello.cpp)
target_compile_features(hello PRIVATE cxx_std_20)
target_include_directories(hello PRIVATE "${SCHEMIFY_SDK}")
target_compile_options(hello PRIVATE -Os -fno-exceptions -fno-rtti -Wall -Wextra)
target_link_options(hello PRIVATE -nostdlib -Wl,--no-entry -Wl,--strip-all)
# The module lands next to schemify.project, which names it.
set_target_properties(hello PROPERTIES
    SUFFIX .wasm
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}")
