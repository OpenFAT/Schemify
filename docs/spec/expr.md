# Expression language v1 (normative)

Implements D12, D19, D20, D23. One language for parameter defaults and derived params, conditions, netlist templates, labels, docs variables, outputs/specs and the waveform calculator. Pure and total: no IO, no loops, no recursion, no user-defined functions, bounded size. Implementation: `crates/expr` (lexer → parser to a flat node table → compile to bytecode with constant folding → scalar or column evaluation), seeded from UW-ASIC `crates/wave/src/expr.rs`.

## 1. Values

| Type | Notes |
|---|---|
| `num` | f64. NaN/±inf are errors at the point they are produced (`X010`), never stored. |
| `int` | i64; produced by int params and integer functions; promotes to `num` in arithmetic. |
| `bool` | `true`/`false`. |
| `str` | UTF-8, ≤ 1 MiB (`X002` beyond). |
| `cnum` | complex f64 (AC results). |
| `col` / `ccol` | real/complex column over the sweep axis of one result set (calculator mode only). Comparisons produce bool columns. |

Scalar mode (params, vars, templates, specs) has no `col`/`ccol`. Calculator mode (outputs, graph traces) has all types; scalar operands broadcast over columns. Column operands must share the sweep axis (`X011` otherwise), except that a per-step column (§5) broadcasts over the steps of its parent axis. A column has one element kind (real, complex or bool): `int` values that meet a column become `num`, and a column condition whose arms mix real and complex values yields a complex column. So, element by element, calculator mode can differ from scalar mode only in the sign of zero, in `int` exactness beyond 2^53, and in taking the complex form of a function for real elements of a complex column.

## 2. Lexical

- **Number:** `digits[.digits][(e|E)[+-]digits][suffix]` or `.digits…`. Suffix, case-insensitive: `f`=1e-15 `p`=1e-12 `n`=1e-9 `u`/`µ`=1e-6 `m`=1e-3 `k`=1e3 `meg`=1e6 `g`=1e9 `t`=1e12 `mil`=25.4e-6 (`µ` is U+00B5; Greek U+03BC is accepted too). Any letters, digits or `_` after a valid suffix (or directly after the number) are an error (`X001`); SPICE's "ignore trailing letters" is **not** accepted (the importer strips them). `1.` is `X001` (a digit must follow the dot); a literal that overflows f64 is `X001`.
  - A literal of digits only that fits i64 is an `int`; every other literal is a `num`.
  - Power-of-ten suffixes fold into the decimal exponent, so `2.5p` is exactly the f64 nearest 2.5e-12 (correctly rounded, not `2.5 × 1e-12`). `mil` multiplies by 25.4e-6.
- **String:** `"…"` with escapes `\" \\ \n \t \u{…}` (1–6 hex digits naming a Unicode scalar value). Any other escape, a surrogate, or a missing closing quote is `X009`.
- **Identifier:** `[A-Za-z_][A-Za-z0-9_]*`; namespaced name `ns.name` (one level). `ns.f(…)` calls a plugin function (D20). `phys.name` is a constant (§5). Any other `ns.name` without a call is an ordinary identifier, resolved by the caller like any other.
- **Reserved:** `true false pi` (lowercase only).
- **Operators:** `+ - * / % ^ == != < <= > >= && || ! ? :` and `( ) ,`. `^` is power, right-associative. There is no unary `+`.
- Whitespace: space, tab, CR, LF.

## 3. Grammar and precedence (low → high)

```
expr    = ternary
ternary = or ["?" expr ":" expr]
or      = and *("||" and)
and     = cmp *("&&" cmp)
cmp     = add [("=="|"!="|"<"|"<="|">"|">=") add]     ; non-associative
add     = mul *(("+"|"-") mul)
mul     = unary *(("*"|"/"|"%") unary)
unary   = ("-"|"!") unary | pow
pow     = call ["^" unary]                             ; right-assoc
call    = primary ["(" [expr *("," expr)] ")"]         ; only identifiers are callable
primary = number | string | ident | ns "." ident | "(" expr ")"
```

So `-2^2` is `-4`, `2^3^2` is `512`, `2^-1` is `0.5`, and `a < b < c` is `X009`.

