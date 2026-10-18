---
name: schemify-ui-loop
description: The edit → render → critique → bless loop for Schemify's UI look. Use when changing `ui.records`, `keymap.records`, canvas colours, toolbar icons, spacing or any visual part of the default UI plugin, or when asked to show how the editor looks now.
---

One **pass** = one visual change seen in a render, judged against `analog-eda-ux`, then blessed. Load `analog-eda-ux` (and `schemify-theme` for any colour) before step 1.

## Where the look lives

- `ui/default-plugin/ui.records`: every node and `style` row of the shell. Style props are the `StyleProp` enum in `crates/ui/src/tables.rs` (colours `rrggbb[aa]`, lengths in logical px). An unknown prop rejects the whole contribution, so check the enum first.
- Canvas colours: `style canvas.<theme>.<layer>` (`fg` only; layer 0 = background), plus `.grid` and `.sel`, at the bottom of `ui.records`.
- Toolbar icons: the `icon` prop, SVG path data in a 16 × 16 box (`crates/ui/src/icon.rs`); the label stays the accessible name.
- `ui/default-plugin/keymap.records`: keys. D25: xschem's bindings are the default. Rebinding an existing key is a decision for the user, not a style change.
- A look that breaks xschem parity (D6) goes in a **separate preset contribution** (later plugin, last wins, D8), not in the default set.

## Steps

1. **State the pass.** One sentence: what changes, which selectors, and which `analog-eda-ux` rule it serves. Done when the sentence names the selectors.
2. **Edit the records.** Change only the selectors you named. Done when `git diff --stat` shows only those files.
3. **Rebuild the guest** (the records are `include_str!`'d into the wasm):
   `systemd-run --user --scope -q -p MemoryMax=6G -p MemorySwapMax=0 nix develop -c ui/build.sh`
   Done when `crates/app/assets/default_ui.wasm` has changed and the script exits 0. Run one heavy job at a time (docs/dev/contributing.md).
4. **Render.** `SCHEMIFY_BLESS=1 cargo test -p schemify-app --test shell golden_ -- --test-threads=4` (run it under the same memory cap). It writes `crates/app/tests/golden/*.png`. Add `--test docs golden_` when docs-pane styles changed. Done when the PNGs are rewritten.
5. **Look.** Read each changed golden with the Read tool, and the previous one with `git show HEAD:<path> > <scratchpad>/old.png`. Critique the pair against every rule in `analog-eda-ux` § Checklist and write the verdict as one line per rule: pass, fail, or n/a. Done when every checklist rule has a verdict. A fail sends you back to step 2.
6. **Prove it holds.** If any colour changed, run `python3 docs/dev/skills/schemify-theme/check_theme.py <records...>`; it must exit 0 for a new preset (the parity default fails by design). Run `cargo test -p schemify-app` without `SCHEMIFY_BLESS`; it must pass. Then `cargo clippy -p schemify-app -- -D warnings` if any Rust changed. Done when both are green.
7. **Report.** Give the before/after paths, the checklist verdicts, and the change radius (which selectors; parity kept or preset). Commit the `.records`, the `.wasm` and the PNGs together. Then `rm -rf target` if this is a worktree.

## Live view (optional)

The goldens are 1280 × 800 at one fixture. To see another state (dialog open, waves, light theme), run the app as described in docs/dev/contributing.md § Running the editor (cage + grim on NixOS).
