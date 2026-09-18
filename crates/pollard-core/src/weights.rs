//! Weights manifests (§3, §5 Tier 2): checkpoints and artifacts attached to a node,
//! `fork --step` checkpoint restore, `prune`, `gc`. Owner: senior dev.
//!
//! A node's `weights` is a manifest of repo-root-relative paths. Entries under
//! `checkpoint_dir` are checkpoints; anything else is an artifact. Large files are
//! CDC-chunked by the store, so sibling checkpoints share chunks.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use pollard_objects::{Entry, GcStats, MODE_FILE, Manifest, WalkOptions};
use rusqlite::params;

use crate::node::{self, Node, Status};
use crate::{IoCtx, OpRecord, Repo, Result, msg, ops};

/// `(mtime_ns, size)` per file under the checkpoint dir.
pub type CkptState = HashMap<PathBuf, (u128, u64)>;

/// State of the checkpoint dir; take it before launching so `register_new` can tell
/// which files the run wrote.
pub fn ckpt_state(repo: &Repo) -> Result<CkptState> {
    let dir = crate::run::ckpt_dir(repo);
    let mut out = CkptState::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for ent in ignore::WalkBuilder::new(&dir)
        .standard_filters(false)
        .build()
    {
        let ent = ent.map_err(|e| msg(format!("{}: {e}", dir.display())))?;
        let md = std::fs::metadata(ent.path()).at(ent.path())?;
        if md.is_file() {
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
            out.insert(
                ent.path().to_path_buf(),
                (mtime.map_or(0, |d| d.as_nanos()), md.len()),
            );
        }
    }
    Ok(out)
}

/// At run end: add every file in the checkpoint dir that is new or changed since `before`
/// to `node`'s weights manifest. Returns the new weights hash, if anything was added.
pub fn register_new(repo: &Repo, node: &str, before: &CkptState) -> Result<Option<String>> {
    let now = ckpt_state(repo)?;
    let mut changed: Vec<PathBuf> = now
        .into_iter()
        .filter(|(p, s)| before.get(p) != Some(s))
        .map(|(p, _)| p)
        .collect();
    if changed.is_empty() {
        return Ok(None);
    }
    changed.sort();
    let _lock = repo.lock()?;
    add_files(repo, node, &changed).map(Some)
}

/// `pollard ckpt <path>` / `pollard artifact <path>`: store files (or directories) now and
/// add them to `node`'s weights manifest. Paths must lie inside the repo.
pub fn attach(repo: &mut Repo, node: &str, paths: &[PathBuf]) -> Result<OpRecord> {
    let id = repo.resolve(node)?;
    let abs: Vec<PathBuf> = paths
        .iter()
        .map(|p| p.canonicalize().at(p))
        .collect::<Result<_>>()?;
    ops::record(repo, None, |repo| {
        add_files(repo, &id, &abs)?;
        Ok(id.clone())
    })
}

fn rel_path(repo: &Repo, abs: &Path) -> Result<String> {
    let rel = abs.strip_prefix(&repo.root).map_err(|_| {
        msg(format!(
            "{}: outside the repo {}",
            abs.display(),
            repo.root.display()
        ))
    })?;
    rel.to_str()
        .map(str::to_string)
        .ok_or_else(|| msg(format!("{}: path is not UTF-8", abs.display())))
}

/// Store `files` (absolute) and merge them into `node`'s weights manifest; same path replaces.
fn add_files(repo: &Repo, node: &str, files: &[PathBuf]) -> Result<String> {
    let n = repo.node(node)?;
    let mut entries: BTreeMap<String, Entry> = match &n.weights {
        Some(h) => repo
            .objects
            .get_manifest(h)?
            .entries
            .into_iter()
            .map(|e| (e.path.clone(), e))
            .collect(),
        None => BTreeMap::new(),
    };
    for abs in files {
        let rel = rel_path(repo, abs)?;
        if abs.is_dir() {
            for e in repo
                .objects
                .snapshot_dir(abs, &WalkOptions::default())?
                .manifest
                .entries
            {
                let path = format!("{rel}/{}", e.path);
                entries.insert(path.clone(), Entry { path, ..e });
            }
        } else {
            let (hash, size) = repo.objects.put_file(abs)?;
            entries.insert(
                rel.clone(),
                Entry {
                    path: rel,
                    size,
                    hash,
                    mode: MODE_FILE,
                },
            );
        }
    }
    let h = repo
        .objects
        .put_manifest(&Manifest::new(entries.into_values().collect()))?;
    repo.db
        .execute("UPDATE nodes SET weights=?1 WHERE id=?2", params![h, node])?;
    Ok(h)
}

