# gmid-lut — gm/Id sizing plugin

A wasm guest (plugin ABI v1) that exports pure expression functions `gmid.*` (D20) backed by
per-device lookup tables (LUTs), loads those tables from the project, and characterizes devices
with ngspice. Port of the UW-ASIC `gmid-lut` salvage (D26, own code): `lut.rs` and the
GmIDVisualizer slice/netlist logic, rewritten in Rust without the C++ runner, serde or JSON.

The app does not embed it: a project loads it through its manifest, e.g.

```
plugin gmid path plugins/gmid_lut.wasm
  grant read_project
  grant write_project
  grant spawn:ngspice
```

`read_project` loads `gmid.chars` and the LUTs; `write_project` and `spawn:ngspice` are needed
only for characterization.

## Functions

All arguments and results are SI. `dev` is a device name from `gmid.chars` (a string), `gm_id`
is gm/Id in 1/V, `l` the channel length in metres, `id` the drain current in amperes.

| Function | Arity | Result |
|---|---|---|
| `gmid.vgs(dev, gm_id, l)` | 3 | \|VGS\| (V) |
| `gmid.jd(dev, gm_id, l)` | 3 | current density \|Id\|/W (A/m) |
| `gmid.w(dev, gm_id, l, id)` | 4 | width W = id / jd (m) |
| `gmid.gm(dev, gm_id, l, id)` | 4 | gm = gm_id · id (S), only where gm_id is reachable |
| `gmid.gds(dev, gm_id, l, id)` | 4 | gds = gm / gain (S) |
| `gmid.gain(dev, gm_id, l)` | 3 | intrinsic gain gm/gds (V/V) |
| `gmid.ft(dev, gm_id, l)` | 3 | transit frequency gm / (2π·Cgg) (Hz) |

Registry records (`contribute functions`, plugin-abi §7): `function <name> <fn_id> <arity>` with
`fn_id = index + 8 × generation`, index in the table order above (vgs 0 … ft 6). The generation
starts at 0 on `Load` and advances whenever the loaded tables change (a LUT loads or a
characterization finishes); the plugin then re-declares the set, so the host's registry epoch
moves and it re-evaluates every expression that calls `gmid.*` (an identical re-declaration would
be ignored). `schemify_call` reads `fn_id % 8`.

Errors: every failure is `E_RANGE` (the host reports `X010`): unknown device or no table loaded
for it, wrong arity or argument type, `l` outside the table's L axis (no extrapolation; a 1e-9
relative slack absorbs `0.5u` vs `0.5e-6` rounding), `gm_id` not reachable, `id ≤ 0`, or a
non-finite result. The functions never panic.

### Query semantics

Every function evaluates one operating point at (gm_id, l) at the device's nominal VDS (VDD/2
by default, stored in the LUT):

1. the two VDS rows bracketing the nominal VDS are blended linearly;
2. VGS is found by inverse lookup of gm/Id along VGS, scanning down from the highest VGS while
   gm/Id keeps rising (the strong → weak inversion branch). The scan stops at the first local
   maximum: below it leakage (or a model-bin artefact) makes gm/Id fall again. Reachable
   targets therefore form `[gm/Id at VDD, first peak]` per L row;
3. inside the bracketing VGS cell, id, gm, gds and Cgg are interpolated geometrically (exact for
   the exponential weak-inversion current);
4. the two L rows bracketing `l` are blended in log L, quantities geometrically (exact for the
   power laws jd ∝ 1/L, ft ∝ 1/L²), VGS linearly; a length on the grid uses that row alone.

Against a synthetic EKV device (closed-form gm, gds, Cgg; `src/synth.rs`) sampled on the
characterization grid, every function agrees with the exact answer within 2 % (worst measured
1.5 %, at off-grid L), VGS within 2 mV.

## `gmid.chars`

Project-root config in line-record syntax (format spec §2; no `schemify` header). One
`device <name>` record per device; `<name>` is a word and is what `dev` matches.

