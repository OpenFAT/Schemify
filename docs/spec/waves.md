# Waveform viewer

Implements the viewer half of D23 (the calculator is [expr.md](expr.md) §5, the reader is `crates/wave/src/raw.rs`). Behaviour follows xschem's graphs as listed in [xschem-feature-inventory.md](../research/xschem-feature-inventory.md) §2.2, §3.4 and §4B.3, written from that inventory and from running xschem, never from its source (D1). Code: `crates/wave` (tables, `Waves::apply`, the only writer), `crates/ui/src/waves.rs` + `crates/ui/waves-ui.records` (window, input, dialog), `crates/render/src/waves.rs` (drawing), `crates/app/src/waves.rs` (the editor's dock).

## 1. Tables

| Table | Rows | Columns |
|---|---|---|
| datasets | ≤ 256 | one `RawPlot` per analysis (consecutive `.step` blocks merged, steps kept) |
| traces | ≤ 64 | source text, program, dataset, pane, RGBA8 colour, stroke, magnitude flag, bus width, y column, own axis |
| panes | ≤ 16 | y axis, own x axis, properties (§3), two horizontal cursors |
| view | 1 | the shared x axis, vertical cursors a/b |

Every change is one `Request` applied by `Waves::apply`; a refused request changes nothing. Positions in requests are fractions of the visible span (0 = low end, 1 = high end) except where a request names data units.

## 2. x axes

- A pane is **locked** (default) or **unlocked**. Locked panes show the shared x axis: zoom, pan, box zoom and log x on any of them move all of them. An unlocked pane has its own x axis, moved only by gestures over it (xschem `unlocked`). A pane in histogram mode always uses its own x axis (values, §5).
- A trace on its **own axis** (`fft` bins, a per-step result of a dataset with ≥ 2 steps; expr §5) never shares the time axis: added to a locked pane, it unlocks that pane when the pane is empty, otherwise it goes to a new unlocked pane appended below (`too many panes` at 16). Moving it into a locked pane, or locking a pane that holds one, is refused (`own axis`). So a locked pane shows only traces on their dataset's scale.
- Full zoom (`f`, `Autoscale`) fits the shared axis to the traces of locked panes (the first dataset's scale when there are none), every unlocked pane to its own traces, then every pane's y. A per-step result of a one-step dataset is still a measurement (one number, shown as text).

## 3. Pane properties

Set together by one `SetPane` request (the graph properties dialog, §8); defaults in brackets.

| Property | Values | Meaning |
|---|---|---|
| mode | `analog` [default], `digital`, `histogram` | how traces are drawn (§4, §5) |
| threshold | finite number [0.5] | logic 1 when `y ≥ threshold` (digital mode, bus bits) |
| radix | `hex` [default], `dec` | bus values and digital readouts |
| bins | 2..=1024 [32] | histogram bins |
| x | `locked` [default], `unlocked` | §2 |
| step | empty [all] or a step index | sweep step shown (xschem `dataset`); a trace without that step draws nothing |

## 4. Digital mode and buses

- Each trace of a digital pane gets one lane, stacked in trace order over the plot height; logic 1 is drawn at 80% of the lane height, 0 at 20%. A single-signal trace is thresholded per sample: value `y ≥ threshold` (NaN reads 0).
- A **bus** trace is written `{e1, e2, …, en}` (1 ≤ n ≤ 32, top-level commas): each `ei` a calculator expression giving a real column on the dataset's axis. Its value per sample is `Σ bit_i · 2^(n−i)`, `bit_i = ei ≥ threshold` of the pane it is in, so `e1` is the MSB (xschem `BUS,b3,b2,b1,b0`). Changing the pane's threshold, moving the bus to another pane, or reloading the data re-evaluates it. In analog mode a bus draws as its integer value.
- A value holds from its sample to the next (sample and hold); nothing is drawn before the first or after the last sample. Drawing works per pixel column: a column whose samples (and the value held into it) all agree shows that value; any disagreement shows the column as busy. Runs of equal columns merge.
- Bus values print in the pane's radix: `hex` as `0x` + lower-case digits, `dec` as decimal. Cursor readouts in a digital pane print logic values (0/1) or bus values.

## 5. Histogram mode

Each trace of a histogram pane shows how its samples (in the steps the pane shows) are distributed over the pane's own x axis: `bins` equal bins between the axis ends, in transformed units on a log axis; samples outside the axis, non-finite ones (and ≤ 0 on a log axis) are not counted. A sample on a bin edge counts in the upper bin, the high end in the last bin. The y axis is the count; full zoom fits x to the value extent of the pane's traces and y to `0 .. 1.05 × the largest count` (from 0.5 on a log y axis). Samples are counted, not weighted by their x spacing.

## 6. Cursors and readouts

- Vertical cursors a and b are positions on the shared x axis, drawn in locked panes (`a`, `b` toggle them at the pointer, `s` swaps, a drag within 6 px moves one). Horizontal cursors A and B (`Shift+A`, `Shift+B`) are y positions of one pane, drawn in analog and histogram panes; the readout shows A, B and B − A for the pane under the pointer.
- The readout lists, per trace in a locked pane, its value at a and b and their difference; a trace with several shown steps lists one row per step (`src [k]`). Values interpolate linearly inside a step between neighbouring samples; a bus holds its last sample. Legends show the value at a for the first shown step that covers it.
- **Measurement tooltip** (`m`): while on, the pointer over a plot shows `y=` and `x=` of the pointer position in engineering units.
- **Dataset tracking** (`t`, analog panes): picks the step of the pane's traces whose value at the pointer x is closest to the pointer y (in transformed units on a log axis) and shows only that step; `t` again shows every step.
- **Typed positions**: the properties dialog has fields for a, b, A and B (empty hides the cursor).

## 7. Colours

New traces take the first unused of 8 trace colours. The properties dialog's swatches (16: the 8 trace colours and 8 more) recolour the selected trace (`SetColor`, any RGBA8 value).

## 8. Graph properties dialog

Double-click in a plot (or a right click within 6 px of a cursor) opens it for that pane (xschem); `Escape` or Cancel closes it without changes. It holds the pane's properties (mode and radix as cycling buttons, x lock as a checkbox), x min/max, y min/max, the four cursor positions and the colour swatches. Numbers accept SI suffixes (`1.5u`, `2meg`) and are shown in canonical number text (expr §7), so OK on an unchanged dialog changes nothing (the round trip). OK applies everything as requests (`SetPane`, `SetX`, `SetY`, `Cursor`, `HCursor`); a field that does not parse keeps the dialog open and names the field in the status line.

## 9. New results

The editor's dock keeps its traces and panes when it is pointed at new data: picking another job in the results table, or a run finishing while the dock shows that bench's results, re-reads the job's raw file and re-evaluates every trace over it. A trace that no longer evaluates (a signal the new run did not save) is dropped and named in the status line.
