# Versioning

What may change between Schemify releases, for the three contracts plugin and theme authors build on. Each has its own version number, separate from the app's: an app release that changes none of them breaks nothing that was built for them.

| Contract | Where its version is | Today |
|---|---|---|
| Plugin ABI | `schemify_abi() -> u32`, exported by every plugin ([plugin-abi.md](../spec/plugin-abi.md) §2) | 1 |
| File format | line 1 of every file, `schemify <version> <kind>` ([format.md](../spec/format.md) §2) | 1 |
| Lua API | not numbered yet; it becomes `schemify.version.api`, readable from a config | 1 |

## Within a version: additions only

A change that keeps the version number only adds. Code written for the version keeps working, unchanged and without a rebuild.

- **Plugin ABI.** New events, new snapshot tables, new columns appended to a table (column ids never move, §4), new contribution kinds, new output records, new error codes, new grants. A plugin ignores an event, record or column it does not know. Nothing is removed and no existing meaning changes. A new event that older guests would not parse is sent only to plugins that opt in by using what came with it (`state_changed` only after a `state` record). Each addition is dated in [plugin-abi.md](../spec/plugin-abi.md) §11; a plugin that uses one needs a host from that date (ABI 1 additions so far: `fs_list` and `read:<NAME>` 2026-10-22, the session-state tables, `state`, `command`, `pointer` and mouse chords 2026-11-06).
- **File format.** New attributes on existing records. A reader keeps attributes it does not know, verbatim and in order (§5), so a file written by a newer Schemify round-trips through an older one. A new record kind is not an addition here: an older reader rejects an unknown record (`E027`) unless the file's header version is newer than its own, so a new record kind raises the version.
- **Lua API.** New functions, options, events and fields. A config that uses one on an older release gets an error at that line, which stops the rest of that file ([config.md](../spec/config.md) §4); a config shared across releases checks `schemify.version.api` first.

## A new version

A change that removes something or changes what something means raises that contract's version.

- **Plugin ABI.** The app loads plugins built for the current version and the one before it, for at least 12 months after the new version ships. Today the host loads only an exact match (`E_VERSION`); it gains the second version the first time one exists.
- **File format.** Readers read every older version. Writers write the current one; opening and saving an older file upgrades it, and the save says so. An older reader opens a file from a newer version and keeps every record it does not know, verbatim and in place (§5).
- **Lua API.** A removed function or option keeps a stub for two minor releases that fails with a message naming its replacement.

## Deprecation

Before anything is removed in a new version:

1. It is marked deprecated in the spec and the SDK docs, with its replacement.
2. Using it writes a line to the health report (once per session per plugin), so authors see it before users do.
3. It stays for at least two app releases and six months, whichever is longer.

## Declaring compatibility

A package's manifest states the versions it was built for: `engine abi 1`, `engine format 1`, `engine lua 1` (any omitted means "any"). The store shows a package only to apps that support all of them, and the app refuses to load one it cannot support with a message that names the version it needs.

## How this is kept honest

- Each SDK's conformance suite (`crates/protocol/abi/sdk/CONFORMANCE.md`) runs against every release, and a release that fails one for the current ABI version does not ship.
- CI keeps example files of every past format version and checks that each still opens and round-trips.
- The example plugins in `examples/plugins/` are built against the previous release's SDK and loaded by the new app in CI.
