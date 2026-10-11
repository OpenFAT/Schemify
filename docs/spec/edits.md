# Edits, undo, journal, external changes (normative)

Implements D11b, D27, D28, D34 and the `staged_edits` table required by the change drill (ARCHITECTURE.md). Owner: `doc`. Every change to design data, from any source (UI, plugin, JSON-RPC, optimizer, importer, AI assistant), is an **edit request**; `doc::apply_edits` is the only code that mutates design tables.

## 1. Edit requests

A request is a **transaction**: an ordered list of ops on one document, plus a label (for undo menus) and a mode.

| Op | Fields | Inverse |
|---|---|---|
| `insert` | record (kind, fields, attrs, verbatim lines, comments; an id field of `-` is assigned by doc); optional row (`insert_at`, ordered tables only) | `delete` |
| `delete` | target | `insert_at` of the full prior record at its prior row |
| `put` | target, full replacement record (same kind, same id; keeps the row) | `put` of the prior record |
| `set_field` | target, field index, value | `put` of the prior record |
| `set_attr` | target, key, values | `put` of the prior record |
| `remove_attr` | target, key, optional first value | `put` of the prior record |
| `reorder` | target in an ordered table (ranks 1, 2, 4, 5), new row | `reorder` back |

Every modification of an existing record is "read it, change a copy, validate, put it back", so one inverse (`put` of the prior record) is exact even where the table normalizes (wire endpoint order, spec format §4) or a value was snapped. The inverse of a transaction is the inverses of its ops in reverse order.

**Targets** are `<record word> <key>`: the id for id-keyed kinds, the name for named kinds, the draw-order index for rank-2 graphics (`line 3`), and `schemify 0` for the header. Ids are immutable (`set_field` 0 or a `put` with another id is `E043`); names are renamed with `set_field` 0. The header is never inserted or deleted; it changes through `set_attr`, `remove_attr` and `put` (attributes only).

**Attributes:** `set_attr` replaces the first line with the key (for a repeatable attribute, the line whose first value matches) or appends one. `remove_attr` removes every line with the key (only those whose first value is the given one, if given); removing nothing is `E040`.

**Default insert position** in an ordered table: after the last row of the same kind, else at the end. Rank-3 and rank-6 tables are unordered (the writer sorts them); a row there is `E043`.

Higher-level gestures (move = `set_attr at` or `set_field` geometry; rename net = `set_field` on labels; paste = inserts with fresh ids) are composed by callers. Ids are drawn by `doc` from a caller-seeded generator (D9; redraw on collision) unless the request supplies them (importer: deterministic).

**Op text** — the line-record form of ops, used by the journal (§3) and by plugin `edit` records (plugin ABI §7), one record per op:

```
insert <word> <fields…>            (+ the record's attribute and | lines; # comments before it are its comments)
insert_at <row> <word> <fields…>
delete <word> <key>
put <word> <key> <fields…>         (+ attribute, | and # lines of the replacement)
set_field <word> <key> <index> <value>
set_attr <word> <key> <attr> <values…>
remove_attr <word> <key> <attr> [<first value>]
reorder <word> <key> <row>
```

Modes:
- **commit**: validate → apply → push inverse to the undo stack → append to the journal.
- **stage**: validate → store in `staged_edits` (not applied; ids are drawn and values snapped at stage time, so the preview is stable). The visual diff renders design ⊕ stage. `accept(stage)` re-validates and commits it as one transaction (a failing accept keeps the stage); `reject(stage)` drops it. Stages are per document, identified by a u16, capped at 2^8 open stages (`E043` beyond). Stages are not journaled.

**What changed.** Each request yields an outcome (committed, staged, undone, redone, dropped, nothing, rejected, refused). On request, `doc` also reports the ops it applied, in application order: a commit's resolved ops (ids drawn, values snapped), an undo's inverse ops, a redo's ops; none for the other outcomes. Committed as one transaction onto the state before the request, they give the state after it, so owners of derived tables (elab) update from them without keeping copies of the requests.

