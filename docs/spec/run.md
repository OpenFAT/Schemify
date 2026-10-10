# Runs v1 (normative)

Implements D7, D13, D15–D18, D38. Inputs: a bench (`.shbench`, [format.md](format.md) ranks 4 and 6), a selection of dimensions, the project, an environment table. Output: the `jobs` and `results` tables and one results file per run. Implementation: `crates/run`. Everything except the native runner (§5) is pure and builds for `wasm32`.

## 1. Plan

The bench tables are read once per Run into a plan. Named rank-6 tables are ordered bytewise by name; vars keep document order. A record that cannot run as written refuses the whole Run, naming the record:

- `sweep`: `var` is required; exactly one of `values` / `lin` / `log`; `lin`/`log` bounds are constant expressions (no identifiers), the count is an integer in `1..=65536`, `log` bounds are nonzero with one sign; every `values` entry compiles.
- `montecarlo`: `samples` is required, an integer in `1..=2^20`; `seed` is a u64, default 0.
- var `dist` (§3): shape, spread compiles, spread does not name the var itself.
- `optimize` (§6): at least one `goal` naming an existing output (`target` needs a constant value) and one `vary` naming a bench var with constant `min` and `max`, `min ≤ max`; `budget` in `1..=65536` (default 100); `seed` a u64 (default 1).
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

The varied expression (base) is the latest assignment of the var in this job, else the selected corner's last `set` of it, else the var's own expression. The assignment text is `(base)+(spread)*(z)`, or `(base)*(1+(spread)*(z))` with `rel`, `z` in canonical number text: still an expression, evaluated by elab in bench scope.

PDK statistical models are selected with a corner whose `lib` section is the PDK's Monte Carlo section; those lines pass through to the netlist unchanged.

**Yield:** per spec, over the jobs with a sample index that finished and were not cancelled; a failed job or a missing value counts as a failure. Reported as pass count, total and the 95% Wilson score interval (`z = 1.959963984540054`; `[0, 1]` when the total is 0).

## 4. Design hash and results

**Gate and hash.** Before any job runs, the bench is elaborated once per distinct config of the job list (the default config when the list is empty), at the default corner without assignments. A cell elaboration needs open, or any `E` violation, blocks the Run. The design hash is sha256 over length-prefixed parts (8-byte little-endian length, then the bytes): `schemify design 1`; the lockfile text; each project `set` key and value; each environment key and value; the sha256 of the bench's canonical bytes; for each reached cell, ordered by display name (`lib:dir/stem`), its name and the sha256 of its document's canonical bytes (the open document, else the indexed interface); each job's key line. An edit that can change any job's netlist changes the hash; results tagged with another hash are **stale**.

**Results** are keyed by (design hash, job index). Per job: the numeric value of every bench var after elaboration, one value per output, one verdict per spec, and error rows `(job, output or job-level, message)`.

- An output is a calculator expression evaluated over the job's raw file after dataset grouping, on the **last** dataset that holds every signal it names (else the last dataset, which reports the missing signal). Identifiers are the job's bench var values. The result must be one number: a scalar, or a column of one point (a reduction of a single-step dataset). Anything else is an error row for that output; the job is still done.
- A spec passes when `min ≤ value ≤ max` (absent bounds are open); bounds are scalar expressions over the job's var values. No value, or a bound that fails, is *unknown*.

Job states: `pending`, `queued`, `running`, `done` (simulator exited 0 and its raw file parsed), `failed` (gate, netlist, compile, simulator, raw file), `cancelled`.

**Results file** (`<run dir>/results`): line records in the format.md §2 lexical layer (not a format.md file kind: `crates/run` reads and writes it), header `schemify 1 results` with attribute `design <64 hex>`; then `output <name>` records (plan order), `spec <output>` records, `var <name>` records; then one `job <index> <state>` record per job in index order, with attributes `config`, `corner`, `sample` (each only when set), `set <var> <value>` per assignment, `var <name> <number>` and `value <output> <number>` per present value (canonical number text), `pass <output> pass|fail` per decided spec, `error <output|-> <message>` per error row sorted by output. Written canonically, so `jobs=1` and `jobs=N` give identical bytes.

## 5. Native runner (not on the web)

- Directories: run `<project>/.schemify/runs/<first 16 hex digits of the design hash>/`, one sub-directory per job index holding `netlist.cir`, `out.raw`, `stdout.log`, `stderr.log`; compiled sources cached in `<project>/.schemify/build/`. `.schemify/` is git-ignored.
- The simulator command line comes from the user config `sim ngspice` (`path`, `args`), never from a document (D7); default `ngspice -b -r {raw} {netlist}`. `{netlist}` and `{raw}` are replaced by the job's file names; the working directory is the job directory; stdin is empty.
- At most `workers` simulators at once (default: available cores, at most 1024), started in submission order. A simulator past the timeout (default 1 h) is killed and the job fails. Exit 0 with a raw file: results are recorded (§4). Non-zero exit, or no raw file: the job fails with the last stderr line. A program that cannot be found fails the job naming it.
- Sources: a leaf impl with lang `veriloga` and a `file` is compiled with `openvaf <src> -o <build>/<file stem>.osdi`, lang `verilog` with `iverilog -o <build>/<file stem>.vvp <src>` (programs from the user config `sim openvaf` / `sim iverilog` `path`), only when the output is missing or older than the source, then copied into the job directory (the netlist names them relative, netlist.md §7.2). A missing compiler fails the job naming the program and the source.
- Progress: the last complete stdout line of each running job.

On the web, runner plugins (D16) receive the job's netlist and return its raw bytes, which go through the same result recording; a plugin's failure fails the job.

## 6. Optimizer

An `optimize` record runs as an ask → job → tell loop, one candidate at a time, each candidate one job at the bench's default config and corner (no sample index) assigning every `vary` var its canonical number text.

- Params: the `vary` vars with their constant `min`/`max`; start at the var's value when constant, else the midpoint, clamped into the bounds. Every candidate is clamped into the bounds (D13).
- Score (lower is better): sum over goals, weight 1, of `value` (`min`), `−value` (`max`), `|value − target|` (`target`). A failed job or a missing goal output scores `f64::MAX`.
- `algo nelder-mead` (default): downhill simplex from the start point plus one vertex per param stepped by 5% of its range; reflection 1, expansion 2, contraction 0.5, shrink 0.5. `algo random`: the start point, then uniform draws from xorshift64* seeded by `seed`.
- At most `budget` evaluations. The same bench, seed and simulator results give the same sequence of jobs.

## 7. Plugin run requests

A plugin's `run <bench> <config> <corner>` record with `vary <var> <value>` lines ([plugin-abi.md](plugin-abi.md) §7) is one job: config and corner as named, no sample index, the vary lines as assignments in order. It is prepared and dispatched like any other job; `RunDone { job }` follows when the job is done or failed.

## 8. Amendments

1. format.md rank 4 `var` gains `dist gauss|uniform <spread> [rel]` (§3); rank 6 `optimize` gains `seed <n>`; `montecarlo` and `optimize` defaults and `output` evaluation as stated here.
2. format.md user config `sim <name>`: `{netlist}`/`{raw}` placeholders; names `ngspice`, `openvaf`, `iverilog`.
3. Results live in `.schemify/runs/<design>/results` next to the job directories (D18 said `.schemify/results/`): one directory per design keeps a run's netlists, raw files and results together.
