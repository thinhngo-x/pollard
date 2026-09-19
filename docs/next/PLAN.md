# pollard next alpha: release plan

Owner: PM. Companion: `docs/next/TECH.md` (Tech Lead: per-item approach, sizes, dependencies, migration mechanism).
Inputs: SPEC.md v3 (§10, §12), open issues #11–#19 (owner remarks on alpha.1), alpha.1 team reports (TEST_REPORT, D-29, release workflow).
Status: **owner decisions applied (2026-09-19)**: D1–D18 in §6 are DECIDED (D19, lossless pins, is open with a default); the resumed label and `prune --failed`/`--killed` are in; SPEC §12 is deferred. Reconciled with TECH.md. Sizes, dependencies and sequencing come from TECH §2–§4. Size key: S < ½ day, M 1–2 days, L 3+ days. In scope: **about 23 engineer-days** (L = 3.5, M = 1.5, S = 0.5).
Team: **PM**, **PO** (stories and acceptance in `docs/next/BACKLOG.md`), **Lead dev** (implements everything, CI and release workflows included), **Tester** (fixtures, acceptance and regression tests). With one implementer the calendar is about 4½–5 weeks; the Tester works in parallel.

---

## 1. Version and theme

**Decided (D1): `0.2.0-alpha.1`** (PyPI `0.2.0a1`), not `0.1.0-alpha.2`.

