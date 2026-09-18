# pollard delivery plan

Source of truth: `SPEC.md` **v3**. All decisions are closed and recorded in `docs/DECISIONS.md` (D-1…D-26).
Rule (§9): ship milestones in order. Each one closes only when its acceptance items are green and CHANGELOG has an entry. Do not start the next milestone while any test is failing.

Owners: **LEAD** (lead dev), **SR** (senior dev), **TEST** (tester), **PM**.
Arrows `SR → LEAD` mean LEAD is blocked until SR delivers.

---

## Cross-owner contracts (agree before M1 coding starts)

### C1. `pollard-objects` Tier-1 API (SR → LEAD, needed in week 1 of M1)

LEAD needs this for M1 snapshots, working-copy restore, and `undo`. Signatures are proposals; SR owns the final shape. Freeze it by M1 mid-point.

- [ ] `ObjectStore::open(repo_dot_pollard: &Path) -> Result<ObjectStore>`: creates `objects/` and `chunks/` if missing.
- [ ] `put(&self, bytes: &[u8], kind: ObjectKind) -> Result<Hash>`: blake3, zstd level 3, idempotent. Hash is lowercase blake3 hex.
- [ ] `get(&self, h: &Hash) -> Result<Vec<u8>>` and `has(&self, h: &Hash) -> bool`.
- [ ] `put_file(&self, path: &Path) -> Result<Hash>` / `restore_file(&self, h: &Hash, dest: &Path) -> Result<()>`: whole object when under 1 MB, chunk map at 1 MB or more. **In M1 the chunk-map branch can store the file whole behind the same API.** M4 changes the internals only, so the caller does not change.
- [ ] `Manifest { entries: Vec<Entry { path, size, hash, mode }> }` with `to_bytes()` in canonical sorted-line form, `hash()`, `from_bytes()`, and `diff(&a, &b) -> Vec<(path, Added|Removed|Modified)>`. The same format is used for code, data, weights, and docs.
- [ ] Error type uses `thiserror`, and every error names the hash or path involved.
- [ ] Refcount hooks (`incref`/`decref` on a manifest) stub in M1; real behavior in M4. The `objects`/`chunks` SQLite tables live in `db.sqlite` owned by core, so decide who owns those tables: **proposal: core owns the schema; objects receives a `&rusqlite::Connection` or a small trait.** Decide at M1 kickoff.

### C2. Git tree hash for `code` (SR → LEAD, needed in M1)

§3 says `code` is a git tree hash. M1 stores `code`, so hashing cannot wait for M6.
- [ ] SR delivers `pollard_git::tree_hash(entries: &[(path, mode, &[u8] | blob_sha1)]) -> Result<String>` (or `tree_hash_of_dir(root, file_list)`) during M1. `import`/`export` stay in M6.
- [ ] LEAD stores `node.code` = the git tree hash, and adds a `code_trees(git_tree PK, manifest_hash)` row per node (D-13).

### C3. Directory walk and ignore rules (LEAD, used by SR in M4/M6)
- [ ] Single `pollard_core::walk::code_files(root, cfg)` that applies `.gitignore`, `.pollardignore`, `.pollard/`, `.git/`, off-tree globs, `output_dirs`, checkpoint dir, and the 10 MB rule. Reused by export (M6), not reimplemented.

### C4. CLI wiring (SR features → LEAD)
- [ ] SR exposes each feature as a `pollard-core` (or own-crate) function taking `&mut Repo` and returning `Result<OpRecord>` for mutating ops. LEAD wires the clap subcommand. Applies to `ckpt`, `artifact`, `fork --step`, `prune`, `gc`, `import`, `export`, `push`, `pull`.

### C5. Test harness (TEST, from M1)
- [ ] `crates/pollard-cli/tests/common`: temp repo builder, 200-file tree generator, fake 50 MB checkpoint generator, binary runner that captures the last-line node id.
- [ ] Timing assertions use generous CI margins and are measured in release mode. Record the raw numbers in `docs/TEST_REPORT.md`.

---

## M1: workspace, node model, SQLite, op log, `init`, `run` (code + config + notes), plain `fork`, git tree hash, Tier 1 store, `tree`, `show`, `note`, `undo`

