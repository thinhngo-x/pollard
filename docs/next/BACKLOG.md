# pollard 0.2.0-alpha.1: product backlog

Owner: PO. Companions: `docs/next/PLAN.md` (PM: scope, schedule), `docs/next/TECH.md` (Lead: approach).
Source of truth for intent: owner decisions on #11, #13, #17, #19 (issue comments, 2026-09-19). Where this file and PLAN/TECH differ, see "Open owner decision" at the end.

Priority: **P0** data loss or data hiding, blocks release. **P1** daily-workflow friction, ships unless the schedule breaks. **P2** polish, cuttable within its phase.
Every acceptance box is written to be checked by an integration test that drives the `pollard`/`po` binary (the `crates/pollard-cli/tests/` style), unless marked *(CI-only)* or *(manual)*. `$` lines are commands; indented lines are expected output. Output marked *exact* is asserted byte-for-byte (except ids, paths, counts and sizes shown as `<…>`); the rest is asserted by content.

---

## Definition of Done (every story)

- [ ] Every acceptance box has a test that fails before the change and passes after it. Tests drive the binary; unit tests are extra, not a substitute.
- [ ] The existing suites stay green (`m1`–`m9`, `cross_cutting`, `journeys`, `review_regressions`). A changed assertion is changed on purpose and named in the PR.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and the `msrv` job (Rust 1.89, `--locked`) pass. No new dependency beyond PLAN §5's list without a PR note.
- [ ] Mutating commands still print the bare node id as the last stdout line; notices and warnings go to stderr.
- [ ] Anything that changes a user-visible behaviour, flag or message has: a CHANGELOG line under `0.2.0-alpha.1` (and a row in **Upgrade notes** if an alpha.1 user or script must act), the README or `--help` text updated, and a SPEC v4 note for the PM.
- [ ] No `pruned` status string is written anywhere new (DB, op snapshot, remote line, `--json`).
- [ ] Anything touching the repo or remote format is tested against the alpha.1 fixture (F0).
- [ ] 1,000-node timing budget (SPEC §10: 200 ms) holds for any command the story touches (release-mode slow test).

---

## Phase 1. Format 2 (ready)

Everything that changes on disk or on the remote, landed once. Nothing in phase 2 starts until F0–F11 are green.

**Behaviour contract for phase 1 only.** `po prune` keeps its alpha.1 *shape* (whole subtree, no prompt) but writes the `pruned_at` flag instead of a status. `tree` hides pruned nodes as before, except the one change in F6. The prune redesign itself is phase 2 (B1–B5).

### F0. alpha.1 fixture repo and remote (enabler) · P0 · #19, D-29

As the tester, I want a real repo and remote written by the published `0.1.0-alpha.1` binary, so that every migration test runs against what users actually have.

- [ ] `crates/pollard-cli/tests/fixtures/alpha1/make.sh` builds the fixture with the published `0.1.0-alpha.1` binary and is checked in with its output (`repo.tar.gz`, `remote.tar.gz`, `golden/`). Re-running it is only needed if the fixture changes.
- [ ] The fixture repo contains, at minimum (ids are whatever alpha.1 generates; `golden/ids.json` maps these roles to them):
  - `root` done; `base` done with metrics and a checkpoint, child of `root`.
  - The #19 repro: `mid` done, `po prune mid` while `mid` is `@`; under it `best` done, pinned twice (`paper`, `best-v1`), and `crashed` failed and `stopped` killed (SIGINT), all pruned by that one subtree prune; then `late` done, run from the pruned `@` **after** the prune (hidden in alpha.1's plain `tree`).
  - `kept` done with a checkpoint, pruned with `--keep-weights`.
  - `ghost` done, created in a second clone, pruned there, pushed, then pulled into the fixture repo (so the fixture has no prune op for it: the fallback case).
  - `redo` done: pruned, `po undo`, pruned again (recovery must use the latest prune).
  - a pin `root-pin` on `root`.
  - at least 10 ops in `op log`, including all of the above.