| Attribute | Meaning |
|---|---|
| `lut <path>` | project-relative LUT file; default `.schemify/gmid/<name>.glut` |
| `deck <path>` | project-relative SPICE file with the model includes and options; its presence makes the device characterizable |
| `inst "<template>"` | instance line; `{n}` instance number, `{l}` / `{w}` in µm; nodes `d g s b` |
| `op "<template>"` | OP-parameter vector prefix; `{n}` instance number; the sweep saves `<prefix>[id]`, `[gm]`, `[gds]`, `[cgg]` |
| `polarity n\|p` | default `n`; `p` sweeps negative VGS/VDS and stores magnitudes |
| `vdd <V>` | supply (required with `deck`) |
| `l <µm>…` | lengths to simulate (required with `deck`; sorted, ≤ 64) |
| `w <µm>` | width simulated at, default 1; results are per metre |
| `note <text>` | provenance text stored in the LUT |

`inst`, `op`, `vdd` and `l` without `deck` are an error, as are unknown attributes, duplicate
devices and paths that leave the project. A device with only `lut` loads an existing table
(load path b). Example (the app's OTA fixture):

```
device nfet_01v8
  lut gmid/nfet_01v8.glut

device pfet_01v8
  lut gmid/pfet_01v8.glut
```

and a characterizable sky130 device (the deck holds `.option scale=1.0u`, `.param
mc_mm_switch=0 mc_pr_switch=0` and the `.include`s of `lod.spice`, `corners/tt/nonfet.spice` and
the device's `__tt.corner.spice` / `__mismatch.corner.spice`):

```
device nfet_01v8
  deck models/sky130_nfet_tt.spice
  inst "X{n} d g s b sky130_fd_pr__nfet_01v8 W={w} L={l} nf=1"
  op "@m.x{n}.msky130_fd_pr__nfet_01v8"
  vdd 1.8
  l 0.15 0.18 0.25 0.5 1 2 4 8
  note "sky130_fd_pr tt, 27 C"
```

## Event flow

- `Load`: contribute `functions` (generation 0), `commands` (`gmid.characterize`,
  `gmid.reload`) and two `ui` items under `menu.tools`; read `gmid.chars`, then each device's
  LUT. A missing config or LUT only logs (info).
- `gmid.reload`: re-read `gmid.chars` and every LUT (run it after editing the config).
- `gmid.characterize [device]` (all characterizable devices without an argument): write
  `.schemify/gmid/work/<dev>.sp` → spawn `ngspice -b .schemify/gmid/work/<dev>.sp` (cwd = the
  project root) → read every `.schemify/gmid/work/<dev>.<n>.data` (one per length, all
  requested at once, each parsed in its own `IoDone` event) → when the last arrives, build the
  LUT, load it, re-declare the functions → write the `lut` path. Each step starts on the previous
  step's `IoDone`, so the plugin does not rely on the host running requests in issue order;
  ≤ 256 requests pending. A failed write, a non-zero ngspice exit (the log carries the tail of
  its output), a missing or malformed data file only log an error; the loaded table is kept.

`ponytail:` characterization uses the `spawn` capability, not core `run` requests: the editor
does not execute run requests yet, and the sweep needs the raw (L, VDS, VGS) grid rather than
bench outputs. Switch to a generated `.shbench` + `run` once the run spine reads results back.

### The sweep

One netlist per device: every L is a parallel instance on shared nodes (`Vgs g`, `Vds d`,
`Vs s`, `Vb b`, all from ground), one nested DC sweep `dc Vgs 0 VDD VDD/72 Vds VDD/12 VDD
VDD/12` (negated for p), and per instance one `wrdata` file (`wr_singlescale` + `wr_vecnames`) of
its four OP vectors. gm, gds and Cgg come from the model's OP parameters (no numeric derivatives,
unlike the salvage). The builder takes the VGS axis from the first scale run, checks every block
repeats it, and derives the VDS axis from the planned step.

Fuel: each 73 × 12 file is ~55 KB; the worst event (the last file, which also builds and encodes
the LUT) measured ≈ 15 M fuel against 10^8 per event (`tests/guest.rs`, which drives the built
module with wasmi). A single file for all 8 lengths measured ≈ 150 M and did not fit — hence
one file per length.

`ponytail:` the grid is fixed (25 mV × 150 mV at 1.8 V) and queries are at the nominal VDS only;
add `vgs_step`/`vds_step` attributes or a VDS argument when someone needs them.

## `.glut` v2 format

Little-endian, no padding:

| Field | Type |
|---|---|
| magic `GLUT` | 4 bytes |
| version = 2 | u32 |
| polarity (0 n, 1 p) | u8 |
| vdd (V), nominal VDS (V) | f64, f64 |
| device name | u16 length + UTF-8 (≤ 256) |
| provenance | u16 length + UTF-8 (≤ 4096) |
| nL, nVDS, nVGS | u16 × 3 (each ≤ 4096, product ≤ 2^22) |
| L (m), VDS (V), VGS (V) axes | f64 × (nL + nVDS + nVGS), strictly ascending |
| id/W (A/m), gm/W (S/m), gds/W (S/m), Cgg/W (F/m) | f32 × cells each, cell `((l·nVDS)+d)·nVGS+g` |

The decoder checks every declared size against the caps and the bytes present before
allocating, rejects trailing bytes, non-finite values, unsorted axes and a nominal VDS outside
the VDS axis, and never panics (tested on every truncation of a valid file and on random bit
flips).

Why binary rather than line records: a LUT is ~30 k numbers per quantity block. As text it would
be ~3.5× larger (≈ 400 KB per device instead of 113 KB) and every load would run a float parser over
it inside the interpreter's fuel budget; the binary form is a bounds-checked copy. LUTs are
derived artefacts (regenerated from the PDK, default location under the git-ignored
`.schemify/`), so they do not need line-record diffs; the human-edited part, `gmid.chars`, is
line records.

## Samples

`crates/app/tests/fixtures/ota/gmid/{nfet,pfet}_01v8.glut` are real ngspice-44.2 runs of sky130
`sky130_fd_pr__nfet_01v8` / `__pfet_01v8` (tt, 27 °C, VDD 1.8 V, W = 1 µm, L = 0.15, 0.18, 0.25,
0.5, 1, 2, 4, 8 µm — the W = 1 model bins), made by

```
cargo run --manifest-path plugins/Cargo.toml -p gmid-lut --example characterize -- \
    tests/corpus/.cache/import/sky130_fd_pr_models crates/app/tests/fixtures/ota/gmid
```

which runs the plugin's own config parser, netlist generator, `wrdata` parser and LUT builder
with `std::process` in place of the host's `spawn`. Reachable gm/Id: nfet from ≈ 1.2 up to a
26–32 V⁻¹ peak at every L; pfet ≈ 1.2–2.3 up to ≈ 30 V⁻¹ for L 0.25–4 µm, but only ≈ 15 V⁻¹ at
L 0.15/0.18 µm and 17 V⁻¹ at 8 µm (those bins show a local gm/Id maximum in moderate inversion,
where the monotone-branch scan stops). At gm/Id = 15, L = 0.5 µm the nfet has jd ≈ 2.6 A/m,
gain ≈ 112 and ft ≈ 2.4 GHz.

## Building and testing

- `plugins/build.sh` builds every guest for `wasm32-unknown-unknown` and installs
  `gmid_lut.wasm` into `crates/app/tests/fixtures/ota/plugins/` (the OTA demo project loads it
  through its manifest; limit 200 KB, it is ≈ 156 KB). The app does not embed it: D20 files
  open without it, on cached values.
- Native tests (the guest glue is `cfg(target_arch = "wasm32")`, everything else is plain Rust):
  `cargo test --manifest-path plugins/Cargo.toml -p gmid-lut`. Unit tests cover the format round
  trip and malformed input, the config parser, the netlist, the `wrdata` parser and builder,
  accuracy against the analytic model, monotonicity (jd, ft, VGS fall and W grows with gm/Id;
  gain grows and ft falls with L), error paths, and the plausibility of the committed sky130
  samples. `tests/guest.rs` loads the built `gmid_lut.wasm` in wasmi (run `build.sh` first) and
  drives Load → config → LUT load → characterize → spawn → 8 data files → LUT write →
  `schemify_call`, asserting every event fits the default fuel and the guest's answers equal
  the native `eval` on the LUT it wrote.
