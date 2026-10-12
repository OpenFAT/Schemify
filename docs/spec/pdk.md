# PDK management and technology classes

Status: **normative for the MVP** (§10: §1–§6 as written here, §9.1–§9.3), 2026-10-12. User
decisions: PDKs installed **per project**; designs can switch **across technologies**, not only
versions. Parts still draft are marked **draft**; open questions **Q**.

Two parts:
1. the `pdk-manager` **plugin** (`plugins/pdk-manager`), which installs, switches and removes
   PDKs (§1–§4);
2. **technology classes** in the core, which let one schematic retarget between PDKs (§5–§8).

Related: D10 (lib:cell references, manifest, lockfile), D40 (lockfile bump → "default changes"
report), D14 (device readers), D24 (xschem import), [import.md](import.md) (the importer the plugin
runs), [plugin-abi.md](plugin-abi.md) §5 (capabilities), [format.md](format.md) (`lib tech map`,
the `techmap` file kind), [netlist.md](netlist.md) §1 (`tech:` resolution).

---

## 1. Store layout (per project)

Every installed PDK lives under the project's git-ignored `.schemify/` directory:

```
.schemify/pdks/
  INSTALLS         one `<family> <version>` line per install started, sorted (the plugin's index:
                   plugins cannot list directories)
  <family>/<version>/
    root/          ciel's PDK root for this version: root/<variant> links into root/ciel/…
    lib/           what `schemify import -o` wrote: one library directory per xschem symbol
                   directory (sky130_fd_pr, …) and its schemify.project (lib + set records)
    tech.map       technology-class map for this PDK (§6), shipped by the plugin's recipe
    STATE          one line: `ok` when the install finished; absent or anything else = incomplete
```

- `<family>` is a recipe name (§2): `sky130`, `gf180mcu`, `ihp-sg13g2`, later `asap7`.
- `<version>` is the source's version id (an open_pdks commit for ciel families, a git rev for
  git recipes): letters, digits, `.`, `_`, `-`, at most 64 bytes, never `latest`, never a path.
- Nothing outside `.schemify/pdks/` is written except `schemify.project` by `pdk.use` (§3.1).
  Two projects using the same version each hold a copy (accepted cost of the per-project
  choice; sky130 with `-l sky130_fd_pr` is about 220 MB, gf180mcu 700 MB, ihp-sg13g2 1.1 GB).

## 2. Recipes (data, in the plugin)

A recipe is one row of `RECIPES` in `plugins/pdk-manager/src/lib.rs`; adding a PDK is adding a
row (no code):

| Field | sky130 | gf180mcu | ihp-sg13g2 | asap7 (follow-up) |
|---|---|---|---|---|
| source | `ciel` family `sky130` | `ciel` family `gf180mcu` | `ciel` family `ihp-sg13` | `git` repo + rev |
| variant | `sky130A` | `gf180mcuD` | `ihp-sg13g2` | — |
| libraries (`-l`) | `sky130_fd_pr` | `gf180mcu_fd_pr` | (default) | — |
| xschem symbols | `libs.tech/xschem` | `libs.tech/xschem` | `libs.tech/xschem` | none: devices come only through `tech:` classes (§5) |
| models file | `libs.tech/combined/sky130.lib.spice` | `libs.tech/ngspice/sm141064.ngspice` | `libs.tech/ngspice/models/cornerMOSlv.lib` | the repo's BSIM-CMG model cards |
| corners (`tt ff ss sf fs`) | `tt ff ss sf fs` | `typical ff ss sf fs` | `mos_tt mos_ff mos_ss mos_sf mos_fs` | `TT FF SS` |
| tech.map | `maps/sky130.map` | `maps/gf180mcu.map` | `maps/ihp-sg13g2.map` | shipped |

**Q1 (resolved 2026-10-12):** checked against trees installed with ciel 3.0.1 (sky130 and
gf180mcu `1e35547…`, ihp-sg13 `22f4335…`): ciel's IHP family is `ihp-sg13` (variants
`ihp-sg13g2`, `ihp-sg13cmos5l`); every models path above exists; the section names are the
files' `.lib` sections (sky130 also has `ll hh hl lh`, gf180mcu `statistical`, IHP `*_mismatch`
and `*_stat`). Running `schemify import` on each tree and netlisting the §5 inverter with the
shipped maps gives each PDK's device line (`sky130_fd_pr__pfet_01v8 L=0.5 W=2 nf=2 … mult=1`,
`pfet_03v3 L=5e-7 W=2e-6 nf=2 … m=1`, `sg13_lv_pmos w=2e-6 l=5e-7 ng=2 m=1`).

