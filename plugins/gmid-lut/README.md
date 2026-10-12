# gmid-lut — gm/Id sizing plugin

A wasm guest (plugin ABI v1) that exports pure expression functions `gmid.*` (D20) backed by
per-device lookup tables (LUTs), loads those tables from the project, and characterizes devices
with ngspice or ESPice. Port of the UW-ASIC `gmid-lut` salvage (D26, own code): `lut.rs` and the
GmIDVisualizer slice/netlist logic, rewritten in Rust without the C++ runner, serde or JSON.

The app does not embed it: a project loads it through its manifest, e.g.

```
plugin gmid path plugins/gmid_lut.wasm
  grant read_project
  grant write_project
  grant spawn:ngspice
  grant spawn:espice
```

`read_project` loads `gmid.chars` and the LUTs; `write_project` and `spawn:ngspice` /
`spawn:espice` (the simulator a device's `sim` names) are needed only for characterization.

## Functions

All arguments and results are SI. `dev` is a device name from `gmid.chars` (a string), `gm_id`
is gm/Id in 1/V, `l` the channel length in metres, `id` the drain current in amperes, `vds` the
drain-source voltage magnitude in volts.

| Function | Arity | Result |
|---|---|---|
| `gmid.vgs(dev, gm_id, l)` | 3 | \|VGS\| (V) |
| `gmid.jd(dev, gm_id, l)` | 3 | current density \|Id\|/W (A/m) |
| `gmid.w(dev, gm_id, l, id)` | 4 | width W = id / jd (m) |
| `gmid.gm(dev, gm_id, l, id)` | 4 | gm = gm_id · id (S), only where gm_id is reachable |
| `gmid.gds(dev, gm_id, l, id)` | 4 | gds = gm / gain (S) |
| `gmid.gain(dev, gm_id, l)` | 3 | intrinsic gain gm/gds (V/V) |
| `gmid.ft(dev, gm_id, l)` | 3 | transit frequency gm / (2π·Cgg) (Hz) |
| `gmid.<f>_at(…, vds)` | +1 | each of the above at \|VDS\| = `vds` instead of the nominal VDS |
| `gmid.l(dev, l)` | 2 | `l` clamped into the device table's L range (m): the nearest length the PDK offers, e.g. ASAP7's one fixed 21 nm gate |

The `_at` variants (`gmid.vgs_at(dev, gm_id, l, vds)`, …, `gmid.w_at(dev, gm_id, l, id, vds)`)
take `vds` as their last argument; at the nominal VDS they return exactly what the plain
function returns. `vds` must lie on the table's VDS axis (0.15 V … VDD for a default
characterization).

Registry records (`contribute functions`, plugin-abi §7): `function <name> <fn_id> <arity>` with
`fn_id = index + 16 × generation`, index in the table order above (vgs 0 … ft 6, then vgs_at 7
… ft_at 13, then l 14). The generation starts at 0 on `Load` and advances whenever the loaded tables
change (a LUT loads or a characterization finishes); the plugin then re-declares the set, so the
host's registry epoch moves and it re-evaluates every expression that calls `gmid.*` (an
identical re-declaration would be ignored). `schemify_call` reads `fn_id % 16`.

Errors: every failure is `E_RANGE` (the host reports `X010`): unknown device or no table loaded
for it, wrong arity or argument type, `l` outside the table's L axis (no extrapolation; a 1e-9
relative slack absorbs `0.5u` vs `0.5e-6` rounding), `vds` outside the VDS axis (1 nV slack),
`gm_id` not reachable, `id ≤ 0`, or a non-finite result. The functions never panic.

### Query semantics

Every function evaluates one operating point at (gm_id, l), at `vds` for the `_at` variants and
at the device's nominal VDS (VDD/2 by default, stored in the LUT) for the others:

1. the two VDS rows bracketing that VDS are blended linearly;
2. VGS is found by inverse lookup of gm/Id along VGS, scanning down from the highest VGS (gm/Id
   rises from strong towards weak inversion) to the first VGS where gm/Id reaches `gm_id`. A dip
   on the way (a local maximum in moderate inversion, which sky130 pfet bins show) is crossed;
   cutoff (no current) ends the row. Reachable targets therefore form `[gm/Id at VDD, the row's
   highest gm/Id]` per L row, and VGS still falls as `gm_id` rises;
