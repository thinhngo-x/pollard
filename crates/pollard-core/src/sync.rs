//! `push` / `pull` (§5 Remote). Owner: senior dev. Blob transfer is `pollard-remote`.
//!
//! `nodes.jsonl` is append-only: one line per node version `{node, deltas, metrics,
//! code_manifest}` (metrics as a blob hash). The last line for an id wins, which makes
//! `note`/`status` last-writer-wins. `pins.json` is the whole pin map, last writer wins.
//! Meta `sync:<id>` / `sync:pins` hold the hash of what this clone last pushed or pulled,
//! so a push only sends local changes and a pull never clobbers unpushed local edits.

use std::collections::{BTreeMap, HashMap};

use pollard_remote::{Remote, Stats};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::delta::{self, Deltas};
use crate::node::{self, Node};
use crate::{OpRecord, Repo, Result, msg, ops, wc};

#[derive(Serialize, Deserialize)]
struct Line {
    node: Node,
    deltas: Deltas,
    /// blob hash of `[[key, step, value, ts], ...]`, or None if no metrics
    metrics: Option<String>,
    /// code manifest behind `node.code` (the `code_trees` row)
    code_manifest: Option<String>,
}

fn remote_err(e: pollard_remote::Error) -> crate::Error {
    msg(e.to_string())
}

fn open(repo: &Repo) -> Result<Remote> {
    let url = repo.config.remote.as_deref().ok_or_else(|| {
        msg(format!(
            "no remote configured: set `remote = \"<path or s3://bucket/prefix>\"` in {}",
            repo.dot.join("config.toml").display()
        ))
    })?;
    let url = if url.contains("://") || url.starts_with('/') {
        url.to_string()
    } else {
        repo.root.join(url).display().to_string()
    };
    Remote::open(&url).map_err(remote_err)
}

