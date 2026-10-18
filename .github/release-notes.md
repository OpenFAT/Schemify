Schemify {{TAG}} (commit {{SHA}}).

Assets: `schemify` and `schemify-gui` for Linux x86_64 (glibc ≥ 2.28), macOS aarch64 and x86_64, and Windows x86_64 (stripped, LTO); `schemify-{{TAG}}-web.tar.gz` is the
browser build (serve the directory with any static file server; WebGPU required).

## Release checklist (docs/dev/testing.md § Release exit criteria)

This release stays a draft until every box is ticked. Link the evidence (run URL or file) on each line.

- [ ] `ci` green on the tagged commit on Linux, macOS, Windows and Web, including the
      `determinism` job (byte-identical CLI outputs on all four platforms).
- [ ] Fuzzing: ≥ 24 CPU-h per target (`format/parse`, `format/merge3`, `expr/compile`,
      `cli/rpc_decode`) with no new crash and a plateaued corpus.
- [ ] Coverage not below the last release (nightly `coverage` job, `tests/coverage/floor.txt`).
- [ ] D39 budgets met: the last `nightly` perf job is green (no > 10% regression against
      `tests/perf/baseline.txt`, every budget assert passing); binary and wasm sizes within D39.
- [ ] Mutation report reviewed (last weekly `mutants` run): no new survivors in core crates
      without a note.
- [ ] Known gaps written below.

## Known gaps

- …
