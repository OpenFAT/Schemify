# PDKs per project: install sky130, draw with `tech:` devices, switch to gf180mcu

The end-to-end flow of [docs/spec/pdk.md](../../spec/pdk.md). You need `ciel` on `PATH`
(`pip install ciel`, or a venv) and the `schemify` CLI.

## 1. Load the PDK manager

In the project's `schemify.project`:

```
schemify 1 project

plugin pdk-manager path plugins/pdk-manager/pdk_manager.wasm
  grant read_project
  grant write_project
  grant spawn:ciel
  grant spawn:schemify
```

(`plugins/pdk-manager` holds `pdk_manager.wasm`; copy it into your project or point the path at
a checkout.)

## 2. Install sky130

From the panel: **View → PDKs**, pick `sky130` in the family dropdown, then **Install** on a
version. Or by command name, from a key binding, `schemify.cmd` in `init.lua` or the JSON-RPC
command stream (D27):

```
pdk.list_remote sky130
pdk.install sky130 1e3554706ddbb2529193bdabbc7128296f3cb44a
```

The panel shows each step (`fetch (ciel)`, `import (schemify)`, `tech.map`, `state`) and the
tail of its output. The result lands in `.schemify/pdks/sky130/<version>/` (`root/` from ciel,
`lib/` from `schemify import`, `tech.map`, `STATE`). Nothing else changes yet.

## 3. Use it

**Use** on the row, or `pdk.use sky130 1e3554706ddbb2529193bdabbc7128296f3cb44a`. The manifest
gains the PDK's libraries and the technology map, and the editor reloads the project:

```
lib sky130_fd_pr path .schemify/pdks/sky130/1e35…/lib/sky130_fd_pr
…
lib tech map .schemify/pdks/sky130/1e35…/tech.map

set PDK_ROOT .schemify/pdks/sky130/1e35…/root
set PDK sky130A
set PDK_MODELS ${PDK_ROOT}/sky130A/libs.tech/combined/sky130.lib.spice
```

## 4. A technology-neutral inverter

Place `tech:pmos` and `tech:nmos` (they draw as the core MOS symbols) and size them with the
class params `w`, `l`, `nf`, `m` in metres:

```
inst m00001 tech:pmos
  at 0 -50 R0
  name M1
  w 2u
  l 0.5u
  nf 2
inst m00002 tech:nmos
  at 0 50 R0
  name M2
  w 1u
  l 0.5u
```

`schemify netlist inv.shcomp` (or `n` in the editor) emits sky130's devices, the map turning
metres into sky130's microns:

```
XM1 Y A VDD VDD sky130_fd_pr__pfet_01v8 L=0.5 W=2
+ nf=2 … mult=1 m=1
XM2 Y A VSS VSS sky130_fd_pr__nfet_01v8 L=0.5 W=1
+ nf=1 … mult=1 m=1
```

A bench corner that follows the PDK reads its models through `PDK_MODELS`:

```
corner tt
  lib ${PDK_MODELS} tt
```

## 5. Switch to gf180mcu

```
pdk.install gf180mcu 1e3554706ddbb2529193bdabbc7128296f3cb44a
pdk.use gf180mcu 1e3554706ddbb2529193bdabbc7128296f3cb44a
```

The same schematic now netlists as gf180mcu devices, with no edit to the drawing:

```
XM1 Y A VDD VDD pfet_03v3 L=5e-7 W=2e-6
+ nf=2 … m=1
```

The corner's section names differ (`typical` instead of `tt` in gf180mcu); until corner mapping
lands (spec §7), edit the bench corner when switching. Cells placed directly from a PDK library
(`sky130_fd_pr:nfet_01v8`) do not retarget: they become unresolved (E001) once their library
leaves the manifest.

## 6. Clean up

`pdk.remove sky130 <version>` deletes a version that is neither active nor pinned by
`schemify.lock`. `pdk.prune` lists every such version (incomplete installs included) and
`pdk.prune confirm` deletes them.