Validation is all-or-nothing per transaction: every failing op is reported as (op index, error code) and nothing is applied. Per op: target exists (`E040`), id free (`E041`), name free (`E024`), the record is legal for the file kind and well formed (format spec §4: `E027`, `E029`, `E022`, `E024`, `E028` for keys that are not words or comments/verbatim lines that could not be written back), the op is allowed (`E043`), computed-param overrides (`E030`). At the end of the transaction, so a transaction may pass through invalid states: zero-length wires it touched (`E026`), kind rules (`E025`, only when the transaction makes a valid document invalid, so a new cell can be built up), and in-file references (`E042`: `waive` → instance, constraint `member` → id, `body` → impl, `spec` → output, header `default` → impl; only references the transaction leaves dangling, so a file loaded with dangling references stays editable). End-of-transaction errors are attributed to the last op touching the record involved.

**Grid snapping** (D13) applies to instance parameter overrides the op writes (every override of an inserted or `put` instance; the one key of a `set_attr`), when the value is a plain number (format spec §3 number literal; SI suffixes and expressions are left for elab to gate, `E032`) and the instantiated cell resolves (netlist spec §1; unresolved cells get no parameter semantics and elab reports `E001`). A param with `grid g` snaps to the nearest multiple of `g`; otherwise `step s` snaps to `min + k·s` (`min` when it is a plain number, else no snapping; 0 without `min`). An expression `grid` or `step` disables snapping. Arithmetic is exact decimal, ties away from zero; the snapped value is what is stored and journaled, so replaying the journal needs no parameter facts.

**Overrides equal to the default** (D40) are kept as the user wrote them (never silently dropped) and reported as `W002` by the redundant-override query; the canonicalizer is an ordinary transaction of `remove_attr` ops the user applies.

## 2. Undo / redo

- One undo stack and one redo stack per open document (cell), independent of hierarchy navigation (descending/ascending never clears them).
- Undo pops a transaction's inverse and applies it as a commit **without** pushing to undo (it goes to redo); any new commit clears redo. Undo and redo are journaled as `undo <seq>` / `redo <seq>` so recovery replays them.
- Stack capacity 2^16 transactions per document; the oldest are dropped first.
- Dirty means "differs from the last loaded/saved bytes": undoing back to the saved state is clean again. Duplicate ids re-assigned at load (format spec §4, `W001`, deterministic, references keep the first occurrence) make the document dirty until saved.

## 3. Journal (crash recovery, session restore, undo after restart)

Sidecar file `.schemify/journal/<project-relative cell path>.journal`, gitignored, a format `journal` file (format spec §1: no known record words, every record kept as written; op text is read the same way):

```
schemify 1 journal
base <sha256 of the file bytes the following records apply to>
hist <seq> <unix_ms> <label>        # undo history at `base`: forward op text as | lines
inverse <seq>                       #   followed by its inverse op text as | lines
tx <seq> <unix_ms> <label>          # a commit after `base`: forward op text as | lines
inverse <seq>                       #   followed by its inverse op text
undo <seq>
redo <seq>
save <seq> <sha256 of written bytes>
```

- Op text (§1) is nested as `|` verbatim lines, the same as plugin `edit` records. Each transaction carries its inverse, so undo after a restart needs no copy of older file bytes.
- The journal starts with the first commit of a document that exists on disk (a never-saved document has no journal until its first save). Every commit appends `tx` + `inverse`, every undo/redo its marker (fsync batched at ≤ 100 ms), every confirmed save `save`. The file hash is computed only when the journal needs it.
- **Open:** the last checkpoint (`base` or `save`) whose hash equals the file's current hash splits the journal. Records before it rebuild the undo/redo stacks without applying anything (`hist` records directly after a `base` are history at that base). Records after it are unsaved edits from a crash → *recovery offer*; the document refuses edits and saves (`E044`) until the app recovers (replays them) or discards them (the journal is rewritten without them). If no checkpoint matches, the file changed outside Schemify; since the last saved bytes are gone after a restart, the tail after the last checkpoint is replayed onto the current file instead of merged: transactions that no longer validate are dropped and counted, history is dropped, and the journal is rewritten against the current file.
- **Torn tail:** a crash mid-append leaves a partial record; the reader drops the record holding the first lexical error and everything after it, and a `tx` without its complete `inverse`.
- **Rewrites** describe the current lineage: `base <hash of the disk bytes>`, the undo entries up to the saved state as `hist`, the rest as `tx`; redo history is not kept across a rewrite. They happen on compaction, discarded or diverged recovery, external reload or merge (§4), and when edits landed between `save` and its confirmation (§5).
- **Compaction:** on a save, when the journal exceeds 2^16 transactions or 64 MiB, rewrite it as `base <current saved hash>` plus the last 2^12 undo entries as `hist` (the in-memory undo stack is cut to the same entries).

