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
    /// `(name, node_id)`; format-1 snapshots (always named) decode unchanged
    pins: Vec<(Option<String>, String)>,
}

/// Snapshots are zstd-compressed JSON kept in the `ops` row (local state; never synced).
fn snapshot(repo: &Repo) -> Result<Vec<u8>> {
    let mut st = repo
        .db
        .prepare("SELECT name, node_id FROM pins ORDER BY name, node_id")?;
    let pins = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let s = Snap {
        head: repo.head()?,
        fork_step: repo.meta("fork_step")?,
        nodes: node::all(&repo.db)?,
        pins,
    };
    zstd::encode_all(&serde_json::to_vec(&s)?[..], 3).map_err(|e| msg(format!("op snapshot: {e}")))
}

fn decode(blob: &[u8]) -> Result<Snap> {
    let raw = zstd::decode_all(blob).map_err(|e| msg(format!("op snapshot: {e}")))?;
    Ok(serde_json::from_slice(&raw)?)
}

fn restore(repo: &Repo, before: &[u8], after: &[u8]) -> Result<()> {
    let (before, after) = (decode(before)?, decode(after)?);
    let before_nodes: std::collections::HashMap<_, _> =
        before.nodes.iter().map(|n| (&n.id, n)).collect();
    let after_nodes: std::collections::HashMap<_, _> =
        after.nodes.iter().map(|n| (&n.id, n)).collect();
    for n in &after.nodes {
        if !before_nodes.contains_key(&n.id) {
            repo.db.execute("DELETE FROM nodes WHERE id=?1", [&n.id])?;
        }
    }
    for n in &before.nodes {
        let after_node = after_nodes.get(&n.id).copied();
        if after_node == Some(n) {
            continue;
        }
        let restored = match (after_node, node::get(&repo.db, &n.id)?) {
            (Some(a), Some(current)) => {
                // Undo only this operation's edits, preserving asynchronous run updates.
                let mut value = serde_json::to_value(current)?;
                let old = serde_json::to_value(n)?;
                let new = serde_json::to_value(a)?;
                // A field missing on one side (e.g. `pruned_at` when unset) counts as null.
                let get = |v: &serde_json::Value, k: &str| {
                    v.get(k).cloned().unwrap_or(serde_json::Value::Null)
                };
                let keys: std::collections::BTreeSet<&String> = old
                    .as_object()
                    .unwrap()
                    .keys()
                    .chain(new.as_object().unwrap().keys())
                    .collect();
                for key in keys {
                    if get(&new, key) != get(&old, key) && get(&value, key) == get(&new, key) {
                        value[key] = get(&old, key);
                    }
                }
                serde_json::from_value(value)?
            }
            (Some(_), None) => continue,
            (None, _) => n.clone(),
        };
        repo.db.execute("DELETE FROM nodes WHERE id=?1", [&n.id])?;
        restored.insert(&repo.db)?;
    }
    for p in &after.pins {
        if !before.pins.contains(p) {
            repo.db.execute(
                "DELETE FROM pins WHERE name IS ?1 AND node_id=?2",
                params![p.0, p.1],
            )?;
        }
    }
    for p in &before.pins {
        if !after.pins.contains(p) {
            repo.db.execute(
                "INSERT OR REPLACE INTO pins(name,node_id) VALUES(?1,?2)",
                params![p.0, p.1],
            )?;
        }
    }
    if before.fork_step != after.fork_step {
        match &before.fork_step {
            Some(v) => repo.set_meta("fork_step", v)?,
            None => repo.del_meta("fork_step")?,
        }
    }
    if before.head != after.head {
        repo.set_head(before.head.as_deref())?;
    }
    Ok(())
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
        Ok(OpRecord {
            op_id: repo.db.last_insert_rowid(),
            command,
            node,
        })
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
    let mut st = repo
        .db
        .prepare("SELECT op_id, ts, command FROM ops ORDER BY op_id DESC LIMIT ?1")?;
    let v = st
        .query_map([limit as i64], |r| {
            Ok(OpEntry {
                op_id: r.get(0)?,
                ts: r.get(1)?,
                command: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Restore nodes, pins, head and the working copy to before the last `n` ops.
/// Undo is itself an op, so `undo` twice is a redo (as in Jujutsu).
pub fn undo(repo: &mut Repo, n: usize) -> Result<OpRecord> {
    if n == 0 {
        return Err(msg("undo: n must be at least 1"));
    }
    type UndoRow = (i64, Vec<u8>, Option<String>, Vec<u8>);
    let rows: Vec<UndoRow> = {
        let mut st = repo.db.prepare("SELECT op_id, before_snapshot, wc_snapshot, after_snapshot FROM ops ORDER BY op_id DESC LIMIT ?1")?;
        st.query_map([n as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    // Snapshots from before the format-2 upgrade are never restored (op-log barrier).
    let barrier: i64 = repo
        .meta("migrated_at_op")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if rows.iter().any(|r| r.0 <= barrier) {
        let since = rows.iter().take_while(|r| r.0 > barrier).count();
        return Err(msg(format!(
            "cannot undo past the format-2 upgrade ({since} op{} since it); nothing undone",
            if since == 1 { "" } else { "s" }
        )));
    }
    if rows.len() < n {
        return Err(msg(format!("undo: only {} op(s) in the log", rows.len())));
    }
    let op_id = rows.last().unwrap().0;
    // Working copy target: the oldest undone op that recorded one.
    let wc_target = rows.iter().rev().find_map(|r| r.2.clone());
    // Save the current working copy first so undo itself is undoable.
    let wc_now = match &wc_target {
        Some(_) => Some(crate::wc::snapshot_manifest(repo)?),
        None => None,
    };
    let pruned_before: Vec<String> = node::all(&repo.db)?
        .into_iter()
        .filter(|n| n.pruned_at.is_some())
        .map(|n| n.id)
        .collect();
    let rec = record(repo, wc_now, |repo| {
        for (_, before, _, after) in &rows {
            restore(repo, before, after)?;
        }
        Ok(repo.head()?.unwrap_or_default())
    })?;
    if let Some(m) = wc_target {
        crate::wc::checkout(repo, &m)?;
    }
    // Un-pruned nodes whose weights `gc` already collected (§3: gc is the point of no return).
    for id in pruned_before {
        if let Some(n) = node::get(&repo.db, &id)?.filter(|n| n.pruned_at.is_none()) {
            let gone = crate::weights::missing_weights(repo, &n)?;
            if !gone.is_empty() {
                eprintln!(
                    "warning: {id}: weights already removed by gc: {}",
                    gone.join(", ")
                );
            }
        }
    }
    Ok(OpRecord {
        node: if rec.node.is_empty() {
            format!("(op {op_id})")
        } else {
            rec.node
        },
        ..rec
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pins(r: &Repo) -> Vec<(Option<String>, String)> {
        let mut st =
            r.db.prepare("SELECT name, node_id FROM pins ORDER BY name")
                .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Format 2: a snapshot holding an unnamed pin `(None, id)` restores it.
    #[test]
    fn unnamed_pin_survives_undo() {
        let t = tempfile::tempdir().unwrap();
        let mut r = Repo::init(t.path()).unwrap();
        r.db.execute_batch("INSERT INTO pins(node_id, name) VALUES('a', NULL), ('a', 'paper');")
            .unwrap();
        let both = pins(&r);
        record(&mut r, None, |r| {
            r.db.execute("DELETE FROM pins", [])?;
            Ok("a".into())
        })
        .unwrap();
        assert!(pins(&r).is_empty());
        undo(&mut r, 1).unwrap();
        assert_eq!(pins(&r), both);
        assert_eq!(both[0], (None, "a".to_string()));
        undo(&mut r, 1).unwrap(); // undo of the undo
        assert!(pins(&r).is_empty());
    }
}