- [ ] The fixture remote is the one both clones pushed to (legacy `nodes.jsonl` + `pins.json`, no `FORMAT`).
- [ ] `golden/` holds alpha.1's output for: `po tree --all`, `po show <each role>`, `po siblings base --json`, `po siblings root --json`, `po op log`, and `SELECT * FROM pins`.

### F1. Format version and refusing newer formats · P0

As a researcher, I want pollard to know which format my repo is in and refuse one it doesn't understand, so that an old binary never misreads new data.

- [ ] `po init` in an empty dir creates a format-2 repo directly: `.pollard/state.sqlite` with `PRAGMA user_version = 2`, a `db.sqlite/` stub directory, no backup, **no migration notice**.
- [ ] Given a repo whose `user_version` is 3, when any command runs, it exits non-zero, writes nothing (file mtimes and sizes under `.pollard/` unchanged), and stderr is *exact*:
      `error: this repo uses format 3, written by a newer pollard; this pollard (0.2.0-alpha.1) reads up to format 2. Upgrade pollard.`
- [ ] `po --version` and `po --help` never open or migrate a repo.

### F2. Automatic migration of an alpha.1 repo · P0 · #19

As a researcher with an alpha.1 repo, I want the first command after upgrading to convert my repo in place, with my pruned runs keeping their real outcome, so that I upgrade without doing anything and lose nothing.

- [ ] Given the F0 repo, when `po tree` runs, then it migrates once, prints the migration notice (see **Messages**) on stderr before its own output, exits 0, and `user_version` is 2. Any command triggers this, including read-only ones (`show`, `siblings`, `log`), except completion (T5).
- [ ] After migration, no node has `status = 'pruned'`. `mid`, `best`, `redo` read `done`, `crashed` reads `failed`, `stopped` reads `killed`, and each has `pruned_at` = the timestamp of the prune op that pruned it (for `redo`, the second prune).
- [ ] `ghost` (no prune op in this repo) gets the fallback: `done` if `finished_at` is set, else `killed`; `pruned_at = finished_at`, else `created_at`; and the notice lists it on the `guessed` line.
- [ ] `late` (never pruned) has `pruned_at` NULL and status `done`.
- [ ] `po show best` prints `status  done` and `pruned  <timestamp>` (two separate facts).
- [ ] Every node id, parent, recipe hash, note, `fork_step`, metric and weights hash is unchanged (compare `po show <each>` minus the status/pruned lines against `golden/`).
- [ ] `po siblings base --json` and `po siblings root --json` are byte-identical to `golden/`.
- [ ] Pins: `paper`, `best-v1`, `root-pin` resolve to the same nodes as before (`po show paper` shows `best`).
- [ ] Running any second command prints no notice and changes nothing (idempotent).
- [ ] Two `po tree` processes started at the same moment on an unmigrated copy: both exit 0, exactly one notice in total, one backup file.

### F3. Backup, crash safety and rollback · P0

As a researcher, I want a complete backup and a documented way back to alpha.1, so that a bad upgrade can't cost me my history.

- [ ] After F2, `.pollard/backup/db-format1.sqlite` exists and is a standalone database: copied alone (without any `-wal`/`-shm` file) into a fresh alpha.1 layout, alpha.1's `po tree --all` output equals `golden/` *(CI-only: needs the alpha.1 binary)*. Without the alpha.1 binary, the test opens it with SQLite and checks `user_version = 0` and the node/pin/op row counts equal the fixture's.
- [ ] After F2, `.pollard/` contains `state.sqlite`, a `db.sqlite/` directory holding `UPGRADED.txt` (text in **Messages**), and no `db.sqlite-wal`, `db.sqlite-shm` or `state.sqlite.tmp`.
- [ ] An existing backup is never overwritten: if `.pollard/backup/db-format1.sqlite` already exists, the new one is `db-format1-<unix seconds>.sqlite` and the notice names that path.
- [ ] Crash safety: with a test-only abort hook (debug builds only, e.g. `POLLARD_TEST_MIGRATION_ABORT=<step>`) at each step (tmp copy written; tmp migrated; tmp renamed to `state.sqlite`; old file moved to backup), the process exits non-zero, and the next plain `po tree` either finds an intact format-1 repo and migrates it, or finds a finished `state.sqlite` and completes the remaining steps. In every case the end state equals a clean F2 run (same `po tree --all`, one backup), and no op, node or pin is lost.
- [ ] Rollback *(CI-only)*: after migrating a copy of F0, running the CHANGELOG rollback steps verbatim (`rm -r .pollard/db.sqlite .pollard/state.sqlite*` then `mv .pollard/backup/db-format1.sqlite .pollard/db.sqlite`) and then the alpha.1 binary's `po tree --all` equals `golden/`. Re-running 0.2.0 afterwards migrates again cleanly.