## 4. External changes

Files are watched (native: OS notifications; web: OPFS writes are always ours). On change of a file open in memory:

1. No unsaved local edits → reload silently (new `base`); the undo history of the old bytes is dropped.
2. Unsaved local edits → `format::merge3(O = last loaded/saved bytes, A = canonical write of the in-memory document, B = new disk bytes)`:
   - clean → load the merged result; local edits remain unsaved (dirty); undo history is rebased: the unsaved transactions are replayed onto B (recomputing their inverses; those that no longer validate are dropped and counted), and when that replay differs from the merge (merge3 re-ids, orders or merges components differently) one more entry "merge external changes" takes the replay to the merged state. Undo-all then reaches B exactly. History older than the last save and the redo stack are dropped; the journal is rewritten against B. When the unsaved state is not reachable through the undo stack (undone below the save, then edited), the unsaved edits are taken as one entry: the record-level difference from O to A.
   - conflicts → the document enters *conflict state*: each conflict block lists our and their records as canonical text, the user picks ours, theirs or edited text per block, and the result loads like a clean merge. Edits, undo, redo, saves and further external changes are refused (`E044`) until resolved.
3. Bytes that do not parse (a git merge left markers) change nothing; the app reports them.

The same `merge3` backs `schemify merge-driver` (D34), so git merges and live external edits behave identically.

## 5. Saves

`save` returns the canonical form (format spec §2) when the document is dirty (a save never rewrites files that are not dirty) and the kind rules hold (`E025` otherwise: the file would not parse); it is refused (`E044`) for read-only documents, in conflict state, or with a pending recovery. The app writes the bytes atomically (write temp + rename; on the web, OPFS write then swap) and hands them back with `saved`, which makes them the merge base and appends `save` to the journal. Edits made between `save` and `saved` stay dirty: if the saved state is still on the undo stack the journal is rewritten from it, otherwise the edits are rebased onto the written bytes as in §4.

## 6. Document model notes

- **Canonical form beyond the format writer:** `doc` writes rank 1 grouped by kind (pins, params, impls; file order within each), and an instance's `at` and `name` before its other attributes (format spec §4). A hand-written file that interleaves rank-1 kinds is regrouped on its first save.
- **Newer format versions** open read-only: `doc` does not model unknown records (format spec §5 keeps them for format-level tools), so edits and saves are refused with `E044`.
- **Load findings** never fail a load: `W001` (re-ided duplicates), `E024` (an instance overriding the same key twice), `E042` (dangling in-file references) are reported for elab and the UI to gate.

## 7. Codes

`E001`, `E020`–`E030` and `W001` keep their meanings (format spec §7, netlist spec §8). Doc-level:

| Code | Meaning |
|---|---|
| E040 | edit target not found (missing id/name/row, stale document handle, unknown stage, attribute to remove absent) |
| E041 | id already in use |
| E042 | in-file reference to a record that does not exist |
| E043 | operation not allowed (immutable id, row in an unordered table or out of range, header insert/delete, capacity) |
| E044 | document read-only (newer version), in conflict state, or awaiting a recovery decision |
| W002 | instance override equal to the cell default (kept; canonicalizer hint) |
