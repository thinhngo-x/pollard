# pollard test report

Owner: Tester. Suite: `crates/pollard-cli/tests/` (drives the built `pollard` binary against temp repos).
Run: `CARGO_TARGET_DIR=target/tester cargo test -p pollard-cli` (add `-- --include-ignored` to also run gated tests).
Spec baseline: SPEC.md **v3** + docs/DECISIONS.md.

Tests for milestones that have not landed are gated with `#[ignore = "M<n> not implemented"]` and are un-ignored as each milestone lands.

## Status by milestone

_Last run: 2026-09-18, debug build, `cargo test -p pollard-cli`: **122 passed, 0 failed, 1 ignored** (the slow timing test, run separately in release). Timing budgets use `common::budget()`: the spec value in `--release`, 5x in debug (PLAN C5 says timings are release numbers)._

| Milestone | File | Tests | Status |
| --- | --- | --- | --- |
| M1 core (+ plain fork, git tree hash) | `m1_core.rs` | 26 | **pass** 26/26 |
| M2 siblings | `m2_siblings.rs` | 16 | **pass** 16/16 |
| M3 metrics | `m3_metrics.rs` | 13 | **pass** 13/13 |
| M4 objects | `m4_objects.rs` | 16 (includes the CDC review check) | **pass** 16/16 |
| M5 uv | `m5_uv.rs` | 9 | **pass** 9/9 |
| M6 git | `m6_git.rs` | 7 | **pass** 7/7 |
| M7 remote (local path) | `m7_remote.rs` | 5 | **pass** 5/5 |
| M8 sweeps | `m8_sweeps.rs` | 12 | **pass** 12/12 |
| M9 python | `m9_python.rs` | 7 | **pass** 7/7 |
| Journeys A–D | `journeys.rs` | 5 | **pass** 5/5 |
| Cross-cutting | `cross_cutting.rs` | 7 | **pass** 6/6, plus the slow 1,000-node test, which passes in release (see below) |

**1,000-node timing (§10), `commands_under_200ms_on_1000_node_repo`:** passes with `cargo test --release`. In a debug build under the raw 200 ms limit, `prune` (306 ms) and `undo` (258 ms) went over. Everything else was under 200 ms even in debug.

## Open failures

None.

### Closed
- F1 (`apply` conflicted on adjacent-line config edits, M8) and F2 (`siblings --expand-sweeps` was ignored, M8): fixed by LEAD.
- F3 (killed status): not a product bug. The journey now simulates Maya's kill as Ctrl-C (SIGINT to pollard's process group), and `cross_cutting::ctrl_c_during_run_marks_node_killed` covers it. See Q6.
- F4 (`init --from-git` printed no summary listing off-tree files, Journey A): fixed.
- A README `!README.md` negation failure seen in the first M2 run was fixed before it was filed.

## Spec questions

Each test follows the most literal reading; these are the places where that reading was a choice.

- **Q1 (M1, all output):** "current node" is found from `show @`: the test takes the first node-id-shaped token in its output. `show` must print the node's own id before any other id (for example, before the parent's).
- **Q2 (M2):** The sibling-table test needs blank cells to render as a placeholder (`—`, as in the §6 example) or as an empty bordered cell. Arrows may be `→` or `->`, and minus may be `−` or `-`. The code-row summary format follows the §6 examples literally: `+attn.py` and `−util.py`.
- **Q3 (M3):** `siblings` metric rows are matched by a label containing the key (`loss…`). The `last_common` row must carry the step number (for example `loss@50`).
- **Q4 (M5):** Without `pyproject.toml`/`uv.lock`, §4 says to run `python <file>`. This machine has only `python3` on PATH, so the test expects the CLI to fall back to `python3` when `python` is missing.
- **Q5 (M8/Journey D):** `run --sweep seeds train.py seed=$s`: the command line is not part of the recipe, so 16 runs that differ only in argv are duplicates of each other. The tests write the seed into `config.yaml` as well. If argv overrides are meant to be captured (Hydra-style), the spec should say so.
- **Q6 (Journey C):** "Maya kills it" is tested as Ctrl-C, meaning SIGINT to the process group, which gives status `killed`. The spec does not say what status a child that dies of a signal on its own should get. Under `uv run` the signal comes back to pollard as an exit code, so pollard records `failed`.
- **Q7 (Journey A):** The init budget is "< 5 s on 200 files + 10 GB dataset". The test uses a 20 MB `data/` directory (not declared as a data root, so it is hashed as code).
- **Q8 (M7):** The test sets the remote key as `remote = "<path>"` in `.pollard/config.toml`, the name used by core's config template.
- **Q9 (M6/Journey D):** For the `--path` export, commit count is taken from `git rev-list <branch> ^main`, falling back to the full branch history, so both "root commit is parentless" and "root commit is parented on HEAD" designs pass.
- **Q10 (M8/Journey B):** Should `apply` merge the captured config file structurally, key by key, rather than as text? Journey B's "no conflict" only holds for a structural merge or for a merge that accepts adjacent-line edits (F1).

## Coverage notes

- **Not covered:** S3 remotes (no endpoint available), `wandb`/`neptune` forwarders (need accounts), real PyTorch/GPU runs, the 10 GB dataset, and `pull` refusing a colliding id with a different recipe (a collision can't be forced from the CLI).
- **Slow test:** the 1,000-node 200 ms budget (`cross_cutting::commands_under_200ms_on_1000_node_repo`) is `#[ignore]`d as slow. It is run manually with `--ignored`, and the result is recorded here.
- **CDC property test (SR, pollard-objects), reviewed and meaningful:** `crates/pollard-objects/tests/cdc.rs::boundaries_stable_under_insertion` runs 24 proptest cases over 2–3 MB random buffers, inserting 1–5000 random bytes at a random offset. It asserts that every chunk ending before the insertion point survives and that at most 3 chunks change, so the chunk stream resynchronises. `one_percent_change_shares_95_percent` also checks the M4 criterion at the library level, by count and by bytes. Minor: it does not check a zero-length edge (insertion at offset 0 or at the end), but proptest's `at_frac` range covers values near both ends.
- **Test-harness notes:** tests run with `GIT_CONFIG_GLOBAL=/dev/null`, because the host's global git config forces gpg signing and made fixture commits fail. `pollard` is prepended to PATH for scripts and the SDK.
