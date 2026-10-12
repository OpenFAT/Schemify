# Academic PDKs: FreePDK45 and ASAP7, for a PDK-manager recipe

> **Clean-room rule (D1):** no xschem source was opened. Everything below comes from the PDKs' own READMEs, model cards and SKILL data files, the vendor/ngspice/BSIM sites, and running ngspice locally.

Status: research, 2026-10-12. Scope: where to fetch each kit, under what licence, what the directory tree and model cards look like, and whether the nixpkgs ngspice can simulate them. A claim with no source is marked **unverified**. "Ran locally" means the probe was run with ngspice-44.2 (`~/.nix-profile/bin/ngspice`), the version installed on this machine.

## Sources (pinned)

| Short | Repo @ commit | Notes |
|---|---|---|
| FP45 | [baichen318/FreePDK45 @ a75cb11](https://github.com/baichen318/FreePDK45/tree/a75cb116a3efd1d3ae05b695285452e6ffbc7ca5) | Unofficial mirror of the full v1.4 kit. `git ls-remote` of `main` was not run, so the hash comes from the GitHub API (`/commits/main`). |
| A7 | [The-OpenROAD-Project/asap7_pdk_r1p7 @ 58d72c9](https://github.com/The-OpenROAD-Project/asap7_pdk_r1p7/tree/58d72c9d291e186a77468586ab0c43d8a21eda6a) | `git ls-remote … HEAD` returned `58d72c9d291e186a77468586ab0c43d8a21eda6a`. Default branch `main`, 3 commits, last push 2024-07-18 ([API](https://api.github.com/repos/The-OpenROAD-Project/asap7_pdk_r1p7)). |
| A7-meta | [The-OpenROAD-Project/asap7 @ d24f8b8](https://github.com/The-OpenROAD-Project/asap7/tree/d24f8b857ff74cf5b21ab18a7e1b11a3954c449b) | `ls-remote` HEAD = `d24f8b857ff74cf5b21ab18a7e1b11a3954c449b`. This is a meta repo: `asap7_pdk_r1p7` is a git **submodule** pinned to `58d72c9…`, the same commit as A7 ([contents API](https://api.github.com/repos/The-OpenROAD-Project/asap7/contents/asap7_pdk_r1p7)). |
| — | `The-OpenROAD-Project/asap7PDK` | **Does not exist**: `git ls-remote` returns "Repository not found". |
| VA | [dwarning/VA-Models @ 6099b4e](https://github.com/dwarning/VA-Models/tree/6099b4e12e6f66ff530f59b8d01fb909094be85a) | BSIM-CMG Verilog-A source for ngspice OSDI. |

---

## 1. FreePDK45 (NCSU)

### 1.1 Distribution
- **Official page:** https://eda.ncsu.edu/freepdk/freepdk45/. The current release is "Version 1.4 (released April 7, 2011)", Subversion revision 173. The page has **no direct download link**. Its "Download" link goes to https://eda.ncsu.edu/downloads/, which sends you to a registration portal at https://labs.ece.ncsu.edu/eda/downloads/register. That page is a JS form (`<registration-form>`) behind Cloudflare, checked with curl 2026-10-12. **Registration is required for the official download.**
- The guessed URL https://eda.ncsu.edu/downloads/FreePDK45.tar.gz returns **HTTP 404** (curl -I, 2026-10-12). No stable official tarball URL exists.
- NCSU's GitHub org only hosts FreePDK3 ([api](https://api.github.com/orgs/ncsu-eda/repos)), not FreePDK45.
- **Mirrors (unofficial):**
  - [baichen318/FreePDK45](https://github.com/baichen318/FreePDK45) holds the complete v1.4 tree under `FreePDK45/` (`ncsu_basekit/`, `osu_soc/`, licence files) plus `ncsu-FreePDK45-1.4.tar.gz.sig`. The repo is about 8.6 MB (`"size": 8628` KB in the [API](https://api.github.com/repos/baichen318/FreePDK45)) and has 1619 tree entries. Branches: `main`, `chipyard`, `gcd`, `mips`. A stable tarball URL: `https://codeload.github.com/baichen318/FreePDK45/tar.gz/a75cb116a3efd1d3ae05b695285452e6ffbc7ca5` (HTTP 200; codeload sends no Content-Length). **Unverified:** whether its contents are byte-identical to the NCSU tarball. The `.sig` file could be checked against them, but NCSU's signing key was not found.
  - [mflowgen/freepdk-45nm](https://github.com/mflowgen/freepdk-45nm) (HEAD `d3be42a72ca6eabff54a4f3be50c16613f69f1b6`) is **not** the kit. It is an mflowgen ADK (Nangate stdcells, LEF/LIB, tech files) with **no SPICE models** ([repo](https://github.com/mflowgen/freepdk-45nm)), so it is useless for analog work.

### 1.2 Licence
- Kit `README.txt`: "SVRF Technology in this kit is licensed under the agreement found in the file SVRF_EULA_06Feb09.txt … All other files are licensed under the Apache License, Version 2.0" ([FP45 README](https://github.com/baichen318/FreePDK45/blob/a75cb116a3efd1d3ae05b695285452e6ffbc7ca5/FreePDK45/README.txt); the official page says the same thing, and notes the licence changed from GPL in v1.2: https://eda.ncsu.edu/freepdk/freepdk45/).
- The SPICE models and CDF are Apache-2.0, so downloading, local use and even redistribution (with NOTICE/licence) are allowed. The Calibre SVRF decks are under a separate Mentor EULA, which this note does not need. Shipping only our own recipe data (URLs, paths, device names) carries no obligation under Apache-2.0: we copy no kit files, so nothing is redistributed. This is my reading of the licence, not legal advice.

### 1.3 Layout (paths relative to `FreePDK45/`, from the FP45 tree API)
- `ncsu_basekit/cdssetup/` holds `cds.lib`, `lib.defs`, `cdsinit`, `cdsenv`, `display.drf`, `setup.csh` and `runset.calibre.{drc,lvs,pex,lfd}`.
- `ncsu_basekit/lib/NCSU_Devices_FreePDK45/` (OA lib) has the cells `NMOS_VTL NMOS_VTG NMOS_VTH NMOS_THKOX PMOS_*`. `ncsu_basekit/lib/NCSU_TechLib_FreePDK45/` is the tech lib.
- The techfile is `ncsu_basekit/techfile/FreePDK45.tf`. The CDF is `ncsu_basekit/skill/cdf/{N,P}MOS_{VTL,VTG,VTH,THKOX}.il`. Global sizes are in `ncsu_basekit/skill/globalData.il`.
- Models live in `ncsu_basekit/models/hspice/`:
  - Corner wrappers: `hspice_nom.include`, `hspice_ff.include`, `hspice_ss.include`. Each one `.inc`s all 8 flavour files through the path `'$PDK_DIR/ncsu_basekit/models/hspice/tran_models/models_nom/NMOS_VTG.inc` and so on ([hspice_nom.include](https://github.com/baichen318/FreePDK45/blob/a75cb116a3efd1d3ae05b695285452e6ffbc7ca5/FreePDK45/ncsu_basekit/models/hspice/hspice_nom.include)). A recipe must substitute `$PDK_DIR` or include the per-flavour files directly. Note the unmatched leading `'` in that line.
  - Per-corner, per-flavour cards: `tran_models/models_{nom,ff,ss}/{NMOS,PMOS}_{VTL,VTG,VTH,THKOX}.inc`. That is 3 corners × 8 = 24 files, **one file per corner per device, with no `.lib` sections**.
  - `ptm/` holds the PTM source cards (`NMOS_last`, `PMOS_last`) and `genptm.pl`.

### 1.4 Model format ([models_nom/NMOS_VTL.inc](https://github.com/baichen318/FreePDK45/blob/a75cb116a3efd1d3ae05b695285452e6ffbc7ca5/FreePDK45/ncsu_basekit/models/hspice/tran_models/models_nom/NMOS_VTL.inc))
- The header comment is `* Customized PTM 45 NMOS nom`, followed by `.model  NMOS_VTL  nmos  level = 54`, i.e. **BSIM4, `version = 4.0`**.
- Flags set in the card: `binunit=1 paramchk=1 mobmod=0 capmod=2 igcmod=1 igbmod=1 geomod=1 diomod=1 rdsmod=0 rbodymod=1 rgatemod=1 permod=1 tnom=27`. Also `toxe=1.14n`, `xl=-20n`, `lint=3.75n`, `wint=5n`.
- **Model names** (case as written): `NMOS_VTL PMOS_VTL NMOS_VTG PMOS_VTG NMOS_VTH PMOS_VTH NMOS_THKOX PMOS_THKOX`. ngspice lower-cases them. Nominal `vth0` values: VTL 0.322, VTG 0.4106, VTH 0.6078, THKOX 1.507 (the same files).
- Flavours are "VTL, VTG, VTH, THKOX (low, general, high threshold voltage, and thick-oxide devices)" (https://eda.ncsu.edu/freepdk/freepdk45/).
- **ngspice (ran locally):** the nom VTL n+p cards load with `M… NMOS_VTL W=90n L=50n` and solve `.op` with **no warnings** and no compat mode (`Id = 120.6 µA` at Vgs = Vds = 1.1 V). ngspice reports the instances as "BSIM4v5 models", so `version=4.0` is routed to its BSIM4v5 code. `rbodymod=1` and `rgatemod=1` add internal nodes (`m1#body`, `m1#gate`, …) that show up in the op dump. The VTG/VTH/THKOX and ff/ss cards were **not run (unverified)**.

### 1.5 Device params
- `globalData.il`: `?MinW "90n"`, `?MinL "50n"`, `?GridRes 2.5e-9`. The CDF `w`/`l` defaults are `ncsuGlobalData->MinW` / `->MinL` ([globalData.il](https://github.com/baichen318/FreePDK45/blob/a75cb116a3efd1d3ae05b695285452e6ffbc7ca5/FreePDK45/ncsu_basekit/skill/globalData.il), [cdf/NMOS_VTL.il](https://github.com/baichen318/FreePDK45/blob/a75cb116a3efd1d3ae05b695285452e6ffbc7ca5/FreePDK45/ncsu_basekit/skill/cdf/NMOS_VTL.il)). **So min L = 50 nm and min W = 90 nm**, and with `xl = -20n` the modelled L at minimum is 30 nm. The kit's own standard-cell library is characterised at 1.1 V (`nom_voltage : 1.1;` in `FreePDK45/osu_soc/lib/files/gscl45nm.lib`), the `vdd` of our `tech.map`.

---

## 2. ASAP7 (ASU + Arm)

### 2.1 Distribution
- Home page: https://asap.asu.edu/. It says "The ASAP PDK is now available on GitHub for free", lists v1.7 as the latest release, and keeps **registration only for the Calibre decks**. The A7-meta README repeats that the "Calibre Decks are not a part of this repository" ([README](https://github.com/The-OpenROAD-Project/asap7)).
- **No registration for the PDK.** The model cards are directly fetchable at a pinned commit, e.g. `https://raw.githubusercontent.com/The-OpenROAD-Project/asap7_pdk_r1p7/58d72c9d291e186a77468586ab0c43d8a21eda6a/models/hspice/7nm_TT_160803.pm` (41 629 bytes, fetched 2026-10-12). The repo is about 3.4 MB (`"size": 3410` KB in the [API](https://api.github.com/repos/The-OpenROAD-Project/asap7_pdk_r1p7)). Tarball: `https://codeload.github.com/The-OpenROAD-Project/asap7_pdk_r1p7/tar.gz/58d72c9d291e186a77468586ab0c43d8a21eda6a`.

### 2.2 Licence
- **BSD-3-Clause**, "Copyright 2020 Lawrence T. Clark, Vinay Vashishtha, or Arizona State University". The text is in [LICENSE](https://github.com/The-OpenROAD-Project/asap7_pdk_r1p7/blob/main/LICENSE) and also at the head of every `.pm` file. The site adds that the kit is "an academic and research aid only and the resulting designs are not manufacturable" (https://asap.asu.edu/). Download, local use and redistribution with the notice are all allowed. Our recipe data (URLs, paths, model names) copies nothing.

### 2.3 Layout (from the [A7 tree API](https://api.github.com/repos/The-OpenROAD-Project/asap7_pdk_r1p7/git/trees/main?recursive=1) and `README_ASAP7PDK_INSTALL_201210a.txt`)
- `cdslib/asap7_TechLib_10/` (`asap7_TechLib` in cds.lib) is the tech library. It also holds the transistor cells, and "the view 'symbol' must be used for the purpose of device instantiation". `cdslib/setup/` has `cds.lib`, `cdsenv`, `cdsinit`, `display.drf`, `setup_asap7.csh` and `set_pdk_path.csh`. In that last file the placeholder `PDKDirectory` must be replaced to set `$PDK_DIR` ([install README](https://github.com/The-OpenROAD-Project/asap7_pdk_r1p7/blob/main/README_ASAP7PDK_INSTALL_201210a.txt)).
- `calibre/` has `ruledirs/{drc,lvs,rcx}` and `rundirs/…`. `docs/`, `asap7ssc7p5t_05/` (sample cells) and `README.md` are also present. The DRM is `asap7_drm_201207a.pdf` (A7-meta README).
- Model cards are in `models/hspice/`: `7nm_TT.pm`, `7nm_FF.pm`, `7nm_SS.pm`, and the dated `7nm_{TT,FF,SS}_160803.pm`. The plain names are **symlinks** to the dated files: the raw fetch of `7nm_TT.pm` returns the 16-byte string `7nm_TT_160803.pm`. A recipe must point at `*_160803.pm`, or resolve the link after a git checkout. The layout is **one file per corner (TT/FF/SS), with no `.lib` sections**.

### 2.4 Model format (`7nm_TT_160803.pm`, header `** ASAP TT models v1.0 8/3/16`, `** Hspice modelcard`)
- There are 8 cards, all `level = 72` (**BSIM-CMG**) with `version = 107`: `nmos_lvt nmos_rvt nmos_slvt nmos_sram pmos_lvt pmos_rvt pmos_slvt pmos_sram`. The kit README describes them as "four Vt flavors, viz. regular Vt (RVT), low Vt (LVT), super-low Vt (SLVT), and SRAM Vt" (install README).
- Common switches: `bulkmod=1 igcmod=1 igbmod=0 gidlmod=1 geomod=1 coremod=0 cgeomod=0 rdsmod=0 nqsmod=0 sdterm=0 tnom=25`, plus `eot=1n toxp=2.1n deltaw=0`.

### 2.5 Device params
- Card defaults (all 8 cards): `l = 2.1e-8` (21 nm), `fpitch = 2.7e-8` (27 nm), `hfin = 3.2e-8` (32 nm), `tfin = 6.5e-9` (6.5 nm). The instance parameter is `nfin`.
- Clark et al., "ASAP7: A 7-nm finFET predictive process design kit", *Microelectronics Journal* 53 (2016) 105–115, doi:10.1016/j.mejo.2016.04.006 (https://www.sciencedirect.com/science/article/pii/S002626921630026X). The paper is paywalled. These sentences come from search-engine snippets of it and match the card values: "fins are 32 nm in height and 6.5 nm thick, on a 27 nm pitch". "To allow a 1 nm drawn grid, the drawn fin width is rounded to 7 nm". "Gates are drawn with a 20 nm gate length to stay on a 1 nm grid while the actual length is 21 nm". "contacted poly pitch (CPP) of 54 nm". VDD nominal is 0.7 V. **Unverified against the full text.**
- So the schematic `l` is 20 nm drawn and 21 nm modelled. Third-party ngspice decks pass `l=20n` ([2AMLogic ADAPTATIONS](https://github.com/2AMLogic/pdks/blob/main/ngspice/ADAPTATIONS.md)), while the card default is 21 n. Which value the Cadence CDF passes is **unverified**.
- Effective width per fin: with `geomod=1` (triple gate in BSIM-CMG) the first-order value is 2·Hfin + Tfin = 2·32 + 6.5 = **70.5 nm/fin**. This is computed from the card values. The exact BSIM-CMG Weff formula (with corner/FECH terms) is **unverified**.

---

## 3. ngspice and BSIM-CMG

- **No built-in BSIM-CMG. Ran locally:** ngspice-44.2 on `7nm_TT_160803.pm` prints "Only MOS device levels 1-6,8-10,14,49,54-58,60,68,73 are supported in this binary" and then "could not find a valid modelname". `level=72` is not built in. nixpkgs currently ships **ngspice 45** (`nix search nixpkgs ^ngspice$`, 2026-10-12). ngspice NEWS for 39–45 lists no built-in BSIM-CMG, only OSDI work ([NEWS](https://sourceforge.net/p/ngspice/ngspice/ci/master/tree/NEWS)). I infer that 45 behaves the same; it was not run (**unverified**).
- **OSDI route:** "OSDI interface integrated to read und simulate with OpenVAF-compiled Verilog-A compact models" was added in **ngspice-39** (2023-01-31). `NGSPICE_OSDI_DIR` arrived in ngspice-43, and OSDI noise in ngspice-45 (NEWS). The ngspice site says it "enables modern models like PSP or FinFET model BSIM-CMG" and points to VA-Models (https://ngspice.sourceforge.io/extras.html). **Ran locally:** ngspice-44.2 knows `pre_osdi`, so the loader is compiled in.
- **How the card must change for OSDI:** a model is picked by Verilog-A module name, not by `level`, and polarity comes from `DEVTYPE`. 2AMLogic rewrites `.model X nmos level = 72` → `.model X bsimcmg devtype=1` (pmos → `devtype=0`) and instantiates with an `N` prefix: `Nn1 d g s b nmos_rvt l=20n nfin=3` ([ADAPTATIONS.md](https://github.com/2AMLogic/pdks/blob/main/ngspice/ADAPTATIONS.md)). The module name depends on the source used. VA-Models' `bsimcmg.va` declares `module bsimcmg_va(d, g, s, e, t);` (fetched at VA @ 6099b4e), so its cards say `bsimcmg_va`. The VA-Models README shows `.model BSIMCMG_N bsimcmg_va` ([README](https://github.com/dwarning/VA-Models)).
- **Version mismatch:** the ASAP7 cards say `version = 107`. The latest public releases are 111.2.1 (2022-06-06), 112.0.0 (2024-12-26) and 112.1.0 (2026-04-28), with 107.0.0 dated 2013-07-12 (https://www.bsim.berkeley.edu/models/bsimcmg/). VA-Models ships **111.2.1** (`code/bsimcmg/vacode/`, Changelog "Origin of the model Version 111.2.1"), plus `vacode110/` and `vacode111/`. 2AMLogic instead compiles **BSIM-CMG 107.0.0** (from CedarEDA/CMC.jl) to OSDI for ngspice 47. It needs two VA-source patches: 26 parameters tagged `(*type="instance"*)` so that `L NFIN NF …` become instance parameters, and the `EOTACC` lower bound relaxed to `[1e-10:inf)` because ASAP7 sets `eotacc = 1e-10`. The PDK cards themselves need only the `.model` header rewrite. It reports "73/73 against the PDK paper" ([2AMLogic/pdks](https://github.com/2AMLogic/pdks)). Run locally (§5): the v107 cards run on the 111.2.1 OSDI after the header rewrite; ngspice ignores `version`, `coremod`, `capmod`, `nseg`, `etaqm` with an "unrecognized parameter" warning. That source's polarity parameter is `TYPE` (1 = n, -1 = p; `bsimcmg_parameters.include`), not `DEVTYPE`.
- **Known warnings:** ngspice support request #67 reports "unrecognized parameter" warnings (`nseg`, `etaqm`) from ngspice's old bundled example BSIM-CMG cards (v110). The maintainer's advice is to use VA-Models 111.2.1 and name the binary by version, e.g. `bsimcmg111.osdi` (https://sourceforge.net/p/ngspice/support-requests/67/). Other ASAP7-on-ngspice efforts: [shaurya0406/ASAP7_FinFET_Inverter_Characterization](https://github.com/shaurya0406/ASAP7_FinFET_Inverter_Characterization) (xschem + OpenVAF-built OSDI, `pre_osdi`, sweeps `nfin`). VLSIDA's tutorial says the `.pm` files are "HSpice-only out of the box … Running them in ngspice requires patching" (https://github.com/VLSIDA/chip-tutorials/blob/main/asap7.md).
- **OpenVAF is in nixpkgs as `openvaf` 24.0.1 (OpenVAF-Reloaded), binary `openvaf-r`** (the repo's flake dev shell has it; `nix search` by name missed it). Precompiled `.osdi` files: VA-Models' README says its `osdilibs/` directory has Linux/Windows binaries, but my tree grep found no `bsimcmg*.osdi` at 6099b4e (**unverified**).
- **BSIM-CMG source licence:** VA-Models' `code/bsimcmg/vacode*/LICENSE.txt` reads "Copyright 2019 University of California … Licensed under Educational Community License, Version 2.0" (**ECL-2.0**, http://opensource.org/licenses/ECL-2.0). Berkeley's "Agreement for BSIM use" grants the right to "modify, copy, and redistribute"; users may not "charge for the University of California code itself", and products must acknowledge UC (https://www.bsim.berkeley.edu/agreement-for-bsim-use/). The VA source is publicly downloadable from the BSIM page, with beta versions reserved to CMC members. A search summary also claimed CC-BY-4.0 for newer releases (**unverified**). Either way, redistributing a compiled `.osdi` with attribution looks allowed. Fetching it at install time avoids the question entirely.

## 4. Verdict for a recipe

| | FreePDK45 | ASAP7 |
|---|---|---|
| Fetch | Registration only on official site. Unofficial pinned mirror: baichen318 @ a75cb11 | No registration: asap7_pdk_r1p7 @ 58d72c9 |
| Licence | Apache-2.0 (SVRF decks: Mentor EULA) | BSD-3-Clause |
| Model files | `ncsu_basekit/models/hspice/tran_models/models_{nom,ff,ss}/{N,P}MOS_{VTL,VTG,VTH,THKOX}.inc` | `models/hspice/7nm_{TT,FF,SS}_160803.pm` |
| Model | BSIM4 level 54, version 4.0 | BSIM-CMG level 72, version 107 |
| Sizing | W ≥ 90 n, L ≥ 50 n | `nfin` (integer), L = 20 n drawn / 21 n model |
| ngspice | **Works as is** (VTL nom probed: no warnings) | **Not built in.** Needs an OSDI `bsimcmg` (OpenVAF build), a `.model` header rewrite to `bsimcmg[_va] devtype=…` and `N`-prefixed instances |

## 5. Verified end to end (2026-10-12, this machine)

ngspice-44.2, OpenVAF-Reloaded 24.0.1 (`openvaf-r`), inverter DC sweep `vin` 0 → vdd:

| Kit | How | v(out) at vin = 0 | at vin ≈ vdd | vout = vin at |
|---|---|---|---|---|
| FreePDK45 @ a75cb11 (codeload tarball, sha256 `5b93a0b1…532e`) | BSIM4 cards included as-is from a generated `.lib tt` section (relative `.include` resolves against the including file's directory) | 1.0997 V | 7.5e-5 V | 0.543 V (vdd 1.1) |
| ASAP7 @ 58d72c9 + VA-Models @ 6099b4e | `bsimcmg.va` → `openvaf-r` → `bsimcmg.osdi`, `pre_osdi`; cards rewritten `.model X bsimcmg_va type=±1`; `N` instances, 4 nodes (the thermal node `t` is optional) | 0.7000 V | 2.2e-7 V | 0.365 V (vdd 0.7) |

Both ran through the plugin (`pdk.install`, `pdk.use`) and `schemify run` on the `tech:` inverter of `crates/cli/tests/fixtures/tech/inv.shcomp` (w 2u/1u, l 0.5u; ASAP7 nfin 15/15 with nf 2/1): `plugins/pdk-manager/tests/guest.rs` `real_kits_simulate_an_inverter`. The ff/ss corners, the other flavours and `m > 1` on OSDI were loaded but not checked against a reference.
