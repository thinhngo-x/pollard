# Alpha release checklist

Scope: `0.1.0-alpha.1`, the first published alpha of pollard. This checklist is docs-only:
it does not grant credentials or push anything. Every step that touches GitHub or a
package registry is marked **DO NOT RUN without owner go-ahead** and is dry-run only for
this team — as of 2026-09-18 there is no `gh auth`, no `~/.cargo/credentials.toml`, and no
`pip`/`twine`/`keyring` installed on this machine, so none of those steps can succeed even
if run.

## 1. Version scheme

- Crates (`pollard-core`, `pollard-objects`, `pollard-git`, `pollard-remote`, `pollard-cli`):
  `0.1.0-alpha.1`, set via `[workspace.package] version` in the root `Cargo.toml` (all five
  crates inherit it with `version.workspace = true`).
- PyPI (`pollard-vcs`): matching pre-release, `0.1.0a1` (PEP 440 form of `alpha.1`) in
  `python/pyproject.toml`.
- Subsequent alphas bump the trailing integer (`alpha.2` / `0.1.0a2`); do not reuse a
  published version.

## 2. Publish order (path-dependency order, verified against Cargo.toml)

Dependency edges among the five crates, read from each `crates/*/Cargo.toml`:

- `pollard-objects` — no path deps on other workspace crates.
- `pollard-git` — path dep on `pollard-objects`.
- `pollard-remote` — path dep on `pollard-objects`.
- `pollard-core` — path deps on `pollard-objects`, `pollard-git`, `pollard-remote`.
- `pollard-cli` — path dep on `pollard-core`.

`cargo publish` requires every path dependency to already be resolvable from crates.io (or
pinned with a matching `version =` alongside the `path =`, which `pollard-git` and
`pollard-remote` already do for `pollard-objects`). A valid topological order is:

```
1. pollard-objects
2. pollard-git       (needs pollard-objects on crates.io)
3. pollard-remote     (needs pollard-objects on crates.io; independent of pollard-git)
4. pollard-core       (needs pollard-objects, pollard-git, pollard-remote on crates.io)
5. pollard-cli        (needs pollard-core on crates.io)
```

`pollard-git` and `pollard-remote` do not depend on each other, so their relative order
doesn't matter — the order above (objects → git → remote → core → cli) satisfies every
edge and matches the spec's crate table (SPEC.md §8).

Python (`pollard-vcs`) is built and published separately, after `pollard-cli` — its
`maturin` build (`python/pyproject.toml`, `bindings = "bin"`) compiles the `pollard-cli`
binary from the workspace directly via `manifest-path`, so it does not need `pollard-cli`
on crates.io first, only a clean local build.

## 3. Preconditions — must all be true before publishing anything

- [ ] `git status` is clean (no uncommitted or untracked files) on `main`, and the commit
      being released is tagged in your notes (coordinator confirms).
- [ ] `cargo build --workspace` and `cargo test --workspace -- --include-ignored` pass.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean (lead dev).
- [ ] `cargo fmt --all -- --check` is clean (lead dev).
- [ ] No duplicate-bin-target warning from `pollard-cli` (lead dev; `pollard` and `po` are
      both `path = "src/main.rs"`, which is fine, but confirm `cargo build --release`
      emits no warning about it).
- [ ] `Cargo.toml` versions bumped to `0.1.0-alpha.1` across the workspace; `Cargo.lock`
      regenerated and committed.
- [ ] `python/pyproject.toml` version bumped to `0.1.0a1`.
- [ ] License files present and correct: `LICENSE-MIT`, `LICENSE-APACHE` at the repo root,
      matching `license = "MIT OR Apache-2.0"` in every `Cargo.toml` and in
      `python/pyproject.toml`.
- [ ] `README.md` accurate: alpha disclaimer present, install commands use `pollard-vcs`,
      command-status table matches what M1–M9 actually shipped.
- [ ] `CHANGELOG.md` has a dated `0.1.0-alpha.1` entry (not "Unreleased").
- [ ] Per crate, in publish order: `cargo publish --dry-run -p <crate>` is clean (no
      errors; warnings about the crate not existing on crates.io yet are expected for
      crates 2–5 until the one before it is actually published).
- [ ] `cd python && uv build` is clean and produces a wheel + sdist in `python/dist/`.
- [ ] `.github/workflows/` CI is green on the release commit (devops).

## 4. Publish steps — dry-run only, in order

Every command below is **DO NOT RUN without owner go-ahead**. This team has no
credentials configured (no `gh auth status` success, no `~/.cargo/credentials.toml`, no
`twine`/`keyring` installed), so treat every one of these as illustrative until the owner
explicitly authorizes it and credentials exist.

```sh
# 1. pollard-objects
cargo publish --dry-run -p pollard-objects          # DO NOT RUN without owner go-ahead (real publish: drop --dry-run)

# 2. pollard-git
cargo publish --dry-run -p pollard-git               # DO NOT RUN without owner go-ahead

# 3. pollard-remote
cargo publish --dry-run -p pollard-remote             # DO NOT RUN without owner go-ahead

# 4. pollard-core
cargo publish --dry-run -p pollard-core               # DO NOT RUN without owner go-ahead

# 5. pollard-cli
cargo publish --dry-run -p pollard-cli                 # DO NOT RUN without owner go-ahead

# Python: build then publish
cd python && uv build                                   # safe: local build only, no network write
uv publish --dry-run                                     # DO NOT RUN without owner go-ahead (or: twine upload --repository testpypi dist/*)

# Git / GitHub, once the above are all real (non-dry-run) and done
git push origin main                                      # DO NOT RUN without owner go-ahead
git tag v0.1.0-alpha.1 && git push origin v0.1.0-alpha.1  # DO NOT RUN without owner go-ahead
gh release create v0.1.0-alpha.1 --notes-file <(sed -n '/^## 0.1.0-alpha.1/,/^## /p' CHANGELOG.md | sed '$d')
                                                             # DO NOT RUN without owner go-ahead
```

Between real (non-dry-run) `cargo publish` calls, wait for the crate to actually appear on
crates.io (indexing can take a minute) before publishing the next one in the chain, since
its `Cargo.toml` path dependency needs a matching published version to resolve.

## 5. Rollback notes

- crates.io publishes cannot be un-published, only `cargo yank`ed (marks a version
  unusable for new dependents; existing lockfiles keep working). If a bad crate is
  published, yank it, fix, and publish `alpha.2`.
- PyPI pre-releases can be deleted from the project page manually (or left and superseded
  by the next pre-release) — no automated rollback here.
- A bad git tag or GitHub release can be deleted (`git push --delete origin <tag>`,
  `gh release delete`) — both are themselves outward-facing and need owner go-ahead.
