# PDK management and technology classes

Status: draft spec, 2026-10-12 (user decisions: PDKs installed **per project**; designs can switch
**across technologies**, not only versions; spec first, then an MVP). Normative once the MVP
lands; until then, open questions are marked **Q**.

Two parts:
1. the `pdk-manager` **plugin**, which installs, switches and removes PDKs (§1–§4);
2. **technology classes** in the core, which let one schematic retarget between PDKs (§5–§8).

Related: D10 (lib:cell references, manifest, lockfile), D40 (lockfile bump → "default changes"
report), D14 (device readers), D24 (xschem import), [import.md](import.md) (the importer the plugin
runs), [plugin-abi.md](plugin-abi.md) §5 (capabilities).

---

## 1. Store layout (per project)

Every installed PDK lives under the project's git-ignored `.schemify/` directory:

```
.schemify/pdks/
  <family>/<version>/
    root/          ciel's PDK root for this version (ciel --pdk-root), e.g. root/sky130A/libs.tech/…
    lib/           the Schemify library imported from the PDK's xschem symbols (schemify import -o)
    tech.map       technology-class map for this PDK (§6), shipped by the plugin's recipe
    STATE          one line: `ok` when install finished; anything else = incomplete (§3)
```

- `<family>` is a recipe name (§2): `sky130`, `gf180mcu`, `ihp-sg13g2`, later `asap7`.
- `<version>` is the source's version id (an open_pdks commit for ciel families, a git rev for
  git recipes), never `latest`.
- Nothing outside `.schemify/pdks/` is written. Two projects using the same version each hold a
  copy (accepted cost of the per-project choice).

## 2. Recipes (data, in the plugin)

A recipe is one table row; adding a PDK is adding a row (no code):

| Field | sky130 | gf180mcu | ihp-sg13g2 | asap7 (follow-up) |
|---|---|---|---|---|
| source | `ciel` family `sky130` | `ciel` family `gf180mcu` | `ciel` family `ihp-sg13g2` | `git` repo + rev |
| variant | `sky130A` | `gf180mcuD` | `ihp-sg13g2` | — |
| libraries (`-l`) | `sky130_fd_pr` | `gf180mcu_fd_pr` | (default) | — |
| xschem symbols | `libs.tech/xschem` | `libs.tech/xschem` | `libs.tech/xschem` | none: devices come only through `tech:` classes (§5) |
| models file | `libs.tech/combined/sky130.lib.spice` | `libs.tech/ngspice/sm141064.ngspice` | `libs.tech/ngspice/models/cornerMOSlv.lib` | the repo's BSIM-CMG model cards |
| corners | `tt ff ss sf fs` | `typical ff ss sf fs` | `mos_tt mos_ff mos_ss mos_sf mos_fs` | `TT FF SS` |
| tech.map | shipped | shipped | shipped | shipped |

**Q1:** gf180mcu and ihp-sg13g2 model paths and corner section names above are from memory and must
be verified against the installed trees when the recipe is written (the MVP test installs each).

## 3. Commands (plugin)

All commands are plugin commands (D27 table), reachable from the panel (§4), keys and scripts.

| Command | Effect |
|---|---|
| `pdk.list_remote <family>` | `ciel ls-remote --pdk-family <family>` → versions shown in the panel |
| `pdk.install <family> <version>` | 1. `ciel fetch --pdk-family F --pdk-root .schemify/pdks/F/V/root [-l LIB]… V`. 2. `schemify import <root>/<variant>/libs.tech/xschem -o .schemify/pdks/F/V/lib --xschemrc <root>/<variant>/libs.tech/xschem/xschemrc`. 3. write `tech.map`. 4. write `STATE` = `ok`. Any failure leaves `STATE` absent and reports the step's stderr. |
| `pdk.use <family> <version>` | Makes the version active for the project (§3.1). Refused unless `STATE` is `ok`. |
| `pdk.remove <family> <version>` | Refused while the version is active or referenced by `schemify.lock`; otherwise deletes `.schemify/pdks/F/V/` (§3.2). |
| `pdk.prune` | Removes every installed version that is neither active nor locked, and every incomplete install. Lists what it will remove and needs a second invocation (`pdk.prune confirm`) to act. |

### 3.1 Using a version

`pdk.use F V` rewrites the project manifest (`schemify.project`) in one write:

```
lib <F> path .schemify/pdks/<F>/<V>/lib
lib tech map <F>                       # §5: tech: cells resolve through this PDK
set PDK_ROOT .schemify/pdks/<F>/<V>/root
set PDK <variant>
set PDK_MODELS ${PDK_ROOT}/<variant>/<models file>
```

replacing any previous `lib <F> …` / `lib tech …` / those `set` lines, then asks the app to reload the
project (§9 core change: `project.reload`). The lockfile (D10) then pins the library hash; when the
hash changes, the D40 "default changes" report runs and is shown in the visual diff. Bench corners
that should follow the PDK use `lib ${PDK_MODELS} <section>` (§7).

Switching **versions** of one family is `pdk.use F V2`. Switching **technology** is `pdk.use F2 V`
plus §5: cells referenced as `tech:<class>` retarget; cells referenced directly as `<F>:<cell>`
keep pointing at the old family and become unresolved (E001) if its `lib` line goes, which the
checks panel lists with a "Retarget" action (§8).

### 3.2 Deletion

