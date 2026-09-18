//! Git interop for pollard: git tree hashing for the `code` field (§7), import, export.
//! Pure Rust via gix; no git binary needed at runtime.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use gix::ObjectId;
use gix::objs::tree::{Entry as TreeEntry, EntryKind};
use gix::objs::{Kind, Tree, Write as _, WriteTo};
use gix::refs::transaction::PreviousValue;
use pollard_objects::{Entry, MODE_EXEC, MODE_LINK, Manifest, Store};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Objects(#[from] pollard_objects::Error),
    #[error("git: {0}")]
    Git(String),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },
}

fn io(p: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io { path: p.to_path_buf(), source }
}

pub(crate) fn git_err(e: impl std::fmt::Display) -> Error {
    Error::Git(e.to_string())
}

/// Git tree hash (sha1 hex) of a manifest whose contents are in `store`. Identical to what
/// `git add -A && git write-tree` gives for the same files and modes.
pub fn tree_hash(store: &Store, m: &Manifest) -> Result<String> {
    let mut sink = |kind: Kind, data: &[u8]| {
        gix::objs::compute_hash(gix::hash::Kind::Sha1, kind, data).map_err(git_err)
    };
    Ok(write_tree(store, m, &mut sink)?.to_string())
}

/// Build the git tree for `m`, handing every blob and tree object to `sink`
/// (which hashes it, or also writes it to a repository). Returns the root tree id.
pub(crate) fn write_tree(
    store: &Store,
    m: &Manifest,
    sink: &mut dyn FnMut(Kind, &[u8]) -> Result<ObjectId>,
) -> Result<ObjectId> {
    let items: Vec<(&str, &Entry)> = m.entries.iter().map(|e| (e.path.as_str(), e)).collect();
    subtree(store, &items, sink)
}

/// `items` are sorted by path relative to this tree, so each subdirectory is contiguous.
fn subtree(
    store: &Store,
    items: &[(&str, &Entry)],
    sink: &mut dyn FnMut(Kind, &[u8]) -> Result<ObjectId>,
) -> Result<ObjectId> {
    let mut tree = Tree::empty();
    let mut i = 0;
    while i < items.len() {
        let (rel, e) = items[i];
        match rel.split_once('/') {
            None => {
                let oid = sink(Kind::Blob, &store.read_blob(&e.hash)?)?;
                let kind = match e.mode {
                    MODE_EXEC => EntryKind::BlobExecutable,
                    MODE_LINK => EntryKind::Link,
                    _ => EntryKind::Blob,
                };
                tree.entries.push(TreeEntry { mode: kind.into(), filename: rel.into(), oid });
                i += 1;
            }
            Some((dir, _)) => {
                let prefix = format!("{dir}/");
                let end = i + items[i..].iter().take_while(|(p, _)| p.starts_with(&prefix)).count();
                let sub: Vec<_> = items[i..end].iter().map(|(p, e)| (&p[prefix.len()..], *e)).collect();
                let oid = subtree(store, &sub, sink)?;
                tree.entries.push(TreeEntry { mode: EntryKind::Tree.into(), filename: dir.into(), oid });
                i = end;
            }
        }
    }
    tree.entries.sort();
    let mut buf = Vec::new();
    tree.write_to(&mut buf).map_err(git_err)?;
    sink(Kind::Tree, &buf)
}

/// Write every file of commit `rev` (of the git repo containing `repo_dir`) under `dest`,
/// with modes and symlinks; submodules are skipped. Reads objects only: HEAD, the index
/// and the worktree are untouched. Returns the full commit id.
pub fn checkout_to(repo_dir: &Path, rev: &str, dest: &Path) -> Result<String> {
    let repo = gix::discover(repo_dir).map_err(git_err)?;
    let commit = repo
        .rev_parse_single(rev)
        .map_err(|e| Error::Git(format!("{rev}: {e}")))?
        .object()
        .map_err(git_err)?
        .peel_to_commit()
        .map_err(|e| Error::Git(format!("{rev}: {e}")))?;
    write_dir(&repo, commit.tree_id().map_err(git_err)?.detach(), dest)?;
    Ok(commit.id.to_string())
}

fn write_dir(repo: &gix::Repository, tree: ObjectId, dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(io(dir))?;
    let tree = repo.find_tree(tree).map_err(git_err)?;
    let entries: Vec<_> = tree
        .decode()
        .map_err(git_err)?
        .entries
        .iter()
        .map(|e| (dir.join(OsStr::from_bytes(e.filename)), e.mode.kind(), e.oid.to_owned()))
        .collect();
    for (path, kind, oid) in entries {
        match kind {
            EntryKind::Tree => write_dir(repo, oid, &path)?,
            EntryKind::Commit => {} // submodule
            EntryKind::Link => {
                let blob = repo.find_blob(oid).map_err(git_err)?;
                std::os::unix::fs::symlink(OsStr::from_bytes(&blob.data), &path).map_err(io(&path))?;
            }
            EntryKind::Blob | EntryKind::BlobExecutable => {
                fs::write(&path, &repo.find_blob(oid).map_err(git_err)?.data).map_err(io(&path))?;
                if kind == EntryKind::BlobExecutable {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).map_err(io(&path))?;
                }
            }
        }
    }
    Ok(())
}

