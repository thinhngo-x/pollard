# Changelog

## Unreleased

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

### M8 (lead): sweeps, seeds, apply, pins, tree — green
- `run --sweep` (members stay siblings), one `sweep:<name>` row in `tree` and one column in `siblings` with mean ± std; seed collapse via `seed_keys`; `--expand-sweeps`.
- `apply`: three-way per file (line-wise for in-place edits, diff3 otherwise), conflict markers + warning, exit 0; undoable.
- `pin`/`unpin`, pins as node refs and sibling headers; `tree` hides pruned and collapses failed subtrees unless `--all`, `--metric` value per row and ★ best path.
- Also wired senior-dev commands: `ckpt`, `artifact`, `prune`, `gc`, `import`, `init --from-git` (with Journey A summary line), `export`, `push`, `pull`, `note --append`.
- Ids now use the D-4 word lists (1,000 adjectives × 1,000 nouns, `pollard-core/src/words/`).
- `m8_sweeps.rs` 12/12, journeys 5/5, cross-cutting 6/6 (incl. 200 ms on 1,000 nodes).

### Docs
- Added `docs/PLAN.md`: per-milestone checklist with owners, cross-owner contracts, and §9 acceptance items.
- Added `docs/DECISIONS.md`: proposed defaults for the §11 open questions, name-availability results, and spec ambiguities awaiting owner sign-off.
- Added `README.md`: overview and quickstart. All commands are marked planned.
- `SPEC.md` is now v3. All §11 open questions and the planning ambiguities are decided; the "Revision log (v2 → v3)" at the end of the spec lists every change. `docs/DECISIONS.md` marks all entries DECIDED, and `docs/PLAN.md` is updated to match.

## Planned
- Next version (not v1): tag-mode and stat-mode data roots for cheaper data hashing (SPEC §12, DECISIONS D-30).