/// Training step of a checkpoint (§4 v3): the last digit run in the file name
/// (`step30000.pt` → 30000, `epoch3-step500.ckpt` → 500, `last.pt` → none).
pub fn step_of(path: &str) -> Option<i64> {
    let name = path.rsplit('/').next()?;
    name.split(|c: char| !c.is_ascii_digit())
        .filter(|d| !d.is_empty())
        .last()?
        .parse()
        .ok()
}

/// Checkpoints of a node (weights entries under `checkpoint_dir`) with their parsed step.
pub fn checkpoints(repo: &Repo, n: &Node) -> Result<Vec<(Entry, Option<i64>)>> {
    let Some(w) = &n.weights else {
        return Ok(vec![]);
    };
    let prefix = format!(
        "{}/",
        repo.config
            .checkpoint_dir
            .trim_start_matches("./")
            .trim_end_matches('/')
    );
    Ok(repo
        .objects
        .get_manifest(w)?
        .entries
        .into_iter()
        .filter(|e| e.path.starts_with(&prefix))
        .map(|e| {
            let s = step_of(&e.path);
            (e, s)
        })
        .collect())
}

/// `fork --step N`: restore the newest checkpoint with step <= N, looking in `node` and then
/// up the fork-step chain (an ancestor only counts up to the step its child was forked at).
/// Writes it back to its recorded path under the repo. Returns `(path, step)` or `None`.
pub fn restore_step(repo: &Repo, node: &str, step: i64) -> Result<Option<(String, i64)>> {
    let mut cur = repo.node(node)?;
    let mut limit = step;
    loop {
        let best = checkpoints(repo, &cur)?
            .into_iter()
            .filter_map(|(e, s)| Some((s?, e)))
            .filter(|(s, _)| *s <= limit)
            .max_by_key(|(s, _)| *s);
        if let Some((s, e)) = best {
            repo.objects.restore_at(&e, &repo.root)?;
            return Ok(Some((e.path, s)));
        }
        match (cur.fork_step, cur.parent.clone()) {
            (Some(fs), Some(p)) => {
                limit = limit.min(fs);
                cur = repo.node(&p)?;
            }
            _ => return Ok(None),
        }
    }
}

/// `prune <node> [--keep-weights]`: mark the node and its whole subtree `pruned`.
/// Recipe, deltas and metrics stay; weights become collectable by `gc` unless kept.
pub fn prune(repo: &mut Repo, node: &str, keep_weights: bool) -> Result<OpRecord> {
    let id = repo.resolve(node)?;
    ops::record(repo, None, |repo| {
        let mut stack = vec![id.clone()];
        while let Some(n) = stack.pop() {
            repo.db
                .execute("UPDATE nodes SET status='pruned' WHERE id=?1", [&n])?;
            let key = format!("keep_weights:{n}");
            if keep_weights {
                repo.set_meta(&key, "1")?
            } else {
                repo.del_meta(&key)?
            }
            stack.extend(node::children(&repo.db, &n)?.into_iter().map(|c| c.id));
        }
        Ok(id.clone())
    })
}

/// Weights paths of `n` whose content is gone (collected by `gc` after a prune).
/// `undo` of a prune warns when this is non-empty (§3 v3: gc is the point of no return).
pub fn missing_weights(repo: &Repo, n: &Node) -> Result<Vec<String>> {
    let Some(w) = &n.weights else {
        return Ok(vec![]);
    };
    let m = repo.objects.get_manifest(w)?;
    Ok(m.entries
        .into_iter()
        .filter(|e| !repo.objects.has_blob(&e.hash))
        .map(|e| e.path)
        .collect())
}

