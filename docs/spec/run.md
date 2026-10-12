# Runs v1 (normative)

Implements D7, D13, D15–D18, D38. Inputs: a bench (`.shbench`, [format.md](format.md) ranks 4 and 6), a selection of dimensions, the project, an environment table. Output: the `jobs` and `results` tables and one results file per run. Implementation: `crates/run`. Everything except the native runner (§5) is pure and builds for `wasm32`.

## 1. Plan

The bench tables are read once per Run into a plan. Named rank-6 tables are ordered bytewise by name; vars keep document order. A record that cannot run as written refuses the whole Run, naming the record:

- `sweep`: `var` is required; exactly one of `values` / `lin` / `log`; `lin`/`log` bounds are constant expressions (no identifiers), the count is an integer in `1..=65536`, `log` bounds are nonzero with one sign; every `values` entry compiles.
- `montecarlo`: `samples` is required, an integer in `1..=2^20`; `seed` is a u64, default 0.
- var `dist` (§3): spread compiles, spread does not name the var itself (the shape is the format's, `E029`).
- `optimize` (§6): at least one `goal` naming an existing output, shaped `<output> min|max [weight <w>]` or `<output> target <t> [weight <w>]` (`t` constant; `w` a constant > 0, default 1), and one `vary` naming a bench var with constant `min` and `max`, `min ≤ max`; every `config` and `corner` line names a config or corner of the bench; `budget` in `1..=65536` (default 100); `seed` a u64 (default 1).
- `output`: compiles in calculator mode. `spec`: names an output; `min`/`max` compile in scalar mode.

## 2. Job expansion

A selection names configs, corners, sweeps (in nesting order), at most one Monte Carlo record, and CLI overrides `(var, expr)`. Empty configs or corners mean the bench default (its header attribute, or none). Naming what the bench lacks refuses the Run; so does more than one corner without the `corners` feature, any sweep without `sweep`, Monte Carlo without `mc`.

Jobs are the product, in this order (outermost first): config, corner, sweep point (an odometer over the selected sweeps, first sweep outermost, last fastest), Monte Carlo sample. Count = product of the dimension sizes, at most 2^20.

Each job is (config, corner, sample index or none, assignments). Assignments, in application order:
1. one per selected sweep, in selection order: the point's value text;
2. with Monte Carlo, one per var carrying `dist`, in var name order (§3);
3. the CLI overrides, in order.

Elaboration applies the corner's `set` lines first, then these assignments in order, later wins ([netlist.md](netlist.md) §6, §9.11).

Sweep points: `values` entries verbatim. `lin <a> <b> <n>`: `a + (b − a)·k/(n − 1)` for `k < n − 1`, exactly `b` at `k = n − 1`, `a` alone for `n = 1`. `log <a> <b> <n>`: `a·(b/a)^(k/(n − 1))` evaluated by the expression language (bit-identical on every target), exactly `b` at the end. Both are written as canonical number text ([expr.md](expr.md) §7).

The job key line, used by the design hash and as the job's label: `<config|-> <corner|-> <sample|-> var=value …`.

## 3. Monte Carlo

A var varies when it carries `dist gauss|uniform <spread> [rel]` (format.md rank 4). Each draw is a pure function of (Monte Carlo seed, sample index, var name):

- stream: splitmix64 seeded with `seed ^ rotl(FNV-1a-64(var), 29)`, one output XORed with `sample · 0xd1b54a32d192ed03` is the state of a second splitmix64 whose next two outputs `x` give `u1 = ((x >> 11) + 1) / 2^53 ∈ (0, 1]` and `u2 = (x >> 11) / 2^53 ∈ [0, 1)`;
- `gauss`: `z = sqrt(−2·ln(u1))·cos(2π·u2)` (expression language, `libm`); `uniform`: `z = 2·u2 − 1`.

So sample `k` of var `R` is the same in every corner, config and sweep point of a run (common random numbers), and adding a var leaves the others' draws unchanged.

**Simulator seed.** Each sample also fixes the simulator's own random seed, `1 + (x >> 34)` ∈ `[1, 2^30]` where `x` is the first output of the second splitmix64 of the stream above for the empty var name `""` (no var has it). The job keeps it (`Jobs::seed`, results file `seed`), and its ngspice netlist ends `.option seed=<n>` before `.end` (ngspice applies it before evaluating `.param`), so the PDK's own statistics (`agauss`/`gauss` in a Monte Carlo `lib` section) draw the same values for the same seed and sample on every run, and sample `k` sees the same PDK draws in every corner, config and sweep point. (Xyce takes its seed on the command line; not set yet.)

The varied expression (base) is the latest assignment of the var in this job, else the selected corner's last `set` of it, else the var's own expression. The assignment text is `(base)+(spread)*(z)`, or `(base)*(1+(spread)*(z))` with `rel`, `z` in canonical number text: still an expression, evaluated by elab in bench scope.

PDK statistical models are selected with a corner whose `lib` section is the PDK's Monte Carlo section; those lines pass through to the netlist unchanged.

**Yield:** per spec, over the jobs with a sample index that finished and were not cancelled; a failed job or a missing value counts as a failure. Reported as pass count, total and the 95% Wilson score interval (`z = 1.959963984540054`; `[0, 1]` when the total is 0).

## 4. Design hash and results

**Gate and hash.** Before any job runs, the bench is elaborated once per distinct config of the job list (the default config when the list is empty), at the default corner without assignments. A cell elaboration needs open, or any `E` violation, blocks the Run. The design hash is sha256 over length-prefixed parts (8-byte little-endian length, then the bytes): `schemify design 1`; the lockfile text; each project `set` key and value; each environment key and value; the sha256 of the bench's canonical bytes; for each reached cell, ordered by display name (`lib:dir/stem`), its name and the sha256 of its document's canonical bytes (the open document, else the indexed interface); each job's key line. An edit that can change any job's netlist changes the hash; results tagged with another hash are **stale**.

**Results** are keyed by (design hash, job index). Per job: the numeric value of every bench var after elaboration, one value per output, one verdict per spec, and error rows `(job, output or job-level, message)`.

- An output is a calculator expression evaluated over the job's raw file after dataset grouping, on the **last** dataset that holds every signal it names (else the last dataset, which reports the missing signal). Identifiers are the job's bench var values. The result must be one number: a scalar, or a column of one point (a reduction of a single-step dataset). Anything else is an error row for that output; the job is still done.
- A spec passes when `min ≤ value ≤ max` (absent bounds are open); bounds are scalar expressions over the job's var values. No value, or a bound that fails, is *unknown*.

Job states: `pending`, `queued`, `running`, `done` (simulator exited 0 and its raw file parsed), `failed` (gate, netlist, compile, simulator, raw file), `cancelled`.

**Results file** (`<run dir>/results`): line records in the format.md §2 lexical layer (not a format.md file kind: `crates/run` reads and writes it), header `schemify 1 results` with attribute `design <64 hex>`; then `output <name>` records (plan order), `spec <output>` records, `var <name>` records; then one `job <index> <state>` record per job in index order, with attributes `config`, `corner`, `sample`, `seed` (each only when set), `set <var> <value>` per assignment, `var <name> <number>` and `value <output> <number>` per present value (canonical number text), `pass <output> pass|fail` per decided spec, `error <output|-> <message>` per error row sorted by output. Written canonically, so `jobs=1` and `jobs=N` give identical bytes.

## 5. Native runner (not on the web)

- Directories: run `<project>/.schemify/runs/<first 16 hex digits of the design hash>/`, one sub-directory per job index holding `netlist.cir`, `out.raw`, `stdout.log`, `stderr.log` and `command.log` (the simulator command line the job ran, one line); compiled sources cached in `<project>/.schemify/build/`. `.schemify/` is git-ignored.
- Corner paths: a corner `lib`/`include` path that is relative (not starting with `/`, `\`, `~`, `$`, a quote or a drive letter `C:`) names a file beside the bench. The netlist keeps it as written (netlist spec §7.6.14: the output must not depend on the checkout); the runner writes it into the job's `netlist.cir` joined onto the bench's directory (the library's directory joined with the bench's), quoted when that holds whitespace.
- The simulator command line comes from the user config `sim ngspice` (`path`, `args`), never from a document (D7); default `ngspice -b -r {raw} {netlist}`. `{netlist}` and `{raw}` are replaced by the job's file names; the working directory is the job directory; stdin is empty.
- At most `workers` simulators at once (default: available cores, at most 1024), started in submission order. A simulator past the timeout (default 1 h) is killed and the job fails. Exit 0 with a raw file: results are recorded (§4). Non-zero exit, or no raw file: the job fails with the last stderr line. A program that cannot be found fails the job naming it.
- Sources: a leaf impl with lang `veriloga` or `verilog` whose source is a `file`, or its `body` (a `verilog` body; a `veriloga` body with an `endmodule` line, netlist.md §7.6.8), is compiled with `openvaf <src> -o <build>/<stem>.osdi` (Verilog-A) or `iverilog -o <build>/<stem>.vvp <src>` (Verilog), programs from the user config `sim openvaf` / `sim iverilog` `path` (OpenVAF-Reloaded installs `openvaf-r`: name it there); `stem` is the `file` stem, else the cell stem. A `body` is first written to `<build>/<stem>.va|.v` (only when its text changed). A source compiles only when its output is missing or older than it, then the output is copied into the job directory (the netlist names it relative, netlist.md §7.2). Compilers are child processes polled like simulators, never waited for: a job holds its worker slot (state `queued`, progress `compiling sources`) until its sources are compiled; one compiler per output serves every job that needs it; compiler output goes to `<build>/<out>.log`. A missing compiler, a non-zero exit, no output or the timeout fails every job of the run that needs that output, naming the program, the source and the compiler's last line.
- Progress: the last complete stdout line of each running job.

On the web, runner plugins (D16) receive the job's netlist and return its raw bytes, which go through the same result recording; a plugin's failure fails the job.

## 6. Optimizer

An `optimize` record runs as an ask → jobs → tell loop, one candidate at a time. A candidate is one job per (config, corner) of the record, configs outer and corners inner in file order (the bench default where it names none: one job at the default config and corner), no sample index, each assigning every `vary` var its canonical number text.

- Params: the `vary` vars with their constant `min`/`max`; start at the var's value when constant, else the midpoint, clamped into the bounds. Every candidate is clamped into the bounds (D13).
- Score (lower is better): sum over goals of `weight ×` the goal's error on the candidate's worst job for it, the error being `value` (`min`), `−value` (`max`), `|value − target|` (`target`). A failed job or a missing goal output in any of the candidate's jobs scores `f64::MAX`.
- `algo nelder-mead` (default): downhill simplex from the start point plus one vertex per param stepped by 5% of its range; reflection 1, expansion 2, contraction 0.5, shrink 0.5. `algo random`: the start point, then uniform draws from xorshift64* seeded by `seed`.
- At most `budget` evaluations. The same bench, seed and simulator results give the same sequence of jobs.

## 7. Plugin run requests

A plugin's `run <bench> <config> <corner>` record with `vary <var> <value>` lines ([plugin-abi.md](plugin-abi.md) §7) is one job: config and corner as named, no sample index, the vary lines as assignments in order. It is prepared and dispatched like any other job; `RunDone { job }` follows when the job is done or failed.

In the editor a request becomes a job of its bench's run, in flight or not (§7a); a bench with no run, or whose shown results are stale, gets a fresh run (gated and hashed with no jobs) once its run in flight is idle. Runs of other benches go on meanwhile. At most 256 requests wait; more are dropped with a status note. The `Results` snapshot holds the rows of the run that last finished a plugin's job (else the shown run); a row's `pass` is the verdict of its output's spec (a spec names one output): 1 pass, 2 fail, 0 unknown or no spec.

## 7a. The run in the editor (non-blocking)

`s` (Simulate) on a `.shbench` selects every corner and sweep and the first Monte Carlo record of the plan; on any other file it refuses (D11b) and points to *New bench from this component*, which writes `<stem>_tb.shbench` beside the cell: one instance of it at the origin, a label on every pin naming the net after the pin, and a `code op spice` block with `.op`. *Optimize* runs the bench's first `optimize` record (or the one named).

Starting a run is the gate and the design hash only (§4); a refused gate lists its blocking violations in the checks panel. Each frame then prepares at most 8 jobs over all runs (an equal share each), polls every runner without waiting, and steps each optimizer (one candidate's jobs in flight). While a run is in flight the editor asks the window for a tick every 25 ms; idle, it asks for none (D39). *Cancel simulation* kills every running simulator, cancels queued and unprepared jobs (`cancelled`) and drops the waiting requests.

**Several runs.** The editor keeps one run per bench, at most 8 (in flight, finished or loaded; a ninth bench drops the oldest idle run that is not shown, whose results stay on disk), and runs of different benches proceed at once. Requests wait in one queue served in order, a bench at a time: a Simulate or Optimize of a bench whose run is in flight starts when that run is idle (a queue per bench). The results panel, plots, OP annotation and stale check follow the **shown** run: the one last started, or the active bench tab's (its run in memory, else its newest saved run).

**Results table.** Columns: job, key line, state, one per output, one per spec. Header buttons sort by a column (`results.sort`; values ascending with no value last, verdicts fail < unknown < pass, ties by job index; the same column again reverses). The filter field (`results.filter`, Enter) keeps the rows whose text — job index, key line, state word and `pass`/`fail` verdicts, lowercase — contains every filter word. Only the rows near the list's scroll position are laid out (64 at a time; spacers of 19 px per row stand for the others), so a table of any size scrolls. Above the jobs: with Monte Carlo samples a **yield** row (per output the sample mean, per spec the yield and its 95% Wilson interval, §3), and for an optimizer the **best** row (its best candidate's job, the varied values; its button picks the job).

**Probe lines.** While *Op Annotate* is on, Simulate and Optimize add before each ngspice job netlist's `.end`: `.save all` (every node voltage and branch current stays saved), `.options savecurrents` (every device's terminal currents, `@r1[i]`, `@m.x1.mm2[id]`), and per MOSFET of the bench's netlist (a leaf whose emitted name starts with `m`, at most 4096) `.save @<dev>[id] [gm] [gds] [vgs] [vds] [vdsat]` with ngspice's device name (lowercase, `<letter>.<path>` inside a subcircuit). The CLI adds none.

When the run goes idle the results file is written (§4) and, beside it, `bench` — one line, the bench's display name (`lib:dir/stem`, a project cell without `lib:`) — so the editor finds a bench's runs. Opening a bench loads its newest run (by the results file's modification time); the results panel lists up to 8 earlier ones. Results are **stale** when the design hash of the bench over the run's original job list (optimizer candidates and plugin jobs excluded) differs from the run's; the editor rechecks after each edit while results are shown.

The optimizer's best evaluation (lowest score, first on ties) is offered as a **staged edit** on the bench (edits spec §1): `set_field var <name> 1 <canonical value>` per `vary` var (plus removing a stale `cached`), shown on the docs page beside each var with Accept / Reject (`edit.accept_staged`, `edit.reject_staged`).

**Reviewing staged edits.** One stage of the active document is under review at a time (the first, until stepped): its visual diff is drawn on the canvas — objects it changes outlined, objects it deletes outlined and crossed out in the error colour, records it inserts drawn where they will be (wires and graphics as drawn, other records as a box at their anchor with their name) — and the docs page shows it (new var values beside the vars). With several stages the banner reads `(k/N) <label>` and `<` / `>` step through them (`edit.show_staged.prev|next|<k>`). Accept / Reject act on the stage under review, or on stage `<k>` (0-based) as `edit.accept_staged.<k>` / `edit.reject_staged.<k>`.

**Netlist text tab.** `n` shows the netlist in a read-only text tab (at most 2^14 lines, 256 KiB; the file has all of it). Its find field (`text.find`, Enter) replaces the text with the numbered lines of the whole file that contain the query, ASCII case-insensitively, and says how many there are; an empty query shows the text again.

The selected job (job 0 after a run; a results row picks another) feeds the waves dock, the docs page's plot slots ([docs-page.md](docs-page.md) §5) and the OP back-annotation: the dataset whose plot name contains "operating point" (ngspice), else a one-point DC dataset (Xyce writes `.op` that way), gives one mark per node voltage — `v(<net>)` or a bare node name; names compare ASCII case-insensitively and Xyce's `:` hierarchy separator reads as `.` (target: the emitted net name, lowercase; text `<value>V`) — and per device quantity: `@<dev>[p]` for `p` in `id gm gds vds vgs vth vdsat i ic`, a branch current `<dev>#branch` (`i`), and Xyce lead currents `i|id|ig|is|ib|ic|ie(<dev>)` (target: the instance path through the name map; text `p=<value>`), as the `op` source of the annotation overlay, while the tab shows the bench's hierarchy and *Op Annotate* is on.

**Cross-probe** (D23): with the waves dock open, a click on a net adds `v(<net>)`; on an instance pin, the current into it under ngspice's name: a branch device's (`v`, `e`, `h`, `l`) branch current, else the terminal current `.options savecurrents` saves (`@<dev>[i<pin initial>]` for MOS/JFET `d g s b` and BJT `c b e s` pins, `id` for a diode, `i` otherwise). When the raw file lacks it (no probe lines), the pin's net voltage is added instead.

## 8. Amendments

1. format.md rank 4 `var` gains `dist gauss|uniform <spread> [rel]` (§3); rank 6 `optimize` gains `seed <n>`, `config <name>`, `corner <name>` and goal weights; all are known attributes (shape errors are `E029`); `montecarlo` and `optimize` defaults and `output` evaluation as stated here.
2. format.md user config `sim <name>`: `{netlist}`/`{raw}` placeholders; names `ngspice`, `openvaf`, `iverilog`.
3. Results live in `.schemify/runs/<design>/results` next to the job directories (D18 said `.schemify/results/`): one directory per design keeps a run's netlists, raw files and results together.
4. (M2 editor) The app writes a `bench` sidecar beside `results` (§7a) and runs the spine without blocking; `crates/run` gained `Jobs::cancel_pending` for jobs never handed to a runner.
5. (2026-10-11) Monte Carlo samples fix the simulator seed (§3); inline `body` sources compile and compilers never block a poll (§5); relative corner paths resolve against the bench (§5); the optimizer runs over configs and corners with goal weights (§6).
6. (2026-10-11, editor) Several runs at once with a queue per bench; the results table sorts, filters and scrolls any number of rows, with yield and best rows; the `Results` snapshot's `pass`; probe lines; Xyce OP naming; cross-probe of device currents (§7, §7a).

## Run log view (editor)

The results pane shows the selected job's run log below its table: the command line from `command.log` (`$ <simulator> <args>`), then the last 12 non-blank lines of `stdout.log` and of `stderr.log` (stderr in the failure style). The editor re-reads these bounded tails (at most 8 KiB per file) only when the runner is polled while a job runs or results change, when a job is selected and when a run starts: never in the frame path. A failing simulator's message is therefore visible without opening a file; "last job data/errors" still open the full logs in a text tab.

