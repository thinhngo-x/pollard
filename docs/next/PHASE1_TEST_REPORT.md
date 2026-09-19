# Phase 1 (Format 2) test report

Tester, branch `next/phase-1-tests`. Last run: 2026-09-19, on the merge of `next/phase-1` at `5d80a26` (commits 7184ec1 F1–F8, 8c13fec F9–F11, 5d80a26 CHANGELOG).

**Result: every F0–F11 acceptance test passes. `cargo test --workspace` is green: all existing suites plus 53 new phase-1 tests, run with the published alpha.1 binary present. No open failures.**

## How to run

```sh
cargo test -p pollard-cli --test n1_format --test n1_migration --test n1_remote --test n1_alpha1 --test n1_journey
# the CI-only tests (F3 rollback/backup-in-alpha.1, F4, F11) also need the published binary:
cargo install pollard-cli --version 0.1.0-alpha.1 --locked --root /some/dir
POLLARD_ALPHA1_BIN=/some/dir/bin/pollard cargo test ...
```

Without `POLLARD_ALPHA1_BIN`, the alpha.1 tests print `SKIP <test>: …` and pass. The phase-1 tests need the `sqlite3` CLI (standard on ubuntu runners) and `tar`.

**CI:** the Lead dev's `ci.yml` change covers what's needed: `cargo install pollard-cli --version 0.1.0-alpha.1 --locked --root "$RUNNER_TEMP/alpha1"` and `POLLARD_ALPHA1_BIN` on the `cargo test` step. `sqlite3` must be on the runner.

## Per story

| Story | Tests | Status |
|---|---|---|
| F0 fixture | `tests/fixtures/alpha1/{make.sh,repo.tar.gz,remote.tar.gz,golden/}` (≈70 KB) | done |
| F1 format version | `n1_format.rs`: 3 | pass |
| F2 migration | `n1_migration.rs` `f2_*`: 12 | pass |
| F3 backup, crash, rollback | `n1_migration.rs` `f3_*`: 6 (2 need alpha.1) | pass |
| F4 alpha.1 fails on repo | `n1_alpha1.rs` `f4_*`: 1 (needs alpha.1) | pass |
| F5 undo barrier | `n1_migration.rs` `f5_*`: 3 | pass |
| F6 visible under pruned | `n1_migration.rs` `f6_*`: 4 | pass |
| F7 flag everywhere | `n1_migration.rs` `f7_*`: 4; `n1_remote.rs` `f7_bogus_status_in_remote_line_is_an_error` | pass |
| F8 pin storage | `n1_migration.rs` `f8_*`: 3 | pass |
| F9 remote format 2 | `n1_remote.rs` `f9_*`: 10 | pass |
| F10 concurrent pushes | `n1_remote.rs` `f10_*`: 4 | pass |
| F11 alpha.1 fails on remote | `n1_alpha1.rs` `f11_*`: 1 (needs alpha.1) | pass |
| Upgrade journey | `n1_journey.rs`: 1 | pass |

A risk probe outside BACKLOG, `probe_guessed_statuses_do_not_override_recovered_ones` (`#[ignore]`, run with `--ignored`), also passes. It checks that a fresh 0.2 clone of the alpha.1 remote, which holds only *guessed* statuses, does not overwrite the migrated clone's recovered statuses when it pushes.

## Open failures

None.

## Test notes and decisions

