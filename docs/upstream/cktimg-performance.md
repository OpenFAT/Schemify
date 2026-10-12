# cktImg: layout time, memory and wasm fuel

Measured at `extern/cktimg` b6631d8 (Zig 0.17.0, x86-64, ReleaseFast `-mcpu=baseline`;
wasm32-freestanding ReleaseSmall under wasmi), through the C ABI from the cktimg plugin.
Nothing here needs a patch to work correctly; it is what limits the plugin.

## Time grows steeply with a cell's size

Native `cktimg_parse_place` on random MOSFET meshes (each `M<i>` on three random nets of
`n/3`, bulk on ground), with cktImg's own `tests/symbols.zon`:

| devices | time | peak RSS |
|---:|---:|---:|
| 25 | 0.03 s | 9 MB |
| 50 | 0.02 s | 6 MB |
| 100 | 3.8 s | 47 MB |
| 150 | 19.5 s | 142 MB |
| 200 | 80 s | 76 MB |
| 400 | > 100 s (stopped) | 123 MB at the stop |

Memory stays modest (no input reached 150 MB); time goes roughly with the fourth power of the
size. Regular circuits are far cheaper (the 117-device `bus_sram_cell_array` fixture: 0.9 s,
26 MB; every one of the 301 xschem golden netlists converts in under 1 s and 20 MB), but some
small ones are not: the 43-device `bus_4bit_flash_adc` (a 16-resistor ladder feeding 15
comparators) takes 94 ms.

Reproducer (a SPICE deck; `zig build gallery -- mesh.cir out.svg` shows it):

```
* mesh, 100 devices: M<i> on three nets drawn from x0..x32, bulk on 0
Vdd vdd 0 1.8
M0 x12 x3 x27 0 nch
M1 x5 x30 x9 0 nch
...
.model nch nmos
```

(The plugin's measurement script draws the nets with Python's `random.Random(n)`; any
random draw shows the same growth.)

## Where the time goes

callgrind on the flash ADC deck (1.10·10^9 instructions in `cktimg_parse_place`):

| function | inclusive |
|---|---:|
| `layout.place` | 99 % |
| `layout.assignLevels` | 36 % |
| `layout.buildRoutes` | 11 % |
| `layout.setXY` | 11 % |
| `hash_map([2]i32 → u32).getOrPut` (the L6 cell map) | 11 % |
| `mem.sort` (block sort) | 8 % |

All of it is the L5/L6 loop in `layout.place`: each round recomputes both axes' levels from
scratch (`assignLevels`), rebuilds the cell map, and, once no two nodes share a cell, runs
`setXY` + `buildRoutes` + `separation` to add one ordering constraint. Clashes are batched
after `batch_after` rounds; separation requests are not, so a drawing with many of them runs
`setXY` + `buildRoutes` once per request. Batching the separation requests the way clashes
are batched, or updating levels incrementally after `addOrdered`, are the two places to look.

## Separation requests batched (cktImg PR #4)

Past `batch_after` every separation request of a round is ordered at once (one per class,
each checked against the constraints before it). `gallery deck.cir out.svg`, ReleaseFast,
GitHub `ubuntu-24.04`, the meshes above:

| devices | before | batched |
|---:|---:|---:|
| 50 | 0.166 s | 0.141 s |
| 75 | 0.980 s | 0.463 s |
| 100 | 3.19 s | 1.19 s |
| 125 | 9.32 s | 3.45 s |
| 150 | 18.0 s | 4.49 s |

As merged (#4) the batch also applied from round one to drawings of more than 256 nodes, and
that over-separates regular ones (AnalogIOC write_dac 16152 → 23153 rounds, 3.4 s → 12 s).
#5 batches separations only past 256 rounds, at most 16 a round: write_dac 3.7 / 4.4 s
(before #4 / now), rstring_ladder 4.7 / 3.3 s, 150-MOSFET mesh 19 / 9.2 s. Incremental levels
remain the next step for regular cells.

## wasm fuel

Under wasmi one millisecond of native layout costs about 10^8 fuel (sr_latch: 1.08 ms native,
1.1·10^8 fuel; flash ADC: 94 ms, 1.3·10^10). The Schemify plugin host gives an event 10^8 by
default, so circuits of about ten devices lay out inside one event; 11 of the plugin's 80
fixtures need more (up to > 4·10^10 for `bus_sram_cell_array`) and abort cleanly. ReleaseFast
instead of ReleaseSmall saves ~30 % fuel (sr_latch 7.2·10^7) for a 1.0 MB module.

## Size

The wasm guest was ~630 KB: cktImg ~290 KB of code, Zig's ZON parser ~110 KB (linked for
`Config.parse` though the plugin passes no settings), the plugin's Rust ~180 KB.
`-Dzon_config=false` (cktImg PR #3) removes the parser (measured: 478 KB).
