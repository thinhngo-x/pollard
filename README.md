# pollard

A version-control tool for deep-learning experiments. Its history is a **tree of runs**, not a git graph.

Every training launch becomes an immutable node with one parent. There are no branches, no staging area, no merges, and no required commit messages. You fork, try, prune, and fork again. Git stays your collaboration layer: pollard imports from it and exports linear branches back to it.

> **Status: alpha (`0.1.0-alpha.1`).** All milestones M1–M9 are implemented and tested (see the command-status table below and `CHANGELOG.md`). The CLI, storage layout, node schema, and Python SDK may still change before `1.0`; pin an exact version if you depend on them. The design is in `SPEC.md`, and decisions are recorded in `docs/DECISIONS.md`.

## Concepts in one breath

- **Node** = one run. **Recipe** `(code, config, data, env)` is hashed and never deleted. **Outcome** (weights, metrics, status, note) can be pruned and reproduced from the recipe.
- **Siblings** are compared against their shared parent, never against each other, so `pollard siblings` shows exactly what each child changed and how it did.
- **Off-tree files** (`*.md`, `notes/`) are snapshotted for reference but never count as a code change.
- **`undo`** reverses any mutating command, using an op log in the style of Jujutsu.
- `po` is installed alongside `pollard` as a short alias.

## Install

```sh
uv tool install pollard-vcs     # PyPI name (`pollard` is an unrelated project); still `import pollard`
cargo install pollard-cli       # or build from source: installs `pollard` and `po`
```

## Quickstart

### First day on an existing git project

```sh
pollard init --from-git              # [M1 init; M6 --from-git] root node from git HEAD
pollard run -m "baseline" train.py   # [M1; M5 for `uv run` launch] snapshot, then launch
pollard tree                         # [M1]
```

A training script needs no library. `pollard run` sets `POLLARD_METRICS` (a JSONL file to append `{"step": …, "val_loss": …}` lines to) and `POLLARD_CKPT_DIR` (a directory to write checkpoints into) [M3/M4]. The optional Python SDK wraps this in three lines [M9]:

```python
import pollard
run = pollard.current()
run.log({"val_loss": 2.31}, step=10_000)
```

### The exploration loop

```sh
pollard fork warm-fox-2                        # [M1] working copy := that node's recipe
sed -i 's/lr: 3e-4/lr: 1e-4/' config.yaml
pollard run train.py                           # no -m → auto note "lr 3e-4→1e-4"

pollard fork warm-fox-2
$EDITOR model.py REPORT.md                     # REPORT.md is off-tree: ignored by the recipe
pollard run -m "deeper + attn" train.py

pollard siblings warm-fox-2 --metric val_loss  # [M2 config/code rows; M3 metric rows]
```

```
                warm-fox    cold-owl    red-ant
lr              3e-4→1e-4   —           3e-4→1e-4
model.depth     —           12→24       12→24
code            —           +attn.py    +attn.py
val_loss@10k    2.31(−.04)  2.28(−.07)  2.19(−.16)
status          done        killed      done
```

Combine two ideas without a merge by forking one and applying the other's change:

```sh
pollard fork red-ant-4
pollard apply cold-owl-3                       # [M8] patch cold-owl-3's code delta in
pollard run -m "combine" train.py
pollard prune cold-owl-3                       # [M4] checkpoint freed at next `gc`
pollard undo                                   # [M1] changed your mind
```

## Command status

All milestones below shipped in `0.1.0-alpha.1`. The milestone column is kept for
traceability back to `docs/PLAN.md` and `SPEC.md` §9.

| Command | Milestone |
|---|---|
| `init`, `run`, `tree`, `show`, `note`, `undo`, `op log`, `fork` | M1 |
| `diff`, `siblings` | M2 |
| `log`, run protocol env vars | M3 |
| `ckpt`, `artifact`, `fork --step`, `prune`, `gc` | M4 |
| uv integration (`uv run` launch, env hash, `--strict`) | M5 |
| `import`, `export`, `init --from-git` | M6 |
| `push`, `pull` | M7 |
| `run --sweep`, `pin`, `unpin`, `apply`, `tree --metric` | M8 |
| Python SDK, wheel | M9 |

## License

MIT OR Apache-2.0.
