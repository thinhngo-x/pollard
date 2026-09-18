# pollard next alpha: release plan

Owner: PM. Companion: `docs/next/TECH.md` (Tech Lead: per-item approach, sizes, dependencies, migration mechanism).
Inputs: SPEC.md v3 (§10, §12), open issues #11–#19 (owner remarks on alpha.1), alpha.1 team reports (TEST_REPORT, D-29, release workflow).
Status: reconciled with TECH.md (2026-09-19). Sizes, dependencies and sequencing come from TECH §2–§4. Size key: S < ½ day, M 1–2 days, L 3+ days. In scope: **about 23 engineer-days**, roughly 3 weeks with LEAD, SR and DEVOPS working in parallel.

---

## 1. Version and theme

**Recommend `0.2.0-alpha.1`** (PyPI `0.2.0a1`), not `0.1.0-alpha.2`.

| | `0.1.0-alpha.2` | `0.2.0-alpha.1` (recommended) |
|---|---|---|
| What it signals | "same line, more fixes" | "breaking change: new node schema, repo and remote format" |
| Cargo | a `pollard-core = "0.1.0-alpha.1"` requirement can pick up alpha.2 on `cargo update`, so the breaking API change arrives unannounced | the 0.x minor bump is Cargo's breaking-change boundary, so nobody upgrades by accident |
| Users | nothing tells them a one-way repo migration is coming | the version itself is the warning; upgrade notes explain what to do |

**Theme:** *Trust the tree.* Nothing hidden, nothing lost, readable at a glance.

---

## 2. Scope

Priority order: (P0) data-loss/data-hiding → (P1) daily-workflow friction → (P2) polish → (P3) integrations.

### In

