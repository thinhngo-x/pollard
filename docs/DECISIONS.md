# pollard decisions log

All entries are **DECIDED** (2026-09-18). The project owner delegated these decisions to PM, and they are reflected in `SPEC.md` v3 (see its revision log).
Rule applied: take the simplest choice that keeps the §9/§10 acceptance tests meaningful.
Evidence was gathered on 2026-09-18 with `cargo info` / `cargo search`, `curl https://pypi.org/pypi/<name>/json`, and PATH/package lookups (Ubuntu, kernel 5.15, rustc 1.85).

## A. §11 open questions

| # | Decision | Rationale |
|---|---|---|
| D-1 | **DECIDED:** don't adopt xet-core; use the `fastcdc` crate (D-10). | No `xet-core` crate exists. The xet crates (`xet-data` 1.6.0, Apache-2.0) mark the chunker "do not use directly" and pull in an HTTP client and tokio. |
| D-2 | **DECIDED:** `run` always blocks on data-manifest hashing; there is no `data: pending` state. | Remote roots are listed by `(key, size, etag)` without reading content, so blocking is cheap. This avoids a node state machine and keeps the duplicate check exact. |
| D-3 | **DECIDED:** `checkpoint_dir` in `config.toml`, default `ckpt/`, auto-ignored from the code manifest. | This was the spec's own default. Auto-ignoring stops checkpoints from counting as code. |
| D-4 | **DECIDED:** a curated in-repo list of 1,000 adjectives and 1,000 nouns (lowercase ASCII, 3–6 letters, screened), compiled in and append-only after v1. | A curated list can be screened for offensive or confusing words. Append-only keeps existing salt-to-word mappings stable. |
| D-5 | **DECIDED:** `README.md` is off-tree by default (via `*.md`), and `offtree` uses gitignore glob semantics including `!README.md` to opt back in. | Editing docs should not make a new experiment, and `export` still ships it. |

## B. Name availability (§1)

| # | Finding | Decision |
|---|---|---|
| D-6 | PyPI `pollard`: **taken** (v1.6.0, "Governed execution trees for AI agents", jemsbhai). `pollard-vcs`: **free**. | **DECIDED:** publish as `pollard-vcs` and import as `pollard`. Install with `uv tool install pollard-vcs`. The other package likely also imports as `pollard`, so the two can't share an environment; README notes this. |
| D-7 | crates.io `pollard`: **taken** (v0.0.9, Firefox-profile MCP server, antiguru). `pollard-vcs`, `pollard-cli`, `-core`, `-objects`, `-git`, `-remote`: **free**. | **DECIDED:** keep the spec crate names; install with `cargo install pollard-cli`. We don't need the bare name. |
| D-8 | `po`: nothing on this machine's PATH, and no suggestion from `command-not-found`. Debian stable contents search: no `bin/po` in any package. Homebrew: no `po` formula. macOS, Nix, and Arch were not checked. | **DECIDED:** ship `po`. The installer warns if `type po` already resolves to something else, such as a shell alias. |

## C. Agent-latitude choices

| # | Decision | Rationale |
|---|---|---|
| D-9 | **DECIDED:** JSONL tailing polls every 200 ms, reading whole lines from the last offset. | No dependency, and it works on NFS/cluster filesystems where inotify doesn't. It can be swapped for `notify` later without any protocol change. |
| D-10 | **DECIDED:** `fastcdc` 5.0.0, `v2020::StreamCDC::new(r, 8 KiB, 64 KiB, 128 KiB)`. | MIT license, a complete streaming chunker with zero default deps, gear-based, and builds on rustc 1.85 (edition 2024). `gearhash` 0.1.4 needs rustc 1.87 and gives only the rolling hash. |
| D-11 | **DECIDED:** `rust-version = "1.85"` pinned in the workspace. | This is the build machine's toolchain and the edition 2024 minimum. It makes dependency MSRV explicit. |

## D. Spec ambiguities