Limits: ≤ 65 536 AST nodes and nesting depth ≤ 256 per expression (`X002`). Nesting depth counts syntactic nesting: the top-level expression, each parenthesised or call-argument or ternary-arm sub-expression, each prefix operator and each `^` right operand. A flat chain such as `a+b+c+…` does not nest, so it is bounded only by the node limit. These bounds make evaluation total.

## 4. Semantics

- Arithmetic on `int` stays `int` for `+ - * %` and unary `-` without overflow; overflow, `/` and `^` give `num`. `%` follows the sign of the dividend; `% 0` is `X010`.
- `&&`/`||` short-circuit; `?:` and `if()` evaluate one arm. Conditions and the operands of `&& || !` must be `bool` (no truthiness): anything else is `X003`.
- Comparisons of mixed `int`/`num` compare as `num`; `str` compares bytewise; `bool` and `cnum` support only `==`/`!=` against their own type; other mixes are `X003`.
- `+` on two `str` concatenates; any other `str` arithmetic is `X003`. `cnum` absorbs `int`/`num` in `+ - * / ^`; `%` on `cnum` is `X003`.
- **Static checking:** a constant subexpression that is always evaluated (not under a `?:`/`if` arm or the right side of `&&`/`||`) is evaluated at compile time, so its errors (`X003`, `X010`, `X001`) are reported when the expression is compiled. Under a lazy arm they are reported only if the arm runs, so `false && 1/0 > 0` is legal.
- Identifier resolution order, by context (the caller resolves; the evaluator receives one value per identifier, in first-occurrence order):
  - **Cell scope** (vars, param defaults, instance overrides inside a cell): the cell's own params (effective values for the instance being elaborated), then the cell's vars. Unknown → `X004`.
  - **Bench scope:** bench vars, after corner `set` overrides and sweep/MC/optimizer assignments.
  - **Calculator:** result signals via `v(net)`, `i(dev)`, `op(inst, param)` (run.md §4), `x()` (sweep axis), plus bench vars. A signal names a raw vector case-insensitively: `v(net)` is the vector `v(net)`, else `net` (Xyce writes bare node names); `i(dev)` is `i(dev)`, else `dev#branch`, else `dev`. No match is `X004`.
- Dependency graph: every identifier is an edge; a cycle is `X005` and names the cycle as `a -> b -> a` (`a` depends on `b`, …). One pass finds them all: every expression on a cycle (a strongly connected component of two or more expressions, or one that reads itself) appears in at least one reported cycle, and no other expression does; the evaluation order puts each component after the components it reads.

## 5. Built-in functions

Function names are case-sensitive; `V(…)` and `I(…)` are accepted as aliases of `v`/`i`. Unknown names are `X006`, wrong argument counts `X008`.

Scalar (also element-wise on columns):
`abs sqrt exp ln log10 sin cos tan asin acos atan atan2(y,x) sinh cosh tanh floor ceil round(x)` (ties away from zero) `int(x)` (truncate toward zero; outside the i64 range is `X010`) `min(a,b,…) max(a,b,…)` (at least two arguments) `clamp(x,lo,hi)` (= `min(max(x,lo),hi)`) `sign pow(x,y)` (= `x^y`) `if(c,a,b)` (= `c ? a : b`) `snap(x,grid)` (round to nearest multiple, ties away from zero; `grid` 0 is `X010`) `eng(x[,digits])` → `str` engineering notation, `str(x)` canonical number text (§7), `num(s)` parse with SI suffix, `len(s)`, `spectre_source(s)` (a SPICE source specification as Spectre parameters, below).
- `abs floor ceil round sign min max clamp` keep `int` when every argument is `int`; `int` and `len` return `int`; the elementary functions return `num`, or `cnum` for a `cnum` argument (`sqrt(cplx(-4,0))` is `2i`, while `sqrt(-4)` is `X010`).
- `eng(x[,digits])`: `digits` is an `int` in 1..=17 (default 3) significant digits; suffixes from §2 (`meg` for 1e6, `u` for micro), so `num(eng(x))` reads it back; beyond the suffix range the mantissa grows (`0.001f`, `1000t`). Examples: `eng(4700)` = `4.7k`, `eng(2.2e6)` = `2.2meg`.
- `str(x)`: `int`/`num` per §7, `bool` as `true`/`false`, `str` unchanged; `cnum` is `X003`.
- `num(s)`: exactly a §2 number literal with an optional leading `+`/`-`; no whitespace, no trailing text (`X001` otherwise). `int`/`num` arguments pass through as `num`.
- `len(s)` counts Unicode scalar values.
- `spectre_source(s)`: `s` is a SPICE independent-source specification `[[dc] v] [ac mag [phase]] [f(args)]` (parts in any order, each at most once; keywords case-insensitive; args in parentheses, separated by blanks or commas); the result is the Spectre `vsource`/`isource` parameters, in this order: `dc=`, `mag=` `phase=`, then `type=` and the function's parameters by position: `pulse` → `type=pulse` `val0 val1 delay rise fall width period`, `sin` → `type=sine` `sinedc ampl freq delay damp sinephase`, `exp` → `type=exp` `val0 val1 td1 tau1 td2 tau2` (from 2 up to that many args), `pwl` → `type=pwl wave=[t1 v1 …]` (time-value pairs). Every value is a number, written in canonical text (§7), or an identifier (a bench parameter), written verbatim. An empty `s` gives an empty string (Spectre defaults to 0). Anything else (`sffm`, `am`, PWL options, an 8th `pulse` arg, a quoted SPICE expression, a repeated part) is `X009` naming the spec. The core sources' `format spectre` use it ([netlist.md](netlist.md) §7.5).

