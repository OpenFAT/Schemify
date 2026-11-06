# Builds hello.wasm: freestanding wasm32, no libc, no WASI (docs/plugins/languages.md).
# Needs clang with the WebAssembly target and wasm-ld (lld). On Nix the wrapped clang adds
# host flags; use `nix shell nixpkgs#llvmPackages.clang-unwrapped nixpkgs#lld -c make`.
#
# SCHEMIFY points at Schemify's C library (crates/protocol/abi/sdk/c). In your own
# repository, vendor that directory or a pinned Schemify checkout and set it here.
SCHEMIFY ?= ../../../crates/protocol/abi/sdk/c
CC = clang
CFLAGS = --target=wasm32 -O2 -nostdlib -Wall -Wextra -I$(SCHEMIFY)
LDFLAGS = -Wl,--no-entry -Wl,--strip-all

hello.wasm: hello.c $(SCHEMIFY)/schemify.h
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ hello.c

clean:
	rm -f hello.wasm

.PHONY: clean