- **D19** (open): `n1_remote.rs` tests the lossless default. To switch to whole-map last-writer-wins, set `const D19_LOSSLESS_PINS` to `false`; `f10_concurrent_pin_edits` then asserts "exactly A's or B's map". `f10_same_pin_name_last_writer_wins` holds under either answer.
- **F3 crash hook:** the step names are `tmp_written`, `tmp_migrated`, `renamed` and `old_moved`, as implemented. The test is `#[cfg(debug_assertions)]`.
- **F3 backup:** the implementation moves the original `db.sqlite` (with its WAL folded in) into `backup/`, instead of taking a `VACUUM INTO` copy. The tests check the property BACKLOG asks for: copied alone, the file opens with `user_version = 0` and has the fixture's node/pin/op counts and statuses, and alpha.1's `tree --all` on it equals golden.
- **F7 duplicate check:** this needs the fixture's env hash to be machine-independent. `make.sh` checks in a `uv.lock` and sets `POLLARD_CUDA=none`, and the test sets the same variable.
- **Wording fixed with the Lead dev:** the migration notice's `N` counts guessed nodes (7 for the fixture), and its optional lines are indented two spaces.

## Existing tests changed by the Lead dev

- `m1_core.rs::init_creates_repo_layout_and_gitignore_entry`: now asserts `state.sqlite` is a file and `db.sqlite` is a directory. This does not weaken coverage.
- `review_regressions.rs::failed_blob_push_can_be_retried`: now looks for the node id in `nodes/*` segments instead of `nodes.jsonl`. This does not weaken coverage.

## BACKLOG criteria that are ambiguous or can't be tested as written

1. **F2 notice:** the count `N` and the indentation of the `[ … ]` lines were not specified. Now settled; the PO should write both into BACKLOG "Messages".
2. **F3:** the text says "old file moved to backup", but the Tech note (C5) says `VACUUM INTO`. The implementation moves the file. PO/PM should pick one wording.
3. **F9 notice and refusal `<url>`:** BACKLOG doesn't say whether this is the configured string or the resolved path. The implementation prints the resolved path (`…/repo/../remote`). The tests accept any `<url>`.
4. **F2 "notice before its own output":** stderr and stdout are separate streams. The test merges them (`2>&1`) and checks the order.
5. **F8 "op snapshots round-trip unnamed pins"** is a unit test in `ops.rs` (Lead dev). No binary-level test is possible until B9 adds the CLI.
6. **F10 "same suite against MinIO"** is phase 2 (B12). Not covered here.
7. **F9 "Legacy pins.json is read only when no segment carries pins":** this only holds if the converting push writes a record for every existing pin. `f9_legacy_pins_json_only_until_segments_carry_pins` checks this indirectly: after one unpin, a fresh clone still sees the other pins and not the removed one.
8. **F2 fallback for a fresh clone of an unconverted remote:** under D4, every pruned node gets a guessed status, so `crashed` reads `done`. That is correct by the rule, but lossy; the probe above guards the case that matters.

## Environment issue (for the coordinator)

`/home/duthngo/pollard` and `/home/duthngo/pollard-tests` share `CARGO_TARGET_DIR=target/ci198`, and cargo's hashes for path packages are workspace-relative. As a result, both worktrees write the **same** artifact and fingerprint names in `target/ci198/debug`. A build in one worktree can link the other's crates: early on, my binary had the Lead dev's WIP `pollard-core` in it.

What I did:
- Deleted the `pollard-cli-*` and `pollard-core-*` fingerprints in `debug/` once, so the next build relinks cleanly.
- Moved all my builds to a custom profile in the same target dir: `cargo test --profile tester` with `CARGO_PROFILE_TESTER_INHERITS=dev`, `…_DEBUG=0` and `…_INCREMENTAL=false`. It uses `target/ci198/tester/`, about 0.8 GB.

The Lead dev should `touch` a source file (or `cargo clean -p pollard-cli -p pollard-core`) before trusting a debug build made around 09:15–09:25.

## Regenerating the fixture

```sh
cargo install pollard-cli --version 0.1.0-alpha.1 --locked --root /some/dir
POLLARD_ALPHA1_BIN=/some/dir/bin/pollard crates/pollard-cli/tests/fixtures/alpha1/make.sh
```

This needs `sh`, `setsid`, `tar`, `gzip` and `sqlite3`. Ids and timestamps change each time, and the tests read them from `golden/ids.json` and `golden/op_log.txt`.
