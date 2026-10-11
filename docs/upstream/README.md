# Upstream changes for extern/cktimg

`extern/cktimg` (cktImg, MIT, the user's library) is never edited in this repository. What
Schemify needs from it is written here for the user to take upstream; each patch applies with
`git -C extern/cktimg apply ../../docs/upstream/<file>` at the pinned commit (b6631d8).

| File | What | Why |
|---|---|---|
| [cktimg-lib-step.patch](cktimg-lib-step.patch) | `zig build lib` installs only `libcktimg.a` and `cktimg.h`; on a freestanding target `zig build` skips the tools. | `zig build -Dtarget=wasm32-freestanding` fails today compiling the gallery and the JSON/xschem tools (they need an OS), and there is no step for the library alone, so `plugins/cktimg/build.rs` calls `zig build-lib` with the library's module graph instead. After the patch it can call `zig build lib`. Checked: builds `libcktimg.a` for wasm32-freestanding. |
| [cktimg-optional-zon.patch](cktimg-optional-zon.patch) | Build option `-Dzon_config=false`: the C ABI refuses settings text and `.zon` symbol files instead of linking Zig's ZON parser. | ~110 KB of the 630 KB wasm guest is `std.zig` (Ast, ZonGen) reached only from `Config.parse`; the plugin never passes settings. `build.rs` already writes `zon_config = false` into the guest's `build_options`, so the saving arrives with the patch (measured: 478 KB). |
| [cktimg-performance.md](cktimg-performance.md) | Measurements, no patch: layout time grows with roughly the fourth power of a cell's size on irregular circuits; one native millisecond is ~10^8 wasm fuel. | Bounds what the plugin can draw inside the default fuel budget; the profile points at the L6 rounds of `layout.place`. |

Two smaller notes, no patch:

- `cktimg.h` says `cktimg_parse_place` rejects "an X card with no .subckt"; the parser
  accepts it (a PDK primitive by name, else a box with ports `p1..pN`, as the README says).
- A master name containing `nfet`/`pfet`/`res`/`cap` as a `_`-word is read as that device
  even when the caller registered a class for it; the plugin defines a `.subckt` stub for
  every block so its own class wins.
