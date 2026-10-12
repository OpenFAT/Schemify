# PDK management and technology classes

Status: **normative for the MVP** (§10: §1–§6 as written here, §9.1–§9.3), 2026-10-12;
academic kits (FreePDK45, ASAP7: source kinds §2.1, device generation §2.2, corners as files
§7, §9.5) normative 2026-10-12. User
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
    root/          the kit: ciel's PDK root (root/<variant> links into root/ciel/…), the git
                   checkout, or the unpacked archive (§2.1); absent for a `local` kit
    ROOT           a `local` kit only: one line, the kit's path (§2.1)
    lib/           what `schemify import -o` wrote: one library directory per xschem symbol
                   directory (sky130_fd_pr, …) and its schemify.project (lib + set records);
                   or the generated library lib/<family>/ and its schemify.project (§2.2)
    models/        corner library corners.lib and derived model cards (§7; generated kits)
    va/            the Verilog-A checkout of an OSDI recipe (§2.2; ASAP7's BSIM-CMG)
    tech.map       technology-class map for this PDK (§6), shipped by the plugin's recipe
    STATE          one line: `ok` when the install finished; absent or anything else = incomplete
```

- `<family>` is a recipe name (§2): `sky130`, `gf180mcu`, `ihp-sg13g2`, `freepdk45`, `asap7`.
- `<version>` is the source's version id (an open_pdks commit for ciel families, the pinned rev
  for git recipes, the first 12 hex digits of the sha256 for url recipes, the user's name for a
  `local` kit): letters, digits, `.`, `_`, `-`, at most 64 bytes, never `latest`, never a path.
- Nothing outside `.schemify/pdks/` is written except `schemify.project` by `pdk.use` (§3.1);
  a `local` kit is read where it is and never written or removed.
  Two projects using the same version each hold a copy (accepted cost of the per-project
  choice; sky130 with `-l sky130_fd_pr` is about 220 MB, gf180mcu 700 MB, ihp-sg13g2 1.1 GB).

## 2. Recipes (data, in the plugin)

A recipe is one row of `RECIPES` in `plugins/pdk-manager/src/lib.rs`; adding a PDK is adding a
row (no code):

| Field | sky130 | gf180mcu | ihp-sg13g2 | freepdk45 | asap7 |
|---|---|---|---|---|---|
| source (§2.1) | `ciel` family `sky130` | `ciel` family `gf180mcu` | `ciel` family `ihp-sg13` | `url` of a pinned GitHub archive of the v1.4 mirror `baichen318/FreePDK45` @ `a75cb11`, sha256 `5b93a0b1…532e` | `git` `The-OpenROAD-Project/asap7_pdk_r1p7` @ `58d72c9` |
| variant | `sky130A` | `gf180mcuD` | `ihp-sg13g2` | — | — |
| libraries (`-l`) | `sky130_fd_pr` | `gf180mcu_fd_pr` | (default) | — | — |
| devices (§2.2) | import `libs.tech/xschem` | import `libs.tech/xschem` | import `libs.tech/xschem` | generated: `{n,p}mos_{vtl,vtg,vth,thkox}` (BSIM4 level 54, `w l nf m`) | generated: `{n,p}mos_{rvt,lvt,slvt,sram}` (BSIM-CMG level 72 via OSDI, `nfin nf m`), Verilog-A from `dwarning/VA-Models` @ `6099b4e` (BSIM-CMG 111.2.1, ECL-2.0) |
| models (§7) | sections of `libs.tech/combined/sky130.lib.spice` | sections of `libs.tech/ngspice/sm141064.ngspice` | sections of `libs.tech/ngspice/models/cornerMOSlv.lib` | files `FreePDK45/ncsu_basekit/models/hspice/tran_models/models_{nom,ff,ss}/*.inc` | files `models/hspice/7nm_{TT,FF,SS}_160803.pm`, derived for OSDI |
| corners | `tt ff ss sf fs` | `typical ff ss sf fs` | `mos_tt mos_ff mos_ss mos_sf mos_fs` | `tt ff ss` | `tt ff ss` |
| tech.map | `maps/sky130.map` | `maps/gf180mcu.map` | `maps/ihp-sg13g2.map` | `maps/freepdk45.map` | `maps/asap7.map` |

Sources, licences and the checks behind the academic rows: [research/academic-pdks.md](../research/academic-pdks.md).
NCSU's own FreePDK45 download needs a registration; a registered copy installs as a `local`
kit (§2.1). We commit only recipe data (URLs, revs, paths, model names) and our maps, never kit
files.

**Q1 (resolved 2026-10-12):** checked against trees installed with ciel 3.0.1 (sky130 and
gf180mcu `1e35547…`, ihp-sg13 `22f4335…`): ciel's IHP family is `ihp-sg13` (variants
`ihp-sg13g2`, `ihp-sg13cmos5l`); every models path above exists; the section names are the
files' `.lib` sections (sky130 also has `ll hh hl lh`, gf180mcu `statistical`, IHP `*_mismatch`
and `*_stat`). Running `schemify import` on each tree and netlisting the §5 inverter with the
shipped maps gives each PDK's device line (`sky130_fd_pr__pfet_01v8 L=0.5 W=2 nf=2 … mult=1`,
`pfet_03v3 L=5e-7 W=2e-6 nf=2 … m=1`, `sg13_lv_pmos w=2e-6 l=5e-7 ng=2 m=1`).

### 2.1 Source kinds

| Kind | Fetch (spec §3 `pdk.install`) | Version |
|---|---|---|
| `ciel <family>` | `ciel enable … --pdk-root <dir>/root` | any ciel version id |
| `git <url> <rev>` | remove `root/`, `git clone --quiet --no-checkout <url> <dir>/root`, `git -C <dir>/root checkout --quiet <rev>` | the pinned rev only |
| `url <url> <sha256>` | remove `root/`, `curl -fsSL --create-dirs -o <dir>/root/source.tar.gz <url>`, check the sha256 (in the plugin, ≤ 64 MiB, 32 KiB per event so the default fuel suffices), `tar -xzf … -C <dir>/root --strip-components=1`, remove the archive | the pinned sha256's first 12 hex digits only |
| `local <path>` | none: `ROOT` records the path; the kit is referenced in place, nothing copied | the user's name |

- `local` is chosen per install, for any family: `pdk.install <family> <name> <path>`. It is the
  hook for kits that cannot be downloaded (a registered FreePDK45, a proprietary kit later). The
  path is refused when empty, longer than 1024 bytes, holding a control character, or with a
  `.schemify` segment (so `pdk.remove`/`pdk.prune`, which delete only `.schemify/pdks/F/V/`,
  can never reach the kit); for a recipe whose cards are read at install (ASAP7) it must be
  project-relative (`fs_read` takes only those). `pdk.use` writes `set PDK_ROOT <path>`.
- Why `curl`/`tar` and not the `network` capability: the host does not execute `fetch` yet
  (plugin-abi §5), `tar` unpacks without a decompressor in the guest, and `curl`/`tar` are on
  every desktop OS (Windows 10+ ships both). The sha256 is computed in the guest from the bytes
  it reads back, so a substituted or truncated download never unpacks. `ponytail:` an archive
  over 64 MiB (the `fs_read` reply limit) cannot be checked; such a kit uses `git` or `local`.
- GitHub archives of a commit are stable in practice, not by contract; a changed archive fails
  the sha256 check (no `STATE ok`), and the recipe row is updated.

### 2.2 Generated devices

A kit without xschem symbols gets one `.shprim` per recipe device in
`.schemify/pdks/F/V/lib/<family>/<cell>.shprim`, plus `lib/schemify.project`
(`lib <family> path <family>`), so `pdk.use` treats it like an import. Each cell:

- the pins and drawing of `core:nmos4` / `core:pmos4` exactly (a `tech:` instance and a direct
  `freepdk45:nmos_vtl` look and connect alike), header `prefix M`, the size label replaced by
  `nfin={nfin}` for FinFETs;
- `param model <kit model>` (string) and the size params;
- BSIM4 kits (freepdk45): `w` `l` (defaults: the kit minimum, 90n/50n), `nf`, `m`, and
  `impl spice spice` with `format spice "M{name} {d} {g} {s} {b} {model} w={w} l={l} nf={nf} m={m}"`;
  `w l nf m` are the class params themselves, so the map assigns nothing;
- OSDI kits (asap7): `nfin`, `nf`, `m`, and `impl osdi veriloga` with
  `format spice "N{name} {d} {g} {s} {b} {model} nfin={nfin} nf={nf} m={m}"` and
  `file ../../va/code/bsimcmg/vacode/bsimcmg.va` (the checkout, relative to the cell): the runner
  compiles it with OpenVAF and the netlist loads it with `pre_osdi bsimcmg.osdi` (run spec §5,
  netlist spec §7). ngspice has no built-in BSIM-CMG (level 72), so the install derives each
  corner card: `.model <m> nmos|pmos level = 72` becomes `.model <m> bsimcmg_va type=1|-1`
  (the compiled module, `TYPE` 1 = n, -1 = p), every other line kept; a MOS card of another
  level fails the install. The gate length is the cards' fixed 21 nm (no `l`).

## 3. Commands (plugin)

All commands are plugin commands (D27 table), reachable from the panel (§4), keys and scripts.

| Command | Effect |
|---|---|
| `pdk.list_remote <family>` | ciel: `ciel ls-remote --pdk-family <ciel family>` → the 40-digit version ids it prints, shown in the panel (at most 24); git/url: the pinned version, nothing spawned |
| `pdk.install <family> <version> [path]` | The recipe's steps (`install::install_steps`), in order: 1. remove `STATE`; 2. add the version to `INSTALLS`; 3. fetch (§2.1: `ciel enable --pdk-family <ciel family> --pdk-root .schemify/pdks/F/V/root [-l LIB]… V`, the git checkout, or download + sha256 + unpack), or for a `path` write `ROOT`; 4. devices: `schemify import <root>/<variant>/libs.tech/xschem -o .schemify/pdks/F/V/lib --xschemrc <root>/<variant>/libs.tech/xschem/xschemrc`, or (§2.2) the Verilog-A checkout, the derived cards, `models/corners.lib`, the generated cells and `lib/schemify.project`; 5. write `tech.map`; 6. write `STATE` = `ok`. Each step starts when the previous one completed; a failing step stops the chain, so `STATE` stays absent, and its output tail is shown. One install runs at a time. A git/url family refuses any version but its pinned one unless a `path` is given. |
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

`pdk.use F V` reads `ROOT` (a `local` kit) and the library manifest `lib/schemify.project`,
and rewrites the project manifest (`schemify.project`) in one write:

```
lib <name> path .schemify/pdks/<F>/<V>/lib/<path>     # one per `lib` the import or §2.2 wrote
lib tech map .schemify/pdks/<F>/<V>/tech.map           # §5: tech: cells resolve through it
set PDK_ROOT .schemify/pdks/<F>/<V>/root               # or the `local` kit's path
set PDK <variant>                                      # ciel families only
set PDK_MODELS ${PDK_ROOT}/<variant>/<models file>     # generated kits: .schemify/pdks/<F>/<V>/models/corners.lib
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

Long steps (`ciel enable`, import, `git`, `curl`, `tar`) run through `spawn`, which never
blocks the frame (plugin-abi §11 item 12). Grants the plugin needs: `read_project`,
`write_project`, and the `spawn:` of the families used: `ciel` and `schemify` (ciel families),
`git` (asap7, and asap7's Verilog-A for a `local` asap7), `curl` and `tar` (freepdk45). A missing
grant fails the step that needs it, naming the grants:

```
plugin pdk-manager path plugins/pdk-manager/pdk_manager.wasm
  grant read_project
  grant write_project
  grant spawn:ciel
  grant spawn:schemify
  grant spawn:git
  grant spawn:curl
  grant spawn:tar
```

A `local` kit is installed by command only (`pdk.install F <name> <path>`): the panel has no
path field. The Available section of a git/url family shows its one pinned version.

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
  param nfin max(1,ceil(w/nf/70.5n))       # fins per finger from the requested width
```

- A `param` value is written as an instance override of that PDK param would be: a D20
  expression for number params, no braces. It is evaluated in the derived cell's scope, so it
  reads the instance's class params (their overrides, else the defaults). Files store only the
  class params (D40: overrides only).
- A class/flavor a PDK lacks is an error at check time ("sky130 has no lvt pmos"), not a silent
  substitution.
- The shipped maps (`plugins/pdk-manager/maps/`): sky130 `W = w/1u`, `L = l/1u`, `mult = m`
  (its symbols take microns); gf180mcu `W = w`, `L = l` (metres); ihp-sg13g2 `ng = nf` (`w`,
  `l`, `m` are the class params themselves); freepdk45 nothing (`w l nf m` are its cells'
  params): core = VTL (the kit's default device), hvt = VTH, io = THKOX, no lvt (VTG is placed
  directly); asap7 `nfin = max(1, ceil(w / nf / 70.5n))` — one fin is 2·hfin + tfin =
  2·32 + 6.5 nm of channel width (the cards' `hfin`, `tfin`; first-order, research §2.5) —
  core = rvt, lvt = lvt, no hvt or io (slvt, sram placed directly); the class `l` does not
  reach an asap7 device (fixed 21 nm, `limits` draft). `ceil` of a quotient that should be an
  integer may round up by one fin (floating point); size with whole multiples of 70.5n plus a
  margin, or set `nfin` on a direct `asap7:` cell.
- **Draft:** `limits` is parsed and kept but not enforced; `vdd` is parsed (`TechMap::vdd`) but
  not yet exported as the read-only var `vdd_nom` in benches (§9.4).

## 7. Corners and models

A bench corner that should follow the active PDK writes `lib ${PDK_MODELS} <section>`.

- **Sectioned kits** (ciel families): `PDK_MODELS` is the kit's library file; the recipe's
  `corners` row lists its section names in `tt ff ss sf fs` order (draft: the core does not
  map `corner tt` onto `typical` yet, so a bench switching from sky130 to gf180mcu edits its
  `lib` line).
- **Corners as files** (generated kits, normative): the recipe's corner table maps each
  corner name to its files; the install writes `.schemify/pdks/F/V/models/corners.lib` with one
  `.lib <corner>` … `.endl <corner>` section per row, each `.include "<path>"` of that corner's
  files, and `pdk.use` sets `PDK_MODELS` to it. The paths are relative to `models/`
  (`../root/<file>`, which ngspice resolves against the including file's directory), absolute
  for an absolute `local` kit, `../../../../../<path>/<file>` for a project-relative one, or the
  derived card beside the library (asap7). The section names are the recipe's corner names,
  `tt ff ss` for both academic kits, so `lib ${PDK_MODELS} tt` is the same line under freepdk45
  and asap7. This answers **Q2** for generated kits: corner names live in the recipe.
- `PDK_MODELS` expands to a project-relative path; the runner joins a relative corner path
  onto the bench's directory (run spec §5), so a bench using it lives at the project root.
  **Q3:** resolve `${PDK_ROOT}`-derived corner paths against the project root instead.

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
5. **Done.** A derived `tech` cell's impls are its PDK cell's, so a Verilog-A `file` they name
   resolves against the PDK cell's library and directory (`Project::tech_source`, used by
   `run::sources`); the CLI's `schemify run` passes every library directory to the runner, as
   the editor does, so library cells compile their sources.

## 10. MVP scope and tests

- **MVP (built):** the plugin with sky130, gf180mcu and ihp-sg13g2 (ciel), §3 commands, §4 panel,
  per-project store, `pdk.use` writing the manifest, `pdk.prune`; core §9.1 for `tech:nmos` and
  `tech:pmos` with the three maps, §9.2, §9.3. **Academic kits (built):** freepdk45 (`url`) and
  asap7 (`git`), `local` kits, §2.2 device generation, §7 corners as files, §9.5. Other classes,
  limits, `vdd_nom`, corner-name mapping for sectioned kits and §8 retargeting follow.
- **Oracles:**
  - `plugins/pdk-manager/tests/guest.rs`: the built guest through the real plugin host with a
    fake `ciel` and `schemify` on `PATH` (shell scripts writing a tiny tree and logging argv):
    exact argv of every step, store layout, `STATE`, `tech.map`, `INSTALLS`, the manifest after
    `pdk.use` (canonical text), a failed fetch or import (and a failed re-install of a complete
    version) leaves no `STATE ok`, `pdk.use` refuses an incomplete install, `pdk.remove` refuses
    the active and the locked version, `pdk.prune` removes exactly the unreferenced installs;
    bad arguments spawn nothing. `git_url_and_local_kits`, with fake `git`, `curl`, `tar`: exact
    argv, the generated store (cells equal to the CLI fixtures, derived cards, corner library,
    `tech.map`), the manifest after `pdk.use`, a failing checkout and a level-54 card leave no
    `STATE ok`, a sha256 mismatch never unpacks, refused versions and paths spawn nothing, a
    `local` kit is never copied and `pdk.remove`/`pdk.prune` of it leave the kit in place.
    `real_ciel_installs_sky130` (ignored, nightly: real `ciel` and `schemify` on `PATH`) installs
    and uses sky130; `real_kits_simulate_an_inverter` (ignored, nightly: network, `git`, `curl`,
    `tar`, `schemify`, ngspice, `openvaf-r` or `$OPENVAF`) installs freepdk45 and asap7, uses
    each and runs the `tech:` inverter's DC sweep with `schemify run`: output above 0.9 vdd at
    vin = 0, below 0.1 vdd at vin ≈ vdd, `vout = vin` between 0.3 and 0.7 vdd (2026-10-12:
    freepdk45 1.0997 / 7.5e-5 / 0.543 V of 1.1 V; asap7 0.7000 / 2.2e-7 / 0.365 V of 0.7 V).
  - `plugins/pdk-manager/src/lib.rs`: argv, version and path checks, the manifest rewrite
    (reproduced byte for byte by the format writer, idempotent), references, prune plan,
    recipe coherence, `local` path refusals. `src/install.rs`: the step list of every source
    kind, the corner library, the generated cells (canonical, the core pins and drawing, the
    CLI fixtures), the OSDI card rewrite and its refusals, sha256 against the FIPS vectors.
  - `crates/cli/tests/tech.rs`: the same `tech:` inverter netlisted under the shipped sky130,
    gf180mcu, freepdk45 and asap7 maps produces each PDK's device lines with the mapped params
    (golden netlists; asap7 `nfin` from `w`, and its `pre_osdi`);
    negative: no `lib tech`, unknown class, unknown flavor.
  - `crates/run/tests/runner.rs` `tech_cells_compile_their_pdk_cells_sources`: §9.5.
  - `crates/doc/tests/tech.rs`: derived cells (pins, params, impls, drawing, `tech_source`), every E001 reason,
    `reload_manifest` keeps open documents and refuses to orphan one.
  - `crates/app/tests/pdk.rs`: `project.reload` resolves a new `lib` and a new `lib tech map`
    without a restart, switches technology, refuses to orphan an open document.
  - `crates/plugin` (`fs_remove_is_scoped_to_the_tool_directory`, the capability proptest) and
    `crates/app` (`fs_remove_removes_trees_and_files`): `fs_remove` refusals and execution.
