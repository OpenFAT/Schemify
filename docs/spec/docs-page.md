# Docs page (normative)

Implements D19 (docs `var` blocks are the cell's variables table), D20 (plugin functions, cached
values), D21 (LaTeX math as a core widget) and the docs half of D23 (graphs embeddable in docs).
Every `.shcomp` and `.shbench` has a docs page shown beside its schematic. Implementation:
`crates/app/src/docs.rs` (projection, edits, cache write-back), `crates/ui/src/prose.rs` (the
`prose` widget), `crates/elab` (`CellState`), `plugins/gmid-lut` (the reference D20 plugin).
Blocks are the rank-4 records of [format.md](format.md) §4; nothing here adds a record kind.

## 1. Placement

- The work area is a row: the schematic canvas, then the docs pane (`docs.pane`: a toolbar
  `docs.bar` and a `scroll` node `docs.page`). The default UI (`ui/default-plugin`) declares
  them with a 560 px pane; the core base (no UI plugin) declares the same nodes, so docs render
  without plugins (D21).
- Shown per tab: a tab opens with its page shown when the document has at least one doc block.
  `view.docs` (default key Alt+D, View → Docs page, the pane's `x` button, the Docs button of the view switcher over the canvas, plugin-abi §11 item 16) toggles it. While
  hidden the app's state contribution removes `docs.pane` and the canvas takes the width.
- The page shows the doc blocks of the tab's active level (descending into a cell shows the
  cell's page) in file order, one `docs.b.<row>` panel per block under `docs.page`. The wheel
  over the page scrolls it (40 px per line, clamped to the content); rows scrolled out of the
  pane are clipped and cannot be clicked.

## 2. Prose blocks

A `prose` block's verbatim lines are markdown, laid out by the `prose` widget at the pane width:

| Markdown | Rendering |
|---|---|
| `#`…`######` heading | bold, ×1.6 / 1.35 / 1.2 / 1.1 / 1 / 1 of the body size |
| paragraph (lines joined by spaces) | wrapped at the pane width, line height 1.35 em |
| `*em*`, `_em_` | italic (synthesized oblique of the UI face); `_` never opens or closes inside a word, so `gm_id_in` stays literal |
| `**strong**`, `__strong__` | bold |
| `` `code` `` | monospace (KaTeX Typewriter) |
| ```` ```lang ```` … ```` ``` ```` | monospace block on a faint shade; wrapped at the pane width |
| `-`, `*`, `+`, `1.` items | marker then body indented 1.6 em; two leading spaces per nesting level (1.5 em each); indented continuation lines join the item |
| `>` quote | italic, indented, with a bar |
| `---` (3+ of `-*_`) | rule |
| pipe table (`\|` rows; a `---` second row makes the first a bold header) | full grid; at most 16 columns. Each column is as wide as its widest cell unwrapped (plus padding) when the table fits the pane; otherwise columns narrower than an equal share of what the others leave keep that width, and the rest share the remainder in proportion to their content (cells wrap), each at least 3 em |
| `[text](url)` | the text with its own markup (emphasis nests inside); links are not followed |
| `[^label]` | the footnote's number in superscript digits (`¹`); an undefined label stays literal |
| `[^label]: text` (a line of its own; lines indented 2+ spaces continue it) | listed at the end of the block under a short rule, numbered `1.`, `2.` … in definition order, at 0.9× the body size |
| `![alt](path)` alone on a line | the PNG image at `path`, at its pixel size, scaled down (never up) to the pane width. `path` is project-relative: no leading `/`, no `\` or `:`, no empty, `.` or `..` segment (the plugin ABI's path rule), so a page cannot read outside its project; at most 16 MiB and 4096 px a side (checked in the header before decoding). Missing, refused or undecodable: `[image: alt]` in italics. Read once per path per page |
| `![alt](path)` inside text | the alt text |
| `\*` and other backslash-escaped punctuation | the literal character |
| `$…$` | inline math through the `math` widget (RaTeX, KaTeX fonts), baseline-aligned in the line |
| `$$…$$` (on its own lines, or one line) | display math centred, ×1.15 the body size |
| ```` ```plot ```` fence | an embedded graph slot (§5) in place of the fence |

Rules for math delimiters (pandoc's): `$` opens only before a non-space, a single-`$` formula
closes at a `$` not escaped and not followed by a digit (`$5 to $10` is text). A formula outside
the supported LaTeX subset is shown as its source text, never dropped. Anything else is paragraph
text. The parser never fails; sources are cut at 2^14 lines.

Editing: a click on a prose block selects it and replaces it with a multi-line `text_area`
holding its source. The area grows with its text up to 24 font sizes tall, then scrolls inside
itself: the wheel over it scrolls it (not the page), and caret moves and typing keep the caret
in view. Enter breaks a line; Ctrl+Enter applies; a click elsewhere applies; Esc cancels.
Applying is one transaction: `put prose <id>` with the new verbatim lines (trailing spaces
trimmed), or `delete prose <id>` when the text is blank.

Live preview: while the editor is open, a `Preview` caption (`docs-preview-cap`) and the typed
text laid out by the same `prose` widget (`docs.b.<row>.preview`, style `docs-preview`, math
included) sit directly below the area, following every keystroke (`field.typed`). Blank text
previews as `(empty: applying deletes this block)`. A ```` ```plot ```` fence previews as a code
block, and an image path not yet on the page as its alt text (images load once applied). The
preview carries no command: a click on it is a click elsewhere, so it applies. Applying or
cancelling removes it. Nothing is written to the document until applying. Cost: the state
contribution is reparsed only when its text changed, and a prose layout is cached per (source,
size, width), so the rest of the page is not laid out again while typing.

Source view: `docs.source` (the toolbar's `Rendered` / `Source` toggle, pressed and `checked`
while source shows) shows every prose block as its verbatim lines, monospace on a faint shade
in a `docs-source` frame: no markup, no math, no plot slots (`$x^2$` reads `$x^2$`). Var blocks
are unchanged. A click on a block still opens the editor with its preview. The mode is kept per
tab (switching tabs and back keeps it; a new tab starts rendered) and never touches the
document.

## 3. Var rows

A `var <name> <expr>` block shows as one row: `name = [expr] = value`, where `[expr]` is an
editable text field.

- **Value text:** a `num` in engineering notation, 4 significant digits, with the SI prefix glued
  to the `unit` attribute (`3.881µm`, `20µA`, `23.87MHz`); `int`, `bool` and `str` in canonical
  text followed by the unit (`15 1/V`).
- **Status (D13 colours):** in range: `docs-value` (green in the default theme). `min`/`max`
  (expressions in the cell scope, format §4) violated: `docs-value-bad` (red) with the reason
  (`above max 25`). An evaluation error: red, the expression code and message (`X009 …`,
  `X010 gmid.w: plugin reports Range`); a value whose inputs failed: `(an input has an error)`.
- **Unverified (D20):** when the value rests on a `cached` stand-in for a missing plugin
  function, the row adds `unverified (cached; plugin not loaded)` (`docs-unverified`, amber).
- The name carries the `doc` attribute as its tooltip; clicking it selects the block.
- **Editing:** Enter in the field commits one transaction: `set_field var <name> 1 <expr>`, plus
  `remove_attr var <name> cached` when the var has a cache (the cache belongs to the old
  expression). Any text is accepted and stored; errors show live (D13).

## 4. Values and the frame

- Values are `elab::CellState` of the active level's document, built when the page needs it
  (the document has doc blocks), incrementally recomputed with the ops of each commit (an
  undo or redo recomputes over the ops of the entry it reverts, while that entry is among the
  last 64 commits), and rebuilt after a plugin composition change, a plugin command / I/O
  completion (a pure function may answer differently after its plugin loaded data), or an
  edit to a cell the document instantiates (its interface may have changed).
- Values are kept for every document shown: switching tabs or hierarchy levels parks the state
  of the document left (at most 8 parked, oldest dropped) and takes back the parked state of
  the one shown, still incremental: ops committed to a parked document are queued on its
  state; an edit to a cell it instantiates, a plugin change or closing it drops it. D20 caches
  of a document are written while it is active (§6).
- In the same frame as an edit: `doc::apply_edits` → recompute → D20 write-back (§6) → UI
  compose (the page) → scene build. Symbol texts (`stext W={eng(W)}`) of instances render the
  evaluated param values, so `W = W_in` shows the var's value on the canvas.
- Measured (D39, `tests/perf.rs`, release): a var edit with 1000 dependent instance overrides,
  through the page field, is 1.4 ms per frame; 1.5 ms when the var calls a `gmid` function and
  its cache is rewritten.

## 5. Plot slots

A ```` ```plot ```` fence holds one calculator-mode expression ([expr.md](expr.md), e.g.
`db(v(out))`). The page splits the prose block at the fence and puts a graph slot there, drawn
from the editor's current results when the page's cell is their bench, the bench instantiates
it (by file stem, also when results were reloaded), or it is any cell below the bench among the
open documents (a run opens every cell it reaches). The slot is an embedded `render::waves`
pane (a `graph` node of style `docs-graph`, 170 px), autoscaled, with one trace per finished job
of the run: every sweep point, corner and Monte Carlo sample (the selected job's first, job 0
after a run; at most 16 jobs; an optimizer run shows the selected job only), each on the last
dataset of its job's raw file that evaluates the expression. Without results the slot says
`no results yet`; an expression that does not evaluate, or reduces to a number on the selected
job, shows why instead of a graph.

A staged edit on the page's document (an optimizer result, a plugin's `stage`) adds a banner at
the top (`docs.staged`, its label, Accept and Reject) and, beside each var it changes, the
staged expression (`→ <expr>`): the docs half of the visual diff; the canvas half outlines the
objects it changes.

## 6. D20 cache write-back

- **Which records:** an own `param` (its default, or its `computed` expression) or an own
  `var` whose expression calls plugin functions (`ns.f(…)`), whose current value a loaded
  plugin computed (not unverified, not an error) and is a number, `bool` or `str` (not `cnum`).
- **What:** `cached <value> <hash>` on the record: `<value>` is the canonical text (expr §7),
  a `str` quoted with the format §2 escapes (`"tt"`) so it never reads back as a number or a
  bool; `<hash>` is the content hash of the plugin serving the functions: the first 8 bytes of
  the sha256 of its wasm module, as 16 lowercase hex digits. Functions from several plugins:
  the same hash over their hashes concatenated in load order.
- **When:** whenever the stored pair differs from the computed one. After a docs-page edit the
  write extends that edit's undo entry and its journal record ([edits.md](edits.md) §1
  `extend`): one edit, one undo, no extra journal records (the edit's record is written out
  after its cache ops joined it, in the same frame). Otherwise (a file opened with stale
  caches, a plugin with a new hash, an edit elsewhere) it is its own transaction, `update
  plugin cache`; it waits in a frame that undid or redid, because a commit would clear the
  redo stack.