Tasks
- [ ] LEAD: set `rust-version = "1.85"` under `[workspace.package]` and check that all dependencies build on 1.85 (D-11).
- [ ] LEAD: `Repo` open/init, `.pollard/` layout, `config.toml` load with defaults, WAL, file lock on `.pollard/lock`.
- [ ] LEAD: schema (§3 v3): `nodes` (+`depth`, +nullable `sweep`), `pins`, `ops` (+nullable `wc_snapshot`), `code_trees`, `objects`, `chunks`, `chunk_map`; empty `deltas` and `metrics` tables so later milestones need no migration.
- [ ] LEAD: id generator `<adj>-<noun>-<counter>`: per-clone sequential counter, word pair = blake3(clone_salt ‖ counter), 64-bit random `clone_salt` at init (D-24). Curated 1,000 adjective + 1,000 noun list, compiled in and append-only (D-4).
- [ ] LEAD: `init` (adds `.pollard/` to `.gitignore`), `run` (snapshot code and config, exec the command, set status on exit, print the node id first and last), duplicate `recipe_hash` check (refuse only if the match is `running`/`done`; notice for `failed`/`killed`/`pruned`; D-19) with `--force`, `-m` repeat/stdin, auto note flagged `auto`.
- [ ] LEAD: config capture `file:<path>` default (`file:config.yaml`), canonical JSON, blake3. `data`/`env` hash stubs are constant until M2/M5 (document the constant).
- [ ] LEAD: op log: every mutating command writes an op before it mutates; `undo [n]`, `op log`; node/pin snapshot blobs.
- [ ] LEAD: `tree` (parent links, note titles), `show`, `note [-e]`.
- [ ] LEAD: plain `fork <node>` (restore code+config, set current; save the working copy to `ops.wc_snapshot` first). Now in §9 M1 (D-12, D-17).
- [ ] LEAD: `undo` restores `wc_snapshot` for ops that wrote the working copy.
- [ ] LEAD: node argument resolution: full id, unique prefix, pin name, `@`, `@-`.
- [ ] SR: C1 and C2 delivered.
- [ ] TEST: C5 harness, plus the acceptance tests below.
- [ ] PM: CHANGELOG M1 entry.

Acceptance (§9 M1)
- [ ] Temp repo with 200 files: 3 consecutive `run`s each take < 1 s wall clock (excluding the child command).
- [ ] `tree` output shows each node's parent link and note title.
- [ ] A `run` with no `-m` gets a note flagged `auto`.
- [ ] `run` then `undo`: the node is gone from `tree`/`show`, and the working copy is byte-identical to its state before the run.
- [ ] Re-running the recipe of a `done` node is refused (non-zero exit, message names the existing node). With `--force` it succeeds. Re-running a `failed` node's recipe succeeds and prints a notice.
- [ ] `fork` with uncommitted edits, then `undo`: the edits are restored byte-identically.
- [ ] A node's `code` equals `git write-tree` over the same files.
- [ ] Every mutating command prints the node id on its last line.

## M2: deltas, `diff`, `siblings` (config/code rows), off-tree files, `output_dirs`

Tasks
- [ ] LEAD: `deltas` computed at `run` against the parent: config (dotted paths, arrays by index, `null` one-sided), code (manifest diff), data (+totals), env stub.
- [ ] LEAD: `offtree` globs (default `["*.md","notes/"]`), `docs` manifest snapshot at run, `diff --docs`.
- [ ] LEAD: `output_dirs` (default `outputs/`) auto-ignored. Warn when there are more than 500 new files or a new file over 10 MB.
- [ ] LEAD: data manifest for local roots with the `(root, mtime, size)` cache. Remote roots list `(key, size, etag)`. Hashing always blocks `run` (D-2).
- [ ] LEAD: the `siblings` code row and auto notes skip the captured config file, which stays in `code_delta` (D-20).
- [ ] LEAD: `diff <a> <b>`, `siblings [<node>] [--json]` with alignment rules §6 steps 2 and 4 (≤ 6 columns then transposed; pin names in headers).
- [ ] LEAD: freeze the `siblings --json` schema. It is public from M2 onward (§11). PM records it in CHANGELOG.
- [ ] TEST: hand-written expected sibling table fixture.

Acceptance (§9 M2)
- [ ] Parent + 5 children: `siblings` output equals the hand-written expected table.
- [ ] Rows that are blank for every child are absent.
- [ ] Editing an off-tree `.md` between two otherwise identical runs: no code delta, and the second run is still refused as a duplicate.
- [ ] `siblings --json` loads into a pandas DataFrame, and re-serialising gives the same data (round trip).
- [ ] `siblings` for 100 children takes < 50 ms.

## M3: run protocol, metrics, inheritance, fork-step rule, `log`, metric rows

