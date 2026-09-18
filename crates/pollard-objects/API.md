# pollard-objects API

Owner: senior dev. Everything is re-exported from the crate root. All hashes are lowercase
blake3 hex `String`s. Errors: `pollard_objects::Error` (thiserror), every variant names the
path or hash involved. Unix only (modes, symlinks).

## Store (`.pollard/` layout)

```
objects/ab/cdef…    zstd-3 blobs < 1 MB (file contents, manifests, anything small)
chunks/ab/cdef…     zstd-3 CDC chunks (keyed by blake3 of the raw chunk)
chunkmaps/ab/cdef…  text "chunk_hash size\n" per chunk, keyed by blake3 of the whole file
```

A blob hash is always blake3 of the full content, whether it is stored as an object or a
chunk map, so manifests never care which tier holds a file.

| Call | Notes |
| --- | --- |
| `Store::new(dot_pollard) -> Store` | Infallible; dirs are created on first write. `store.root()` |
| `put_object(&[u8]) -> Result<String>` | Idempotent, atomic (tmp + rename). |
| `get_object(&str) -> Result<Vec<u8>>` | Verifies the hash; `Error::NotFound` / `Error::Corrupt`. Objects only. |
| `has_object(&str)`, `has_blob(&str) -> bool` | `has_blob` = object or chunk map. |
| `put_file(&Path) -> Result<(hash, size)>` | `< SMALL_LIMIT` (1 MB) → object; otherwise streamed through CDC (8/64/128 KB) into `chunks/` + `chunkmaps/`. Never loads a large file whole. |
| `write_blob(&str, &mut dyn Write)`, `read_blob(&str) -> Vec<u8>` | Any blob (object or chunked), hash-verified. |
| `restore(&Entry, dest)` | Write one entry to `dest` atomically with its mode (symlinks recreated), mkdir -p. Use for `fork --step` checkpoint restore. |
| `chunks_of(&str) -> Result<Option<Vec<(chunk_hash, size)>>>` | `None` if not chunked. |
| `put_manifest(&Manifest) -> hash`, `get_manifest(&str) -> Manifest` | Manifest object id == `manifest.hash()`. |
| `snapshot_dir(root, &WalkOptions) -> Result<Snapshot>` | Walk + store every file + store the manifest object. For code and docs. |
| `materialize(&Manifest, root, &WalkOptions)` | Make the files selected by `opts` under `root` equal the manifest: deletes extras (and emptied dirs), writes missing/changed. Never touches files outside `opts` (ignored, off-tree, `.pollard/`, `.git/`). Checks every blob exists **before** changing anything. For fork/undo: snapshot the working copy first so nothing is lost. |
| `gc(live_manifests: &[String]) -> Result<GcStats>` | Mark-and-sweep: keeps chunk maps + chunks reachable from the given manifests, deletes all other chunks/maps. Objects are never deleted. Errors (deleting nothing) if a listed manifest is missing. Must run under the repo lock. |
| `list(kind) -> Vec<(hash, path, disk_size)>` | `kind` ∈ `objects`, `chunks`, `chunkmaps`. For remotes and stats. |

Constants: `SMALL_LIMIT` (1 MB), `CODE_FILE_LIMIT` (10 MB).
Free functions: `hash_bytes(&[u8])`, `hash_file(&Path)`.

## Manifests

```rust
pub struct Entry { pub path: String, pub size: u64, pub hash: String, pub mode: u32 }
pub struct Manifest { pub entries: Vec<Entry> }   // sorted by path, unique
```

- Serialized form: one `path\tsize\thash\tmode_octal\n` line per entry (spec order), UTF-8.
  `Manifest::new(entries)` sorts/dedups; `to_bytes()`, `parse(&[u8])`, `hash()`, `get(path)`,
  `total_size()`.
- `mode` is git-style: `0o100644`, `0o100755`, `0o120000` (symlink; `hash` = blake3 of the
  link target, `size` = target length). Same values git uses, so `pollard-git` can reuse them.
- Paths are relative, `/`-separated, UTF-8, no newlines (else `Error::BadPath`). Empty dirs
  are not recorded (like git).
- `diff(&old, &new) -> Vec<Change { path, kind: ChangeKind::{Added,Removed,Modified} }>`,
  sorted by path; mode change = modified. `Change`/`ChangeKind` are serde
  (`{"path":"a.py","kind":"added"}`), ready for `deltas.code_delta`.

## Walking a directory