| Pri | Item | Reason |
|---|---|---|
| P0 | **#19 prune redesign**: node-only by default, `-r` for the subtree, `pruned_at` flag instead of a status, pins and `@` protected, preview and confirm, `po unprune` | Fixes the data-hiding bug (done runs under a pruned node disappear from `tree`) and the silent loss of outcomes. Owner-approved schema change. |
| P0 | **Remote concurrent-push race** (D-29: two pushes can lose one push's `nodes.jsonl` lines) | Silent loss of remote data. The remote format changes in this release anyway, so fix it now. |
| P0 | **Repo and remote migration** from alpha.1 ("format 2") | Needed by #19 and #17; it has to be safe and one command, or nobody upgrades. |
| P0 | **Hydra capture records the previous run's config** (TECH §0: newest `.hydra/config.yaml` with no mtime bound) | Silently wrong recipe on any Hydra run that crashes early. S. |
| P0 | **`Status::parse` maps unknown strings to `Pruned`**; duplicate check must use `pruned_at IS NULL` | Silent data hiding once `pruned` stops being a status. Fixed inside N1 and N2, not separate work. |
| P1 | **#13**: a run from a failed `@` becomes a sibling of the failed node | Owner-confirmed bug. It corrupts the tree's shape on every failed retry. |
| P1 | **#17**: `pin` takes an optional name, pinned nodes are highlighted, `tree --pinned` | Owner-chosen way to highlight runs. It shares the `pins` reshape with the migration. |
| P1 | **#11**: stop collapsing failed subtrees; dim them instead | Owner remark: the tree looks cut off at arbitrary depths. Falls out of the #19 tree change. |
| P1 | **#12**: `diff A B` on siblings shows a direct A-vs-B diff | Daily friction and a small change. `siblings` still gives the parent-relative view. |
| P1 | **Remote data roots hashed from the listing** (`(key, size, etag)` per SPEC §3), not from the URL alone | Silent recipe error: a changed S3 dataset gives the same `data` hash, so the duplicate check and deltas are wrong. S in size, but its test needs S3 CI, so it lands in phase 4 and is the last item cut. |
| P1 | **S3 tested in CI** (MinIO) | The remote format changes; S3 is the main shared remote and was never tested. |
| P2 | **#18 terminal polish**: colour by meaning, `NO_COLOR`/`--color`, width-aware tables, aligned metric column in `tree`, `metric_goal` | Owner chose this over a TUI or web UI. Every command benefits. |
| P2 | **#14 dynamic tab completion** for node ids and pins | Small, and useful every day. |
| P2 | **macOS arm64 and musllinux** builds (binary + wheel) | Reach: alpha.1 is Linux x86_64 only. Second item cut (after §12) if runners are a problem (see D9). |
| P2 | **SPEC §12 tag/stat data modes** | Already approved, no schema change. **Cuttable:** the first item dropped if the release slips. |

### Deferred (next alpha or later)

| Item | Reason |
|---|---|
| #16 W&B/MLflow forwarding from the CLI, tree carried over (L) | Largest item, and last in priority. The SDK forwarder already covers W&B/Neptune. Plan it for the release after this one (TECH suggests a Python sidecar; D14). |
| #15 `run --name` (S) | #17 already gives a way to name any node (`po pin <node> <name>`, usable as a node ref). User-chosen names collide across clones, and today one collision refuses the whole `pull` (D8). |
| `gc` deleting unreferenced Tier-1 objects (M) | Disk use only, no data at risk. Chunks, where most bytes are, are already collected. Caveat: checkpoints under 1 MB live in `objects/` and are never freed. A safe sweep has to be targeted at weights, so it gets its own release. |
| sdk/hydra duplicate check made blocking (TECH Q5) | By design today (L-5). Any fix is a run-protocol change or slows every Hydra run. |
| Remote segment compaction (TECH Q8) | One small segment per push is fine at alpha scale. |
| #11 `tree --expand <node>` | Not needed once failed subtrees stop collapsing. `--pinned` and `--all` cover the rest. |
| #19 extras: `gc --older-than`, `prune --failed` | The issue marks them "maybe later". |
| #18 `siblings` delta colouring by per-key goal beyond `metric_goal` | A global `metric_goal` plus a per-key table is enough for this release; richer rules can wait. |

### Won't do (this line)

| Item | Reason |
|---|---|
| Interactive TUI / local web UI | Owner chose terminal polish (#18). SPEC §11 non-goal. |
| In-place replace of a failed node (#13, first form) | Immutability is load-bearing (pins, `fork_step`, deltas). The sibling rule solves the real problem. |
| Windows builds | SPEC §11 non-goal. |

---

## 3. Milestones

Rules carried over from SPEC §9: ship milestones in order, each one closes only when its tests are green and CHANGELOG has an entry, and the next one does not start while anything is failing. Roles: **LEAD** (lead dev), **SR** (senior dev), **TEST** (tester), **DEVOPS**, **PM**.
Ordering follows TECH §3–§4. Every on-disk and remote format change lands first, together (phase 1), so users migrate once and everything after it builds on a stable format. Hard dependencies: N1 blocks #19, #17, R1 and #14; R1 blocks R2 (S3 CI), which blocks the S3 data-listing test; `metric_goal` blocks #18. #13, #11, #12, the Hydra fix, §12 and platforms don't depend on anything. DEVOPS platform work starts on day 1.

| Phase | # | Milestone | Items (size) | Owner | Phase size |
|---|---|---|---|---|---|
| 1. Format 2 | N1 | Schema v2, migration, remote segments | migration framework + step 1 (L), `Status::parse` fix, R1 remote segments / push race (M), #17 pins storage | LEAD (repo), SR (remote), TEST (alpha.1 fixtures) | 5–6 d |
| 2. Behaviour | N2 | Prune redesign | #19 (L) | SR (+LEAD for `tree`) | ~8 d, shared with N3 |
| 2. Behaviour | N3 | Tree and run workflow | #13 (S), #11 (S), #12 (S), #17 cmd + `--pinned` (M), Hydra stale-config fix (S) | LEAD | (in ~8 d) |
| 2. Behaviour | N4 | S3 in CI | R2 MinIO job (M) | DEVOPS + SR | (in ~8 d) |
| 3. Terminal | N5 | Terminal polish and completion | `metric_goal` (S), #18 (L), #17 highlight (S), #14 (M) | LEAD | 5–6 d |
| 4. Reach (cuttable) | N6 | Data hashing | S3 data roots by listing (S), §12 tag/stat (M, **first cut**) | SR | 3–4 d, with N7 builds |
| 4. Reach (cuttable) | N7 | Platforms, docs, release | macOS arm64 + musllinux (M, second cut), SPEC v4 / DECISIONS / README / upgrade notes, release | DEVOPS + PM + TEST | (in 3–4 d) |

Cut order if the release slips: §12 tag/stat, then platforms, then S3 data listing. Phases 1–3 and the docs are not cuttable.

### N1. Format 2: schema v2, migration, remote segments (LEAD + SR; TEST owns fixtures)

Mechanism is in TECH §1: `PRAGMA user_version`, one-transaction migration, the database moves to `.pollard/state.sqlite` with a `db.sqlite/` stub directory, and remote node lines go into immutable per-push segments plus a `FORMAT` marker.
- [ ] Node record: `pruned_at` timestamp (NULL = not pruned). `status` loses `pruned`. `Status::parse` errors on unknown strings instead of mapping them to `Pruned`.
- [ ] `pins` becomes `(node_id, name NULL)`, with `name` unique when present.
- [ ] Remote: `FORMAT` = 2, node lines in `nodes/<ts>-<salt>-<n>.jsonl` segments (this fixes the push race). A new client reads alpha.1 remotes.
- [ ] TEST: a checked-in alpha.1 fixture repo and remote, made with the published `0.1.0-alpha.1` binary, containing: a subtree prune, a node run under a pruned `@`, a named pin, and a `prune --keep-weights`.

Acceptance
- [ ] Opening the alpha.1 fixture with the new binary migrates it in one step, prints one notice naming the backup, and leaves the backup at `.pollard/backup/db-format1.sqlite`.
- [ ] A repo written by a newer format is refused with "upgrade pollard", not opened.
- [ ] Duplicate check: a pruned `done` node's recipe gives a notice, not a refusal (`pruned_at IS NULL` in the refusal branch; D-19 unchanged).
- [ ] After migration, every node that was `pruned` has `pruned_at` set, and its `status` is its pre-prune outcome, recovered from the prune op's snapshot. If that can't be recovered, it gets a documented fallback and is listed in the notice.
- [ ] After migration, the "new work under pruned `@`" node from the fixture shows in plain `po tree` (the #19 data-hiding bug is fixed on existing repos).
- [ ] Named pins resolve as node refs exactly as before migration. `siblings` headers are unchanged.
- [ ] `po undo` right after migration does not cross the migration: it refuses with a message, or undoes only post-migration ops.
- [ ] Migration is idempotent (running it on a v2 repo does nothing), and a migration killed partway (after `state.sqlite.tmp` is created) leaves the repo either v1 and intact or v2 and complete.
- [ ] `po push`/`pull` against the alpha.1 fixture remote works. After a push, the remote carries `FORMAT` = 2.
- [ ] Two clones × 20 concurrent pushes to one local-path remote, then both `pull`: no node from either clone is missing (the push race is fixed).
- [ ] Unnamed and named pins round-trip through push/pull. `pruned_at` syncs: prune in X, push, pull in Y, and Y's `tree` shows the placeholder.
- [ ] `siblings --json` output for the fixture is byte-identical before and after migration (public format, frozen since M2).
- [ ] In CI, with the published alpha.1 binary installed: alpha.1 exits non-zero, without writing anything, on a migrated repo (any command) and on a converted remote (`push` and `pull`) (D2).

### N2. Prune redesign (#19) (SR; LEAD wires `tree`)

Acceptance
- [ ] `po prune X` on a node with children sets `pruned_at` on X only. Children keep their status, and `po tree` shows X as one dimmed placeholder line with its children under it.
- [ ] `po prune -r X` sets `pruned_at` on X and every descendant.
- [ ] `po prune` on `@`, on a pinned node, or (with `-r`) on a subtree containing either, is refused with a non-zero exit that lists every offender. `--force` overrides.
- [ ] Without `-y`, `prune` prints the affected node ids and the bytes `gc` would free, then asks. Answering no changes nothing (the op log has no new entry). `--dry-run` prints the same thing and never asks. On a non-TTY stdin without `-y`, it refuses rather than hanging.
- [ ] The preview's byte count equals exactly what the next `gc` frees on the M4 50 MB checkpoint fixture.
- [ ] A pruned node's `po show` status reads `done`/`failed`/`killed`, plus `pruned <timestamp>`.
- [ ] `po unprune X` (and `-r`) clears the flag without touching later ops. After `gc` it restores the node and warns that the weights are gone.
- [ ] Regression (#19 repro): prune `@`'s parent with `--force`, run again from `@`, and the new done node shows in plain `po tree`.
- [ ] `siblings` hides pruned children unless `--all`. The duplicate check treats a pruned match as a notice, not a refusal (D-19 unchanged).
- [ ] `fork --step` through a pruned ancestor still inherits metrics. After `gc` it fails with an error that names the node.
- [ ] `--keep-weights` still hides without freeing: `gc` frees 0 bytes for that node.
- [ ] The 1,000-node timing test runs `prune -y` and stays under 200 ms. The interactive preview is exempt from the §10 budget (D10).
- [ ] `undo` of a `--keep-weights` prune also clears the keep-weights mark (a pre-existing bug), and `unprune` clears it too.
- [ ] `gc --auto` prints a deprecation warning and behaves like `gc` (D11).
- [ ] SPEC §9 M4 prune/gc test updated and green. Existing `m4`, `cross_cutting` and `journeys` prune steps pass `-y` (or `-r`).

### N3. Tree and run workflow (#13, #11, #17, #12) (LEAD)

Acceptance
- [ ] #13: root → `buggy` (fails) → edit config → `po run` gives a new node whose parent is root, with a one-line notice naming `buggy`. The last output line is still the node id.
- [ ] #13: after `po fork buggy --step N`, the next `run` is a **child** of `buggy` with `fork_step=N` (resuming from a failed run's checkpoint stays possible).
- [ ] #13: `po run --parent buggy` makes a child of `buggy`, because an explicit parent wins. A `killed` `@` keeps today's behaviour (a child), per D6.
- [ ] #11: a failed node with children is shown with its children, never as `… N collapsed`. Failed rows are dimmed on a TTY and marked `failed` in plain text.
- [ ] #17: `po pin X` with no name succeeds. `po pin X paper` adds the name, and `paper` resolves as a node ref. `po unpin X` and `po unpin paper` both work. Both are undoable.
- [ ] #17: pinned nodes carry `◆` in `tree` when not on a TTY or when `NO_COLOR` is set, and are highlighted on a TTY. `★` stays reserved for `--metric`.
- [ ] #17: `po tree --pinned` on a 1,000-node repo with 3 pins prints exactly the 3 pinned nodes plus their root paths, in under 200 ms.
- [ ] #17: `po pin X <name>` is refused when `<name>` equals an existing node id, so the pin can't be silently shadowed.
- [ ] #17: unnamed pins sync through `push`/`pull` like named ones (D7 default).
- [ ] #12: `po diff A B` on siblings prints a direct A→B diff (e.g. `x 10→20`, not `1→10` / `1→20`). The parent-relative view is still available through `po siblings`, and through a `diff` flag if D5 keeps one. A direct diff against an imported root with no stored manifest works rather than erroring.
- [ ] Hydra: a stale `.hydra/config.yaml` left by an earlier run (mtime before this run's start) is not captured. The run warns "no Hydra config written by this run".

### N4. S3 in CI (DEVOPS + SR)

Acceptance
- [ ] A new CI job (MinIO service container) runs the M7 suite and the N1 remote tests (concurrent push, pins, `pruned_at`, alpha.1 conversion) against `s3://` on every PR.
- [ ] A second `push` with nothing new transfers 0 bytes on S3 too (M7 unchanged). The one-time full re-push of node lines after the upgrade is expected and excluded.
- [ ] S3 bugs this job finds are fixed in this release (TECH expects some in prefix listing and ranged gets).

### N5. Terminal polish and completion (#18, #14) (LEAD)

Acceptance
- [ ] With stdout not a TTY, or with `NO_COLOR=1`, output contains no ANSI escapes (grep for `\x1b[`). `--color=always` forces them, `--color=never` removes them.
- [ ] Status colours: done green, failed red, killed yellow, running cyan, pruned dim. `@` is bold.
- [ ] At `COLUMNS=80`, no `tree` or `siblings` line is longer than 80 display columns. Long notes and paths end in `…`.
- [ ] `siblings` switches to the transposed layout when the columns don't fit the width, not at a fixed 6 columns.
- [ ] `tree --metric k` values are right-aligned in one column. The `@N` fork step shows on edges.
- [ ] `metric_goal = "max"` (global) and a per-key table in `config.toml` override the name-based guess (L-8). Test: key `loss` with goal `max` puts ★ on the highest value.
- [ ] Every mutating command's last line is still the bare node id with colour on (`--color=always | tail -1` equals the id with no escapes).
- [ ] #14: `COMPLETE=fish po fork ` (and bash, zsh) lists live node ids, pin names, `@` and `@-` for every node-taking argument. In a 1,000-node repo it answers in under 200 ms. README has install lines for bash, zsh and fish.
- [ ] Completion outside a pollard repo prints nothing and exits 0. Completion on an alpha.1 (unmigrated) repo prints nothing, and never migrates or writes.

### N6. Data hashing (SR). Cuttable; §12 is the first cut.

Acceptance
- [ ] Remote data root: changing one object's contents (new etag) under the prefix changes the node's `data` hash. Listing again with nothing changed gives the same hash. Runs against MinIO.
- [ ] Upgrade note written: the first run after upgrade shows a `data` delta for repos with remote roots (a one-time hash change).
- [ ] §12 tag mode: `data = [{ tag = "v3" }]` reads no files (test with an unreadable directory present). Bumping the tag changes the hash, and `siblings` shows `data v2 → v3`.
- [ ] §12 stat mode: adding, removing or resizing a file changes the hash. A same-size edit with mtime restored does not (a documented limit, tested so it can't change silently).
- [ ] The plain-string form `data = ["./data"]` gives the same hash as alpha.1 for an unchanged local root.

### N7. Platforms, docs, release (DEVOPS, PM, TEST)

Acceptance
- [ ] The release workflow builds `pollard`/`po` tarballs and wheels for x86_64 Linux gnu, x86_64 musllinux, and aarch64 macOS. CI runs `cargo test` on `macos-latest`.
- [ ] `uv tool install pollard-vcs==0.2.0a1` works on a clean macOS arm64 machine and in an Alpine container.
- [ ] Journeys A–D and the new upgrade journey (below) pass end to end. The 1,000-node release timing test passes, with results in TEST_REPORT.
- [ ] SPEC bumped to v4: §3 (pruned flag, pins shape), §4 (`prune -r`, `unprune`, `pin` optional name, `tree --pinned`, `diff`, `run` parent rule, `--color`), §5 (remote format 2), §9 (M4 test), §10 (interactive `prune` preview exempt from 200 ms), §12 (done or still planned), and a revision log. DECISIONS gains entries for every owner decision below.
- [ ] Stale-doc fix (PM): SPEC §8 and DECISIONS D-11 still say MSRV 1.85; `Cargo.toml` and the CI msrv job use **1.89**. Correct both in the SPEC v4 pass.
- [ ] README: status table, the tree example without `pruned` as a status, completion setup, the platform line, and a link to the upgrade notes.

**Upgrade journey (new, TEST):** alpha.1 user with a repo and a local remote → install 0.2.0a1 → `po tree` (migrates, prints notice) → `po push` → a second clone on 0.2.0a1 does `po pull` → both trees are identical and show the previously hidden node.

---

## 4. Compatibility and upgrade

| Area | alpha.1 → 0.2.0 | User action |
|---|---|---|
| Repo (`.pollard/`) | One-way migration to format 2 on first open. The database moves to `.pollard/state.sqlite`, and `db.sqlite` becomes a stub directory so alpha.1 binaries fail instead of misreading it. The backup is at `.pollard/backup/db-format1.sqlite` | None. Keep the backup until satisfied. Rollback: `rm -r .pollard/db.sqlite .pollard/state.sqlite && mv .pollard/backup/db-format1.sqlite .pollard/db.sqlite`, then reinstall alpha.1 |
| Old binaries | alpha.1 fails with an error, and writes nothing, on a migrated repo or a converted remote | Upgrade every machine and CI image that touches the repo or remote |
| `prune` in scripts | Now asks for confirmation, and refuses on a non-TTY stdin without `-y`. Also node-only by default | Add `-y` to scripted prunes, and `-r` where the whole subtree was meant |
| Pruned nodes | `pruned` status → `pruned_at` flag. Outcome recovered from the op log. Subtree prunes from alpha.1 stay pruned; nodes created under them after the prune become visible | Review `po tree` once, and re-prune if needed |
| Op log | `undo` does not cross the migration | Undo anything you want undone **before** upgrading |
| Pins | Existing named pins unchanged; unnamed pins become possible | None |
| Remote | The first push by a 0.2 client converts the remote (`FORMAT`, per-push segments) and re-sends every node line once (KBs per node, no blobs). The new client reads the old format | **Everyone on a shared remote upgrades together.** After conversion, alpha.1 `push`/`pull` fail loudly |
| `gc --auto` | Deprecated: warns, then runs as plain `gc` | Drop `--auto` from scripts |
| Remote data roots | `data` hash now comes from the listing | Expect a one-time `data` delta on the next run |
| `siblings --json` | Unchanged | None |
| `diff A B` on siblings | Now direct A-vs-B | Scripts that parsed the parent-relative `diff` output use `siblings --json` instead |
| Colour | On by default on a TTY | Scripts piping output are unaffected. `NO_COLOR` or `--color=never` turns it off |
| Run protocol, Python SDK | Unchanged | Upgrade `pollard-vcs` to get the bundled binary |

**CHANGELOG requirement:** the `0.2.0-alpha.1` entry opens with an **Upgrade notes** section (the table above, in user terms, with the rollback steps), before the feature list. PM writes it. TEST confirms every row against the upgrade journey. The GitHub release notes copy it verbatim.

---

## 5. Release checklist delta vs `docs/RELEASE.md`

| Change | What's different now |
|---|---|
| Version | `0.2.0-alpha.1` / PyPI `0.2.0a1` in `Cargo.toml` and `python/pyproject.toml`. Path deps between crates that carry `version =` bumped too. |
| MSRV | CI has an `msrv` job building `--locked` on `rust-version` (now **1.89**, not SPEC's 1.85). It must be green. Update SPEC §8 and D-11. |
| PyPI | **Automated**: pushing the `v*` tag runs `publish-pypi` (manylinux wheel, `PYPI_API_TOKEN`). The tag is the point of no return for PyPI. RELEASE.md §4's manual `uv publish` step is obsolete. Precondition: the secret exists and a `workflow_dispatch` build-only run is green. |
| crates.io | Still manual, in order objects → git → remote → core → cli, **before** pushing the tag, so the tag never points at unpublished crates. |
| GitHub release | Created as a **draft** by the workflow. Before publishing: the asset list is exactly this version's tarballs and wheels. Delete the stale wheel on the alpha.1 draft release, and publish or delete that draft (D16). |
| Platforms | New matrix (musllinux, macOS arm64) must be green. Every wheel is installed by `uv tool install` in CI before the tag. |
| New gates | MinIO S3 job green; upgrade journey and alpha.1 loud-fail tests green against the checked-in alpha.1 fixture; release-mode 1,000-node timing test green (not run in CI). |
| New dependencies | `clap_complete` (pinned `=4.5.x`, `unstable-dynamic`), `terminal_size`, `unicode-width`. All must build on MSRV 1.89 (`msrv` job). |
| Docs gates | CHANGELOG has a dated entry with **Upgrade notes** first. SPEC v4 and DECISIONS updated. README status and platform lines updated. |
| Unchanged | fmt, clippy `-D warnings`, `cargo publish --dry-run` per crate, rollback notes (yank / supersede). Nothing is published without owner go-ahead. |

---

## 6. Decisions needed from the owner

Merged with TECH §5 Q1–Q9 (TECH id in brackets). Every item has a default, so work starts without waiting and anything the owner overrides is changed later.

| # | Question | Recommended default |
|---|---|---|
| D1 | Version: `0.2.0-alpha.1` (breaking-format signal) or `0.1.0-alpha.2`? | `0.2.0-alpha.1` / PyPI `0.2.0a1` (§1). |
| D2 | Make alpha.1 binaries fail loudly by moving the database to `state.sqlite` with a `db.sqlite/` stub, and appending a poison line to the remote's legacy `nodes.jsonl`? Both are deliberate one-way breaks. [Q1] | Yes. Otherwise alpha.1 clients silently un-prune nodes and overwrite pins. |
| D3 | Migration: automatic on first open (with a backup), and no `undo` across it (op-log barrier)? [Q2] | Yes to both. The upgrade notes say "undo before upgrading". |
| D4 | Fallback status for a pruned node whose pre-prune outcome isn't in the op log [Q3] | `done` if `finished_at` is set, else `killed`. The migration notice lists those ids. |
| D5 | `diff A B` on siblings: direct only, or keep a flag for the parent-relative view? | Direct only. `siblings` is the parent-relative view. |
| D6 | #13: does a `killed` `@` also make the next run a sibling? | No, only `failed`. A killed run often has checkpoints worth building on. Explicit `--parent` and a pending `fork --step` always win. |
| D7 | Do unnamed pins sync on `push`? | Yes, same as named pins. No `--local` until someone asks. |
| D8 | #15 `run --name`: defer, and what happens on a cross-clone name collision if it returns? [Q6] | Defer; pins cover naming for now. Settle the collision rule (refuse the whole pull, or scope it as `name@clone`) before bringing it back. |
| D9 | macOS arm64 and musllinux in this release? | Yes, as best-effort platforms (CI-tested only). Cut them rather than delay the release. |
| D10 | Exempt the interactive `prune` preview (exact bytes freed) from the §10 200 ms budget, keeping `prune -y` fast? [Q4] | Yes. Add it to the §10 exempt list in SPEC v4. |
| D11 | `gc --auto`: remove the flag, or deprecate it? [Q9] | Hidden no-op alias with a deprecation warning; remove it in the next minor. |
| D12 | sdk/hydra duplicate check stays warn-only (L-5)? [Q5] | Yes for this release. The alternatives are a run-protocol change or slow every Hydra launch. |
| D13 | Remote segments: defer compaction? [Q8] | Yes, until pulls measurably slow down. |
| D14 | #16 (deferred): may the CLI spawn Python for opt-in forwarding, as an exception to SPEC §8's "no runtime Python"? [Q7] | Yes, for `forward = [...]` only. Decide now so #16 can be planned next. |
| D15 | §12 tag/stat modes: in this release (cuttable), or push to the next? | In, as the last item and the first cut. |
| D16 | The alpha.1 draft GitHub release has a stale `linux_x86_64` wheel | Delete that asset, attach the manylinux wheel PyPI serves, and publish the draft so the history is complete. |