fn line_of(repo: &Repo, n: &Node) -> Result<(String, String)> {
    let mut st = repo.db.prepare(
        "SELECT key, step, value, ts FROM metrics WHERE node_id=?1 ORDER BY key, step, ts",
    )?;
    let rows: Vec<(String, i64, f64, String)> = st
        .query_map([&n.id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let metrics = match rows.is_empty() {
        true => None,
        false => Some(repo.objects.put_bytes(&serde_json::to_vec(&rows)?)?.0),
    };
    let line = Line {
        node: n.clone(),
        deltas: delta::load(repo, &n.id)?,
        metrics,
        code_manifest: wc::manifest_of(repo, &n.code).ok(),
    };
    let json = serde_json::to_string(&line)?;
    Ok((crate::b3(json.as_bytes()), json))
}

/// `(id, line hash, line)` per node.
type RemoteLines = Vec<(String, String, Line)>;

/// Raw `nodes.jsonl` and the latest line per id, in first-seen order.
fn remote_nodes(remote: &Remote) -> Result<(Vec<u8>, RemoteLines)> {
    let raw = remote
        .get("nodes.jsonl")
        .map_err(remote_err)?
        .unwrap_or_default();
    let mut order: Vec<String> = vec![];
    let mut latest: HashMap<String, (String, Line)> = HashMap::new();
    for (i, text) in String::from_utf8_lossy(&raw).lines().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        let line: Line = serde_json::from_str(text)
            .map_err(|e| msg(format!("remote nodes.jsonl line {}: {e}", i + 1)))?;
        let id = line.node.id.clone();
        if !latest.contains_key(&id) {
            order.push(id.clone());
        }
        latest.insert(id, (crate::b3(text.as_bytes()), line));
    }
    let out = order.into_iter().map(|id| {
        let (h, l) = latest.remove(&id).expect("inserted above");
        (id, h, l)
    });
    Ok((raw, out.collect()))
}

fn local_pins(repo: &Repo) -> Result<BTreeMap<String, String>> {
    let mut st = repo.db.prepare("SELECT name, node_id FROM pins")?;
    let v = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// What `push`/`pull` did.
#[derive(Debug, Default)]
pub struct SyncStats {
    pub nodes: usize,
    pub pins: bool,
    pub blobs: Stats,
}

/// Upload missing blobs, then append lines for nodes changed locally since the last sync,
/// then `pins.json` if the pins changed locally. A push with nothing new writes nothing.
pub fn push(repo: &Repo) -> Result<SyncStats> {
    let remote = open(repo)?;
    let _lock = repo.lock()?;
    let (mut raw, remote_lines) = remote_nodes(&remote)?;
    let remote_hash: HashMap<&str, &str> = remote_lines
        .iter()
        .map(|(id, h, _)| (id.as_str(), h.as_str()))
        .collect();
    let mut new = vec![];
    let mut markers = vec![];
    for n in node::all(&repo.db)? {
        let (h, json) = line_of(repo, &n)?;
        let key = format!("sync:{}", n.id);
        let synced = repo.meta(&key)?;
        if remote_hash.get(n.id.as_str()) != Some(&h.as_str())
            && synced.as_deref() != Some(h.as_str())
        {
            new.push(json);
        }
        markers.push((key, h));
    }
    let mut stats = SyncStats {
        blobs: remote.push_blobs(&repo.objects).map_err(remote_err)?,
        ..Default::default()
    };
    if !new.is_empty() {
        if !raw.is_empty() && !raw.ends_with(b"\n") {
            raw.push(b'\n');
        }
        for l in &new {
            raw.extend_from_slice(l.as_bytes());
            raw.push(b'\n');
        }
        stats.blobs.bytes += raw.len() as u64;
        remote.put("nodes.jsonl", raw).map_err(remote_err)?;
        stats.nodes = new.len();
    }
    let pins = serde_json::to_vec_pretty(&local_pins(repo)?)?;
    let ph = crate::b3(&pins);
    let remote_pins = remote.get("pins.json").map_err(remote_err)?;
    if remote_pins.as_deref().map(crate::b3).as_deref() != Some(ph.as_str())
        && repo.meta("sync:pins")?.as_deref() != Some(ph.as_str())
    {
        stats.blobs.bytes += pins.len() as u64;
        remote.put("pins.json", pins).map_err(remote_err)?;
        stats.pins = true;
    }
    for (key, hash) in markers {
        repo.set_meta(&key, &hash)?;
    }
    repo.set_meta("sync:pins", &ph)?;
    Ok(stats)
}

/// Download missing blobs and merge remote nodes and pins (one op, so `undo` reverts it).
/// A remote node replaces the local one only if the local one is unchanged since the last
/// sync. Refuses, before changing anything, a remote node whose id exists locally as a
/// different node (different recipe and creation time), naming the id.
pub fn pull(repo: &mut Repo) -> Result<(OpRecord, SyncStats)> {
    let remote = open(repo)?;
    let (_, lines) = remote_nodes(&remote)?;
    for (id, _, l) in &lines {
        if let Some(local) = node::get(&repo.db, id)? {
            if local.recipe_hash != l.node.recipe_hash && local.created_at != l.node.created_at {
                return Err(msg(format!(
                    "pull: node id {id} exists here with a different recipe (local {}, remote {}); refusing",
                    &local.recipe_hash[..12.min(local.recipe_hash.len())],
                    &l.node.recipe_hash[..12.min(l.node.recipe_hash.len())]
                )));
            }
        }
    }
    let mut stats = SyncStats::default();
    let rec = ops::record(repo, None, |repo| {
        stats.blobs = remote.pull_blobs(&repo.objects).map_err(remote_err)?;
        for (id, h, l) in &lines {
            let key = format!("sync:{id}");
            let (replace, same) = match node::get(&repo.db, id)? {
                None => (true, false),
                Some(local) => {
                    let (lh, _) = line_of(repo, &local)?;
                    (
                        lh != *h && repo.meta(&key)?.as_deref() == Some(lh.as_str()),
                        lh == *h,
                    )
                }
            };
            if replace {
                apply_line(repo, l)?;
                stats.nodes += 1;
            }
            if replace || same {
                repo.set_meta(&key, h)?;
            }
        }
        if let Some(raw) = remote.get("pins.json").map_err(remote_err)? {
            let remote_pins: BTreeMap<String, String> = serde_json::from_slice(&raw)?;
            let mine = local_pins(repo)?;
            let mh = crate::b3(&serde_json::to_vec_pretty(&mine)?);
            let synced = repo.meta("sync:pins")?;
            if synced.as_deref() == Some(mh.as_str()) || (synced.is_none() && mine.is_empty()) {
                repo.db.execute("DELETE FROM pins", [])?;
                for (name, id) in &remote_pins {
                    repo.db
                        .execute("INSERT INTO pins(name,node_id) VALUES(?1,?2)", [name, id])?;
                }
                repo.set_meta(
                    "sync:pins",
                    &crate::b3(&serde_json::to_vec_pretty(&remote_pins)?),
                )?;
                stats.pins = remote_pins != mine;
            }
        }
        Ok(repo.head()?.unwrap_or_default())
    })?;
    Ok((rec, stats))
}

fn apply_line(repo: &Repo, l: &Line) -> Result<()> {
    let id = &l.node.id;
    repo.db.execute("DELETE FROM nodes WHERE id=?1", [id])?;
    l.node.insert(&repo.db)?;
    delta::store(repo, id, &l.deltas)?;
    if let Some(m) = &l.code_manifest {
        repo.db.execute(
            "INSERT OR IGNORE INTO code_trees(git_tree, manifest_hash) VALUES(?1,?2)",
            [&l.node.code, m],
        )?;
    }
    repo.db
        .execute("DELETE FROM metrics WHERE node_id=?1", [id])?;
    if let Some(h) = &l.metrics {
        let rows: Vec<(String, i64, f64, String)> =
            serde_json::from_slice(&repo.objects.read_blob(h)?)?;
        for (k, s, v, ts) in rows {
            repo.db.execute(
                "INSERT INTO metrics(node_id,key,step,value,ts) VALUES(?1,?2,?3,?4,?5)",
                params![id, k, s, v, ts],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Status;

    fn clone_at(dir: &std::path::Path, remote: &std::path::Path) -> Repo {
        std::fs::create_dir_all(dir).unwrap();
        let mut r = Repo::init(dir).unwrap();
        r.config.remote = Some(remote.display().to_string());
        r
    }

    fn mk(repo: &Repo, id: &str, parent: Option<&str>, recipe: &str) {
        let n = Node {
            id: id.into(),
            parent: parent.map(Into::into),
            code: "c".into(),
            config: "c".into(),
            data: "d".into(),
            env: "e".into(),
            recipe_hash: recipe.into(),
            weights: None,
            docs: None,
            fork_step: None,
            status: Status::Done,
            created_at: crate::now(),
            finished_at: None,
            command: "x".into(),
            note: Some("orig".into()),
            note_auto: false,
            lock_ok: None,
            depth: 0,
            sweep: None,
        };
        n.insert(&repo.db).unwrap();
        delta::store(repo, id, &Deltas::default()).unwrap();
    }

    fn listing(dir: &std::path::Path) -> Vec<(String, u64)> {
        let mut v: Vec<_> = ignore::WalkBuilder::new(dir)
            .standard_filters(false)
            .build()
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| (e.path().display().to_string(), e.metadata().unwrap().len()))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn two_clones_push_pull() {
        let t = tempfile::tempdir().unwrap();
        let remote = t.path().join("remote");
        let mut a = clone_at(&t.path().join("a"), &remote);
        let mut b = clone_at(&t.path().join("b"), &remote);
        mk(&a, "p", None, "r0");
        crate::metrics::ingest_line(&a.db, "p", r#"{"step":1,"loss":2.5}"#).unwrap();
        a.cmdline = "pin".into();
        crate::cmd::pin(&mut a, "p", "best").unwrap();
        let s = push(&a).unwrap();
        assert_eq!((s.nodes, s.pins), (1, true));

        let (_, s) = pull(&mut b).unwrap();
        assert_eq!(s.nodes, 1);
        assert_eq!(b.node("p").unwrap().note.as_deref(), Some("orig"));
        assert_eq!(
            crate::metrics::series(&b.db, "p", "loss").unwrap(),
            [(1, 2.5)]
        );
        assert_eq!(
            local_pins(&b).unwrap().get("best").map(String::as_str),
            Some("p")
        );

        mk(&a, "c1", Some("p"), "r1");
        mk(&b, "c2", Some("p"), "r2");
        push(&a).unwrap();
        push(&b).unwrap();
        pull(&mut a).unwrap();
        pull(&mut b).unwrap();
        for r in [&a, &b] {
            for id in ["p", "c1", "c2"] {
                r.node(id).unwrap();
            }
        }
        // second push: nothing changes on the remote
        let before = listing(&remote);
        assert_eq!(push(&a).unwrap().blobs, Stats::default());
        push(&b).unwrap();
        assert_eq!(listing(&remote), before);

        // note: last writer wins; a clone that did not edit does not clobber it
        b.db.execute("UPDATE nodes SET note='renamed' WHERE id='p'", [])
            .unwrap();
        push(&b).unwrap();
        push(&a).unwrap();
        pull(&mut a).unwrap();
        assert_eq!(a.node("p").unwrap().note.as_deref(), Some("renamed"));

        // collision: same id, different recipe and creation time
        let mut c = clone_at(&t.path().join("c"), &remote);
        mk(&c, "c1", None, "other");
        let e = pull(&mut c).err().unwrap().to_string();
        assert!(e.contains("c1") && e.contains("different recipe"), "{e}");

        a.config.remote = None;
        assert!(push(&a).unwrap_err().to_string().contains("remote"));
    }
}