Tasks
- [ ] LEAD: env vars `POLLARD_NODE_ID`, `POLLARD_FORK_STEP`, `POLLARD_METRICS`, `POLLARD_CKPT_DIR`, `POLLARD_CONFIG`.
- [ ] LEAD: JSONL tailing during the run, with remainder ingest on exit. Malformed lines produce a warning with file and line number and are skipped. Poll every 200 ms (D-9).
- [ ] LEAD: `sdk` config capture via `POLLARD_CONFIG` (read before the first metric line).
- [ ] LEAD: `metrics` table, inherited read through the parent chain (§5 tier 3), `log <node> [--key]`.
- [ ] LEAD: `fork --step N` records `fork_step` only (checkpoint restore comes in M4). Monotonicity re-parent with a one-line notice. The re-parented fork behaves as `fork <ancestor> --step N`, taking the ancestor's recipe (D-21).
- [ ] LEAD: metric rows in `siblings`: `last_common`, `last_own`, `value (±delta)`, key choice order `--metric` > `primary_metric` > alphabetical.
- [ ] TEST: shell-script trainer fixture that emits JSONL with sleeps.

Acceptance (§9 M3)
- [ ] A shell script echoing JSONL with delays has its metrics visible in `log` **while it is still running**.
- [ ] A logs steps 1–100; B = fork A `--step 100` logs 101–200; `log B` returns exactly 200 points.
- [ ] Forking B at step 50 creates a node whose parent is A, and one notice line is printed.
- [ ] `last_common` and `last_own` cells match hand-computed values, including when a child failed.

## M4: CDC chunking, weights manifests, `ckpt`, `artifact`, `fork --step` restore, `prune`, `gc`

Tasks
- [ ] SR: CDC in `pollard-objects` via `fastcdc` 5.0.0 `v2020::StreamCDC::new(r, 8 KiB, 64 KiB, 128 KiB)` (D-10).
- [ ] SR: `put_file` switches to a chunk map for files of 1 MB or more (API unchanged from C1). Maintain `chunks` refcounts and `chunk_map`.
- [ ] SR: weights manifest; `ckpt <path>`, `artifact <path>`; register everything in `POLLARD_CKPT_DIR` at run end.
- [ ] SR: `fork --step N` restores the checkpoint for step N into `POLLARD_CKPT_DIR`. Step = last digit run in the file name. Search the node, then ancestors by the metric inheritance rule. With no exact match, use the largest step ≤ N, set `fork_step` to it, and print a notice (D-22).
- [ ] SR: `prune <node> [--keep-weights]` (whole subtree, recipe/deltas/metrics kept); `gc [--auto]`. `undo` of `prune` after `gc` warns that the weights are gone (D-18).
- [ ] SR → LEAD: core functions for C4 wiring; LEAD wires the CLI.
- [ ] TEST: property test: CDC boundaries stable under insertion (proptest). Also a fake 50 MB checkpoint pair differing in one contiguous, in-place 1 % region (D-25).

Acceptance (§9 M4)
- [ ] Two checkpoints differing in one contiguous 1 % region share ≥ 95 % of chunks, by count and by bytes.
- [ ] `fork --step` restores a file whose blake3 equals the original.
- [ ] `prune` then `gc` deletes exactly the pruned checkpoint's unique chunks. Shared chunks and every other node's checkpoints are still readable.
- [ ] Property test: inserting bytes at offset k changes only the chunks around k.

## M5: uv integration

Tasks
- [ ] LEAD: env hash = blake3(`uv.lock` + `.python-version` + `requires-python` + CUDA/driver string (+ PEP 723 block)). Fallback: `uv pip freeze`.
- [ ] LEAD: launch rule: a bare `x.py` runs as `uv run x.py` when `pyproject.toml` or `uv.lock` exists, else `python x.py`. `-- uv run …` is passed through verbatim.
- [ ] LEAD: `uv lock --check` before hashing: warn on failure, refuse with `--strict`, store `lock_ok`.
- [ ] LEAD: `fork` restores `uv.lock` and runs `uv sync --frozen` unless `--no-sync`. `show` prints the reproduce one-liner.
- [ ] LEAD: `env_delta` (lockfile package key diff + versions), and the env row in `siblings`.
- [ ] TEST: uv fixture project (tiny, no torch). The CUDA string is stubbed in tests.

