# Agents

Read [docs/dev/contributing.md](docs/dev/contributing.md) before any change: it holds the rules for agents and people alike (the code's shape and layers, the order of a change, hard rules, tests, builds). `CLAUDE.md` imports the same file.

## Skills to load first

Load each with the Skill tool (other agents: read
`~/.claude/skills/<name>/SKILL.md` and the companion files it names). Source:
https://github.com/OmarSiwy/NixOSConfig/tree/main/skills

| Skill                  | Use it for here                                                                                                                   |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `data-oriented-design` | `CircuitIR` (`src/ir`), the import AST and result tables: the six questions, SoA, narrowest widths, handles                       |
| `codebase-clean-code`  | Owners: `import` reads, `ir` holds, `codegen` writes one dialect each, `backend` runs one simulator each; one spine per direction |
| `api-design`           | Every `pub` item and the Python API (PySpice-close, backend neutral): usage code first, caller owns memory and flow               |
| `code-style`           | File order, ≤500 code lines per file, `check_style.py`                                                                            |
| `simd-loops`           | Hot loops over raw-file columns (`src/rawfile.rs`): the plain-loop forms, or explicit SIMD with a scalar oracle                   |
| `doc-comments`         | Every `pub` item                                                                                                                  |
| `ponytail` (ultra)     | How little code; never what shape the data takes                                                                                  |
| `assertions`           | Contracts, capacities, bounded loops                                                                                              |
| `test-plan`, `tdd`     | The oracles below, the gates, a feature's definition of done                                                                      |

Language:

| Skill  | Use it for here                                                                                                                    |
| ------ | ---------------------------------------------------------------------------------------------------------------------------------- |
| `rust` | Types, memory, `zero-cost.md`: an abstraction enters a hot loop only with an `asm_probe` verdict; explicit SIMD is `fearless_simd` |