Complex: `re im mag ph` (degrees; the phase of zero is 0) `db` (20·log10|x|) `conj cplx(re,im)`. They accept `int`/`num` as a complex number with zero imaginary part.

Calculator-only (columns): `v(net) i(dev) op(inst, param) x() deriv(y) integ(y) avg(y) rms(y) ymax(y) ymin(y) pp(y) at(y, x0)` (linear interpolation) `cross(y, level[, edge, n])` → x of nth crossing (`edge` = `1` rising, `-1` falling, `0` either) `delay(y1, l1, y2, l2)` `settle(y, tol)` `fft(y)` `gain_margin(h) phase_margin(h)`. In scalar mode they are `X006`; given a scalar where a column is needed they are `X003`. Functions operate per sweep step; `deriv`/`fft` never cross step boundaries.
- `v`/`i` take a signal name: a string literal or a bare (optionally dotted) identifier taken verbatim, never resolved in scope (`v(out)`, `v("d[3]")`). Anything else is `X003`.
- `op(inst, param)`: the DC operating-point value of parameter `param` of the device at instance path `inst` (`op("M1", "gm")`, `op("/X1/M2", "id")`, `op(M1, id)`): one number, which broadcasts like a scalar over any column it meets. Both arguments are names taken verbatim like `v`'s (a string literal or a bare identifier); `inst` is non-empty without blanks or `]`, `param` is `[A-Za-z0-9_]+`; anything else is `X003`. It is a signal of kind *op* named `<inst>[<param>]`, bound by the caller to one finite number (anything else is `X011`). Only a run's job binds it (dataset, path syntax and errors: [run.md](run.md) §4); a graph trace or plot slot gets `X006`, and scalar mode (docs vars) has no results, so `X006` there too, like every calculator function.
- `deriv`: central differences inside a step, one-sided at its ends, 0 for a one-point step. `integ`: cumulative trapezoid from the start of each step.
- Reductions `avg rms ymax ymin pp at cross delay settle gain_margin phase_margin` return one value per step: a column on the per-step axis (x = step index), which broadcasts over its parent's steps (`v(a) - avg(v(a))` removes each step's mean). `avg`/`rms` are trapezoid-weighted over x (a one-point step gives its value, or its magnitude for `rms`). `avg rms ymax ymin pp cross delay settle fft` need a real column; `deriv integ at` and the margins also take complex ones.
- A crossing is rising when y goes from below `level` to at-or-above it, falling from above to at-or-below; x is linearly interpolated. `delay` = first crossing of `y2` minus first crossing of `y1`, either edge. `settle(y, tol)` = the first x after which `|y − y_last| ≤ tol` holds to the end of the step (absolute `tol` ≥ 0).
- `fft`: per step, resample linearly onto `m` uniform points over `[x_first, x_last)`, `m` = the step length rounded up to a power of two; keep bins `0..m/2` at `k / (x_last − x_first)`, scaled to single-sided amplitude (DC by 1/m, others by 2/m). Needs ≥ 2 points and `x_last > x_first` per step (`X011`). Results of `fft` on the same source axis share one frequency axis.
- `phase_margin(h)` = 180° + phase at the first unity-gain crossing (|h| from ≥ 1 to < 1); `gain_margin(h)` = −|h| in dB at the first −180° phase crossing. `h` is a loop gain whose phase starts near 0°; phase is unwrapped along the step; values are interpolated linearly between points.
- A measurement with no answer (no crossing, `at` outside the axis, no unity-gain or −180° point) is `X010`.
- A column condition (`?:`, `if`, `&&`, `||` with a bool column) evaluates both arms over the whole column and selects per element, so an error in either arm is reported even where it is not selected. A scalar condition still short-circuits.