## 7. FreePDK45 (NCSU, 45 nm, BSIM4)

Grant `spawn:curl` and `spawn:tar` as well. `freepdk45` installs one pinned version: GitHub's
archive of a mirror of the v1.4 release (NCSU's own download needs a registration), checked
against its sha256 before it is unpacked:

```
pdk.install freepdk45 5b93a0b1e12d
pdk.use freepdk45 5b93a0b1e12d
```

The kit has no xschem symbols, so the install generates its devices
(`freepdk45:{n,p}mos_{vtl,vtg,vth,thkox}`, drawn like the core MOS symbols) and a corner
library with sections `tt ff ss`. The manifest gains:

```
lib freepdk45 path .schemify/pdks/freepdk45/5b93a0b1e12d/lib/freepdk45
lib tech map .schemify/pdks/freepdk45/5b93a0b1e12d/tech.map

set PDK_ROOT .schemify/pdks/freepdk45/5b93a0b1e12d/root
set PDK_MODELS .schemify/pdks/freepdk45/5b93a0b1e12d/models/corners.lib
```

The inverter of §4 netlists as BSIM4 M elements (`tech:` core = VTL; `flavor hvt` is VTH,
`flavor io` THKOX):

```
MM1 Y A VDD VDD PMOS_VTL w=2e-6 l=5e-7 nf=2 m=1
MM2 Y A VSS VSS NMOS_VTL w=1e-6 l=5e-7 nf=1 m=1
```

A DC-sweep bench at the project root, with the nominal 1.1 V supply:

```
schemify 1 shbench
  corner tt

inst 0x0001 ./inv
  at 0 0 R0
  name X1

label 0n0001 -40 0 R0 a
label 0n0002 40 0 R0 y
label 0n0003 0 -30 R0 vdd
label 0n0004 0 30 R0 0

var vdd 1.1

corner tt
  lib ${PDK_MODELS} tt

output vm "cross(v(y)-v(a),0)"

code dc spice
  | VDD vdd 0 {vdd}
  | VIN a 0 0
  | .dc VIN 0 {vdd} {vdd/200}
```

`schemify run inv_tb.shbench` gives `vm` ≈ 0.54 V. If you registered with NCSU, install your
own copy in place instead (nothing is copied; `pdk.remove` never deletes it):

```
pdk.install freepdk45 ncsu /opt/FreePDK45-1.4      # the directory that holds FreePDK45/
```

## 8. ASAP7 (ASU/Arm, 7 nm FinFET, BSIM-CMG)

Grant `spawn:git`, and name OpenVAF in `init.lua` (nixpkgs installs it as `openvaf-r`):

```
schemify.sim("openvaf", { path = "openvaf-r" })
```

```
pdk.install asap7 58d72c9d291e186a77468586ab0c43d8a21eda6a
pdk.use asap7 58d72c9d291e186a77468586ab0c43d8a21eda6a
```

The install checks out the kit and the BSIM-CMG Verilog-A source (ngspice has no built-in
level 72), rewrites the kit's TT/FF/SS cards for the compiled module, and generates
`asap7:{n,p}mos_{rvt,lvt,slvt,sram}`. A device is sized in fins: the map turns the class
width into `nfin = ceil(w / nf / 70.5n)`; the gate length is the kit's fixed 21 nm.

```
NM1 Y A VDD VDD pmos_rvt nfin=15 nf=2 m=1
NM2 Y A VSS VSS nmos_rvt nfin=15 nf=1 m=1
.control
pre_osdi bsimcmg.osdi
.endc
```

The same bench with `var vdd 0.7` runs unchanged (`lib ${PDK_MODELS} tt`): the runner
compiles `bsimcmg.osdi` once into `.schemify/build/` and `vm` is ≈ 0.36 V. A copy of the kit
inside the project installs in place with `pdk.install asap7 mine kits/asap7` (the path must be
project-relative: the plugin reads its cards).
