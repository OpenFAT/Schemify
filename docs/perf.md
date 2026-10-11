# Performance

## Binary size

D39 (amended 2026-10-11): the headless core `schemify` ≤ 2 MB, the editor `schemify-gui` ≤
10 MB, both as `flake.nix` ships them: stripped, fat LTO, one codegen unit, `panic = "abort"`,
no debug info, and on Linux `-C force-unwind-tables=no -C link-arg=-Wl,-z,pack-relative-relocs`.
Check: `tools/size-check.sh` (builds both for the host with that env, exits 1 over budget).

Measured 2026-10-11 on x86_64-linux, rustc 1.95, bytes of the shipped (stripped) binary.
"Before" is `main` at 5da0192: one `schemify` binary with the editor built in; its core is the
headless build (`--no-default-features`).

### Totals

| | Core `schemify` | Editor `schemify-gui` |
|---|---:|---:|
| Before (5da0192) | 3,330,976 | 22,869,880 |
| After | 2,565,216 | 16,010,696 |
| Change | −766 K (−23 %) | −6,859 K (−30 %) |
| Budget (D39) | 2,000,000 — **over by 565 K** | 10,000,000 — **over by 6.0 M** |

### Per cut

Measured by building with and without the cut where marked "built"; otherwise attributed from
an unstripped build's symbol sizes (`nm --size-sort`, symbols grouped by the crate they name),
marked ≈. Attributions overlap a little (LTO inlines across crates), so rows do not add up
exactly to the totals.

| Cut | Core | Editor | How |
|---|---:|---:|---|
| Split (D39): the core links no GPU, window, text-shaping, accessibility, LaTeX or plugin-host crate | (the editor half is gone) | — | `crates/cli/tests/deps.rs` checks the resolved graph for every target |
| `rustfft` → own radix-2 + Bluestein FFT (`expr/src/fft.rs`, libm twiddles) | ≈ −661 K | ≈ −661 K | rustfft, transpose, strength_reduce, primal_check symbols |
| Full Noto Sans → `NotoSans-UI.ttf` subset; KaTeX Main (already embedded) draws → ≤ ≥ | 0 | −1,428 K | file sizes 1,581,884 → 153,536 |
| System-font fallback for glyphs outside the subset (fontique `system`, dlopen'ed fontconfig) | 0 | ≈ +65 K | fontique, fontconfig-sys symbols |
| winit without `wayland-csd-adwaita` | 0 | ≈ −309 K | tiny-skia, ttf-parser, ab_glyph, sctk-adwaita symbols |
| Lua 5.4 user config + package manager (phase 2: mlua, vendored Lua, `config`, `pack`) | ≈ +350 K | ≈ +330 K | core built before/after; probe: mlua +434 K at opt 3 |
| `[profile.release.package]`: cold crates at `opt-level = "z"`, GPU setup at `"s"` | −352 K | −2,945 K | built with and without the table |
| Linux: no unwind tables, packed relative relocations (DT_RELR) | −131 K | −1,364 K | built with and without the flags |

Not cut, measured: the GLES backend (glow, khronos-egl, wgpu-hal `gles`, naga GLSL out) is
≈ 176 K; it is the only GPU path on Linux machines without Vulkan, so it stays. `cargo tree -d`
finds no duplicate in the core; the editor has small ones from third-party version skew
(rustix 0.38/1.1 via winit, hashbrown 0.16/0.17, miniz_oxide 0.8/0.9, phf 0.11/0.14,
thiserror 1/2, bit-set/bit-vec), each tens of K, fixed only by upgrading those dependents.

Trade-offs taken: on GNOME (no server-side decorations) the window frame is smithay's plain
fallback (move, resize, close, maximize, minimize; no title text, no Adwaita styling); KDE,
wlroots compositors and X11 decorate as before. Panics in shipped Linux binaries print their
message but no backtrace (the binaries are stripped, so backtraces had no symbols anyway).
Shipped Linux binaries need glibc ≥ 2.36 (nix's glibc).

### Frame time (the `opt-level` table)

`cargo test --release -p schemify-app --test perf -- --ignored --nocapture`, three runs each,
same tree with and without `[profile.release.package]` (release profile, `debug = 0`; the
machine was shared with other builds, so ±7 % is noise):

| Metric | Without table (median of 3) | With table (median of 3) |
|---|---:|---:|
| 100k objects: open + first build | 191 ms | 186 ms |
| pan/zoom frame (update + scene encode) | 10.84 ms | 10.89 ms |
| edit round trip | 113 ms | 115 ms |
| update | 0.28 ms | 0.29 ms |
| var edit, 1000 dependents | 1.90 ms | 1.92 ms |
| var edit, gmid-backed | 2.03 ms | 2.05 ms |

No regression beyond noise: the table touches no crate on these paths.

### Where the editor's bytes are now (≈, symbol sizes, K)

| Group | K |
|---|---:|
| Our crates (with the generics they instantiate) | 2,825 |
| GPU: wgpu, naga, vello | 2,266 |
| Text: parley, harfrust, skrifa, read-fonts, fontique, ICU | 1,618 |
| Accessibility: accesskit, zbus/zvariant, async runtime | 1,141 |
| LaTeX: ratex + regex, fancy-regex, regex-lite, aho-corasick | 927 |
| Windowing: winit, wayland, x11 | 913 |
| Plugins: wasmi, wasmparser | 803 |
| std (generic + runtime) | 854 |
| Lua (mlua + C) | ≈ 250 |
| Other third-party | ≈ 400 |

Plus ≈ 3.0 M of `.rodata` (fonts 0.65 M, vello's WGSL shader sources with comments 0.45 M,
ratex's mhchem JSON 0.18 M, string literals), 0.6 M of std's own unwind tables and 0.6 M of
`.data.rel.ro`.

### What is left over budget, and why

Under the constraints of this change (AccessKit kept, hot crates at `opt-level = 3`, no forks
or patches of dependencies), neither budget is reachable. The remaining levers, largest first,
with owners:

| Lever | Est. saving | Owner / cost |
|---|---:|---|
| Hot crates at `opt-level = "s"` (ours, text, vello, wasmi) | editor ≈ −2 M, core ≈ −0.4 M (all-`s` core measured 2,401,304 before the flags) | frame time to be measured; user decision |
| One shared sort instantiation: `elab` alone instantiates 312 K of `slice::sort` code (113 instances), `format` 77 K, `netlist` 55 K, `run` 43 K, `doc` 25 K | both ≈ −0.4 M | elab, format, netlist, run, doc owners |
| ratex: put mhchem (`\ce`, three regex engines + serde_json + 175 K of JSON) behind a feature | editor ≈ −0.8 M | upstream ratex-parser |
| vello: ship minified WGSL | editor ≈ −0.2 M | upstream vello_shaders |
| Drop the GLES backend | editor −0.18 M | loses non-Vulkan Linux GPUs |

Even all of them leave the editor near 12 M: the 10 M budget needs a feature-level decision
(e.g. the D-Bus accessibility client or the LaTeX widget as plugins) or an amended D39.