Acceptance (§9 M5)
- [ ] In a uv project, `pollard run train.py` launches `uv run train.py` (visible in the node's `command`).
- [ ] Changing `uv.lock` changes the node's `env` hash. An unchanged lock gives the same hash.
- [ ] A stale lock warns and the run proceeds with `lock_ok=false`. With `--strict` it is refused.
- [ ] After `fork <node>`, `uv pip freeze` in the venv matches that node's lock.

## M6: `pollard-git`: tree hashing via gix, `import`, `export`

Tasks
- [ ] SR (moved to M1): `pollard_git` tree hashing (C2).
- [ ] SR: `import <git-rev>` (root node, `code` = commit tree hash, other hashes from the working copy); `init --from-git` = `import HEAD`.
- [ ] SR: `export <node> --branch` (single commit) and `--path a..b` (linear, one commit per node). Generated message (id, pin, note title / config delta, primary metric at `last_own`). Include the working copy's current off-tree files and `.pollard-recipe.json`. `import` hashes the commit's files after the code-manifest rules (D-15).
- [ ] SR: never touch the git index or HEAD. Write refs only under the named branch.
- [ ] LEAD: CLI wiring.

Acceptance (§9 M6 v3)
- [ ] Fixture HEAD has no off-tree, ignored, or >10 MB files. After `import HEAD`, the node's `code` equals HEAD's tree hash.
- [ ] `export` of that node gives a commit whose tree, with `.pollard-recipe.json` removed, hashes equal to HEAD's tree.
- [ ] `export --path` over a 4-node chain gives 4 commits, each with exactly one parent (the previous one), a generated message, and `.pollard-recipe.json`.
- [ ] Off-tree files (e.g. `REPORT.md`) are present in the exported trees.
- [ ] `git rev-parse HEAD` and `git status --porcelain` are unchanged before and after export.

## M7: `pollard-remote`: local-path and S3 remotes, `push`, `pull`, id salting

Tasks
- [ ] SR: remote layout `objects/`, `packs/` (64 MB packs), `nodes.jsonl` (append-only), `pins.json`.
- [ ] SR: `push` uploads only missing objects and packs, then appends node lines. `pull` reverses it. `note`/`pins` are last-writer-wins.
- [ ] SR: `pull` refuses a same-id node that has a different recipe and names both (D-24). The id salt itself ships in M1 (LEAD).
- [ ] SR: S3 via `object_store`. Tests run against the local-path remote. S3 coverage uses MinIO or is marked manual.
- [ ] LEAD: CLI wiring.

Acceptance (§9 M7)
- [ ] Clones X and Y each create a child of the same parent, and both push to one local-path remote.
- [ ] After `pull` on both, each sees all nodes, and all ids are distinct.
- [ ] A second `push` with no new nodes transfers 0 bytes (instrumented byte counter).

## M8: sweeps, seed collapse, `apply`, `pin`, tree collapsing, `--metric` highlight

Tasks
- [ ] LEAD: `run --sweep <name>`: store the name in `nodes.sweep` (D-16). One row in `tree`, one column in `siblings`, and `--expand-sweeps`.
- [ ] LEAD: seed collapse via `seed_keys`, with mean ± std.
- [ ] LEAD: `apply <node>`: 3-way patch of the node's code delta. Conflicts leave markers, exit 0, and warn.
- [ ] LEAD: `pin`/`unpin` (pin names usable as node args; no auto-prune, D-23); `tree` collapses pruned/failed subtrees unless `--all`; `--metric` best-path ★.
- [ ] TEST: 32-seed sweep fixture (shell trainer).

Acceptance (§9 M8)
- [ ] A 32-seed sweep is 1 row in `tree` and 1 column in `siblings`, showing mean ± std.
- [ ] `apply` of a conflicting delta leaves conflict markers in the file, exits 0, and prints a warning.
- [ ] `apply` of a non-conflicting config-only delta touches only `config.yaml` (Journey B).
- [ ] `tree` hides pruned subtrees, and `tree --all` shows them.

## M9: Python SDK, config adapters, wandb/neptune extras, wheel with binary

Tasks
- [ ] SR: `python/pollard`: `current()`, `Run.log`, `save_checkpoint`, `set_config`, `fork_step()`, `siblings()` (DataFrame if pandas is present). Under 200 lines, pure Python, Python ≥ 3.10.
- [ ] SR: config-capture adapters `hydra`, `file`, `sdk`; extras `wandb`, `neptune` via `forward()`.
- [ ] SR: wheel bundling the platform binary (maturin `bindings = "bin"` or equivalent), published as PyPI `pollard-vcs`, import `pollard` (D-6).
- [ ] TEST: PyTorch-free minimal script (numpy optional). One variant uses the SDK, one uses plain file I/O.

Acceptance (§9 M9)
- [ ] `uv tool install ./dist/<wheel>` provides working `pollard` and `po` on PATH.
- [ ] A training script with exactly 3 added SDK lines logs metrics and one checkpoint.
- [ ] The same script with the SDK removed and ≤ 10 lines of plain file I/O produces the same metrics and checkpoint.

---

## Cross-cutting acceptance (§10), tracked by TEST from the milestone where each item applies
- [ ] Every command except `run`, `fork --step`, `push`, `pull`, `gc` takes < 200 ms on a 1,000-node repo.
- [ ] Every mutating command prints the node id it created or changed on its last line.
- [ ] `undo` reverses every mutating command in §4, and `fork` never loses working-copy changes.
- [ ] Journeys A–D run end to end as scripts (A/B after M5, C after M4+M5, D after M8).
- [ ] `po` alias works for every example.
