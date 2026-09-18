# pollard

[![crates.io](https://img.shields.io/crates/v/pollard-cli.svg)](https://crates.io/crates/pollard-cli)
[![PyPI](https://img.shields.io/pypi/v/pollard-vcs.svg)](https://pypi.org/project/pollard-vcs/)
[![CI](https://github.com/thinhngo-x/pollard/actions/workflows/ci.yml/badge.svg)](https://github.com/thinhngo-x/pollard/actions/workflows/ci.yml)

**Version control for deep-learning experiments, where history is a tree of runs.**

Every `pollard run` snapshots your code, config, data and environment into an immutable node, then launches your script. You fork from any node, change something, run again, and compare the children side by side. Nothing to stage, no branches to name, no merges, no commit messages required. Git remains your collaboration layer: pollard imports from it and exports clean linear branches back to it.

```
quiet-elm-1  done  import from git
└─ warm-fox-2  done  baseline                     val_loss 2.31
   ├─ cold-owl-3  pruned  lr 3e-4→1e-4
   ├─ red-ant-4  done  deeper + attn              val_loss 2.19
   │  └─ blue-elk-5  killed  combine
   │     └─ gold-fin-6 @30000  done  resume, lower lr   val_loss 0.91 ★
   └─ sweep:seeds  16 runs                        val_loss 0.90 ± 0.02
```

> **Alpha (`0.1.0-alpha.1`).** Linux x86_64 only. The CLI, storage layout and node schema may change before 1.0, so pin an exact version.

## Install

```sh
uv tool install pollard-vcs    # the PyPI name is pollard-vcs; the import is still `import pollard`
# or
cargo install pollard-cli
```

Both install two binaries, `pollard` and its short alias `po`. The examples below use `po`.

## Quickstart

In an existing git project with a `train.py` and a `config.yaml`:

```sh
po init --from-git               # root node from git HEAD; adds .pollard/ to .gitignore
po run -m "baseline" train.py    # snapshot, then launch (as `uv run train.py` in a uv project)
po tree
```

Your script needs no library. `run` sets two environment variables, and the script writes to them:

```python
import json, os

metrics = open(os.environ["POLLARD_METRICS"], "a")
metrics.write(json.dumps({"step": 1000, "val_loss": 2.31}) + "\n"); metrics.flush()
torch.save(model.state_dict(), f"{os.environ['POLLARD_CKPT_DIR']}/step1000.pt")
```

Or use the optional pure-Python SDK:

```python
import pollard

run = pollard.current()
run.log({"val_loss": 2.31}, step=1000)
run.save_checkpoint("ckpt/step1000.pt")
```

## Workflows

### Try ideas and compare them

```sh
po fork warm-fox-2                       # working copy := that node's code and config
sed -i 's/lr: 3e-4/lr: 1e-4/' config.yaml
po run train.py                          # no -m: the note is generated, "lr 3e-4→1e-4"

po fork warm-fox-2
$EDITOR model.py
po run -m "deeper + attn" train.py

po siblings warm-fox-2 --metric val_loss
```

```
                warm-fox    cold-owl    red-ant
lr              3e-4→1e-4   —           3e-4→1e-4
model.depth     —           12→24       12→24
code            —           +attn.py    +attn.py
val_loss@10k    2.31(−.04)  2.28(−.07)  2.19(−.16)
status          done        killed      done
```

Each child is diffed against the shared parent, so every row shows what that child changed, and metrics are compared at the last step all children reached.

### Combine two ideas, without a merge

```sh
po fork red-ant-4
po apply cold-owl-3        # patch cold-owl-3's change into the working copy
po run -m "combine" train.py
po prune cold-owl-3        # its checkpoints are freed at the next `po gc`
po undo                    # changed your mind: every mutating command can be undone
```

### Resume from a checkpoint

```sh
po show blue-elk-5                     # status, last step, checkpoints, how to reproduce
po fork blue-elk-5 --step 30000        # restores ckpt/step30000.pt, sets POLLARD_FORK_STEP
sed -i 's/lr: 1e-4/lr: 5e-5/' config.yaml
po run -m "resume, lower lr" train.py
po log gold-fin-6 --key val_loss       # one continuous curve: 0–30k inherited, then its own
```

Your script reads `POLLARD_FORK_STEP` to know where to resume.

### Sweeps

```sh
for s in $(seq 1 16); do po run --sweep seeds -m "seed $s" train.py seed=$s; done
po tree --metric val_loss              # the sweep is one row, with mean ± std
```

Runs that differ only in `seed` / `random_seed` also collapse into one `N seeds` column in `siblings`.

### Share and publish

```sh
po pin gold-fin-6 paper-v1                                  # a name for a node
po push                                                     # sync to the remote (see config below)
po export --path quiet-elm-1..paper-v1 --branch paper-v1    # one git commit per node; HEAD untouched
```

A collaborator can `git checkout paper-v1 && uv sync --frozen` and reproduce the result without ever using pollard.

## Referring to nodes

Anywhere a command takes a node, you can pass a full id (`warm-fox-2`), a unique prefix (`warm`), a pin name (`paper-v1`), `@` for the current node, or `@-` for its parent.

## Commands

| Task | Commands |
|---|---|
| Record and launch | `run`, `ckpt`, `artifact` |
| Move around | `fork`, `fork --step N`, `apply` |
| Inspect | `tree`, `siblings`, `diff`, `show`, `log` |
| Annotate | `note`, `pin`, `unpin` |
| Clean up | `prune`, `gc` |
| Recover | `undo`, `op log` |
| Git | `init --from-git`, `import`, `export` |
| Sync | `push`, `pull` |

`po <command> --help` has the details. Every mutating command prints the id of the node it created or changed on its last line, so scripts can capture it.

## Run protocol

`po run` sets these for the launched process, whatever the language:

| Variable | Meaning |
|---|---|
| `POLLARD_NODE_ID` | The node this process is |
| `POLLARD_METRICS` | JSONL file to append `{"step": int, "<key>": number}` lines to; ingested live |
| `POLLARD_CKPT_DIR` | Directory for checkpoints; files are deduplicated and stored at run end |
| `POLLARD_FORK_STEP` | Step to resume from, when forked with `--step` |
| `POLLARD_CONFIG` | Where to write the resolved config as JSON, in `sdk` capture mode |

## Configuration

`.pollard/config.toml`, created by `po init`:

```toml
# data = ["./data", "s3://bucket/prefix"]   # dataset roots, hashed into each recipe
offtree = ["*.md", "notes/"]                # snapshotted for reference, never a code change
output_dirs = ["outputs/"]                  # script outputs, not code
checkpoint_dir = "ckpt/"
seed_keys = ["seed", "random_seed"]         # collapsed together in `siblings`
# config_capture = "file:config.yaml"       # or "hydra", "sdk"
# primary_metric = "val_loss"
# remote = "/path/or/s3://bucket/prefix"    # for push/pull
```

## How it works

- A node's **recipe** is four hashes: code, config, data, env (from `uv.lock`). Recipes are never deleted, so any outcome can be rebuilt. Re-running a recipe that already has a running or finished node is refused unless you pass `--force`.
- A node's **outcome** (checkpoints, metrics, status, note) can be pruned. Checkpoints are stored with content-defined chunking, so siblings that share most of their weights share most of their storage.
- The code hash is a git tree hash, so any node's code maps straight onto a git tree for export.
- Everything lives in `.pollard/`: a SQLite database plus a content-addressed object store.

The full design is in [`SPEC.md`](SPEC.md), and the decisions behind it in [`docs/DECISIONS.md`](docs/DECISIONS.md).

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). Planned work is tracked in [issues](https://github.com/thinhngo-x/pollard/issues).

## License

MIT OR Apache-2.0.
