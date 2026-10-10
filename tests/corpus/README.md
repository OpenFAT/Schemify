# Netlist corpus

Differential oracle for the `netlist` row of [TESTING.md](../../TESTING.md):
golden netlists produced by *running* a pinned xschem (clean-room, D1) on real
PDK test benches, compared semantically against ours.

Only scripts and the bench list are committed. Everything third-party is
fetched into `.cache/` (gitignored) at pinned revisions; nothing GPL is vendored.

## What is fetched (`fetch.sh`)

Shallow, blob-filtered, sparse checkouts, about 30 MB in total:

| Repo | Commit | Paths | License |
|---|---|---|---|
| [xschem](https://github.com/StefanSchippers/xschem) | `92dd8fe` (tag 3.4.7) | `xschem_library/` (example data only, no C/Tcl/awk source) | GPL-2.0 (data is never committed) |
| [xschem_sky130](https://github.com/StefanSchippers/xschem_sky130) | `fef70fa` | `sky130_fd_pr/ sky130_stdcells/ sky130_tests/ stdcells/ xschemrc` | Apache-2.0 |
| [gf180mcu_fd_pr](https://github.com/fossi-foundation/globalfoundries-pdk-libs-gf180mcu_fd_pr) | `e11a8c9` | `cells/xschem/ models/ngspice/` | Apache-2.0 |
| [IHP-Open-PDK](https://github.com/IHP-GmbH/IHP-Open-PDK) | `5e6d592` | `ihp-sg13g2/libs.tech/{xschem,ngspice,verilog-a}/`, `libs.ref/sg13g2_stdcell/{sym,sch,spice,cdl,verilog}/`, `libs.ref/sg13g2_io/{spice,cdl}/` | Apache-2.0 |

The xschem tag matches the binary `golden.sh` runs (nixpkgs `331800d`,
xschem 3.4.7). sky130 model files are not fetched: open_pdks generates them,
and netlisting only needs their paths. GF180 models ship as `sm141064.spice`
while the benches reference `sm141064.ngspice` (open_pdks renames them);
rename or symlink before simulating.

## Regenerating

```sh
tests/corpus/fetch.sh          # idempotent; prints commit and size per repo
tests/corpus/golden.sh         # all benches; or a name regex: golden.sh '^ihp-'
tests/corpus/diff_all.sh DIR   # compare DIR/<name>.{spice,cdl} with the goldens
```

`golden.sh` needs `nix` (flakes). Without it, point `XSCHEM=/path/to/xschem`
at an xschem 3.4.7 binary (the sky130 generator bench also needs `tclsh`).

For each line of `benches.txt` (`name rcfile path`) it writes into
`.cache/golden/`:

- `<name>.spice`: the default SPICE netlist (`xschem -x -s` + `xschem netlist`);
- `<name>.cdl`: the LVS netlist, same run with `--tcl 'set lvs_netlist 1'`
  (uses each symbol's `lvs_format`, wraps the top level in `.subckt`, adds
  `*.PININFO`);
- `<name>.{spice,cdl}.log`: xschem output, including its ERC messages between
  `ERC-BEGIN`/`ERC-END` (`undriven node`, `open net`, `shorted output node`);
- `REPORT.txt`: `ok` or `FAIL <reason>` per bench and mode. A missing
  netlist, a crash, or a non-ERC error line (missing symbol, Tcl error) is a
  failure; the script exits 1 if any bench failed.

The rcfiles are generated: the xschem examples get xschem's default library
path re-rooted at the fetched `xschem_library`; PDK benches set
`PDK_ROOT`/`PDK` into the cache and `source` the PDK's own `xschemrc`, so
`SKYWATER_MODELS`, `180MCU_MODELS` and `MODELS_NGSPICE` resolve to cache paths
exactly as they would in a user install. `HOME` points into the cache so no
user `~/.xschem` leaks in.

Note: `xschem -q -n` exits 10 whenever ERC reports anything and prints
nothing; the script netlists through `--script` instead (byte-identical
output) so the ERC text is kept.

## Equivalence (`tools/spicecmp`)

`spicecmp a b` (exit 0 equal, 1 different, 2 usage/I/O) and the library call
`schemify_spicecmp::compare(a, b, &Options) -> Report` treat two netlists as
equivalent when all six sections agree:

1. **Connectivity.** Every subckt defined in the text is flattened; calls to
   undefined subckts (PDK cells) stay leaf devices. The flat device/net graphs
   must be isomorphic under Weisfeiler–Lehman colour refinement, starting from
   device signatures (letter, model, evaluated values and params, `m=1`
   dropped) with terminal order significant. Net names do not matter except
   for anchors: `0`/`gnd`, `.global` names, names ending in `!`, and the
   ports of root subckts (a CDL top cell). Instance names, statement order and
   subckt order never matter.
2. **Params**: top-level `.param` values.
3. **Models**: `.model` cards, params in any order.
4. **Includes**: `.include`/`.lib` lines; paths verbatim after `--alias FROM=TO`
   substitution (`diff_all.sh` aliases the cache directory, so goldens from
   another checkout still compare).
5. **Directives**: all other dot-cards, case-folded, whitespace collapsed.
6. **Control**: `.control` blocks as text, comment and blank lines dropped.

Numbers are evaluated (SI suffixes, arithmetic over parameters in scope) and
compared to 10 significant digits; expressions that do not evaluate are
compared as case-folded text. Known blind spots: 1-WL cannot separate some
highly regular graphs, and B-source expressions and F/H/K references keep
names as text.

Self-tests: `cargo test -p schemify-spicecmp` (metamorphic proptests:
shuffled lines, renamed nets/instances/subckts, reordered subckts, respelled
numbers and case compare equal; one moved connection or one changed value
compares different; arbitrary text never panics and comparison is reflexive
and symmetric). With goldens present, `-- --include-ignored` also shuffles
every golden and checks that dropping a device is always detected.
