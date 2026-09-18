# Contributing to pollard

## Build

```sh
cargo build --workspace
```

## Test

```sh
cargo test --workspace -- --include-ignored
```

Some integration tests are marked `#[ignore]` because they are slow or need a tool
(`uv`, `git`) on `PATH`; `--include-ignored` runs them too. CI should always run with
this flag.

For the Python SDK:

```sh
cd python && uv run pytest   # if/when Python-side tests exist there
```

## Crate layout

The Rust workspace (`crates/*`) mirrors SPEC.md §8:

| Crate | Responsibility |
| --- | --- |
| `pollard-objects` | Tier-1 object store, manifests, CDC chunking, packs, gc. No workspace deps. |
| `pollard-git` | Git tree hashing (`gix`), `import`/`export`. Depends on `pollard-objects`. |
| `pollard-remote` | S3-compatible and local-path remotes, push/pull. Depends on `pollard-objects`. |
| `pollard-core` | Node model, SQLite store, op log, deltas, sibling join, metric inheritance, run protocol. Depends on `pollard-objects`, `pollard-git`, `pollard-remote`. |
| `pollard-cli` | The `pollard` binary and `po` alias (thin `clap` layer). Depends on `pollard-core`. |

`python/` holds the pure-Python SDK (`import pollard`, PyPI distribution `pollard-vcs`),
built as a wheel that bundles the `pollard-cli` binary via `maturin` (`bindings = "bin"`).
It never links to Rust directly; it only reads the run-protocol env vars and files.

## Where things are decided

- `SPEC.md` is the source of truth for behavior. Read it first — it defines the data
  model (§3), commands (§4), and the milestone acceptance tests (§9).
- `docs/PLAN.md` tracks per-milestone tasks and owners.
- `docs/DECISIONS.md` records why each spec ambiguity was resolved the way it was.
- `docs/RELEASE.md` has the alpha release checklist and publish order.

## Before opening a PR

- `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings`.
- Add or update a `CHANGELOG.md` entry under `Unreleased` (or the next alpha) if the
  change is user-visible.
- If you touch the node schema, the recipe definition, the run protocol variables, the
  fork-step rule, or add a new storage tier/remote type, that needs owner sign-off first
  (SPEC.md §11, "Decisions that need the owner").
