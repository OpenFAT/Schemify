# Publishing a plugin

## What users install

`schemify pack` installs a git repository by cloning it and loading the module at its root ([config.md](../spec/config.md) §5). There are no build hooks: the committed `.wasm` is what runs. So a release is a commit whose module is up to date, and CI's reproducibility check ([testing.md](testing.md)) guarantees that every commit on `main` is one.

## Releases

Tag a release with a version (`git tag v0.3.0 && git push --tags`). Users pin it with `version`, or follow a branch with `rev`:

```lua
-- init.lua
schemify.pack.add {
  { src = "https://github.com/OmarSiwy/GmIDVisualizer", version = "v0.3.0",
    grant = { "read_project", "write_project", "spawn:ngspice" } },
}
```

`schemify pack sync` clones it and records the commit and the module's sha256 in `schemify-pack.lock`. `schemify pack update` moves to the newest commit of `rev`/`version`. The user grants capabilities; the plugin gets the intersection of those and what its `schemify.project` requests.

A project that needs the plugin (a docs page that calls its functions, a PDK flow) declares it in its own `schemify.project` with a `plugin` record and the module copied into the project, so the design works for anyone who opens it.

Users who cannot use git can install a single module with `url` and `sha256`. Attach the `.wasm` to a GitHub release for that.

## Moving to a newer Schemify

1. Pick the Schemify commit (usually `main` when the nightly is green).
2. Replace the `rev` of every Schemify dependency in `Cargo.toml` with its full sha, all to the same commit.
3. `cargo update -p schemify-abi` (and every other Schemify crate you use), then `./build.sh` and `cargo test`.
4. Commit `Cargo.toml`, `Cargo.lock` and the rebuilt `.wasm` together.

The module exports the ABI version it was built for (`schemify_abi`, [plugin-abi.md](../spec/plugin-abi.md) §2). An editor with another ABI version refuses to load it (`E_VERSION`) instead of running it with the wrong layout.

## Listing a plugin in Schemify

Schemify keeps a few plugins as examples under `examples/plugins/` (git submodules). Adding one is a pull request to Schemify that runs `git submodule add <url> examples/plugins/<name>` and adds a row to [README.md](README.md). Schemify's CI does not build or test submodules; the plugin's own CI is what vouches for it.
