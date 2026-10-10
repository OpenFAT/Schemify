# Edits, undo, journal, external changes (normative)

Implements D11b, D27, D28, D34 and the `staged_edits` table required by the change drill (ARCHITECTURE.md). Owner: `doc`. Every change to design data, from any source (UI, plugin, JSON-RPC, optimizer, importer, AI assistant), is an **edit request**; `doc::apply_edits` is the only code that mutates design tables.

## 1. Edit requests

A request is a **transaction**: an ordered list of ops on one document, plus a label (for undo menus) and a mode.

| Op | Fields | Inverse |
|---|---|---|
| `insert` | record (kind, fields, attrs; id assigned by doc if absent) | `delete` |
| `delete` | id (or kind+name) | `insert` of the full prior record |
| `set_field` | id, field index, value | `set_field` with prior value |
| `set_attr` | id, key, value | `set_attr` prior / `remove_attr` |
| `remove_attr` | id, key | `set_attr` prior |
| `reorder` | rank-1/2/4 record, new position | `reorder` back |

Higher-level gestures (move = `set_attr at` or `set_field` geometry; rename net = `set_field` on labels; paste = inserts with fresh ids) are composed by callers. Ids are drawn by `doc` (random, redraw on collision; D9) unless the request supplies them (importer: deterministic).

Modes:
- **commit**: validate → apply → push inverse to the undo stack → append to the journal.
- **stage**: validate → store in `staged_edits` (not applied). The visual diff renders design ⊕ stage. `accept(stage)` commits it as one transaction; `reject(stage)` drops it. Stages are per document, identified by a u16, capped at 2^8 open stages.

Validation is all-or-nothing per transaction: any failing op rejects the whole transaction with a list of (op index, error code); nothing is applied. Validation covers id existence, kind/rank rules (format spec), computed-param overrides (`E030`), and grid snapping (D13: values with `grid`/`step` are snapped *before* validation, and the snapped value is what is stored and journaled).

## 2. Undo / redo

- One undo stack and one redo stack per open document (cell), independent of hierarchy navigation (descending/ascending never clears them).
- Undo pops a transaction's inverse and applies it as a commit **without** pushing to undo (it goes to redo); any new commit clears redo.
- Stack capacity 2^16 transactions per document; the oldest are dropped first (and the journal keeps them, §3).

## 3. Journal (crash recovery, session restore, undo after restart)

Sidecar file `.schemify/journal/<project-relative cell path>.journal`, gitignored, same line-record syntax (kind `journal`):

```
schemify 1 journal
base <sha256 of the file bytes this journal applies to>
tx <seq> <unix_ms> <label>
  <op lines, one per op, same field/attr encoding as the document>
save <seq> <sha256 of written bytes>
```

- Every commit appends one `tx` (fsync batched at ≤ 100 ms). Every save appends `save`.
- **Open:** if the journal's last `save` hash (or `base`) equals the file's current hash and `tx` entries follow it, those are unsaved edits from a crash → offer recovery (replay them as a transaction group). If the hash doesn't match any `save`/`base`, the file changed outside Schemify → treat the journal's tail as a local edit set against the matching `save` and run the §4 merge.
- **Undo after restart:** transactions before the last `save` are loaded lazily as undo history (inverses recomputed by replaying from `base`), bounded by the stack capacity.
- **Compaction:** when the journal exceeds 2^16 transactions or 64 MiB, rewrite it as `base <current saved hash>` plus the last 2^12 transactions.

## 4. External changes

Files are watched (native: OS notifications; web: OPFS writes are always ours). On change of a file open in memory:

1. No unsaved local edits → reload silently (new `base`).
2. Unsaved local edits → `format::merge3(O = last loaded/saved bytes, A = canonical write of the in-memory document, B = new disk bytes)`:
   - clean → load the merged result; local edits remain unsaved (dirty); undo history is rebased (inverses recomputed against the merged state; entries that no longer apply are dropped with a notice);
   - conflicts → the document enters *conflict state*: conflicting records are listed, both versions are shown, and the user picks per record (or edits); saving is blocked until resolved.

The same `merge3` backs `schemify merge-driver` (D34), so git merges and live external edits behave identically.

## 5. Saves

`save` writes the canonical form (format spec §2), atomically (write temp + rename; on the web, OPFS write then swap), then appends `save` to the journal. A save never rewrites files that are not dirty.
