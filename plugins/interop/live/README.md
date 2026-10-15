# Live Virtuoso test kit

One command that tells you whether the interop plugin works on a real Cadence Virtuoso. It
drives Virtuoso in batch mode through every path the plugin offers, grades the results and
writes a report that ends in a verdict: **FULLY FUNCTIONAL**, **PASS on everything tested**
(when optional areas were left out), or **NOT FULLY FUNCTIONAL** with the failing checks.

Everything the kit creates goes into libraries whose names start with `sfy` under its own work
directory. It never writes to your libraries: your own cells (`--cell`) are only read, then
copied into scratch libraries.

## Requirements

On the machine that runs Virtuoso:

- Virtuoso on `PATH`, and a `cds.lib` that defines `analogLib` and `basic`. Your usual project directory has one.
- This repository with its submodules, and the Rust toolchain (`rustup`, installable in your
  home directory without root). The kit's host steps are a Rust test of this crate. The first
  run compiles it, which takes a few minutes.
- Optional, each enables more checks: `si` (netlists), `ocean` and `spectre` (simulation). The
  kit finds them on `PATH` and reports SKIP for what it could not run.

Before the run, close every Virtuoso window on the machine's `sfy*` libraries (there are none
on a first run). If your site's Virtuoso start-up shows a dialog (licence level, PDK first use),
answer it once in an interactive session with "Always", since batch mode has nobody to click.

## Run it

From your Virtuoso project directory (the one with `cds.lib`):

`test.sh` is the shortest way: it starts clean, runs everything with the flags you give it, and
writes what to send back into `sfylive/summary.txt`:

```sh
/path/to/Schemify/plugins/interop/live/test.sh --lib sample
```

`run.sh` alone does the same without the cleanup and the summary:

```sh
/path/to/Schemify/plugins/interop/live/run.sh
```

That runs the fixtures only, with no PDK. To cover everything, add your own cells, a PDK and a
retarget map:

```sh
/path/to/Schemify/plugins/interop/live/run.sh \
    --lib myLib --project ~/designs/my_schemify_project \
    --cell myLib/amp --cell myLib/amp_tb \
    --pdk tsmcN28/nch_mac --callbacks \
    --retarget ~/maps/tsmc28.retarget
```

| Flag | Adds |
|---|---|
| `--lib LIB` | A whole Virtuoso library, repeatable: every cell is dumped, imported into Schemify, exported again, loaded into a scratch library and dumped there, and the two dumps must agree. This is the "move my project to Schemify and back" check. |
| `--project DIR` | A whole Schemify project, repeatable: it gets everything the fixture projects get (both export modes, the load, the dump back, the exact round trip). This is the "move my Schemify project to Virtuoso and back" check. |
| `--cell LIB/CELL` | Your own cell, repeatable. It gets the self-test and the full round trip (Virtuoso to Schemify to Virtuoso). Use one cell with PDK transistors and one testbench with a maestro view. |
| `--pdk LIB[/CELL]` | The PDK mirror: every device of `LIB` dumped and imported as Schemify cells. With `/CELL`, the probe also records that transistor's CDF and callbacks. |
| `--callbacks` | Loads your `--cell` cells again with the PDK's CDF callbacks running, as the Edit Properties form runs them. |
| `--retarget MAP` | The gm/Id retarget of the Exercise 1 benches onto your PDK, with your target map (spec `interop.md` §10.2). Needs `ocean` and `spectre`. |
| `--cdslib FILE` | Another `cds.lib` (default `./cds.lib`). |
| `--work DIR` | Where everything goes (default `./sfylive`). The Virtuoso libraries live in `DIR/libs`. |
| `--step STEP` | Runs one step (below) instead of all. |

Environment: `SFYLIVE_VIRTUOSO` replaces the Virtuoso command (default
`virtuoso -nograph -restore`), and `SFYLIVE_TIMEOUT` the seconds a session may take (default
3600).

When it finishes, read `sfylive/out/report.md` and send back `sfylive/sfylive-results.tgz`.
The archive holds the report, the probe output, every log and every dump. The probe lines and
your cells' dumps can contain PDK parameter names; remove what you cannot share.

## What it does

| Step | Runs | Does |
|---|---|---|
| `prepare` | Rust | Exports five Schemify fixture projects to `.sfyx` twice: with core devices as `schemify_core` cells, and on analogLib masters (the default). Writes the OCEAN scripts, damaged copies for the refusal checks, and `session1.il`. |
| `session1` | Virtuoso | Checks that every entry point and Cadence function the bridge calls exists, runs the probe, loads every export, dumps each library back, runs `SfySelfTest`, loads dumps that the SKILL dumper wrote and dumps them again, tries the refusals, and runs your cells, the PDK and the retarget. |
| `tools` | si, OCEAN | Netlists the RC fixture and your cells with Virtuoso's own netlisters (Spectre and auCdl), and runs the exported OCEAN scripts of the RC bench in Spectre. |
| `mid` | Rust | Imports the dumps, edits one instance in Schemify, exports again, and writes `session2.il`. |
| `session2` | Virtuoso | Loads that export over the first load, edits one instance parameter in Virtuoso, dumps it, and sends your cells back through a scratch library. |
| `verify` | Rust | Grades everything into `out/report.md`. |