3. inside the bracketing VGS cell, id, gm, gds and Cgg are interpolated geometrically (exact for
   the exponential weak-inversion current);
4. the two L rows bracketing `l` are blended in log L, quantities geometrically (exact for the
   power laws jd ∝ 1/L, ft ∝ 1/L²), VGS linearly; a length on the grid uses that row alone.

Against a synthetic EKV device (closed-form gm, gds, Cgg; `src/synth.rs`) sampled on the
characterization grid, every function agrees with the exact answer within 2 % (worst measured
1.5 %, at off-grid L), VGS within 2 mV, at the nominal VDS and off it (id and gm are linear in
VDS in that model, so the VDS blend adds no error beyond f32 storage).

## `gmid.chars`

Project-root config in line-record syntax (format spec §2; no `schemify` header). One
`device <name>` record per device; `<name>` is a word and is what `dev` matches.

| Attribute | Meaning |
|---|---|
| `lut <path>` | project-relative LUT file; default `.schemify/gmid/<name>.glut` |
| `deck <path>` | project-relative SPICE file with the model includes and options; its presence makes the device characterizable |
| `inst "<template>"` | instance line; `{n}` instance number, `{l}` / `{w}` in µm; nodes `d g s b` |
| `op "<template>"` | OP-parameter vector prefix; `{n}` instance number; the ngspice sweep saves `<prefix>[<id>]`, `[gm]`, `[gds]`, `[cgg]` (required for ngspice, ignored by espice) |
| `id <name>` | OP name of the drain current, default `id` (BSIM4); `ids` for BSIM-CMG through OSDI (ASAP7); ignored by espice |
| `sim ngspice\|espice` | the simulator, default `ngspice` (see "The sweep"); espice needs `inst` to name the nodes `d g s b` first after the instance name, in that order, and at most 8 lengths |
| `polarity n\|p` | default `n`; `p` sweeps negative VGS/VDS and stores magnitudes |
| `vdd <V>` | supply (required with `deck`) |
| `l <µm>…` | lengths to simulate (required with `deck`; sorted, ≤ 64) |
| `w <µm>` | width simulated at, default 1; results are per metre |
| `note <text>` | provenance text stored in the LUT |

`inst`, `op`, `id`, `sim`, `vdd` and `l` without `deck` are an error, as are unknown attributes, duplicate
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
  `.schemify/gmid/work/<dev>.sp` → spawn the simulator (cwd = the project root): ngspice
  `ngspice -b .schemify/gmid/work/<dev>.sp`, then read every `.schemify/gmid/work/<dev>.<n>.data`
  (one per length, all requested at once, each parsed in its own `IoDone` event); ESPice
  `espice -b --tokenizer=ngspice -r .schemify/gmid/work/<dev>.raw .schemify/gmid/work/<dev>.sp`,
  then read that one binary raw file (parsed in one event) → when the last block arrives, build
  the LUT, load it, re-declare the functions → write the `lut` path. Each step starts on the previous
  step's `IoDone`, so the plugin does not rely on the host running requests in issue order;
  ≤ 256 requests pending. A failed write, a non-zero simulator exit (the log carries the tail of
  its output), a missing or malformed data or raw file only log an error; the loaded table is
  kept.

`ponytail:` characterization uses the `spawn` capability, not core `run` requests, because the
ABI does not allow it yet: a plugin `run` record names a bench that is already a project cell
(a `.shbench` the plugin writes is indexed only on the next project load), and `RunDone` /
the `Results` snapshot carry the job's scalar outputs, not its raw file or directory, while the
LUT needs the whole (L, VDS, VGS) grid of four OP quantities. Switch to a generated `.shbench`
+ `run` once the ABI can run a written bench and hand back the job directory (BACKLOG).

### The sweep

The simulator is chosen per device by `sim` in `gmid.chars`, not by the user config's
`schemify.opt.simulator`: a plugin cannot read the user config (the ABI gives it the project and
the host's tables, not `init.lua`), and a LUT is a project artefact whose provenance should not
change with who regenerates it. `spawn` resolves the program on `PATH` with the app's
environment, so ESPice's Verilog-A compiles (ASAP7) need `ZIG` set where the app starts.

**ngspice.** One netlist per device: every L is a parallel instance on shared nodes (`Vgs g`, `Vds d`,
`Vs s`, `Vb b`, all from ground), one nested DC sweep `dc Vgs 0 VDD VDD/72 Vds VDD/12 VDD
VDD/12` (negated for p), and per instance one `wrdata` file (`wr_singlescale` + `wr_vecnames`) of
its four OP vectors. gm, gds and Cgg come from the model's OP parameters (no numeric derivatives,
unlike the salvage). The builder takes the VGS axis from the first scale run, checks every block
repeats it, and derives the VDS axis from the planned step.

