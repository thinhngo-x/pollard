//! Op log and undo (§3 `ops`). A snapshot is `{head, fork_step, nodes, pins}` as JSON.

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::node::{self, Node};
use crate::{Repo, Result, msg};

/// What a mutating command did; the CLI prints `node` on its last line.
#[derive(Debug, Clone)]
pub struct OpRecord {
    pub op_id: i64,
    pub command: String,
    pub node: String,
}

#[derive(Serialize, Deserialize)]
struct Snap {
    head: Option<String>,
    /// pending `fork --step` for the next run
    #[serde(default)]
    fork_step: Option<String>,
    nodes: Vec<Node>,
    pins: Vec<(String, String)>,
}

/// Snapshots are zstd-compressed JSON kept in the `ops` row (local state; never synced).
fn snapshot(repo: &Repo) -> Result<Vec<u8>> {
    let mut st = repo.db.prepare("SELECT name, node_id FROM pins ORDER BY name")?;
    let pins = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let s = Snap { head: repo.head()?, fork_step: repo.meta("fork_step")?, nodes: node::all(&repo.db)?, pins };
    zstd::encode_all(&serde_json::to_vec(&s)?[..], 3).map_err(|e| msg(format!("op snapshot: {e}")))
}

fn restore(repo: &Repo, blob: &[u8]) -> Result<()> {
    let raw = zstd::decode_all(blob).map_err(|e| msg(format!("op snapshot: {e}")))?;
    let s: Snap = serde_json::from_slice(&raw)?;
    repo.db.execute("DELETE FROM nodes", [])?;
    repo.db.execute("DELETE FROM pins", [])?;
    for n in &s.nodes {
        n.insert(&repo.db)?;
    }
    for (name, id) in &s.pins {
        repo.db.execute("INSERT INTO pins(name,node_id) VALUES(?1,?2)", [name, id])?;
    }
    match &s.fork_step {
        Some(v) => repo.set_meta("fork_step", v)?,
        None => repo.del_meta("fork_step")?,
    }
    repo.set_head(s.head.as_deref())
}

/// Run `f` as one op: lock, transaction, before/after snapshots, op row.
/// `wc_snapshot` is the manifest hash of the working copy if the op may change it.
pub fn record(
    repo: &mut Repo,
    wc_snapshot: Option<String>,
    f: impl FnOnce(&mut Repo) -> Result<String>,
) -> Result<OpRecord> {
    let _lock = repo.lock()?;
    repo.db.execute_batch("BEGIN IMMEDIATE")?;
    let res = (|| {
        let before = snapshot(repo)?;
        let node = f(repo)?;
        let after = snapshot(repo)?;
        let command = repo.cmdline.clone();
        repo.db.execute(
            "INSERT INTO ops(ts,command,before_snapshot,after_snapshot,wc_snapshot) VALUES(?1,?2,?3,?4,?5)",
            params![crate::now(), command, before, after, wc_snapshot],
        )?;
        Ok(OpRecord { op_id: repo.db.last_insert_rowid(), command, node })
    })();
    match res {
        Ok(r) => {
            repo.db.execute_batch("COMMIT")?;
            Ok(r)
        }
        Err(e) => {
            let _ = repo.db.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// One row of `pollard op log`.
pub struct OpEntry {
    pub op_id: i64,
    pub ts: String,
    pub command: String,
}

pub fn log(repo: &Repo, limit: usize) -> Result<Vec<OpEntry>> {
    let mut st = repo.db.prepare("SELECT op_id, ts, command FROM ops ORDER BY op_id DESC LIMIT ?1")?;
    let v = st
        .query_map([limit as i64], |r| Ok(OpEntry { op_id: r.get(0)?, ts: r.get(1)?, command: r.get(2)? }))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Restore nodes, pins, head and the working copy to before the last `n` ops.
/// Undo is itself an op, so `undo` twice is a redo (as in Jujutsu).
pub fn undo(repo: &mut Repo, n: usize) -> Result<OpRecord> {
    if n == 0 {
        return Err(msg("undo: n must be at least 1"));
    }
    let rows: Vec<(i64, Vec<u8>, Option<String>)> = {
        let mut st = repo.db.prepare("SELECT op_id, before_snapshot, wc_snapshot FROM ops ORDER BY op_id DESC LIMIT ?1")?;
        st.query_map([n as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    if rows.len() < n {
        return Err(msg(format!("undo: only {} op(s) in the log", rows.len())));
    }
    let (op_id, before, _) = rows.last().unwrap().clone();
    // Working copy target: the oldest undone op that recorded one.
    let wc_target = rows.iter().rev().find_map(|r| r.2.clone());
    // Save the current working copy first so undo itself is undoable.
    let wc_now = match &wc_target {
        Some(_) => Some(crate::wc::snapshot_manifest(repo)?),
        None => None,
    };
    let pruned_before: Vec<String> =
        node::all(&repo.db)?.into_iter().filter(|n| n.status == node::Status::Pruned).map(|n| n.id).collect();
    let rec = record(repo, wc_now, |repo| {
        restore(repo, &before)?;
        Ok(repo.head()?.unwrap_or_default())
    })?;
    if let Some(m) = wc_target {
        crate::wc::checkout(repo, &m)?;
    }
    // Un-pruned nodes whose weights `gc` already collected (§3: gc is the point of no return).
    for id in pruned_before {
        if let Some(n) = node::get(&repo.db, &id)?.filter(|n| n.status != node::Status::Pruned) {
            let gone = crate::weights::missing_weights(repo, &n)?;
            if !gone.is_empty() {
                eprintln!("warning: {id}: weights already removed by gc: {}", gone.join(", "));
            }
        }
    }
    Ok(OpRecord { node: if rec.node.is_empty() { format!("(op {op_id})") } else { rec.node }, ..rec })
}
