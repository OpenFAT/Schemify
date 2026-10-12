# Upstream changes for extern/cktimg

`extern/cktimg` (cktImg, MIT, the user's library) is never edited in this repository. What
Schemify needs from it goes upstream as pull requests on
[OmarSiwy/cktImg](https://github.com/OmarSiwy/cktImg), stacked in merge order:

| PR | What | Why |
|---|---|---|
| [#2](https://github.com/OmarSiwy/cktImg/pull/2) | `zig build lib` installs only `libcktimg.a` and `cktimg.h`; on a freestanding target `zig build` skips the tools and the archive bundles compiler-rt; `-Dpic=true`; CI on Zig 0.17 (with 0.17's `zig fmt` and a fix in the xschem tool). | `zig build -Dtarget=wasm32-freestanding` failed compiling the tools (they need an OS) and there was no step for the library alone, so `plugins/cktimg/build.rs` called `zig build-lib` with the library's module graph copied out of `build.zig`. Now it calls `zig build lib`. |
| [#3](https://github.com/OmarSiwy/cktImg/pull/3) | Build option `-Dzon_config=false`: the C ABI refuses settings text and `.zon` symbol files instead of linking Zig's ZON parser. | ~110 KB of the 630 KB wasm guest was `std.zig` (Ast, ZonGen), reached only from `Config.parse`; the plugin never passes settings (measured: 478 KB without). |
| [#4](https://github.com/OmarSiwy/cktImg/pull/4) | L6 batches its wire separation requests past `batch_after`, as it batches clashes. | Each request cost a whole layout round; irregular cells lay out 2–4× faster, see [cktimg-performance.md](cktimg-performance.md). |
| [#5](https://github.com/OmarSiwy/cktImg/pull/5) | Separations batched past 256 rounds only, at most 16 a round. | #4 also batched them in regular drawings of more than 256 nodes, which over-separated them (AnalogIOC write_dac 3.4 s → 12 s). |

#2–#4 are merged. The submodule points at #5's head (on top of them) until it merges;
then it moves to the merged `main` commit.

Two smaller notes, no PR:

- `cktimg.h` says `cktimg_parse_place` rejects "an X card with no .subckt"; the parser
  accepts it (a PDK primitive by name, else a box with ports `p1..pN`, as the README says).
- A master name containing `nfet`/`pfet`/`res`/`cap` as a `_`-word is read as that device
  even when the caller registered a class for it; the plugin defines a `.subckt` stub for
  every block so its own class wins.