```rust
pub struct WalkOptions {
    pub globs: Vec<String>,        // gitignore-style overrides relative to root
    pub ignore_files: bool,        // honour .gitignore + .pollardignore
    pub max_file_size: Option<u64>,
    pub offtree: Vec<String>,      // full gitignore semantics: last match wins, `!` re-includes
    pub offtree_only: bool,        // false: offtree files excluded (code); true: only them (docs)
}
WalkOptions::code(&excludes)       // ignore files on, 10 MB limit, each exclude negated
scan_dir(root, &opts) -> Result<Snapshot>   // hash only, stores nothing (data manifests, checks)
pub struct Snapshot { pub manifest: Manifest, pub oversized: Vec<(String, u64)> }
```

- Always excluded: `.pollard/`, `.git/`. Hidden files (`.python-version`, `.gitignore`) are
  included. Global gitignore, `.git/info/exclude`, `.ignore` and parent-dir ignore files
  are **not** read, so manifests are reproducible across machines.
- `globs`: `!pat` excludes; a plain `pat` whitelists (once any plain glob is present only
  matching files are walked). A glob ending in `/` covers everything below that dir.
  - Code: `WalkOptions::code(&[offtree…, output_dirs…, checkpoint_dir…])`, e.g.
    `["*.md", "notes/", "outputs/", "ckpt/"]`. Oversized files land in `snapshot.oversized`
    for the §3 warning.
  - Off-tree (v3, `!README.md` works): set `offtree` instead of putting the globs in `globs`.
    Code: `WalkOptions { offtree, ..WalkOptions::code(&[output_dirs…, ckpt_dir]) }`;
    docs: `WalkOptions { offtree, offtree_only: true, ignore_files: true, ..Default::default() }`.
  - Data roots: `WalkOptions::default()` + `scan_dir`.
- This is the C3 walk; `pollard_core::walk::code_files` can just build a `WalkOptions`.

## Refcounts (C1 decision)

No refcount tables and no `incref`/`decref`: `gc` is mark-and-sweep from the manifests the
caller says are live. Core computes the live set (e.g. weights manifests of non-pruned
nodes, plus those referenced by the last N op snapshots for D-18, plus every code/docs
manifest). This cannot drift under `undo` and keeps `pollard-objects` free of SQLite; the
`chunks`/`chunk_map` tables in §3 are not needed. See docs/DECISIONS.md D-28.

## pollard-git: code tree hash (C2)

```rust
pollard_git::tree_hash(&store, &manifest) -> pollard_git::Result<String>  // 40-hex sha1
```

