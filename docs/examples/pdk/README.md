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