Physical constants live under `phys.`: `phys.k` = 1.380649e-23, `phys.q` = 1.602176634e-19, `phys.h` = 6.62607015e-34, `phys.c` = 299792458 (SI 2019 exact), `phys.eps0` = 8.8541878188e-12 (CODATA 2022), `phys.kelvin0` = 273.15. An unknown `phys.` name is `X004`; calling one is `X006`.

Plugin functions: `ns.f(args)` resolved through the host's registry, at most 16 arguments (`X008`), called once per element when an argument is a column. If the plugin is missing and the record has `cached <value> <hash>`, the cached value is used for the whole expression and the value is flagged *unverified* (D20); otherwise `X006`. Caching is the caller's: the compiled expression lists the plugin functions it calls.
- Calls cross the plugin boundary as text (plugin-abi §2 `schemify_call`): `int`/`num` arguments in canonical number text (§7), `bool` as `true`/`false`, `str` quoted with the format §2 escapes; a `cnum` argument is `X003`. An argument count other than the registered arity is `X008`. The result is one number (`num`), `true`/`false`, or one quoted string (`str`, format §2 escapes); anything else is `X003`. A string result is owned by the caller's value (the evaluator copies it in), and its D20 `cached` text is quoted ([docs-page.md](docs-page.md) §6). A plugin that reports an error code fails the call with `X010` naming the function and the code.

## 6. Templates

A template is a string with `{expr}` holes; `{{` and `}}` are literal braces. A hole ends at the first `}` outside a string literal; an unclosed `{`, an empty hole, or a lone `}` is `X009`. Each hole is a scalar-mode expression; all holes of a template share one identifier list. Each hole's value is rendered by §7 (`bool` as `true`/`false`, `str` verbatim, `cnum` is `X003`). A failed render produces no partial output. In netlist context these extra identifiers exist:

| Identifier | Value |
|---|---|
| `name` | the instance name as emitted for the dialect |
| `cell` | subckt / module name of the selected impl |
| `model` | the impl's `model` attr, else the `model` param |
| `pins` | space-joined net names in pin order |
| `<pin name>` | the net connected to that pin (bus pins: space-joined bits) |
| `params` | `k=v` pairs for every param marked for the dialect (default: all non-computed params whose value differs from the subckt default; see netlist spec) |
| `file` | absolute path of the impl's `file` |

Pin names shadow params of the same name inside templates (`X007` warns at load when they collide; the caller detects it, since only it knows pins and params).

## 7. Canonical number text

Used by templates, netlists and `str()`, identical on every platform:
- `int`: plain integer, always (`2`, `-40`, `1000000000000000000`).
- `num` that is an integer with |x| < 1e15: plain integer (`2`, `-40`; `-0` is `0`).
- otherwise if 1e-4 ≤ |x| < 1e15: shortest round-trip decimal (`0.15`, `2.5e-3` is written `0.0025`).
- otherwise: shortest round-trip mantissa with exponent `e-12` form (`1e-12`, `2.5e-15`, `1.2e+15` is `1.2e15`).

`num(str(x)) == x` for every finite `x`.

## 8. Error codes

| Code | Meaning |
|---|---|
| X001 | bad number literal / suffix (also `num()` of bad text) |
| X002 | expression too large or too deep; string over 1 MiB |
| X003 | type error |
| X004 | unknown identifier (or `phys.` constant) |
| X005 | dependency cycle |
| X006 | unknown function / calculator-only function in scalar mode / missing plugin without cache |
| X007 | pin/param name shadowing in a template (warning) |
| X008 | wrong arity |
| X009 | parse error |
| X010 | NaN or infinity produced; `int()` out of range; a measurement with no answer |
| X011 | column length / axis mismatch |

Every error carries the byte span of the offending token, operator or call in the compiled text (template coordinates for a template).
