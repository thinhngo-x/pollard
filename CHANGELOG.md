# Changelog

## Unreleased (0.2.0-alpha.1)

### Upgrade notes

This release changes the repo and remote format ("format 2"). The upgrade is one-way.

| Area | What changes | What to do |
|---|---|---|
| Repo (`.pollard/`) | The first command after upgrading migrates the repo and prints one notice. The database moves to `.pollard/state.sqlite`; `.pollard/db.sqlite` becomes a stub directory so 0.1.0-alpha.1 fails instead of misreading it. The old database is kept at `.pollard/backup/db-format1.sqlite` (an existing backup is never overwritten; a new one gets a `-<unix seconds>` suffix). | Nothing. Keep the backup until you are satisfied. |
| Old binaries | pollard 0.1.0-alpha.1 exits with an error, and writes nothing, on a migrated repo or a converted remote. | Upgrade every machine and CI image that touches the repo or remote. |
| Pruned nodes | `pruned` is no longer a status but a flag: a pruned node keeps its `done`/`failed`/`killed` status, read back from the op log. When the op log has no record (e.g. a node pulled already pruned), the status is guessed (`done` if the run finished, else `killed`) and the notice lists those ids. Runs started under a pruned node after the prune are no longer hidden in `po tree`. | Review `po tree` once, and re-prune if needed. |
| Op log | `po undo` does not cross the upgrade: a request that would reach an op from before it is refused whole. | Undo anything you want undone **before** upgrading. |
| Pins | Named pins are unchanged. | None. |
| Remote | The first `po push` from 0.2 converts the remote (`FORMAT`, one new `nodes/<ts>-<salt>-<n>.jsonl` segment per push) and re-sends every node record once (KBs per node, no weights). 0.2 still reads unconverted remotes. | **Everyone on a shared remote upgrades together.** After the conversion, 0.1.0-alpha.1 `push`/`pull` fail. |

**Rolling back** to 0.1.0-alpha.1 (from the repo root), then reinstall 0.1.0-alpha.1:

```sh
rm -r .pollard/db.sqlite .pollard/state.sqlite*
mv .pollard/backup/db-format1.sqlite .pollard/db.sqlite
```

Changes made after the upgrade are lost by a rollback. A converted remote cannot be rolled back.