If cargo and Virtuoso are on different machines, run the steps one by one (`--step`) with the
work directory at the same absolute path on both, for example on a shared disk.

## The checks

| Check | Passes when |
|---|---|
| `env.entry-points`, `env.cadence-functions` | `schemify.il` loads, and this release has every Cadence function the bridge calls. |
| `probe.sections` | Every probe section ran. A WARN lists the sections that raised an error: they settle the open questions of spec §13, so send the file back either way. |
| `load.<project>.<mode>` | `SfyImport` loaded the export with nothing refused, and every schematic passed `schCheck` (`schcheck.*` lists the errors otherwise). |
| `maestro.rclive.*` | The bench came with its maestro view. |
| `dump.*` | `SfyExport` dumped every library back. |
| `roundtrip.p_<dir>.*` | The same two round trips for each `--project`. |
| `roundtrip.<project>.core` | The dumps import to the original Schemify files byte for byte. This is the exact round trip of spec §7.4. |
| `roundtrip.<project>.alib` | On analogLib masters, every bench netlists the same before and after Virtuoso. |
| `dumpfix.*` | Dumps that the SKILL dumper wrote load and dump back to the same records, covering every shape kind, label kind, bus, net expression and text view. |
| `selftest.*` | `SfySelfTest` (dump, load, dump) agrees on a loaded cell, and the retarget math passes. |
| `refuse.*` | A truncated or malformed file is refused and creates nothing, an existing view needs `?overwrite`, and a cellview open for editing is never rewritten. |
| `team.kept-unchanged` | Cellviews nobody edited in Schemify stay exactly as Virtuoso drew them (spec §7.5). |
| `team.edits-arrive` | A Schemify edit reaches Virtuoso, a Virtuoso edit comes back to Schemify, and nothing else changes. |
| `analoglib.table.<cell>` | The analogLib this Virtuoso has puts its pins where the plugin's table says (export places re-mastered devices by that table). |
| `align.<device>.<cell>` | Whether a core device's pins line up with the analogLib cell's: PASS means no stubs; INFO rows name devices with another pin pattern, which keep stubs. |
| `align.rclive.no-stubs` | The RC bench exported on analogLib masters came back from Virtuoso without a single stub. |
| `netlist.<sim>.<lib>.<cell>` | Virtuoso's netlister writes a netlist of the loaded cell. |
| `netlist.cdl-matches-aucdl` | Schemify's CDL of the RC section equals Virtuoso's auCdl. A WARN shows the difference, which settles GUESS G7. |
| `ocean.rclive.<mode>.*` | The exported OCEAN script runs in Spectre and gives tau = RC = 1 ms and v(out) at 1 ms = 1 − 1/e, both within 1 %. |
| `user.*` | Your cell (or with `--lib`, your whole library, as `user.*.<lib>.ALL`) passes the self-test, imports into Schemify with every file loading, and goes back to Virtuoso and dumps to the same records. |
| `user.callbacks-run.*` | No CDF callback failed. INFO rows list the values the PDK clamped or derived. |
| `pdk.import` | The PDK mirror imports and every device cell loads. |
| `retarget.*` | `SfyRetarget` placed every class placeholder, and its log ends in PASS. |

## Known failures

`roundtrip.rclive.alib` fails today, on the mock Virtuoso and therefore on a real one. In
analogLib mode the stub on the source's MINUS pin is labelled `gnd!`, which imports as a global
`GND` label beside the bench's own ground label `0`, so the bench then fails check E003 (two
net names on one net). It is listed in `KNOWN_BUGS` in `tests/virtuoso_live.rs`.

## How the kit is tested

`tests/virtuoso_live.rs` runs the whole kit against the mock Virtuoso of `tests/skill` (the
same interpreter and database model the plugin's other tests use) in `kit_passes_on_the_mock`.
Every check passes there except the known failures above, and only si, OCEAN and the optional
areas are skipped. So a failure on a real Virtuoso means Virtuoso behaves differently from our
reading of its manuals; it does not point at the kit. `sfy_live.il` is parsed by the same
SKILL reader that gates the shipped scripts.

The `si.env` keys and the `si -batch -command nl` call follow common site setups and are not
covered by the mock. If the netlist checks fail with an si usage error, the log is in
`out/netlist/*.log`.