/// `gc`: delete chunks not reachable from any live manifest. Live = weights of non-pruned
/// (or `--keep-weights`) nodes, every node's docs, every stored code manifest, and every
/// working-copy snapshot in the op log. Objects are never deleted.
pub fn gc(repo: &Repo) -> Result<GcStats> {
    let _lock = repo.lock()?;
    let mut live = HashSet::new();
    for n in node::all(&repo.db)? {
        if n.status != Status::Pruned || repo.meta(&format!("keep_weights:{}", n.id))?.is_some() {
            live.extend(n.weights);
        }
        live.extend(n.docs);
    }
    for sql in [
        "SELECT manifest_hash FROM code_trees",
        "SELECT wc_snapshot FROM ops WHERE wc_snapshot IS NOT NULL",
    ] {
        let mut st = repo.db.prepare(sql)?;
        for h in st.query_map([], |r| r.get::<_, String>(0))? {
            live.insert(h?);
        }
    }
    Ok(repo.objects.gc(&live.into_iter().collect::<Vec<_>>())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn steps_from_names() {
        for (p, s) in [
            ("ckpt/step30000.pt", Some(30000)),
            ("ckpt/epoch3-step_500.ckpt", Some(500)),
            ("ckpt/ckpt_30000.pt", Some(30000)),
            ("ckpt/30000.pt", Some(30000)),
            ("ckpt/last.pt", None),
        ] {
            assert_eq!(step_of(p), s, "{p}");
        }
    }

    fn mk(repo: &Repo, id: &str, parent: Option<&str>, fork_step: Option<i64>) {
        Node {
            id: id.into(),
            parent: parent.map(Into::into),
            code: "c".into(),
            config: "c".into(),
            data: "d".into(),
            env: "e".into(),
            recipe_hash: id.into(),
            weights: None,
            docs: None,
            fork_step,
            status: Status::Done,
            created_at: crate::now(),
            finished_at: None,
            command: "x".into(),
            note: None,
            note_auto: false,
            lock_ok: None,
            depth: 0,
            sweep: None,
        }
        .insert(&repo.db)
        .unwrap();
    }

    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    /// Write checkpoint files as a run would, then register them the way `run` does.
    fn train(repo: &Repo, node: &str, files: &[(&str, &[u8])]) {
        let before = ckpt_state(repo).unwrap();
        for (name, body) in files {
            let p = crate::run::ckpt_dir(repo).join(name);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
        register_new(repo, node, &before).unwrap();
    }

    #[test]
    fn register_restore_prune_gc() {
        let t = tempfile::tempdir().unwrap();
        let mut repo = Repo::init(t.path()).unwrap();
        let base = noise(3 << 20, 7);
        let mut other = base.clone();
        other[1 << 20..(1 << 20) + 5000].fill(1);

        mk(&repo, "a", None, None);
        train(
            &repo,
            "a",
            &[("step100.pt", &base), ("step200.pt", &noise(2 << 20, 9))],
        );
        mk(&repo, "b", Some("a"), Some(100));
        train(&repo, "b", &[("step300.pt", &other)]);
        let a = repo.node("a").unwrap();
        assert_eq!(checkpoints(&repo, &a).unwrap().len(), 2);
        // b only registered what it wrote
        let b = repo.node("b").unwrap();
        let cb = checkpoints(&repo, &b).unwrap();
        assert_eq!(cb.iter().map(|c| c.1).collect::<Vec<_>>(), [Some(300)]);

        // fork b --step 250: b has nothing <= 250, a only counts up to b's fork_step 100
        fs::remove_dir_all(crate::run::ckpt_dir(&repo)).unwrap();
        assert_eq!(
            restore_step(&repo, "b", 250).unwrap(),
            Some(("ckpt/step100.pt".into(), 100))
        );
        assert_eq!(fs::read(t.path().join("ckpt/step100.pt")).unwrap(), base);
        assert_eq!(restore_step(&repo, "b", 50).unwrap(), None);
        assert_eq!(restore_step(&repo, "a", 250).unwrap().unwrap().1, 200);

        // artifact via attach (an op)
        fs::create_dir_all(t.path().join("outputs/plots")).unwrap();
        fs::write(t.path().join("outputs/plots/loss.png"), b"png").unwrap();
        let rec = attach(&mut repo, "b", &[t.path().join("outputs/plots")]).unwrap();
        assert_eq!(rec.node, "b");
        let wb = repo
            .objects
            .get_manifest(&repo.node("b").unwrap().weights.unwrap())
            .unwrap();
        assert!(wb.get("outputs/plots/loss.png").is_some() && wb.get("ckpt/step300.pt").is_some());

        // prune b then gc: frees only b's unique chunks; a's checkpoints still readable
        assert_eq!(gc(&repo).unwrap().chunks_removed, 0);
        prune(&mut repo, "b", false).unwrap();
        assert_eq!(repo.node("b").unwrap().status, Status::Pruned);
        let st = gc(&repo).unwrap();
        assert!(st.chunks_removed > 0 && st.chunks_removed < 5, "{st:?}");
        for (e, _) in checkpoints(&repo, &repo.node("a").unwrap()).unwrap() {
            repo.objects.read_blob(&e.hash).unwrap();
        }
        assert!(repo.objects.read_blob(&cb[0].0.hash).is_err());
        assert_eq!(
            missing_weights(&repo, &repo.node("b").unwrap()).unwrap(),
            ["ckpt/step300.pt"]
        );
        assert!(
            missing_weights(&repo, &repo.node("a").unwrap())
                .unwrap()
                .is_empty()
        );
    }
}