### Format 2 (phase 1)
- Repo format version in `PRAGMA user_version`; forward migrations run on open under `.pollard/lock`, in one transaction, on a temporary copy renamed into place, so an interrupted upgrade is finished by the next command. A repo or remote written by a newer format is refused with "Upgrade pollard." and left untouched.
- Node record: `pruned_at` timestamp replaces the `pruned` status. `po show` prints `status` and `pruned <time>` separately; `po tree --all` marks pruned nodes `done (pruned)`. An unknown status in the database or in a remote line is an error naming the node and the value. `--json` outputs never carry `pruned` as a status.
- `po prune` keeps its subtree behaviour but sets the flag; already-pruned nodes keep their prune time.
- `po tree`: a pruned node with unpruned descendants is shown as a placeholder with them, so no live run is hidden. Fully pruned subtrees stay hidden unless `--all`.
- Duplicate check: a pruned match only prints a notice (`same recipe as <id> (done, pruned)`), as before.
- Pins are stored as `(node_id, name)` with an optional name (storage only; commands unchanged).
- Remote: per-push segment files fix the concurrent-push race (two pushes could drop one another's node lines). Pin changes are one record each, so concurrent pin edits from two clones both survive; only two edits of the same pin name are last-writer-wins. `pull` downloads only segments it has not read.

## 0.1.0-alpha.1 — 2026-09-18

First alpha release. All nine milestones (M1–M9) are implemented with green tests
(`d7c54d8`): node model and op log, deltas/diff/siblings, run protocol and metrics,
CDC-chunked object storage, uv integration, git import/export, S3-compatible remote
sync, sweeps/pins/apply, and the pure-Python SDK with wheel bundling. See the milestone
entries below for detail. Published as crates `pollard-core`/`pollard-objects`/
`pollard-git`/`pollard-remote`/`pollard-cli` (binaries `pollard`, `po`) and PyPI
distribution `pollard-vcs` (import `pollard`).

### M1 (lead): node model, store, op log — green
- `pollard-core`: `Repo` (`.pollard/` layout, `config.toml` defaults, SQLite WAL, `.pollard/lock` writer lock), node schema (§3 + `sweep`, `note_auto`), `code_trees`, `ops` with `wc_snapshot`, per-clone salted ids `adj-noun-N`.
- Commands: `init`, `run` (code/config/data/env/docs snapshot, duplicate check vs running/done, `-m`/`-m -`/auto note), `tree`, `show`, `note [-e]`, plain `fork`, `undo [n]` (itself an op; restores `@` and the working copy), `op log`. Node refs: id, prefix, pin, `@`, `@-`. `po` alias binary.
- `m1_core.rs`: 26/26 pass.

### M3 (lead): run protocol and metrics — green
- Env vars `POLLARD_NODE_ID`/`FORK_STEP`/`METRICS`/`CKPT_DIR`/`CONFIG`; JSONL tailed every 200 ms (whole lines, remainder on exit, malformed lines warned); `sdk`/`hydra` config capture; `key=value` launch args become config overrides.
- Metric inheritance through `fork_step` links, fork-step monotonicity re-parent (acts as forking the ancestor), `log`, `last_common`/`last_own` rows in `siblings`.
- `m3_metrics.rs`: 13/13 pass.

### M2 (lead): deltas, diff, siblings — green
- Parent-relative deltas stored at creation; `diff` (sibling view or direct, text diffs, `--docs`); `siblings`/`sib` table (blank rows dropped, >6 columns transposed, pins as headers); `--json` is `{column: {row: cell|null}}` (pandas `read_json` orient), frozen from here on.
- `offtree` uses gitignore semantics incl. `!README.md` (walk support from senior dev). `m2_siblings.rs`: 16/16.

### M5 (lead): uv integration — green
- Env hash from `uv.lock` (else `uv pip freeze`) + `.python-version` + `requires-python` + CUDA string (+ PEP 723 block); `x.py` launches as `uv run x.py` in uv projects, else `python`/`python3`; `uv lock --check` → `lock_ok`, warning, `--strict` refusal; `fork` runs `uv sync --frozen` unless `--no-sync`; env row in `siblings`; `show` prints the reproduce one-liner. `m5_uv.rs`: 9/9.

### M4 (senior): CDC chunking, weights, ckpt/artifact, prune, gc — green
- `pollard-objects`: content-defined chunking via `fastcdc` (gear hash, 8/64/128 KiB bounds), chunk map + refcounts, `put_file`/`restore_file` switch to chunking at 1 MB.
- `ckpt`/`artifact` register outputs from `POLLARD_CKPT_DIR`; `fork --step` restores the checkpoint at or below the requested step with a notice on inexact match; `prune [--keep-weights]` and `gc [--auto]` (point of no return — `undo` of a `prune` after `gc` warns weights are gone).
- `m4_objects.rs`: 16/16 pass (incl. ≥95% chunk sharing on a 1% in-place edit, CDC boundary stability).

### M6 (senior): git interop — green
- `pollard-git`: tree hashing via `gix`; `import <git-rev>` (root node, `code` = commit tree hash after code-manifest rules); `init --from-git` = `import HEAD`; `export <node> [--branch|--path a..b]` (single or linear multi-commit, generated messages, `.pollard-recipe.json`, off-tree files included); HEAD and the git index are never touched outside `export`'s own branch ref.
- `m6_git.rs`: 7/7 pass.

### M7 (senior): remote sync — green
- `pollard-remote`: local-path and S3-compatible remotes (`object_store`), layout mirrors `.pollard/` (`objects/`, `packs/` ≤64 MB, `nodes.jsonl` append-only, `pins.json`); `push`/`pull` transfer only missing objects/packs; salted per-clone ids collide at ~1 in 4M per same-counter pair, and `pull` refuses a same-id/different-recipe collision, naming both.
- `m7_remote.rs`: 5/5 pass (incl. two-clone push/pull with zero-byte second push).

### M8 (lead): sweeps, seeds, apply, pins, tree — green
- `run --sweep` (members stay siblings), one `sweep:<name>` row in `tree` and one column in `siblings` with mean ± std; seed collapse via `seed_keys`; `--expand-sweeps`.
- `apply`: three-way per file (line-wise for in-place edits, diff3 otherwise), conflict markers + warning, exit 0; undoable.
- `pin`/`unpin`, pins as node refs and sibling headers; `tree` hides pruned and collapses failed subtrees unless `--all`, `--metric` value per row and ★ best path.
- Also wired senior-dev commands: `ckpt`, `artifact`, `prune`, `gc`, `import`, `init --from-git` (with Journey A summary line), `export`, `push`, `pull`, `note --append`.
- Ids now use the D-4 word lists (1,000 adjectives × 1,000 nouns, `pollard-core/src/words/`).
- `m8_sweeps.rs` 12/12, journeys 5/5, cross-cutting 6/6 (incl. 200 ms on 1,000 nodes).

### M9 (senior): Python SDK, config adapters, wheel — green
- `python/pollard`: pure-Python, no compiled extension — `current()`, `Run.log`/`save_checkpoint`/`set_config`, `fork_step()`, `siblings()` (DataFrame when pandas is present); config-capture adapters `hydra`/`file`/`sdk`; `wandb`/`neptune` forwarders as optional extras (`_forward.py`).
- Wheel bundles the `pollard`/`po` binaries via `maturin` (`bindings = "bin"`), published as PyPI distribution `pollard-vcs`, `import pollard`.
- `m9_python.rs`: 7/7 pass (SDK and plain-file-I/O variants produce the same metrics/checkpoint).

### Docs
- Added `docs/PLAN.md`: per-milestone checklist with owners, cross-owner contracts, and §9 acceptance items.
- Added `docs/DECISIONS.md`: proposed defaults for the §11 open questions, name-availability results, and spec ambiguities, now all DECIDED.
- `README.md`: alpha disclaimer added; command-status table reflects M1–M9 shipped; install lines use `pollard-vcs`.
- Added `CONTRIBUTING.md`: build/test commands, crate layout, pointer to `SPEC.md`.
- Added `docs/RELEASE.md`: alpha release checklist, version scheme, publish order, dry-run-only publish steps.
- Added `LICENSE-MIT` and `LICENSE-APACHE`, matching `license = "MIT OR Apache-2.0"` in every crate and in `python/pyproject.toml`.
- `SPEC.md` is now v3. All §11 open questions and the planning ambiguities are decided; the "Revision log (v2 → v3)" at the end of the spec lists every change.

## Planned
- Next version (not v1): tag-mode and stat-mode data roots for cheaper data hashing (SPEC §12, DECISIONS D-30).