The plugin capability model (plugin-abi §5) has no delete. **ABI change (§9):** `write_project`
also grants `fs_remove(path)`, removing a project-relative file or directory tree, refused for
paths outside `.schemify/` and for `schemify.project`/`schemify.lock`. Ciel's own tree may instead be
removed with `ciel rm --pdk-family F --pdk-root … --yes V`; `fs_remove` still removes `lib/`,
`tech.map` and `STATE`.

## 4. Panel (plugin UI)

A docked "PDKs" panel (View → PDKs) in the default-UI style:

- **Installed:** one row per `<family>/<version>`: version (short hash), size on disk, state
  (`active`, `locked`, `incomplete`, `installed`), buttons Use / Remove (greyed when refused, with
  the reason as tooltip).
- **Available:** family dropdown → `ls-remote` versions → Install. Installation progress shows the
  current step and the tail of its output (as the results pane's run log, run.md).
- **Prune:** shows the total it would free, then confirms.

Long steps (`ciel fetch`, import) run through `spawn`, which never blocks the frame (plugin-abi §11
item 12). Grants the plugin needs: `spawn:ciel`, `spawn:schemify`, `read_project`, `write_project`,
`network` (for `ls-remote` without ciel, optional).

## 5. Technology classes (core)

A **technology class** is a technology-neutral device: `tech:nmos`, `tech:pmos`, `tech:res`,
`tech:cap`, `tech:diode`, `tech:npn`, `tech:pnp`. A schematic that uses only `tech:` devices (plus
`core:` sources and passives) retargets between PDKs by changing the manifest's `lib tech map <F>`
line.

- `tech` is a reserved library name; `lib tech map <F>` names the PDK whose `tech.map` resolves it.
  Without that line, `tech:` cells are unresolved (E001, "no technology selected").
- Symbols are the core library's (`core:nmos4`, `core:pmos4`, `core:res`, …), so the drawing does
  not change when the technology does. Pins are the core symbol's.
- Parameters are technology-neutral, in SI units: MOS `w`, `l`, `nf`, `m`, `flavor`
  (`core|io|lvt|hvt`, default `core`); res `value` or `w`/`l` with `flavor` (`poly_hi|poly_lo|diff`);
  cap `value` or `w`/`l` with `flavor` (`mim|mos`).

## 6. `tech.map` (line records, format §2)

```
schemify 1 techmap
  pdk sky130
  vdd 1.8                                  # nominal supply of the core flavor, volts

class nmos core
  cell sky130:nfet_01v8                    # the PDK cell it becomes
  param W {w/1u}                           # PDK param = expression over the class params
  param L {l/1u}
  param nf {nf}
  param mult {m}
  limits l min 0.15u                       # optional, for E005 before netlisting

class nmos io
  cell sky130:nfet_g5v0d10v5
  vdd 3.3
  ...

class nmos core                            # in asap7's map: FinFET
  cell asap7:nmos_rvt
  param nfin {max(1, ceil(w / 27n))}       # fins from the requested width
  param l {l}
```

- Expressions are the D20 expression language over the instance's class params; the result
  replaces the instance's params when the netlister and checks resolve the cell. Files store only
  the class params (D40: overrides only).
- A class/flavor a PDK lacks is an error at check time (E0xx "sky130 has no lvt pmos"), not a
  silent substitution.
- `vdd` lines are exported as the read-only var `vdd_nom` in every bench (docs page, sources), so a
  supply written as `vdd_nom` follows the technology.

## 7. Corners and models

A bench corner that should follow the active PDK writes `lib ${PDK_MODELS} <section>`; the map's
`corners` record lists the PDK's section names in a fixed order (`tt ff ss sf fs` positions), and
the corner names `tt|ff|ss|sf|fs` resolve through it, so `corner tt` means the right section in
every technology. **Q2:** whether corner-name mapping lives in `tech.map` or in the recipe.

## 8. Retargeting existing designs

`pdk.retarget` (core command) converts instances of a family's PDK cells (`sky130:nfet_01v8`)
into `tech:` instances where the active `tech.map` has a class whose `cell` matches and whose param
expressions are invertible for the instance's values (linear scalings like `W = w/1u`); the rest
are listed, not changed. One transaction (one undo), shown in the visual diff before commit.

## 9. Core changes this needs

1. `lib tech map <family>` manifest records and `tech:` resolution in `doc`/`elab` (§5, §6),
   including param expressions and limits; netlisters see the resolved PDK cell.
2. `project.reload` command: re-read `schemify.project` and the lockfile without restarting.
3. plugin ABI: `fs_remove` under `write_project`, scoped to `.schemify/` (§3.2).
4. `vdd_nom` var export into benches (§6).

## 10. MVP scope and tests

- **MVP:** the plugin with sky130, gf180mcu and ihp-sg13g2 (ciel), §3 commands, §4 panel,
  per-project store, `pdk.use` writing the manifest, `pdk.prune`; core §9.1 for `tech:nmos` and
  `tech:pmos` (`core` flavor) with the three maps, §9.2, §9.3. ASAP7, other classes, §7 corner
  mapping and §8 retargeting follow.
- **Oracles:** an install test against a fake `ciel`/`schemify` on PATH (shell scripts writing a
  tiny tree), asserting the store layout, `STATE`, manifest lines and that a failed step leaves no
  `ok`; a real-PDK test (ignored, nightly: actually installs sky130 with ciel and netlists a bench);
  the same `tech:` inverter netlisted under sky130 and gf180 maps produces each PDK's device lines
  with the mapped params (golden netlists); `pdk.remove` refuses the active/locked version;
  `pdk.prune` removes exactly the unreferenced versions; `fs_remove` refuses paths outside
  `.schemify/`.