| # | Decision | Rationale |
|---|---|---|
| D-12 | **DECIDED:** plain `fork <node>` ships in M1. `--step` stays in M4 (recording `fork_step` is M3) and sync stays in M5. | M1 `undo` and the M3 test both need it. |
| D-13 | **DECIDED:** `pollard-git` tree hashing ships in M1. `node.code` = git tree hash, and a new table `code_trees(git_tree PK, manifest_hash)` finds the blake3 manifest. | This keeps §3 and §7 both true without re-hashing. It is one lookup table instead of a second hash column. |
| D-14 | **DECIDED:** files of 1–10 MB are stored whole behind `put_file` until M4, which switches the internals to chunking. | M1 isn't blocked on CDC, and callers never change. |
| D-15 | **DECIDED:** `import` hashes the commit's files after the code rules. The M6 test uses a clean HEAD (no off-tree, ignored, or >10 MB files) and compares the exported tree with `.pollard-recipe.json` removed. | The original test was unsatisfiable, because export adds the recipe file and off-tree files. |
| D-16 | **DECIDED:** a nullable `sweep` text column on `nodes`; members are ordinary children of the current node. | This is the smallest schema change that lets `tree`/`siblings` group members. |
| D-17 | **DECIDED:** a nullable `wc_snapshot` column on `ops` (the working-copy manifest before `fork`/`apply`/`undo`), which `undo` restores. | This makes "never lost" at fork time real, and reuses the manifest format. |
| D-18 | **DECIDED:** `gc` is the point of no return. `undo` of `prune` after `gc` restores the nodes and warns that the weights are gone. | This needs no retention machinery. The recipe still reproduces the weights, and Journey B's undo happens before `gc`. |
| D-19 | **DECIDED:** the duplicate check refuses only when the match is `running` or `done`. Matches that are `failed`/`killed`/`pruned` get a notice. | Relaunching a crashed run is the common case, and the M1 test stays meaningful against a `done` node. |
| D-20 | **DECIDED:** the captured config file stays in `code_delta` (so `apply` carries it, as Journey B needs), but the `siblings` code row and auto notes skip it. | It is shown once, as config rows, and `apply` still works. |
| D-21 | **DECIDED:** on a fork-step re-parent, `fork X --step N` behaves exactly like `fork <ancestor> --step N`, restoring the ancestor's recipe and checkpoint. | The ancestor's recipe produced step N, so the parent and the working copy agree and the deltas are honest. |
| D-22 | **DECIDED:** a checkpoint's step is the last digit run in its file name. The lookup walks ancestors by the metric inheritance rule. If there is no exact match, the largest step ≤ N is used, `fork_step` is set to it, and a notice is printed. | No schema change, and it matches `ckpt/step30000.pt` in Journey C. |
| D-23 | **DECIDED:** "auto-prune" is dropped. Nothing is pruned except by an explicit `prune`. | No mechanism was ever specified for it. |
| D-24 | **DECIDED:** ids are `<adj>-<noun>-<counter>`, with the counter per-clone and sequential, and the word pair = `blake3(clone_salt ‖ counter)` over the word lists. `clone_salt` is a random 64-bit value. `pull` refuses a same-id node with a different recipe and names both. | This keeps the spec's readable ids. Collision odds are about 1 in 4M per same-counter pair, and a collision fails loudly instead of silently. |
| D-25 | **DECIDED:** M4's 1 % change is one contiguous in-place region, and sharing of ≥ 95 % is measured by count and by bytes. | 1 % of bytes spread at random touches nearly every 64 KB chunk, which would make the test meaningless. |
| D-26 | **DECIDED:** all install commands use `pollard-vcs` (spec §1, §4, §10, and README). | Follows from D-6. |

## E. Lead-dev implementation choices (append-only; lead owns this section)