Git tree hash of a manifest whose contents are already in the store (i.e. after
`store.snapshot_dir`). Equal to `git add -A && git write-tree` for the same files/modes
(tested against the git binary, incl. symlinks, exec bits, git's `a.txt` vs `a/` ordering).
Empty manifest → `4b825dc…` (git's empty tree). Error type `pollard_git::Error`
(`Objects(pollard_objects::Error)` | `Git(String)`). Usage in `run`:

```rust
let snap = repo.objects.snapshot_dir(&root, &WalkOptions::code(&excludes))?;
let code = pollard_git::tree_hash(&repo.objects, &snap.manifest)?;   // node.code
let code_manifest = snap.manifest.hash();                            // D-13 code_trees row
```

## pollard-core::weights (M4, senior dev)

`crates/pollard-core/src/weights.rs`. Weights manifest paths are repo-root relative;
entries under `checkpoint_dir` are checkpoints, anything else is an artifact.

| Fn | Use |
| --- | --- |
| `ckpt_state(&Repo) -> CkptState` | Before launch: `(mtime, size)` of files in the ckpt dir. |
| `register_new(&Repo, node, &CkptState) -> Option<weights_hash>` | After the child exits: stores new/changed ckpt files, updates `nodes.weights`. Takes the lock itself (not an op, like `finish`). |
| `attach(&mut Repo, node, &[PathBuf]) -> OpRecord` | `pollard ckpt <path>` and `pollard artifact <path>` (files or dirs, must be inside the repo; relative paths are resolved against the process cwd). |
| `step_of(path) -> Option<i64>` | Last digit run in the file name (v3). |
| `checkpoints(&Repo, &Node) -> Vec<(Entry, Option<step>)>` | For `show` ("checkpoints: 10k 20k"). |
| `restore_step(&Repo, node, N) -> Option<(path, step)>` | `fork --step`: largest step ≤ N in `node`, then up the fork-step chain (metric inheritance rule); writes the file back to its recorded path. |
| `prune(&mut Repo, node, keep_weights) -> OpRecord` | Whole subtree → `pruned`; `--keep-weights` stored as meta `keep_weights:<id>`. |
| `gc(&Repo) -> GcStats` | Live = weights of non-pruned/kept nodes + all docs + all `code_trees` manifests + all `ops.wc_snapshot`. Not an op (not undoable). |
| `missing_weights(&Repo, &Node) -> Vec<path>` | For the undo-after-gc warning. |

## pollard-git: import/export (M6) and pollard-core::gitops

Low level (`pollard-git`, pure gix, never touches HEAD/index/worktree):
- `checkout_to(repo_dir, rev, dest) -> commit_id`: write a commit's files (modes, symlinks; submodules skipped) under `dest`.
- `export(repo_dir, &store, &[(Manifest, message)], branch) -> Vec<commit_id>`: linear commits, first on top of HEAD (none if unborn), force-updates `refs/heads/<branch>` only.
- `Store::put_bytes(&[u8]) -> (hash, size)` (pollard-objects) was added for in-memory blobs.

Core glue (`crates/pollard-core/src/gitops.rs`):
- `gitops::import(&mut Repo, rev) -> OpRecord`: root node, status `done`, `command = "pollard import <rev>"`, note `import <rev> (git <sha7>)`; `code` = tree hash of the commit's files after `wc::code_opts` (v3 §7); config/data/env/docs from the working copy; sets head; working copy untouched.
- `gitops::export(&Repo, spec, branch: Option<&str>) -> Exported { branch, commits: Vec<(node, commit)> }`: `spec` is `<node>` or `a..b` (a must be an ancestor of b). Tree = code manifest + current off-tree files + `.pollard-recipe.json` (node, parent, fork_step, code, config JSON, data manifest list, env, recipe_hash, command). Message: `<id> (<pins>): <title>` + config delta lines + `<primary> = v at step s`. Default branch `pollard/<last id>`. Not an op (touches no pollard state).

## pollard-remote + pollard-core::sync (M7)

`pollard-remote::Remote::open(url)`: local dir or `s3://bucket/prefix` (creds/endpoint from
`AWS_*` env; MinIO via `AWS_ENDPOINT` + `AWS_ALLOW_HTTP=true`). Remote layout: `objects/`,
`chunkmaps/` (individual files, stored form), `packs/<n>.pack|.idx` (chunks, ≤ 64 MB packs,
idx written last), `nodes.jsonl`, `pins.json`. `push_blobs`/`pull_blobs(&Store) -> Stats{files, bytes}`
upload/download only what the other side lacks. Store gained `read_raw/write_raw/has_raw`
(write_raw verifies objects/chunks and rejects non-hash names).

Core (`crates/pollard-core/src/sync.rs`, remote = `config.remote`, relative paths are
relative to the repo root):
- `sync::push(&Repo) -> SyncStats { nodes, pins, blobs: Stats }`: blobs first, then appends
  `nodes.jsonl` lines (`{node, deltas, metrics: blob, code_manifest}`) only for nodes changed
  locally since the last sync, then `pins.json` if pins changed locally. Second push writes
  nothing (tested). Not an op; takes the lock. Error without a remote names `config.toml`.
- `sync::pull(&mut Repo) -> (OpRecord, SyncStats)`: one op (undoable). Refuses first,
  changing nothing, if a remote id exists locally as a different node (different recipe
  and created_at). Last line per id wins; a local node is replaced only if unchanged
  locally since the last sync (so note/status/pins are last-writer-wins and unpushed local
  edits survive). Brings metrics, deltas, and `code_trees` rows.
- Sync bookkeeping lives in meta keys `sync:<id>` and `sync:pins`.

## python/ (M9)

`python/src/pollard/__init__.py` (130 lines, pure Python ≥ 3.10): `current()`, `Run.log(values, step=None)`,
`Run.save_checkpoint(path)` (copies into `POLLARD_CKPT_DIR` unless already there; registered at
run end), `Run.set_config(cfg)` (dict / OmegaConf-Hydra / dataclass / argparse Namespace; atomic
write to `POLLARD_CONFIG`), `fork_step()`, `siblings(node, metric=None)` (DataFrame if pandas,
else dict; shells out to `$POLLARD_BIN`/`pollard`/`po`), `forward("wandb"|"neptune")`
(`_forward.py`). `python/pyproject.toml`: maturin `bindings = "bin"`, dist `pollard-vcs`,
extras `pandas`/`wandb`/`neptune`; `uv build --wheel` in `python/` bundles `pollard` + `po`
(verified: `uv tool install <wheel>` gives working `pollard`/`po`, and `import pollard` works in the tool env).

## Integration requests

(for the lead)

1. **Register module**: I added `pub mod weights;` to `pollard-core/src/lib.rs` (one line) so it compiles/tests. Keep it.
2. **run**: in `run::execute`, `let before = weights::ckpt_state(repo)?;` before `cmd.spawn()`, and after the final `tail.poll(.., true)` call `weights::register_new(repo, &l.node, &before)?;` (before `finish`).
3. **fork --step**: in `cmd::fork`, after `wc::checkout`, when `step = Some(n)`:
   `let r = weights::restore_step(repo, &anchor.id, n)?;` then per v3 §4: if `r = Some((path, s))` and `s != n`, set meta `fork_step` to `s` and print a notice; print `restored <path>`; if `None`, print "no checkpoint ≤ N" (keep `fork_step = n`). Needs `restored` in `Forked` for the CLI. (Setting fork_step after the op is recorded means undo sees N; simplest is to call `restore_step` inside the `ops::record` closure — it only writes a file under `ckpt/`.)
4. **undo**: after restoring a snapshot, for nodes that are no longer pruned, `weights::missing_weights` non-empty → `warning: weights of <id> were garbage-collected: <paths>`.
5. **CLI**: `ckpt <path>...` and `artifact <path>...` → `weights::attach(repo, node, paths)` with node = `$POLLARD_NODE_ID` if set, else `@`; `prune <node> [--keep-weights]` → `weights::prune`; `gc [--auto]` → `weights::gc` (print `removed N chunks, freed X MB`; `--auto` can be the same call); `show` lists `weights::checkpoints`.

- **DONE** (senior): `WalkOptions.offtree` + `offtree_only` added, and I switched `wc::code_opts`/`docs_opts` over (the struct literal in `docs_opts` stopped compiling once the fields existed). Core tests green.
- **[lead → senior] Off-tree `!` negation (v3 §3, test `m2_siblings::readme_is_offtree_by_default_and_negation_opts_in`).**
  `offtree` globs need gitignore semantics incl. `!README.md`. With overrides, a plain glob flips the
  walk into whitelist mode, so core cannot express "exclude `*.md` except README.md" through
  `WalkOptions::globs`. Request: add `WalkOptions.offtree: Vec<String>` + `offtree_mode: Exclude | Only`
  (or two fields, your call) matched with `ignore::gitignore::GitignoreBuilder` (last match wins, `!`
  re-includes, `dir/` covers the dir) — `Exclude` for code manifests/materialize, `Only` for docs.
  Core builds them in exactly one place: `pollard_core::wc::{code_opts, docs_opts}`; I'll switch
  those two functions over as soon as the fields exist.
- **DONE** (senior): `gitops::import` uses `wc::code_hash`, so the `code_trees` row is written.
- **[lead → senior] `import` must insert a `code_trees` row** (`INSERT OR IGNORE INTO code_trees(git_tree, manifest_hash)`)
  so `fork`/`apply`/`diff` of an imported root work; call `pollard_core::wc::code_hash(repo, &manifest)`
  which does both (hash + row). Data/env/docs: `wc::data_manifest`, `env::store(repo, &env::inputs(..))`,
  `docs_opts` + `snapshot_dir`. Mutating fns go through `ops::record(repo, wc_snapshot, |repo| ...)`.
6. **import / init --from-git**: `init --from-git` = `Repo::init` then `gitops::import(repo, "HEAD")`; `import <rev>` → `gitops::import`. I added `pub mod gitops;` to lib.rs.
7. **duplicate check vs imported root**: the root from `import` is `done`, so the first `run` of an unchanged checkout (Journey A) is refused as a duplicate of the root. Please skip nodes with `command LIKE 'pollard import %'` in the duplicate query in `run::start` (they are snapshots of git, not runs).
8. **export CLI**: `export [<node>] [--path a..b] [--branch name]` → `gitops::export(repo, path.unwrap_or(node), branch)`; print one `<node> <commit7>` line per commit, then the branch, and the last node id on the last line.
9. **push / pull CLI** (verified: with exactly this wiring in a scratch copy, all 5 `m7_remote` tests pass with `--include-ignored`). Import `sync` from pollard_core, add `Push, Pull` variants, and:
   ```rust
   Cmd::Push => { let repo = open()?; let st = sync::push(&repo)?; println!("pushed {} nodes, {} bytes", st.nodes, st.blobs.bytes); }
   Cmd::Pull => { let mut repo = open()?; let (rec, st) = sync::pull(&mut repo)?; println!("pulled {} nodes, {} bytes", st.nodes, st.blobs.bytes); println!("{}", rec.node); }
   ```
   I added `pub mod sync;` to lib.rs and `pollard-remote` to pollard-core's Cargo.toml.
10. **Cargo.lock**: `idna_adapter` is pinned to 1.1.0 (newer pulls icu/yoke-derive 0.8.3, which needs rustc > 1.85). Don't `cargo update` blindly; see DECISIONS D-29.
11. **`note --append <text>`** (for the Python SDK's `forward("wandb")`, which records the foreign run id in the note, §4): append `\n<text>` to the existing note (creating it if empty). The SDK calls `pollard note <id> --append "wandb: <run id>"`.