/// Write one commit per `(manifest, message)`, each on top of the previous and the first on
/// top of HEAD (none if HEAD is unborn), then force `refs/heads/<branch>` to the last.
/// Only objects and that one ref are written: HEAD, the index and the worktree are untouched.
/// Returns the commit ids in order.
pub fn export(repo_dir: &Path, store: &Store, commits: &[(Manifest, String)], branch: &str) -> Result<Vec<String>> {
    let mut repo = gix::discover(repo_dir).map_err(git_err)?;
    let sig = repo.committer_or_set_generic_fallback().map_err(git_err)?.to_owned().map_err(git_err)?;
    let mut parent = repo.head_id().ok().map(|id| id.detach());
    let mut ids = Vec::new();
    for (m, message) in commits {
        let tree = write_tree(store, m, &mut |kind, data| repo.objects.write_buf(kind, data).map_err(git_err))?;
        let mut message = message.clone();
        if !message.ends_with('\n') {
            message.push('\n');
        }
        let commit = gix::objs::Commit {
            tree,
            parents: parent.into_iter().collect(),
            author: sig.clone(),
            committer: sig.clone(),
            encoding: None,
            message: message.into(),
            extra_headers: vec![],
        };
        let id = repo.write_object(&commit).map_err(git_err)?.detach();
        parent = Some(id);
        ids.push(id.to_string());
    }
    let Some(tip) = parent.filter(|_| !commits.is_empty()) else { return Ok(ids) };
    repo.reference(format!("refs/heads/{branch}"), tip, PreviousValue::Any, "pollard export")
        .map_err(|e| Error::Git(format!("branch {branch}: {e}")))?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pollard_objects::WalkOptions;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").args(args).current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn matches_git_write_tree() {
        let t = tempfile::tempdir().unwrap();
        let wc = t.path().join("wc");
        let w = |rel: &str, body: &str| {
            let p = wc.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        };
        // tricky git ordering: "a.txt" vs dir "a", "a-b", nested dirs
        w("a.txt", "1");
        w("a/b.py", "2");
        w("a-b", "3");
        w("a/c/d.py", "4");
        w("z.sh", "#!/bin/sh\n");
        fs::set_permissions(wc.join("z.sh"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("a.txt", wc.join("link")).unwrap();

        let store = Store::new(t.path().join("dot"));
        let m = store.snapshot_dir(&wc, &WalkOptions::default()).unwrap().manifest;
        git(&wc, &["init", "-q"]);
        git(&wc, &["add", "-A"]);
        assert_eq!(tree_hash(&store, &m).unwrap(), git(&wc, &["write-tree"]));
        // empty manifest = git's well-known empty tree
        assert_eq!(tree_hash(&store, &Manifest::default()).unwrap(), "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
    }

    #[test]
    fn import_export_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let wc = t.path().join("wc");
        fs::create_dir_all(wc.join("src")).unwrap();
        fs::write(wc.join("src/a.py"), "a").unwrap();
        fs::write(wc.join("run.sh"), "#!/bin/sh").unwrap();
        fs::set_permissions(wc.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("run.sh", wc.join("link")).unwrap();
        git(&wc, &["init", "-q"]);
        git(&wc, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
        git(&wc, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
        let head = git(&wc, &["rev-parse", "HEAD"]);
        let head_tree = git(&wc, &["rev-parse", "HEAD^{tree}"]);

        // import: materialize HEAD elsewhere, snapshot, hash == HEAD's tree
        let out = t.path().join("imp");
        assert_eq!(checkout_to(&wc, "HEAD", &out).unwrap(), head);
        let store = Store::new(t.path().join("dot"));
        let m = store.snapshot_dir(&out, &pollard_objects::WalkOptions::default()).unwrap().manifest;
        assert_eq!(tree_hash(&store, &m).unwrap(), head_tree);
        assert!(checkout_to(&wc, "no-such-rev", &out).is_err());

        // export: two linear commits on top of HEAD, HEAD/index/status untouched
        let mut m2 = m.clone();
        let (h, size) = store.put_bytes(b"{}").unwrap();
        m2.entries.push(Entry { path: ".pollard-recipe.json".into(), size, hash: h, mode: 0o100644 });
        let m2 = Manifest::new(m2.entries);
        fs::write(wc.join("dirty.txt"), "x").unwrap();
        let status = git(&wc, &["status", "--porcelain"]);
        let ids = export(&wc, &store, &[(m.clone(), "one".into()), (m2, "two\n\nbody".into())], "exp").unwrap();
        assert_eq!(git(&wc, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&wc, &["status", "--porcelain"]), status);
        assert_eq!(git(&wc, &["rev-parse", "exp"]), ids[1]);
        assert_eq!(git(&wc, &["rev-list", "--parents", "-n1", "exp"]), format!("{} {}", ids[1], ids[0]));
        assert_eq!(git(&wc, &["rev-list", "--parents", "-n1", &ids[0]]), format!("{} {head}", ids[0]));
        assert_eq!(git(&wc, &["rev-parse", &format!("{}^{{tree}}", ids[0])]), head_tree);
        assert_eq!(git(&wc, &["show", "-s", "--format=%s", "exp"]), "two");
        assert_eq!(git(&wc, &["show", "exp:.pollard-recipe.json"]), "{}");
    }
}