### F4. alpha.1 binaries fail loudly on a migrated repo · P0

As a team member who forgot to upgrade one machine, I want the old binary to stop with an error, so that it can't silently un-prune nodes or drop pins.

- [ ] *(CI-only, alpha.1 binary installed)* On a migrated copy of F0, each of `po tree`, `po show <id>`, `po run -- true`, `po pin <id> x`, `po prune <id>`, `po undo`, `po push` run with the alpha.1 binary exits non-zero, and afterwards the `.pollard/` tree is byte-identical to before (hash every file).
- [ ] The failure output mentions the database (alpha.1's own message, e.g. `unable to open database file`); `UPGRADED.txt` explains why for anyone who looks.

### F5. Undo stops at the upgrade · P0

As a researcher, I want `po undo` to refuse to reach back before the upgrade instead of half-restoring old snapshots, so that undo never corrupts a migrated repo.

- [ ] Right after migration, `po undo` exits non-zero, changes nothing, and stderr is *exact*:
      `error: cannot undo past the format-2 upgrade (0 ops since it); nothing undone`
- [ ] After two post-migration ops, `po undo 3` refuses the same way (`(2 ops since it)`) and undoes nothing (no partial undo); `po undo 2` succeeds and restores the state just after migration.
- [ ] `po op log` still lists the pre-migration ops (command and time) and does not error.

### F6. Hidden work under a pruned node becomes visible · P0 · #19, #11

As a researcher, I want runs I made under a pruned node to show in `po tree`, so that done work (and `@`) is never hidden.

- [ ] On the migrated F0 repo, plain `po tree` shows `mid` as a placeholder line with `late` under it, and hides `best`, `crashed`, `stopped` (fully pruned). Plain-text shape (the dimming comes in T4):
  ```
  <root>  done  …
  ├─ <base>  done  …
  └─ <mid>  done (pruned)  …
     └─ <late> (@)  done  …
  ```
- [ ] `po tree --all` shows every node; pruned ones carry `(pruned)` after their status.
- [ ] A fully pruned subtree (every node pruned) is hidden in plain `po tree`, as in alpha.1.
- [ ] `po siblings <mid>` hides pruned children unless `--all`, as in alpha.1.

### F7. Pruned is a flag in every code path · P0 · #19

As a researcher, I want everything that used to check "status = pruned" to check the flag, so that nothing changes behaviour by accident when the status goes away.

- [ ] Duplicate check (D-19 unchanged): after migration, `po run` with the recipe of `best` (pruned, `done`) prints a notice naming `best` and creates the node; with the recipe of `base` (done, not pruned) it refuses without `--force`.
- [ ] `po prune X` (phase-1 shape: whole subtree) sets `pruned_at` on X and its descendants and leaves their status unchanged (`po show` reads `done`/`failed`/`killed` + `pruned <ts>`); `po undo` clears it.
- [ ] A status string that isn't `running|done|failed|killed` in the DB or in a remote line is an error naming the node and the value, never read as pruned. (Test: hand-edit one row to `status='bogus'`; `po show` on it errors with the id and `bogus`.)
- [ ] `--json` outputs contain no `pruned` status value.

### F8. Pin storage with optional names · P1 (must land in phase 1: format change) · #17

As a researcher, I want the repo to be able to store a pin without a name, so that phase 2 can add unnamed highlights without a second migration.

- [ ] After migration (and in a fresh `init`), `pins` is `(node_id NOT NULL, name NULL UNIQUE)` with at most one unnamed pin per node. All alpha.1 named pins carry over, including two names on one node (`paper`, `best-v1`).
- [ ] Named-pin behaviour is unchanged from alpha.1: `po pin <id> <name>`, `po unpin <name>`, name as a node ref, `siblings` headers, `undo` of pin/unpin. (The existing `m8` pin tests pass untouched.)
- [ ] Op snapshots taken after migration round-trip unnamed pins (unit test in `ops.rs`: snapshot with `(None, id)` restores it). CLI for unnamed pins is B9.

### F9. Remote format 2: segments, reading and converting alpha.1 remotes · P0 · D-29

As a team sharing a remote, I want the new client to read our alpha.1 remote and convert it on the first push, so that we keep one shared history across the upgrade.

- [ ] Given the F0 remote (no `FORMAT`), a migrated clone's `po pull` works, converts nothing on the remote (remote files byte-identical after), and its `po tree --all` shows every node with the F2 statuses. Legacy lines with `"status":"pruned"` get the F2 fallback rule.
- [ ] The first `po push` from a 0.2 clone writes `FORMAT` (content `2`), writes its node lines to one new `nodes/<utc-ts>-<clone_salt>-<n>.jsonl` segment, appends the poison line (text in **Messages**) to legacy `nodes.jsonl`, and prints the conversion notice on stderr. It never rewrites an existing segment.
- [ ] That first push re-sends every node line once (expected; upgrade note). A second `po push` with nothing new writes no file and reports 0 bytes (M7 unchanged).
- [ ] A fresh 0.2 clone that pulls the converted remote gets all nodes, statuses, `pruned_at` and pins; the poison line is skipped silently.
- [ ] `pruned_at` syncs: prune in clone X, push; pull in clone Y; `po show` in Y reads the outcome status + `pruned <ts>`, and Y's `tree` matches X's.
- [ ] Named pins round-trip push → pull between two 0.2 clones. Legacy `pins.json` is read only when no segment carries pins.
- [ ] Pull downloads only segments it hasn't applied (second `po pull` with nothing new: 0 bytes).
- [ ] A remote whose `FORMAT` is `3`: `push` and `pull` exit non-zero, write nothing locally or remotely, stderr *exact*:
      `error: remote <url> uses format 3, written by a newer pollard; this pollard (0.2.0-alpha.1) reads up to format 2. Upgrade pollard.`
- [ ] Two 0.2 clones doing their first push to the same alpha.1 remote at the same time: both succeed, `FORMAT` is `2`, one poison line or two (both harmless), and a fresh clone pulls every node from both.

### F10. Concurrent pushes lose nothing · P0 · D-29

As a team, I want two people pushing at the same moment to both land, so that no run silently disappears from the shared remote.

- [ ] Two clones A and B of one repo, one local-path remote. 20 rounds: in each, A and B each create one node and then push at the same time (both binaries spawned before either is waited on). Then A, B and a fresh clone C each `pull`: all three have every node from both (base + 40), with identical `po tree --all`.
- [ ] Same test with each round's pushes carrying a checkpoint: every node's weights are fetchable (`po fork <id> --step N` in C restores a byte-identical file for a sample of 5 nodes).
- [ ] The same suite runs against MinIO in phase 2 (B12).
- [ ] Pins under concurrency *(per D19 default, pending owner)*: each pin add or remove is its own segment record (`{"pin": [node_id, name|null], "removed": bool}`), replayed in segment order. Test: A pins `x` (named `ax`) and unpins `old-a`; B pins `y` (named `by`) and unpins `old-b`; A and B push at the same time; after A, B and a fresh C `pull`, all three have `ax → x` and `by → y`, and neither `old-a` nor `old-b`. Only two changes to the *same* pin are last-writer-wins by segment order (test: A and B both set name `best`, on different nodes; after pull all clones agree on one of them, no error).

### F11. alpha.1 binaries fail loudly on a converted remote · P0

As a team member on an old binary, I want push and pull to fail on a converted remote, so that I can't overwrite what upgraded teammates pushed.

- [ ] *(CI-only)* Against a remote converted by F9, the alpha.1 binary's `po push` and `po pull` exit non-zero, and afterwards both the remote and the alpha.1 clone's `.pollard/` are byte-identical to before.

### Messages (phase 1, exact wording)

All on stderr. `<…>` is filled in; lines in `[ ]` are printed only when they apply.

**Repo migration notice** (once, on the command that migrates):
```
pollard: upgraded this repo to format 2 (pollard 0.2.0-alpha.1)
  backup of the old database: .pollard/backup/db-format1.sqlite
[ pruned is now a flag: <N> pruned nodes keep their done/failed/killed status ]
[ guessed status (no op-log record): <id> (done), <id> (killed) ]
  po undo cannot go back past this upgrade, and pollard 0.1.0-alpha.1 can no longer open this repo
  to roll back, see the upgrade notes: https://github.com/thinhngo-x/pollard/blob/main/CHANGELOG.md#upgrade-notes
```

**Remote conversion notice** (on the push that converts):
```
pollard: converted remote <url> to format 2
  pollard 0.1.0-alpha.1 can no longer push to or pull from it: everyone sharing it must upgrade
  this push re-sends all <N> node records once (no weights)
```

**`.pollard/db.sqlite/UPGRADED.txt`:**
```
This repo was upgraded to format 2 by pollard 0.2.0-alpha.1. The database is now .pollard/state.sqlite.
Install pollard 0.2.0-alpha.1 or newer to use it.
The old database is in .pollard/backup/. To roll back, see the upgrade notes in pollard's CHANGELOG.md.
```

**Remote poison line** (appended to legacy `nodes.jsonl`):
```
{"pollard_format":2,"upgrade":"this remote was converted by pollard 0.2.0-alpha.1; upgrade pollard to push or pull"}
```

**Refusals:** the exact lines in F1, F5 and F9.

---

## Phase 2. Behaviour

### B1. Prune one node by default, `-r` for the subtree · P0 · #19

As a researcher, I want `po prune X` to hide only X, so that I don't hide finished runs I built on it.

- [ ] Given `root → mid → {a done, b done}`, `po prune mid -y` sets `pruned_at` on `mid` only; `a` and `b` keep their status and show under the `mid (pruned)` placeholder in plain `po tree`.
- [ ] `po prune -r mid -y` sets `pruned_at` on `mid`, `a` and `b`; plain `po tree` hides all three.
- [ ] #19 regression: prune `@`'s parent with `--force -y`, then `po run`: the new done node shows in plain `po tree`.
- [ ] `--keep-weights` still hides without freeing: next `po gc` frees 0 bytes for that node. `po undo` of that prune also clears the keep-weights mark.
- [ ] `po gc --auto` prints `warning: --auto is deprecated and does nothing; running gc` and behaves like `po gc`.
- [ ] SPEC §9 M4 test updated; existing `m4`, `cross_cutting`, `journeys` prune steps pass `-y` (and `-r` where the subtree was meant).

### B2. Pins and `@` are protected · P0 · #19, #17

As a researcher, I want prune to refuse to hide my pinned runs or where I am, so that I can't lose track of them by accident.

- [ ] `po prune <@>` and `po prune <pinned>` exit non-zero, change nothing, and list every offender with the reason, e.g. `error: refusing to prune: late (@), best (pinned: paper); use --force`.
- [ ] `po prune -r X` where the subtree contains `@` and a pinned node lists both, not just the first.
- [ ] `--force` overrides both; pins stay attached to the pruned node.

### B3. Preview, confirm, `-y`, `--dry-run` · P0 · #19

As a researcher, I want to see what a prune will hide and free before it happens, so that I can back out.

- [ ] On a TTY without `-y`: prints the affected node ids and the bytes the next `gc` would free, then `prune <N> nodes? [y/N]`. Answering `n` (or Enter) changes nothing and adds no op.
- [ ] `--dry-run` prints the same preview, never asks, adds no op, exit 0.
- [ ] Non-TTY stdin without `-y` or `--dry-run`: exits non-zero without asking or hanging: `error: prune needs confirmation; pass -y (or --dry-run) when stdin is not a terminal`.
- [ ] On the M4 50 MB checkpoint fixture, the preview's byte count equals what the following `po gc` reports freeing.
- [ ] 1,000-node slow test: `po prune -y` < 200 ms. The interactive preview is exempt (D10).

### B4. `po unprune` · P1 · #19

As a researcher, I want to un-hide a node directly, so that I don't have to undo everything I did since.

- [ ] `po unprune X` clears `pruned_at` on X only (`-r`: X and descendants), as one op, without touching later ops; `po undo` re-prunes. It also clears X's keep-weights mark.
- [ ] After `gc` freed X's weights, `po unprune X` succeeds and warns `warning: <X>: weights were freed by gc (<paths>); the recipe can reproduce them`.
- [ ] `po unprune` on a node that isn't pruned: notice, exit 0, no op.

### B5. `po prune --failed` / `--killed [<node>]` · P1 · #19

As a researcher after a sweep with crashes, I want to hide all dead ends in one command, so that the tree shows only runs worth reading.

- [ ] `--failed` and `--killed` combine. Without `<node>`, the whole repo; with `<node>`, that node's subtree (the node included).
- [ ] Selects only dead ends: status failed/killed, not pruned, no unpruned children. Never running, never already pruned. One pass: a failed parent whose only child is selected in this call is not selected.
- [ ] Pins and `@` are skipped with a notice (`skipped: <id> (@), <id> (pinned)`), not refused.
- [ ] Preview groups sweep members under their sweep name; `-y`, `--dry-run` and non-TTY rules as in B3.
- [ ] One op; one `po undo` restores every node it pruned.
- [ ] Nothing matched: `nothing to prune`, exit 0, no op. `--failed` with `-r` or `--force` is a usage error.

### B6. A run after a failed or killed `@` is its sibling · P1 · #13

As a researcher fixing a crashed run, I want my next run to sit next to the crash, not under it, so that dead ends don't become the trunk.

- [ ] `root → buggy (failed, @)`; edit config; `po run -- …` creates a child of `root`. stderr: `note: @ buggy failed; attaching to root (use --parent @ to build on it)`. Last stdout line is the new id.
- [ ] Same with a killed `@` (SIGINT during the run): sibling, notice says `killed`.
- [ ] `po fork buggy` (no `--step`) then `po run`: sibling (a plain fork doesn't make it a resume).
- [ ] `po fork buggy --step N` then `po run`: child of `buggy` with `fork_step = N`. Journey C (`fork blue-elk-5 --step 30000` on a killed node) stays a child.
- [ ] `po run --parent buggy`: child of `buggy`.
- [ ] A failed root (no parent): the next run is a new root, with the notice.
- [ ] Sweeps unchanged (`m8_sweeps` green): a failed member doesn't change where the next `--sweep` member attaches.

### B7. "Resumed" label (derived) · P1 · #19, #13

As a researcher, I want a killed run I resumed to say so, so that it doesn't read as abandoned.

- [ ] A failed/killed node with at least one unpruned child that has `fork_step` set shows `killed · resumed @30000` in `tree`, `show` and `siblings` text; several steps: `resumed @20000 @30000`.
- [ ] Done nodes never get it. Pruning the only resume child removes it.
- [ ] `--json` outputs and the stored `status` never contain it (`siblings --json` byte-identical with and without a resume child's label; `SELECT DISTINCT status` has no `resumed`).
- [ ] The narrow form `resumed ×K` is T3's job.

### B8. Only pruned nodes collapse in `tree` · P1 · #11

As a researcher, I want every node with children to show them, so that the tree never looks cut off.

- [ ] A failed node with children shows its children in plain `po tree`; no `… N collapsed` line exists anywhere (update `m8_sweeps`).
- [ ] Same for done, killed, running.
- [ ] Pruned behaviour as F6/B1: fully pruned subtrees hidden unless `--all`; a pruned node with live descendants is a placeholder.

### B9. Pin commands: optional name, `tree --pinned` · P1 · #17

As a researcher, I want to flag interesting runs without naming them and see only those, so that big trees stay navigable.

- [ ] `po pin X` succeeds with no name; `po pin X paper` adds a name that resolves as a node ref. Pinning an already-unnamed-pinned node without a name: notice, no op.
- [ ] `po unpin paper` removes that name; `po unpin X` removes all of X's pins. Each is one op, undoable.
- [ ] `po pin X <name>` where `<name>` equals an existing node id (or starts with `@`) is refused.
- [ ] Plain-text marker: pinned nodes carry `◆` in `tree` (colour highlight is T4). `★` stays for `--metric`.
- [ ] `po tree --pinned` prints only pinned nodes and their paths from the root. 1,000 nodes, 3 pins: exactly those nodes, < 200 ms.
- [ ] Unnamed pins round-trip through push/pull like named ones (D7).

### B10. `diff` of two siblings is direct · P1 · #12

As a researcher, I want `po diff A B` to show what differs between A and B, so that I don't subtract two parent-relative rows in my head.

- [ ] Siblings with `x=1→10` and `x=1→20`: `po diff A B` shows `x  10 → 20`, not the parent-relative table.
- [ ] `po diff` against an imported root with no stored manifest works (missing manifest = empty).
- [ ] `po siblings` still gives the parent-relative view; no new `diff` flag (D5).

### B11. Hydra never captures a stale config · P0

As a Hydra user, I want a run that crashed before Hydra wrote its config to record no config, so that its recipe isn't the previous run's.

- [ ] A `.hydra/config.yaml` with mtime before the run's start is not captured; stderr warns `warning: no Hydra config written by this run`.
- [ ] A config written during the run is captured as in alpha.1.

### B12. S3 remotes tested in CI · P1

As a team on S3, I want the remote tested against a real S3 API, so that the format-2 remote works where we actually use it.

- [ ] *(CI)* A MinIO job runs `m7_remote` and the F9/F10 suites with `remote = "s3://…"` on every PR; skipped locally when `POLLARD_TEST_S3_URL` is unset.
- [ ] Second push with nothing new: 0 bytes on S3 too. S3 bugs it finds are fixed in this release.

---

## Phase 3. Terminal

### T1. `metric_goal` · P2 · #18

As a researcher, I want to say whether a metric should go up or down, so that ★ and colours aren't guessed from its name.

- [ ] `metric_goal = "max"` in `config.toml`: `po tree --metric loss` puts ★ on the highest `loss`.
- [ ] Per-key table `[metric_goal] loss = "max"` wins over the global value. No key: today's name-based guess.
- [ ] An alpha.1 `config.toml` parses unchanged.

### T2. Colour by meaning, and off when piped · P2 · #18

As a researcher, I want status colours on my terminal and clean text in scripts, so that I read faster without breaking pipelines.

- [ ] Piped stdout, or `NO_COLOR=1`: no `\x1b[` in any output. `--color=always` forces it; `--color=never` removes it.
- [ ] Colours: done green, failed red, killed yellow, running cyan, pruned dim; `@` bold; ★ coloured.
- [ ] `--color=always`: the last stdout line of every mutating command is still the bare id (no escapes).

### T3. Width-aware output · P2 · #18

- [ ] `COLUMNS=80`: no `tree` or `siblings` line exceeds 80 display columns; long notes and paths end in `…`.
- [ ] `siblings` switches to the transposed layout when it doesn't fit the width (not at a fixed 6 columns).
- [ ] The resumed label (B7) shortens to `resumed ×K` when the steps don't fit.

### T4. Clearer `tree` · P2 · #18, #17, #11

- [ ] `--metric k` values are right-aligned in one column; the `@N` fork step shows on edges.
- [ ] On a colour terminal: pruned placeholders and failed rows dimmed; pinned nodes highlighted and still carry `◆`.

### T5. Tab completion for node ids and pins · P2 · #14

As a researcher, I want `po fork <TAB>` to offer my runs, so that I stop copying ids.

- [ ] `COMPLETE=bash po -- po fork <prefix>` (and zsh, fish) lists matching node ids, pin names, `@`, `@-`, for every node-taking argument (fork, diff, siblings, show, prune, unprune, pin, unpin, note, apply, log, export).
- [ ] 1,000 nodes: < 200 ms.
- [ ] Outside a repo: empty output, exit 0. On an unmigrated alpha.1 repo: empty output, exit 0, repo byte-identical after (never migrates or writes).
- [ ] README has the install line for bash, zsh and fish.

---

## Phase 4. Reach (cuttable, cut in this order: X2, then X1; X3 is not cuttable)

### X1. S3 data roots hashed from the listing · P1

As a researcher with data on S3, I want a changed dataset to change the recipe, so that the duplicate check and deltas are honest.

- [ ] *(MinIO)* Changing one object under the prefix changes the node's `data` hash; relisting unchanged gives the same hash.
- [ ] A local plain-string root (`data = ["./data"]`) hashes identically to alpha.1 (F0 fixture).
- [ ] Upgrade note: the first run after upgrading shows a one-time `data` delta for repos with S3 roots.

### X2. macOS arm64 and musllinux builds · P2

- [ ] *(CI)* Release workflow builds binaries and wheels for x86_64 Linux gnu, x86_64 musllinux, aarch64 macOS; `cargo test` runs on `macos-latest`.
- [ ] *(manual)* `uv tool install pollard-vcs==0.2.0a1` works on macOS arm64 and in an Alpine container.

### X3. Release gate: docs and the upgrade journey · P0 (not cuttable)

As an alpha.1 user, I want clear upgrade notes and a tested upgrade path, so that I know what changes and what to do.

- [ ] Upgrade journey passes: alpha.1 repo + local remote → install 0.2.0 → `po tree` (migrates, notice) → `po push` (converts, notice) → second clone on 0.2.0 `po pull` → both `po tree --all` identical, and `late` visible in plain `po tree` in both.
- [ ] Journeys A–D and the 1,000-node timing test pass; results in TEST_REPORT.
- [ ] CHANGELOG `0.2.0-alpha.1` opens with **Upgrade notes** (PLAN §4 table in user terms, the rollback steps verbatim from F3, "undo before upgrading", "everyone on a shared remote upgrades together", "add `-y` to scripted prunes"). The GitHub release notes copy it.
- [ ] SPEC v4, DECISIONS and README updated (PM), including the tree example without `pruned` as a status.

---

## Out of scope for 0.2.0-alpha.1

| Item | Why |
|---|---|
| SPEC §12 tag/stat data modes | Owner deferred to the next release to keep the budget. |
| #15 `run --name` | Owner deferred; pins cover naming, and cross-clone name collisions have no rule yet. |
| #16 W&B/MLflow forwarding from the CLI | Owner deferred; largest item, and the SDK forwarder covers W&B/Neptune. |
| `tree --expand <node>` (#11) | Not needed once only pruned nodes collapse. |
| `gc --older-than` | Not asked for by the owner in this release. |
| `gc` freeing Tier-1 objects | Disk only, no data at risk; needs its own careful design. |
| Undo across the migration; rolling a converted remote back to alpha.1 | Owner decision: one-way upgrade with a local backup; everyone on a remote upgrades together. |
| Remote segment compaction | One small segment per push is fine at alpha scale. |
| Storing "resumed" or showing it in `--json` | Owner: derived label only. |
| Interactive TUI, web UI, Windows | Owner chose terminal polish; SPEC §11 non-goals. |
| sdk/hydra blocking duplicate check | By design (L-5); a run-protocol change. |

---

## Open owner decision

- **D19 (was C4).** "Two clones pushing at once lose nothing": does it cover pins? Default (PLAN D19, TECH 1b), built unless the owner says otherwise before F9 starts: yes, one remote record per pin change, so concurrent pin edits from both clones survive (F10). Alternative: D-29 last-writer-wins on the whole pin map, in which case F10's pin box becomes "the result is exactly A's or B's map".
