# Virtuoso interop: SKILL tooling, lexical rules, bridges, OA, prior art

Research date 2026-10-12. Confidence tags: **VERIFIED** (read in the cited source),
**LIKELY** (secondary source / strong inference), **GUESS** (unconfirmed).
Clean-room note (D1): GPL/LGPL projects below were evaluated from README/docs and repo
metadata only. Source was read only for permissively licensed projects (BAG BSD-3, cbag
Apache/BSD, virtuoso-bridge-lite MIT, zed-skill MIT). Facts recorded, nothing copied.

Primary language reference used throughout: *SKILL Language User Guide, Product Version
06.70, January 2007* (Cadence), public mirror:
<https://bpb-us-e1.wpmucdn.com/sites.gatech.edu/dist/0/367/files/2016/03/Intro_to_skill_prog.pdf>
(cited below as **[SLUG]** with page numbers). It is from 2007, so newer IC6/IC23 releases may
differ in small ways. Anything that may have changed is flagged.

---

## 1. Open-source SKILL tooling that runs without Cadence

| Project | License | Maturity | What it does | C-call `f(a b)` / infix / `~>` / `?key` | Usable in our CI? |
|---|---|---|---|---|---|
| **zed-skill** (`tree-sitter-skill/` + `skill-lsp/`) <https://github.com/deanyou/zed-skill> | MIT | v0.2.1, 16 commits, 0 stars, last push 2026-09 | Tree-sitter grammar plus a Rust tower-lsp server: completion, hover, paren/quote-balance diagnostics | The grammar is **Lisp-only**: no infix, `f(` is read as symbol `f` followed by a list, `a+b` is one symbol. `?key` is a token. Block comments are handled. (VERIFIED, read `grammar.js`) | Works only as a paren/string-balance checker. Too weak to catch real syntax errors. |
| **Pill** <https://github.com/jonpry/Pill> | **GPL-2.0** | 120 commits, last push 2020-08, 77 stars | SKILL interpreter in Python (parsimonious PEG, compiles to Python bytecode), built to run PCell code through KLayout. Ships tools that dump binary contexts and **decrypt `.ile`** files | README says it handles infix, `~>`, `?key`, `let/foreach/when/case/procedure`. It warns that parsing depends on knowing which identifiers are functions (VERIFIED, README) | **No.** GPL (D1 forbids reading it), stale, depends on KLayout/g++, and the decrypt tooling is a legal hazard. Not even as a black box. |
| **Skillup** (Skill Verifier static analyzer) <https://github.com/skillup-core/skillup> | "Source Available License", **non-commercial only** | active 2026 | Python static checks: undeclared vars, undefined functions, paren mismatch, `=` in conditions. "Skill Bot" drives a live Virtuoso via xdotool, so it is not an interpreter | n/a | **No** (license). |
| **SKILL#** <https://github.com/Da-Buche/skill-sharp> | MIT | 9 stars, 2025-12 | Lint rules, unit tests, docgen. Runs on Cadence's `$CDS_INST_DIR/tools.lnx86/dfII/bin/skill` interpreter (VERIFIED, README) | real SKILL | Only where Cadence is installed. |
| **SKILL_Tools / qtest** <https://github.com/MatthewLoveQUB/SKILL_Tools> | MIT | 2020 | `unittest`-style framework written in SKILL++ | real SKILL | Needs Virtuoso. |
| **skill-unit-test** <https://github.com/skyworksinc/skill-unit-test>, <https://github.com/curtisma/SKILL_Unit_Test> | GPL-3.0 | 2020-21 | SKILL++ unit-test framework | real SKILL | Needs Virtuoso; GPL. |
| **virtue** <https://github.com/cascode-labs/virtue> | MIT | 57 stars, 2025-12 | SKILL++ framework with a TOML reader/writer and a test framework (`tests/*.ils`) | real SKILL | Needs Virtuoso. Prior art for SKILL-side serialization. |
| **Skill +** VS Code extension (herbertagosto.skillherb) <https://marketplace.visualstudio.com/items?itemName=herbertagosto.skillherb> | not stated, no public repo | v0.10.1 (2026-10) | Highlighting, go-to-definition, completion. The linter "checks only for defun and procedure syntax errors" (VERIFIED) | n/a | No. |
| **vim-skill** <https://github.com/AugustUnderground/vim-skill> | BSD-3 | 2021 | Syntax highlighting plus a REPL bridge (`virtuoso -nographE`) | n/a | No. |
| **SKILLdoc-ng** <https://github.com/IFTE-EDA/SKILLdoc-ng> | MIT | 2015 | Doc generator | n/a | No. |
| Cadence **SKILL Lint** (`sklint(?file "x.il" ?outputFile "x.lnt")`) and the standalone `tools/dfII/bin/skill`, `dbAccess`, `virtuoso -nograph` binaries | proprietary | — | Real lint and real interpreter. `skill` covers core SKILL only, `dbAccess` adds the db API, `virtuoso -nograph` covers everything (LIKELY, Google-groups / Cadence-forum snippets: <https://groups.google.com/g/comp.cad.cadence/c/SF2XDDeHZPk>) | real | Only with a Cadence install. This is the optional "live" tier. |

**Not found** (searched GitHub topics `cadence-skill`, `virtuoso`, `cadence-virtuoso`, `skill-language` and the web):
an ANTLR SKILL grammar, a full-syntax tree-sitter grammar (infix + C-call), a permissively
licensed SKILL interpreter in Python/Rust/CL/Scheme, or a mock framework for the `db*`/`sch*` API.
(LIKELY none exist publicly. Search coverage was GitHub search plus web search.)

### Why borrowing a host Lisp does not work cheaply (analysis, LIKELY)
- **Guile/Racket (Scheme):** `'()` is truthy, but in SKILL `nil` is both the empty list and false ([SLUG] p.71). List-walking code would silently change meaning.
- **Common Lisp:** `nil`/`t` semantics match, but the CL reader does not interpret C escapes (`"\n"` reads as `n`), and `1u` reads as a symbol.
- **Emacs Lisp:** closest cousin (dynamic scope, `nil`=`()`, C-like string escapes). But `?key` reads as a character literal, there are no scale suffixes, and C-call/infix syntax is absent.
- The common failure is the reader. SKILL's mixed C-call/infix syntax plus scale-suffixed numbers needs its own reader anyway.

### Recommendation for (a) syntax-check and (b) pure-logic execution in CI
1. **(a) Write our own SKILL reader in Rust**, as test-only code in the interop crate, implementing §2 below: comments, strings (escapes, `\ddd`, the 8191-character literal limit), numbers with scale suffixes, `\`-escaped symbols, `?key`, the no-space-before-`(` call rule, unary-minus adjacency, and the [SLUG] p.121-123 precedence table for `= == != && || + - * / ** -> ~> : !`. Pair it with a **house-subset lint**: a whitelist of special forms and Cadence functions our scripts may call, plus arity checks. This has zero dependencies, runs offline and in Nix, and is the cheapest reliable option. Estimate ~400 lines of reader plus ~150 of lint. *Cheapest variant:* write our `.il` files entirely in **prefix Lisp form**. SKILL accepts `(fn a b)` everywhere ([SLUG] p.64), and `obj->p` is `getq` / `putpropq` ([SLUG] p.122). The reader then drops to an s-expression reader of about 100 lines, and the infix table is only needed for the lint that rejects infix.
2. **(b) A tiny evaluator over that reader** for the pure functions only: the string escaper, the record printer, and the line/record parser in the loader. It needs `let`, `setq`, `if/when/unless/cond/case`, `foreach`, `while`, `procedure`, `strcat`, `strlen`, `substring`, `getchar`/`parseString`, `sprintf` with `%s`/`%d`, and list ops. Then run property tests: Rust writes a record, the evaluated SKILL loader parses it, and the results must be equal, and the same in the reverse direction. `ponytail:` the evaluator's oracle is our reading of [SLUG]. Upgrade path: golden outputs captured from a real Virtuoso run (tier 3 below) replace the hand-written expectations.
3. **(c) Live tier, optional.** When `CDS_INST_DIR`/`virtuoso` is on `PATH`, run the same `.il` through `virtuoso -nograph -replay run.il -log x.log` (flag names LIKELY), or through skillbridge/`dbAccess`, and diff against the goldens. CI skips this tier when Cadence is absent.

Keep the db-touching code **thin and free of logic**: open the cellview, iterate, call the pure printer. Then everything worth unit-testing sits in the pure layer that (b) can execute. A db mock is not worth building: no prior art exists, and mocking `dbOpenCellViewByType`/`~>` semantics faithfully is the hard part.

---

## 2. SKILL lexical rules (from [SLUG] unless noted)

| Topic | Rule | Source / confidence |
|---|---|---|
| Case | Data and function names are case sensitive | [SLUG] p.32 ("SKILL data is case sensitive", "Function names are case sensitive"), VERIFIED |
| Comments | `;` to end of line. `/* ... */` block comments that **do not nest** (the first `*/` ends the comment) | [SLUG] p.63, VERIFIED |
| Whitespace | ` \t \f \r \n \v` | [SLUG] p.64, VERIFIED |
| Call syntax | `f(a b c)` and `(f a b c)` are equivalent. **No space** is allowed between the name and `(`: `f (a b)` is illegal. Arguments are separated by whitespace, and the comma is an optional separator | [SLUG] p.32, p.62-64, VERIFIED |
| Redundant parens | `(x)` or `((a+b))` makes the inner value be called as a function. Never wrap a lone variable or constant in parentheses | [SLUG] p.64, VERIFIED |
| Unary minus | Must touch its operand (`-1`, `-a`). Binary minus needs spaces on both sides or neither: `a -b` is illegal | [SLUG] p.63, VERIFIED |
| Special chars | `\ () [] {} ' " , ; : . + - * / ! ^ & \| < > = # @ ? `` ` `` % $`. `#` in column 1 means special parsing. `@` as first char means a reserved word (`@key`, `@rest`, `@optional`). `?` as first char means a keyword parameter. `{}` is `progn`. `$` is reserved | [SLUG] p.62, VERIFIED |
| Symbols | `[A-Za-z0-9_?]`. A leading digit must be `\`-escaped. Any other printable character must be `\`-escaped (`\*name\+` names `*name+`). Print name is at most 255 chars | [SLUG] p.72, VERIFIED |
| Characters | No char type. Characters are single-char symbols (`'a`, `'\120` is `P`). NUL cannot be represented | [SLUG] p.72-73, VERIFIED |
| Strings | `"..."` with C escapes `\n \t \v \b \r \f \\ \"` and `\ddd` (2-3 octal digits). **The parser limits a string literal to 8191 characters.** Longer strings can exist at runtime but cannot be read back from source or `lineread` | [SLUG] p.70-71, VERIFIED (limit as of 06.70; may have been raised later, GUESS) |
| Integers | Decimal. `0b`/`0B` binary, leading `0` **octal** (`077`=63, so beware zero-padded numbers), `0x`/`0X` hex. Fixnums are **32-bit** in 06.70 | [SLUG] p.68-69, VERIFIED (64-bit in current IC releases is a GUESS) |
| Floats | `10.0 10. 0.5 .5 1e10 1e-10 1.1e10`, double precision | [SLUG] p.69, p.130, VERIFIED |
| **Scale suffixes** | **Yes, lexical and always on, with no `sstatus` switch.** `Y Z E P T G M k/K % m u n p f a z y` directly after a number (no space): `1.2u` reads as 1.2e-6, `5%` as 0.05, `10k` as 10000. Only the **first** non-numeric character counts: "other characters following the scaling factor are ignored", so `10um` reads as 10e-6. Integer times an integral factor stays an integer; otherwise the result is a float. Note `M` is mega and `m` is milli | [SLUG] p.69-70, VERIFIED |
| `sstatus` | The flags relevant to us are `fullPrecision` (below), `integermode` (makes the parser emit integer-only arithmetic), and `printlength`/`printlevel` (which truncate printed lists, so make sure they are nil in the dumper) | [SLUG] p.60, p.123; forum (below) |
| Operators | Precedence table on [SLUG] p.121-123: `[] <> ' . -> ~>` then unary `++ -- - ! ~` then `**`, `* /`, `+ -`, `<< >>`, `< > <= >=`, `== !=`, bitwise, `&& \|\|`, `=`. `x:y` builds a point list `(x y)` | VERIFIED |
| `~>` | Same as `->` on one object. On a list, it maps over the elements and returns a list | [SLUG] p.90, VERIFIED |
| `print`/`println` | Default format: strings are printed **with double quotes** (`print("hello")` prints `"hello"`) | [SLUG] p.41, p.162, VERIFIED |
| `%L` | "Default format for the data type", i.e. the same as `print`. Ignores width and precision. `%P` prints a point, `%B` a box, `%A` any value | [SLUG] p.42, p.164, VERIFIED |
| Embedded quotes in printed strings | `println` and CIW echo show escapes (`\"`) "so that the displayed value could be copied and reused as valid input" | Cadence forum answer quoted in search: <https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/19409>, LIKELY. How non-printables and non-ASCII come out (octal? raw?) is a GUESS; capture a golden |
| Float printing | The default print representation is **`%.7g`, i.e. 7 significant digits, which is lossy**. `sstatus(fullPrecision t)` raises it to 16 digits. `printf("%.16g" x)` also works | Andrew Beckett answer, <https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/41466>, VERIFIED (fetched) |
| `lineread(port)` | **Parses, does not evaluate.** Reads one line, continuing while parens are open or a binary infix operator lacks its right operand. Returns **`t` for a blank line** and **`nil` at EOF**. Returns the *list of forms on the line*: the line `f(a b c)` gives `((f a b c))`, and the line `f 1 2 +` / `3` gives `(f (1 (2 + 3)))` | [SLUG] p.169, VERIFIED. Comment-only lines also give `t` (LIKELY, <https://groups.google.com/g/comp.cad.cadence/c/ZaaVTtW56_0>) |
| `linereadstring(s)` | `lineread` on a string. Only the **first** form is returned, the rest is ignored | [SLUG] p.169, VERIFIED |
| `load` / `loadstring` / `evalstring` / `errsetstring` | `load` calls `lineread` repeatedly and evaluates each form. `evalstring` does not allow the outer parens to be omitted. `errsetstring(s t)` traps errors and returns `nil` on error, otherwise `(value)` | [SLUG] p.167-168, VERIFIED |
| `fscanf` | Strings read are capped at 8K. "lineread is a faster alternative" | [SLUG] p.170, VERIFIED |

**Consequences for our wire format.** These are design inferences from the rules above.
- When the loader reads data with `lineread`, write only **strings, integers and nested lists**. Bare words would become symbols, would be subject to scale suffixes (`1u`, `5m`), would trigger the octal-leading-zero rule, and would be infix-parsed (`a-b`, `x:y`). Quote every name.
- Keep every string literal under 8191 characters. Chunk long text (doc blocks) into lists of strings.
- **Never print floats with `%L`/`print`**, because 7 digits is lossy. Dump coordinates as **integers in database units** (`round(x * cv~>DBUPerUU)` printed with `%d`), which also matches D33's i32 coordinates. For any float that must stay a float, use `%.17g`.
- Escape strings ourselves, with one escaper function covering `\\`, `\"`, `\n`, and `\ddd` for other control bytes. Then the format is defined by *our* spec rather than by version-dependent `print` behaviour.
- One record per line, so `lineread` returns `((record))` per call. Treat `t` (blank line) as skip and `nil` as end of input.
- An alternative used by BAG: a line-tagged format (`#list`, `#prop_list`, `#int N`, `#float X`, `#bool 0|1`, `#end`, one value per line) read with `gets` + `parseString`, which never touches the SKILL reader (BSD, <https://github.com/ucb-art/BAG_framework/blob/master/run_scripts/start_bag.il>, VERIFIED). It avoids the reader's quirks but makes the format line-oriented and needs a hand parser in SKILL.

---

## 3. Python/IPC bridges (for users who do have Virtuoso)

| Project | License | Transport | Notes |
|---|---|---|---|
| **skillbridge** <https://github.com/unihd-cag/skillbridge>, docs <https://unihd-cag.github.io/skillbridge/> | **LGPL-3.0** | In the CIW, `load("<skillbridge path>")` then `pyStartServer(?id "default" ?logLevel ?singleMode ?timeout ?python "python" ?forceTcp)`. The SKILL side starts a Python server with `ipcBeginProcess`. **IPC protocol**: the server writes one line of SKILL code to the child's stdin, SKILL replies with one line `success <printed value>` or `failure <msg>`, and a missing reply after 60 s counts as a timeout. **Client protocol**: Unix socket (TCP with `?forceTcp`), every message prefixed by a 10-digit length | VERIFIED (docs: usage/server.html, reference/protocol.html). 347 stars, 424 commits, Python ≥3.8, `Workspace.open()` → `ws.db.open_cell_view(...)` (snake_case mapped to camelCase). Never interleave commands from two connections |
| **virtuoso-bridge-lite** <https://github.com/Arcadia-1/virtuoso-bridge-lite> | MIT | SSH tunnel + a SKILL loader in the CIW; executes inline SKILL and returns the printed results | VERIFIED. 812 stars, active 2026-10. Schematic read/create/params/pins/wires. `skill_output.py` has a Python parser for `%L` output, and `escape_skill_string` escapes only `\` and `"` (VERIFIED, read source). Its CDF-callback approach is described in §5 |
| **via** <https://github.com/si-view/via> (crate `virtuoso-via`) | **no license** (all rights reserved) | Rust binary launched by `ipcBeginProcess`, Unix socket, JSON results | VERIFIED README. Do not use or copy |
| CadenceSKILL-Python <https://github.com/unnir/CadenceSKILL-Python> | none | IPC example (2016) | — |
| socad <https://github.com/mdmfernandes/socad> | GPL-3.0 | sockets | — |
| SiliCAD/EDA_MCP <https://github.com/SiliCAD/EDA_MCP> | MIT | MCP server driving remote Virtuoso | 2026 |

**Can our harness optionally use skillbridge?** Yes, as an optional, unvendored dependency
(LGPL use is fine; we never ship it). With `Workspace.open()`, call `load` on our `.il`, then call
our dump/load entry points and compare files. **Cheaper still, and needing no Python:** the
harness spawns `virtuoso -nograph -replay harness.il -log out.log` (LIKELY flags), or runs the
`dbAccess` binary for db-only scripts (LIKELY), and diffs the produced files against the goldens.
A third option needs no third-party code at all. The documented line protocol is trivial: our own
Rust test binary can be started from SKILL via `ipcBeginProcess` and read results from stdin,
exactly as BAG does with `ipcBeginProcess` / `ipcWriteProcess` / `errsetstring` (BSD, VERIFIED).

---

## 4. Alternatives to SKILL for reading/writing Virtuoso data

**OpenAccess (Si2).**
- The API and reference implementation source are available **only to OpenAccess Coalition members** who sign Si2 license agreements. It is not OSI open source. (VERIFIED: <https://en.wikipedia.org/wiki/OpenAccess>; the Si2 FAQ mirror says project files "require that your company has signed project-specific license agreements": <http://vlsicad.eecs.umich.edu/BK/Slots/cache/www.si2.org/openaccess/faq2.html>.)
- Current line: oaScript 5.1 (Python/Perl/Ruby/Tcl bindings) supports OA 22.60/22.61, and 6.0 targets 22.62. Released 2024-10, Si2 member portal. (VERIFIED: <https://si2.org/openaccess-coalition-releases-new-version-of-oascript/>; the language list is LIKELY.)
- An unofficial mirror of the OA 2.2 API docs exists at <https://oa22doc.github.io/relnotes/intro.html> (VERIFIED exists; its copyright status is doubtful).
- Virtuoso installs ship OA shared libraries (and LIKELY headers). Users with a Cadence install build against them via `OA_INCLUDE_DIR`/`OA_LINK_DIR`, which is what cbag does (VERIFIED in cbag `CMakeLists.txt`, links `oaBase oaCommon oaPlugIn oaDM oaDMFileSysBase oaTech oaDesign`).

**Bindings that use OA.**
- **cbag/pybag** (BAG3's C++ database). License Apache-2.0, some files also BSD-3. Reads and writes OA schematics/layouts directly through the OA C++ API: `oaScalarInst::create`, `oaNet::create`, `oaTerm::create`, `oaPin::create`, `oaAppProp`, `oaCdbaNS`. Restrictions include bus pins only with step 1 and no `a,b` or `<*3>` names. (VERIFIED: <https://github.com/bluecheetah/cbag>.) This is the only open Rust/Python-reachable OA *writer* found, and it still needs proprietary OA libraries at link time.
- **Xic** (XicTools, Apache-2.0, <https://github.com/wrcad/xictools>). Has an OA plug-in (`xic/src/oa`) that must be built from **OA source**. It has "fairly reasonable capability of taking Virtuoso designs into Xic", but "**schematic and schematic symbol views from Xic can not be read as anything but garbage by Virtuoso**" (VERIFIED: <https://www.wrcad.com/manual/xicmanual/node62.html>). This is strong evidence that writing OA schematics directly, without Cadence's own schematic conventions (CDF, extraction/check stamps), does not produce usable Virtuoso schematics. Use SKILL plus `schCheck` for writes.
- **No Rust OA bindings** found (LIKELY none).

**Reverse-engineered OA readers.** **oaFileParser** <https://github.com/EDDRSoftware/oaFileParser>
(MIT, 16 commits, 2017, abandoned) is a low-level callback parser of `*.oa`/`tech.db` with no data
model (VERIFIED README). No other public OA file-format reader was found. KLayout does not read
OA (LIKELY). `sch.oa` has no public format documentation (LIKELY). Not a viable base.

**How other commercial tools interoperate.**
- **Keysight ADS:** native OA libraries shared with Virtuoso ("only a single OpenAccess schematic view representation exists", ADS 2016+; ADS 2017 moved to OA 22.50 to use Virtuoso's Express PCell plug-in). Older RFIC Dynamic Link used a live Virtuoso session. (LIKELY: <https://docs.keysight.com/display/engdocads/ADS+2017+Release+Notes>, <https://edadownload.software.keysight.com/eedl/ads/2011_01/pdf/dynlnkug.pdf>.)
- **Synopsys Custom Compiler:** native OA. PCells are Tcl/Python (PyCell), not SKILL, and it works with "interoperable PDKs" (iPDK). SKILL CDF callbacks do not run there. (LIKELY: <https://semiwiki.com/eda/synopsys/1615-custom-ic-and-ams-tool-flow-with-synopsys/>, <https://www.edn.com/synopsys-unleashes-orion-now-custom-designer-to-hunt-down-virtuoso/>.)
- **Siemens Tanner S-Edit:** imports native OA (v16.10+), EDIF (with Cadence-flavour conversion), SPICE and Verilog. For Cadence OA it translates `cdsParam` properties **from `cdfDump` files**, so CDF is moved as a dump, not executed. Forum users report OA schematic exchange with Cadence failing, and EDIF with pre/post cleanup as the recommended route. (LIKELY: S-Edit datasheet <https://www.ednc.com/wp/wp-content/uploads/2017/03/s-edit-ds.pdf>; <https://community.cadence.com/cadence_technology_forums/f/custom-ic-design/41168>.)

**EDIF 200 with Virtuoso.**
- Virtuoso has EDIF 200 Reader/Writer products (also EDIF 300) (VERIFIED: <http://www.princeton.edu/~cad/cadence/Cadence.htm>).
- Driven from CIW File → Export/Import → EDIF 200 with a **template file** (`tools/dfII/samples/xlUI/edifOut.il`). Command-line `edifout <template.il>` / `edifin <template.il>` exist (LIKELY, <https://www.vrg.utoronto.ca/vrg/DesignManual/CDS_SYN_2006.pdf>).
- What survives: symbols are recreated as cellviews in an imported library, along with instances, wires and connectivity. Global net names lose `!` (`VSS!` → `VSS`), and vendor-specific simulator properties (e.g. Spectre `model` vs another tool's `MNAME`) need manual remapping (VERIFIED for a Virtuoso → Silvaco Gateway case: <https://silvaco.com/wp-content/uploads/content/appNotes/analog/1-012_EDIF200.pdf>).
- EDIF 200 itself has weak bus semantics and no multi-symbol or page-border support (LIKELY, <https://en.wikipedia.org/wiki/EDIF>). CDF parameter semantics and callbacks do not cross (LIKELY).
- Verdict: EDIF is a lossy fallback. SKILL is the higher-fidelity path.

---

## 5. Prior art: Virtuoso schematic ⇄ text via SKILL

### BAG2: `run_scripts/start_bag.il` (BSD-3, <https://github.com/ucb-art/BAG_framework>, VERIFIED, source read)
- **Server.** `ipcBeginProcess("bash virt_server.sh" "" 'stdoutHandler 'stderrHandler 'exitHandler "")`. Each stdout line from Python is evaluated with `errsetstring(data t)`. The reply is `sprintf("%A\n" result)`, written as a length line and then the payload via `ipcWriteProcess`. On error the reply is `car(nthelem(5 errset.errset))`.
- **Input data.** Read from the tagged line format with `infile`/`gets`/`parseString`/`atoi`/`cdfParseFloatString`, as described in §2. Uses disembodied property lists (`ncons(nil)` + `putprop`) and `tconc` for list building.
- **Dump (`parse_cad_sch`).** `dbOpenCellViewByType(lib cell "schematic" nil "r")` (falls back to `"veriloga"`). Then `cv~>terminals~>name`, and for each `cv~>instances`: `inst~>name/libName/cellName`, `inst~>instTerms` → `~>name ~>direction ~>net~>name ~>numBits`. Writes YAML with `fprintf`, quoting pin and net names with `sprintf("\"%s\"")`, but **without escaping** embedded quotes or backslashes. `outfile`/`close`/`dbClose` at the end.
- **Copy templates.** `gdmCreateSpecList`, `gdmCreateSpec(lib cell nil nil "CDBA")`, `gdmAddSpecToSpecList`, then `ccpCopy(src dst t 'CCP_EXPAND_COMANAGED nil nil "" "" 'CCP_UPDATE_FROM_LIBLIST empty)`. `ccpCopy` cannot copy one cell to several targets in one call, so BAG batches unique sources per pass. Before copying it calls `dbPurge` on open target views. Libraries come from `ddGetObj`/`ddCreateLib` + `techBindTechFile` + `ddReleaseObj`.
- **Instantiate and modify (`modify_schematic_content`).** `dbFindAnyInstByName`. Deletes with `dbDeleteObject`, and wires with `schDelete`. Rename is `inst->name = new`. Changing the master uses `schReplaceProperty(list(inst) "master" "lib cell view")`. Arraying copies via `dbCopyFig(inst nil transform)`.
- **CDF params.** `foreach(p cdfGetInstCDF(inst)~>parameters ... p->value = strValue)`, with values always strings. **No callbacks run.** The file header says `CCSinvokeCdfCallbacks()` (Cadence solution 11018344) was omitted for licensing reasons and is only needed for PCells.
- **Connectivity.** Terminals are assumed to carry one stub wire with a label. To find the stub, BAG transforms the master pin bbox with `dbTransformBBox(pin~>fig~>bBox inst~>transform)`, snaps to `schGetEnv("schSnapSpacing")` using `round2`, and matches that point against `net~>figs~>points`. Renaming a net means deleting the wire and recreating it with `schCreateWire(cv "draw" "full" points snap snap 0)` + `schCreateWireLabel(cv wire xy name "lowerCenter" "R0" "stick" 0.0625 nil)`. Unwired terminals get `dbCreateInstTerm(nil inst term)` plus a stub wire and label.
- **Pins.** Schematic pins: `schCreatePin(cv pinMaster name dir nil xy "R0")` (pin masters such as `basic ipin/opin/iopin symbol`). To rename, BAG creates a pin under a **temporary unique terminal name**, then sets `term~>name`, which sidesteps bit-width conflicts. Symbol pins: `schCreateSymbolPin`, `schCreateSymbolLabel(cv xy "pin label" name ...)`, `schEditPinOrder(cv order t)`, then termOrder updated per simulator through `cdfGetBaseCellCDF` + `cdfSaveCDF`.
- **Save.** `schSetEnv("checkHierSave" t)`, `schSetEnv("saveAction" "Save")`, `schCheckHier(cv "schematic symbol" "")` (warns on errors), then `dbReopen(cv "r")` + `dbClose`. This drops edit locks even when dependent symbols keep the cv open.

### BAG3 (<https://github.com/bluecheetah/bag>, BSD-3 + Apache-2.0, VERIFIED)
`start_bag.il` shrinks to about 250 lines (server, `release_write_locks`, `refresh_cellviews` via
`dbRefreshCellView`, `modify_label`). **Schematic generation moved out of SKILL into cbag, which
writes OA directly.** That works for BAG because it starts from Virtuoso-made *templates*, and so
inherits Cadence's conventions. Xic's experience writing schematics from scratch says otherwise.

### virtuoso-bridge-lite (MIT, VERIFIED, source read)
- Uses `dbCreateInst`, `schCreateWire`, `schCreateWireLabel`, `schCreatePin`, `schCreateNetExpression`, `dbReplaceProp`, `schCheck`, `dbSave`, `dbTransformPoint`.
- **CDF callbacks.** Re-implements the `CCSinvokeCdfCallbacks` idea:
  1. `cCDF = cdfGetCellCDF(ddGetObj(lib cell))`, then copy the instance's values into it.
  2. Bind the globals `cdfgData = cCDF` and `cdfgForm = cCDF`, and set the new `p~>value`s.
  3. For each changed parameter, `errset(evalstring(p~>callback) t)`.
  4. `cdfUpdateInstParam(inst)`, then restore the cell CDF defaults.
  5. `schCheck` + `dbSave`.

  This is the known approach for keeping derived PDK parameters (e.g. `fingers`, `wf`) consistent. Note that PDK callbacks may depend on a form or on GUI state.
- The planner places instances and connects them **by net labels only (no wires)**.

### Others
- **ALIGN-public** (BSD-3), **MAGICAL** (BSD-3), **OpenFASOC** (Apache-2.0) and **laygo** (BSD-2) contain **no SKILL files** (VERIFIED: tree scan for `.il/.ils/.scm`). ALIGN, MAGICAL and OpenFASOC are layout/netlist-driven. laygo layout goes through BAG.
- **cadence-drawing-skill** <https://github.com/Maxnopnop/cadence-drawing-skill> (no license, 2026): SKILL schematic drawing with `dbCreateInst`, `schCreateWire`, `schCreateWireLabel`, `dbReplaceProp`.
- **drawio PR #38** (Apache-2.0 fork) <https://github.com/courcirc8/drawio/pull/38>: "Virtuoso export: skillbridge and pure SKILL scripts", 2026. Small.
- No gdsfactory Virtuoso plugin was found (LIKELY none). kfactory is KLayout-only.
- Concept Engineering sells a SPICE-to-Virtuoso schematic flow through a generated SKILL script (commercial, <https://www.concept.de/video_SpiceNetlistToVirtuosoSchematicUsingSkillExport.html>).

### Synthesis for our loader (design inference)
- Create: `dbOpenCellViewByType(lib cell "schematic" "schematic" "w")` (or `"a"` to edit), then `dbCreateInst(cv master name xy orient)` with masters opened `"r"`.
- Params: set them through the CDF (`cdfGetInstCDF(inst)~>param~>value = "1u"`, using string values as BAG does). Offer an **opt-in** callback pass in the virtuoso-bridge-lite style, wrapped in `errset`, and default it off, because callbacks are PDK code.
- Connectivity: label-only connectivity (`schCreateWire` stub + `schCreateWireLabel`) or real wires. Pins via `schCreatePin` with `basic` pin masters.
- Finish: `schCheck(cv)` or `schCheckHier`, then `dbSave`, then `dbReopen(cv "r")` / `dbClose`.
- The dumper mirrors BAG's `parse_cad_sch` iteration, plus `inst~>xy`, `~>orient`, CDF values and the label/wire geometry. All names go through **our escaper**; BAG has the escaping bug noted above.

---

## Sources (primary)
- SKILL Language User Guide 06.70: <https://bpb-us-e1.wpmucdn.com/sites.gatech.edu/dist/0/367/files/2016/03/Intro_to_skill_prog.pdf>
- Float precision: <https://community.cadence.com/cadence_technology_forums/f/custom-ic-skill/41466/displaying-all-the-decimal-digits-of-a-floating-point-number>
- lineread returns t: <https://groups.google.com/g/comp.cad.cadence/c/ZaaVTtW56_0>
- skillbridge: <https://github.com/unihd-cag/skillbridge>, <https://unihd-cag.github.io/skillbridge/usage/server.html>, <https://unihd-cag.github.io/skillbridge/reference/protocol.html>
- BAG2: <https://github.com/ucb-art/BAG_framework/blob/master/run_scripts/start_bag.il>; BAG3: <https://github.com/bluecheetah/bag>; cbag: <https://github.com/bluecheetah/cbag>
- virtuoso-bridge-lite: <https://github.com/Arcadia-1/virtuoso-bridge-lite>
- zed-skill: <https://github.com/deanyou/zed-skill>; Pill: <https://github.com/jonpry/Pill>; Skillup: <https://github.com/skillup-core/skillup>; SKILL#: <https://github.com/Da-Buche/skill-sharp>
- OpenAccess: <https://en.wikipedia.org/wiki/OpenAccess>, <https://si2.org/openaccess-coalition-releases-new-version-of-oascript/>; oaFileParser: <https://github.com/EDDRSoftware/oaFileParser>; Xic OA: <https://www.wrcad.com/manual/xicmanual/node62.html>
- EDIF: <https://www.vrg.utoronto.ca/vrg/DesignManual/CDS_SYN_2006.pdf>, <https://silvaco.com/wp-content/uploads/content/appNotes/analog/1-012_EDIF200.pdf>
- ADS: <https://docs.keysight.com/display/engdocads/ADS+2017+Release+Notes>; S-Edit: <https://www.ednc.com/wp/wp-content/uploads/2017/03/s-edit-ds.pdf>