## 3. Commands (plugin)

All commands are plugin commands (D27 table), reachable from the panel (§4), keys and scripts.

| Command | Effect |
|---|---|
| `pdk.list_remote <family>` | `ciel ls-remote --pdk-family <ciel family>` → the 40-digit version ids it prints, shown in the panel (at most 24) |
| `pdk.install <family> <version>` | 1. remove `STATE`; 2. add the version to `INSTALLS`; 3. `ciel enable --pdk-family <ciel family> --pdk-root .schemify/pdks/F/V/root [-l LIB]… V`; 4. `schemify import <root>/<variant>/libs.tech/xschem -o .schemify/pdks/F/V/lib --xschemrc <root>/<variant>/libs.tech/xschem/xschemrc`; 5. write `tech.map`; 6. write `STATE` = `ok`. Each step starts when the previous one completed; a failing step stops the chain, so `STATE` stays absent, and its output tail is shown. One install runs at a time. |
| `pdk.use <family> <version>` | Makes the version active for the project (§3.1). Refused unless `STATE` is `ok`. |
| `pdk.remove <family> <version>` | Refused while the version is active (a `lib` path in the manifest is under its directory) or referenced by `schemify.lock` (a `locked` row's resolved path is); otherwise removes `.schemify/pdks/F/V/` and its `INSTALLS` line (§3.2). |
| `pdk.prune` | Lists every install that is neither active nor locked, complete or not, and needs a second invocation (`pdk.prune confirm`, or `pdk.prune.confirm`) to remove them. The confirm removes only versions it listed that are still unreferenced; an active or locked install is never pruned, even when incomplete. |
| `pdk.panel` | Shows or hides the panel (View → PDKs). |

`ciel enable` (not `ciel fetch`): `fetch` leaves the tree in ciel's own store
(`root/ciel/<family>/versions/<V>/<variant>`), `enable` also links `root/<variant>` to it,
which is the path the import and `PDK_ROOT` use. The panel's row buttons run row commands
(`pdk.row.use.<i>`, `pdk.row.remove.<i>`, `pdk.remote.install.<k>`, `pdk.family.<family>`),
because a clicked button sends no arguments.

### 3.1 Using a version

`pdk.use F V` reads the import's `lib/schemify.project` and rewrites the project manifest
(`schemify.project`) in one write:

```
lib <name> path .schemify/pdks/<F>/<V>/lib/<path>     # one per `lib` the import wrote
lib tech map .schemify/pdks/<F>/<V>/tech.map           # §5: tech: cells resolve through it
set PDK_ROOT .schemify/pdks/<F>/<V>/root
set PDK <variant>
set PDK_MODELS ${PDK_ROOT}/<variant>/<models file>
set <NAME> <value>                                     # the import's PDK `set` entries
```

replacing every `lib` whose path is under `.schemify/pdks/`, `lib tech`, and the `set` entries
`pdk.use` owns (`PDK_ROOT`, `PDK`, `PDK_MODELS`, and any whose value names `PDK_ROOT` or
`.schemify/pdks/`); every other record and comment is kept, byte for byte and in order, and a
canonical manifest stays canonical (format §2). The import writes one library per symbol directory, so a PDK
is several `lib` lines (sky130: `sky130_fd_pr`, `sky130_stdcells`, …), named as the import
names them, which is also how the PDK's own cells reference each other.

The write of `schemify.project` makes the app run `project.reload` (§9.2, plugin-abi §5)
before the plugin hears it completed. Once locked, the lockfile (D10) pins the libraries' hashes; when a
hash changes, the D40 "default changes" report runs and is shown in the visual diff. Bench
corners that should follow the PDK use `lib ${PDK_MODELS} <section>` (§7).

Switching **versions** of one family is `pdk.use F V2`. Switching **technology** is `pdk.use F2 V`
plus §5: cells referenced as `tech:<class>` retarget; cells referenced directly as `<lib>:<cell>`
keep pointing at the old family and become unresolved (E001) once its `lib` lines go, which the
checks panel lists (the "Retarget" action is §8, draft).

### 3.2 Deletion

`write_project` grants `fs_remove(path)` (plugin-abi §5): it removes a project-relative file or
directory tree strictly inside `.schemify/`, and is refused for anything else, for `.schemify`
itself and for `schemify.project`/`schemify.lock`. `pdk.remove` and `pdk.prune` remove the whole
`.schemify/pdks/F/V/` directory (ciel's tree under `root/` included), then rewrite `INSTALLS`.

## 4. Panel (plugin UI)

A docked "PDKs" panel (View → PDKs, a pane of the work split like the cell panel) on the
default UI's cell-panel styles (`cell-pane`, `docs-bar`, `cell-page`, `cell-section`,
`cell-row`, `cell-key`), so it follows the theme and the Modern preset:

- **Installed:** one row per `<family>/<version>`: family and short version (7 characters; the
  full id as tooltip), state (`active`, `locked`, `incomplete`, `installed`), buttons Use /
  Remove, greyed when refused with the reason as tooltip (already active; incomplete; pinned by
  `schemify.lock`; an install is running).
- **Available:** family dropdown → `ls-remote` versions → Install (Reinstall when present).
- **Install:** the current step (`fetch (ciel)`, `import (schemify)`, `tech.map`, `state`, or
  `failed`) and the last 6 lines of the previous step's output.
- **Prune:** lists what it would remove, then Confirm.

Long steps (`ciel enable`, import) run through `spawn`, which never blocks the frame
(plugin-abi §11 item 12). Grants the plugin needs: `spawn:ciel`, `spawn:schemify`,
`read_project`, `write_project`:

```
plugin pdk-manager path plugins/pdk-manager/pdk_manager.wasm
  grant read_project
  grant write_project
  grant spawn:ciel
  grant spawn:schemify
```

**Draft:** size on disk per version and the total a prune frees. `ponytail:` plugins cannot
list directories or stat files; add a `du` spawn or an `fs_stat` import when a user needs it.

## 5. Technology classes (core)

A **technology class** is a technology-neutral device: `tech:nmos`, `tech:pmos` (MVP); later
`tech:res`, `tech:cap`, `tech:diode`, `tech:npn`, `tech:pnp`. A schematic that uses only `tech:`
devices (plus `core:` sources and passives) retargets between PDKs by changing the manifest's
`lib tech map <file>` line.

- `tech` is a reserved library name; `lib tech map <relpath>` names the `tech.map` that resolves
  it (format §4). Without that line, `tech:` cells are unresolved (E001, "no technology
  selected"); the E001 detail also names an unloadable map, a name that is not a class, a
  (class, flavor) the map lacks (`sky130 has no lvt pmos`) and a map row whose PDK cell is not
  in the project's libraries.
- The core **derives one cell per map row** (class, flavor) whenever a library is indexed:
  drawing and pin positions from the class's core symbol (`core:nmos4`, `core:pmos4`), so the
  drawing does not change when the technology does; pin names and order, params and impls from
  the PDK cell the row names. PDK pins match core pins by name ignoring case (`D` ↔ `d`); a pin
  without a counterpart on either side is a derivation error. The map's `param` entries become
  `computed` params of the derived cell (an override of them is `E030`), and the class params are
  added with their defaults. The netlisters see an ordinary leaf: no emitter knows about classes.
- Class params (MOS): `w` (default `1u`), `l` (`0.18u`), `m` (`1`) as the core symbol declares
  them, `nf` (`1`), `flavor` (`core|io|lvt|hvt`, default `core`); SI units. When the PDK cell has
  a param of the same name (IHP's `w l m`, gf180's `nf m`), the class param stands in for it
  (the class default, the PDK's other attributes); the map may only assign it to itself.
- `tech:<class>` resolves to the `core` flavor everywhere (drawing, the editor's live checks);
  elaboration resolves an instance whose `flavor` override is not `core` to that flavor's row.
- **Draft:** res `value` or `w`/`l` with `flavor` (`poly_hi|poly_lo|diff`); cap `value` or
  `w`/`l` with `flavor` (`mim|mos`).

## 6. `tech.map` (line records, format §4 "Technology map")

```
schemify 1 techmap
  pdk sky130
  vdd 1.8                                  # nominal supply of the core flavor, volts

class nmos core
  cell sky130_fd_pr:nfet_01v8              # the PDK cell it becomes
  param W w/1u                             # PDK param = expression over the class params
  param L l/1u
  param mult m
  limits l min 0.15u                       # optional, for E005 before netlisting (draft)
class nmos io
  cell sky130_fd_pr:nfet_g5v0d10v5
  vdd 3.3

class nmos core                            # in asap7's map: FinFET
  cell asap7:nmos_rvt
  param nfin max(1, ceil(w / 27n))         # fins from the requested width
```

- A `param` value is written as an instance override of that PDK param would be: a D20
  expression for number params, no braces. It is evaluated in the derived cell's scope, so it
  reads the instance's class params (their overrides, else the defaults). Files store only the
  class params (D40: overrides only).
- A class/flavor a PDK lacks is an error at check time ("sky130 has no lvt pmos"), not a silent
  substitution.
- The shipped maps (`plugins/pdk-manager/maps/`): sky130 `W = w/1u`, `L = l/1u`, `mult = m`
  (its symbols take microns); gf180mcu `W = w`, `L = l` (metres); ihp-sg13g2 `ng = nf` (`w`,
  `l`, `m` are the class params themselves).
- **Draft:** `limits` is parsed and kept but not enforced; `vdd` is parsed (`TechMap::vdd`) but
  not yet exported as the read-only var `vdd_nom` in benches (§9.4).

## 7. Corners and models (draft)

A bench corner that should follow the active PDK writes `lib ${PDK_MODELS} <section>`; the
recipe's `corners` row lists the PDK's section names in a fixed order (`tt ff ss sf fs`
positions), and the corner names `tt|ff|ss|sf|fs` resolve through it, so `corner tt` means the
right section in every technology. **Q2:** whether corner-name mapping lives in `tech.map` or in
the recipe (it is in the recipe today, unused by the core).

## 8. Retargeting existing designs (draft)

`pdk.retarget` (core command) converts instances of a family's PDK cells (`sky130_fd_pr:nfet_01v8`)
into `tech:` instances where the active `tech.map` has a class whose `cell` matches and whose param
expressions are invertible for the instance's values (linear scalings like `W = w/1u`); the rest
are listed, not changed. One transaction (one undo), shown in the visual diff before commit.

## 9. Core changes

1. **Done.** `lib tech map <file>` manifest records, the `techmap` file kind, derived `tech`
   cells and `tech:` resolution in `doc` (`Project::resolve_tech`, `explain_tech`) and `elab`
   (flavor), param expressions; netlisters see the resolved PDK cell. Limits: draft.
2. **Done.** `project.reload` core command: re-reads `schemify.project` and the lockfile
   without restarting (`Project::reload_manifest` keeps open documents and their history,
   refused while an open document's library would disappear), re-indexes every library and
   rebuilds symbols, scenes and cell states. Also run after a plugin writes the manifest.
3. **Done.** plugin ABI: `fs_remove` under `write_project`, scoped to `.schemify/` (§3.2).
4. **Draft.** `vdd_nom` var export into benches (§6).

## 10. MVP scope and tests

- **MVP (built):** the plugin with sky130, gf180mcu and ihp-sg13g2 (ciel), §3 commands, §4 panel,
  per-project store, `pdk.use` writing the manifest, `pdk.prune`; core §9.1 for `tech:nmos` and
  `tech:pmos` with the three maps, §9.2, §9.3. ASAP7, other classes, limits, `vdd_nom`, §7 corner
  mapping and §8 retargeting follow.
- **Oracles:**
  - `plugins/pdk-manager/tests/guest.rs`: the built guest through the real plugin host with a
    fake `ciel` and `schemify` on `PATH` (shell scripts writing a tiny tree and logging argv):
    exact argv of every step, store layout, `STATE`, `tech.map`, `INSTALLS`, the manifest after
    `pdk.use` (canonical text), a failed fetch or import (and a failed re-install of a complete
    version) leaves no `STATE ok`, `pdk.use` refuses an incomplete install, `pdk.remove` refuses
    the active and the locked version, `pdk.prune` removes exactly the unreferenced installs;
    bad arguments spawn nothing. `real_ciel_installs_sky130` (ignored, nightly: real `ciel` and
    `schemify` on `PATH`) installs and uses sky130.
  - `plugins/pdk-manager/src/lib.rs`: argv, version and path checks, the manifest rewrite
    (reproduced byte for byte by the format writer, idempotent), references, prune plan.
  - `crates/cli/tests/tech.rs`: the same `tech:` inverter netlisted under the shipped sky130 and
    gf180mcu maps produces each PDK's device lines with the mapped params (golden netlists);
    negative: no `lib tech`, unknown class, unknown flavor.
  - `crates/doc/tests/tech.rs`: derived cells (pins, params, impls, drawing), every E001 reason,
    `reload_manifest` keeps open documents and refuses to orphan one.
  - `crates/app/tests/pdk.rs`: `project.reload` resolves a new `lib` and a new `lib tech map`
    without a restart, switches technology, refuses to orphan an open document.
  - `crates/plugin` (`fs_remove_is_scoped_to_the_tool_directory`, the capability proptest) and
    `crates/app` (`fs_remove_removes_trees_and_files`): `fs_remove` refusals and execution.