Fuel: each 73 × 12 file is ~55 KB; the worst event (the last file, which also builds and encodes
the LUT) measured ≈ 15 M fuel against 10^8 per event (`tests/guest.rs`, which drives the built
module with wasmi). A single file for all 8 lengths measured ≈ 150 M and did not fit — hence
one file per length.

**ESPice** (espice.md §4c recipe ii; ESPice has no device OP vectors and ignores `.control`).
One netlist per device:

```
* gmid-lut espice sweep
* gm/Id characterization of <dev> (generated by the gmid-lut plugin)
.option search="../../.."            ESPice resolves paths from the including file's directory,
.include "<deck>"                    never the cwd: this reaches the project root from the work dir
.param vgs=0 vds=0
Vda<n> da da<n> 0                    per length n: copy a, drain and gate behind 0 V ammeters
Vga<n> ga ga<n> 0
<inst, {n} → a<n>, nodes da<n> ga<n> s b>
Vdb<n> db db<n> 0                    copy b, drain ammeter, gate on the shared node
<inst, {n} → b<n>, nodes db<n> gb s b>
Vgsa ga 0 dc {vgs} ac 1              AC on copy a's gate
Vdsa da 0 dc {vds}
Vgsb gb 0 dc {vgs}
Vdsb db 0 dc {vds} ac 1              AC on copy b's drain
Vs s 0 0
Vb b 0 0
.save i(vda1) i(vga1) i(vdb1) …
.op
.ac lin 1 1e6 1e6
.step param vds list <VDD/12 … VDD>  outer
.step param vgs list <0 … VDD>       inner: ESPice varies the last .step fastest
```

