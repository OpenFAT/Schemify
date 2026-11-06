# Builds conformance.wasm (../../CONFORMANCE.md). Needs clang (wasm32 target) and wasm-ld.
CC = clang
CFLAGS = --target=wasm32 -O2 -nostdlib -Wall -Wextra -I..
LDFLAGS = -Wl,--no-entry -Wl,--strip-all

conformance.wasm: conformance.c ../schemify.h
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ conformance.c
