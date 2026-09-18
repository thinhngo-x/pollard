# pollard next alpha: technical notes

Owner: Tech Lead. Companion to `docs/next/PLAN.md` (PM: scope, priorities, acceptance). Milestone ids N1–N7 follow PLAN §3.
Baseline: `0.1.0-alpha.1` source (read 2026-09-19), SPEC v3, DECISIONS, TEST_REPORT, issues #11–#19.
Sizes are for one focused engineer or agent: **S** < ½ day, **M** 1–2 days, **L** 3+ days. Paths are relative to `crates/` unless noted.

---

## 0. Findings from reading the code (verified)

| Claim | Verdict | Evidence |
|---|---|---|
| Concurrent `push` can lose `nodes.jsonl` lines | **True** | `pollard-core/src/sync.rs::push` does a GET, appends in memory, then `remote.put("nodes.jsonl", raw)`. The `.pollard/lock` is local only. `pins.json` is the same (whole-file put). |
| `gc` never deletes objects | **True** | `pollard-objects/src/store.rs::Store::gc` sweeps only `chunkmaps/` and `chunks/`. Checkpoints under 1 MB live in `objects/`, so they are **never** freed after a prune. |
| `gc --auto` is plain `gc` | **True** | `pollard-cli/src/bin/pollard.rs`: `Cmd::Gc { auto: _ }`. |
| `s3://` data roots hashed by URL only | **True** | `pollard-core/src/wc.rs::data_manifest` has a `ponytail:` note: one entry, `hash = b3(url)`. A changed bucket gives the same `data` hash. |
| sdk/hydra only warn on duplicate recipes | **True, by design (L-5)** | `run.rs::start` skips the duplicate check for `Capture::Sdk | Capture::Hydra`, and `run.rs::set_config` only prints a warning. |
| hydra capture picks the newest `.hydra/config.yaml` | **True, and a bug** | `run.rs::capture_hydra_config` takes the newest file under `output_dirs` with **no lower bound on mtime**. A run that crashes before Hydra writes its config picks up the **previous run's** config, so the recipe is wrong. |
| Metric direction guessed from the key name | **True** | `tree.rs::higher_is_better`: substring list, so `"map"` also matches e.g. `"heatmap_loss"`. |
| S3 remote untested | **True** | `TEST_REPORT.md` coverage notes. `pollard-remote` uses `AmazonS3Builder::from_env` + `PrefixStore` + `get_ranges`, and none of these has ever run against S3. |
| Linux x86_64 only | **True** | `.github/workflows/release.yml`: one gnu target, one `manylinux: auto` x86_64 wheel. Code uses `std::os::unix` only (`run.rs` `ExitStatusExt`), so it should port to macOS. |
| Draft release has a stale wheel | **True** | `gh release view v0.1.0-alpha.1` is a draft with `pollard_vcs-0.1.0a1-py3-none-linux_x86_64.whl`, a plain `linux_x86_64` tag, not manylinux. |
| #14 says `clap_complete` is "already a dependency" | **False** | Not in `Cargo.lock`. It is a new dependency and needs the `unstable-dynamic` feature for `CompleteEnv`. |
| #18: `anstream`/`anstyle` are already in the tree | **True** (through `clap`) | `Cargo.lock`. `terminal_size` and `unicode-width` are **not** in the lock; they would be new dependencies. |
| MSRV | 1.89 in `Cargo.toml`; SPEC §8 and D-11 still say 1.85 | Docs are stale. |
| Status parse is lossy | **Hazard** | `node.rs::Status::parse` maps **any unknown string to `Pruned`**. Must change before `pruned` stops being a status. |
| No schema version anywhere | **True** | `repo.rs::open_at` runs `CREATE TABLE IF NOT EXISTS …` on every open. There is no `user_version` and no version key in `meta`. |

---

## 1. Migration mechanism (one mechanism for every format change)

Everything that changes on disk ships together as **format 2** in one release, so users migrate once.

### 1a. Local repo (`.pollard/`)

- **Version:** SQLite's built-in `PRAGMA user_version` (0 = alpha.1). No new table.
- **Where:** `repo.rs::open_at`. After `SCHEMA`, read `user_version`:
  - `== LATEST`: continue.
  - `> LATEST`: refuse with "repo written by a newer pollard (format N); upgrade".
  - `< LATEST`: take `.pollard/lock`, then run `MIGRATIONS[v..LATEST]` (a `&[fn(&Connection) -> Result<()>]`) in **one** `BEGIN IMMEDIATE` transaction, with each step bumping `user_version`. Print one notice to stderr.