(VGS, VDS negated for p.) ESPice writes one binary raw file with an `.op` and an `.ac` plot per
point, named `… (vds=…, vgs=…)`; the parser checks every point against the planned grid and
order, reads the variable indices from the first pair and requires every later plot's header to
match it, and takes **Id = i(vda)** at the OP, **gm = Re i(vda)**, **gds = Re i(vdb)** and
**Cgg = Im i(vga) / 2πf** (f = 1 MHz). These are terminal quantities, so they differ from
ngspice's OP vectors by definition, and the LUT's provenance says so ("espice characterization
(… terminal id and gds; cgg includes overlap …)"):

- id and gds include junction leakage and GIDL (`@m[id]` is the channel current), visible in deep
  subthreshold and in the linear region of long devices;
- Cgg includes the overlap capacitances (`@m[cgg]` is intrinsic), so ft from an ESPice LUT is
  lower.

Measured on FreePDK45 (`tests/espice.rs`, ESPice 1.0.0 vs ngspice 44.2, L 0.05/0.2/1 µm, VDD
0.8 V, points with Id > 1 µA/µm), relative differences, median (worst):

| | id | gm | gds | Cgg ESPice / ngspice |
|---|---|---|---|---|
| NMOS_VTL | 8.9e-6 (1.4e-3) | 9.8e-6 (5.1e-3) | 1.6e-5 (5.8e-3) | 1.31 (1.04 … 8.5) |
| PMOS_VTL | 2.2e-5 (5.6e-3) | 2.5e-5 (1.8e-2) | 2.6e-5 (9.5e-3) | 1.28 (1.04 … 8.5) |

The worst id points are definitional: at L = 1 µm, VGS 0.7 V, VDS 0.067 V ngspice's own terminal
current i(vd) = 1.062016e-5 A matches ESPice's 1.062021e-5 A while `@m[id]` = 1.063035e-5 A.
ngspice takes 0.05 s for that grid, ESPice ≈ 3 s (876 operating points, each with its own AC).

Fuel: the raw file is ≈ 1.7 MB at 8 lengths; parsing and building measured ≈ 73 M fuel of the
10^8 per event (`tests/guest.rs`), hence at most 8 lengths with espice. `ponytail:` one deck
for all lengths; split it into decks of ≤ 8 lengths run in sequence when a device needs more.

**ASAP7 on ESPice.** ESPice has no built-in BSIM-CMG and refuses OSDI (`pre_osdi` is fatal),
so the deck loads the kit's Verilog-A source itself, and the instance connects the module's
5th (thermal) port, grounded (the cards set `shmod = 0`), with `mfactor=` rather than `m=` if a
multiplier is written:

```
# gmid/asap7_tt_espice.sp
.hdl ".schemify/pdks/asap7/<h>/va/code/bsimcmg/vacode/bsimcmg.va"
.lib ".schemify/pdks/asap7/<h>/models/corners.lib" tt

# gmid.chars
device nmos_asap7
  deck gmid/asap7_tt_espice.sp
  inst "N{n} d g s b 0 nmos_rvt nfin=14 l={l}u"
  sim espice
  vdd 0.8
  l 0.021
  w 0.987
```

The first run compiles BSIM-CMG (≈ 4 min, 2.8 GB; ESPice caches it); measured: Id =
6.284561e-5 A at VGS = VDS = 0.4 V, the ngspice + OpenVAF value (`tests/espice.rs`).

`ponytail:` the grid is fixed (25 mV × 150 mV at 1.8 V); add `vgs_step`/`vds_step` attributes
when someone needs a finer one.

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
with `std::process` in place of the host's `spawn`. Reachable gm/Id: nfet from ≈ 1.2 up to
26–32 V⁻¹ at every L; pfet from ≈ 1.2–2.3 up to ≈ 29–31 V⁻¹ for L 0.25–8 µm, 27 V⁻¹ at L 0.18 µm
and 23 V⁻¹ at L 0.15 µm (still rising at VGS = 0). The pfet bins at L 0.15, 0.18 and 8 µm show a
local gm/Id maximum in moderate inversion (15.1, 15.6 and 17.3 V⁻¹) followed by a dip; the scan
crosses it (query step 2), so targets above the local maximum answer at a lower VGS past the
dip. At gm/Id = 15, L = 0.5 µm the nfet has jd ≈ 2.6 A/m, gain ≈ 112 and ft ≈ 2.4 GHz.

## Building and testing

- `plugins/build.sh` builds every guest for `wasm32-unknown-unknown` and installs
  `gmid_lut.wasm` into `crates/app/tests/fixtures/ota/plugins/` (the OTA demo project loads it
  through its manifest; limit 200 KB, it is ≈ 156 KB). The app does not embed it: D20 files
  open without it, on cached values.
- Native tests (the guest glue is `cfg(target_arch = "wasm32")`, everything else is plain Rust):
  `cargo test --manifest-path plugins/Cargo.toml -p gmid-lut`. Unit tests cover the format round
  trip and malformed input, the config parser, the netlist, the `wrdata` parser and builder,
  accuracy against the analytic model (also off the nominal VDS), monotonicity (jd, ft, VGS
  fall and W grows with gm/Id; gain grows and ft falls with L), a synthetic gm/Id dip, the `_at`
  variants (equal to the plain functions at the nominal VDS), error paths, and the plausibility
  of the committed sky130 samples; the ESPice config rules, deck text, a recorded ESPice
  `.op`/`.ac` plot pair (`tests/fixtures/espice_op_ac.raw`), and the raw parser's negative
  space (truncation, ASCII, off-grid, unpaired or reordered plots, a header that changes).
  `tests/guest.rs` loads the built `gmid_lut.wasm` in wasmi (run `build.sh` first) and
  drives Load → config → LUT load → characterize → spawn → 8 data files (ngspice) or one
  real-size raw file (espice) → LUT write → `schemify_call`, asserting every event fits the
  default fuel and the guest's answers equal the native `eval` on the LUT it wrote.
- Real simulators (ignored): `tests/espice.rs` characterizes FreePDK45 with both simulators and
  compares them (`GMID_FREEPDK45_LIB` = a pdk-manager `corners.lib`, `ngspice` and `espice` on
  `PATH`), and ASAP7 with ESPice (`GMID_ASAP7_PDK` = a pdk-manager ASAP7 install, `ZIG`):
  `cargo test --manifest-path plugins/Cargo.toml -p gmid-lut --test espice -- --ignored`.