| # | Decision | Why |
|---|---|---|
| L-1 | Hydra-style `key.path=value` launch args (after the script) are applied onto the captured config before hashing, so `train.py seed=3` is part of the resolved config. | Journey D sweeps via CLI args; otherwise every member is a duplicate recipe. Recipe definition unchanged (config = *resolved* config). |
| L-2 | Op snapshots hold `{head, pending fork_step, nodes, pins}`. `undo` is itself an op (`undo; undo` = redo, as in jj). `run` also records `wc_snapshot` (its code manifest) so `undo` of a run restores the working copy. | `undo` must move `@` back; pending `--step` must not leak across undo. |
| L-3 | Node gets `note_auto INTEGER` alongside `sweep`. | §3 says auto notes are "flagged auto"; the flag needs a home. **Schema addition, flag for owner.** |
| L-4 | `siblings --json` = `{"<column header>": {"<row label>": "<cell>" \| null}}` in display order (pandas `read_json` default orient). Stable from M2. | Round-trips through `pd.read_json` / `df.to_json()` unchanged. |
| L-5 | `sdk`/`hydra` capture: config is known only after launch, so the duplicate check becomes a warning at capture time. | The script writes `POLLARD_CONFIG` after `run` must already have created the node. |
| L-6 | `env` = blake3 of a JSON record `{lock, freeze, python_version, requires_python, cuda, pep723}` stored as an object; `POLLARD_CUDA` overrides the CUDA/driver string. | Same inputs as §4, but the record can be re-read to compute `env_delta`. |
| L-7 | `run` exits with the child's exit code; last stdout line is still the node id. | Scripts can use both `$?` and `$(... \| tail -1)`. |
| L-8 | `tree --metric`: higher-is-better for keys containing acc/score/reward/auc/f1/bleu/precision/recall/map/iou, lower otherwise; ★ marks the ancestry of the best node. Pruned subtrees are hidden, failed subtrees collapse to one line, unless `--all`. | No metric direction in the spec. |
| L-9 | Sibling cells: blank = `—`; group columns (sweep / seeds) show a config value only if all members agree, else `N values`; metrics show `mean ± std` (sample std). `last_own` cells carry `@step`. | Formatting latitude (§11). |
| L-10 | `uv lock --check` runs with `--offline`. | Keeps `run` fast and deterministic; a stale lock still fails offline. |

### D-29. Remote layout and sync semantics (M7)
**AGENT DECISION.** `object_store` 0.14 (`aws` feature) behind a current-thread tokio runtime. Objects and chunk maps are stored individually (`objects/`, `chunkmaps/`, as on disk); chunks go into ≤ 64 MB packs with a text index written after the pack, so a crashed push never leaves an index pointing at missing data. `pull` fetches missing chunks by byte range. `nodes.jsonl` is append-only; each line is a full node version plus deltas, a metrics blob hash and the code-manifest hash, and the last line per id wins (note/status/pins last-writer-wins). Per-clone meta `sync:<id>` stops a clone from re-pushing stale copies over newer remote edits. Appending is read-modify-write (object stores have no append); two concurrent pushes can lose one push's lines until it pushes again. Collision rule: refuse when the same id has a different recipe **and** a different `created_at` (a node whose sdk-captured config changed its recipe after an early push is the same node). Lockfile pin: `idna_adapter = 1.1.0` so the tree avoids icu/yoke-derive releases that need rustc ≥ 1.86.


### D-30. Cheaper data hashing: tag and stat modes
**DEFERRED (next version).** The owner approved it for the release after v1; see SPEC §12. v1 scope, milestones and tests are unchanged.
- Tag mode: `data = [{ tag = "..." }]`. The hash is blake3 of the tag and no files are read. The user must bump the tag when the data changes.
- Stat mode: `data = [{ path = "...", mode = "stat" }]`. The manifest uses `(path, size, mtime)`. It misses same-size edits with a preserved mtime.
- The default stays content hashing for local roots and etag listing for remote roots. The plain-string form stays valid, and there is no node schema change.
| L-11 | Op snapshots are zstd-compressed JSON in the `ops` row (not in `objects/`), so they stay local and are never pushed. | 1,000-node snapshots ×2 per op made `db.sqlite` large and ops slow; objects would leak into `push`. |
| L-12 | `apply` merges per line when base/ours/theirs have equal line counts (in-place edits), else diff3 (`diffy`). | diff3 treats edits on adjacent lines (`lr`, `depth`) as one conflicting hunk; Journey B needs them to merge. |
| L-13 | `run` checks for child exit every 20 ms and tails metrics every 200 ms. | Short runs return quickly; D-9 cadence unchanged. |