- **A cache belongs to its expression:** every transaction the editor applies that changes a
  var's expression or a param's default or `computed` expression without touching its
  `cached` also removes that `cached` (a `put` has it stripped from the replacement), so a
  stale value never stands in for a new expression.
- **Without the plugin:** the cached value stands in for the whole expression and is marked
  unverified, as are its dependents (D20); nothing is written. A record whose expression
  changed has no cache and shows `X006` until the plugin is back.
- **Changed plugin:** the loaded plugin recomputes every value; the differing hash makes the
  pair differ, so every cache is rewritten.

## 7. Commands

| Command | Effect |
|---|---|
| `view.docs` | show / hide the active tab's page |
| `docs.edit` | (click on prose) select the block, open its editor with the live preview; from a script the argument is the block row |
| `docs.select` | (click on a var name) select the block; from a script the argument is the block row |
| `docs.source` | show the active tab's prose blocks as source, or rendered again |
| `docs.var_ok` | (Enter in a var field) set the var's expression |
| `docs.prose_ok` | apply the open prose editor (argument: the text) |
| `docs.add_prose`, `docs.add_var` | insert a block after the selection (end when none); a new var is `v<k> 0` with the first free `k`, its field focused |
| `docs.move_up`, `docs.move_down` | `reorder` the selected block by one |
| `docs.delete_block` | delete the selected block |

Each command that edits is one transaction, hence one undo.