| | `0.1.0-alpha.2` | `0.2.0-alpha.1` (chosen) |
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
| P1 | **#13**: a run from a failed or killed `@` becomes a sibling of it; resuming (`fork --step`) makes a child | Owner-confirmed bug. It corrupts the tree's shape on every failed retry. |
| P1 | **#17**: `pin` takes an optional name, pinned nodes are highlighted, `tree --pinned` | Owner-chosen way to highlight runs. It shares the `pins` reshape with the migration. |
| P1 | **Resumed label** (derived, not stored): a failed/killed node with ≥1 unpruned child forked from it with `--step` renders as `killed · resumed @30000` | Owner decision (#19 comment). Once #13 makes plain retries siblings, the only children of a dead node are resumes; the label says so without a new status. Text only: `tree`/`show`/`siblings`, never `--json` or stored status. S, phase 2 with #13; colour in phase 3 with #18. |
| P1 | **#11**: a node with children never collapses in `tree`, except a pruned node | Owner remark: the tree looks cut off at arbitrary depths. Falls out of the #19 tree change. |
| P1 | **#12**: `diff A B` on siblings shows a direct A-vs-B diff | Daily friction and a small change. `siblings` still gives the parent-relative view. |
| P1 | **Remote data roots hashed from the listing** (`(key, size, etag)` per SPEC §3), not from the URL alone | Silent recipe error: a changed S3 dataset gives the same `data` hash, so the duplicate check and deltas are wrong. S in size, but its test needs S3 CI, so it lands in phase 4 and is the last item cut. |
| P1 | **S3 tested in CI** (MinIO) | The remote format changes; S3 is the main shared remote and was never tested. |
| P1 | **`po prune --failed` / `--killed [<node>]`** (bulk prune of dead ends) | Owner decision (#19 comment; was a deferred #19 extra). Cleans up after a sweep with crashes. Reuses the #19 preview/confirm path. S, end of N2. |
| P2 | **#18 terminal polish**: colour by meaning, `NO_COLOR`/`--color`, width-aware tables, aligned metric column in `tree`, `metric_goal` | Owner chose this over a TUI or web UI. Every command benefits. |
| P2 | **#14 dynamic tab completion** for node ids and pins | Small, and useful every day. |
| P2 | **macOS arm64 and musllinux** builds (binary + wheel) | Reach: alpha.1 is Linux x86_64 only. **Cuttable: the first item dropped** if the release slips or runners are a problem (D9). |

### Deferred (next alpha or later)

| Item | Reason |
|---|---|
| #16 W&B/MLflow forwarding from the CLI, tree carried over (L) | Largest item, and last in priority. The SDK forwarder already covers W&B/Neptune. Plan it for the release after this one (TECH suggests a Python sidecar; D14). |
| #15 `run --name` (S) | #17 already gives a way to name any node (`po pin <node> <name>`, usable as a node ref). User-chosen names collide across clones, and today one collision refuses the whole `pull` (D8). |
| `gc` deleting unreferenced Tier-1 objects (M) | Disk use only, no data at risk. Chunks, where most bytes are, are already collected. Caveat: checkpoints under 1 MB live in `objects/` and are never freed. A safe sweep has to be targeted at weights, so it gets its own release. |
| sdk/hydra duplicate check made blocking (TECH Q5) | By design today (L-5). Any fix is a run-protocol change or slows every Hydra run. |
| Remote segment compaction (TECH Q8) | One small segment per push is fine at alpha scale. |
| #11 `tree --expand <node>` | Not needed once failed subtrees stop collapsing. `--pinned` and `--all` cover the rest. |
| SPEC §12 tag/stat data modes (M) | Owner decision (D15): moved to the release after this one to keep the budget. No schema change, so nothing here blocks it. |
| #19 extra: `gc --older-than` (S) | "Maybe later" in the issue. (`prune --failed`/`--killed` moved to In.) |
| #18 `siblings` delta colouring by per-key goal beyond `metric_goal` | A global `metric_goal` plus a per-key table is enough for this release; richer rules can wait. |

### Won't do (this line)

| Item | Reason |
|---|---|
| Interactive TUI / local web UI | Owner chose terminal polish (#18). SPEC §11 non-goal. |
| In-place replace of a failed node (#13, first form) | Immutability is load-bearing (pins, `fork_step`, deltas). The sibling rule solves the real problem. |
| Windows builds | SPEC §11 non-goal. |

---

## 3. Milestones

Rules carried over from SPEC §9: ship milestones in order, each one closes only when its tests are green and CHANGELOG has an entry, and the next one does not start while anything is failing. Roles: **Lead dev**, **Tester**, **PM**, **PO**. Earlier drafts split the work into LEAD/SR/DEVOPS lanes; on this team the Lead dev owns all of them.
Ordering follows TECH §3–§4. **This table is authoritative.** Every on-disk and remote format change lands first, together (phase 1), so users migrate once and everything after it builds on a stable format. Hard dependencies: N1 blocks #19, #17, R1 and #14; R1 blocks R2 (S3 CI), which blocks the S3 data-listing test; #19 blocks `prune --failed`; #13 comes before the resumed label; `metric_goal` blocks #18. #13, #11, #12, the Hydra fix and platforms don't depend on anything.

| Phase | # | Milestone | Items (size) | Owner | Phase size |
|---|---|---|---|---|---|
| 1. Format 2 | N1 | Schema v2, migration, remote segments | migration framework with `PRAGMA user_version` + step 1 (L): `pruned_at`, `Status::parse` fix, op-log barrier, alpha.1 loud failure; R1 remote segments + `FORMAT` marker, fixing the concurrent-push race (M); #17 pins storage `(node_id, name NULL)` | Lead dev; Tester (alpha.1 fixture, acceptance, upgrade journey) | 5–6 d |
| 2. Behaviour | N2 | Prune redesign | #19 (L), then `prune --failed`/`--killed` (S) at the end | Lead dev; Tester | ~9–10 d, shared with N3/N4 |
| 2. Behaviour | N3 | Tree and run workflow | #13 (S), resumed label (S, text only), #11 (S), #12 (S), #17 cmd + `--pinned` (M), Hydra stale-config fix (S) | Lead dev; Tester | (in ~9–10 d) |
| 2. Behaviour | N4 | S3 in CI | R2 MinIO job (M) | Lead dev; Tester | (in ~9–10 d) |
| 3. Terminal | N5 | Terminal polish and completion | `metric_goal` (S), #18 (L) incl. resumed-label colour, #17 highlight (S), #14 (M) | Lead dev; Tester | 5–6 d |
| 4. Reach (cuttable) | N6 | Data hashing | S3 data roots by listing (S, **second cut**) | Lead dev | 2–3 d, with N7 |
| 4. Reach (cuttable) | N7 | Platforms, docs, release | macOS arm64 + musllinux (M, **first cut**), draft-asset cleanup (S), SPEC v4 / DECISIONS / README / upgrade notes, release | Lead dev (platforms), PM (docs), Tester (journeys, timing) | (in 2–3 d) |

Cut order if the release slips: **platforms first, then S3 data listing.** Phases 1–3 and the docs are not cuttable. (§12 tag/stat is no longer in this release; D15.) Task order, owners and the done criteria for phase 1 are in §7.

### N1. Format 2: schema v2, migration, remote segments (Lead dev; Tester owns the fixture and tests)

Mechanism is in TECH §1: `PRAGMA user_version`, one-transaction migration, the database moves to `.pollard/state.sqlite` with a `db.sqlite/` stub directory, and remote node lines go into immutable per-push segments plus a `FORMAT` marker.
- [ ] Node record: `pruned_at` timestamp (NULL = not pruned). `status` loses `pruned`. `Status::parse` errors on unknown strings instead of mapping them to `Pruned`.
- [ ] `pins` becomes `(node_id, name NULL)`, with `name` unique when present (storage only; the `pin`/`unpin` command changes are N3).
- [ ] Migration framework: `PRAGMA user_version`, `MIGRATIONS` run in one transaction, backup via `VACUUM INTO`, `SCHEMA` frozen at the alpha.1 shape (new repos migrate at `init`), `Repo::open_read_only` for #14.
- [ ] Op-log barrier: `meta.migrated_at_op`; `undo` refuses ops at or below it.
- [ ] alpha.1 loud failure: database moves to `.pollard/state.sqlite`, `db.sqlite/` stub directory; poison line in the remote's legacy `nodes.jsonl` (D2).
- [ ] Remote: `FORMAT` = 2, node lines in `nodes/<ts>-<salt>-<n>.jsonl` segments (this fixes the push race), pins as segment records (one record per pin add/remove if D19's default holds). A new client reads alpha.1 remotes.
- [ ] Every `Status::Pruned` / `'pruned'` use switched to `pruned_at` (TECH N1, 9 sites). `prune` keeps its alpha.1 subtree behaviour until N2 (no protection, no prompt), but sets `pruned_at` instead of `status`. `tree` gains the placeholder guard now: a pruned node with unpruned descendants renders as a placeholder line so they stay visible.
- [ ] Tester: a checked-in alpha.1 fixture repo and remote (`crates/pollard-cli/tests/fixtures/alpha1/`, generated once by `make.sh` with the published `0.1.0-alpha.1` binary), containing at least: a subtree prune over done/failed/killed nodes, a node run under a pruned `@`, two named pins on one node, a `prune --keep-weights`, a node pulled already pruned (fallback case), and a prune/undo/prune. Full list and golden outputs: BACKLOG F0.

Acceptance
- [ ] Opening the alpha.1 fixture with the new binary migrates it in one step, prints one notice naming the backup, and leaves the backup at `.pollard/backup/db-format1.sqlite` as a **standalone** copy (`VACUUM INTO`, readable without any `-wal`/`-shm`; an existing backup is never overwritten).
- [ ] A repo written by a newer format is refused with "upgrade pollard", not opened.
- [ ] Duplicate check: a pruned `done` node's recipe gives a notice, not a refusal (`pruned_at IS NULL` in the refusal branch; D-19 unchanged).
- [ ] After migration, every node that was `pruned` has `pruned_at` set, and its `status` is its pre-prune outcome, recovered from the prune op's snapshot. If that can't be recovered, it gets a documented fallback and is listed in the notice.
- [ ] After migration, the "new work under pruned `@`" node from the fixture shows in plain `po tree`, under its pruned parent rendered as a placeholder line (`done (pruned)`; dimming comes in N5). A fully pruned subtree stays hidden (the #19 data-hiding bug is fixed on existing repos; BACKLOG F6).
- [ ] Named pins resolve as node refs exactly as before migration. `siblings` headers are unchanged.
- [ ] `po undo` right after migration does not cross the migration: it refuses (D3). An `undo n` that would cross the upgrade is refused whole, never partly applied; ops after the migration undo normally. Wording: BACKLOG F5.
- [ ] Migration is idempotent (running it on a v2 repo does nothing), and a migration killed partway (after `state.sqlite.tmp` is created) leaves the repo either v1 and intact or v2 and complete.
- [ ] `po push`/`pull` against the alpha.1 fixture remote works. After a push, the remote carries `FORMAT` = 2.
- [ ] Two clones × 20 concurrent pushes to one local-path remote, then both `pull`: no node from either clone is missing (the push race is fixed). With D19's default, concurrent pin edits from both clones also both survive.
- [ ] Named and unnamed pins round-trip through push/pull (the test stores an unnamed pin directly; the `pin` command gains it in N3). `pruned_at` syncs: prune in X, push, pull in Y, and Y's `po show` reads the outcome status plus `pruned <ts>`, and Y's `tree` matches X's.
- [ ] `siblings --json` output for the fixture is byte-identical before and after migration (public format, frozen since M2).
- [ ] In CI, with the published alpha.1 binary installed: alpha.1 exits non-zero, without writing anything, on a migrated repo (any command) and on a converted remote (`push` and `pull`) (D2).
- [ ] Upgrade journey (Tester, from the checked-in fixture): alpha.1 repo + local remote → new binary → `po tree` (migrates, one notice) → `po push` (converts the remote) → a fresh clone on the new binary does `po pull` → both `po tree --all` outputs are identical and the previously hidden node shows in plain `po tree` in both. N7 reruns it end to end.

### N2. Prune redesign (#19) and bulk prune (Lead dev; Tester)

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

Bulk prune: `po prune --failed` / `--killed [<node>]` (S, lands after the #19 boxes above are green)
- [ ] `--failed` selects every **dead end** with status `failed`: no unpruned children (a node whose children are all pruned counts). `--killed` does the same for `killed`. Both flags together select the union. Without either flag, `po prune <node>` is the plain #19 command.
- [ ] With `<node>`, selection is limited to `<node>`'s subtree, `<node>` included. Without it, the whole repo.
- [ ] Never selected: `done` and `running` nodes, already-pruned nodes, and failed/killed nodes with an unpruned child (e.g. a resumed node).
- [ ] One pass per call: in root → `a` (failed) → `b` (failed, leaf), the first `prune --failed -y` prunes only `b`; the second prunes `a`.
- [ ] A pinned node or `@` that matches is skipped with a notice naming it, and the command still succeeds for the rest (exit 0). Plain `po prune <pinned>` still refuses (unchanged #19 rule).
- [ ] Sweep members are included; the preview groups them under their sweep (e.g. `sweep lr-grid: 3 failed`) and lists the other nodes on their own.
- [ ] Same preview/confirm path as #19: node ids and bytes freed, then ask; `-y` skips the question; `--dry-run` prints and never asks or writes; non-TTY stdin without `-y` refuses. Nothing selected → a one-line message, exit 0, no op written.
- [ ] Node-only: only selected nodes get `pruned_at`; their statuses read `failed`/`killed` in `po show`. The op log gains exactly **one** entry, and one `po undo` restores every node it pruned.
- [ ] Regression: after a 32-seed sweep with 5 crashed members, `po prune --failed -y` prunes exactly those 5 and no other node.

### N3. Tree and run workflow (#13, resumed label, #11, #17, #12) (Lead dev; Tester)

Acceptance
- [ ] #13: root → `buggy` (fails) → edit config → `po run` gives a new node whose parent is root, with a one-line notice naming `buggy`. The last output line is still the node id.
- [ ] #13: the same with a `killed` `@` (Ctrl-C during the run): the next plain `po run` is a sibling of the killed node.
- [ ] #13: resuming is the exception. After `po fork buggy --step N` (failed) or `po fork blue-elk-5 --step N` (killed, Journey C), the next `run` is a **child** with `fork_step=N`.
- [ ] #13: `po run --parent buggy` makes a child of `buggy`, because an explicit parent wins.
- [ ] Resumed label (Journey C): killed `blue-elk-5` → `po fork blue-elk-5 --step 30000` → `po run` → `po tree`, `po show blue-elk-5` and `po siblings` (parent of `blue-elk-5`) all read `killed · resumed @30000`. The same for a failed node: `failed · resumed @N`.
- [ ] Resumed label, several resumes: a second child forked at step 20000 gives `killed · resumed @20000 @30000` (ascending, each step once).
- [ ] Resumed label counts only unpruned children with `fork_step` set: a child made with `--parent` and no `--step` does not count; pruning the only resume child removes the label, and `po unprune` brings it back.
- [ ] Resumed label never on `done`: a done node with a `--step` child reads plain `done`. `running` nodes never get it either.
- [ ] Resumed label is text only: `siblings --json` output and the stored `status` column are unchanged (`killed`), and no JSON output contains `resumed`.
- [ ] #11: a node with children (done, failed, killed or running) is always shown with its children, never as `… N collapsed`.
- [ ] #11: a pruned node collapses: hidden unless `--all`, as today. Guard: if a pruned node still has non-pruned descendants (node-only prune, #19), it is shown as a dimmed placeholder with those descendants instead, so live runs are never hidden.
- [ ] #17: `po pin X` with no name succeeds. `po pin X paper` adds the name, and `paper` resolves as a node ref. `po unpin X` and `po unpin paper` both work. Both are undoable.
- [ ] #17: pinned nodes carry `◆` in `tree` when not on a TTY or when `NO_COLOR` is set, and are highlighted on a TTY. `★` stays reserved for `--metric`.
- [ ] #17: `po tree --pinned` on a 1,000-node repo with 3 pins prints exactly the 3 pinned nodes plus their root paths, in under 200 ms.
- [ ] #17: `po pin X <name>` is refused when `<name>` equals an existing node id, so the pin can't be silently shadowed.
- [ ] #17: unnamed pins sync through `push`/`pull` like named ones (D7).
- [ ] #12: `po diff A B` on siblings prints a direct A→B diff (e.g. `x 10→20`, not `1→10` / `1→20`). The parent-relative view is still available through `po siblings` (no `diff` flag, D5). A direct diff against an imported root with no stored manifest works rather than erroring.
- [ ] Hydra: a stale `.hydra/config.yaml` left by an earlier run (mtime before this run's start) is not captured. The run warns "no Hydra config written by this run".

### N4. S3 in CI (Lead dev; Tester)

Acceptance
- [ ] A new CI job (MinIO service container) runs the M7 suite and the N1 remote tests (concurrent push, pins, `pruned_at`, alpha.1 conversion) against `s3://` on every PR.
- [ ] A second `push` with nothing new transfers 0 bytes on S3 too (M7 unchanged). The one-time full re-push of node lines after the upgrade is expected and excluded.
- [ ] S3 bugs this job finds are fixed in this release (TECH expects some in prefix listing and ranged gets).

### N5. Terminal polish and completion (#18, #14) (Lead dev; Tester)

Acceptance
- [ ] With stdout not a TTY, or with `NO_COLOR=1`, output contains no ANSI escapes (grep for `\x1b[`). `--color=always` forces them, `--color=never` removes them.
- [ ] Status colours: done green, failed red, killed yellow, running cyan, pruned dim. `@` is bold.
- [ ] Resumed label colour: the status word keeps its colour, `· resumed @…` is styled separately; with `NO_COLOR` the text is identical to phase 2 output.
- [ ] Resumed label width: when the step list doesn't fit (e.g. `COLUMNS=60` with 4 resumes), it shortens to `resumed ×4`, and the line stays within the width.
- [ ] At `COLUMNS=80`, no `tree` or `siblings` line is longer than 80 display columns. Long notes and paths end in `…`.
- [ ] `siblings` switches to the transposed layout when the columns don't fit the width, not at a fixed 6 columns.
- [ ] `tree --metric k` values are right-aligned in one column. The `@N` fork step shows on edges.
- [ ] `metric_goal = "max"` (global) and a per-key table in `config.toml` override the name-based guess (L-8). Test: key `loss` with goal `max` puts ★ on the highest value.
- [ ] Every mutating command's last line is still the bare node id with colour on (`--color=always | tail -1` equals the id with no escapes).
- [ ] #14: `COMPLETE=fish po fork ` (and bash, zsh) lists live node ids, pin names, `@` and `@-` for every node-taking argument. In a 1,000-node repo it answers in under 200 ms. README has install lines for bash, zsh and fish.
- [ ] Completion outside a pollard repo prints nothing and exits 0. Completion on an alpha.1 (unmigrated) repo prints nothing, and never migrates or writes.

### N6. Data hashing (Lead dev). Cuttable: the second cut, after platforms.

Acceptance
- [ ] Remote data root: changing one object's contents (new etag) under the prefix changes the node's `data` hash. Listing again with nothing changed gives the same hash. Runs against MinIO.
- [ ] Upgrade note written: the first run after upgrade shows a `data` delta for repos with remote roots (a one-time hash change).
- [ ] The plain-string form `data = ["./data"]` gives the same hash as alpha.1 for an unchanged local root.

(§12 tag/stat modes are deferred to the next release, D15; their acceptance moves with them.)

### N7. Platforms, docs, release (Lead dev: platforms; PM: docs; Tester: journeys and timing). Platforms are the first cut.

Acceptance
- [ ] The release workflow builds `pollard`/`po` tarballs and wheels for x86_64 Linux gnu, x86_64 musllinux, and aarch64 macOS. CI runs `cargo test` on `macos-latest`.
- [ ] `uv tool install pollard-vcs==0.2.0a1` works on a clean macOS arm64 machine and in an Alpine container.
- [ ] Journeys A–D and the new upgrade journey (below) pass end to end. The 1,000-node release timing test passes, with results in TEST_REPORT.
- [ ] SPEC bumped to v4: §3 (pruned flag, pins shape), §4 (`prune -r`, `unprune`, `pin` optional name, `tree --pinned`, `diff`, `run` parent rule, `--color`), §5 (remote format 2), §9 (M4 test), §10 (interactive `prune` preview exempt from 200 ms), §4 `prune --failed`/`--killed` and the resumed label, §12 (still planned, next release), and a revision log. DECISIONS gains entries for every owner decision in §6.
- [ ] Stale-doc fix (PM): SPEC §8 and DECISIONS D-11 still say MSRV 1.85; `Cargo.toml` and the CI msrv job use **1.89**. Correct both in the SPEC v4 pass.
- [ ] README: status table, the tree example without `pruned` as a status, completion setup, the platform line, and a link to the upgrade notes.

**Upgrade journey (new, Tester; written in phase 1, rerun here):** alpha.1 user with a repo and a local remote → install 0.2.0a1 → `po tree` (migrates, prints notice) → `po push` → a second clone on 0.2.0a1 does `po pull` → both trees are identical and show the previously hidden node.

---

## 4. Compatibility and upgrade

| Area | alpha.1 → 0.2.0 | User action |
|---|---|---|
| Repo (`.pollard/`) | One-way migration to format 2 on first open. The database moves to `.pollard/state.sqlite`, and `db.sqlite` becomes a stub directory so alpha.1 binaries fail instead of misreading it. The backup is at `.pollard/backup/db-format1.sqlite` | None. Keep the backup until satisfied. Rollback: `rm -r .pollard/db.sqlite .pollard/state.sqlite* && mv .pollard/backup/db-format1.sqlite .pollard/db.sqlite`, then reinstall alpha.1 |
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

**CHANGELOG requirement:** the `0.2.0-alpha.1` entry opens with an **Upgrade notes** section (the table above, in user terms, with the rollback steps), before the feature list. PM writes it. Tester confirms every row against the upgrade journey. The GitHub release notes copy it verbatim.

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

## 6. Owner decisions (D1–D18 DECIDED 2026-09-19; D19 open)

Merged with TECH §5 Q1–Q9 (TECH id in brackets). The owner accepted every recommended default (#19 comment, 2026-09-18) plus the three decisions in D15, D17 and D18. PM records each one in DECISIONS during the SPEC v4 pass (N7).

| # | Question | Decision |
|---|---|---|
| D1 | Version: `0.2.0-alpha.1` (breaking-format signal) or `0.1.0-alpha.2`? | **DECIDED:** `0.2.0-alpha.1` / PyPI `0.2.0a1` (§1). |
| D2 | Make alpha.1 binaries fail loudly by moving the database to `state.sqlite` with a `db.sqlite/` stub, and appending a poison line to the remote's legacy `nodes.jsonl`? Both are deliberate one-way breaks. [Q1] | **DECIDED:** yes. Otherwise alpha.1 clients silently un-prune nodes and overwrite pins. |
| D3 | Migration: automatic on first open (with a backup), and no `undo` across it (op-log barrier)? [Q2] | **DECIDED:** yes to both. The upgrade notes say "undo before upgrading". |
| D4 | Fallback status for a pruned node whose pre-prune outcome isn't in the op log [Q3] | **DECIDED:** `done` if `finished_at` is set, else `killed`. The migration notice lists those ids. |
| D5 | `diff A B` on siblings: direct only, or keep a flag for the parent-relative view? | **DECIDED:** direct only. `siblings` is the parent-relative view. |
| D6 | #13: does a `killed` `@` also make the next run a sibling? | **DECIDED:** yes, failed and killed alike. Resuming via `fork --step` makes a child; explicit `--parent` also wins. |
| D7 | Do unnamed pins sync on `push`? | **DECIDED:** yes, same as named pins. No `--local` until someone asks. |
| D8 | #15 `run --name`: defer, and what happens on a cross-clone name collision if it returns? [Q6] | **DECIDED:** defer; pins cover naming for now. Settle the collision rule (refuse the whole pull, or scope it as `name@clone`) before bringing it back. |
| D9 | macOS arm64 and musllinux in this release? | **DECIDED:** yes, as best-effort platforms (CI-tested only), and the **first** cut rather than delay the release. |
| D10 | Exempt the interactive `prune` preview (exact bytes freed) from the §10 200 ms budget, keeping `prune -y` fast? [Q4] | **DECIDED:** yes. Add it to the §10 exempt list in SPEC v4. |
| D11 | `gc --auto`: remove the flag, or deprecate it? [Q9] | **DECIDED:** hidden no-op alias with a deprecation warning; remove it in the next minor. |
| D12 | sdk/hydra duplicate check stays warn-only (L-5)? [Q5] | **DECIDED:** yes for this release. |
| D13 | Remote segments: defer compaction? [Q8] | **DECIDED:** yes, until pulls measurably slow down. |
| D14 | #16 (deferred): may the CLI spawn Python for opt-in forwarding, as an exception to SPEC §8's "no runtime Python"? [Q7] | **DECIDED:** yes, for `forward = [...]` only, so #16 can be planned next. |
| D15 | §12 tag/stat modes: in this release (cuttable), or push to the next? | **DECIDED (owner): deferred** to the release after this one, to keep the budget. |
| D16 | The alpha.1 draft GitHub release has a stale `linux_x86_64` wheel | **DECIDED:** delete that asset, attach the manylinux wheel PyPI serves, and publish the draft so the history is complete. |
| D17 | "Resumed": a stored status or a derived label? | **DECIDED (owner): derived label.** Failed/killed node with ≥1 unpruned `--step` child → `killed · resumed @30000` (`resumed ×K` when narrow). Never on done nodes; text output only, never `--json` or stored status. Phase 2 (N3), colour in phase 3 (N5). |
| D18 | `po prune --failed` / `--killed [<node>]` in this release? | **DECIDED (owner): in.** Dead ends only, pins and `@` skipped with a notice, one pass per call, sweep members included and grouped, #19 preview/confirm/`-y`/`--dry-run`, one op. End of N2. `gc --older-than` stays deferred. |
| D19 | **OPEN (owner).** Pins under concurrent pushes: lossless (one record per pin add/remove in segments) or D-29 last-writer-wins on the whole pin map? (BACKLOG C4) | **Recommended: lossless**, done in phase 1 with the other remote-format changes (F9/F10), so the remote format doesn't change again later. Work proceeds on this default. |

---

## 7. Phase 1 execution plan

**Branch:** `next/phase-1`, created from `main` by the coordinator. All phase-1 work lands on it and reaches `main` through **one PR**. Phase 2 branches from `main` after that PR merges.

**Stories, in order** (ids and acceptance from BACKLOG phase 1, F0–F11; Lead dev implements, Tester writes each story's tests from its acceptance boxes, driving the binary):

| Story | What | Implements | Tests | Depends on | Size |
|---|---|---|---|---|---|
| F0 | alpha.1 fixture repo + remote + `golden/` outputs, built once by `crates/pollard-cli/tests/fixtures/alpha1/make.sh` with the **published `0.1.0-alpha.1` binary**, checked in | Tester | Tester | none (day 1) | S–M |
| F1 | Format version: `PRAGMA user_version`, migration framework (`MIGRATIONS` in one `BEGIN IMMEDIATE` transaction), `init` creates format 2 directly, newer format refused, `Repo::open_read_only` | Lead dev | Tester | none | M |
| F2 | Migration step 1: `pruned_at`, status recovery from op snapshots, D4 fallback, notice | Lead dev | Tester | F0, F1 | M |
| F3 | Standalone backup (`VACUUM INTO`, never overwritten), crash safety via tmp file + resume, rollback steps | Lead dev | Tester (rollback CI-only) | F2 | S |
| F4 | alpha.1 loud failure on the repo: `state.sqlite` + `db.sqlite/` stub with `UPGRADED.txt` | Lead dev | Tester (CI-only) | F3 | S |
| F5 | Op-log barrier: `undo` that would cross the upgrade is refused whole | Lead dev | Tester | F2 | S |
| F6 | `tree` placeholder guard: pruned node with live descendants shows as a placeholder; migrated #19 repro visible | Lead dev | Tester | F2 | S |
| F7 | Flag everywhere: 9 `Status::Pruned` sites, `Status::parse` errors on unknown values, duplicate check `AND pruned_at IS NULL`, phase-1 `prune` = alpha.1 subtree shape writing `pruned_at` | Lead dev | Tester | F2 (code lands with F2: removing the status and adding the flag can't be split; its tests follow in order) | S |
| F8 | Pins storage `(node_id, name NULL)` in the same migration step; named-pin behaviour unchanged | Lead dev | Tester | F2 | S |
| F9 | Remote format 2: `FORMAT`, per-push segments, `remote_segments`, legacy read + normalization, poison line, pins records (per-pin if D19 default holds) | Lead dev | Tester | F2, F7, F8 | M |
| F10 | Concurrent pushes lose nothing (2 clones × 20 rounds, with checkpoints; pins per D19) | Lead dev (fixes) | Tester | F9 | S |
| F11 | alpha.1 loud failure on a converted remote; CI job that installs the published alpha.1 binary for F3/F4/F11 | Lead dev (workflow) | Tester | F9 | S |
| — | Upgrade journey from the F0 fixture (N1 box); existing suites green on format 2; CHANGELOG entry | Lead dev (fixes, CHANGELOG) | Tester | F0–F11 | S |

The Tester starts F0 on day 1 and writes each story's tests while the Lead dev works on the one before (tests may be red until their story lands; none may be `#[ignore]`d at handoff). Exact message wording is BACKLOG's "Messages (phase 1)".

**Check-ins** (PM runs them; short, in the phase-1 PR thread):
1. **Day 1–2:** F0 fixture checked in, F1 framework opens it. PM and PO confirm the fixture covers every F-story case.
2. **Local format done (F1–F8):** their tests green locally. PO checks the notice and refusal wording against BACKLOG "Messages".
3. **Remote done (F9–F11):** remote and concurrency tests, the upgrade journey and the CI alpha.1 loud-failure job green. D19 answer needed before F9 starts; if none, build the default.
4. **Handoff:** Lead dev marks the PR ready for review; Tester runs the full gate below and reports; PM ticks the N1 boxes; PO accepts the phase-1 stories in BACKLOG. Merge to `main` only when all of that is done.

**Phase 1 is done when:**
- every N1 acceptance box in §3 and every F0–F11 box in BACKLOG is ticked, each backed by a named test;
- the full test suite is green (`cargo test --workspace`, including the new phase-1 tests and the CI alpha.1 loud-failure job);
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` are clean;
- the MSRV build passes on Rust **1.89** (CI `msrv` job, `--locked`);
- CHANGELOG has the format-2 entry, and nothing outside phase 1 (no #19 command changes, no #17 commands) is in the PR.