- **`SCHEMA` stays frozen at the alpha.1 shape.** New repos run every migration at `init`. There is one code path, so the migrations get tested on every `init`.
- **Backup:** before migrating, `VACUUM INTO '.pollard/backup/db-format1.sqlite'`, and the notice names that path. The file is a few MB, because metrics and nodes are small.
- **Op log barrier:** `meta.migrated_at_op = max(op_id)`. `ops::undo` refuses to undo ops at or below it ("op N predates the format-2 upgrade"). This satisfies PLAN N1: old op snapshots are never decoded into the new `Node` struct, so there is no legacy handling in `ops::decode`.
- **Crash safety:** the migration is a single transaction, so a crash leaves the repo either v1 and intact or v2 and complete.
- **Make alpha.1 binaries fail loudly (PLAN D3). Feasible, cheap:** alpha.1 opens `.pollard/db.sqlite` by fixed path. Format 2 moves the database to `.pollard/state.sqlite`, and in place of the old `db.sqlite` leaves a **directory** named `db.sqlite` containing `UPGRADED.txt`. alpha.1's `Connection::open` then fails on every command ("database: unable to open database file") before writing anything. Order: `VACUUM INTO state.sqlite.tmp` → migrate the tmp file → rename it to `state.sqlite` → move the old file to `backup/` → `mkdir db.sqlite`. On open, a `state.sqlite.tmp` left behind is deleted and the migration is redone. Rollback: `rm -r db.sqlite && mv backup/db-format1.sqlite db.sqlite`, then reinstall alpha.1. Cost: S on top of the framework. Future formats (3+) need no rename, because `> LATEST` already refuses.
- **Read-only opens** (tab completion, #14): add `Repo::open_read_only` (`SQLITE_OPEN_READ_ONLY`, no `SCHEMA`, no migration). If the version is not `LATEST`, it returns an empty result.

**Migration step 1 (format 2), contents:**

| Change | SQL / logic |
|---|---|
| `nodes.pruned_at TEXT NULL` | `ALTER TABLE nodes ADD COLUMN pruned_at TEXT` |
| Recover the pre-prune status | For each `status='pruned'` row, scan `ops` newest→oldest, zstd-decode `before_snapshot` as `serde_json::Value` (not `Node`), and take the first snapshot where that node's status is not `pruned`. Set `status` to it and `pruned_at` to that op's `ts`. **Fallback** (not found, e.g. a node pulled already pruned): `done` if `finished_at` is set, else `killed`; `pruned_at = finished_at` or else `created_at`; list the id in the notice (open question Q3). |
| `pins` reshape | `CREATE TABLE pins2(node_id TEXT NOT NULL, name TEXT UNIQUE)`, plus `CREATE UNIQUE INDEX pins_unnamed ON pins2(node_id) WHERE name IS NULL`, copy the rows, drop `pins`, rename. |
| Index | `CREATE INDEX nodes_pruned ON nodes(pruned_at)` (not strictly needed at 1,000 nodes; skip it if it isn't measurable). |
| Barrier | `meta.migrated_at_op`. |

### 1b. Remote (`nodes.jsonl`, `pins.json`)

This also fixes the concurrent-push race (see R1). It is the one remote format change.

- **Marker:** a `FORMAT` object at the remote root containing `2`. It is absent on alpha.1 remotes. A client refuses a remote whose `FORMAT` is newer than it knows.
- **Node lines** go to **immutable segments** `nodes/<utc-ts>-<clone_salt>-<n>.jsonl`, one per push, create-only. Two pushes can no longer overwrite each other, and no conditional-put support is needed, so it works on local paths, S3 and MinIO alike. Readers union the legacy `nodes.jsonl` (segment 0) with `nodes/*` in name order; last line per id wins, as today. A local table `remote_segments(name)` records segments already applied, so `pull` downloads only new ones.
- **Pins** become a line in the segment, `{"pins": [[node_id, name|null], …]}`, still last-writer-wins on the whole map by segment order (D-29 semantics unchanged). The legacy `pins.json` is read only when no segment carries pins.
- **Conversion** happens on the first push by a format-2 client: write `FORMAT`, then append one **poison line** `{"pollard_format":2,"upgrade":"…"}` to the legacy `nodes.jsonl`. alpha.1's `sync::remote_nodes` fails to parse it (`missing field node`) and errors **before** writing, on both push and pull. That makes alpha.1 fail loudly (PLAN D3). Format-2 readers skip the line.
- **Legacy lines** with `status:"pruned"` are normalized on read in `sync::remote_nodes`, using the same fallback as 1a. Any format-2 line for the same id supersedes them.
- **One-time full re-push:** adding `pruned_at` to `Node` changes every line hash, so the first push after the upgrade re-sends every node line (KBs per node, no blobs). This is expected; the "second push transfers 0 bytes" M7 test still holds after that.
- `sync::Line.node.pruned_at` carries `#[serde(default)]`.

### 1c. What does not migrate

| Item | Why |
|---|---|
| Objects, chunks, chunk maps, packs | Formats are unchanged. |
| Op snapshots | Barrier instead (1a). New snapshots carry `pruned_at` and `(Option<name>, node)` pins; an old string-tuple pin decodes into `Option<String>` unchanged anyway. |
| `config.toml` | Only additive keys (`metric_goal`, table-form `data`). An alpha.1 config parses unchanged. |
| Run protocol env vars, Python SDK | Unchanged. |
| `siblings --json` | Unchanged (frozen since M2). |

---

## 2. Items

### N1: schema v2 and migration framework (prerequisite for #19 and #17)

| | |
|---|---|
| Approach | Section 1a/1b. Files: `pollard-core/src/repo.rs` (`open_at`, new `migrate`, `open_read_only`, db path const), `node.rs` (`pruned_at` in `Node`, `COLS`, `from_row`, `insert`; `Status::parse` returns `Result` and never silently maps to `Pruned`; the `Pruned` variant is removed), `ops.rs` (barrier in `undo`), `weights.rs`/`sync.rs`/`siblings.rs`/`tree.rs`/`run.rs` (every `Status::Pruned` / `'pruned'` use, 9 sites, switched to `pruned_at`). |
| Size | **L** (framework M, migration step + status recovery S, loud-fail rename S, fixtures and tests M). |
| Depends on | Nothing. It blocks #19, #17 storage and remote v2. |
| Risk | High: touches every alpha.1 repo. Mitigated by the backup, the single transaction, and fixture tests. **Behaviour trap:** `run.rs::start`'s duplicate check refuses matches whose status is `running`/`done`. A pruned `done` node used to read `pruned` and got only a notice (D-19). The new query must add `AND pruned_at IS NULL` to the refusal branch, or pruned nodes start blocking reruns. The same applies to `set_config`'s `status!='pruned'`. |
| Tests | New `pollard-cli/tests/n1_migration.rs` plus checked-in fixtures `tests/fixtures/alpha1/{repo,remote}.tar.gz`, generated once by `tests/fixtures/alpha1/make.sh` with the published `0.1.0-alpha.1` binary. Fixture contents (from PLAN): a subtree prune, a node run under a pruned `@`, named pins, a `--keep-weights` prune, and ops that include the prune. Tests: migrate once (notice, backup, `user_version`); statuses recovered; the hidden node is visible in `tree`; pins resolve; `siblings --json` byte-identical before and after; `undo` stops at the barrier; a second open is a no-op; a crash mid-migration (the test kills after `state.sqlite.tmp` is created) leaves v1 intact; the alpha.1 binary on a migrated repo exits non-zero (only in CI, where the alpha.1 binary is installed). |

### N2: #19 prune redesign

| | |
|---|---|
| Approach | `weights.rs::prune(repo, node, Opts{recursive, keep_weights, force, dry_run})`: without `-r` only the node, with `-r` the subtree walk as today. It sets `pruned_at = now()` instead of `status`. **Protection:** collect the pinned nodes and `@` within the target set, and error listing all of them unless `--force`. **Preview:** split `weights::gc` into `live_set(repo, excluding: &[id])` and a dry-run `Store::gc(live, dry_run: bool)` in `pollard-objects/src/store.rs`. Preview bytes = dry-run gc with the target nodes treated as pruned, so it matches the next `gc` exactly (PLAN N2). CLI (`pollard.rs`): `-r`, `-y`, `--dry-run`, `--force`; prompt via stdin only when it is a TTY. New `weights::unprune(repo, node, recursive)` clears `pruned_at` as one op and warns with `missing_weights`. `tree.rs`: a pruned node **with non-pruned descendants** renders as a dim placeholder line and recursion continues. A fully pruned subtree is hidden unless `--all`. `best` excludes `pruned_at` nodes. `siblings.rs:64` filters on `pruned_at`. `ops.rs::undo`'s `pruned_before` warning uses `pruned_at`. |
| Size | **L**. |
| Depends on | N1. Tree dimming looks best after #18, but works in plain text (`(pruned)` suffix). |
| Risk | Medium. (1) **Timing:** the exact-bytes preview reads every live chunk map (Tier 2), so `prune` without `-y` can exceed §10's 200 ms on big repos. Skip byte computation with `-y`, so the 1,000-node timing test (which must pass `-y`) stays under budget, and document the preview as exempt. (2) **Script break:** PLAN says a non-TTY stdin without `-y` refuses. Every existing script and test that runs `prune` needs `-y`. Add this row to PLAN §4's upgrade table. (3) `keep_weights:<id>` lives in `meta`, not in op snapshots, so `undo` of a `--keep-weights` prune doesn't clear it. This is a pre-existing bug; it is harmless, but fix it in the same pass by clearing it in `unprune`. |
| Tests | New `n2_prune.rs`: node-only prune keeps children visible; `-r` subtree; refusal lists pinned and `@` offenders; `--force`; `--dry-run` writes no op; preview bytes == `gc` bytes on the M4 50 MB fixture; `show` prints `done` + `pruned <ts>`; `unprune` (and `-r`) independent of op order; `unprune` after `gc` warns; #19 repro (prune `@`'s parent, run, node visible); a pruned `done` match is a notice, not a refusal; `fork --step` through a pruned ancestor. Update `m4_objects.rs::prune_marks_whole_subtree_and_undo_restores` (use `-r`), `prune_then_gc_*` (`-y`), `cross_cutting.rs` (`-y`), and the `journeys.rs` prune steps. |
| Extras | `gc --older-than 30d` is S once `pruned_at` exists: filter the dead set by `pruned_at`. `prune --failed`: skip (PLAN defers both). |

### N3: tree and run workflow

| Item | Approach (files) | Size | Depends on | Risk / format | Tests |
|---|---|---|---|---|---|
| **#13** failed `@` → sibling | `run.rs::start` parent resolution. When `o.parent.is_none()`, head status is `Failed` or `Killed` (owner decision), **and there is no pending `meta.fork_step`**, use `head.parent` (and `head.fork_step`) and print `note: @ <id> failed; attaching to <parent> (use --parent @ to build on it)`. The pending-fork-step guard matters: `fork <failed> --step N` must still give a child (PLAN N3). The same guard keeps Journey C (`fork blue-elk-5 --step 30000` on a killed node) a child. | S | none | Run-protocol semantics (owner approved). No schema change. | `n3_tree_run.rs`: fail → edit → run is a sibling with a notice and the last line is the id; `fork buggy --step N` → child; `--parent buggy` → child; killed `@` → sibling; `fork <killed> --step N` → child; sweep members unaffected (`m8_sweeps.rs` stays green). |
| **#11** failed collapse | Owner rule: a node with children never collapses, except a pruned node. `tree.rs::Ctx::children`: delete the `collapsed` branch (`n.status == Failed && !all`); pruned nodes stay hidden unless `--all`, but one with non-pruned descendants renders as a dimmed placeholder so live runs are never hidden (#19). Failed rows are dimmed by #18. Remove `Ctx::count` if nothing else uses it. `--expand <node>`: skip (PLAN defers it). | S | none (dimming after #18) | None. | Update `m8_sweeps.rs`, which asserts `… collapsed`; a new test in `n3_tree_run.rs` checks that a failed node with children shows its children. |
| **#17** optional pin name, highlight, `--pinned` | Storage in N1. `cmd.rs::pin(repo, node, name: Option<&str>)`; `unpin(arg)`: a pin name removes that pin, otherwise resolve as a node and remove all of its pins. `pin` must also refuse a name equal to an existing node id (today `resolve` checks node ids before pins, so such a pin would be shadowed silently). `repo.rs::pins_of` skips NULL names; `resolve` is unchanged (`WHERE name=?`). `ops.rs::Snap.pins: Vec<(Option<String>, String)>`; `restore` uses `name IS ?1`. `tree.rs`: marker `◆` for pinned nodes (colour from #18); `Opts.pinned` keeps only nodes in ∪ `ancestry(pin)` and skips everything else. `sync.rs`: pins as a segment record (1b). | M | N1; #18 for colour; remote v2 for syncing unnamed pins | Pins shape changes locally and on the remote (covered by format 2). | `n3_tree_run.rs`: unnamed pin; named pin resolves; `unpin` by name and by node; `undo` of each; `◆` without a TTY; `--pinned` output on a 1,000-node repo in < 200 ms (add to `cross_cutting.rs`'s slow test); a pin name that equals a node id is refused. `n4_remote.rs`: unnamed pins round-trip. |
| **#12** direct sibling diff | `cmd.rs::diff`: delete the sibling special case (`na.parent == nb.parent` → `siblings::build`). The direct path below already handles siblings. D1 default: no flag. Also, `wc::manifest_of` fails for imported roots with no stored manifest, which currently errors the direct diff; treat a missing manifest as empty, as `run.rs::compute_deltas` does. | S | none | Output change for `diff` on siblings (PLAN §4 has it). | `n3_tree_run.rs`: siblings `x=1→10` and `x=1→20` give `x 10 → 20`. Update any `m2_siblings.rs` assertion on "each vs. the parent" in `diff` output. |

### N4: remote safety and S3

| Item | Approach (files) | Size | Depends on | Risk / format | Tests |
|---|---|---|---|---|---|
| **R1** concurrent push race | Remote format 2 (1b): `sync.rs::push` writes a new segment and never rewrites a file; `remote_nodes` reads legacy + segments; new local table `remote_segments`; poison + `FORMAT` on conversion. `pollard-remote/src/lib.rs` gains `list_names(prefix)` (it exists as the private `list`) and a create-only `put` (plain `put` is fine because names are unique). Compaction of many segments: not now; mark it `ponytail:`, upgrade when pulls get slow. | M | N1 (`pruned_at` in lines) | Remote format change, covered by 1b. The segment count grows by one per push. | `n4_remote.rs`: two clones × 20 concurrent pushes (threads spawning the binary) to one local-path remote, then both pull and no node is missing; conversion of the alpha.1 fixture remote; an alpha.1 binary against a converted remote exits non-zero (CI only); `pruned_at` syncs; second push = 0 bytes (M7 unchanged). |
| **R2** S3 coverage | CI job in `.github/workflows/ci.yml` with a `minio/minio` service container, `AWS_ENDPOINT`/`AWS_ALLOW_HTTP=true`/keys env, and a bucket created with `mc`. Tests read `POLLARD_TEST_S3_URL` and are skipped when it is unset: run `m7_remote.rs` and `n4_remote.rs` bodies with `remote = "s3://…"` through a shared helper in `tests/common/mod.rs`. | M | R1 (so the new format is what gets tested) | Expect real bugs: `PrefixStore` + `list` prefix handling, `get_ranges` coalescing, `AWS_ALLOW_HTTP` for MinIO. Budget time to fix them. | The same suites against MinIO, on every PR. |

### N5: terminal polish and completion

| Item | Approach (files) | Size | Depends on | Risk | Tests |
|---|---|---|---|---|---|
| **metric_goal** | `config.rs`: `metric_goal: Option<MetricGoal>` with `#[serde(untagged)] enum { All(Goal), PerKey(BTreeMap<String, Goal>) }`, `Goal = min|max`. A `Config::higher_is_better(key)` falls back to today's `tree::higher_is_better` guess. Callers: `tree.rs::render`, and the `siblings.rs` delta colouring. Add a commented line to `DEFAULT_TOML`. | S | none | Additive config key. | `n5_terminal.rs`: `loss` with `metric_goal="max"` → ★ on the highest; the per-key table wins over the global value. |
| **#18** colour, width, tree layout | Core renderers (`tree.rs`, `siblings.rs::render`, `pollard.rs::show`) emit `anstyle` styles inline. The CLI prints through `anstream::stdout()` (an `AutoStream`: strips escapes when not a TTY, honours `NO_COLOR`/`CLICOLOR_FORCE`), and a global `--color` arg maps to `anstream::ColorChoice::write_global`. **The last line of mutating commands is printed unstyled**, always. Width: `terminal_size` (new dep) with a `COLUMNS` env override for tests; truncate notes and paths with `…` using `unicode-width` (new dep; the renderers already use `→`/`★`/`—`). `siblings::render` transposes when the table width exceeds the terminal, not at `len() <= 6`. `tree`: right-aligned metric column (two passes: collect lines, then pad). The `@N` fork step is already shown (`tree.rs::line`); keep it. | L | metric_goal; #19 and #11 for the pruned/failed row states | Low. Tests run without a TTY, so plain output is unchanged. The risk is width math on wide glyphs; use `unicode-width` rather than `len()`. | `n5_terminal.rs`: no `\x1b[` when piped or with `NO_COLOR`; present with `--color=always`; `--color=always` + `tail -1` == the bare id; at `COLUMNS=80` no line exceeds 80 display columns; transposed layout at a narrow width; aligned metric column. |
| **#14** dynamic completion | Add `clap_complete` (`unstable-dynamic` feature, pinned `=4.5.x` because the API is unstable). `pollard.rs::main`: `CompleteEnv::with_factory(Cli::command).complete()` as the first line (`po.rs` calls the same `main`). Every node argument gets `add = ArgValueCandidates::new(node_candidates)`: `fork, diff (a, b), siblings, show, prune, unprune, pin, unpin, note, apply, log, export`. `node_candidates` uses `Repo::open_read_only` (N1), with ids from `SELECT id FROM nodes`, pin names, `@`, `@-`; outside a repo it returns an empty list. README: `source <(COMPLETE=bash po)` / zsh / fish lines. | M | N1 (`open_read_only`) | The unstable API may change between clap_complete minors; the exact pin plus a smoke test contain it. The completer must never migrate or write. | `n5_terminal.rs`: `COMPLETE=bash po -- po fork <prefix>` (the env protocol) lists matching ids and pins; outside a repo the output is empty and the exit code is 0; < 200 ms on 1,000 nodes (in the slow test). |

### N6: data hashing and capture correctness

| Item | Approach (files) | Size | Depends on | Risk / format | Tests |
|---|---|---|---|---|---|
| **S3 data roots by listing** | `wc.rs::data_manifest`: for a `s3://` root, `pollard_remote::Remote::open(root)` → a new `Remote::list_meta(prefix) -> Vec<(key, size, e_tag)>` → one `Entry{path: "s3://bucket/key", size, hash: b3(etag), mode: 0}` per object. D-2 (blocking) is unchanged. | S | R2 (to test it) | **One-time hash change** for repos with remote roots: the next run shows a data delta and misses the duplicate check. This is in PLAN §4. No schema change. | `n6_data.rs` (MinIO-gated): a changed object → a new hash; relisting unchanged → the same hash. |
| **§12 tag/stat modes** | `config.rs`: `data: Vec<DataRoot>`, `#[serde(untagged)] enum DataRoot { Path(String), Tag{tag}, Stat{path, mode} }`, so the plain string stays valid. `wc.rs::data_manifest`: Tag → one entry `path: "tag:<tag>"`, `hash = b3(tag)`, reads nothing; Stat → per-file `hash = b3(size ‖ mtime_ns)`, never reading content. `delta.rs::summarize_changes` renders a `tag:` pair as `data v2 → v3`. | M | none | No node schema change (§12). An unchanged plain-string config must hash byte-identically to alpha.1. | `n6_data.rs`: tag mode reads nothing (a `chmod 000` dir under the root); a tag bump changes the hash and `siblings` shows `data v2 → v3`; stat mode detects add/remove/resize and misses a same-size edit with mtime restored (the limit is pinned by the test); the alpha.1 fixture's local root keeps the same hash. |
| **Hydra stale config** (bug) | `run.rs::capture_hydra_config`: accept only `.hydra/config.yaml` with `mtime >= run start` (pass the start time from `execute`); otherwise warn "no Hydra config written by this run" and keep the empty config. | S | none | None. | `n6_data.rs`: a stale `.hydra/config.yaml` from an earlier run is not captured. |
| **sdk/hydra duplicate check** | By design (L-5): the config is known only after launch. Options in Q5. Default: no code change. | S if chosen | none | Changing it is a run-protocol change and needs the owner. | Only if chosen. |

### N7: platforms and release hygiene

| Item | Approach | Size | Depends on | Risk | Tests |
|---|---|---|---|---|---|
| **macOS arm64, musllinux x86_64** (+ optional linux aarch64) | `release.yml`: add a `build-binaries` matrix (`macos-14` aarch64-apple-darwin; `x86_64-unknown-linux-musl`) and a `build-wheel` matrix (maturin-action `target: aarch64-apple-darwin`; `manylinux: musllinux_1_2`). `ci.yml`: `cargo test` on `macos-latest`. | M | none (can start day 1) | **macOS test failures are likely:** `/tmp` → `/private/tmp` canonicalization (`Repo::discover`/`open_at` canonicalize, and tests compare paths), SIGINT/process-group behaviour in `cross_cutting::ctrl_c_during_run_marks_node_killed`, and BSD `sed -i` in the journey scripts. Budget for fixes in tests, not product. The bundled `zstd`/`rusqlite` C builds are fine on musl. | CI matrix green; `uv tool install` of each built wheel in its job (Alpine container for musl). |
| **Stale alpha.1 draft asset** | Manual owner action (outside this repo's code): delete `pollard_vcs-0.1.0a1-py3-none-linux_x86_64.whl` from the draft, attach the manylinux wheel that PyPI serves (or none), then publish or delete the draft (PLAN D7). The workflow already builds manylinux (`release.yml` `build-wheel`), so there is no code change. | S | none | None. | Checklist item in RELEASE.md. |
| **Docs** | SPEC v4 (§3 `pruned_at`, pins shape; §4 `prune -r`/`unprune`/`pin`/`tree --pinned`/`diff`/`run` parent rule/`--color`; §5 remote format 2; §8 MSRV 1.89; §9 M4 test), DECISIONS entries. PM owns this. | — | all | — | — |

### Deferred by PLAN (sized for completeness)

| Item | Approach | Size | Notes |
|---|---|---|---|
| **#15** `run --name` | `RunOpts.name`; `ids.rs::validate_user_id`: `[a-z0-9._-]+`, not starting with `@`, not an existing node or pin, **not matching the auto-id shape** `^[a-z]+-[a-z]+-\d+$` (it would collide with another clone's future auto id). `run.rs::start` uses it instead of `repo.next_id()`. Reject it together with `--sweep`. | S | **Remote risk:** user names collide across clones far more often than salted ids ("baseline"), and `sync::pull` refuses the **whole** pull on a collision. Needs a collision story (Q6) before shipping. Agree with deferring. |
| **#16** W&B/MLflow/Neptune from the CLI | A **Python sidecar**, not Rust HTTP clients: when `config.toml` has `forward = ["wandb", …]`, `run.rs::execute` spawns `python -m pollard.forward <backend> --node <id>`, which tails the same `POLLARD_METRICS` file (the protocol is unchanged, and the script needs no change). The CLI passes node JSON (parent, `fork_step`, resolved config, recipe hashes) on stdin and the final status at exit. **Lineage without stored mappings:** W&B `wandb.init(id=<node id>, fork_from=f"{parent}?_step={fork_step}")`; Neptune `custom_run_id=<node id>`; MLflow tag `pollard.node_id`, with the parent found by `search_runs` on that tag → `nested`/`parent_run_id`. A `forward` failure never fails the run (warn only). The SDK `forward()` stays as a thin fallback. | L | The CLI then depends at runtime on Python for an **optional** feature; the SPEC §8 rule "no runtime dependency on Python" needs an owner exemption (Q7). Test with a `test` backend in `pollard.forward` that writes the events to a file (`m9_python.rs` style); real backends are gated on credentials. |
| **gc objects + `--auto`** | `Store::gc` also sweeps `objects/` entries that appear in a **dead** weights manifest (pruned, not kept) and in no live manifest. This is targeted and not a full mark-sweep of `objects/`, because objects also hold configs, env records, metric blobs, deltas and manifests, and missing one root would lose data. `--auto`: remove the flag, or make it a hidden no-op with a deprecation warning. | M | Correctness matters more than the bytes; the targeted sweep keeps the blast radius to weights only. Agree with deferring. |

---

## 3. Dependency graph

```
N1 (format 2: user_version, pruned_at, pins reshape, rename+stub)
 ├─> N2 #19 prune ───────────────┐
 ├─> N3 #17 pin storage/cmd ─────┼─> #17 rendering (◆, colour) ─┐
 ├─> N4 R1 remote segments ──> R2 MinIO CI ──> S3 data listing  │
 └─> #14 completion (open_read_only)                            │
metric_goal ──> #18 colour/width ───────────────────────────────┘
#13, #11, #12, hydra fix, §12, platforms: independent
```

Hard orderings: N1 before #19, #17, R1 and #14; `pruned_at` before `tree --pinned` rendering (one tree walk and one row-state model); metric_goal before #18 delta colouring; R1 before R2 (so S3 is tested on the new format); R2 before the S3 data listing test.

---

## 4. Recommended technical sequencing

| Phase | Items | Size | Why together |
|---|---|---|---|
| **1. Format 2 (all on-disk and remote changes)** | N1 migration framework and step 1; N4 R1 remote segments; #17 storage half | L + M + (part of M) ≈ 5–6 days | Every format change lands together and is tested against one alpha.1 fixture. After this phase nothing else touches the formats. |
| **2. Behaviour on the new schema** | N2 #19; #13; #11; #12; #17 cmd + `--pinned`; hydra fix; R2 MinIO CI (DEVOPS, in parallel) | L + S + S + S + M + S + M ≈ 8 days | #19 and #17 share the tree row-state model; the S-sized fixes fill in around them. |
| **3. Terminal UX** | metric_goal; #18; #17 highlight rendering; #14 | S + L + (S) + M ≈ 5–6 days | One pass through every renderer; colour, the `◆` marker and width math designed once. |
| **4. Reach (cuttable, in this order)** | Platforms (DEVOPS, can start in phase 1); S3 data listing; §12 tag/stat | M + S + M ≈ 3–4 days | Independent of the rest; §12 is the first cut if the release slips. |

**Totals.** In scope per PLAN: 3 L (N1, #19, #18) + 6 M (R1, R2, #17, #14, §12, platforms) + 7 S (#13, #11, #12, metric_goal, S3 data listing, hydra fix, draft asset) ≈ **23 engineer-days** (L = 3.5, M = 1.5, S = 0.5), about 4½ weeks for one agent, or about 3 weeks with LEAD/SR/DEVOPS in parallel as PLAN §3 assigns. Deferred: #16 L, gc objects M, #15 S ≈ 5–6 more days.

---

## 5. Open technical questions (owner decision needed)

| # | Question | Recommended default |
|---|---|---|
| Q1 | Accept the **loud-fail mechanisms** for alpha.1 binaries: the database moves to `state.sqlite`, with a `db.sqlite/` stub directory, and a poison line is appended to the remote's legacy `nodes.jsonl`? Both are deliberate one-way breaks of alpha.1 clients. | Yes. The alternative is alpha.1 clients silently un-pruning nodes (they ignore `pruned_at`) and overwriting pins. |
| Q2 | Is **no undo across the migration** acceptable (the op-log barrier)? | Yes (PLAN §4 already says "undo before upgrading"). Supporting it would mean decoding legacy snapshots forever. |
| Q3 | **Fallback status** for a pruned node whose pre-prune status isn't in the op log (e.g. pulled already pruned, or ops pruned): `done` if finished, else `killed`? | Yes, and list those ids in the migration notice. The real outcome is unrecoverable; any value is a guess. |
| Q4 | **`prune` preview timing:** the exact bytes-freed preview reads every live chunk map, so it can exceed the §10 200 ms budget. Exempt the interactive preview, keeping `-y` fast? | Yes. Add `prune` (without `-y`) to the §10 list of exempt commands. |
| Q5 | **sdk/hydra duplicate check:** keep warn-only (L-5), or (a) Hydra pre-compose via `python train.py --cfg job` before launch (an extra interpreter start, several seconds), or (b) kill the child with SIGTERM when a duplicate is detected at capture (status `killed`)? | Keep warn-only for this release. (a) is a run-protocol change and slows every Hydra run. |
| Q6 | **#15 names across clones:** if `run --name` returns, should a name collision on `pull` refuse the whole pull (today's rule), or be scoped (e.g. `baseline@<clone>`)? | Decide before un-deferring #15; pins cover naming for now. |
| Q7 | **#16:** allow the CLI to spawn Python for optional forwarding (an exception to SPEC §8 "no runtime dependency on Python")? | Yes, for opt-in `forward = [...]` only; it's the only way to use each tool's official client and lineage API. |
| Q8 | **Remote segment compaction:** one segment per push grows without bound. OK to defer compaction (for example, a future `po remote compact` writing a merged segment) until pulls measurably slow down? | Yes, defer it. Listing is one call; a thousand small segments are fine for an alpha. |
| Q9 | **`gc --auto`:** remove the flag (a CLI break), or keep it as a hidden no-op with a warning? | Hidden no-op with a deprecation warning, removed in the next minor. |

Notes for PM (PLAN.md; not edited here):
- PLAN §4 needs a row for "`prune` in scripts now needs `-y`" (a consequence of PLAN N2's non-TTY refusal).
- PLAN D3 is feasible (Q1).
- PLAN N1's "backup of `db.sqlite` next to it" becomes `.pollard/backup/db-format1.sqlite` under this design.
