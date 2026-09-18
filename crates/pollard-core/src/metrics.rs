//! Tier 3 metric streams: JSONL ingestion and inheritance across forks (§5).

use std::collections::BTreeMap;

use rusqlite::{Connection, params};

use crate::node::{self, Node};
use crate::{Result, msg};

/// Ingest one JSONL line `{"step": int, "<key>": number, ...}`; returns points written.
/// Lines without an integer `step` or that are not JSON objects are skipped.
pub fn ingest_line(db: &Connection, node: &str, line: &str) -> Result<usize> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(0);
    }
    let Ok(serde_json::Value::Object(m)) = serde_json::from_str::<serde_json::Value>(line) else {
        return Ok(0);
    };
    let Some(step) = m.get("step").and_then(|s| s.as_i64()) else {
        return Ok(0);
    };
    let ts = crate::now();
    let mut n = 0;
    let mut st =
        db.prepare_cached("INSERT INTO metrics(node_id,key,step,value,ts) VALUES(?1,?2,?3,?4,?5)")?;
    for (k, v) in &m {
        if k == "step" {
            continue;
        }
        if let Some(v) = v.as_f64() {
            st.execute(params![node, k, step, v, ts])?;
            n += 1;
        }
    }
    Ok(n)
}

fn own(db: &Connection, node: &str, key: &str, max_step: Option<i64>) -> Result<Vec<(i64, f64)>> {
    let mut st = db.prepare_cached(
        "SELECT step, value FROM metrics WHERE node_id=?1 AND key=?2 AND step<=?3 ORDER BY step, rowid",
    )?;
    let v = st
        .query_map(params![node, key, max_step.unwrap_or(i64::MAX)], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Stream for `key` with inherited points: walk up while links carry `fork_step`,
/// taking each ancestor's points with `step <= min(fork_step so far)`. Later writers win per step.
pub fn series(db: &Connection, node: &str, key: &str) -> Result<Vec<(i64, f64)>> {
    let mut layers = vec![own(db, node, key, None)?];
    let mut cur = node::get(db, node)?.ok_or_else(|| msg(format!("unknown node: {node}")))?;
    let mut cap = i64::MAX;
    while let (Some(fs), Some(p)) = (cur.fork_step, cur.parent.clone()) {
        cap = cap.min(fs);
        layers.push(own(db, &p, key, Some(cap))?);
        let Some(pn) = node::get(db, &p)? else { break };
        cur = pn;
    }
    let mut merged = BTreeMap::new();
    for layer in layers.into_iter().rev() {
        merged.extend(layer);
    }
    Ok(merged.into_iter().collect())
}

/// Keys this node can read (own plus inherited through fork_step links).
pub fn keys(db: &Connection, node: &str) -> Result<Vec<String>> {
    let mut ids = vec![node.to_string()];
    let mut cur = node::get(db, node)?;
    while let Some(n) = cur {
        match (n.fork_step, &n.parent) {
            (Some(_), Some(p)) => {
                ids.push(p.clone());
                cur = node::get(db, p)?;
            }
            _ => break,
        }
    }
    let mut out = std::collections::BTreeSet::new();
    let mut st = db.prepare_cached("SELECT DISTINCT key FROM metrics WHERE node_id=?1")?;
    for id in ids {
        for k in st.query_map([id], |r| r.get::<_, String>(0))? {
            out.insert(k?);
        }
    }
    Ok(out.into_iter().collect())
}

/// Value at exactly `step`, else the last point before it.
pub fn value_at(series: &[(i64, f64)], step: i64) -> Option<f64> {
    match series.binary_search_by_key(&step, |p| p.0) {
        Ok(i) => Some(series[i].1),
        Err(0) => None,
        Err(i) => Some(series[i - 1].1),
    }
}

/// Fork-step monotonicity (§5): forking `target` at `step` re-parents to the ancestor that
/// logged `step` when `target` itself was forked at a later step. Returns (node, notice).
pub fn fork_target(db: &Connection, target: Node, step: i64) -> Result<(Node, Option<String>)> {
    let orig = target.id.clone();
    let mut cur = target;
    while let (Some(m), Some(p)) = (cur.fork_step, cur.parent.clone()) {
        if step >= m {
            break;
        }
        cur = node::get(db, &p)?.ok_or_else(|| msg(format!("unknown node: {p}")))?;
    }
    let notice = (cur.id != orig).then(|| {
        format!(
            "note: {orig} was forked after step {step}; forking its ancestor {} at {step} instead",
            cur.id
        )
    });
    Ok((cur, notice))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Status;

    fn mk(db: &Connection, id: &str, parent: Option<&str>, fs: Option<i64>) {
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
            fork_step: fs,
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
        .insert(db)
        .unwrap();
    }

    #[test]
    fn inheritance_and_fork_rule() {
        let t = tempfile::tempdir().unwrap();
        let mut repo = crate::Repo::init(t.path()).unwrap();
        let db = &mut repo.db;
        mk(db, "a", None, None);
        mk(db, "b", Some("a"), Some(100));
        mk(db, "c", Some("b"), Some(150));
        for s in 1..=150 {
            ingest_line(db, "a", &format!(r#"{{"step":{s},"loss":{s}.0}}"#)).unwrap();
        }
        for s in 101..=200 {
            ingest_line(db, "b", &format!(r#"{{"step":{s},"loss":-{s}.0,"acc":1}}"#)).unwrap();
        }
        let sb = series(db, "b", "loss").unwrap();
        assert_eq!(sb.len(), 200);
        assert_eq!(sb[99], (100, 100.0));
        assert_eq!(sb[100], (101, -101.0));
        // c inherits b up to 150, which inherits a up to 100
        let sc = series(db, "c", "loss").unwrap();
        assert_eq!(sc.len(), 150);
        assert_eq!(keys(db, "c").unwrap(), ["acc", "loss"]);
        assert_eq!(value_at(&sc, 1000), Some(-150.0));

        let b = node::get(db, "b").unwrap().unwrap();
        let (n, notice) = fork_target(db, b.clone(), 50).unwrap();
        assert_eq!(n.id, "a");
        assert!(notice.is_some());
        let (n, notice) = fork_target(db, b, 120).unwrap();
        assert_eq!((n.id.as_str(), notice), ("b", None));
    }
}
